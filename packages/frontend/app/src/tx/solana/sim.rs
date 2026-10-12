//! A simulated Solana cluster and wallet, so every Solana flow's sequence is
//! checked natively: what is read, what is signed, what is sent, what the
//! sent transaction holds, and what each wallet or cluster answer leads to.
//!
//! The cluster keeps the vault's accounts in the program's own layout and
//! plays the vault program's part for the instructions the flows send (read
//! back from the transaction's bytes), and the keeper's: a request is
//! priced and settled a few polls after it lands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use chain::solana_ix::accounts::discriminator as d;
use chain::solana_ix::transaction::Unsigned;
use chain::solana_ix::vault::{self as ix, VaultKeys};
use chain::solana_ix::{Hash, Pubkey, address, ids, read};
use futures::executor::block_on;

use super::error::{Reason, SolError};
use super::flow::{self, Cluster, Ctx, Outcome, Prompt, Step};
use super::port::{
    AccountData, Blockhash, Failure, Record, Sig, Signed, Simulated, SolReader, SolSigner, Status,
};
use crate::tx::flow::{Cancel, Pace};
use crate::tx::port::Clock;
use crate::wallet::solana::key::SolKey;

const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;
const OFFSET: u128 = 1_000_000_000_000;

fn mint() -> Pubkey {
    Pubkey::new_from_array([0xdd; 32])
}

fn platform() -> Pubkey {
    Pubkey::new_from_array([0xee; 32])
}

fn author() -> Pubkey {
    Pubkey::new_from_array([0xaa; 32])
}

const STRATEGY: u64 = 7;

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

// The program's layouts ---------------------------------------------------------------

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

#[derive(Clone)]
struct VaultState {
    state: u8,
    halted_at: u64,
    total_supply: u128,
    cash: u64,
    request_count: u64,
    priced: bool,
    latest_time: u64,
    gross: u128,
    supply: u128,
    latest_cash: u64,
    claim_count: u64,
    restricted_hash: [u8; 32],
    minimum_deposit: u64,
    /// The station the account names: the derived one unless a test lies.
    station: Pubkey,
}

fn vault_bytes(v: &VaultState) -> Vec<u8> {
    let k = keys();
    Writer::new(d::VAULT)
        .u64(STRATEGY)
        .bytes32([1; 32])
        .key(&author())
        .key(&Pubkey::new_from_array([0xad; 32]))
        .key(&platform())
        .key(&mint())
        .key(&k.vault_dollars)
        .key(&v.station)
        .bytes32([2; 32])
        .u64(1) // created_at
        .u16(100) // exit band
        .u16(100)
        .u16(1000)
        .u16(500)
        .u16(10) // fees
        .u64(10)
        .u64(5)
        .u64(3)
        .u64(86_400)
        .n(6, |w| w.u64(0)) // timers
        .u64(v.minimum_deposit)
        .u32(0) // drivers
        .u8(255) // bump
        .u8(v.state)
        .u64(v.halted_at)
        .u64(0) // retired_at
        .u8(0) // halt reason
        .u128(v.total_supply)
        .u128(0)
        .u128(0)
        .u128(0) // pending, unpaid holding, dead shares
        .u64(v.cash)
        .u64(0) // escrow
        .u128(1_000_000_000_000_000_000_000_000_000_000) // mark
        .u64(0)
        .u64(0)
        // tank
        .n(6, |w| w.u64(0))
        .u128(0)
        .u8(0)
        .u64(0)
        // allowances
        .u64(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u64(0) // allowances_since
        .u16(500)
        .u16(0)
        .u64(0) // floor
        .u64(v.request_count)
        .n(7, |w| w.u64(0)) // cursors and tails
        .u64(0)
        .u64(v.latest_time) // valuation count, last valued
        .u8(u8::from(v.priced))
        // latest snapshot
        .u64(1)
        .u64(v.latest_time)
        .u128(v.gross)
        .u128(v.gross)
        .u128(v.supply)
        .u128(0)
        .u128(0)
        .u64(v.latest_cash)
        .u128(0)
        .u128(0)
        .n(3, |w| w.u128(0))
        .u64(0) // exit paid .. deposits_for
        .u64(v.claim_count)
        .u64(0)
        .u128(0)
        .u128(0)
        .u64(0)
        .bytes32([0; 32]) // units
        .u128(0)
        .u128(0)
        .bytes32(v.restricted_hash)
        .u64(0)
        .u64(0)
        .u64(0)
        .0
}

fn holder_bytes(vault: &Pubkey, owner: &Pubkey, shares: u128, statement: [u8; 32]) -> Vec<u8> {
    Writer::new(d::HOLDER)
        .key(vault)
        .key(owner)
        .u128(shares)
        .bytes32(statement)
        .u128(0)
        .u128(0)
        .u64(0)
        .u64(0)
        .u32(0)
        .key(owner)
        .u8(255)
        .0
}

#[allow(clippy::too_many_arguments)]
fn request_bytes(
    vault: &Pubkey,
    id: u64,
    owner: &Pubkey,
    kind: u8,
    earliest: u64,
    done: bool,
    amount: u128,
    payer: &Pubkey,
) -> Vec<u8> {
    Writer::new(d::REQUEST)
        .key(vault)
        .u64(id)
        .key(owner)
        .u8(kind)
        .u64(earliest)
        .u8(u8::from(done))
        .u128(amount)
        .u64(0)
        .key(payer)
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

fn restricted_bytes(vault: &Pubkey, codes: &[[u8; 32]]) -> Vec<u8> {
    let mut w = Writer::new(d_restricted())
        .key(vault)
        .u8(255)
        .u32(codes.len() as u32);
    for code in codes {
        w = w.bytes32(*code);
    }
    w.0
}

fn d_restricted() -> [u8; 8] {
    [199, 149, 246, 4, 140, 61, 154, 126]
}

fn code(text: &str) -> [u8; 32] {
    let mut code = [0u8; 32];
    code[..text.len()].copy_from_slice(text.as_bytes());
    code
}

fn event(tag: [u8; 8], body: Writer) -> Vec<u8> {
    let mut data = tag.to_vec();
    data.extend_from_slice(&body.0[8..]);
    data
}

fn base64(bytes: &[u8]) -> String {
    crate::wallet::solana::rpc::base64_encode(bytes)
}

// A legacy message, read back ----------------------------------------------------------

fn short_vec(bytes: &[u8], at: &mut usize) -> usize {
    let mut value = 0usize;
    for i in 0..3 {
        let byte = bytes[*at];
        *at += 1;
        value |= usize::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            break;
        }
    }
    value
}

/// The instructions of a signed legacy transaction, as (program, accounts,
/// data).
fn instructions(wire: &[u8]) -> Vec<(Pubkey, Vec<Pubkey>, Vec<u8>)> {
    let mut at = 0;
    let signatures = short_vec(wire, &mut at);
    at += 64 * signatures + 3;
    let count = short_vec(wire, &mut at);
    let keys: Vec<Pubkey> = (0..count)
        .map(|i| Pubkey::new_from_array(wire[at + 32 * i..at + 32 * (i + 1)].try_into().unwrap()))
        .collect();
    at += 32 * count + 32;
    let ixs = short_vec(wire, &mut at);
    (0..ixs)
        .map(|_| {
            let program = keys[wire[at] as usize];
            at += 1;
            let n = short_vec(wire, &mut at);
            let accounts = wire[at..at + n].iter().map(|i| keys[*i as usize]).collect();
            at += n;
            let len = short_vec(wire, &mut at);
            let data = wire[at..at + len].to_vec();
            at += len;
            (program, accounts, data)
        })
        .collect()
}

// The cluster -------------------------------------------------------------------------------

/// How a withdrawal settles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    Cash,
    Booked,
}

/// What the wallet does with its next prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Signs and hands it back.
    Sign,
    /// Signs and sends it itself.
    SignAndSend,
    Reject,
    /// Hands back a transaction with another blockhash.
    Alter,
    /// The person's account changes while the prompt is open (the session
    /// cancels the flow), and the wallet signs anyway.
    SignAfterCancel,
    /// The person switches account in the wallet, and the old one signs.
    SignAfterSwitch,
}

struct Cluster0 {
    tick: u64,
    time: u64,
    height: u64,
    genesis: String,
    accounts: HashMap<Pubkey, AccountData>,
    vault: VaultState,
    /// Transactions sent, as their wire bytes, in order.
    sent: Vec<Vec<u8>>,
    prompts: Vec<Prompt>,
    statuses: HashMap<Sig, Status>,
    records: HashMap<Sig, Record>,
    signatures: HashMap<Pubkey, Vec<Sig>>,
    /// Requests made, and the tick they were made at.
    requests: Vec<(Pubkey, u64)>,
    exit: Exit,
    /// The cluster never sees what is sent (it expires).
    drop_sends: bool,
    /// What the next simulation answers instead of running.
    simulation: Option<Failure>,
    answer: Answer,
    wallet_account: Option<Pubkey>,
    /// Ticks the keeper waits after a request lands before pricing it.
    keeper_delay: u64,
    settlements: u64,
    cancel: Cancel,
    /// The node takes each send, but its answer never reaches the app.
    lose_send_answers: bool,
    /// Each landed transaction is first seen at `processed` with an error
    /// (on a fork the cluster then drops), and confirmed fine a tick later.
    fork_first: bool,
    /// Statuses that become the confirmed ones at the next tick.
    later: Vec<(Sig, Status)>,
    /// Status looks the node doesn't answer before it answers again.
    unanswered_looks: u32,
}

#[derive(Clone)]
struct Sim(Rc<RefCell<Cluster0>>);

fn owned(data: Vec<u8>) -> AccountData {
    AccountData {
        owner: ids::VAULT,
        lamports: 1_000_000,
        data,
    }
}

/// A vault as its account reads: open, priced, its cash all at home; for
/// the pages' tests to change as they need.
pub(crate) fn a_vault() -> chain::solana_ix::accounts::Vault {
    let sim = Sim::new();
    let state = sim.0.borrow().vault.clone();
    chain::solana_ix::accounts::Vault::read(&vault_bytes(&state)).unwrap()
}

impl Sim {
    fn new() -> Self {
        let k = keys();
        let account = depositor().pubkey();
        let codes = vec![code("SANCTIONED"), code("US")];
        let vault = VaultState {
            state: 1,
            halted_at: 0,
            total_supply: 10_000 * WAD,
            cash: 10_000 * USD,
            request_count: 4,
            priced: true,
            latest_time: 1_000,
            gross: u128::from(10_000 * USD) + 1,
            supply: 10_000 * WAD + OFFSET,
            latest_cash: 10_000 * USD,
            claim_count: 0,
            restricted_hash: read::restricted_hash(&codes),
            minimum_deposit: 10 * USD,
            station: k.station,
        };
        let mut accounts = HashMap::new();
        accounts.insert(k.vault, owned(vault_bytes(&vault)));
        accounts.insert(
            address::restricted(&k.vault),
            owned(restricted_bytes(&k.vault, &codes)),
        );
        accounts.insert(
            address::dollars(&account, &mint()),
            AccountData {
                owner: ids::TOKEN,
                lamports: 2_039_280,
                data: token_bytes(&account, 5_000 * USD),
            },
        );
        Self(Rc::new(RefCell::new(Cluster0 {
            tick: 0,
            time: 1_003,
            height: 100,
            genesis: "local".to_string(),
            accounts,
            vault,
            sent: Vec::new(),
            prompts: Vec::new(),
            statuses: HashMap::new(),
            records: HashMap::new(),
            signatures: HashMap::new(),
            requests: Vec::new(),
            exit: Exit::Cash,
            drop_sends: false,
            simulation: None,
            answer: Answer::Sign,
            wallet_account: Some(account),
            keeper_delay: 2,
            settlements: 0,
            cancel: Cancel::new(),
            lose_send_answers: false,
            fork_first: false,
            later: Vec::new(),
            unanswered_looks: 0,
        })))
    }

    fn set_vault(&self, change: impl FnOnce(&mut VaultState)) {
        let mut c = self.0.borrow_mut();
        change(&mut c.vault);
        let bytes = vault_bytes(&c.vault);
        c.accounts.insert(keys().vault, owned(bytes));
    }

    fn give_shares(&self, owner: &Pubkey, shares: u128) {
        let k = keys();
        let hash = self.0.borrow().vault.restricted_hash;
        self.0.borrow_mut().accounts.insert(
            k.holder(owner),
            owned(holder_bytes(&k.vault, owner, shares, hash)),
        );
    }

    fn sends(&self) -> usize {
        self.0.borrow().sent.len()
    }

    fn last_sent(&self) -> Vec<(Pubkey, Vec<Pubkey>, Vec<u8>)> {
        instructions(self.0.borrow().sent.last().unwrap())
    }

    /// The program's part: what a landed transaction does.
    fn land(&self, wire: &[u8], signature: Sig) {
        let k = keys();
        let mut logs = vec![format!("Program {} invoke [1]", ids::VAULT)];
        for (program, accounts, data) in instructions(wire) {
            if program != ids::VAULT {
                continue;
            }
            let tag: [u8; 8] = data[..8].try_into().unwrap();
            let mut c = self.0.borrow_mut();
            let earliest = (c.time + 5).div_ceil(10) * 10;
            if tag == [243, 202, 197, 215, 135, 97, 213, 109] {
                // request_deposit(assets, receiver, statement, nonce)
                let assets = u64::from_le_bytes(data[8..16].try_into().unwrap());
                let payer = accounts[0];
                c.vault.request_count += 1;
                let id = c.vault.request_count;
                c.accounts.insert(
                    accounts[3],
                    owned(request_bytes(
                        &k.vault,
                        id,
                        &payer,
                        0,
                        earliest,
                        false,
                        u128::from(assets),
                        &payer,
                    )),
                );
                let tick = c.tick;
                c.requests.push((accounts[3], tick));
            } else if tag == [105, 49, 44, 38, 207, 241, 33, 173] {
                // request_redeem(shares, receiver, nonce)
                let shares = u128::from_le_bytes(data[8..24].try_into().unwrap());
                let owner = accounts[0];
                c.vault.request_count += 1;
                let id = c.vault.request_count;
                c.accounts.insert(
                    accounts[3],
                    owned(request_bytes(
                        &k.vault, id, &owner, 1, earliest, false, shares, &owner,
                    )),
                );
                let tick = c.tick;
                c.requests.push((accounts[3], tick));
            } else if tag == [140, 145, 176, 135, 202, 47, 186, 219] {
                // redeem_closed(shares, receiver)
                let shares = u128::from_le_bytes(data[8..24].try_into().unwrap());
                let assets = (u128::from(c.vault.cash) * shares / c.vault.total_supply) as u64;
                logs.push(format!(
                    "Program data: {}",
                    base64(&event(
                        d::WITHDRAWN,
                        Writer::new([0; 8])
                            .u64(0)
                            .key(&accounts[0])
                            .u128(shares)
                            .u64(assets)
                            .u128(0)
                    ))
                ));
            }
            let bytes = vault_bytes(&c.vault);
            c.accounts.insert(k.vault, owned(bytes));
        }
        logs.push(format!("Program {} success", ids::VAULT));
        let mut c = self.0.borrow_mut();
        let slot = c.tick;
        let landed = Status {
            slot,
            err: None,
            confirmed: true,
        };
        if c.fork_first {
            c.statuses.insert(
                signature,
                Status {
                    slot,
                    err: Some(r#"{"InstructionError":[2,{"Custom":6000}]}"#.to_string()),
                    confirmed: false,
                },
            );
            c.later.push((signature, landed));
        } else {
            c.statuses.insert(signature, landed);
        }
        c.records.insert(
            signature,
            Record {
                slot,
                err: None,
                logs,
                calls: Vec::new(),
            },
        );
    }

    /// The keeper's part: price and settle each request a few ticks after
    /// it lands.
    fn tick(&self) {
        let k = keys();
        let mut c = self.0.borrow_mut();
        c.tick += 1;
        c.time += 1;
        c.height += 10;
        for (signature, status) in std::mem::take(&mut c.later) {
            c.statuses.insert(signature, status);
        }
        let due: Vec<Pubkey> = c
            .requests
            .iter()
            .filter(|(_, at)| c.tick >= at + c.keeper_delay)
            .map(|(address, _)| *address)
            .collect();
        c.requests.retain(|(address, _)| !due.contains(address));
        for at in due {
            let mut data = c.accounts[&at].data.clone();
            let request = chain::solana_ix::accounts::Request::read(&data).unwrap();
            c.vault.latest_time = request.earliest;
            // done
            data[8 + 32 + 8 + 32 + 1 + 8] = 1;
            c.accounts.get_mut(&at).unwrap().data = data;
            let said = match request.kind {
                chain::solana_ix::accounts::RequestKind::Deposit => event(
                    d::DEPOSITED,
                    Writer::new([0; 8])
                        .u64(request.id)
                        .key(&request.owner)
                        .u64(request.amount as u64)
                        .u128(request.amount * (OFFSET - 1)),
                ),
                _ if c.exit == Exit::Cash => event(
                    d::WITHDRAWN,
                    Writer::new([0; 8])
                        .u64(request.id)
                        .key(&request.owner)
                        .u128(request.amount)
                        .u64((request.amount / OFFSET) as u64)
                        .u128(0),
                ),
                _ => event(
                    d::CLAIM_BOOKED,
                    Writer::new([0; 8])
                        .u64(request.id)
                        .key(&request.owner)
                        .u128(request.amount),
                ),
            };
            c.settlements += 1;
            let signature = Sig([0x50 + c.settlements as u8; 64]);
            let mut call = d::EVENT_CALL.to_vec();
            call.extend_from_slice(&said);
            let slot = c.tick;
            c.records.insert(
                signature,
                Record {
                    slot,
                    err: None,
                    logs: vec![],
                    calls: vec![(ids::VAULT, call)],
                },
            );
            c.signatures.entry(at).or_default().insert(0, signature);
            let bytes = vault_bytes(&c.vault);
            c.accounts.insert(k.vault, owned(bytes));
        }
    }
}

impl SolReader for Sim {
    async fn genesis(&self) -> Result<String, SolError> {
        Ok(self.0.borrow().genesis.clone())
    }

    async fn account(&self, key: &Pubkey) -> Result<Option<AccountData>, SolError> {
        if *key == Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111") {
            return Ok(None);
        }
        Ok(self.0.borrow().accounts.get(key).cloned())
    }

    async fn accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<AccountData>>, SolError> {
        Ok(keys
            .iter()
            .map(|key| self.0.borrow().accounts.get(key).cloned())
            .collect())
    }

    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Result<Vec<(Pubkey, AccountData)>, SolError> {
        Ok(self
            .0
            .borrow()
            .accounts
            .iter()
            .filter(|(_, account)| account.owner == *program)
            .filter(|(_, account)| {
                filters
                    .iter()
                    .all(|(at, bytes)| account.data.get(*at..at + bytes.len()) == Some(bytes))
            })
            .map(|(key, account)| (*key, account.clone()))
            .collect())
    }

    async fn blockhash(&self) -> Result<Blockhash, SolError> {
        let c = self.0.borrow();
        Ok(Blockhash {
            hash: Hash::new_from_array([c.tick as u8 + 1; 32]),
            last_valid: c.height + 150,
        })
    }

    async fn block_height(&self) -> Result<u64, SolError> {
        Ok(self.0.borrow().height)
    }

    async fn chain_time(&self) -> Result<u64, SolError> {
        Ok(self.0.borrow().time)
    }

    async fn simulate(&self, _wire: &[u8]) -> Result<Simulated, SolError> {
        Ok(match self.0.borrow_mut().simulation.take() {
            Some(failure) => Simulated::Failed(failure),
            None => Simulated::Ok { units: 100_000 },
        })
    }

    async fn send(&self, wire: &[u8]) -> Result<Sig, SolError> {
        let signature = Sig(wire[1..65].try_into().unwrap());
        self.0.borrow_mut().sent.push(wire.to_vec());
        if !self.0.borrow().drop_sends {
            self.land(wire, signature);
        }
        if self.0.borrow().lose_send_answers {
            return Err(SolError::Network(
                "the connection dropped before the node answered".to_string(),
            ));
        }
        Ok(signature)
    }

    async fn status(&self, signature: &Sig, _history: bool) -> Result<Option<Status>, SolError> {
        let mut c = self.0.borrow_mut();
        if c.unanswered_looks > 0 {
            c.unanswered_looks -= 1;
            return Err(SolError::Network("the node didn't answer".to_string()));
        }
        Ok(c.statuses.get(signature).cloned())
    }

    async fn signatures_for(&self, key: &Pubkey, _limit: usize) -> Result<Vec<Sig>, SolError> {
        Ok(self
            .0
            .borrow()
            .signatures
            .get(key)
            .cloned()
            .unwrap_or_default())
    }

    async fn transaction(&self, signature: &Sig) -> Result<Option<Record>, SolError> {
        Ok(self.0.borrow().records.get(signature).cloned())
    }
}

/// The wallet: a key, answering as the test says.
struct Wallet {
    sim: Sim,
    key: SolKey,
}

impl SolSigner for Wallet {
    fn account(&self) -> Option<Pubkey> {
        self.sim.0.borrow().wallet_account
    }

    fn name(&self) -> String {
        "Phantom".to_string()
    }

    fn supports(&self, chain: &str) -> bool {
        chain == "solana:localnet"
    }

    async fn sign(&self, unsigned: &Unsigned, _chain: &str) -> Result<Signed, SolError> {
        let answer = self.sim.0.borrow().answer;
        match answer {
            Answer::Reject => Err(SolError::Rejected),
            Answer::SignAfterSwitch => {
                self.sim.0.borrow_mut().wallet_account = Some(Pubkey::new_from_array([0x77; 32]));
                Ok(Signed::Wire(
                    unsigned
                        .signed(&[self.key.sign(unsigned.message())])
                        .unwrap(),
                ))
            }
            Answer::SignAfterCancel => {
                self.sim.0.borrow().cancel.cancel();
                Ok(Signed::Wire(
                    unsigned
                        .signed(&[self.key.sign(unsigned.message())])
                        .unwrap(),
                ))
            }
            Answer::Sign => Ok(Signed::Wire(
                unsigned
                    .signed(&[self.key.sign(unsigned.message())])
                    .unwrap(),
            )),
            Answer::SignAndSend => {
                let wire = unsigned
                    .signed(&[self.key.sign(unsigned.message())])
                    .unwrap();
                let signature = self.sim.send(&wire).await?;
                Ok(Signed::Sent(signature))
            }
            Answer::Alter => {
                // Same instructions, another blockhash.
                let mut message = unsigned.message().to_vec();
                let at = message.len() - 1;
                message[at] ^= 1;
                let signature = self.key.sign(&message);
                let mut wire = vec![1u8];
                wire.extend_from_slice(&signature);
                wire.extend_from_slice(&message);
                Ok(Signed::Wire(wire))
            }
        }
    }
}

struct Ticks(Sim);

impl Clock for Ticks {
    async fn sleep(&self, _millis: u32) {
        self.0.tick();
    }
}

fn local() -> Cluster {
    Cluster {
        name: "localnet".to_string(),
        chain: "solana:localnet".to_string(),
        mint: mint(),
        local_endpoint: true,
    }
}

/// Run a flow against `sim`, giving its end and every step it reported.
macro_rules! run {
    ($sim:expr, $cluster:expr, |$ctx:ident| $flow:expr) => {{
        let sim: &Sim = $sim;
        let steps = RefCell::new(Vec::new());
        let on_step = |step: Step| {
            if let Step::Signing(prompt) = &step {
                sim.0.borrow_mut().prompts.push(prompt.clone());
            }
            steps.borrow_mut().push(step);
        };
        let wallet = Wallet {
            sim: sim.clone(),
            key: depositor(),
        };
        let clock = Ticks(sim.clone());
        let cluster = $cluster;
        let $ctx = Ctx {
            reader: sim,
            signer: &wallet,
            clock: &clock,
            cluster: &cluster,
            vault: keys().vault,
            account: depositor().pubkey(),
            cancel: sim.0.borrow().cancel.clone(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let end = block_on($flow);
        drop($ctx);
        (end, steps.into_inner())
    }};
}

fn shown() -> [u8; 32] {
    read::restricted_hash(&[code("SANCTIONED"), code("US")])
}

// Deposits ----------------------------------------------------------------------------------

#[test]
fn a_deposit_states_the_list_shown_in_its_one_transaction_and_reports_the_shares_settled() {
    let sim = Sim::new();
    let (end, steps) = run!(&sim, local(), |ctx| flow::deposit(
        &ctx,
        1_000 * USD,
        shown()
    ));
    let Ok(Outcome::Deposited { assets, shares, .. }) = end else {
        panic!("{end:?}");
    };
    assert_eq!(assets, 1_000 * USD);
    assert_eq!(shares, u128::from(1_000 * USD) * (OFFSET - 1));
    // One transaction, one prompt, and the deposit in it is exactly the
    // builder's: the statement the hash of the list shown, at a fresh nonce.
    assert_eq!(sim.sends(), 1);
    assert_eq!(
        sim.0.borrow().prompts,
        vec![Prompt::Deposit {
            amount: 1_000 * USD
        }]
    );
    let account = depositor().pubkey();
    let deposit = ix::request_deposit(&keys(), &account, &account, 1_000 * USD, Some(shown()), 5);
    let sent = sim.last_sent();
    let vault_ixs: Vec<_> = sent.iter().filter(|(p, ..)| *p == ids::VAULT).collect();
    assert_eq!(vault_ixs.len(), 1);
    assert_eq!(vault_ixs[0].2, deposit.data);
    assert_eq!(
        vault_ixs[0].1,
        deposit
            .accounts
            .iter()
            .map(|m| m.pubkey)
            .collect::<Vec<_>>()
    );
    // The compute budget goes first, as the services send it.
    assert_eq!(sent[0].0, ids::COMPUTE_BUDGET);
    // It went through the countdown before it was done.
    assert!(steps.iter().any(|s| matches!(s, Step::Requested(_))));
}

#[test]
fn a_deposit_priced_and_settled_between_two_looks_reports_the_valuation_it_was_priced_at() {
    let sim = Sim::new();
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    let Ok(Outcome::Deposited { at, .. }) = end else {
        panic!("{end:?}");
    };
    // Requested at 1,003: the first ten-second mark at least five away.
    assert_eq!(at, 1_010);
}

#[test]
fn a_signature_given_after_the_flow_was_cancelled_is_never_sent() {
    let sim = Sim::new();
    sim.0.borrow_mut().answer = Answer::SignAfterCancel;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::Cancelled));
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_list_that_changed_since_it_was_shown_is_never_signed() {
    let sim = Sim::new();
    let mut other = shown();
    other[0] ^= 1;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, other));
    assert_eq!(end, Err(SolError::ListChanged));
    assert!(sim.0.borrow().prompts.is_empty());
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_list_the_vault_no_longer_holds_is_never_signed_even_if_it_was_shown() {
    let sim = Sim::new();
    sim.set_vault(|v| v.restricted_hash = [9; 32]);
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::ListChanged));
    assert_eq!(sim.sends(), 0);
}

#[test]
fn more_than_the_wallet_holds_or_under_the_minimum_is_refused_before_any_prompt() {
    let sim = Sim::new();
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(
        &ctx,
        6_000 * USD,
        shown()
    ));
    assert_eq!(
        end,
        Err(SolError::InsufficientBalance { have: 5_000 * USD })
    );
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 9 * USD, shown()));
    assert_eq!(end, Err(SolError::BelowMinimum { minimum: 10 * USD }));
    assert!(sim.0.borrow().prompts.is_empty());
}

#[test]
fn a_vault_that_isnt_open_takes_no_deposit() {
    let sim = Sim::new();
    sim.set_vault(|v| v.state = 2);
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongState));
}

#[test]
fn a_vault_whose_account_names_another_station_is_refused() {
    let sim = Sim::new();
    sim.set_vault(|v| v.station = Pubkey::new_from_array([0x13; 32]));
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongAccount));
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_transaction_that_would_fail_is_refused_in_words_before_the_wallet_is_asked() {
    let sim = Sim::new();
    sim.0.borrow_mut().simulation = Some(Failure {
        err: r#"{"InstructionError":[2,{"Custom":6003}]}"#.to_string(),
        logs: vec!["Program log: AnchorError occurred. Error Code: NoStatement. Error Number: 6003. Error Message: no.".to_string()],
    });
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(
        end,
        Err(SolError::WouldFail {
            reason: Reason::Vault("NoStatement")
        })
    );
    assert!(sim.0.borrow().prompts.is_empty());
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_rejection_sends_nothing() {
    let sim = Sim::new();
    sim.0.borrow_mut().answer = Answer::Reject;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::Rejected));
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_flow_built_for_another_account_never_asks_the_wallet() {
    let sim = Sim::new();
    sim.0.borrow_mut().wallet_account = Some(Pubkey::new_from_array([0x77; 32]));
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::AccountChanged));
    assert!(sim.0.borrow().prompts.is_empty());
}

#[test]
fn a_transaction_the_wallet_changed_before_signing_is_never_sent() {
    let sim = Sim::new();
    sim.0.borrow_mut().answer = Answer::Alter;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::Altered));
    assert_eq!(sim.sends(), 0);
}

#[test]
fn a_wallet_that_sends_itself_is_followed_to_the_same_outcome() {
    let sim = Sim::new();
    sim.0.borrow_mut().answer = Answer::SignAndSend;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert!(matches!(end, Ok(Outcome::Deposited { .. })), "{end:?}");
    assert_eq!(sim.sends(), 1);
}

#[test]
fn an_expired_blockhash_is_reported_and_the_transaction_is_never_sent_again() {
    let sim = Sim::new();
    sim.0.borrow_mut().drop_sends = true;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert!(matches!(end, Err(SolError::Expired { .. })), "{end:?}");
    assert_eq!(sim.sends(), 1);
    assert_eq!(sim.0.borrow().prompts.len(), 1);
}

#[test]
fn an_endpoint_answering_for_another_cluster_is_refused() {
    let sim = Sim::new();
    let devnet = Cluster {
        name: "devnet".to_string(),
        chain: "solana:devnet".to_string(),
        mint: mint(),
        local_endpoint: false,
    };
    let (end, _) = run!(&sim, devnet, |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongEndpoint));
    // A remote endpoint can't stand in for a local cluster either.
    let remote = Cluster {
        local_endpoint: false,
        ..local()
    };
    let (end, _) = run!(&sim, remote, |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongEndpoint));
}

#[test]
fn a_send_whose_answer_was_lost_and_that_landed_is_followed_to_its_outcome() {
    let sim = Sim::new();
    sim.0.borrow_mut().lose_send_answers = true;
    let (end, steps) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert!(matches!(end, Ok(Outcome::Deposited { .. })), "{end:?}");
    assert_eq!(sim.sends(), 1);
    assert_eq!(sim.0.borrow().prompts.len(), 1, "the wallet was asked once");
    assert!(steps.iter().any(|step| matches!(step, Step::Sent { .. })));
}

#[test]
fn a_send_whose_answer_was_lost_is_watched_until_it_can_no_longer_land_then_said_plainly() {
    let sim = Sim::new();
    {
        let mut c = sim.0.borrow_mut();
        c.lose_send_answers = true;
        c.drop_sends = true;
    }
    let (end, steps) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    // Followed as sent (never "the network didn't answer, try again") until
    // its blockhash passed: then it can't land, and that is what is said.
    assert!(steps.iter().any(|step| matches!(step, Step::Sent { .. })));
    let Err(error) = end else { panic!("{end:?}") };
    assert!(matches!(error, SolError::Expired { .. }), "{error:?}");
    assert!(!error.may_have_moved());
    assert_eq!(sim.sends(), 1, "sent again");
}

#[test]
fn a_status_look_the_node_doesnt_answer_is_looked_again_never_said_as_a_failure_to_retry() {
    let sim = Sim::new();
    {
        let mut c = sim.0.borrow_mut();
        c.lose_send_answers = true;
        c.unanswered_looks = 3;
    }
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert!(matches!(end, Ok(Outcome::Deposited { .. })), "{end:?}");
    assert_eq!(sim.sends(), 1);
}

#[test]
fn an_error_seen_before_confirmation_is_not_judged_until_the_cluster_confirms() {
    let sim = Sim::new();
    sim.0.borrow_mut().fork_first = true;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert!(matches!(end, Ok(Outcome::Deposited { .. })), "{end:?}");
}

#[test]
fn a_cluster_is_checked_by_its_genesis_under_either_name_and_a_local_one_is_never_a_public_one() {
    const MAINNET: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
    const DEVNET: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
    let sim = Sim::new();
    // `mainnet` is the cluster `mainnet-beta`: its endpoint is checked, not
    // refused for the name. (The wallet here offers only the local cluster.)
    sim.0.borrow_mut().genesis = MAINNET.to_string();
    let mainnet = Cluster {
        name: "mainnet".to_string(),
        chain: "solana:mainnet".to_string(),
        mint: mint(),
        local_endpoint: false,
    };
    let (end, _) = run!(&sim, mainnet.clone(), |ctx| flow::deposit(
        &ctx,
        100 * USD,
        shown()
    ));
    assert_eq!(end, Err(SolError::WrongCluster));
    sim.0.borrow_mut().genesis = DEVNET.to_string();
    let (end, _) = run!(&sim, mainnet, |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongEndpoint));
    // A local endpoint answering for a public cluster isn't a local cluster.
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::WrongEndpoint));
    assert_eq!(sim.sends(), 0);
}

/// A claim's close through a venue where the station holds eight markets
/// names more accounts than a legacy transaction carries: measured, it
/// doesn't fit, so it is never offered from the app. One market fits.
#[test]
fn a_transaction_larger_than_the_app_can_send_is_measured_as_not_fitting() {
    use chain::solana_ix::accounts::{Claim, ClaimState};
    use chain::solana_ix::vault::{Groups, Pinned, stand_in_group};
    let k = keys();
    let owner = depositor().pubkey();
    let claim = Claim {
        id: 3,
        owner,
        shares: WAD,
        booked_at: 1,
        last_attempt_at: 0,
        state: ClaimState::Pending,
        paid: 0,
        request: 0,
        payer: owner,
        held: 0,
    };
    let venue = Pubkey::new_from_array([0x31; 32]);
    let authority = address::venue_authority(&k.station, &venue);
    let pinned = Pinned {
        program: Pubkey::new_from_array([0x32; 32]),
        venue,
        authority,
        dollars: address::dollars(&authority, &k.mint),
    };
    let close = |markets: u8| {
        let held: Vec<Pubkey> = (0..markets)
            .map(|i| Pubkey::new_from_array([0x40 + i; 32]))
            .collect();
        let mut groups = Groups::none();
        groups.push(stand_in_group(&pinned, &k.mint, &held));
        vec![
            chain::solana_ix::budget::open_dollars(&flow::someone(), &owner, &k.mint),
            ix::settle_claim(&k, &flow::someone(), &claim.keys(), &groups),
        ]
    };
    assert!(flow::fits(&flow::someone(), close(1)));
    assert!(!flow::fits(&flow::someone(), close(8)));
}

// Withdrawals ---------------------------------------------------------------------------------

#[test]
fn a_withdrawal_paid_from_cash_reports_what_it_paid_and_opens_the_dollar_account_first_if_missing()
{
    let sim = Sim::new();
    let account = depositor().pubkey();
    sim.give_shares(&account, 100 * WAD);
    // No dollar account yet.
    sim.0
        .borrow_mut()
        .accounts
        .remove(&address::dollars(&account, &mint()));
    let (end, _) = run!(&sim, local(), |ctx| flow::withdraw(&ctx, 40 * WAD));
    let Ok(Outcome::Paid { shares, at, .. }) = end else {
        panic!("{end:?}");
    };
    assert_eq!(shares, 40 * WAD);
    assert!(at.is_some());
    let sent = sim.last_sent();
    let programs: Vec<Pubkey> = sent.iter().map(|(p, ..)| *p).collect();
    assert!(programs.contains(&ids::ASSOCIATED_TOKEN));
    let redeem = ix::request_redeem(&keys(), &account, 40 * WAD, &account, 5);
    assert!(
        sent.iter()
            .any(|(p, _, data)| *p == ids::VAULT && *data == redeem.data)
    );
}

#[test]
fn a_withdrawal_the_cash_cant_cover_is_booked_as_a_claim() {
    let sim = Sim::new();
    sim.0.borrow_mut().exit = Exit::Booked;
    let account = depositor().pubkey();
    sim.give_shares(&account, 100 * WAD);
    let (end, _) = run!(&sim, local(), |ctx| flow::withdraw(&ctx, 90 * WAD));
    assert!(
        matches!(end, Ok(Outcome::Booked { claim: 5, shares, .. }) if shares == 90 * WAD),
        "{end:?}"
    );
}

#[test]
fn more_shares_than_held_are_refused() {
    let sim = Sim::new();
    sim.give_shares(&depositor().pubkey(), WAD);
    let (end, _) = run!(&sim, local(), |ctx| flow::withdraw(&ctx, 2 * WAD));
    assert_eq!(end, Err(SolError::InsufficientShares { have: WAD }));
}

#[test]
fn a_closed_vault_pays_its_fraction_of_the_cash_at_once() {
    let sim = Sim::new();
    sim.set_vault(|v| v.state = 5);
    let account = depositor().pubkey();
    sim.give_shares(&account, 1_000 * WAD);
    let (end, steps) = run!(&sim, local(), |ctx| flow::withdraw(&ctx, 1_000 * WAD));
    assert_eq!(
        end.as_ref().map(|o| match o {
            Outcome::Paid { assets, at, .. } => (*assets, *at),
            _ => (0, Some(0)),
        }),
        Ok((1_000 * USD, None))
    );
    assert!(!steps.iter().any(|s| matches!(s, Step::Requested(_))));
    let closed = ix::redeem_closed(&keys(), &account, 1_000 * WAD, &account);
    assert!(
        sim.last_sent()
            .iter()
            .any(|(_, _, data)| *data == closed.data)
    );
}

#[test]
fn a_request_that_landed_and_isnt_settled_in_time_is_said_to_stand_never_that_nothing_was_sent() {
    let sim = Sim::new();
    sim.0.borrow_mut().keeper_delay = 1_000_000;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    let Err(error) = end else {
        panic!("{end:?}");
    };
    assert!(matches!(error, SolError::Pending { .. }), "{error:?}");
    assert!(error.after_send());
    assert!(error.signature().is_some());
    let words = error.words(&crate::tx::error::Context {
        wallet: "Phantom",
        chain: "Solana",
        wallet_chain: None,
        symbol: "USDC",
        decimals: 6,
        native: "SOL",
    });
    assert!(!words.title.contains("Nothing was sent") && !words.text.contains("Nothing was sent"));
}

#[test]
fn a_signature_from_an_account_the_wallet_has_since_left_is_never_sent() {
    let sim = Sim::new();
    sim.0.borrow_mut().answer = Answer::SignAfterSwitch;
    let (end, _) = run!(&sim, local(), |ctx| flow::deposit(&ctx, 100 * USD, shown()));
    assert_eq!(end, Err(SolError::AccountChanged));
    assert_eq!(sim.sends(), 0);
}
