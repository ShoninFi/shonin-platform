//! A deposit nothing has priced for the freeze period, or one on a vault
//! that is no longer open, goes back without asking the station: the
//! program consults no driver group then, so the app's take-back names
//! none, fits one transaction and is offered. Within the freeze period on
//! an open vault the program asks every group, the app names them all, and
//! a station whose groups don't fit is said, not offered.
//!
//! These tests run the app's own take-back flow (`flow::take_cash`) and its
//! measure (`flow::taking_back` + `flow::fits`) against a small simulated
//! cluster holding a station of two drivers, each holding eight markets.

use std::cell::RefCell;
use std::collections::HashMap;

use app::tx::solana::SolError;
use app::tx::solana::flow::{self, Cluster, Ctx, Fallback, Step};
use app::tx::solana::port::{
    AccountData, Blockhash, Failure, Record, Sig, Signed, Simulated, SolReader, SolSigner, Status,
};
use app::tx::{Cancel, Clock, Pace};
use app::wallet::solana::key::SolKey;
use chain::solana_ix::accounts::discriminator as d;
use chain::solana_ix::transaction::Unsigned;
use chain::solana_ix::vault::VaultKeys;
use chain::solana_ix::{Hash, Pubkey, address, ids};
use futures::executor::block_on;

const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;
const OFFSET: u128 = 1_000_000_000_000;
const STRATEGY: u64 = 4;
const DAY: u64 = 86_400;
const FREEZE_AFTER: u64 = 7 * DAY;
/// The deposit's valuation time.
const EARLIEST: u64 = 1_010;
/// The id and nonce of the deposit's request.
const REQUEST: u64 = 5;

fn vault_of(sim: &Sim) -> chain::solana_ix::accounts::Vault {
    let data = block_on(sim.account(&keys().vault)).unwrap().unwrap();
    chain::solana_ix::accounts::Vault::read(&data.data).unwrap()
}

fn mint() -> Pubkey {
    Pubkey::new_from_array([0xdd; 32])
}
fn platform() -> Pubkey {
    Pubkey::new_from_array([0xee; 32])
}
fn author() -> Pubkey {
    Pubkey::new_from_array([0xaa; 32])
}
fn keys() -> VaultKeys {
    VaultKeys::new(STRATEGY, mint(), author(), platform())
}
fn depositor() -> SolKey {
    SolKey::from_phrase(
        "test test test test test test test test test test test junk",
        0,
    )
    .unwrap()
}

// The program's layouts --------------------------------------------------------------

struct Writer(Vec<u8>);

impl Writer {
    fn new(tag: [u8; 8]) -> Self {
        Self(tag.to_vec())
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u128(mut self, v: u128) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i128(mut self, v: i128) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn key(mut self, k: &Pubkey) -> Self {
        self.0.extend_from_slice(k.as_ref());
        self
    }
    fn bytes32(mut self, b: [u8; 32]) -> Self {
        self.0.extend_from_slice(&b);
        self
    }
    fn n(mut self, count: usize, f: impl Fn(Self) -> Self) -> Self {
        for _ in 0..count {
            self = f(self);
        }
        self
    }
}

/// The vault's account: `state` as the program numbers it (1 open, 2
/// winding down), priced once at 1,000, its cash all at home, and a freeze
/// period of a week.
fn vault_bytes(state: u8) -> Vec<u8> {
    let k = keys();
    let cash = 10_000 * USD;
    let total_supply = 10_000 * WAD;
    Writer::new(d::VAULT)
        .u64(STRATEGY)
        .bytes32([1; 32])
        .key(&author())
        .key(&Pubkey::new_from_array([0xad; 32]))
        .key(&platform())
        .key(&mint())
        .key(&k.vault_dollars)
        .key(&k.station)
        .bytes32([2; 32])
        .u64(1)
        .u16(100)
        .u16(100)
        .u16(1000)
        .u16(500)
        .u16(10)
        // Timers: slot interval, lead, window, the fallback delay, usage
        // freshness, the tank escape delay, the floor delay, the halt
        // grace, the freeze period, the wind-down.
        .u64(10)
        .u64(5)
        .u64(3)
        .u64(DAY)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(0)
        .u64(FREEZE_AFTER)
        .u64(0)
        .u64(10 * USD)
        .u32(0)
        .u8(255)
        .u8(state)
        .u64(0)
        .u64(0)
        .u8(0)
        .u128(total_supply)
        .u128(0)
        .u128(0)
        .u128(0)
        .u64(cash)
        .u64(0)
        .u128(1_000_000_000_000_000_000_000_000_000_000)
        .u64(0)
        .u64(0)
        .n(6, |w| w.u64(0))
        .u128(0)
        .u8(0)
        .u64(0)
        .u64(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u64(0)
        .u16(500)
        .u16(0)
        .u64(0)
        .u64(13)
        .n(7, |w| w.u64(0))
        .u64(0)
        .u64(1_000)
        .u8(1)
        .u64(1)
        .u64(1_000)
        .u128(u128::from(cash) + 1)
        .u128(u128::from(cash) + 1)
        .u128(total_supply + OFFSET)
        .u128(0)
        .u128(0)
        .u64(cash)
        .u128(0)
        .u128(0)
        .n(3, |w| w.u128(0))
        .u64(0)
        .u64(0)
        .u64(0)
        .u128(0)
        .u128(0)
        .u64(0)
        .bytes32([0; 32])
        .u128(0)
        .u128(0)
        .bytes32([3; 32])
        .u64(0)
        .u64(0)
        .u64(0)
        .0
}

/// A deposit request of `owner`'s, id 5, due at `EARLIEST`, not done.
fn deposit_bytes(owner: &Pubkey) -> Vec<u8> {
    Writer::new(d::REQUEST)
        .key(&keys().vault)
        .u64(REQUEST)
        .key(owner)
        .u8(0)
        .u64(EARLIEST)
        .u8(0)
        .u128(u128::from(100 * USD))
        .u64(0)
        .key(owner)
        .u64(0)
        .key(&Pubkey::default())
        .u8(255)
        .bytes32([0; 32])
        .0[..8 + 32 + 8 + 32 + 1 + 8 + 1 + 16 + 8 + 32 + 8 + 32 + 1 + 16]
        .to_vec()
}

fn token_bytes(owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint().as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    data
}

/// A driver the station pins: its venue program, and its authority there.
struct Driver {
    program: Pubkey,
    venue: Pubkey,
}

/// The vault's station, pinning `drivers`, as its account reads.
fn station_bytes(drivers: &[Driver]) -> Vec<u8> {
    let k = keys();
    let mut w = Writer::new(d::STATION)
        .key(&k.vault)
        .key(&ids::VAULT)
        .key(&k.vault_dollars)
        .key(&address::dollars(&k.station, &mint()))
        .key(&mint())
        .bytes32([2; 32])
        .bytes32([7; 32])
        .u16(500)
        .u64(0)
        .u128(0)
        .u8(0)
        .u64(0)
        .key(&author())
        .u8(255)
        .u32(drivers.len() as u32);
    for driver in drivers {
        let authority = address::venue_authority(&k.station, &driver.venue);
        w = w
            .bytes32([b'd'; 32])
            .key(&driver.program)
            .key(&driver.venue)
            .key(&authority)
            .u8(254)
            .key(&address::dollars(&authority, &mint()))
            .key(&Pubkey::new_from_array([0x55; 32]))
            // No rules, no fixed accounts, no addresses.
            .u32(0)
            .u32(0)
            .u32(0);
    }
    w.0
}

/// The stand-in venue's account of `master`, holding `markets` markets.
fn margin_bytes(master: &Pubkey, markets: u8) -> Vec<u8> {
    let mut w = Writer::new(d::MARGIN_ACCOUNT)
        .key(master)
        .key(master)
        .i128(0)
        .u32(u32::from(markets));
    for i in 0..markets {
        w = w.bytes32([0x40 + i; 32]).i128(1).i128(0);
    }
    w.0
}

// The cluster and the wallet ----------------------------------------------------------

#[derive(Default)]
struct State {
    accounts: HashMap<Pubkey, AccountData>,
    /// Every transaction simulated, as its wire bytes.
    simulated: Vec<Vec<u8>>,
    now: u64,
}

struct Sim(RefCell<State>);

impl SolReader for Sim {
    async fn genesis(&self) -> Result<String, SolError> {
        Ok("local".to_string())
    }
    async fn account(&self, key: &Pubkey) -> Result<Option<AccountData>, SolError> {
        Ok(self.0.borrow().accounts.get(key).cloned())
    }
    async fn accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<AccountData>>, SolError> {
        let s = self.0.borrow();
        Ok(keys.iter().map(|k| s.accounts.get(k).cloned()).collect())
    }
    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Result<Vec<(Pubkey, AccountData)>, SolError> {
        let s = self.0.borrow();
        Ok(s.accounts
            .iter()
            .filter(|(_, a)| a.owner == *program)
            .filter(|(_, a)| {
                filters
                    .iter()
                    .all(|(at, bytes)| a.data.get(*at..at + bytes.len()) == Some(&bytes[..]))
            })
            .map(|(k, a)| (*k, a.clone()))
            .collect())
    }
    async fn blockhash(&self) -> Result<Blockhash, SolError> {
        Ok(Blockhash {
            hash: Hash::new_from_array([9; 32]),
            last_valid: 1_000,
        })
    }
    async fn block_height(&self) -> Result<u64, SolError> {
        Ok(100)
    }
    async fn chain_time(&self) -> Result<u64, SolError> {
        Ok(self.0.borrow().now)
    }
    async fn simulate(&self, wire: &[u8]) -> Result<Simulated, SolError> {
        let mut s = self.0.borrow_mut();
        s.simulated.push(wire.to_vec());
        // Every flow stops here, once the transaction it built can be read.
        Ok(Simulated::Failed(Failure {
            err: "stopped here, to read what was built".to_string(),
            logs: Vec::new(),
        }))
    }
    async fn send(&self, _wire: &[u8]) -> Result<Sig, SolError> {
        unreachable!("nothing is sent: every simulation stops the flow")
    }
    async fn status(&self, _signature: &Sig, _history: bool) -> Result<Option<Status>, SolError> {
        Ok(None)
    }
    async fn signatures_for(&self, _key: &Pubkey, _limit: usize) -> Result<Vec<Sig>, SolError> {
        Ok(Vec::new())
    }
    async fn transaction(&self, _signature: &Sig) -> Result<Option<Record>, SolError> {
        Ok(None)
    }
}

/// A wallet that signs and hands the transaction back for the app to send.
struct Wallet(SolKey);

impl SolSigner for Wallet {
    fn account(&self) -> Option<Pubkey> {
        Some(self.0.pubkey())
    }
    fn name(&self) -> String {
        "a wallet that only signs".to_string()
    }
    fn supports(&self, _chain: &str) -> bool {
        true
    }
    async fn sign(&self, unsigned: &Unsigned, _chain: &str) -> Result<Signed, SolError> {
        Ok(Signed::Wire(
            unsigned.signed(&[self.0.sign(unsigned.message())]).unwrap(),
        ))
    }
}

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

fn local() -> Cluster {
    Cluster {
        name: "localnet".to_string(),
        chain: "solana:localnet".to_string(),
        mint: mint(),
        local_endpoint: true,
    }
}

/// Run a flow against `sim`, giving its end.
macro_rules! run {
    ($sim:expr, |$ctx:ident| $flow:expr) => {{
        let sim: &Sim = $sim;
        let wallet = Wallet(depositor());
        let clock = NoWait;
        let cluster = local();
        let steps = RefCell::new(Vec::<Step>::new());
        let on_step = |step: Step| steps.borrow_mut().push(step);
        let $ctx = Ctx {
            reader: sim,
            signer: &wallet,
            clock: &clock,
            cluster: &cluster,
            vault: keys().vault,
            account: depositor().pubkey(),
            cancel: Cancel::new(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let end = block_on($flow);
        drop($ctx);
        end
    }};
}

/// A cluster holding the vault in `state`, its station pinning two drivers
/// whose authorities each hold eight markets at their venue, the
/// depositor's dollar account, and the depositor's deposit (request 5, due
/// at `EARLIEST`) that nothing has priced; the chain's clock at `now`.
fn with_a_waiting_deposit(state: u8, now: u64) -> (Sim, Pubkey) {
    let k = keys();
    let me = depositor().pubkey();
    let request_address = address::request(&k.vault, &me, REQUEST);
    let drivers = [
        Driver {
            program: Pubkey::new_from_array([0x32; 32]),
            venue: Pubkey::new_from_array([0x31; 32]),
        },
        Driver {
            program: Pubkey::new_from_array([0x34; 32]),
            venue: Pubkey::new_from_array([0x33; 32]),
        },
    ];
    let mut accounts = HashMap::new();
    accounts.insert(
        k.vault,
        AccountData {
            owner: ids::VAULT,
            lamports: 1_000_000,
            data: vault_bytes(state),
        },
    );
    accounts.insert(
        request_address,
        AccountData {
            owner: ids::VAULT,
            lamports: 1_000_000,
            data: deposit_bytes(&me),
        },
    );
    accounts.insert(
        k.station,
        AccountData {
            owner: ids::STATION,
            lamports: 1_000_000,
            data: station_bytes(&drivers),
        },
    );
    for driver in &drivers {
        let authority = address::venue_authority(&k.station, &driver.venue);
        accounts.insert(
            address::venue_account(&driver.venue, &authority),
            AccountData {
                owner: driver.venue,
                lamports: 1_000_000,
                data: margin_bytes(&authority, 8),
            },
        );
    }
    accounts.insert(
        address::dollars(&me, &mint()),
        AccountData {
            owner: ids::TOKEN,
            lamports: 2_039_280,
            data: token_bytes(&me, 5 * USD),
        },
    );
    let sim = Sim(RefCell::new(State {
        accounts,
        simulated: Vec::new(),
        now,
    }));
    (sim, request_address)
}

/// The vault's open state, and winding down (no longer open: deposits are
/// sent back without asking the station).
const OPEN: u8 = 1;
const WINDING_DOWN: u8 = 2;

/// The station here is one whose driver groups don't fit a legacy
/// transaction from the app, which is the case these tests are about. (One
/// venue of eight markets fits; two don't.)
#[test]
fn the_station_here_is_one_whose_groups_do_not_fit_one_transaction_from_the_app() {
    let (sim, at) = with_a_waiting_deposit(OPEN, EARLIEST + DAY);
    let me = depositor().pubkey();
    let request = block_on(flow::request_at(&sim, &keys().vault, &at))
        .unwrap()
        .unwrap();
    let with_groups = block_on(flow::taking_back(
        &sim,
        &mint(),
        &keys(),
        &vault_of(&sim),
        block_on(sim.chain_time()).unwrap(),
        &at,
        &request,
    ))
    .unwrap();
    assert!(
        !flow::fits(&me, with_groups),
        "the take-back with every driver group fits: this station is too small for the case"
    );
}

/// A day on, on an open vault, the program asks the station through every
/// driver group whether the vault could be valued now, so the groups must
/// be named: the app's build is the program's, and it is too large to send.
/// That much is said, not offered, and is right.
#[test]
fn within_the_freeze_period_on_an_open_vault_the_station_is_asked_and_the_build_is_too_large() {
    let (sim, _) = with_a_waiting_deposit(OPEN, EARLIEST + DAY);
    let end = run!(&sim, |ctx| flow::take_cash(
        &ctx,
        Fallback::Request(REQUEST)
    ));
    assert_eq!(end, Err(SolError::TooLarge));
}

/// A deposit nothing has priced for the freeze period goes back without
/// asking the station: the program reads no driver group then, so the
/// take-back names none, fits, and reaches the cluster. This is the door a
/// deposit has when a driver fails, and only its owner can open it.
#[test]
fn a_deposit_nothing_priced_for_the_freeze_period_is_taken_back_without_asking_the_station() {
    let (sim, at) = with_a_waiting_deposit(OPEN, EARLIEST + FREEZE_AFTER + 1);
    let me = depositor().pubkey();
    let request = block_on(flow::request_at(&sim, &keys().vault, &at))
        .unwrap()
        .unwrap();
    let built = block_on(flow::taking_back(
        &sim,
        &mint(),
        &keys(),
        &vault_of(&sim),
        block_on(sim.chain_time()).unwrap(),
        &at,
        &request,
    ))
    .unwrap();
    assert!(
        flow::fits(&me, built),
        "past the freeze period the program asks the station nothing, so the take-back \
         needs no driver group and fits one transaction"
    );
    let end = run!(&sim, |ctx| flow::take_cash(
        &ctx,
        Fallback::Request(REQUEST)
    ));
    assert!(
        matches!(end, Err(SolError::WouldFail { .. })),
        "the take-back must reach the cluster (this cluster stops every flow at simulation), \
         not be refused as too large before anything is asked: {end:?}"
    );
}

/// On a vault that is no longer open (winding down here), a deposit a day
/// old goes back without asking the station either.
#[test]
fn a_deposit_on_a_vault_no_longer_open_is_taken_back_without_asking_the_station() {
    let (sim, at) = with_a_waiting_deposit(WINDING_DOWN, EARLIEST + DAY);
    let me = depositor().pubkey();
    let request = block_on(flow::request_at(&sim, &keys().vault, &at))
        .unwrap()
        .unwrap();
    let built = block_on(flow::taking_back(
        &sim,
        &mint(),
        &keys(),
        &vault_of(&sim),
        block_on(sim.chain_time()).unwrap(),
        &at,
        &request,
    ))
    .unwrap();
    assert!(
        flow::fits(&me, built),
        "on a vault no longer open the program asks the station nothing, so the take-back \
         needs no driver group and fits one transaction"
    );
    let end = run!(&sim, |ctx| flow::take_cash(
        &ctx,
        Fallback::Request(REQUEST)
    ));
    assert!(
        matches!(end, Err(SolError::WouldFail { .. })),
        "the take-back must reach the cluster, not be refused as too large: {end:?}"
    );
}
