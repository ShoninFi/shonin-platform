//! What the author's flows say and leave standing when a payment or a
//! top-up stops after its approval was mined, and that a deposit the vault
//! settles while the wallet is still asked for the second step ends on its
//! own, however long the wallet takes. Each test runs a flow over a small
//! simulated chain and wallet through the flows' public ports, the way the
//! crate's own tests do.

// The author's flows ------------------------------------------------------------

mod author {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{SolCall, SolEvent, SolValue};
    use app::tx::author::AuthorError;
    use app::tx::author::flow::{self, Ctx, Outcome, Prompt, Step};
    use app::tx::author::form::{self, Allowances};
    use app::tx::error::Context;
    use app::tx::flow::{Cancel, Pace};
    use app::tx::port::{
        Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest,
    };
    use app::tx::read::State;
    use app::tx::{Revert, TxError};
    use chain::bindings::erc20::IERC20;
    use chain::bindings::publishing::{IPublishing, Submission};
    use chain::bindings::vault::{self as b, IVault};
    use futures::executor::block_on;

    const CHAIN: u64 = 31337;
    const USD: u64 = 1_000_000;
    const WAD: u128 = 1_000_000_000_000_000_000;
    const DAY: u64 = 86_400;

    fn usd() -> Address {
        Address::repeat_byte(0xdd)
    }
    fn publishing() -> Address {
        Address::repeat_byte(0xb0)
    }
    fn vault() -> Address {
        Address::repeat_byte(0xaa)
    }
    fn upload() -> B256 {
        B256::repeat_byte(0x11)
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
        fee: U256,
        /// The publishing contract's record: who paid, for which upload.
        submissions: Vec<(Address, B256)>,
        author: Address,
        state: State,
        tank: b::Tank,
        allowances: Allowances,
        shares: HashMap<Address, U256>,
        receipts: HashMap<B256, Receipt>,
        logs: Vec<Log>,
        nonce: HashMap<Address, u64>,
        /// Every transaction's selector, in the order sent.
        calls: Vec<[u8; 4]>,
        sent: Vec<TxRequest>,
        wallet_chain: u64,
        wallet_account: Option<Address>,
        /// The wallet declines every transaction after this many were sent.
        reject_after: Option<usize>,
    }

    type Sim = Rc<RefCell<Chain>>;

    pub struct World {
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
                state: State::None,
                tank: empty_tank(),
                allowances: Allowances::default(),
                shares,
                receipts: HashMap::new(),
                logs: Vec::new(),
                nonce: HashMap::new(),
                calls: Vec::new(),
                sent: Vec::new(),
                wallet_chain: CHAIN,
                wallet_account: Some(account),
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

        fn pay(&self) -> (Result<Outcome, AuthorError>, Vec<Step>) {
            let uploader = self.account();
            self.run(|ctx| {
                Box::pin(async move { flow::pay(ctx, publishing(), upload(), uploader).await })
            })
        }

        fn fund_tank(&self, amount: U256) -> (Result<Outcome, AuthorError>, Vec<Step>) {
            self.run(|ctx| {
                Box::pin(async move { flow::fund_tank(ctx, vault(), usd(), amount).await })
            })
        }

        fn calls(&self) -> Vec<[u8; 4]> {
            self.sim.borrow().calls.clone()
        }

        fn allowance(&self, owner: Address, spender: Address) -> U256 {
            self.sim
                .borrow()
                .allowance
                .get(&(owner, spender))
                .copied()
                .unwrap_or_default()
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

    pub struct NoWait;
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
                    IPublishing::countCall::SELECTOR => ok(U256::from(self.submissions.len())),
                    IPublishing::submissionOfCall::SELECTOR => {
                        let call = IPublishing::submissionOfCall::abi_decode(data).unwrap();
                        let at = usize::try_from(call.strategyId).unwrap_or(usize::MAX);
                        let (author, upload) = at
                            .checked_sub(1)
                            .and_then(|at| self.submissions.get(at).copied())
                            .unwrap_or((Address::ZERO, B256::ZERO));
                        ok(Submission {
                            author,
                            upload,
                            paidAt: 0,
                        })
                    }
                    _ => panic!("unexpected publishing read"),
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
                (to, IPublishing::submitCall::SELECTOR) if to == publishing() => {
                    (!takes(publishing(), self.fee)).then_some("Allowance")
                }
                (to, IVault::fundTankCall::SELECTOR) if to == vault() => {
                    let call = IVault::fundTankCall::abi_decode(data).unwrap();
                    (!takes(vault(), call.assets)).then_some("Allowance")
                }
                (to, IERC20::approveCall::SELECTOR) if to == usd() => None,
                _ => panic!("a transaction to an unknown contract"),
            }
        }

        fn execute(&mut self, request: &TxRequest) -> bool {
            let data = request.data.clone();
            let selector: [u8; 4] = data[..4].try_into().unwrap();
            self.calls.push(selector);
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
            if request.to == publishing() {
                let call = IPublishing::submitCall::abi_decode(&data).unwrap();
                let fee = self.fee;
                pay(self, publishing(), Address::repeat_byte(0x01), fee);
                self.submissions.push((from, call.upload));
                let id = U256::from(self.submissions.len());
                self.log(
                    publishing(),
                    IPublishing::Submitted {
                        strategyId: id,
                        author: from,
                        upload: call.upload,
                        fee,
                    },
                );
                return true;
            }
            if request.to == vault() && selector == IVault::fundTankCall::SELECTOR {
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
                return true;
            }
            panic!("unexpected transaction {selector:?}")
        }
    }

    pub struct SimReader(Sim);

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

    pub struct SimWallet {
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
            Ok(Bytes::from(signature.as_bytes().to_vec()))
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

    /// The words an error is shown with, as the page shows them.
    fn said(error: &AuthorError) -> String {
        let words = error.words(&context());
        format!("{} {}", words.title, words.text)
    }

    /// An approval went out and was mined before the flow stopped: the
    /// words must not say nothing was sent, and the page must be told
    /// something was (`after_send`).
    fn assert_says_the_approval_went(error: &AuthorError, approved: U256) {
        let said = said(error);
        assert!(
            error.after_send(),
            "an approval of {approved} was mined, but the page is told nothing was sent: {error:?}"
        );
        assert!(
            !said.contains("Nothing was sent")
                && !said.contains("No money moved")
                && !said.contains("no request was made"),
            "an approval of {approved} was mined and stands, but the page says: {said:?}"
        );
        assert!(
            said.contains("stands") || said.contains("approval"),
            "the standing approval of {approved} is not mentioned: {said:?}"
        );
    }

    // The payment ------------------------------------------------------------

    /// The fee's approval is sent and mined, then the author declines the
    /// payment itself in the wallet. The approval of exactly the fee to the
    /// publishing contract stands, and is used by the next payment without a
    /// prompt; the words must not say nothing was sent.
    #[test]
    fn a_payment_declined_after_its_approval_never_says_nothing_was_sent() {
        let world = World::new();
        world.with(|chain| chain.reject_after = Some(1));
        let (result, steps) = world.pay();
        let error = result.unwrap_err();
        assert!(
            matches!(&error, AuthorError::ApprovalStands { then, .. } if **then == AuthorError::Tx(TxError::Rejected)),
            "{error:?} {steps:?}"
        );
        // Only the approval went: the fee is approved and still held.
        assert_eq!(world.calls(), vec![IERC20::approveCall::SELECTOR]);
        assert_eq!(
            world.allowance(world.account(), publishing()),
            U256::from(USD),
            "the approval of exactly the fee stands"
        );
        assert_eq!(world.usd_of(world.account()), U256::from(500 * USD));
        assert_eq!(
            prompts(&steps),
            vec![
                Prompt::Approve {
                    amount: U256::from(USD),
                    spender: flow::Spender::Publishing
                },
                Prompt::Pay {
                    fee: U256::from(USD)
                }
            ]
        );
        assert_says_the_approval_went(&error, U256::from(USD));
    }

    // The tank top-up ----------------------------------------------------------

    /// The top-up's approval is sent and mined, then the author declines the
    /// top-up itself. The approval of the whole amount to the vault stands;
    /// the words must not say nothing was sent.
    #[test]
    fn a_top_up_declined_after_its_approval_never_says_nothing_was_sent() {
        let world = World::live();
        world.with(|chain| chain.reject_after = Some(1));
        let (result, steps) = world.fund_tank(dollars(50));
        let error = result.unwrap_err();
        assert!(
            matches!(&error, AuthorError::ApprovalStands { then, .. } if **then == AuthorError::Tx(TxError::Rejected)),
            "{error:?} {steps:?}"
        );
        assert_eq!(world.calls(), vec![IERC20::approveCall::SELECTOR]);
        assert_eq!(
            world.allowance(world.account(), vault()),
            dollars(50),
            "the approval of the top-up stands"
        );
        assert_eq!(world.sim.borrow().tank.balance, U256::from(9_990_000u64));
        assert_says_the_approval_went(&error, dollars(50));
    }

    /// After a $50 top-up stopped once its approval was mined, a $20 top-up
    /// approves exactly $20: nothing larger than the action stays approved
    /// to the vault.
    #[test]
    fn topping_up_again_with_a_smaller_amount_leaves_no_approval_larger_than_the_top_up() {
        let world = World::live();
        world.with(|chain| chain.reject_after = Some(1));
        let (first, _) = world.fund_tank(dollars(50));
        assert!(
            matches!(&first, Err(AuthorError::ApprovalStands { .. })),
            "{first:?}"
        );
        assert_eq!(world.allowance(world.account(), vault()), dollars(50));

        world.with(|chain| chain.reject_after = None);
        let (second, steps) = world.fund_tank(dollars(20));
        assert!(
            matches!(second, Ok(Outcome::TankFunded { amount, .. }) if amount == dollars(20)),
            "{second:?}"
        );
        assert_eq!(
            world.sim.borrow().tank.balance,
            U256::from(9_990_000u64) + dollars(20)
        );
        let left = world.allowance(world.account(), vault());
        assert!(
            left.is_zero(),
            "the top-up took $20 and left {} approved to the vault (prompts: {:?})",
            left / U256::from(USD),
            prompts(&steps)
        );
    }

    // The record looked for before paying -------------------------------------

    /// A payment this wallet made for another upload is not this upload's:
    /// the payment is still asked for. (Sound; kept as the brief's check on
    /// what the look before paying may skip.)
    #[test]
    fn a_payment_by_this_wallet_for_another_upload_is_not_taken_as_this_ones() {
        let world = World::new();
        let uploader = world.account();
        world.with(|chain| chain.submissions = vec![(uploader, B256::repeat_byte(0x22))]);
        let (result, _) = world.pay();
        assert!(
            matches!(result, Ok(Outcome::Paid { strategy_id, .. }) if strategy_id == U256::from(2u8)),
            "{result:?}"
        );
        assert_eq!(world.sim.borrow().submissions[1], (uploader, upload()));
    }
}

// The depositor's second step ------------------------------------------------------

mod depositor {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::task::Poll;

    use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{SolCall, SolEvent, SolValue};
    use app::tx::TxError;
    use app::tx::flow::{self, Cancel, Ctx, Nudge, Outcome, Pace, Prompt, Step};
    use app::tx::port::{
        Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest,
    };
    use chain::bindings::vault::{self as b, IVault};
    use futures::executor::block_on;

    const CHAIN: u64 = 31337;
    const USD: u64 = 1_000_000;
    const WAD: u128 = 1_000_000_000_000_000_000;

    fn vault() -> Address {
        Address::repeat_byte(0xaa)
    }
    fn asset() -> Address {
        Address::repeat_byte(0xdd)
    }

    struct Chain {
        time: u64,
        block: u64,
        restricted: Vec<B256>,
        restricted_text: String,
        statement_of: HashMap<Address, B256>,
        usd: HashMap<Address, U256>,
        allowance: HashMap<(Address, Address), U256>,
        shares: HashMap<Address, U256>,
        /// (owner, kind, earliest, done, amount); kind 0 deposit.
        requests: Vec<(Address, u8, u64, bool, U256)>,
        /// The time of the newest priced valuation; the keeper prices the
        /// first waiting request's slot when it is polled.
        priced: Option<u64>,
        receipts: HashMap<B256, Receipt>,
        logs: Vec<(u64, Log)>,
        nonce: HashMap<Address, u64>,
        sent: Vec<TxRequest>,
        wallet_chain: u64,
        wallet_account: Address,
        /// The wallet has been asked for the second step (whether or not it
        /// has answered since).
        prompt_opened: bool,
        /// Reads of the request since the wallet was asked.
        reads_since_prompt: u32,
        /// The keeper settles the request at this many reads since the
        /// wallet was asked (and never before).
        keeper_settles_at_read: u32,
        /// The person presses "Skip" at this many reads since the wallet
        /// was asked.
        skip_at_read: Option<u32>,
        /// How many times the wallet's open prompt has been polled; it
        /// answers (declining) after a great many.
        prompt_polls: u32,
        skip: Option<Nudge>,
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
            let restricted = vec![
                B256::right_padding_from(b"SANCTIONED"),
                B256::right_padding_from(b"US"),
            ];
            let mut usd = HashMap::new();
            usd.insert(account, U256::from(10_000u64 * USD));
            usd.insert(vault(), U256::from(100_000u64 * USD));
            let mut shares = HashMap::new();
            shares.insert(account, U256::from(5_000u128 * WAD));
            let mut allowance = HashMap::new();
            allowance.insert((account, vault()), U256::MAX);
            let mut statement_of = HashMap::new();
            statement_of.insert(account, keccak256(restricted.abi_encode()));
            let chain = Chain {
                time: 1_000,
                block: 100,
                restricted,
                restricted_text: "SANCTIONED,US".to_string(),
                statement_of,
                usd,
                allowance,
                shares,
                requests: Vec::new(),
                priced: None,
                receipts: HashMap::new(),
                logs: Vec::new(),
                nonce: HashMap::new(),
                sent: Vec::new(),
                wallet_chain: CHAIN,
                wallet_account: account,
                prompt_opened: false,
                reads_since_prompt: 0,
                keeper_settles_at_read: u32::MAX,
                skip_at_read: None,
                prompt_polls: 0,
                skip: None,
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

        fn deposit(&self, dollars: u64) -> (Result<Outcome, TxError>, Vec<Step>) {
            let steps = RefCell::new(Vec::new());
            let on_step = |step: Step| steps.borrow_mut().push(step);
            let skip_second = Nudge::new();
            self.sim.borrow_mut().skip = Some(skip_second.clone());
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
                settle_now: Nudge::new(),
                skip_second,
                pace: Pace::FAST,
                on_step: &on_step,
            };
            let result = block_on(flow::deposit(
                &ctx,
                U256::from(dollars * USD),
                "SANCTIONED,US",
            ));
            (result, steps.into_inner())
        }
    }

    struct NoWait;
    impl Clock for NoWait {
        async fn sleep(&self, _millis: u32) {}
    }

    fn ok<T: SolValue>(value: T) -> Result<Bytes, TxError> {
        Ok(Bytes::from(value.abi_encode()))
    }

    impl Chain {
        fn request_valuation(&self) -> b::Valuation {
            b::Valuation {
                time: self.priced.unwrap_or_default(),
                status: b::ValuationStatus::Priced,
                gross: U256::from(100_000u64 * USD + 1),
                grossExit: U256::from(100_000u64 * USD + 1),
                supply: U256::from(100_000u128 * WAD) + U256::from(1_000_000_000_000u64),
                fee: U256::ZERO,
                feeExit: U256::ZERO,
                cash: self.usd[&vault()],
                postsMissing: 0,
            }
        }

        /// The keeper, on each look at a request: prices its slot, and
        /// settles only when the scenario says.
        fn tick(&mut self) {
            if let Some(request) = self.requests.iter().find(|r| !r.3) {
                let earliest = request.2;
                self.time = self.time.max(earliest + 1);
                self.priced = Some(earliest);
            }
            if self.prompt_opened {
                self.reads_since_prompt += 1;
                if self.reads_since_prompt == self.keeper_settles_at_read {
                    self.settle_all();
                }
                if Some(self.reads_since_prompt) == self.skip_at_read
                    && let Some(skip) = &self.skip
                {
                    skip.nudge();
                }
            }
        }

        /// Settle every waiting request at a dollar a share, in its own
        /// block: a deposit buys shares.
        fn settle_all(&mut self) {
            self.block += 1;
            let waiting: Vec<usize> = (0..self.requests.len())
                .filter(|&i| !self.requests[i].3)
                .collect();
            for i in waiting {
                self.requests[i].3 = true;
                let (owner, _, _, _, amount) = self.requests[i];
                let id = U256::from(i + 1);
                self.log(
                    vault(),
                    IVault::Deposited {
                        requestId: id,
                        owner,
                        assets: amount,
                        shares: amount * U256::from(1_000_000_000_000u64),
                    },
                );
            }
        }

        fn log<E: SolEvent>(&mut self, address: Address, event: E) {
            let data = event.encode_log_data();
            self.logs.push((
                self.block,
                Log {
                    address,
                    topics: data.topics().to_vec(),
                    data: data.data.clone(),
                    transaction: None,
                },
            ));
        }

        fn call(&mut self, to: Address, data: &[u8]) -> Result<Bytes, TxError> {
            let selector: [u8; 4] = data[..4].try_into().unwrap();
            if to == asset() {
                return match selector {
                    chain::bindings::erc20::IERC20::balanceOfCall::SELECTOR => {
                        let call = chain::bindings::erc20::IERC20::balanceOfCall::abi_decode(data)
                            .unwrap();
                        ok(self.usd.get(&call.account).copied().unwrap_or_default())
                    }
                    chain::bindings::erc20::IERC20::allowanceCall::SELECTOR => {
                        let call = chain::bindings::erc20::IERC20::allowanceCall::abi_decode(data)
                            .unwrap();
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
                IVault::stateCall::SELECTOR => ok(b::VaultState::Open),
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
                IVault::minimumDepositCall::SELECTOR => ok(U256::from(10 * USD)),
                IVault::readyToCloseCall::SELECTOR => ok(false),
                IVault::lastPricedCall::SELECTOR => match self.priced {
                    Some(time) => ok((true, U256::ZERO, time)),
                    None => ok((false, U256::ZERO, 0u64)),
                },
                IVault::valuationAtCall::SELECTOR => ok(self.request_valuation()),
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
                    ok(chain::statement::digest(
                        CHAIN,
                        vault(),
                        call.depositor,
                        &self.restricted_text,
                    ))
                }
                IVault::requestAtCall::SELECTOR => {
                    self.tick();
                    let call = IVault::requestAtCall::abi_decode(data).unwrap();
                    let index = usize::try_from(call.requestId).unwrap() - 1;
                    let (owner, _, earliest, done, amount) = self.requests[index];
                    ok(b::Request {
                        owner,
                        kind: b::RequestKind::Deposit,
                        earliest,
                        done,
                        amount,
                        r#ref: U256::ZERO,
                    })
                }
                IVault::requestCountCall::SELECTOR => ok(U256::from(self.requests.len())),
                IVault::openedBlockCall::SELECTOR => ok(0u64),
                _ => panic!("unexpected vault read {selector:?}"),
            }
        }

        fn execute(&mut self, request: &TxRequest) {
            let data = &request.data;
            let selector: [u8; 4] = data[..4].try_into().unwrap();
            let from = request.from;
            match selector {
                IVault::requestDepositCall::SELECTOR => {
                    let call = IVault::requestDepositCall::abi_decode(data).unwrap();
                    *self.usd.get_mut(&from).unwrap() -= call.assets;
                    *self.usd.get_mut(&vault()).unwrap() += call.assets;
                    let earliest = (self.time + 5).div_ceil(10) * 10;
                    self.requests
                        .push((call.receiver, 0, earliest, false, call.assets));
                    self.log(
                        vault(),
                        IVault::Requested {
                            requestId: U256::from(self.requests.len()),
                            owner: call.receiver,
                            kind: b::RequestKind::Deposit,
                            earliest,
                            amount: call.assets,
                        },
                    );
                }
                IVault::settleCall::SELECTOR => self.settle_all(),
                _ => {}
            }
        }
    }

    struct SimReader(Sim);

    impl Reader for SimReader {
        async fn chain_id(&self) -> Result<u64, TxError> {
            Ok(CHAIN)
        }
        async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
            self.0.borrow_mut().call(to, &data)
        }
        async fn native_balance(&self, _who: Address) -> Result<U256, TxError> {
            Ok(U256::from(10u64).pow(U256::from(18u8)))
        }
        async fn estimate_gas(&self, _request: &TxRequest) -> Result<u64, TxError> {
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

    /// How many polls of the open prompt before the wallet answers it (by
    /// declining): far more than the flow's long wait, as a wallet session
    /// gone quiet for the afternoon.
    const WALLET_ANSWERS_AFTER_POLLS: u32 = 2_000;

    impl Signer for SimWallet {
        fn account(&self) -> Option<Address> {
            Some(self.sim.borrow().wallet_account)
        }
        fn name(&self) -> String {
            "MetaMask".to_string()
        }
        async fn chain_id(&self) -> Result<u64, TxError> {
            Ok(self.sim.borrow().wallet_chain)
        }
        async fn sign_statement(&self, statement: &StatementRequest) -> Result<Bytes, TxError> {
            let signature = self.key.sign_hash_sync(&statement.digest()).unwrap();
            Ok(Bytes::from(signature.as_bytes().to_vec()))
        }
        async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
            let selector: [u8; 4] = request.data[..4].try_into().unwrap();
            if selector == IVault::settleCall::SELECTOR {
                // The prompt stays open in the wallet for a very long time,
                // then is declined. Meanwhile the chain is read.
                self.sim.borrow_mut().prompt_opened = true;
                let sim = self.sim.clone();
                std::future::poll_fn(move |cx| {
                    let mut sim = sim.borrow_mut();
                    sim.prompt_polls += 1;
                    if sim.prompt_polls >= WALLET_ANSWERS_AFTER_POLLS {
                        Poll::Ready(())
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                return Err(TxError::Rejected);
            }
            let mut sim = self.sim.borrow_mut();
            assert_eq!(
                request.chain_id, sim.wallet_chain,
                "sent on the wrong chain"
            );
            sim.sent.push(request.clone());
            let hash = keccak256(sim.sent.len().to_be_bytes());
            sim.block += 1;
            let first = sim.logs.len();
            sim.execute(request);
            let logs = sim.logs[first..]
                .iter()
                .map(|(_, log)| log.clone())
                .collect();
            let block = sim.block;
            *sim.nonce.entry(request.from).or_default() += 1;
            sim.receipts.insert(
                hash,
                Receipt {
                    hash,
                    from: Some(request.from),
                    success: true,
                    block,
                    logs,
                },
            );
            Ok(hash)
        }
    }

    /// Reads of the request past the flow's long wait (`give_up_after`
    /// looks at the fast pace), as the keeper settling after it.
    fn past_the_long_wait() -> u32 {
        Pace::FAST.give_up_after + 50
    }

    /// The wallet never answers the second step; the keeper settles the
    /// deposit after the flow's long wait. The deposit is settled, so the
    /// flow ends "Deposited" on its own, as it does when the keeper settles
    /// sooner, rather than wait for the wallet's answer.
    #[test]
    fn a_deposit_the_vault_settles_after_the_long_wait_still_ends_done_while_the_wallet_hasnt_answered()
     {
        let world = World::new();
        world.with(|chain| chain.keeper_settles_at_read = past_the_long_wait());
        let (result, steps) = world.deposit(100);
        assert!(
            steps.contains(&Step::Signing(Prompt::Finish)),
            "the wallet was asked for the second step: {steps:?}"
        );
        assert!(world.sim.borrow().requests[0].3, "the keeper settled it");
        assert!(
            matches!(result, Ok(Outcome::Deposited { .. })),
            "{result:?}; steps {steps:?}"
        );
        // The flow ended because the vault settled it, not because the
        // wallet finally answered: no answer from the wallet is reported.
        assert!(
            !steps.iter().any(|step| matches!(
                step,
                Step::Priced {
                    unsent: Some(TxError::Rejected),
                    ..
                }
            )),
            "the flow stayed on the wallet's prompt after the vault settled the deposit, until the wallet answered: {steps:?}"
        );
        assert!(
            world.sim.borrow().prompt_polls < WALLET_ANSWERS_AFTER_POLLS,
            "the flow waited out the wallet"
        );
    }

    /// "Skip" pressed after the flow's long wait on the wallet must still
    /// let the step go, as it does before it: the button is still shown.
    #[test]
    fn skipping_the_second_step_after_the_long_wait_still_lets_it_go() {
        let world = World::new();
        world.with(|chain| {
            chain.skip_at_read = Some(past_the_long_wait());
            chain.keeper_settles_at_read = past_the_long_wait() + 20;
        });
        let (result, steps) = world.deposit(100);
        assert!(
            matches!(result, Ok(Outcome::Deposited { .. })),
            "{result:?}; steps {steps:?}"
        );
        assert!(
            steps.iter().any(|step| matches!(
                step,
                Step::Priced {
                    unsent: Some(TxError::Cancelled),
                    ..
                }
            )),
            "the skipped step is said as not sent: {steps:?}"
        );
    }
}
