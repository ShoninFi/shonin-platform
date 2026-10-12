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
use super::flow::{self, Cancel, Ctx, Fallback, Outcome, Pace, Prompt, Step};
use super::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use super::read::ClaimState;

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
}

type Sim = Rc<RefCell<Chain>>;

struct World {
    sim: Sim,
    key: PrivateKeySigner,
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
        };
        Self {
            sim: Rc::new(RefCell::new(chain)),
            key,
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
        let on_step = |step: Step| steps.borrow_mut().push(step);
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
        if self.keeper != Keeper::PricesOnly {
            self.settle_all();
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
            },
        ));
    }

    fn call(&mut self, to: Address, data: &[u8]) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
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
                self.tick();
                let call = IVault::requestAtCall::abi_decode(data).unwrap();
                let (owner, kind, earliest, done, amount) =
                    self.requests[usize::try_from(call.requestId).unwrap() - 1];
                ok(b::Request {
                    owner,
                    kind: if kind == 0 {
                        b::RequestKind::Deposit
                    } else {
                        b::RequestKind::Redeem
                    },
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
            IVault::settleCall::SELECTOR => self.settle_all(),
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
            kind: if kind == 0 {
                b::RequestKind::Deposit
            } else {
                b::RequestKind::Redeem
            },
            earliest,
            amount,
        });
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
        Ok(self.0.borrow().receipts.get(&hash).cloned())
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
        assert_eq!(
            request.chain_id, sim.wallet_chain,
            "sent on the wrong chain"
        );
        sim.sent.push(request.clone());
        let hash = keccak256(sim.sent.len().to_be_bytes());
        if sim.drops {
            return Ok(hash);
        }
        sim.block += 1;
        let first = sim.logs.len();
        let success = sim.execute(request);
        let logs = if success {
            sim.logs[first..]
                .iter()
                .map(|(_, log)| log.clone())
                .collect()
        } else {
            sim.logs.truncate(first);
            Vec::new()
        };
        let block = sim.block;
        *sim.nonce.entry(request.from).or_default() += 1;
        sim.receipts.insert(
            hash,
            Receipt {
                hash,
                success,
                block,
                logs,
            },
        );
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
        pace: Pace::FAST,
        on_step: &on_step,
    };
    let result = block_on(flow::deposit(&ctx, amount(1_000)));
    assert_eq!(result, Err(TxError::Cancelled));
    assert!(world.sim.borrow().sent.is_empty());
}
