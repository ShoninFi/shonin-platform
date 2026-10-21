//! The author's money flows on the EVM chains, through their public ports,
//! over a small simulated chain and wallet: the activation's statement as
//! the vault takes it, an approval left standing when the activation stops,
//! a signed tank withdrawal that asks the wallet for exactly what it pays,
//! and the job page's words about a payment.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use app::tx::author::AuthorError;
use app::tx::author::flow::{self, Activation, Ctx, Outcome, Prompt, Spender, Step, Usage};
use app::tx::author::form::{self, Allowances};
use app::tx::author::read::{PaymentRecord, Registries};
use app::tx::error::Context;
use app::tx::flow::{Cancel, Pace};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use app::tx::read::State;
use app::tx::{Revert, TxError};
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::{ChainName, IVaultFactory, VaultConfig};
use chain::bindings::prices::IPrices;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::strategies::{ChainNeed, IStrategyRegistry, Strategy};
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

const CHAIN: u64 = 31337;
const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;
const DAY: u64 = 86_400;

fn usd() -> Address {
    Address::repeat_byte(0xdd)
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
fn name(text: &str) -> B256 {
    B256::right_padding_from(text.as_bytes())
}
fn dollars(amount: u64) -> U256 {
    U256::from(amount * USD)
}

struct Chain {
    time: u64,
    block: u64,
    usd: HashMap<Address, U256>,
    native: HashMap<Address, U256>,
    allowance: HashMap<(Address, Address), U256>,
    author: Address,
    terms: B256,
    minimum_tank: U256,
    activated: bool,
    state: State,
    tank: b::Tank,
    allowances: Allowances,
    shares: HashMap<Address, U256>,
    receipts: HashMap<B256, Receipt>,
    logs: Vec<Log>,
    nonce: HashMap<Address, u64>,
    sent: Vec<TxRequest>,
    wallet_chain: u64,
    wallet_account: Option<Address>,
    /// The wallet ends a signature with the recovery id itself (0 or 1), as
    /// some do, rather than 27 or 28.
    wallet_parity_byte: bool,
    /// The factory refuses the activation for this reason, after the tank's
    /// approval.
    refuse_activation: Option<&'static str>,
    /// The wallet declines every transaction after this many were sent.
    reject_after: Option<usize>,
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
            author: account,
            terms: B256::repeat_byte(0x7a),
            minimum_tank: U256::from(10 * USD),
            activated: false,
            state: State::None,
            tank: empty_tank(),
            allowances: Allowances::default(),
            shares,
            receipts: HashMap::new(),
            logs: Vec::new(),
            nonce: HashMap::new(),
            sent: Vec::new(),
            wallet_chain: CHAIN,
            wallet_account: Some(account),
            wallet_parity_byte: false,
            refuse_activation: None,
            reject_after: None,
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

/// Whether a 65-byte signature ends the way the vault's `ECDSA` check takes
/// it: `v` as 27 or 28. `ecrecover` with 0 or 1 recovers nobody.
fn vault_takes(signature: &[u8]) -> bool {
    signature.len() == 65 && matches!(signature[64], 27 | 28)
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
        if to == strategies() {
            assert_eq!(selector, IStrategyRegistry::strategyOfCall::SELECTOR);
            return ok(Strategy {
                author: self.author,
                registeredAt: 5,
                chains: vec![ChainNeed {
                    chain: name("base"),
                    drivers: vec![name("local")],
                }],
                sourceHash: B256::ZERO,
                bundleHash: B256::ZERO,
                manifestHash: B256::ZERO,
                bridges: Vec::new(),
                restricted: Vec::new(),
            });
        }
        if to == reviews() {
            assert_eq!(selector, IReviewRegistry::hasReviewCall::SELECTOR);
            return ok(true);
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
                IVaultFactory::chainsCall::SELECTOR => ok(vec![ChainName {
                    name: name("base"),
                    chainId: U256::from(CHAIN),
                }]),
                _ => panic!("unexpected factory read"),
            };
        }
        assert_eq!(to, vault(), "a read of an unknown contract");
        match selector {
            IVault::stateCall::SELECTOR => ok(sol_state(self.state)),
            IVault::authorCall::SELECTOR => ok(self.author),
            IVault::tankCall::SELECTOR => ok(self.tank.clone()),
            IVault::allowancesCall::SELECTOR => ok(self.allowances.to_sol()),
            IVault::idleFloorCall::SELECTOR => {
                Ok(Bytes::from((500u16, 0u16, 0u64).abi_encode_params()))
            }
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

    /// What `eth_estimateGas` says: the reason when the contract would
    /// revert.
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
                } else if !vault_takes(&call.statement) {
                    // `recordStatement` → `SignatureChecker` → `ECDSA`.
                    Some("BadSignature")
                } else {
                    self.refuse_activation
                }
            }
            (to, _) if to == vault() => (from != self.author).then_some("NotAllowed"),
            (to, IERC20::approveCall::SELECTOR) if to == usd() => None,
            _ => panic!("a transaction to an unknown contract"),
        }
    }

    fn execute(&mut self, request: &TxRequest) -> bool {
        let data = request.data.clone();
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if self.would_revert(request).is_some() {
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
        if request.to == factory() {
            let call = IVaultFactory::activateCall::abi_decode(&data).unwrap();
            pay(self, factory(), vault(), call.tankFunding);
            self.activated = true;
            self.state = State::Open;
            self.tank.balance = call.tankFunding;
            self.allowances = Allowances::from_sol(&call.allowances);
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
            IVault::withdrawTankCall::SELECTOR => {
                // As the vault does: the bill is collected first, from the
                // tank, then what is asked for is paid, never more than
                // what the tank still holds.
                let call = IVault::withdrawTankCall::abi_decode(&data).unwrap();
                if call.total != self.tank.usageSettled || call.asOf != self.tank.usageAsOf {
                    let billed = call.total - self.tank.usageSettled;
                    let charge = billed.min(self.tank.balance);
                    self.tank.usageSettled = call.total;
                    self.tank.usageAsOf = call.asOf;
                    self.tank.balance -= charge;
                    *self.usd.entry(Address::repeat_byte(0x01)).or_default() += charge;
                    self.log(
                        vault(),
                        IVault::TankCharged {
                            billed,
                            charged: charge,
                            total: call.total,
                        },
                    );
                }
                let assets = call.assets.min(self.tank.balance);
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
        let signature = self.key.sign_hash_sync(&statement.digest()).unwrap();
        let mut bytes = signature.as_bytes().to_vec();
        if self.sim.borrow().wallet_parity_byte {
            bytes[64] -= 27;
        }
        Ok(Bytes::from(bytes))
    }
    async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
        let mut sim = self.sim.borrow_mut();
        if sim
            .reject_after
            .is_some_and(|after| sim.sent.len() >= after)
        {
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
                from: Some(request.from),
                success,
                block,
                logs,
            },
        );
        Ok(hash)
    }
}

fn context() -> Context<'static> {
    Context {
        wallet: "MetaMask",
        keeps_activity: true,
        chain: "Base",
        wallet_chain: None,
        symbol: "USDC",
        decimals: 6,
        native: "ETH",
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

// Activation --------------------------------------------------------------------

/// A wallet may end the statement's signature with the recovery id itself
/// (0 or 1); the vault's `ECDSA` check takes only 27 or 28. The activation
/// sends it in the vault's form, as the deposit does, so such a wallet can
/// activate.
#[test]
fn an_activation_statement_signed_with_a_parity_byte_is_sent_as_the_vault_takes_it() {
    let world = World::new();
    world.with(|chain| chain.wallet_parity_byte = true);
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    assert!(
        matches!(result, Ok(Outcome::Activated { vault: at, .. }) if at == vault()),
        "the activation didn't go through: {result:?}; prompts {:?}",
        prompts(&steps)
    );
    let sim = world.sim.borrow();
    let activate = sim
        .sent
        .iter()
        .find(|tx| tx.data[..4] == IVaultFactory::activateCall::SELECTOR)
        .expect("the activation was never sent");
    let call = IVaultFactory::activateCall::abi_decode(&activate.data).unwrap();
    assert!(
        vault_takes(&call.statement),
        "v byte {}",
        call.statement[64]
    );
    assert_eq!(sim.tank.balance, dollars(210));
}

/// When the activation stops after the tank's approval was mined, the
/// approval stands: the words say so (never "Nothing was sent"), and trying
/// again goes straight to the activation without approving again.
#[test]
fn an_activation_stopped_after_its_approval_says_the_approval_stands_and_retries_without_it() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| chain.refuse_activation = Some("NotAllowed"));
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        AuthorError::ApprovalStands {
            spender: flow::Spender::Factory,
            amount: dollars(210),
            then: Box::new(AuthorError::Tx(revert("NotAllowed"))),
        }
    );
    assert!(error.after_send());
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Statement,
            Prompt::Approve {
                amount: dollars(210),
                spender: Spender::Factory
            }
        ]
    );
    let words = error.words(&context());
    assert!(!words.text.contains("Nothing was sent"), "{}", words.text);
    assert!(
        words
            .text
            .contains("Your approval of $210.00 to the vault factory went through and stands"),
        "{}",
        words.text
    );
    assert_eq!(
        world.sim.borrow().allowance.get(&(account, factory())),
        Some(&dollars(210))
    );

    world.with(|chain| chain.refuse_activation = None);
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    assert!(
        matches!(result, Ok(Outcome::Activated { .. })),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![Prompt::Statement, Prompt::Activate { tank: dollars(210) }]
    );
}

/// The tank is approved and mined, then the author declines the activation
/// itself in the wallet: the approval stands, and no title says nothing was
/// sent above the words that say so.
#[test]
fn an_activation_declined_after_its_approval_is_not_titled_nothing_was_sent() {
    let world = World::new();
    let account = world.account();
    // The wallet declines the second transaction: the activation.
    world.with(|chain| chain.reject_after = Some(1));
    let (result, _) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        AuthorError::ApprovalStands {
            spender: flow::Spender::Factory,
            amount: dollars(210),
            then: Box::new(AuthorError::Tx(TxError::Rejected)),
        }
    );
    assert_eq!(
        world.sim.borrow().allowance.get(&(account, factory())),
        Some(&dollars(210))
    );
    let words = error.words(&context());
    assert!(words.text.contains("stands"), "{}", words.text);
    assert!(
        !words.title.contains("Nothing was sent"),
        "{:?} / {:?}",
        words.title,
        words.text
    );
}

/// An approval of the tank stands after a stopped activation; activating
/// again with a smaller tank approves exactly that tank first, so nothing
/// larger than the action stays approved to the factory.
#[test]
fn activating_again_with_a_smaller_tank_leaves_no_approval_larger_than_the_tank() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| chain.refuse_activation = Some("NotAllowed"));
    let (result, _) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    assert!(
        matches!(result, Err(AuthorError::ApprovalStands { .. })),
        "{result:?}"
    );
    world.with(|chain| chain.refuse_activation = None);
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(100))).await
        })
    });
    assert!(
        matches!(result, Ok(Outcome::Activated { .. })),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![
            Prompt::Statement,
            Prompt::Approve {
                amount: dollars(100),
                spender: Spender::Factory
            },
            Prompt::Activate { tank: dollars(100) }
        ]
    );
    assert_eq!(world.sim.borrow().tank.balance, dollars(100));
    assert_eq!(
        world
            .sim
            .borrow()
            .allowance
            .get(&(account, factory()))
            .copied()
            .unwrap_or_default(),
        U256::ZERO
    );
}

/// An activation refused before any approval leaves nothing standing and
/// says nothing was sent.
#[test]
fn an_activation_refused_before_its_approval_says_nothing_was_sent() {
    let world = World::new();
    let account = world.account();
    world.with(|chain| {
        chain.allowance.insert((account, factory()), dollars(210));
        chain.refuse_activation = Some("NotAllowed");
    });
    let (result, _) = world.run(|ctx| {
        Box::pin(async move {
            flow::activate(ctx, &registries(), usd(), &activation(dollars(210))).await
        })
    });
    assert_eq!(result, Err(AuthorError::Tx(revert("NotAllowed"))));
}

// The tank ------------------------------------------------------------------------

/// A signed withdrawal settles the bill first, then pays what is asked out
/// of what is left: the wallet is asked for what will be paid, never more,
/// and is told how much of the bill comes off first.
#[test]
fn a_signed_withdrawal_asks_the_wallet_for_the_figure_that_will_be_paid() {
    let world = World::live();
    let account = world.account();
    let before = world.usd_of(account);
    // The executor's latest total: $2 of usage nobody has collected yet.
    let fresh = Usage {
        total: dollars(2),
        as_of: 1_000_000 - 60,
        signature: Bytes::from(vec![1u8; 65]),
    };
    // Everything the page says the tank holds.
    let asked = U256::from(9_990_000u64);
    let (result, steps) = world
        .run(|ctx| Box::pin(async move { flow::withdraw_tank(ctx, vault(), asked, &fresh).await }));
    let paid = match result {
        Ok(Outcome::TankWithdrawn { amount, .. }) => amount,
        other => panic!("{other:?}"),
    };
    assert_eq!(world.usd_of(account), before + paid);
    let prompted = prompts(&steps)
        .into_iter()
        .find_map(|prompt| match prompt {
            Prompt::WithdrawTank { amount, tab } => Some((amount, tab)),
            _ => None,
        })
        .expect("the wallet was asked");
    let (prompted, tab) = prompted;
    assert_eq!(tab, dollars(2));
    // Either refuse the amount before the wallet is asked (the tank less the
    // bill is what can be paid) or ask the wallet for what will be paid.
    assert_eq!(
        prompted, paid,
        "the wallet was asked for {prompted} and the tank paid {paid}"
    );
}

/// A bill that would take the whole tank leaves nothing to withdraw: refused
/// before the wallet is asked.
#[test]
fn a_withdrawal_the_bill_would_empty_is_refused_before_the_wallet_is_asked() {
    let world = World::live();
    let fresh = Usage {
        total: dollars(20),
        as_of: 1_000_000 - 60,
        signature: Bytes::from(vec![1u8; 65]),
    };
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move { flow::withdraw_tank(ctx, vault(), dollars(1), &fresh).await })
    });
    assert_eq!(
        result,
        Err(AuthorError::TabTakesTank {
            tab: U256::from(9_990_000u64)
        })
    );
    assert!(prompts(&steps).is_empty());
    assert!(world.sim.borrow().sent.is_empty());
}

/// The vault collects only a total signed later than the one it settled
/// last (and not ahead of its clock); an older one would revert once mined.
/// It is refused before the wallet is asked.
#[test]
fn a_total_not_later_than_the_one_settled_is_refused_before_the_wallet_is_asked() {
    let world = World::live();
    world.with(|chain| {
        chain.tank.usageSettled = dollars(1);
        chain.tank.usageAsOf = 1_000_000 - 30;
    });
    for usage in [
        // Signed before the total the vault settled.
        Usage {
            total: dollars(2),
            as_of: 1_000_000 - 60,
            signature: Bytes::from(vec![1u8; 65]),
        },
        // Ahead of the chain's clock.
        Usage {
            total: dollars(2),
            as_of: 1_000_000 + 60,
            signature: Bytes::from(vec![1u8; 65]),
        },
    ] {
        let (result, steps) = world.run(|ctx| {
            Box::pin(async move { flow::withdraw_tank(ctx, vault(), dollars(1), &usage).await })
        });
        assert_eq!(result, Err(AuthorError::NoUsageTotal));
        assert!(prompts(&steps).is_empty());
    }
    // The total already settled isn't collected again: nothing comes off.
    let settled = Usage {
        total: dollars(1),
        as_of: 1_000_000 - 30,
        signature: Bytes::from(vec![1u8; 65]),
    };
    let (result, steps) = world.run(|ctx| {
        Box::pin(async move { flow::withdraw_tank(ctx, vault(), dollars(9), &settled).await })
    });
    assert!(
        matches!(result, Ok(Outcome::TankWithdrawn { amount, .. }) if amount == dollars(9)),
        "{result:?}"
    );
    assert_eq!(
        prompts(&steps),
        vec![Prompt::WithdrawTank {
            amount: dollars(9),
            tab: U256::ZERO
        }]
    );
}

// The job page ---------------------------------------------------------------------

/// Once the author's payment is mined, the build service admits the job only
/// after it has read the payment confirmed, and until then the queue still
/// says the job awaits payment: the page never says nothing was charged.
#[test]
fn the_job_page_never_says_nothing_was_charged_for_a_job_the_author_has_paid_for() {
    use shared::{Job, JobState};
    let job = Job {
        id: "8801".to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: "Funding basis".to_string(),
        author: "0x2222222222222222222222222222222222222222".to_string(),
        state: JobState::AwaitingPayment,
        submitted: time::PrimitiveDateTime::new(
            time::Date::from_ordinal_date(2026, 281).unwrap(),
            time::Time::MIDNIGHT,
        ),
        duration_seconds: 0,
        source_hash: String::new(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    };
    let (_, detail) = app::pages::publish::job::status_words(&job);
    assert!(
        !detail.to_lowercase().contains("nothing has been charged"),
        "{detail}"
    );
    // With the payment the chain records for the upload, from the wallet
    // that made it: paid, and waiting for the confirmations.
    let (headline, detail) = app::pages::publish::job::job_words(
        &job,
        Some(PaymentRecord::ByUploader {
            strategy_id: U256::from(8u8),
        }),
    );
    assert!(headline.starts_with("Paid"), "{headline}");
    assert!(detail.contains("confirmed"), "{detail}");
    assert!(!detail.to_lowercase().contains("nothing"), "{detail}");
}

/// A payment the chain records for the upload from a wallet the upload
/// didn't name starts nothing, and the job page never says it started the
/// job; neither does any success the payment flow reports.
#[test]
fn a_payment_from_another_wallet_is_never_shown_as_starting_the_job() {
    use shared::{Job, JobState};
    let job = Job {
        id: "8808".to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: "Funding basis".to_string(),
        author: "0x2222222222222222222222222222222222222222".to_string(),
        state: JobState::AwaitingPayment,
        submitted: time::PrimitiveDateTime::new(
            time::Date::from_ordinal_date(2026, 281).unwrap(),
            time::Time::MIDNIGHT,
        ),
        duration_seconds: 0,
        source_hash: String::new(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    };
    let (headline, detail) = app::pages::publish::job::job_words(
        &job,
        Some(PaymentRecord::ByAnother {
            strategy_id: U256::from(8u8),
            payer: Address::repeat_byte(0x97),
        }),
    );
    assert!(!headline.contains("started"), "{headline}");
    assert!(detail.contains("doesn't start the job"), "{detail}");
    // No record: the queue's own words.
    assert_eq!(
        app::pages::publish::job::job_words(&job, Some(PaymentRecord::None)),
        app::pages::publish::job::status_words(&job)
    );
}
