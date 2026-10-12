//! A simulated vault chain and wallet, so every flow's sequence is checked
//! natively: what is read, what is signed, what is sent and in which order,
//! and what each wallet or chain answer leads to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use chain::bindings::erc20::IERC20;
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

use super::error::{Revert, TxError};
use super::flow::{self, Cancel, Ctx, Fallback, Nudge, Outcome, Pace, Prompt, Step};
use super::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use super::read::{ClaimState, State};

const CHAIN: u64 = 31337;
const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;

fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn asset() -> Address {
    Address::repeat_byte(0xdd)
}

/// What the keeper does on its own once a request is made.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keeper {
    /// Records the valuation and settles in the same breath.
    PricesAndSettles,
    /// Records the valuation and leaves settling to someone else.
    PricesOnly,
    /// Misses this many slots first, then prices and settles.
    MissesSlots(u32),
    /// Prices at once, and settles only this many looks later.
    SettlesLate(u32),
}

/// How a withdrawal settles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    Cash,
    Booked,
}

struct Chain {
    time: u64,
    block: u64,
    state: b::VaultState,
    restricted: Vec<B256>,
    restricted_text: String,
    statement_of: HashMap<Address, B256>,
    usd: HashMap<Address, U256>,
    native: HashMap<Address, U256>,
    allowance: HashMap<(Address, Address), U256>,
    shares: HashMap<Address, U256>,
    minimum: U256,
    requests: Vec<(Address, u8, u64, bool, U256)>,
    claims: Vec<b::Claim>,
    priced: Option<u64>,
    keeper: Keeper,
    exit: Exit,
    misses: u32,
    receipts: HashMap<B256, Receipt>,
    logs: Vec<(u64, Log)>,
    nonce: HashMap<Address, u64>,
    /// Sends the wallet made, in order.
    sent: Vec<TxRequest>,
    /// Calls of anything that moves money, by selector, in order.
    calls: Vec<[u8; 4]>,
    /// The chain the wallet is on.
    wallet_chain: u64,
    /// The account the wallet offers now.
    wallet_account: Option<Address>,
    /// How the wallet answers its next prompt.
    wallet_rejects: bool,
    /// Sign with another key than the account's.
    wallet_signs_wrong: bool,
    /// Report a digest other than the app's from `statementDigest`.
    digest_differs: bool,
    /// Mined transactions revert.
    reverts: bool,
    /// Sent transactions are never mined nor known.
    drops: bool,
    held: U256,
    units_claimable: U256,
    polls: u32,
    /// Declines this many second-step prompts (`settle`), then approves.
    declines_settle: u32,
    /// Every read of the vault fails once a request is on the chain.
    reads_fail_after_request: bool,
    /// A sent transaction the endpoint doesn't see until this many looks
    /// for its receipt, then mined.
    lands_after_looks: Option<u32>,
    unseen: Vec<(B256, TxRequest)>,
    receipt_looks: u32,
    /// Look-ups of a request's state, so a test can see the flow watched.
    request_looks: u32,
}

type Sim = Rc<RefCell<Chain>>;

struct World {
    sim: Sim,
    key: PrivateKeySigner,
    /// The person presses "Settle now" whenever the page offers it.
    presses_settle_now: bool,
}

impl World {
    fn new() -> Self {
        let key = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).unwrap();
        let account = key.address();
        let mut usd = HashMap::new();
        usd.insert(account, U256::from(4_250u64 * USD));
        usd.insert(vault(), U256::from(131_870u64 * USD));
        let mut native = HashMap::new();
        native.insert(account, U256::from(10u64).pow(U256::from(18u8)));
        let mut shares = HashMap::new();
        shares.insert(account, U256::from(8_771u128 * WAD));
        let chain = Chain {
            time: 14 * 3600 + 32 * 60 + 33,
            block: 100,
            state: b::VaultState::Open,
            restricted: vec![
                B256::right_padding_from(b"SANCTIONED"),
                B256::right_padding_from(b"US"),
            ],
            restricted_text: "SANCTIONED,US".to_string(),
            statement_of: HashMap::new(),
            usd,
            native,
            allowance: HashMap::new(),
            shares,
            minimum: U256::from(10 * USD),
            requests: Vec::new(),
            claims: Vec::new(),
            priced: None,
            keeper: Keeper::PricesAndSettles,
            exit: Exit::Cash,
            misses: 0,
            receipts: HashMap::new(),
            logs: Vec::new(),
            nonce: HashMap::new(),
            sent: Vec::new(),
            calls: Vec::new(),
            wallet_chain: CHAIN,
            wallet_account: Some(account),
            wallet_rejects: false,
            wallet_signs_wrong: false,
            digest_differs: false,
            reverts: false,
            drops: false,
            held: U256::ZERO,
            units_claimable: U256::ZERO,
            polls: 0,
            declines_settle: 0,
            reads_fail_after_request: false,
            lands_after_looks: None,
            unseen: Vec::new(),
            receipt_looks: 0,
            request_looks: 0,
        };
        Self {
            sim: Rc::new(RefCell::new(chain)),
            key,
            presses_settle_now: false,
        }
    }

    fn account(&self) -> Address {
        self.key.address()
    }

    fn with(&self, change: impl FnOnce(&mut Chain)) -> &Self {
        change(&mut self.sim.borrow_mut());
        self
    }

    /// Run a flow and collect every step it reported.
    fn run(
        &self,
        flow: impl for<'a> FnOnce(
            &'a Ctx<'a, SimReader, SimWallet, NoWait>,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<Outcome, TxError>> + 'a>,
        >,
    ) -> (Result<Outcome, TxError>, Vec<Step>) {
        let steps = RefCell::new(Vec::new());
        let settle_now = Nudge::new();
        let on_step = |step: Step| {
            if self.presses_settle_now
                && matches!(
                    step,
                    Step::Priced {
                        unsent: Some(_),
                        ..
                    }
                )
            {
                settle_now.nudge();
            }
            steps.borrow_mut().push(step);
        };
        let reader = SimReader(self.sim.clone());
        let wallet = SimWallet {
            sim: self.sim.clone(),
            key: self.key.clone(),
        };
        let ctx = Ctx {
            reader: &reader,
            signer: &wallet,
            clock: &NoWait,
            chain_id: CHAIN,
            vault: vault(),
            asset: asset(),
            account: self.account(),
            cancel: Cancel::new(),
            settle_now: settle_now.clone(),
            skip_second: Nudge::new(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let result = block_on(flow(&ctx));
        (result, steps.into_inner())
    }

    fn sent_selectors(&self) -> Vec<[u8; 4]> {
        self.sim.borrow().calls.clone()
    }
}

struct NoWait;
impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

struct SimReader(Sim);

fn ok<T: SolValue>(value: T) -> Result<Bytes, TxError> {
    Ok(Bytes::from(value.abi_encode()))
}

impl Chain {
    /// The keeper's clockwork, run on each poll of a request.
    fn tick(&mut self) {
        self.polls += 1;
        let Some(index) = self.requests.iter().position(|r| !r.3) else {
            return;
        };
        let earliest = self.requests[index].2;
        if let Keeper::MissesSlots(slots) = self.keeper
            && self.misses < slots
        {
            // Its slot passes unrecorded.
            self.misses += 1;
            self.time = earliest + u64::from(self.misses - 1) * 10 + 4;
            return;
        }
        self.time = self.time.max(earliest + 1);
        self.priced = Some(earliest + u64::from(self.misses) * 10);
        match self.keeper {
            Keeper::PricesOnly => {}
            Keeper::SettlesLate(looks) => {
                if self.misses >= looks {
                    self.settle_all();
                }
                self.misses += 1;
                self.priced = Some(earliest);
            }
            _ => self.settle_all(),
        }
    }

    fn settle_all(&mut self) {
        let account_events: Vec<_> = self
            .requests
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.3)
            .map(|(i, r)| (i, *r))
            .collect();
        for (i, (owner, kind, _, _, amount)) in account_events {
            self.requests[i].3 = true;
            let id = U256::from(i + 1);
            if kind == 2 {
                // A claim's own request: paid with its claim, elsewhere.
                continue;
            }
            if kind == 0 {
                // A dollar a share, less the 0.1% fee.
                let shares =
                    (amount - amount / U256::from(1000u32)) * U256::from(1_000_000_000_000u64);
                *self.shares.entry(owner).or_default() += shares;
                self.log(IVault::Deposited {
                    requestId: id,
                    owner,
                    assets: amount,
                    shares,
                });
            } else if self.exit == Exit::Cash {
                let assets = amount / U256::from(1_000_000_000_000u64);
                let assets = assets - assets.div_ceil(U256::from(1000u32));
                self.log(IVault::Withdrawn {
                    requestId: id,
                    owner,
                    shares: amount,
                    assets,
                    feeShares: U256::ZERO,
                });
            } else {
                self.claims.push(b::Claim {
                    owner,
                    shares: amount,
                    bookedAt: self.time,
                    lastAttemptAt: 0,
                    state: b::ClaimState::Pending,
                    paid: U256::ZERO,
                });
                self.log(IVault::ClaimBooked {
                    claimId: U256::from(self.claims.len()),
                    owner,
                    shares: amount,
                });
            }
        }
    }

    fn log<E: SolEvent>(&mut self, event: E) {
        let data = event.encode_log_data();
        self.logs.push((
            self.block,
            Log {
                address: vault(),
                topics: data.topics().to_vec(),
                data: data.data.clone(),
                transaction: None,
            },
        ));
    }

    fn call(&mut self, to: Address, data: &[u8]) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if self.reads_fail_after_request && !self.requests.is_empty() {
            return Err(TxError::Network(
                "the chain answered a read with nothing".to_string(),
            ));
        }
        if to == asset() {
            return match selector {
                IERC20::balanceOfCall::SELECTOR => {
                    let call = IERC20::balanceOfCall::abi_decode(data).unwrap();
                    ok(self.usd.get(&call.account).copied().unwrap_or_default())
                }
                IERC20::allowanceCall::SELECTOR => {
                    let call = IERC20::allowanceCall::abi_decode(data).unwrap();
                    ok(self
                        .allowance
                        .get(&(call.owner, call.spender))
                        .copied()
                        .unwrap_or_default())
                }
                _ => panic!("unexpected dollar read"),
            };
        }
        match selector {
            IVault::stateCall::SELECTOR => ok(self.state),
            IVault::feesCall::SELECTOR => ok(b::Fees {
                holdingBps: 100,
                authorBps: 1000,
                platformBps: 500,
                transactionBps: 10,
            }),
            IVault::timersCall::SELECTOR => ok(b::Timers {
                slotInterval: 10,
                slotLead: 5,
                slotWindow: 3,
                postDeadline: 60,
                transitTimeout: 3600,
                fallbackAfter: 86_400,
                usageFreshness: 3600,
                tankEscapeDelay: 86_400,
                floorDelay: 86_400,
                haltGrace: 600,
                freezeAfter: 604_800,
                windDown: 604_800,
                activationWindow: 259_200,
            }),
            IVault::cashCall::SELECTOR => ok(self.usd[&vault()]),
            IVault::totalSupplyCall::SELECTOR => ok(U256::from(100_000u128 * WAD)),
            IVault::minimumDepositCall::SELECTOR => ok(self.minimum),
            IVault::readyToCloseCall::SELECTOR => ok(false),
            IVault::lastPricedCall::SELECTOR => match self.priced {
                Some(time) => ok((true, U256::ZERO, time)),
                None => ok((false, U256::ZERO, 0u64)),
            },
            IVault::valuationAtCall::SELECTOR => ok(b::Valuation {
                time: self.priced.unwrap_or_default(),
                status: b::ValuationStatus::Priced,
                gross: U256::from(100_000u64 * USD + 1),
                grossExit: U256::from(100_000u64 * USD + 1),
                supply: U256::from(100_000u128 * WAD) + U256::from(1_000_000_000_000u64),
                fee: U256::ZERO,
                feeExit: U256::ZERO,
                cash: self.usd[&vault()],
                postsMissing: 0,
            }),
            IVault::balanceOfCall::SELECTOR => {
                let call = IVault::balanceOfCall::abi_decode(data).unwrap();
                ok(self.shares.get(&call.owner).copied().unwrap_or_default())
            }
            IVault::statementOfCall::SELECTOR => {
                let call = IVault::statementOfCall::abi_decode(data).unwrap();
                ok(self
                    .statement_of
                    .get(&call.depositor)
                    .copied()
                    .unwrap_or_default())
            }
            IVault::restrictedCall::SELECTOR => ok(self.restricted.clone()),
            IVault::restrictedTextCall::SELECTOR => ok(self.restricted_text.clone()),
            IVault::statementDigestCall::SELECTOR => {
                let call = IVault::statementDigestCall::abi_decode(data).unwrap();
                let mut digest =
                    chain::statement::digest(CHAIN, vault(), call.depositor, &self.restricted_text);
                if self.digest_differs {
                    digest = keccak256(digest);
                }
                ok(digest)
            }
            IVault::requestAtCall::SELECTOR => {
                self.request_looks += 1;
                self.tick();
                let call = IVault::requestAtCall::abi_decode(data).unwrap();
                let (owner, kind, earliest, done, amount) =
                    self.requests[usize::try_from(call.requestId).unwrap() - 1];
                ok(b::Request {
                    owner,
                    kind: kind_of(kind),
                    earliest,
                    done,
                    amount,
                    r#ref: U256::ZERO,
                })
            }
            IVault::claimAtCall::SELECTOR => {
                let call = IVault::claimAtCall::abi_decode(data).unwrap();
                ok(self.claims[usize::try_from(call.claimId).unwrap() - 1].clone())
            }
            IVault::unitsOfCall::SELECTOR => ok((U256::from(5u8), self.units_claimable)),
            IVault::requestCountCall::SELECTOR => ok(U256::from(self.requests.len())),
            IVault::openedBlockCall::SELECTOR => ok(0u64),
            IVault::heldForCall::SELECTOR => ok(self.held),
            _ => panic!("unexpected vault read {selector:?}"),
        }
    }

    /// Run a sent transaction: what it changes, and what it logs.
    fn execute(&mut self, request: &TxRequest) -> bool {
        let data = &request.data;
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        self.calls.push(selector);
        if self.reverts {
            return false;
        }
        let from = request.from;
        if request.to == asset() {
            let call = IERC20::approveCall::abi_decode(data).unwrap();
            self.allowance.insert((from, call.spender), call.value);
            return true;
        }
        match selector {
            IVault::requestDepositCall::SELECTOR => {
                let call = IVault::requestDepositCall::abi_decode(data).unwrap();
                let hash = keccak256(self.restricted.abi_encode());
                if !call.statement.is_empty() {
                    self.statement_of.insert(call.receiver, hash);
                }
                if self.statement_of.get(&call.receiver) != Some(&hash) {
                    return false;
                }
                let allowed = self
                    .allowance
                    .get(&(from, vault()))
                    .copied()
                    .unwrap_or_default();
                if allowed < call.assets {
                    return false;
                }
                self.allowance
                    .insert((from, vault()), allowed - call.assets);
                *self.usd.get_mut(&from).unwrap() -= call.assets;
                self.request(call.receiver, 0, call.assets);
            }
            IVault::requestRedeemCall::SELECTOR => {
                let call = IVault::requestRedeemCall::abi_decode(data).unwrap();
                *self.shares.get_mut(&from).unwrap() -= call.shares;
                self.request(call.receiver, 1, call.shares);
            }
            IVault::settleCall::SELECTOR => {
                // Settling prices nothing: only what a valuation priced.
                if self.priced.is_some() {
                    self.settle_all();
                }
            }
            IVault::settleClaimCall::SELECTOR => {
                let call = IVault::settleClaimCall::abi_decode(data).unwrap();
                let claim = &mut self.claims[usize::try_from(call.claimId).unwrap() - 1];
                claim.state = b::ClaimState::Paid;
                claim.paid = U256::from(2_501u64 * USD);
            }
            IVault::claimClosedCall::SELECTOR => {
                let call = IVault::claimClosedCall::abi_decode(data).unwrap();
                let claim = &mut self.claims[usize::try_from(call.claimId).unwrap() - 1];
                claim.state = b::ClaimState::Paid;
                claim.paid = U256::from(900u64 * USD);
            }
            IVault::takeProRataCall::SELECTOR
                if self
                    .requests
                    .get(
                        usize::try_from(
                            IVault::takeProRataCall::abi_decode(data).unwrap().requestId,
                        )
                        .unwrap()
                            - 1,
                    )
                    .is_some_and(|request| request.1 == 0) =>
            {
                // On a deposit the fallback sends the money back, to the
                // wallet that paid it in, and records a refund.
                let call = IVault::takeProRataCall::abi_decode(data).unwrap();
                let index = usize::try_from(call.requestId).unwrap() - 1;
                let (owner, _, _, _, assets) = self.requests[index];
                self.requests[index].3 = true;
                *self.usd.entry(owner).or_default() += assets;
                self.log(IVault::Refunded {
                    requestId: call.requestId,
                    to: owner,
                    assets,
                });
            }
            IVault::settleClosedCall::SELECTOR => {
                let call = IVault::settleClosedCall::abi_decode(data).unwrap();
                let index = usize::try_from(call.requestId).unwrap() - 1;
                let (owner, kind, _, _, amount) = self.requests[index];
                self.requests[index].3 = true;
                if kind == 0 {
                    self.log(IVault::Refunded {
                        requestId: call.requestId,
                        to: owner,
                        assets: amount,
                    });
                } else {
                    self.log(IVault::Withdrawn {
                        requestId: call.requestId,
                        owner,
                        shares: amount,
                        assets: U256::from(1_234u64 * USD),
                        feeShares: U256::ZERO,
                    });
                }
            }
            IVault::takeProRataCall::SELECTOR | IVault::takeProRataClaimCall::SELECTOR => {
                self.log(IVault::ProRataTaken {
                    owner: from,
                    shares: U256::from(WAD),
                    assets: U256::from(400_000u64),
                    units: U256::from(7u8),
                });
            }
            IVault::claimInsteadCall::SELECTOR => {
                self.claims.push(b::Claim {
                    owner: from,
                    shares: U256::from(WAD),
                    bookedAt: self.time,
                    lastAttemptAt: 0,
                    state: b::ClaimState::Pending,
                    paid: U256::ZERO,
                });
                self.log(IVault::ClaimBooked {
                    claimId: U256::from(self.claims.len()),
                    owner: from,
                    shares: U256::from(WAD),
                });
            }
            IVault::collectUnitsCall::SELECTOR => {
                let assets = self.units_claimable;
                self.units_claimable = U256::ZERO;
                self.log(IVault::UnitsPaid {
                    owner: from,
                    assets,
                });
            }
            IVault::collectHeldCall::SELECTOR => {
                let value = self.held;
                self.held = U256::ZERO;
                let data = IERC20::Transfer {
                    from: vault(),
                    to: from,
                    value,
                }
                .encode_log_data();
                self.logs.push((
                    self.block,
                    Log {
                        address: asset(),
                        topics: data.topics().to_vec(),
                        data: data.data.clone(),
                        transaction: None,
                    },
                ));
            }
            IVault::redeemClosedCall::SELECTOR => {
                let call = IVault::redeemClosedCall::abi_decode(data).unwrap();
                self.log(IVault::Withdrawn {
                    requestId: U256::ZERO,
                    owner: call.receiver,
                    shares: call.shares,
                    assets: U256::from(1_234u64 * USD),
                    feeShares: U256::ZERO,
                });
            }
            _ => panic!("unexpected send {selector:?}"),
        }
        true
    }

    fn request(&mut self, owner: Address, kind: u8, amount: U256) {
        let earliest = (self.time + 5).div_ceil(10) * 10;
        self.requests.push((owner, kind, earliest, false, amount));
        self.log(IVault::Requested {
            requestId: U256::from(self.requests.len()),
            owner,
            kind: kind_of(kind),
            earliest,
            amount,
        });
    }
}

/// The sim's kinds: 0 a deposit, 1 a withdrawal, 2 a claim's own request,
/// 3 a tank fill.
fn kind_of(kind: u8) -> b::RequestKind {
    match kind {
        0 => b::RequestKind::Deposit,
        1 => b::RequestKind::Redeem,
        2 => b::RequestKind::Claim,
        _ => b::RequestKind::TankFill,
    }
}

impl Chain {
    /// Mine a sent transaction: run it and keep its receipt.
    fn mine(&mut self, hash: B256, request: &TxRequest) {
        self.block += 1;
        let first = self.logs.len();
        let success = self.execute(request);
        let logs = if success {
            self.logs[first..]
                .iter()
                .map(|(_, log)| log.clone())
                .collect()
        } else {
            self.logs.truncate(first);
            Vec::new()
        };
        let block = self.block;
        *self.nonce.entry(request.from).or_default() += 1;
        self.receipts.insert(
            hash,
            Receipt {
                hash,
                success,
                block,
                logs,
            },
        );
    }
}

impl Reader for SimReader {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        self.0.borrow_mut().call(to, &data)
    }
    async fn native_balance(&self, who: Address) -> Result<U256, TxError> {
        Ok(self
            .0
            .borrow()
            .native
            .get(&who)
            .copied()
            .unwrap_or_default())
    }
    async fn estimate_gas(&self, request: &TxRequest) -> Result<u64, TxError> {
        // The vault refuses a deposit without a statement for its list.
        let sim = self.0.borrow();
        if request.data[..4] == IVault::requestDepositCall::SELECTOR {
            let call = IVault::requestDepositCall::abi_decode(&request.data).unwrap();
            let hash = keccak256(sim.restricted.abi_encode());
            if call.statement.is_empty() && sim.statement_of.get(&call.receiver) != Some(&hash) {
                return Err(TxError::WouldRevert {
                    reason: Revert::Vault("NoStatement"),
                });
            }
        }
        Ok(100_000)
    }
    async fn gas_price(&self) -> Result<U256, TxError> {
        Ok(U256::from(1_000_000_000u64))
    }
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        let mut sim = self.0.borrow_mut();
        sim.receipt_looks += 1;
        if let Some(after) = sim.lands_after_looks
            && sim.receipt_looks >= after
        {
            for (hash, request) in std::mem::take(&mut sim.unseen) {
                sim.mine(hash, &request);
            }
        }
        Ok(sim.receipts.get(&hash).cloned())
    }
    async fn pending(&self, _hash: B256) -> Result<Pending, TxError> {
        Ok(Pending::Unknown)
    }
    async fn mined_nonce(&self, who: Address) -> Result<u64, TxError> {
        Ok(self.0.borrow().nonce.get(&who).copied().unwrap_or_default())
    }
    async fn chain_time(&self) -> Result<u64, TxError> {
        Ok(self.0.borrow().time)
    }
    async fn block_time(&self, _block: u64) -> Result<u64, TxError> {
        Ok(self.0.borrow().time)
    }
    async fn logs(
        &self,
        address: Address,
        topic0: B256,
        topic1: Option<B256>,
        from_block: u64,
    ) -> Result<Vec<Log>, TxError> {
        Ok(self
            .0
            .borrow()
            .logs
            .iter()
            .filter(|(block, log)| {
                *block >= from_block
                    && log.address == address
                    && log.topics.first() == Some(&topic0)
                    && topic1.is_none_or(|topic| log.topics.get(1) == Some(&topic))
            })
            .map(|(_, log)| log.clone())
            .collect())
    }
}

struct SimWallet {
    sim: Sim,
    key: PrivateKeySigner,
}

impl Signer for SimWallet {
    fn account(&self) -> Option<Address> {
        self.sim.borrow().wallet_account
    }
    fn name(&self) -> String {
        "MetaMask".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(self.sim.borrow().wallet_chain)
    }
    async fn sign_statement(&self, statement: &StatementRequest) -> Result<Bytes, TxError> {
        let sim = self.sim.borrow();
        if sim.wallet_rejects {
            return Err(TxError::Rejected);
        }
        let key = if sim.wallet_signs_wrong {
            PrivateKeySigner::from_bytes(&B256::repeat_byte(0x43)).unwrap()
        } else {
            self.key.clone()
        };
        let signature = key.sign_hash_sync(&statement.digest()).unwrap();
        Ok(Bytes::from(signature.as_bytes().to_vec()))
    }
    async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
        let mut sim = self.sim.borrow_mut();
        if sim.wallet_rejects {
            return Err(TxError::Rejected);
        }
        if request.data[..4] == IVault::settleCall::SELECTOR && sim.declines_settle > 0 {
            sim.declines_settle -= 1;
            return Err(TxError::Rejected);
        }
        assert_eq!(
            request.chain_id, sim.wallet_chain,
            "sent on the wrong chain"
        );
        sim.sent.push(request.clone());
        let hash = keccak256(sim.sent.len().to_be_bytes());
        if sim.drops {
            return Ok(hash);
        }
        if sim.lands_after_looks.is_some() {
            sim.unseen.push((hash, request.clone()));
            return Ok(hash);
        }
        sim.mine(hash, request);
        Ok(hash)
    }
}

fn amount(dollars: u64) -> U256 {
    U256::from(dollars * USD)
}

fn deposit(world: &World, dollars: u64) -> (Result<Outcome, TxError>, Vec<Step>) {
    world.run(|ctx| Box::pin(flow::deposit(ctx, amount(dollars))))
}

fn prompts(steps: &[Step]) -> Vec<Prompt> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Signing(prompt) => Some(prompt.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_first_deposit_signs_the_statement_approves_exactly_the_amount_then_requests() {
    let world = World::new();
    let (result, steps) = deposit(&world, 1_000);
    let Ok(Outcome::Deposited { assets, shares, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(assets, amount(1_000));
    assert_eq!(shares, U256::from(999u128 * WAD));
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Statement,
            Prompt::Approve {
                amount: amount(1_000)
            },
            Prompt::Deposit {
                amount: amount(1_000)
            },
        ]
    );
    let sim = world.sim.borrow();
    // The approval is for exactly the deposit, to the vault.
    let approve = IERC20::approveCall::abi_decode(&sim.sent[0].data).unwrap();
    assert_eq!(approve.spender, vault());
    assert_eq!(approve.value, amount(1_000));
    // The request carries the statement, signed by the account over the
    // vault's own list.
    let request = IVault::requestDepositCall::abi_decode(&sim.sent[1].data).unwrap();
    assert_eq!(request.receiver, world.account());
    let digest = chain::statement::digest(CHAIN, vault(), world.account(), "SANCTIONED,US");
    assert!(flow::recovers_to(
        &request.statement,
        digest,
        world.account()
    ));
    // Every send was on the vault's chain, from the account.
    assert!(
        sim.sent
            .iter()
            .all(|sent| sent.chain_id == CHAIN && sent.from == world.account())
    );
}

#[test]
fn the_steps_follow_the_canvas_from_checking_to_done() {
    let world = World::new();
    let (_, steps) = deposit(&world, 1_000);
    let names: Vec<&str> = steps
        .iter()
        .map(|step| match step {
            Step::Checking => "checking",
            Step::Signing(_) => "signing",
            Step::Sent { .. } => "sent",
            Step::Requested(_) => "requested",
            Step::Priced { .. } => "priced",
            Step::Done(_) => "done",
            Step::Failed(_) => "failed",
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "checking",
            "signing",
            "signing",
            "sent",
            "signing",
            "sent",
            "requested",
            "priced"
        ]
    );
}

#[test]
fn a_second_deposit_needs_no_statement_and_no_approval_when_the_allowance_covers_it() {
    let world = World::new();
    let account = world.account();
    let hash = keccak256(world.sim.borrow().restricted.abi_encode());
    world.with(|chain| {
        chain.statement_of.insert(account, hash);
        chain.allowance.insert((account, vault()), amount(5_000));
    });
    let (result, steps) = deposit(&world, 1_000);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        prompts(&steps),
        vec![Prompt::Deposit {
            amount: amount(1_000)
        }]
    );
    let request = IVault::requestDepositCall::abi_decode(&world.sim.borrow().sent[0].data).unwrap();
    assert!(request.statement.is_empty());
}

#[test]
fn a_statement_signed_for_an_older_list_is_signed_again() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain
            .statement_of
            .insert(account, keccak256(vec![B256::ZERO].abi_encode()));
    });
    let (_, steps) = deposit(&world, 1_000);
    assert_eq!(prompts(&steps)[0], Prompt::Statement);
}

#[test]
fn a_rejection_in_the_wallet_sends_nothing() {
    let world = World::new();
    world.with(|chain| chain.wallet_rejects = true);
    let (result, _) = deposit(&world, 1_000);
    assert_eq!(result, Err(TxError::Rejected));
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_wallet_on_another_chain_is_never_asked_to_sign_or_send() {
    let world = World::new();
    world.with(|chain| chain.wallet_chain = 8453);
    let (result, steps) = deposit(&world, 1_000);
    assert_eq!(
        result,
        Err(TxError::WrongNetwork {
            wallet: 8453,
            vault: CHAIN
        })
    );
    assert!(prompts(&steps).is_empty());
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn an_account_that_changed_cancels_the_flow_before_anything_is_sent() {
    let world = World::new();
    world.with(|chain| chain.wallet_account = Some(Address::repeat_byte(9)));
    let (result, _) = deposit(&world, 1_000);
    assert_eq!(result, Err(TxError::AccountChanged));
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn more_than_the_wallet_holds_on_the_vaults_chain_is_refused_with_the_balance() {
    let world = World::new();
    let (result, _) = deposit(&world, 5_000);
    assert_eq!(
        result,
        Err(TxError::InsufficientBalance {
            have: amount(4_250)
        })
    );
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn under_the_minimum_deposit_is_refused_before_anything_is_asked() {
    let world = World::new();
    let (result, steps) = deposit(&world, 9);
    assert_eq!(
        result,
        Err(TxError::BelowMinimum {
            minimum: amount(10)
        })
    );
    assert!(prompts(&steps).is_empty());
    // Exactly ten dollars is enough.
    let (result, _) = deposit(&world, 10);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn no_coin_for_the_network_fee_is_said_before_the_wallet_is_asked() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.native.insert(account, U256::from(1u8));
    });
    let (result, steps) = deposit(&world, 1_000);
    assert!(
        matches!(result, Err(TxError::InsufficientGas { .. })),
        "{result:?}"
    );
    // The statement is a signature, not a transaction: it may come first.
    assert!(
        !prompts(&steps)
            .iter()
            .any(|p| matches!(p, Prompt::Approve { .. } | Prompt::Deposit { .. }))
    );
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_vault_not_open_takes_no_deposit() {
    let world = World::new();
    world.with(|chain| chain.state = b::VaultState::WindingDown);
    let (result, _) = deposit(&world, 1_000);
    assert_eq!(result, Err(TxError::WrongState));
}

#[test]
fn a_digest_that_differs_from_the_vaults_is_never_signed() {
    let world = World::new();
    world.with(|chain| chain.digest_differs = true);
    let (result, steps) = deposit(&world, 1_000);
    assert_eq!(result, Err(TxError::StatementMismatch));
    assert!(prompts(&steps).is_empty());
}

#[test]
fn a_signature_from_another_key_is_never_sent() {
    let world = World::new();
    world.with(|chain| chain.wallet_signs_wrong = true);
    let (result, _) = deposit(&world, 1_000);
    assert_eq!(result, Err(TxError::BadSignature));
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_reverted_transaction_is_failure_with_its_hash_never_success() {
    let world = World::new();
    world.with(|chain| chain.reverts = true);
    let (result, steps) = deposit(&world, 1_000);
    let Err(TxError::Reverted { hash, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(hash, keccak256(1usize.to_be_bytes()));
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::Requested(_) | Step::Done(_)))
    );
}

#[test]
fn a_transaction_the_network_never_knew_is_reported_dropped() {
    let world = World::new();
    world.with(|chain| chain.drops = true);
    let (result, _) = deposit(&world, 1_000);
    assert!(matches!(result, Err(TxError::Dropped { .. })), "{result:?}");
}

#[test]
fn when_nobody_settles_after_the_price_the_app_sends_the_second_step() {
    let world = World::new();
    world.with(|chain| chain.keeper = Keeper::PricesOnly);
    let (result, steps) = deposit(&world, 1_000);
    assert!(
        matches!(result, Ok(Outcome::Deposited { .. })),
        "{result:?}"
    );
    assert_eq!(prompts(&steps).last(), Some(&Prompt::Finish));
    let sent = world.sent_selectors();
    assert_eq!(sent.last(), Some(&IVault::settleCall::SELECTOR));
}

#[test]
fn when_the_keeper_settles_the_app_sends_no_second_step() {
    let world = World::new();
    let _ = deposit(&world, 1_000);
    assert!(
        !world
            .sent_selectors()
            .contains(&IVault::settleCall::SELECTOR)
    );
}

#[test]
fn a_missed_slot_moves_the_countdown_to_the_next_valuation_and_is_not_an_error() {
    let world = World::new();
    world.with(|chain| chain.keeper = Keeper::MissesSlots(1));
    let (result, steps) = deposit(&world, 1_000);
    let Ok(Outcome::Deposited { at, .. }) = result else {
        panic!("{result:?}");
    };
    let moved: Vec<_> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Requested(countdown) if countdown.moved_from.is_some() => Some(*countdown),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].slot, moved[0].moved_from.unwrap() + 10);
    assert_eq!(at, moved[0].slot);
}

#[test]
fn a_withdrawal_the_cash_covers_is_paid() {
    let world = World::new();
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(2_000u128 * WAD))));
    let Ok(Outcome::Paid { assets, shares, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(shares, U256::from(2_000u128 * WAD));
    assert_eq!(assets, U256::from(1_998u64 * USD));
    assert_eq!(
        prompts(&steps),
        vec![Prompt::Withdraw {
            shares: U256::from(2_000u128 * WAD)
        }]
    );
    let request = IVault::requestRedeemCall::abi_decode(&world.sim.borrow().sent[0].data).unwrap();
    assert_eq!(request.receiver, world.account());
}

#[test]
fn a_withdrawal_the_cash_cant_cover_ends_booked_with_its_claim() {
    let world = World::new();
    world.with(|chain| chain.exit = Exit::Booked);
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(2_400u128 * WAD))));
    assert_eq!(
        result.map(|outcome| match outcome {
            Outcome::Booked { claim, shares, .. } => (claim, shares),
            other => panic!("{other:?}"),
        }),
        Ok((U256::from(1u8), U256::from(2_400u128 * WAD)))
    );
}

#[test]
fn more_shares_than_held_are_refused() {
    let world = World::new();
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(9_000u128 * WAD))));
    assert_eq!(
        result,
        Err(TxError::InsufficientShares {
            have: U256::from(8_771u128 * WAD)
        })
    );
}

#[test]
fn a_closed_vault_pays_at_once_from_its_cash_without_a_request() {
    let world = World::new();
    world.with(|chain| chain.state = b::VaultState::Closed);
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(1_000u128 * WAD))));
    assert!(
        matches!(result, Ok(Outcome::Paid { at: None, .. })),
        "{result:?}"
    );
    assert_eq!(prompts(&steps), vec![Prompt::Closed]);
    assert_eq!(
        world.sent_selectors(),
        vec![IVault::redeemClosedCall::SELECTOR]
    );
}

#[test]
fn settle_now_pushes_a_waiting_claim_through_and_reads_where_it_ended() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.claims.push(b::Claim {
            owner: account,
            shares: U256::from(2_400u128 * WAD),
            bookedAt: 1,
            lastAttemptAt: 0,
            state: b::ClaimState::Waiting,
            paid: U256::ZERO,
        })
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_claim(ctx, U256::from(1u8))));
    let Ok(Outcome::Claim { state, paid, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(state, ClaimState::Paid);
    assert_eq!(paid, U256::from(2_501u64 * USD));
    assert_eq!(
        world.sent_selectors(),
        vec![IVault::settleClaimCall::SELECTOR]
    );
}

#[test]
fn a_paid_claim_is_not_settled_again() {
    let world = World::new();
    world.with(|chain| {
        chain.claims.push(b::Claim {
            owner: Address::ZERO,
            shares: U256::from(1u8),
            bookedAt: 1,
            lastAttemptAt: 0,
            state: b::ClaimState::Paid,
            paid: U256::from(1u8),
        })
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_claim(ctx, U256::from(1u8))));
    assert_eq!(result, Err(TxError::WrongState));
    assert!(world.sent_selectors().is_empty());
}

#[test]
fn a_closed_vault_pays_a_claim_from_its_cash() {
    let world = World::new();
    world.with(|chain| {
        chain.state = b::VaultState::Closed;
        chain.claims.push(b::Claim {
            owner: Address::ZERO,
            shares: U256::from(1u8),
            bookedAt: 1,
            lastAttemptAt: 0,
            state: b::ClaimState::Pending,
            paid: U256::ZERO,
        })
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_claim(ctx, U256::from(1u8))));
    assert!(
        matches!(
            result,
            Ok(Outcome::Claim {
                state: ClaimState::Paid,
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(
        world.sent_selectors(),
        vec![IVault::claimClosedCall::SELECTOR]
    );
}

#[test]
fn the_fallback_takes_the_cash_share_and_reports_the_units() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        for _ in 0..3 {
            chain
                .requests
                .push((account, 1, 1_000, false, U256::from(WAD)));
        }
    });
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::take_cash(ctx, Fallback::Request(U256::from(3u8)))));
    assert_eq!(
        result.map(|outcome| match outcome {
            Outcome::TookCash { assets, units, .. } => (assets, units),
            other => panic!("{other:?}"),
        }),
        Ok((U256::from(400_000u64), U256::from(7u8)))
    );
    assert_eq!(prompts(&steps), vec![Prompt::TakeCash]);
    let call = IVault::takeProRataCall::abi_decode(&world.sim.borrow().sent[0].data).unwrap();
    assert_eq!(call.requestId, U256::from(3u8));
    let world = World::new();
    let _ = world.run(|ctx| Box::pin(flow::take_cash(ctx, Fallback::Claim(U256::from(2u8)))));
    let call = IVault::takeProRataClaimCall::abi_decode(&world.sim.borrow().sent[0].data).unwrap();
    assert_eq!(call.claimId, U256::from(2u8));
}

#[test]
fn claim_instead_books_a_claim() {
    let world = World::new();
    let (result, _) = world.run(|ctx| Box::pin(flow::claim_instead(ctx, U256::from(4u8))));
    assert!(matches!(result, Ok(Outcome::ClaimMade { claim, .. }) if claim == U256::from(1u8)));
}

#[test]
fn collecting_units_or_held_dollars_reads_what_was_paid_and_refuses_nothing_to_collect() {
    let world = World::new();
    let (result, steps) = world.run(|ctx| Box::pin(flow::collect_units(ctx)));
    assert!(matches!(result, Err(TxError::WouldRevert { .. })));
    assert!(prompts(&steps).is_empty());
    world.with(|chain| chain.units_claimable = U256::from(1_500_000u64));
    let (result, _) = world.run(|ctx| Box::pin(flow::collect_units(ctx)));
    assert!(
        matches!(result, Ok(Outcome::Collected { assets, .. }) if assets == U256::from(1_500_000u64))
    );

    let world = World::new();
    world.with(|chain| chain.held = U256::from(2_000_000u64));
    let (result, _) = world.run(|ctx| Box::pin(flow::collect_held(ctx)));
    assert!(
        matches!(result, Ok(Outcome::Collected { assets, .. }) if assets == U256::from(2_000_000u64))
    );
    assert_eq!(
        world.sent_selectors(),
        vec![IVault::collectHeldCall::SELECTOR]
    );
}

#[test]
fn a_cancelled_flow_sends_nothing() {
    let world = World::new();
    let steps = RefCell::new(Vec::new());
    let on_step = |step: Step| steps.borrow_mut().push(step);
    let reader = SimReader(world.sim.clone());
    let wallet = SimWallet {
        sim: world.sim.clone(),
        key: world.key.clone(),
    };
    let cancel = Cancel::new();
    cancel.cancel();
    let ctx = Ctx {
        reader: &reader,
        signer: &wallet,
        clock: &NoWait,
        chain_id: CHAIN,
        vault: vault(),
        asset: asset(),
        account: world.account(),
        cancel,
        settle_now: Nudge::new(),
        skip_second: Nudge::new(),
        pace: Pace::FAST,
        on_step: &on_step,
    };
    let result = block_on(flow::deposit(&ctx, amount(1_000)));
    assert_eq!(result, Err(TxError::Cancelled));
    assert!(world.sim.borrow().sent.is_empty());
}

fn context() -> super::error::Context<'static> {
    super::error::Context {
        wallet: "MetaMask",
        chain: "Base",
        wallet_chain: Some("Hyperliquid"),
        symbol: "USDC",
        decimals: 6,
        native: "ETH",
    }
}

fn said(error: &TxError) -> String {
    let words = error.words(&context());
    format!("{} {}", words.title, words.text)
}

#[test]
fn declining_the_second_step_leaves_the_request_standing_and_the_flow_watching_until_the_keeper_settles_it()
 {
    let world = World::new();
    world.with(|chain| {
        chain.keeper = Keeper::SettlesLate(6);
        chain.declines_settle = 1;
    });
    let (result, steps) = deposit(&world, 1_000);
    assert!(
        matches!(result, Ok(Outcome::Deposited { .. })),
        "{result:?}"
    );
    assert!(steps.iter().any(|step| matches!(
        step,
        Step::Priced {
            unsent: Some(TxError::Rejected),
            ..
        }
    )));
    assert!(!steps.iter().any(|step| matches!(step, Step::Failed(_))));
    assert!(
        !world
            .sent_selectors()
            .contains(&IVault::settleCall::SELECTOR)
    );
}

#[test]
fn settle_now_after_a_declined_second_step_sends_it_again_and_ends_settled() {
    let mut world = World::new();
    world.presses_settle_now = true;
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        chain.declines_settle = 1;
    });
    let (result, steps) = deposit(&world, 1_000);
    assert!(
        matches!(result, Ok(Outcome::Deposited { .. })),
        "{result:?}"
    );
    let finishes = prompts(&steps)
        .into_iter()
        .filter(|prompt| *prompt == Prompt::Finish)
        .count();
    assert_eq!(finishes, 2);
    assert_eq!(
        world.sent_selectors().last(),
        Some(&IVault::settleCall::SELECTOR)
    );
}

#[test]
fn reads_that_fail_once_the_request_is_mined_end_as_a_request_that_stands_never_as_nothing_sent() {
    let world = World::new();
    world.with(|chain| chain.reads_fail_after_request = true);
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(1_000u128 * WAD))));
    let Err(
        error @ TxError::Landed {
            request: Some(_),
            then: Some(_),
            ..
        },
    ) = &result
    else {
        panic!("{result:?}");
    };
    assert!(steps.iter().any(|step| matches!(step, Step::Requested(_))));
    assert!(error.after_send() && error.may_have_moved());
    let said = said(error);
    assert!(
        !said.contains("Nothing was sent") && !said.contains("try again"),
        "{said}"
    );
}

#[test]
fn a_request_nobody_settles_before_the_long_wait_ends_is_said_to_stand() {
    let world = World::new();
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        chain.declines_settle = u32::MAX;
    });
    let (result, _) = deposit(&world, 1_000);
    let Err(
        error @ TxError::Landed {
            request: Some(request),
            then: None,
            ..
        },
    ) = &result
    else {
        panic!("{result:?}");
    };
    assert_eq!(*request, U256::from(1u8));
    assert!(!world.sim.borrow().requests[0].3, "it still stands");
    assert!(said(error).starts_with("Your request stands"));
}

#[test]
fn a_transaction_the_endpoint_never_saw_is_watched_to_the_end_and_said_as_not_seen_yet() {
    let world = World::new();
    world.with(|chain| chain.drops = true);
    let (result, steps) = deposit(&world, 1_000);
    let Err(error @ TxError::Dropped { .. }) = &result else {
        panic!("{result:?}");
    };
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Step::Sent { unseen: true, .. }))
    );
    assert!(world.sim.borrow().receipt_looks >= Pace::FAST.give_up_after);
    let said = said(error);
    assert!(said.starts_with("Not seen by the network yet"), "{said}");
    assert!(!said.contains("nothing moved") && !said.contains("You can try again"));
}

#[test]
fn a_transaction_not_seen_for_a_while_that_then_lands_is_followed_to_its_outcome() {
    let world = World::new();
    world.with(|chain| chain.lands_after_looks = Some(10));
    let (result, steps) = deposit(&world, 1_000);
    assert!(
        matches!(result, Ok(Outcome::Deposited { .. })),
        "{result:?}"
    );
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Step::Sent { unseen: true, .. }))
    );
}

#[test]
fn the_fallback_on_a_deposit_asks_to_take_it_back_and_ends_refunded_to_the_wallet_that_paid() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        chain.requests.push((account, 0, 1_000, false, amount(250)));
    });
    let before = world.sim.borrow().usd[&account];
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::take_cash(ctx, Fallback::Request(U256::from(1u8)))));
    assert_eq!(
        result.map(|outcome| match outcome {
            Outcome::Refunded { assets, to, .. } => (assets, to),
            other => panic!("{other:?}"),
        }),
        Ok((amount(250), account))
    );
    assert_eq!(
        prompts(&steps),
        vec![Prompt::TakeBack {
            amount: amount(250)
        }]
    );
    assert_eq!(world.sim.borrow().usd[&account], before + amount(250));
}

#[test]
fn the_fallback_is_never_asked_for_on_someone_elses_a_settled_or_a_claims_own_request() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        chain
            .requests
            .push((Address::repeat_byte(9), 0, 1_000, false, amount(250)));
        chain
            .requests
            .push((account, 1, 1_000, true, U256::from(WAD)));
        chain
            .requests
            .push((account, 2, 1_000, false, U256::from(WAD)));
    });
    for id in 1u8..=3 {
        let (result, steps) =
            world.run(|ctx| Box::pin(flow::take_cash(ctx, Fallback::Request(U256::from(id)))));
        assert!(
            matches!(result, Err(TxError::WouldRevert { .. })),
            "{id}: {result:?}"
        );
        assert!(prompts(&steps).is_empty());
    }
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_queued_claim_on_an_open_vault_is_not_pushed_again() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.claims.push(b::Claim {
            owner: account,
            shares: U256::from(WAD),
            bookedAt: 1,
            lastAttemptAt: 2,
            state: b::ClaimState::Queued,
            paid: U256::ZERO,
        })
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_claim(ctx, U256::from(1u8))));
    assert_eq!(result, Err(TxError::WrongState));
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_claim_is_offered_only_the_doors_the_contract_opens_for_its_state() {
    use flow::claim_door_open as door;
    let claims = [
        ClaimState::None,
        ClaimState::Pending,
        ClaimState::Waiting,
        ClaimState::Queued,
        ClaimState::Paid,
        ClaimState::Fallback,
        ClaimState::Closing,
    ];
    for claim in claims {
        // A closed vault pays pending, waiting and queued claims.
        assert_eq!(
            door(claim, State::Closed, false),
            matches!(
                claim,
                ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
            ),
            "{claim:?}"
        );
        // An open one tries pending and waiting claims, until it is ready
        // to close.
        for open in [
            State::Open,
            State::WindingDown,
            State::Frozen,
            State::Killed,
        ] {
            assert_eq!(
                door(claim, open, false),
                matches!(claim, ClaimState::Pending | ClaimState::Waiting),
                "{claim:?} {open:?}"
            );
            assert!(!door(claim, open, true));
        }
        assert!(!door(claim, State::Cancelled, false));
    }
}

#[test]
fn the_claim_fallback_opens_a_day_after_booking_and_a_queued_claim_a_day_after_the_slot_it_was_queued_for()
 {
    let claim = |state, booked_at, last_attempt_at| super::read::Claim {
        owner: Address::ZERO,
        shares: U256::from(WAD),
        booked_at,
        last_attempt_at,
        state,
        paid: U256::ZERO,
    };
    let day = 86_400;
    let timers = super::read::Timers {
        slot_interval: 10,
        slot_lead: 5,
        slot_window: 3,
        fallback_after: day,
    };
    let open = |c: &super::read::Claim, now| {
        flow::claim_fallback_open(c, State::Open, false, &timers, now)
    };
    let waiting = claim(ClaimState::Waiting, 100, 0);
    assert!(!open(&waiting, 100 + day - 1));
    assert!(open(&waiting, 100 + day));
    // Queued at 5,000: its own request is for the slot at 5,010, and the
    // vault counts the day from there.
    let queued = claim(ClaimState::Queued, 100, 5_000);
    assert!(!open(&queued, 100 + day));
    assert!(!open(&queued, 5_000 + day));
    assert!(open(&queued, 5_010 + day));
    assert!(!open(&claim(ClaimState::Paid, 100, 0), 10 * day));
    assert!(!flow::claim_fallback_open(
        &waiting,
        State::Closed,
        false,
        &timers,
        10 * day
    ));
    assert!(!flow::claim_fallback_open(
        &waiting,
        State::Open,
        true,
        &timers,
        10 * day
    ));
}

#[test]
fn settle_now_on_a_priced_request_settles_it_and_reads_how_it_ended() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.keeper = Keeper::PricesOnly;
        chain.requests.push((account, 0, 1_000, false, amount(500)));
    });
    let (result, steps) = world.run(|ctx| Box::pin(flow::settle_request(ctx, U256::from(1u8))));
    assert!(
        matches!(result, Ok(Outcome::Deposited { assets, .. }) if assets == amount(500)),
        "{result:?}"
    );
    assert_eq!(prompts(&steps), vec![Prompt::Finish]);
}

#[test]
fn settle_now_on_a_request_not_yet_priced_sends_nothing() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.keeper = Keeper::MissesSlots(50);
        chain.requests.push((account, 0, 1_000, false, amount(500)));
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_request(ctx, U256::from(1u8))));
    assert!(
        matches!(result, Err(TxError::WouldRevert { .. })),
        "{result:?}"
    );
    assert!(world.sim.borrow().sent.is_empty());
}

#[test]
fn a_closed_vault_settles_a_waiting_deposit_as_a_refund_and_never_a_claims_own_request() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.state = b::VaultState::Closed;
        chain.keeper = Keeper::PricesOnly;
        chain.requests.push((account, 0, 1_000, false, amount(300)));
        chain
            .requests
            .push((account, 2, 1_000, false, U256::from(WAD)));
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_closed(ctx, U256::from(1u8))));
    assert!(
        matches!(result, Ok(Outcome::Refunded { assets, to, .. }) if assets == amount(300) && to == account),
        "{result:?}"
    );
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_closed(ctx, U256::from(2u8))));
    assert_eq!(result, Err(TxError::WrongState));
    assert_eq!(
        world.sent_selectors(),
        vec![IVault::settleClosedCall::SELECTOR]
    );
}

#[test]
fn a_signature_ending_in_its_recovery_id_is_sent_with_the_parity_the_vault_takes() {
    let key = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).unwrap();
    let digest = keccak256(b"statement");
    let signature = key.sign_hash_sync(&digest).unwrap().as_bytes();
    let mut raw = signature;
    raw[64] -= 27;
    for given in [raw, signature] {
        let sent = flow::as_the_vault_takes_it(&given).unwrap();
        assert_eq!(sent.as_ref(), signature.as_slice());
        assert!(sent[64] == 27 || sent[64] == 28);
        assert!(flow::recovers_to(&sent, digest, key.address()));
    }
    assert_eq!(flow::as_the_vault_takes_it(&[0u8; 12]), None);
}
