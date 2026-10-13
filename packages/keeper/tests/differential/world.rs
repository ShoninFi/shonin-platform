//! The scripted world every differential run is made against: stand-in
//! contracts (see `contracts/`) deployed on a fresh anvil, with every change
//! the scenario needs scripted at setup as a value from a set chain time on.
//! Once the keepers start, nothing but the keepers sends a transaction.
//!
//! The scenario is laid out in chain time from `t0`, a slot boundary the
//! keepers start a little before. Each vault drives a few of the keeper's
//! duties; [`World::plan`] says which, vault by vault.

use std::collections::BTreeMap;
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::node::{BlockTime, Rpc};

const DAY: i64 = 86_400;
const WAD: u64 = 1_000_000_000_000_000_000;
/// A dollar, in the stand-in vaults' six-decimal units.
const DOLLAR: u64 = 1_000_000;

alloy::sol! {
    interface IScripted {
        function script(bytes32[] keys, uint64[] froms, uint256[] values) external;
    }

    struct SeededValuation {
        uint64 time;
        uint8 status;
        uint256 gross;
        uint256 grossExit;
        uint256 supply;
        uint256 fee;
        uint256 feeExit;
        uint256 cash;
        uint32 postsMissing;
    }

    interface IStandInSetup {
        function seedValuation(SeededValuation valuation) external;
        function emitProRataTaken(address owner, uint256 shares, uint256 assets, uint256 units) external;
        function emitStateChanged(uint8 state) external;
        function emitRevokeSkipped(bytes32 driver, address key) external;
    }
}

/// The vault states, as the vault numbers them.
mod state {
    pub const ACTIVATING: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const WINDING_DOWN: u64 = 3;
    pub const FROZEN: u64 = 4;
}

/// The claim states, as the vault numbers them.
mod claim {
    pub const PENDING: u64 = 1;
    pub const WAITING: u64 = 2;
    pub const QUEUED: u64 = 3;
    pub const PAID: u64 = 4;
}

/// A valuation's status, as the vault numbers it.
mod valuation {
    pub const OPEN: u8 = 1;
    pub const PRICED: u8 = 2;
}

/// The stand-in contracts' creation code, compiled by `build_contracts.sh`.
fn creation_code(name: &str) -> Vec<u8> {
    let code: BTreeMap<String, String> =
        serde_json::from_str(include_str!("code.json")).expect("code.json reads");
    alloy::hex::decode(&code[name]).unwrap_or_else(|_| panic!("{name}'s code decodes"))
}

/// The key a scripted value is held under (`Scripted.key`).
pub fn key(name: &str, a: U256, b: U256) -> B256 {
    keccak256(
        [
            name.as_bytes(),
            &a.to_be_bytes::<32>(),
            &b.to_be_bytes::<32>(),
        ]
        .concat(),
    )
}

/// A driver's or a venue's name as the contracts store it.
pub fn name_word(name: &str) -> B256 {
    B256::right_padding_from(name.as_bytes())
}

fn word(address: Address) -> U256 {
    U256::from_be_slice(address.as_slice())
}

/// The scenario's timers and its zero.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// A slot boundary: the scenario's zero, in chain time.
    pub t0: u64,
    pub block: BlockTime,
    pub slot_interval: u64,
    pub slot_lead: u64,
    pub slot_window: u64,
    /// Shortened from the contracts' 600 s floor so a halt's grace ends
    /// inside a run.
    pub halt_grace: u64,
    /// Shortened from the contracts' one-day floor.
    pub freeze_after: u64,
    /// Shortened from the contracts' three-day floor.
    pub activation_window: u64,
}

impl Timing {
    /// Chain time `offset` seconds from the scenario's zero.
    pub fn at(&self, offset: i64) -> u64 {
        (self.t0 as i64 + offset) as u64
    }

    /// When a request that the slot at `slot` prices arrives: far enough
    /// ahead of the slot that its `earliest` is that slot.
    fn request_for(&self, slot: i64) -> i64 {
        slot - self.slot_lead as i64 - 3
    }
}

/// How far ahead of `t0` the keepers start: their first rounds (each open
/// vault's daily sends, the keys' first look) are over before the scenario's
/// own changes begin.
pub const LEAD: u64 = 50;
/// How long after `t0` the run lasts.
pub const LENGTH: u64 = 145;

/// One vault and its home station, and what it drives.
#[derive(Clone, Debug)]
pub struct VaultAt {
    pub name: &'static str,
    pub vault: Address,
    pub station: Address,
    pub drives: &'static str,
}

/// A moment an outsider's change (a booking, a listing, a band clearing, a
/// lapse with no event) first makes a transaction due: the candidate notices
/// it at its next observation, at most `keeper_notice_ms` later, where the
/// baseline's next round is at most a round later.
#[derive(Clone, Debug)]
pub struct Noticed {
    /// The transactions it applies to: those whose label starts with this.
    pub label: String,
    /// When the change happened, in chain time.
    pub at: u64,
}

/// The executor's answer for a vault: a signed total and the time it was
/// signed as of, or `None` for a 404.
pub type Answer = Option<(U256, u64)>;

/// What the executor stand-in serves: each vault's signed total from a
/// wall-clock second on (`None` a 404), and its attestation.
#[derive(Clone, Debug, Default)]
pub struct ExecutorScript {
    pub usage: BTreeMap<Address, Vec<(u64, Answer)>>,
    pub attestation: Value,
}

/// Everything about one deployment of the scenario: identical on every
/// chain it is set up on, since each setup sends the same transactions from
/// the same account in the same order.
#[derive(Clone, Debug)]
pub struct World {
    pub timing: Timing,
    pub chain_id: u64,
    pub deployer: Address,
    pub keeper: Address,
    pub registry: Address,
    pub factory: Address,
    pub old_factory: Address,
    pub closer: Address,
    pub exchange: Address,
    pub board: Address,
    pub plain_driver: Address,
    pub lighter_driver: Address,
    pub vaults: Vec<VaultAt>,
    /// Names for addresses, for the tables.
    pub names: BTreeMap<Address, String>,
    pub executor: ExecutorScript,
    pub noticed: Vec<Noticed>,
    /// The vaults whose restricted list a `refreshRestricted` would add to,
    /// once: the drivers name more codes than it holds from the start. Every
    /// vault's does but V13's, listed last, so the keepers' first rounds send
    /// the same, and the candidate's leaving out a refresh that adds nothing
    /// (see `compare::left_out_by_design`) moves nothing after it.
    pub restricted_grows: Vec<Address>,
    /// Whether the units vault's station's `value()` costs more gas than one
    /// read of a batch may (a fault case).
    pub costly_view: bool,
    /// Sends the candidate can't make before a withheld log can be checked,
    /// by label, and when it can be (a fault case).
    pub withheld: Vec<(String, u64)>,
    /// When a third holder's units appear in the units vault, its log
    /// emitted then by the harness (a fault case).
    pub late_holder: Option<u64>,
}

/// How many setup transactions are sent before waiting for them to land.
const BATCH: usize = 16;

/// The setup transactions, in the order they are sent.
struct Setup {
    txs: Vec<(Option<Address>, Vec<u8>, u64)>,
    scripts: BTreeMap<Address, Vec<(B256, u64, U256)>>,
}

impl Setup {
    fn deploy(&mut self, code: &str) {
        self.txs.push((None, creation_code(code), 6_000_000));
    }

    fn call(&mut self, to: Address, data: Vec<u8>) {
        self.txs.push((Some(to), data, 1_000_000));
    }

    /// `value` under `name`/`a`/`b` at `contract`, from `from` on.
    fn at(&mut self, contract: Address, name: &str, a: U256, b: U256, from: u64, value: U256) {
        self.scripts
            .entry(contract)
            .or_default()
            .push((key(name, a, b), from, value));
    }

    /// `value` under `name` from the start.
    fn set(&mut self, contract: Address, name: &str, value: U256) {
        self.at(contract, name, U256::ZERO, U256::ZERO, 0, value);
    }

    fn set1(&mut self, contract: Address, name: &str, a: U256, value: U256) {
        self.at(contract, name, a, U256::ZERO, 0, value);
    }

    /// The scripts, batched into `script` calls.
    fn script_calls(&mut self) {
        let scripts = std::mem::take(&mut self.scripts);
        for (contract, entries) in scripts {
            for chunk in entries.chunks(40) {
                let call = IScripted::scriptCall {
                    keys: chunk.iter().map(|(k, ..)| *k).collect(),
                    froms: chunk.iter().map(|(_, from, _)| *from).collect(),
                    values: chunk.iter().map(|(.., value)| *value).collect(),
                };
                self.txs
                    .push((Some(contract), call.abi_encode(), 6_000_000));
            }
        }
    }
}

/// Keys the scenario names.
struct Keys {
    /// Approved at most stations, blessed throughout: every revoke of it is
    /// refused until something else (a stop, a revoked driver, a reclaim)
    /// lets it go.
    standing: Address,
    /// The executor's key for the requests vault: its blessing comes due for
    /// renewal during the run.
    renewed: Address,
    /// Approved at the cancelled vault's station: its blessing lapses.
    lapsing: Address,
    /// Left approved by a skipped revoke at the requests vault's station: its
    /// blessing lapses early.
    skipped: Address,
    /// The executor's key for the heartbeat vault: blessed, holds no gas.
    unstocked: Address,
    /// The root key's signer.
    root: Address,
}

impl World {
    /// The scenario, laid out from `timing` for a chain whose deployer and
    /// keeper are these accounts.
    pub fn plan(timing: Timing, chain_id: u64, deployer: Address, keeper: Address) -> World {
        let created = |nonce: u64| deployer.create(nonce);
        let registry = created(0);
        let factory = created(1);
        let old_factory = created(2);
        let closer = created(3);
        let exchange = created(4);
        let board = created(5);
        let plain_driver = created(6);
        let lighter_driver = created(7);
        let mut vaults = Vec::new();
        let layout: [(&str, &str); 13] = [
            (
                "V1 requests",
                "valuation for a request, settle, daily fill and bill, escape's bill, skipped revoke",
            ),
            (
                "V2 heartbeat",
                "settle before a heartbeat valuation, revoke after the registry revokes a driver, gas stocking",
            ),
            ("V3 payday", "payday valuation"),
            (
                "V4 claims",
                "claim tries and their back-off, a claim the owner booked, a band that holds a try off",
            ),
            ("V5 claim freeze", "claim freeze"),
            (
                "V6 units",
                "valuation for units, serve units, units freeze, valuation after a staged close ended",
            ),
            (
                "V7 halted",
                "halt asks and bills, stop, unwind, valuation for the close, close, closed vault",
            ),
            ("V8 Closer", "the Closer's one-transaction close"),
            (
                "V9 retired",
                "evict, an executor that answers 404 then a total",
            ),
            (
                "V10 cancelled",
                "cancel, revoke a key whose blessing lapsed",
            ),
            (
                "V11 delayed",
                "close an open valuation, pending withdrawal, staged close, reclaim",
            ),
            (
                "V12 listed later",
                "a vault listed during the run, never priced",
            ),
            (
                "V13 listed last",
                "a refresh of a restricted list that would add nothing",
            ),
        ];
        for (index, (name, drives)) in layout.into_iter().enumerate() {
            vaults.push(VaultAt {
                name,
                vault: created(8 + 2 * index as u64),
                station: created(9 + 2 * index as u64),
                drives,
            });
        }
        let keys = scenario_keys();
        let mut names = BTreeMap::new();
        for (address, name) in [
            (registry, "registry"),
            (factory, "factory"),
            (old_factory, "old factory"),
            (closer, "Closer"),
            (exchange, "exchange"),
            (board, "board"),
            (plain_driver, "plain driver"),
            (lighter_driver, "lighter driver"),
            (keys.standing, "standing key"),
            (keys.renewed, "renewed key"),
            (keys.lapsing, "lapsing key"),
            (keys.skipped, "skipped key"),
            (keys.unstocked, "unstocked key"),
            (keys.root, "root signer"),
            (HOLDERS.0, "holder 1"),
            (HOLDERS.1, "holder 2"),
            (LATE_HOLDER, "holder 3"),
        ] {
            names.insert(address, name.to_string());
        }
        for at in &vaults {
            let short = at.name.split(' ').next().unwrap_or(at.name);
            names.insert(at.vault, at.name.to_string());
            names.insert(at.station, format!("S{}", &short[1..]));
        }
        let mut world = World {
            timing,
            chain_id,
            deployer,
            keeper,
            registry,
            factory,
            old_factory,
            closer,
            exchange,
            board,
            plain_driver,
            lighter_driver,
            vaults,
            names,
            executor: ExecutorScript::default(),
            noticed: Vec::new(),
            restricted_grows: Vec::new(),
            costly_view: false,
            withheld: Vec::new(),
            late_holder: None,
        };
        let complete = world.vault("V13").vault;
        world.restricted_grows = world
            .vaults
            .iter()
            .map(|at| at.vault)
            .filter(|vault| *vault != complete)
            .collect();
        world.executor = world.executor_script(&keys);
        world.noticed = world.noticed_changes();
        world
    }

    pub fn vault(&self, name_prefix: &str) -> &VaultAt {
        self.vaults
            .iter()
            .find(|at| at.name.starts_with(name_prefix))
            .unwrap_or_else(|| panic!("no vault {name_prefix}"))
    }

    pub fn name_of(&self, address: Address) -> String {
        self.names
            .get(&address)
            .cloned()
            .unwrap_or_else(|| format!("{address}"))
    }

    /// A vault's bundle: what its executor's key is blessed for.
    fn bundle(&self, vault: Address) -> B256 {
        keccak256(vault.as_slice())
    }

    fn executor_script(&self, keys: &Keys) -> ExecutorScript {
        let t = self.timing;
        let mut usage = BTreeMap::new();
        let total =
            |dollars: f64, as_of: u64| Some((U256::from((dollars * DOLLAR as f64) as u64), as_of));
        usage.insert(
            self.vault("V1").vault,
            vec![(0, total(2.0, t.at(-60))), (t.at(59), total(2.5, t.at(58)))],
        );
        usage.insert(
            self.vault("V7").vault,
            vec![
                (0, total(3.0, t.at(-60))),
                (t.at(87), total(4.0, t.at(86))),
                (t.at(106), total(5.0, t.at(105))),
            ],
        );
        usage.insert(
            self.vault("V9").vault,
            vec![(0, None), (t.at(5), total(1.0, t.at(4)))],
        );
        let journal = (
            B256::repeat_byte(0x11),
            B256::repeat_byte(0x22),
            Address::repeat_byte(0x33),
            B256::repeat_byte(0x44),
            U256::from(t.at(-10)),
        )
            .abi_encode_params();
        let leaf = |vault: Address, key: Address| {
            json!({
                "chain_id": self.chain_id,
                "vault_chain": self.chain_id,
                "vault": vault,
                "strategy_hash": self.bundle(vault),
                "key": key,
                "proof": [],
            })
        };
        let attestation = json!({
            "seal": "0x01",
            "journal": Bytes::from(journal),
            "leaves": [
                leaf(self.vault("V1").vault, keys.renewed),
                leaf(self.vault("V2").vault, keys.unstocked),
            ],
            "root": {"chain_id": self.chain_id, "signer": keys.root, "due": true, "proof": []},
        });
        ExecutorScript { usage, attestation }
    }

    fn noticed_changes(&self) -> Vec<Noticed> {
        let t = self.timing;
        let name = |prefix: &str| self.vault(prefix).name.to_string();
        let station = |prefix: &str| self.name_of(self.vault(prefix).station);
        vec![
            // The owner books a claim.
            Noticed {
                label: format!("{} settleClaim(2)", name("V4")),
                at: t.at(56),
            },
            // The band clears with no event of the vault's.
            Noticed {
                label: format!("{} settleClaim(1)", name("V4")),
                at: t.at(26),
            },
            // The author asks to withdraw the tank.
            Noticed {
                label: format!("{} keep(bill", name("V1")),
                at: t.at(60),
            },
            // The registry revokes a driver.
            Noticed {
                label: format!("{} revokeKey(plain2", station("V2")),
                at: t.at(66),
            },
            // The venue pays a withdrawal into the station's pending balance.
            Noticed {
                label: "exchange withdrawPendingBalance".to_string(),
                at: t.at(5),
            },
            // The vault stages a close at its station.
            Noticed {
                label: format!("{} settleClose", station("V11")),
                at: t.at(36),
            },
            // The staged close's dollars land at the station: deliverable
            // six seconds after its sweep (about t0+40), with no event of the
            // station's.
            Noticed {
                label: format!("{} settleClose", station("V11")),
                at: t.at(46),
            },
            // A vault is listed: its first daily round goes out when it is
            // seen.
            Noticed {
                label: format!("{} refreshRestricted", name("V12")),
                at: t.at(42),
            },
            // A skipped revoke's key lapses: a log-only fact.
            Noticed {
                label: format!("{} revokeKey(plain, skipped key", station("V1")),
                at: t.at(33),
            },
        ]
    }

    /// Deploy and script the world on one chain, all but the scripts that
    /// depend on that chain's own setup blocks; then those. Sent from the
    /// deployer, which must not have sent anything before.
    pub async fn set_up(&self, rpc: &Rpc) -> Result<()> {
        let mut nonce = rpc.nonce(self.deployer).await?;
        ensure!(
            nonce == 0,
            "the deployer has sent before; the stand-ins would not be where the plan says"
        );
        let mut setup = self.setup_transactions();
        setup.script_calls();
        let retire_index = setup
            .txs
            .iter()
            .position(|(to, data, _)| {
                *to == Some(self.vault("V9").vault)
                    && data.starts_with(&IStandInSetup::emitStateChangedCall::SELECTOR)
            })
            .context("the retirement's log is sent")?;
        // In batches, each landed before the next is sent: anvil drops a
        // transaction now and then from a long burst while it builds blocks.
        let mut retired_at = None;
        for (batch, txs) in setup.txs.chunks(BATCH).enumerate() {
            let mut hashes = Vec::new();
            for (to, data, gas) in txs {
                hashes.push(rpc.send(self.deployer, nonce, *to, data, *gas).await?);
                nonce += 1;
            }
            for (at, hash) in hashes.iter().enumerate() {
                let index = batch * BATCH + at;
                let receipt = rpc.receipt(*hash, Duration::from_secs(30)).await?;
                if receipt["status"] != json!("0x1") {
                    bail!("setup transaction {index} reverted: {receipt}");
                }
                if index == retire_index {
                    let block = super::node::quantity(&receipt["blockNumber"])?;
                    retired_at = Some(rpc.block_time(Some(block)).await?.1);
                }
            }
        }
        // The retired vault's wind-down ends at t0 + 53 on every chain,
        // whichever second its retirement's log landed in.
        let retired_at = retired_at.context("the retirement's block")?;
        let mut later = Setup {
            txs: Vec::new(),
            scripts: BTreeMap::new(),
        };
        let v9 = self.vault("V9").vault;
        later.set(v9, "retiredAt", U256::from(retired_at));
        later.set1(
            v9,
            "timers",
            U256::from(11),
            U256::from(self.timing.at(53).saturating_sub(retired_at)),
        );
        later.script_calls();
        for (to, data, gas) in &later.txs {
            let hash = rpc.send(self.deployer, nonce, *to, data, *gas).await?;
            nonce += 1;
            let receipt = rpc.receipt(hash, Duration::from_secs(60)).await?;
            ensure!(receipt["status"] == json!("0x1"), "late setup reverted");
        }
        Ok(())
    }

    /// The setup's transactions, for a look at one that fails.
    pub fn setup_txs(&self) -> Vec<(Option<Address>, Vec<u8>, u64)> {
        let mut setup = self.setup_transactions();
        setup.script_calls();
        setup.txs
    }

    /// The late holder's `ProRataTaken`, sent by the deployer on one chain:
    /// the same transaction on every chain, sent at the same moment.
    pub async fn emit_late_holder(&self, rpc: &Rpc) -> Result<()> {
        let nonce = rpc.nonce(self.deployer).await?;
        let call = IStandInSetup::emitProRataTakenCall {
            owner: LATE_HOLDER,
            shares: U256::from(1),
            assets: U256::from(1),
            units: U256::from(15),
        };
        rpc.send(
            self.deployer,
            nonce,
            Some(self.vault("V6").vault),
            &call.abi_encode(),
            200_000,
        )
        .await?;
        Ok(())
    }

    fn setup_transactions(&self) -> Setup {
        let t = self.timing;
        let keys = scenario_keys();
        let mut s = Setup {
            txs: Vec::new(),
            scripts: BTreeMap::new(),
        };
        for code in [
            "StandInRegistry",
            "StandInFactory",
            "StandInFactory",
            "StandInCloser",
            "StandInExchange",
            "StandInBoard",
            "StandInDriver",
            "StandInDriver",
        ] {
            s.deploy(code);
        }
        for _ in &self.vaults {
            s.deploy("StandInVault");
            s.deploy("StandInStation");
        }
        let u = U256::from;
        let plain = name_word("plain");
        let plain2 = name_word("plain2");
        let lighter = name_word("lighter");

        // The registry: blessings and the root's record.
        let r = self.registry;
        s.set(r, "ttl", u(300));
        s.set(r, "verifier", word(Address::repeat_byte(0x7e)));
        for (key, until) in [
            (keys.standing, t.at(10 * DAY)),
            (keys.renewed, t.at(205)),
            (keys.lapsing, t.at(140)),
            (keys.skipped, t.at(33)),
            (keys.unstocked, t.at(10 * DAY)),
        ] {
            s.set1(r, "until", word(key), u(until));
            s.set1(r, "kind", word(key), u(1));
        }
        // The registry revokes the heartbeat vault's driver.
        s.at(
            r,
            "driverRevoked",
            U256::from_be_bytes(plain2.0),
            word(self.plain_driver),
            t.at(66),
            u(1),
        );

        // The drivers and the venue.
        s.set(self.plain_driver, "name", U256::from_be_bytes(plain.0));
        s.set(self.plain_driver, "exitPath", u(0));
        let ld = self.lighter_driver;
        s.set(ld, "name", U256::from_be_bytes(lighter.0));
        s.set(ld, "exitPath", u(2));
        s.set(ld, "exchange", word(self.exchange));
        s.set(ld, "board", word(self.board));
        s.set(ld, "dollarAsset", u(3));
        let pool = Address::with_last_byte(7);
        s.set1(ld, "equity", word(pool), u(3 * DOLLAR));
        let v11 = self.vault("V11").station;
        s.at(
            self.exchange,
            "pending",
            word(v11),
            U256::ZERO,
            t.at(5),
            u(7 * DOLLAR),
        );
        s.set1(self.board, "poolCount", word(v11), u(1));
        s.at(self.board, "pool", word(v11), U256::ZERO, 0, u(7));
        s.at(self.board, "shares", word(v11), U256::ZERO, 0, u(100));

        // The factories: the old one lists the retired and the cancelled
        // vaults; the deployment's replaces it, and lists a vault more
        // during the run.
        let listed_now = ["V1", "V2", "V3", "V4", "V5", "V6", "V7", "V8", "V11"];
        for (i, prefix) in listed_now.iter().enumerate() {
            s.set1(
                self.factory,
                "vault",
                u(i as u64),
                word(self.vault(prefix).vault),
            );
        }
        s.set1(
            self.factory,
            "vault",
            u(listed_now.len() as u64),
            word(self.vault("V12").vault),
        );
        s.set1(
            self.factory,
            "vault",
            u(listed_now.len() as u64 + 1),
            word(self.vault("V13").vault),
        );
        s.at(
            self.factory,
            "vaultCount",
            U256::ZERO,
            U256::ZERO,
            t.at(135),
            u(listed_now.len() as u64 + 2),
        );
        s.set(self.factory, "vaultCount", u(listed_now.len() as u64));
        s.at(
            self.factory,
            "vaultCount",
            U256::ZERO,
            U256::ZERO,
            t.at(42),
            u(listed_now.len() as u64 + 1),
        );
        s.set(self.factory, "attestation", word(self.registry));
        s.set(self.factory, "previous", word(self.old_factory));
        s.set1(
            self.old_factory,
            "vault",
            u(0),
            word(self.vault("V9").vault),
        );
        s.set1(
            self.old_factory,
            "vault",
            u(1),
            word(self.vault("V10").vault),
        );
        s.set(self.old_factory, "vaultCount", u(2));

        // Every vault and station starts alike; each vault's own script
        // follows.
        for at in &self.vaults {
            self.common(&mut s, at, &keys);
        }
        let priced_before = |s: &mut Setup, vault: Address, gross: u64, cash: u64| {
            s.call(
                vault,
                IStandInSetup::seedValuationCall {
                    valuation: seeded(t.at(-100), valuation::PRICED, gross, cash),
                }
                .abi_encode(),
            );
        };

        // V1: a request for the slot at t0+20 and another for t0+110; a fill,
        // a bill and a restricted list that grows at the start; the author
        // asks to withdraw the tank at t0+60; a revoke skipped at its station
        // left a key whose blessing lapses at t0+33.
        let v1 = self.vault("V1");
        s.set(v1.vault, "state", u(state::OPEN));
        priced_before(&mut s, v1.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v1.vault, "lastPayment", u(t.at(-100)));
        let first = t.request_for(20);
        let second = t.request_for(110);
        s.at(
            v1.vault,
            "nextUnpriced",
            U256::ZERO,
            U256::ZERO,
            t.at(first),
            u(t.at(20)),
        );
        s.at(
            v1.vault,
            "nextUnpriced",
            U256::ZERO,
            U256::ZERO,
            t.at(second),
            u(t.at(110)),
        );
        s.set(v1.vault, "tank.balance", u(50 * DOLLAR));
        s.set(v1.vault, "tank.keepAt", u(80 * DOLLAR));
        s.set(v1.vault, "tank.usageSettled", u(DOLLAR));
        s.set(v1.vault, "tank.usageAsOf", u(t.at(-3600)));
        s.at(
            v1.vault,
            "tank.escapeRequestedAt",
            U256::ZERO,
            U256::ZERO,
            t.at(60),
            u(t.at(60)),
        );
        let author = Address::repeat_byte(0xa0);
        s.set(v1.vault, "author", word(author));
        s.set1(v1.vault, "balanceOf", word(author), u(WAD));
        s.set(v1.vault, "cash", u(10 * DOLLAR));
        s.call(
            v1.station,
            IStandInSetup::emitRevokeSkippedCall {
                driver: plain,
                key: keys.skipped,
            }
            .abi_encode(),
        );

        // V2: a day since its last priced valuation at the slot at t0+30,
        // with a valuation's requests to settle from t0+28; its key's driver
        // is revoked at t0+66.
        let v2 = self.vault("V2");
        s.set(v2.vault, "state", u(state::OPEN));
        s.call(
            v2.vault,
            IStandInSetup::seedValuationCall {
                valuation: seeded(t.at(30 - DAY), valuation::PRICED, 100 * DOLLAR, 50 * DOLLAR),
            }
            .abi_encode(),
        );
        s.set(v2.vault, "lastPayment", u(t.at(-100)));
        s.at(
            v2.vault,
            "unsettled",
            U256::ZERO,
            U256::ZERO,
            t.at(28),
            u(1),
        );
        s.set(v2.station, "driverCount", u(1));
        s.set1(v2.station, "driver", u(0), U256::from_be_bytes(plain2.0));
        s.set1(
            v2.station,
            "pinned",
            U256::from_be_bytes(plain2.0),
            word(self.plain_driver),
        );
        s.set1(
            v2.station,
            "approvedKey",
            U256::from_be_bytes(plain2.0),
            word(keys.standing),
        );

        // V3: payday is due from t0+5 (its last payment is scripted back a
        // week).
        let v3 = self.vault("V3");
        s.set(v3.vault, "state", u(state::WINDING_DOWN));
        priced_before(&mut s, v3.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v3.vault, "lastPayment", u(t.at(-100)));
        s.at(
            v3.vault,
            "lastPayment",
            U256::ZERO,
            U256::ZERO,
            t.at(5),
            u(t.at(-8 * DAY)),
        );

        // V4: a claim that waits, is held off by the band at its fourth try
        // (the slot at t0+20) until t0+26 and paid then; another the owner
        // books at t0+56.
        let v4 = self.vault("V4");
        s.set(v4.vault, "state", u(state::OPEN));
        priced_before(&mut s, v4.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v4.vault, "lastPayment", u(t.at(-100)));
        s.set(v4.vault, "claimCount", u(1));
        s.at(
            v4.vault,
            "claimCount",
            U256::ZERO,
            U256::ZERO,
            t.at(56),
            u(2),
        );
        claim_booked(&mut s, v4.vault, 1, t.at(-200), 0);
        s.at(
            v4.vault,
            "claim.outcome",
            u(1),
            U256::ZERO,
            0,
            u(claim::WAITING),
        );
        s.at(
            v4.vault,
            "claim.outcome",
            u(1),
            U256::ZERO,
            t.at(18),
            u(claim::PAID),
        );
        claim_booked(&mut s, v4.vault, 2, t.at(56), t.at(56));
        s.set1(v4.vault, "claim.outcome", u(2), u(claim::QUEUED));
        s.at(
            v4.station,
            "outsideBand",
            U256::ZERO,
            U256::ZERO,
            t.at(19),
            u(1),
        );
        s.at(
            v4.station,
            "outsideBand",
            U256::ZERO,
            U256::ZERO,
            t.at(26),
            u(0),
        );

        // V5: a claim that keeps waiting and is a freeze period old at t0+73.
        let v5 = self.vault("V5");
        s.set(v5.vault, "state", u(state::WINDING_DOWN));
        priced_before(&mut s, v5.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v5.vault, "lastPayment", u(t.at(-100)));
        s.set(v5.vault, "claimCount", u(1));
        claim_booked(&mut s, v5.vault, 1, t.at(73) - t.freeze_after, 0);
        s.set1(v5.vault, "claim.outcome", u(1), u(claim::WAITING));
        // Money at its station: once frozen it does not close in its grace.
        s.set(v5.station, "value.mark", u(5 * DOLLAR));

        // V6: two holders of units: one served once a valuation counts the
        // emptied station, one left a freeze period at t0+85; a staged close
        // ends at t0+75 with units still waiting.
        let v6 = self.vault("V6");
        s.set(v6.vault, "state", u(state::WINDING_DOWN));
        priced_before(&mut s, v6.vault, 100 * DOLLAR, 0);
        s.set(v6.vault, "lastPayment", u(t.at(-100)));
        s.set(v6.station, "value.mark", u(DOLLAR));
        if self.costly_view {
            // About 1.5M gas, past a batch read's 1M.
            s.set(v6.station, "value.burn", u(20_000));
        }
        let (h1, h2) = HOLDERS;
        s.set(v6.vault, "units.total", u(30));
        s.set1(v6.vault, "unitsOf", word(h1), u(10));
        s.set1(v6.vault, "unitsSince", word(h1), u(t.at(-300)));
        s.set1(v6.vault, "serve.outcome", word(h1), u(3));
        s.set1(v6.vault, "serve.after", word(h1), u(t.at(-(LEAD as i64))));
        s.set1(v6.vault, "unitsOf", word(h2), u(20));
        s.set1(
            v6.vault,
            "unitsSince",
            word(h2),
            u(t.at(85) - t.freeze_after),
        );
        s.set1(v6.vault, "serve.outcome", word(h2), u(2));
        s.at(
            v6.vault,
            "needsValuationFirst",
            U256::ZERO,
            U256::ZERO,
            t.at(75),
            u(1),
        );
        if let Some(at) = self.late_holder {
            // Holder 3's units from `at` on, beside what is left of the
            // others; served at once when asked.
            s.at(v6.vault, "units.total", U256::ZERO, U256::ZERO, at, u(35));
            s.at(
                v6.vault,
                "unitsOf",
                word(LATE_HOLDER),
                U256::ZERO,
                at,
                u(15),
            );
            s.at(
                v6.vault,
                "unitsSince",
                word(LATE_HOLDER),
                U256::ZERO,
                at,
                u(at),
            );
        }
        for (holder, units) in [(h1, 10u64), (h2, 20)] {
            s.call(
                v6.vault,
                IStandInSetup::emitProRataTakenCall {
                    owner: holder,
                    shares: u(1),
                    assets: u(1),
                    units: u(units),
                }
                .abi_encode(),
            );
        }

        // V7: halted, its grace over at t0+88, closed step by step; a
        // deposit and a claim left for the closed vault to pay.
        let v7 = self.vault("V7");
        s.set(v7.vault, "state", u(state::FROZEN));
        s.set(v7.vault, "haltedAt", u(t.at(88) - t.halt_grace));
        priced_before(&mut s, v7.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v7.vault, "lastPayment", u(t.at(-100)));
        s.set(v7.vault, "tank.balance", u(20 * DOLLAR));
        s.set(v7.vault, "tank.usageSettled", u(DOLLAR));
        s.set(v7.vault, "tank.usageAsOf", u(t.at(-3600)));
        s.set(v7.station, "value.mark", u(5 * DOLLAR));
        s.set(v7.vault, "claimCount", u(1));
        claim_booked(&mut s, v7.vault, 1, t.at(-200), 0);
        s.set(v7.vault, "requestCount", u(1));
        s.set1(
            v7.vault,
            "request.owner",
            u(1),
            word(Address::repeat_byte(0xc1)),
        );
        s.set1(v7.vault, "request.kind", u(1), u(0));
        s.set1(v7.vault, "request.earliest", u(1), u(t.at(100 * DAY)));
        s.set1(v7.vault, "request.amount", u(1), u(DOLLAR));

        // V8: halted, its grace over as the slot at t0+130 opens; the Closer
        // may close it.
        let v8 = self.vault("V8");
        s.set(v8.vault, "state", u(state::FROZEN));
        let grace_end = t.at(130) - t.block.measured_seconds();
        s.set(v8.vault, "haltedAt", u(grace_end - t.halt_grace));
        priced_before(&mut s, v8.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v8.vault, "lastPayment", u(t.at(-100)));
        s.set(v8.station, "value.mark", u(3 * DOLLAR));
        s.set(v8.vault, "closer.allowed", u(1));

        // V9: retired at setup (its wind-down's end is scripted after, see
        // `set_up`); the executor serves no total for it until t0+5.
        let v9 = self.vault("V9");
        s.set(v9.vault, "state", u(state::WINDING_DOWN));
        priced_before(&mut s, v9.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v9.vault, "lastPayment", u(t.at(-100)));
        s.set(v9.vault, "tank.balance", u(10 * DOLLAR));
        s.set(v9.station, "value.mark", u(5 * DOLLAR));
        s.call(
            v9.vault,
            IStandInSetup::emitStateChangedCall {
                state: state::WINDING_DOWN as u8,
            }
            .abi_encode(),
        );

        // V10: never opened, its activation window over at t0+16; its
        // station's key lapses at t0+140.
        let v10 = self.vault("V10");
        s.set(v10.vault, "state", u(state::ACTIVATING));
        s.set(v10.vault, "createdAt", u(t.at(16) - t.activation_window));
        s.set1(
            v10.station,
            "approvedKey",
            U256::from_be_bytes(plain.0),
            word(keys.lapsing),
        );

        // V11: a delayed venue: an open valuation past its post deadline, a
        // pending withdrawal from t0+5, a close staged at t0+36, money in a
        // venue vault.
        let v11 = self.vault("V11");
        s.set(v11.vault, "state", u(state::WINDING_DOWN));
        priced_before(&mut s, v11.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.call(
            v11.vault,
            IStandInSetup::seedValuationCall {
                valuation: seeded(t.at(-50), valuation::OPEN, 100 * DOLLAR, 50 * DOLLAR),
            }
            .abi_encode(),
        );
        s.set(v11.vault, "lastPayment", u(t.at(-100)));
        s.set1(v11.vault, "timers", u(3), u(51));
        s.set1(v11.station, "driver", u(0), U256::from_be_bytes(lighter.0));
        s.set1(
            v11.station,
            "pinned",
            U256::from_be_bytes(lighter.0),
            word(ld),
        );
        s.set1(
            v11.station,
            "approvedKey",
            U256::from_be_bytes(lighter.0),
            word(keys.standing),
        );
        s.at(
            v11.station,
            "stage.stagedAt",
            U256::ZERO,
            U256::ZERO,
            t.at(36),
            u(t.at(36)),
        );
        s.set(v11.station, "stage.fraction", u(WAD / 10));
        s.set(v11.station, "stage.delay", u(4));
        s.set(v11.station, "stage.window", u(600));
        s.set(v11.station, "stage.deliverAfter", u(6));
        s.set(v11.station, "stage.forVault", u(1));

        // V12: listed at t0+42, never priced.
        let v12 = self.vault("V12");
        s.set(v12.vault, "state", u(state::OPEN));

        // V13: listed at t0+135, priced lately, its restricted list complete.
        let v13 = self.vault("V13");
        s.set(v13.vault, "state", u(state::OPEN));
        priced_before(&mut s, v13.vault, 100 * DOLLAR, 50 * DOLLAR);
        s.set(v13.vault, "lastPayment", u(t.at(-100)));
        s.set(v13.vault, "restricted.wanted", u(0));
        s
    }

    /// What every vault and its station start with.
    fn common(&self, s: &mut Setup, at: &VaultAt, keys: &Keys) {
        let t = self.timing;
        let u = U256::from;
        let timers: [u64; 13] = [
            t.slot_interval,
            t.slot_lead,
            t.slot_window,
            60,
            3600,
            86_400,
            600,
            86_400,
            86_400,
            t.halt_grace,
            t.freeze_after,
            7 * 86_400,
            t.activation_window,
        ];
        for (i, value) in timers.into_iter().enumerate() {
            s.set1(at.vault, "timers", u(i as u64), u(value));
        }
        s.set(at.vault, "homeStation", word(at.station));
        s.set(
            at.vault,
            "bundleHash",
            U256::from_be_bytes(self.bundle(at.vault).0),
        );
        s.set(at.vault, "createdAt", u(t.at(-10 * DAY)));
        s.set(at.vault, "author", word(Address::repeat_byte(0xa0)));
        s.set(at.vault, "minimumDeposit", u(10 * DOLLAR));
        s.set(at.vault, "exitBandBps", u(100));
        s.set(at.vault, "value.gross", u(100 * DOLLAR));
        s.set(at.vault, "value.cash", u(50 * DOLLAR));
        s.set(at.vault, "closer", word(self.closer));
        // The drivers name two codes the list does not hold yet.
        s.set(at.vault, "restricted.wanted", u(2));
        let plain = name_word("plain");
        s.set(at.station, "vault", word(at.vault));
        s.set(at.station, "registry", word(self.registry));
        s.set(at.station, "factory", word(self.factory));
        s.set(at.station, "vaultChain", u(self.chain_id));
        s.set(
            at.station,
            "bundleHash",
            U256::from_be_bytes(self.bundle(at.vault).0),
        );
        s.set(at.station, "exitBandBps", u(100));
        s.set(at.station, "stoppedBand", u(100));
        s.set(at.station, "driverCount", u(1));
        s.set1(at.station, "driver", u(0), U256::from_be_bytes(plain.0));
        s.set1(
            at.station,
            "pinned",
            U256::from_be_bytes(plain.0),
            word(self.plain_driver),
        );
        s.set1(
            at.station,
            "approvedKey",
            U256::from_be_bytes(plain.0),
            word(keys.standing),
        );
    }
}

fn seeded(time: u64, status: u8, gross: u64, cash: u64) -> SeededValuation {
    SeededValuation {
        time,
        status,
        gross: U256::from(gross + 1),
        grossExit: U256::from(gross + 1),
        supply: U256::from(1),
        fee: U256::ZERO,
        feeExit: U256::ZERO,
        cash: U256::from(cash),
        postsMissing: 0,
    }
}

/// Script a claim booked at `booked`, pending from `from` on.
fn claim_booked(s: &mut Setup, vault: Address, id: u64, booked: u64, from: u64) {
    let u = U256::from;
    s.at(
        vault,
        "claim.owner",
        u(id),
        U256::ZERO,
        from,
        word(Address::repeat_byte(0xc0 + id as u8)),
    );
    s.at(
        vault,
        "claim.shares",
        u(id),
        U256::ZERO,
        from,
        u(5 * DOLLAR),
    );
    s.at(vault, "claim.bookedAt", u(id), U256::ZERO, from, u(booked));
    s.at(
        vault,
        "claim.state",
        u(id),
        U256::ZERO,
        from,
        u(claim::PENDING),
    );
}

/// The topic of the vault's `ProRataTaken`, which names a holder of units.
pub fn pro_rata_taken() -> B256 {
    use alloy::sol_types::SolEvent;
    chain::bindings::vault::IVault::ProRataTaken::SIGNATURE_HASH
}

/// The holder whose units appear during a run (a fault case).
const LATE_HOLDER: Address = Address::repeat_byte(0xb3);

/// The units vault's two holders.
const HOLDERS: (Address, Address) = (Address::repeat_byte(0xb1), Address::repeat_byte(0xb2));

fn scenario_keys() -> Keys {
    let named = |name: &str| Address::from_slice(&keccak256(name.as_bytes())[12..]);
    Keys {
        standing: named("standing key"),
        renewed: named("renewed key"),
        lapsing: named("lapsing key"),
        skipped: named("skipped key"),
        unstocked: named("unstocked key"),
        root: named("root signer"),
    }
}
