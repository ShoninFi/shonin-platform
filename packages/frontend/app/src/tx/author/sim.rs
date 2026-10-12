//! A simulated chain for the author's flows: the dollar, the publishing
//! contract, the strategy and review registries, the price list, the vault
//! factory and one vault, each keeping the rules its contract keeps, and a
//! wallet. Every author flow's sequence is checked natively against it:
//! what is read, what is signed, what is sent and in which order, and what
//! each wallet or chain answer leads to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::{ChainName, IVaultFactory, VaultConfig};
use chain::bindings::prices::IPrices;
use chain::bindings::publishing::IPublishing;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::strategies::{ChainNeed, IStrategyRegistry, Strategy};
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

use super::error::AuthorError;
use super::flow::{self, Activation, Ctx, Outcome, Prompt, Spender, Step, Usage};
use super::form::{self, Allowances};
use super::read::Registries;
use crate::tx::flow::{Cancel, Pace};
use crate::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use crate::tx::read::State;
use crate::tx::{Revert, TxError};

const CHAIN: u64 = 31337;
const OTHER_CHAIN: u64 = 998;
const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;
const DAY: u64 = 86_400;

fn usd() -> Address {
    Address::repeat_byte(0xdd)
}
fn publishing() -> Address {
    Address::repeat_byte(0xb0)
}
fn factory() -> Address {
    Address::repeat_byte(0xfa)
}
fn strategies() -> Address {
    Address::repeat_byte(0x5e)
}
fn reviews() -> Address {
    Address::repeat_byte(0x7e)
}
fn prices() -> Address {
    Address::repeat_byte(0x9c)
}
fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn registries() -> Registries {
    Registries {
        factory: factory(),
        strategies: strategies(),
        reviews: reviews(),
        prices: prices(),
    }
}
fn upload() -> B256 {
    B256::repeat_byte(0x11)
}
fn name(text: &str) -> B256 {
    B256::right_padding_from(text.as_bytes())
}

struct Chain {
    time: u64,
    block: u64,
    usd: HashMap<Address, U256>,
    native: HashMap<Address, U256>,
    allowance: HashMap<(Address, Address), U256>,
    fee: U256,
    submissions: Vec<(Address, B256)>,
    // The strategy (id 7) and its review.
    author: Address,
    registered: bool,
    reviewed: bool,
    chains: Vec<B256>,
    terms: B256,
    minimum_tank: U256,
    // The vault, once activated.
    activated: bool,
    state: State,
    tank: b::Tank,
    allowances: Allowances,
    floor: (u16, u16, u64),
    shares: HashMap<Address, U256>,
    requests: u64,
    receipts: HashMap<B256, Receipt>,
    logs: Vec<Log>,
    nonce: HashMap<Address, u64>,
    /// Every transaction's selector, in the order sent.
    calls: Vec<[u8; 4]>,
    sent: Vec<TxRequest>,
    wallet_chain: u64,
    wallet_account: Option<Address>,
    wallet_rejects: bool,
    wallet_signs_wrong: bool,
    reverts: bool,
    /// Pay events record another upload than the one paid for.
    pay_logs_other: bool,
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
        let mut usd_balances = HashMap::new();
        usd_balances.insert(account, U256::from(500 * USD));
        let mut native = HashMap::new();
        native.insert(account, U256::from(10u64).pow(U256::from(18u8)));
        let mut shares = HashMap::new();
        shares.insert(account, U256::from(1_642u128 * WAD));
        let chain = Chain {
            time: 1_000_000,
            block: 100,
            usd: usd_balances,
            native,
            allowance: HashMap::new(),
            fee: U256::from(USD),
            submissions: Vec::new(),
            author: account,
            registered: true,
            reviewed: true,
            chains: vec![name("base")],
            terms: B256::repeat_byte(0x7a),
            minimum_tank: U256::from(10 * USD),
            activated: false,
            state: State::None,
            tank: empty_tank(),
            allowances: Allowances::default(),
            floor: (500, 0, 0),
            shares,
            requests: 0,
            receipts: HashMap::new(),
            logs: Vec::new(),
            nonce: HashMap::new(),
            calls: Vec::new(),
            sent: Vec::new(),
            wallet_chain: CHAIN,
            wallet_account: Some(account),
            wallet_rejects: false,
            wallet_signs_wrong: false,
            reverts: false,
            pay_logs_other: false,
        };
        Self {
            sim: Rc::new(RefCell::new(chain)),
            key,
        }
    }

    /// A world whose vault is already open, with a $9.99 tank.
    fn live() -> Self {
        let world = Self::new();
        world.with(|chain| {
            chain.activated = true;
            chain.state = State::Open;
            chain.tank.balance = U256::from(9_990_000u64);
            chain.allowances = form::shape("modest");
        });
        world
    }

    fn account(&self) -> Address {
        self.key.address()
    }

    fn with(&self, change: impl FnOnce(&mut Chain)) -> &Self {
        change(&mut self.sim.borrow_mut());
        self
    }

    fn run(
        &self,
        flow: impl for<'a> FnOnce(
            &'a Ctx<'a, SimReader, SimWallet, NoWait>,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<Outcome, AuthorError>> + 'a>,
        >,
    ) -> (Result<Outcome, AuthorError>, Vec<Step>) {
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
            account: self.account(),
            cancel: Cancel::new(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let result = block_on(flow(&ctx));
        (result, steps.into_inner())
    }

    fn calls(&self) -> Vec<[u8; 4]> {
        self.sim.borrow().calls.clone()
    }

    fn usd_of(&self, who: Address) -> U256 {
        self.sim.borrow().usd.get(&who).copied().unwrap_or_default()
    }
}

fn timers() -> b::Timers {
    b::Timers {
        slotInterval: 10,
        slotLead: 5,
        slotWindow: 3,
        postDeadline: 60,
        transitTimeout: 3_600,
        fallbackAfter: DAY,
        usageFreshness: DAY,
        tankEscapeDelay: 3 * DAY,
        floorDelay: DAY,
        haltGrace: DAY,
        freezeAfter: 30 * DAY,
        windDown: 7 * DAY,
        activationWindow: 3 * DAY,
    }
}

fn empty_tank() -> b::Tank {
    b::Tank {
        balance: U256::ZERO,
        keepAt: U256::ZERO,
        usageSettled: U256::ZERO,
        usageAsOf: 0,
        lastCollection: 0,
        escapeRequestedAt: 0,
        capCarried: U256::ZERO,
        fillPending: false,
    }
}

fn sol_state(state: State) -> b::VaultState {
    use b::VaultState as S;
    match state {
        State::None => S::None,
        State::Activating => S::Activating,
        State::Open => S::Open,
        State::WindingDown => S::WindingDown,
        State::Frozen => S::Frozen,
        State::Killed => S::Killed,
        State::Closed => S::Closed,
        State::Cancelled => S::Cancelled,
    }
}

struct NoWait;
impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

fn ok<T: SolValue>(value: T) -> Result<Bytes, TxError> {
    Ok(Bytes::from(value.abi_encode()))
}

fn revert(name: &'static str) -> TxError {
    TxError::WouldRevert {
        reason: Revert::Vault(name),
    }
}

impl Chain {
    fn log<E: SolEvent>(&mut self, address: Address, event: E) {
        let data = event.encode_log_data();
        self.logs.push(Log {
            address,
            topics: data.topics().to_vec(),
            data: data.data.clone(),
            transaction: None,
        });
    }

    fn floor_now(&self) -> u16 {
        let (bps, pending, at) = self.floor;
        if at != 0 && self.time >= at {
            pending
        } else {
            bps
        }
    }

    fn call(&self, to: Address, data: &[u8]) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if to == usd() {
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
        if to == publishing() {
            return match selector {
                IPublishing::feeCall::SELECTOR => ok(self.fee),
                IPublishing::assetCall::SELECTOR => ok(usd()),
                _ => panic!("unexpected publishing read"),
            };
        }
        if to == strategies() {
            assert_eq!(selector, IStrategyRegistry::strategyOfCall::SELECTOR);
            return ok(Strategy {
                author: self.author,
                registeredAt: if self.registered { 5 } else { 0 },
                chains: self
                    .chains
                    .iter()
                    .map(|chain| ChainNeed {
                        chain: *chain,
                        drivers: vec![name("local")],
                    })
                    .collect(),
                sourceHash: B256::ZERO,
                bundleHash: B256::ZERO,
                manifestHash: B256::ZERO,
                bridges: Vec::new(),
                restricted: Vec::new(),
            });
        }
        if to == reviews() {
            assert_eq!(selector, IReviewRegistry::hasReviewCall::SELECTOR);
            return ok(self.reviewed);
        }
        if to == prices() {
            assert_eq!(selector, IPrices::dailyCostCall::SELECTOR);
            return ok(U256::from(1_058_500u64));
        }
        if to == factory() {
            return match selector {
                IVaultFactory::vaultOfCall::SELECTOR => ok(if self.activated {
                    vault()
                } else {
                    Address::ZERO
                }),
                IVaultFactory::predictVaultCall::SELECTOR => ok(vault()),
                IVaultFactory::restrictedForCall::SELECTOR => Ok(Bytes::from(
                    (vec![name("SANCTIONED")], "SANCTIONED".to_string()).abi_encode_params(),
                )),
                IVaultFactory::termsCall::SELECTOR => ok(self.terms),
                IVaultFactory::configCall::SELECTOR => ok(VaultConfig {
                    holdingBps: 100,
                    authorBps: 1_000,
                    platformBps: 500,
                    transactionBps: 10,
                    exitBandBps: 100,
                    minimumDeposit: U256::from(10 * USD),
                    minimumTank: self.minimum_tank,
                    deadAssets: U256::from(10_000u64),
                    maxExitCost: U256::from(1_000 * USD),
                    timers: timers(),
                }),
                IVaultFactory::chainsCall::SELECTOR => ok(vec![
                    ChainName {
                        name: name("base"),
                        chainId: U256::from(CHAIN),
                    },
                    ChainName {
                        name: name("hyperliquid"),
                        chainId: U256::from(OTHER_CHAIN),
                    },
                ]),
                _ => panic!("unexpected factory read"),
            };
        }
        assert_eq!(to, vault(), "a read of an unknown contract");
        match selector {
            IVault::stateCall::SELECTOR => ok(sol_state(self.state)),
            IVault::authorCall::SELECTOR => ok(self.author),
            IVault::tankCall::SELECTOR => ok(self.tank.clone()),
            IVault::allowancesCall::SELECTOR => ok(self.allowances.to_sol()),
            IVault::idleFloorCall::SELECTOR => Ok(Bytes::from(
                (self.floor.0, self.floor.1, self.floor.2).abi_encode_params(),
            )),
            IVault::balanceOfCall::SELECTOR => {
                let call = IVault::balanceOfCall::abi_decode(data).unwrap();
                ok(self.shares.get(&call.owner).copied().unwrap_or_default())
            }
            IVault::sharePriceCall::SELECTOR => ok(U256::from(1_140_200u64)),
            IVault::cashCall::SELECTOR => ok(U256::from(131_870 * USD)),
            IVault::feesCall::SELECTOR => ok(b::Fees {
                holdingBps: 100,
                authorBps: 1_000,
                platformBps: 500,
                transactionBps: 10,
            }),
            IVault::timersCall::SELECTOR => ok(timers()),
            IVault::readyToCloseCall::SELECTOR => ok(false),
            _ => panic!("unexpected vault read {selector:?}"),
        }
    }

    /// Whether the contract would take this transaction, as `eth_estimateGas`
    /// says: the reason when it would revert.
    fn would_revert(&self, request: &TxRequest) -> Option<&'static str> {
        let data = &request.data;
        let from = request.from;
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        let takes = |spender: Address, amount: U256| {
            self.allowance
                .get(&(from, spender))
                .copied()
                .unwrap_or_default()
                >= amount
                && self.usd.get(&from).copied().unwrap_or_default() >= amount
        };
        match (request.to, selector) {
            (to, IPublishing::submitCall::SELECTOR) if to == publishing() => {
                (!takes(publishing(), self.fee)).then_some("Allowance")
            }
            (to, IVaultFactory::activateCall::SELECTOR) if to == factory() => {
                let call = IVaultFactory::activateCall::abi_decode(data).unwrap();
                if self.activated {
                    Some("VaultExists")
                } else if call.terms != self.terms {
                    Some("TermsChanged")
                } else if from != self.author {
                    Some("NotTheAuthor")
                } else if call.tankFunding < self.minimum_tank {
                    Some("TankTooSmall")
                } else if !takes(factory(), call.tankFunding) {
                    Some("Allowance")
                } else {
                    None
                }
            }
            (to, _) if to == vault() => {
                let author_only = selector != IVault::fundTankCall::SELECTOR;
                if author_only && from != self.author {
                    return Some("NotAllowed");
                }
                match selector {
                    IVault::fundTankCall::SELECTOR => {
                        let call = IVault::fundTankCall::abi_decode(data).unwrap();
                        (!takes(vault(), call.assets)).then_some("Allowance")
                    }
                    IVault::retireCall::SELECTOR => {
                        (self.state != State::Open).then_some("WrongState")
                    }
                    IVault::withdrawTankUnsignedCall::SELECTOR => {
                        let asked = self.tank.escapeRequestedAt;
                        if asked == 0 || self.time < asked + 3 * DAY {
                            Some("NotYet")
                        } else if self.time > asked + 6 * DAY {
                            Some("NotAllowed")
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            (to, IERC20::approveCall::SELECTOR) if to == usd() => None,
            _ => panic!("a transaction to an unknown contract"),
        }
    }

    fn execute(&mut self, request: &TxRequest) -> bool {
        let data = request.data.clone();
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        self.calls.push(selector);
        if self.reverts || self.would_revert(request).is_some() {
            return false;
        }
        let from = request.from;
        let pay = |chain: &mut Chain, spender: Address, to: Address, amount: U256| {
            *chain.allowance.get_mut(&(from, spender)).unwrap() -= amount;
            *chain.usd.get_mut(&from).unwrap() -= amount;
            *chain.usd.entry(to).or_default() += amount;
        };
        if request.to == usd() {
            let call = IERC20::approveCall::abi_decode(&data).unwrap();
            self.allowance.insert((from, call.spender), call.value);
            return true;
        }
        if request.to == publishing() {
            let call = IPublishing::submitCall::abi_decode(&data).unwrap();
            let fee = self.fee;
            pay(self, publishing(), Address::repeat_byte(0x01), fee);
            self.submissions.push((from, call.upload));
            let id = U256::from(self.submissions.len());
            let recorded = if self.pay_logs_other {
                B256::repeat_byte(0x22)
            } else {
                call.upload
            };
            self.log(
                publishing(),
                IPublishing::Submitted {
                    strategyId: id,
                    author: from,
                    upload: recorded,
                    fee,
                },
            );
            return true;
        }
        if request.to == factory() {
            let call = IVaultFactory::activateCall::abi_decode(&data).unwrap();
            pay(self, factory(), vault(), call.tankFunding);
            self.activated = true;
            self.state = State::Open;
            self.tank.balance = call.tankFunding;
            self.tank.keepAt = call.tankKeep;
            self.allowances = Allowances::from_sol(&call.allowances);
            self.floor = (call.idleFloorBps, 0, 0);
            self.log(
                factory(),
                IVaultFactory::Activated {
                    strategyId: call.strategyId,
                    author: from,
                    vault: vault(),
                    station: Address::repeat_byte(0x55),
                    tankFunding: call.tankFunding,
                },
            );
            return true;
        }
        match selector {
            IVault::fundTankCall::SELECTOR => {
                let call = IVault::fundTankCall::abi_decode(&data).unwrap();
                pay(self, vault(), vault(), call.assets);
                self.tank.balance += call.assets;
                self.log(
                    vault(),
                    IVault::TankFunded {
                        from,
                        assets: call.assets,
                    },
                );
            }
            IVault::setTankKeepCall::SELECTOR => {
                let call = IVault::setTankKeepCall::abi_decode(&data).unwrap();
                self.tank.keepAt = call.assets;
                self.log(
                    vault(),
                    IVault::TankKeepSet {
                        assets: call.assets,
                    },
                );
            }
            IVault::topUpFromSharesCall::SELECTOR => {
                let call = IVault::topUpFromSharesCall::abi_decode(&data).unwrap();
                self.requests += 1;
                self.log(
                    vault(),
                    IVault::Requested {
                        requestId: U256::from(self.requests),
                        owner: from,
                        kind: b::RequestKind::TankFill,
                        earliest: self.time + 10,
                        amount: call.shares,
                    },
                );
            }
            IVault::setAllowancesCall::SELECTOR => {
                let call = IVault::setAllowancesCall::abi_decode(&data).unwrap();
                self.allowances = Allowances::from_sol(&call.allowances);
                self.log(
                    vault(),
                    IVault::AllowancesSet {
                        allowances: call.allowances,
                    },
                );
            }
            IVault::setIdleFloorCall::SELECTOR => {
                // As the vault does: a raise (or the same) at once, clearing
                // any lowering waiting; a lowering after the delay.
                let call = IVault::setIdleFloorCall::abi_decode(&data).unwrap();
                let current = self.floor_now();
                if call.bps >= current {
                    self.floor = (call.bps, 0, 0);
                    self.log(
                        vault(),
                        IVault::IdleFloorSet {
                            bps: call.bps,
                            effectiveAt: self.time,
                        },
                    );
                } else {
                    let at = self.time + DAY;
                    self.floor = (current, call.bps, at);
                    self.log(
                        vault(),
                        IVault::IdleFloorSet {
                            bps: call.bps,
                            effectiveAt: at,
                        },
                    );
                }
            }
            IVault::retireCall::SELECTOR => {
                self.state = State::WindingDown;
                self.log(
                    vault(),
                    IVault::StateChanged {
                        state: b::VaultState::WindingDown,
                    },
                );
            }
            IVault::requestTankWithdrawalCall::SELECTOR => {
                self.tank.escapeRequestedAt = self.time;
                self.log(
                    vault(),
                    IVault::TankWithdrawalRequested {
                        readyAt: self.time + 3 * DAY,
                    },
                );
            }
            IVault::withdrawTankUnsignedCall::SELECTOR | IVault::withdrawTankCall::SELECTOR => {
                let assets = if selector == IVault::withdrawTankCall::SELECTOR {
                    IVault::withdrawTankCall::abi_decode(&data).unwrap().assets
                } else {
                    self.tank.escapeRequestedAt = 0;
                    IVault::withdrawTankUnsignedCall::abi_decode(&data)
                        .unwrap()
                        .assets
                };
                let assets = assets.min(self.tank.balance);
                self.tank.balance -= assets;
                *self.usd.entry(from).or_default() += assets;
                self.log(vault(), IVault::TankWithdrawn { assets });
            }
            _ => panic!("unexpected vault transaction {selector:?}"),
        }
        true
    }
}

struct SimReader(Sim);

impl Reader for SimReader {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        self.0.borrow().call(to, &data)
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
        match self.0.borrow().would_revert(request) {
            Some(reason) => Err(revert(reason)),
            None => Ok(100_000),
        }
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
        _address: Address,
        _topic0: B256,
        _topic1: Option<B256>,
        _from_block: u64,
    ) -> Result<Vec<Log>, TxError> {
        Ok(Vec::new())
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
        sim.block += 1;
        let first = sim.logs.len();
        let success = sim.execute(request);
        let logs = if success {
            sim.logs[first..].to_vec()
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

fn prompts(steps: &[Step]) -> Vec<Prompt> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Signing(prompt) => Some(prompt.clone()),
            _ => None,
        })
        .collect()
}

fn dollars(amount: u64) -> U256 {
    U256::from(amount * USD)
}

// Publishing -------------------------------------------------------------------

fn pay(world: &World) -> (Result<Outcome, AuthorError>, Vec<Step>) {
    world.run(|ctx| Box::pin(flow::pay(ctx, publishing(), upload())))
}

#[test]
fn paying_to_publish_approves_exactly_the_fee_then_pays_against_the_upload() {
    let world = World::new();
    let before = world.usd_of(world.account());
    let (result, steps) = pay(&world);
    assert_eq!(
        result,
        Ok(Outcome::Paid {
            strategy_id: U256::from(1u8),
            hash: match &steps.last().unwrap() {
                Step::Sent { hash, .. } => *hash,
                _ => unreachable!(),
            },
        })
    );
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Approve {
                amount: dollars(1),
                spender: Spender::Publishing
            },
            Prompt::Pay { fee: dollars(1) }
        ]
    );
    assert_eq!(
        world.calls(),
        vec![
            IERC20::approveCall::SELECTOR,
            IPublishing::submitCall::SELECTOR
        ]
    );
    assert_eq!(world.usd_of(world.account()), before - dollars(1));
    let approved = IERC20::approveCall::abi_decode(&world.sim.borrow().sent[0].data).unwrap();
    assert_eq!(approved.value, dollars(1));
    assert_eq!(approved.spender, publishing());
    assert_eq!(
        world.sim.borrow().submissions,
        vec![(world.account(), upload())]
    );
}

#[test]
fn an_allowance_that_already_covers_the_fee_pays_in_one_prompt() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.allowance.insert((account, publishing()), dollars(1));
    });
    let (result, steps) = pay(&world);
    assert!(result.is_ok());
    assert_eq!(prompts(&steps), vec![Prompt::Pay { fee: dollars(1) }]);
}

#[test]
fn a_wallet_without_the_fee_is_refused_before_anything_is_asked() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.usd.insert(account, U256::from(999_999u64));
    });
    let (result, steps) = pay(&world);
    assert_eq!(
        result,
        Err(AuthorError::Tx(TxError::InsufficientBalance {
            have: U256::from(999_999u64)
        }))
    );
    assert!(prompts(&steps).is_empty());
    assert!(world.calls().is_empty());
}

#[test]
fn a_rejected_payment_sends_nothing_and_says_nothing_was_sent() {
    let world = World::new();
    world.with(|chain| chain.wallet_rejects = true);
    let (result, _) = pay(&world);
    let error = result.unwrap_err();
    assert_eq!(error, AuthorError::Tx(TxError::Rejected));
    assert!(!error.after_send());
    assert!(world.calls().is_empty());
}

#[test]
fn a_wallet_on_another_chain_is_never_asked_to_pay() {
    let world = World::new();
    world.with(|chain| chain.wallet_chain = 1);
    let (result, steps) = pay(&world);
    assert_eq!(
        result,
        Err(AuthorError::Tx(TxError::WrongNetwork {
            wallet: 1,
            vault: CHAIN
        }))
    );
    assert!(prompts(&steps).is_empty());
    assert!(world.calls().is_empty());
}

#[test]
fn a_payment_recorded_for_another_upload_is_never_reported_as_this_ones() {
    let world = World::new();
    world.with(|chain| chain.pay_logs_other = true);
    let (result, _) = pay(&world);
    assert_eq!(result, Err(AuthorError::PaymentMismatch));
}

#[test]
fn an_account_that_changed_stops_the_payment_before_anything_is_sent() {
    let world = World::new();
    world.with(|chain| chain.wallet_account = Some(Address::repeat_byte(0x99)));
    let (result, _) = pay(&world);
    assert_eq!(result, Err(AuthorError::Tx(TxError::AccountChanged)));
    assert!(world.calls().is_empty());
}

// Activation -------------------------------------------------------------------

fn activation(tank: U256) -> Activation {
    Activation {
        strategy_id: U256::from(7u8),
        allowances: form::shape("modest"),
        idle_floor_bps: 500,
        tank,
        keep: U256::ZERO,
        terms: B256::repeat_byte(0x7a),
    }
}

fn activate(world: &World, chosen: Activation) -> (Result<Outcome, AuthorError>, Vec<Step>) {
    world.run(move |ctx| {
        Box::pin(async move { flow::activate(ctx, &registries(), usd(), &chosen).await })
    })
}

#[test]
fn activating_signs_the_statement_for_the_vault_to_be_approves_the_tank_exactly_then_activates() {
    let world = World::new();
    let (result, steps) = activate(&world, activation(dollars(210)));
    assert!(
        matches!(result, Ok(Outcome::Activated { vault: at, .. }) if at == vault()),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Statement,
            Prompt::Approve {
                amount: dollars(210),
                spender: Spender::Factory
            },
            Prompt::Activate { tank: dollars(210) }
        ]
    );
    let sim = world.sim.borrow();
    let approved = IERC20::approveCall::abi_decode(&sim.sent[0].data).unwrap();
    assert_eq!(
        (approved.spender, approved.value),
        (factory(), dollars(210))
    );
    let sent = IVaultFactory::activateCall::abi_decode(&sim.sent[1].data).unwrap();
    assert_eq!(sent.tankFunding, dollars(210));
    assert_eq!(sent.idleFloorBps, 500);
    assert_eq!(
        Allowances::from_sol(&sent.allowances),
        form::shape("modest")
    );
    assert_eq!(sent.terms, B256::repeat_byte(0x7a));
    // The statement is the author's, for the predicted vault, over its list.
    let digest = chain::statement::digest(CHAIN, vault(), world.account(), "SANCTIONED");
    assert!(crate::tx::flow::recovers_to(
        &sent.statement,
        digest,
        world.account()
    ));
    assert_eq!(sim.tank.balance, dollars(210));
}

#[test]
fn a_tank_under_the_minimum_is_refused_before_anything_is_signed() {
    let world = World::new();
    let (result, steps) = activate(&world, activation(U256::from(9_999_999u64)));
    assert_eq!(
        result,
        Err(AuthorError::TankTooSmall {
            minimum: dollars(10)
        })
    );
    assert!(prompts(&steps).is_empty());
}

#[test]
fn only_the_author_of_a_reviewed_and_registered_strategy_can_activate_it_once() {
    let world = World::new();
    world.with(|chain| chain.author = Address::repeat_byte(0x33));
    assert_eq!(
        activate(&world, activation(dollars(10))).0,
        Err(AuthorError::NotAuthor)
    );
    let world = World::new();
    world.with(|chain| chain.reviewed = false);
    assert_eq!(
        activate(&world, activation(dollars(10))).0,
        Err(AuthorError::NotReady)
    );
    let world = World::new();
    world.with(|chain| chain.registered = false);
    assert_eq!(
        activate(&world, activation(dollars(10))).0,
        Err(AuthorError::NotReady)
    );
    let world = World::new();
    world.with(|chain| chain.activated = true);
    assert_eq!(
        activate(&world, activation(dollars(10))).0,
        Err(AuthorError::AlreadyActive)
    );
    assert!(world.calls().is_empty());
}

#[test]
fn a_strategy_that_trades_on_other_chains_is_not_activated_here() {
    let world = World::new();
    world.with(|chain| chain.chains.push(name("hyperliquid")));
    assert_eq!(
        activate(&world, activation(dollars(10))).0,
        Err(AuthorError::OtherChains)
    );
    assert!(world.calls().is_empty());
}

#[test]
fn terms_that_changed_since_the_page_read_them_stop_the_activation() {
    let world = World::new();
    let mut chosen = activation(dollars(10));
    chosen.terms = B256::repeat_byte(0x01);
    assert_eq!(activate(&world, chosen).0, Err(AuthorError::TermsChanged));
    assert!(world.calls().is_empty());
}

#[test]
fn a_statement_signed_by_another_key_is_never_sent() {
    let world = World::new();
    world.with(|chain| chain.wallet_signs_wrong = true);
    let (result, _) = activate(&world, activation(dollars(10)));
    assert_eq!(result, Err(AuthorError::Tx(TxError::BadSignature)));
    assert!(world.calls().is_empty());
}

#[test]
fn a_reverted_activation_is_a_failure_with_its_hash_never_success() {
    let world = World::new();
    world.with(|chain| chain.reverts = true);
    let (result, _) = activate(&world, activation(dollars(10)));
    let error = result.unwrap_err();
    assert!(error.after_send(), "{error:?}");
    assert!(matches!(error, AuthorError::Tx(TxError::Reverted { .. })));
}

#[test]
fn a_tank_larger_than_the_wallet_is_refused_with_the_balance() {
    let world = World::new();
    let (result, _) = activate(&world, activation(dollars(501)));
    assert_eq!(
        result,
        Err(AuthorError::Tx(TxError::InsufficientBalance {
            have: dollars(500)
        }))
    );
    assert!(world.calls().is_empty());
}

// The tank ---------------------------------------------------------------------

#[test]
fn a_top_up_approves_exactly_the_amount_and_reads_the_funding_from_its_event() {
    let world = World::live();
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::fund_tank(ctx, vault(), usd(), dollars(25))));
    assert!(
        matches!(result, Ok(Outcome::TankFunded { amount, .. }) if amount == dollars(25)),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Approve {
                amount: dollars(25),
                spender: Spender::Vault
            },
            Prompt::FundTank {
                amount: dollars(25)
            }
        ]
    );
    assert_eq!(
        world.sim.borrow().tank.balance,
        U256::from(9_990_000u64) + dollars(25)
    );
}

#[test]
fn someone_other_than_the_author_is_refused_before_anything_is_asked() {
    let world = World::live();
    world.with(|chain| chain.author = Address::repeat_byte(0x33));
    for result in [
        world
            .run(|ctx| Box::pin(flow::fund_tank(ctx, vault(), usd(), dollars(5))))
            .0,
        world
            .run(|ctx| Box::pin(flow::set_keep(ctx, vault(), dollars(5))))
            .0,
        world.run(|ctx| Box::pin(flow::retire(ctx, vault()))).0,
        world
            .run(|ctx| Box::pin(flow::set_idle_floor(ctx, vault(), 300)))
            .0,
    ] {
        assert_eq!(result, Err(AuthorError::NotAuthor));
    }
    assert!(world.calls().is_empty());
}

#[test]
fn auto_fill_sets_the_keep_level_and_turning_it_off_sets_zero() {
    let world = World::live();
    let (result, _) = world.run(|ctx| Box::pin(flow::set_keep(ctx, vault(), dollars(50))));
    assert!(matches!(result, Ok(Outcome::KeepSet { amount, .. }) if amount == dollars(50)));
    assert_eq!(world.sim.borrow().tank.keepAt, dollars(50));
    let (result, _) = world.run(|ctx| Box::pin(flow::set_keep(ctx, vault(), U256::ZERO)));
    assert!(matches!(result, Ok(Outcome::KeepSet { amount, .. }) if amount.is_zero()));
    assert_eq!(world.sim.borrow().tank.keepAt, U256::ZERO);
}

#[test]
fn a_fill_from_shares_is_queued_for_exactly_the_shares_and_never_more_than_held() {
    let world = World::live();
    let shares = U256::from(100u128 * WAD);
    let (result, _) = world.run(|ctx| Box::pin(flow::top_up_from_shares(ctx, vault(), shares)));
    assert!(
        matches!(result, Ok(Outcome::FillQueued { shares: s, request, .. }) if s == shares && request == U256::from(1u8))
    );
    let too_many = U256::from(1_643u128 * WAD);
    let (result, _) = world.run(|ctx| Box::pin(flow::top_up_from_shares(ctx, vault(), too_many)));
    assert_eq!(
        result,
        Err(AuthorError::Tx(TxError::InsufficientShares {
            have: U256::from(1_642u128 * WAD)
        }))
    );
}

#[test]
fn the_unsigned_escape_waits_its_delay_then_pays_the_tank_to_the_author() {
    let world = World::live();
    let account = world.account();
    let before = world.usd_of(account);
    // Not asked yet.
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw_unsigned(ctx, vault(), dollars(5))));
    assert_eq!(result, Err(AuthorError::EscapeNotReady { ready_at: None }));
    // Asked: it opens three days on.
    let (result, _) = world.run(|ctx| Box::pin(flow::request_escape(ctx, vault())));
    let ready_at = match result {
        Ok(Outcome::EscapeRequested { ready_at, .. }) => ready_at,
        other => panic!("{other:?}"),
    };
    assert_eq!(ready_at, 1_000_000 + 3 * DAY);
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw_unsigned(ctx, vault(), dollars(5))));
    assert_eq!(
        result,
        Err(AuthorError::EscapeNotReady {
            ready_at: Some(ready_at)
        })
    );
    // Open: it pays exactly what was asked.
    world.with(|chain| chain.time = ready_at);
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw_unsigned(ctx, vault(), dollars(5))));
    assert!(
        matches!(result, Ok(Outcome::TankWithdrawn { amount, .. }) if amount == dollars(5)),
        "{result:?}"
    );
    assert_eq!(world.usd_of(account), before + dollars(5));
    // More than the tank holds is refused, never clamped silently.
    world.with(|chain| {
        chain.tank.escapeRequestedAt = chain.time;
        chain.time += 3 * DAY;
    });
    let (result, _) =
        world.run(|ctx| Box::pin(flow::withdraw_unsigned(ctx, vault(), dollars(100))));
    assert!(matches!(
        result,
        Err(AuthorError::Tx(TxError::InsufficientBalance { .. }))
    ));
    // Lapsed after twice the delay.
    world.with(|chain| chain.time += 3 * DAY + 1);
    let (result, _) = world.run(|ctx| Box::pin(flow::withdraw_unsigned(ctx, vault(), dollars(1))));
    assert_eq!(result, Err(AuthorError::EscapeLapsed));
}

#[test]
fn a_signed_withdrawal_needs_a_recent_total() {
    let world = World::live();
    let stale = Usage {
        total: U256::from(100u8),
        as_of: 1_000_000 - DAY - 1,
        signature: Bytes::from(vec![1u8; 65]),
    };
    let (result, _) = world.run(|ctx| {
        Box::pin(async move { flow::withdraw_tank(ctx, vault(), dollars(1), &stale).await })
    });
    assert_eq!(result, Err(AuthorError::NoUsageTotal));
    let fresh = Usage {
        total: U256::from(100u8),
        as_of: 1_000_000 - 60,
        signature: Bytes::from(vec![1u8; 65]),
    };
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move { flow::withdraw_tank(ctx, vault(), dollars(1), &fresh).await })
    });
    assert!(
        matches!(result, Ok(Outcome::TankWithdrawn { .. })),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![Prompt::WithdrawTank { amount: dollars(1) }]
    );
}

// Allowances, the idle floor, retiring ---------------------------------------------

#[test]
fn allowances_are_sent_exactly_as_typed() {
    let world = World::live();
    let quoting = form::shape("quoting");
    let chosen = quoting;
    let (result, _) = world
        .run(move |ctx| Box::pin(async move { flow::set_allowances(ctx, vault(), &chosen).await }));
    assert!(matches!(result, Ok(Outcome::AllowancesSet { .. })));
    assert_eq!(world.sim.borrow().allowances, quoting);
}

#[test]
fn lowering_the_floor_waits_a_day_and_cancel_change_keeps_the_floor_in_force() {
    let world = World::live();
    let (result, _) = world.run(|ctx| Box::pin(flow::set_idle_floor(ctx, vault(), 300)));
    assert!(
        matches!(result, Ok(Outcome::FloorSet { bps: 300, effective_at, .. }) if effective_at == 1_000_000 + DAY),
        "{result:?}"
    );
    assert_eq!(world.sim.borrow().floor, (500, 300, 1_000_000 + DAY));
    let (result, steps) = world.run(|ctx| Box::pin(flow::cancel_floor_change(ctx, vault())));
    assert!(
        matches!(result, Ok(Outcome::FloorKept { bps: 500, .. })),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![Prompt::CancelFloorChange { bps: 500 }]
    );
    assert_eq!(world.sim.borrow().floor, (500, 0, 0));
    // Nothing waiting now.
    let (result, _) = world.run(|ctx| Box::pin(flow::cancel_floor_change(ctx, vault())));
    assert_eq!(result, Err(AuthorError::NothingPending));
}

#[test]
fn a_lowering_whose_time_has_come_has_taken_effect_and_cannot_be_cancelled() {
    let world = World::live();
    world.with(|chain| {
        chain.floor = (500, 0, chain.time - 1);
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::cancel_floor_change(ctx, vault())));
    assert_eq!(result, Err(AuthorError::NothingPending));
    assert!(world.calls().is_empty());
}

#[test]
fn raising_the_floor_applies_at_once() {
    let world = World::live();
    let (result, _) = world.run(|ctx| Box::pin(flow::set_idle_floor(ctx, vault(), 800)));
    assert!(matches!(
        result,
        Ok(Outcome::FloorSet {
            bps: 800,
            effective_at: 1_000_000,
            ..
        })
    ));
    assert_eq!(world.sim.borrow().floor, (800, 0, 0));
    let (result, _) = world.run(|ctx| Box::pin(flow::set_idle_floor(ctx, vault(), 10_001)));
    assert_eq!(result, Err(AuthorError::FloorTooHigh));
}

#[test]
fn retiring_winds_the_vault_down_once_and_only_from_open() {
    let world = World::live();
    let (result, steps) = world.run(|ctx| Box::pin(flow::retire(ctx, vault())));
    assert!(matches!(result, Ok(Outcome::Retired { .. })));
    assert_eq!(prompts(&steps), vec![Prompt::Retire]);
    assert_eq!(world.sim.borrow().state, State::WindingDown);
    let (result, _) = world.run(|ctx| Box::pin(flow::retire(ctx, vault())));
    assert_eq!(result, Err(AuthorError::NotOpen));
    assert_eq!(world.calls(), vec![IVault::retireCall::SELECTOR]);
}
