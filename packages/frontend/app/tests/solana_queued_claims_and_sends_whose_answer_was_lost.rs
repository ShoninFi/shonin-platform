//! Two Solana depositor flows against a small simulated cluster and wallet,
//! through the app's public ports.
//!
//! 1. A claim the cash covered when it was tried is *queued*: the program
//!    made a request for it at the claim's own request address for the
//!    nonce the claim held before (0 the first time), and the claim now
//!    holds that request's queue id. The flows that act on a queued claim
//!    (its fallback, and the closed vault's payout of it) name that request,
//!    found by its id, and not an address derived from the id.
//!
//! 2. When the app sends a signed transaction itself and the node's answer
//!    is lost (a timeout, a dropped connection), the transaction may well
//!    have landed: the app holds its signature, so it follows it rather
//!    than end with an error that says to try again.

use std::cell::RefCell;
use std::collections::HashMap;

use app::tx::solana::SolError;
use app::tx::solana::flow::{self, Cluster, Ctx, Fallback, Outcome, Step};
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

struct VaultState {
    state: u8,
    total_supply: u128,
    cash: u64,
    request_count: u64,
    claim_count: u64,
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
        .key(&k.station)
        .bytes32([2; 32])
        .u64(1)
        .u16(100)
        .u16(100)
        .u16(1000)
        .u16(500)
        .u16(10)
        .u64(10)
        .u64(5)
        .u64(3)
        .u64(86_400)
        .n(6, |w| w.u64(0))
        .u64(10 * USD)
        .u32(0)
        .u8(255)
        .u8(v.state)
        .u64(0)
        .u64(0)
        .u8(0)
        .u128(v.total_supply)
        .u128(0)
        .u128(0)
        .u128(0)
        .u64(v.cash)
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
        .u64(v.request_count)
        .n(7, |w| w.u64(0))
        .u64(0)
        .u64(1_000)
        .u8(1)
        .u64(1)
        .u64(1_000)
        .u128(u128::from(v.cash) + 1)
        .u128(u128::from(v.cash) + 1)
        .u128(v.total_supply + OFFSET)
        .u128(0)
        .u128(0)
        .u64(v.cash)
        .u128(0)
        .u128(0)
        .n(3, |w| w.u128(0))
        .u64(0)
        .u64(v.claim_count)
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

fn holder_bytes(owner: &Pubkey, shares: u128) -> Vec<u8> {
    Writer::new(d::HOLDER)
        .key(&keys().vault)
        .key(owner)
        .u128(shares)
        .bytes32([3; 32])
        .u128(0)
        .u128(0)
        .u64(0)
        .u64(0)
        .u32(0)
        .key(owner)
        .u8(255)
        .0
}

/// A request of kind `kind` (2 is a queued claim's), `reference` its claim.
fn request_bytes(id: u64, owner: &Pubkey, kind: u8, amount: u128, reference: u64) -> Vec<u8> {
    Writer::new(d::REQUEST)
        .key(&keys().vault)
        .u64(id)
        .key(owner)
        .u8(kind)
        .u64(1_010)
        .u8(0)
        .u128(amount)
        .u64(reference)
        .key(owner)
        .u64(0)
        .key(&Pubkey::default())
        .u8(255)
        .bytes32([0; 32])
        .0[..8 + 32 + 8 + 32 + 1 + 8 + 1 + 16 + 8 + 32 + 8 + 32 + 1 + 16]
        .to_vec()
}

/// A claim: `state` 3 is queued; `request` the id of the request it was
/// queued by.
fn claim_bytes(id: u64, owner: &Pubkey, shares: u128, state: u8, request: u64) -> Vec<u8> {
    Writer::new(d::CLAIM)
        .u64(id)
        .key(owner)
        .u128(shares)
        .u64(900)
        .u64(950)
        .u8(state)
        .u64(0)
        .u64(request)
        .key(owner)
        .u64(0)
        .u8(255)
        .0
}

fn token_bytes(owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint().as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    data
}

fn owned(data: Vec<u8>) -> AccountData {
    AccountData {
        owner: ids::VAULT,
        lamports: 1_000_000,
        data,
    }
}

// A legacy transaction, read back ---------------------------------------------------

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

// The cluster and the wallet ----------------------------------------------------------

#[derive(Default)]
struct State {
    accounts: HashMap<Pubkey, AccountData>,
    /// Every transaction simulated, as its wire bytes.
    simulated: Vec<Vec<u8>>,
    /// Every transaction the node took.
    sent: Vec<Vec<u8>>,
    statuses: HashMap<Sig, Status>,
    records: HashMap<Sig, Record>,
    /// Simulations answer "would fail", so a flow stops before signing and
    /// the transaction it built can be read.
    stop_at_simulation: bool,
    /// The node takes every send but its answer never reaches the app.
    lose_send_answer: bool,
    /// What a landed transaction's logs say.
    landed_logs: Vec<String>,
}

struct Sim(RefCell<State>);

fn signature_of(wire: &[u8]) -> Sig {
    Sig(wire[1..65].try_into().unwrap())
}

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
        Ok(1_003)
    }
    async fn simulate(&self, wire: &[u8]) -> Result<Simulated, SolError> {
        let mut s = self.0.borrow_mut();
        s.simulated.push(wire.to_vec());
        if s.stop_at_simulation {
            return Ok(Simulated::Failed(Failure {
                err: "stopped here, to read what was built".to_string(),
                logs: Vec::new(),
            }));
        }
        Ok(Simulated::Ok { units: 1 })
    }
    async fn send(&self, wire: &[u8]) -> Result<Sig, SolError> {
        let mut s = self.0.borrow_mut();
        let signature = signature_of(wire);
        s.sent.push(wire.to_vec());
        s.statuses.insert(
            signature,
            Status {
                slot: 7,
                err: None,
                confirmed: true,
            },
        );
        let logs = s.landed_logs.clone();
        s.records.insert(
            signature,
            Record {
                slot: 7,
                err: None,
                logs,
                calls: Vec::new(),
            },
        );
        if s.lose_send_answer {
            return Err(SolError::Network(
                "the connection dropped before the node answered".to_string(),
            ));
        }
        Ok(signature)
    }
    async fn status(&self, signature: &Sig, _history: bool) -> Result<Option<Status>, SolError> {
        Ok(self.0.borrow().statuses.get(signature).cloned())
    }
    async fn signatures_for(&self, _key: &Pubkey, _limit: usize) -> Result<Vec<Sig>, SolError> {
        Ok(Vec::new())
    }
    async fn transaction(&self, signature: &Sig) -> Result<Option<Record>, SolError> {
        Ok(self.0.borrow().records.get(signature).cloned())
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

/// A cluster holding the vault in `state`, the depositor's dollar account,
/// and a claim of theirs (id 9, 100 shares) that was queued as request 13:
/// the program made that request at the claim's request address for nonce
/// 0, and the claim's account now holds `request = 13`.
fn with_a_queued_claim(state: u8) -> (Sim, Pubkey) {
    let k = keys();
    let me = depositor().pubkey();
    let claim_address = address::claim(&k.vault, 9);
    let request_address = address::request(&k.vault, &claim_address, 0);
    let mut accounts = HashMap::new();
    accounts.insert(
        k.vault,
        owned(vault_bytes(&VaultState {
            state,
            total_supply: 10_000 * WAD,
            cash: 10_000 * USD,
            request_count: 13,
            claim_count: 1,
        })),
    );
    accounts.insert(claim_address, owned(claim_bytes(9, &me, 100 * WAD, 3, 13)));
    accounts.insert(
        request_address,
        owned(request_bytes(13, &me, 2, 100 * WAD, 9)),
    );
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
        stop_at_simulation: true,
        ..State::default()
    }));
    (sim, request_address)
}

/// The vault-program instruction with discriminator `tag` in the last
/// transaction simulated.
fn built(sim: &Sim, tag: [u8; 8]) -> Vec<Pubkey> {
    let s = sim.0.borrow();
    let wire = s.simulated.last().expect("a transaction was built");
    instructions(wire)
        .into_iter()
        .find(|(program, _, data)| *program == ids::VAULT && data[..8] == tag)
        .map(|(_, accounts, _)| accounts)
        .expect("the vault instruction was in it")
}

const TAKE_PRO_RATA_CLAIM: [u8; 8] = [224, 187, 162, 186, 238, 88, 176, 69];
const CLAIM_CLOSED: [u8; 8] = [254, 56, 145, 210, 191, 133, 210, 86];

#[test]
fn a_queued_claim_s_fallback_names_the_request_that_exists() {
    let (sim, request_address) = with_a_queued_claim(1);
    let end = run!(&sim, |ctx| flow::take_cash(&ctx, Fallback::Claim(9)));
    assert!(
        matches!(end, Err(SolError::WouldFail { .. })),
        "this cluster stops every flow at simulation: {end:?}"
    );
    // take_pro_rata_claim: owner, the pool's eight, the claim, then the
    // request a queued claim is priced by.
    let accounts = built(&sim, TAKE_PRO_RATA_CLAIM);
    assert_eq!(
        accounts[10], request_address,
        "the fallback must name the request the program made (at nonce 0, holding id 13); \
         an address derived from the id names an account that doesn't exist"
    );
    assert!(
        sim.0.borrow().accounts.contains_key(&accounts[10]),
        "the request account named must exist on the cluster"
    );
}

#[test]
fn a_queued_claim_is_collected_from_the_closed_vault_through_the_request_that_exists() {
    let (sim, request_address) = with_a_queued_claim(5);
    let end = run!(&sim, |ctx| flow::settle_claim(&ctx, 9));
    assert!(
        matches!(end, Err(SolError::WouldFail { .. })),
        "this cluster stops every flow at simulation: {end:?}"
    );
    // claim_closed: vault, station, the claim, then a queued claim's request.
    let accounts = built(&sim, CLAIM_CLOSED);
    assert_eq!(
        accounts[3], request_address,
        "a closed vault's payout of a queued claim must name the request the program made"
    );
}

#[test]
fn a_send_whose_answer_was_lost_is_followed_not_called_a_failure_to_try_again() {
    let k = keys();
    let me = depositor().pubkey();
    let shares = 100 * WAD;
    let mut accounts = HashMap::new();
    accounts.insert(
        k.vault,
        owned(vault_bytes(&VaultState {
            state: 5,
            total_supply: 10_000 * WAD,
            cash: 10_000 * USD,
            request_count: 13,
            claim_count: 0,
        })),
    );
    accounts.insert(k.holder(&me), owned(holder_bytes(&me, shares)));
    accounts.insert(
        address::dollars(&me, &mint()),
        AccountData {
            owner: ids::TOKEN,
            lamports: 2_039_280,
            data: token_bytes(&me, 5 * USD),
        },
    );
    // What the closed vault's payout logs once it lands.
    let withdrawn = Writer::new(d::WITHDRAWN)
        .u64(0)
        .key(&me)
        .u128(shares)
        .u64(100 * USD)
        .u128(0)
        .0;
    let landed_logs = vec![
        format!("Program {} invoke [1]", ids::VAULT),
        format!(
            "Program data: {}",
            app::wallet::solana::rpc::base64_encode(&withdrawn)
        ),
        format!("Program {} success", ids::VAULT),
    ];
    let sim = Sim(RefCell::new(State {
        accounts,
        lose_send_answer: true,
        landed_logs,
        ..State::default()
    }));
    let end = run!(&sim, |ctx| flow::withdraw(&ctx, shares));
    // The node took the transaction: it is confirmed, and the app knows its
    // signature. Either it is followed to its outcome, or what is said must
    // be an error that owns up to the send, never one that offers a resend.
    assert_eq!(sim.0.borrow().sent.len(), 1);
    match &end {
        Ok(Outcome::Paid { assets, .. }) => assert_eq!(*assets, 100 * USD),
        Ok(other) => panic!("an unexpected outcome: {other:?}"),
        Err(error) => assert!(
            error.after_send(),
            "a send the node may have taken was reported as {error:?}, which the page shows with \
             'try again': a second payout request would follow"
        ),
    }
}

#[test]
fn a_queued_claim_whose_request_cant_be_found_builds_nothing() {
    let (sim, request_address) = with_a_queued_claim(1);
    sim.0.borrow_mut().accounts.remove(&request_address);
    let end = run!(&sim, |ctx| flow::take_cash(&ctx, Fallback::Claim(9)));
    assert!(matches!(end, Err(SolError::Network(_))), "{end:?}");
    assert!(
        sim.0.borrow().simulated.is_empty(),
        "a transaction was built without the request"
    );
}

#[test]
fn a_payout_that_landed_and_whose_record_couldnt_be_read_is_never_offered_again() {
    let k = keys();
    let me = depositor().pubkey();
    let shares = 100 * WAD;
    let mut accounts = HashMap::new();
    accounts.insert(
        k.vault,
        owned(vault_bytes(&VaultState {
            state: 5,
            total_supply: 10_000 * WAD,
            cash: 10_000 * USD,
            request_count: 13,
            claim_count: 0,
        })),
    );
    accounts.insert(k.holder(&me), owned(holder_bytes(&me, shares)));
    accounts.insert(
        address::dollars(&me, &mint()),
        AccountData {
            owner: ids::TOKEN,
            lamports: 2_039_280,
            data: token_bytes(&me, 5 * USD),
        },
    );
    // It lands, but says nothing the app can read.
    let sim = Sim(RefCell::new(State {
        accounts,
        ..State::default()
    }));
    let end = run!(&sim, |ctx| flow::withdraw(&ctx, shares));
    assert_eq!(sim.0.borrow().sent.len(), 1);
    let Err(error) = end else { panic!("{end:?}") };
    assert!(
        error.after_send() && error.may_have_moved(),
        "a payout that landed was reported as {error:?}"
    );
}
