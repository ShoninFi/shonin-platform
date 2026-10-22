//! The second step's outcome: a settle signed in the page is followed once
//! signed (before then, "Skip" or the request settled ends it, nothing
//! sent), and a settle of any wallet kind, when the record names another
//! transaction, is said neutrally: never as sped up, nor as another's.
#![allow(dead_code)]
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
    use app::tx::flow::{self, Cancel, Ctx, Nudge, Outcome, Pace, Prompt, SettledBy, Step};
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
        /// Transactions the node holds unmined, at these nonces.
        pending_known: HashMap<B256, u64>,
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
        /// Reads of the request in all; the keeper settles at this one
        /// (counted from 1), before the wallet is asked or not.
        settles_at_any_read: Option<u32>,
        reads: u32,
        /// Every second step the wallet was asked to sign, as handed to it.
        asked: Vec<TxRequest>,
        /// The wallet signs the second step in the page and hands it to the
        /// network at once, answering a few polls later.
        signs_in_page: bool,
        /// Second steps the wallet handed to the network.
        broadcast: Vec<B256>,
        /// Second steps signed in the page, with the nonce each was signed at.
        page_signed: HashMap<B256, u64>,
        /// The transaction the vault's record names for logs written now.
        log_tx: Option<B256>,
        /// The wallet answers the second step with `OWN` after this many
        /// polls; `true`: its settle runs on the chain as it is handed over.
        settle_mode: Option<(u32, bool)>,
        /// "Skip" is pressed at this poll of that prompt.
        skip_at_prompt_poll: Option<u32>,
        /// Looks the flow keeps a prompt said to be no longer needed; set,
        /// the deposit runs on a clock that yields at every look.
        unneeded_shown: Option<u32>,
        in_page_polls: u32,
        /// Once a browser wallet answers the settle, the person switches the
        /// session to a wallet that signs in the page.
        switches_to_page_after_answer: bool,
        /// The page reads the chain for this many polls before it signs the
        /// second step; `None` signs it at once.
        signs_after_polls: Option<u32>,
        /// The page has signed the second step it is sending.
        signed: bool,
        /// The keeper settles the request while the page reads the chain,
        /// and the page's send then fails before it is signed.
        fails_after_keeper: bool,
        keeper_tx: B256,
        keeper_from: Option<Address>,
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
                pending_known: HashMap::new(),
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
                settles_at_any_read: None,
                reads: 0,
                asked: Vec::new(),
                signs_in_page: false,
                broadcast: Vec::new(),
                page_signed: HashMap::new(),
                log_tx: None,
                settle_mode: None,
                skip_at_prompt_poll: None,
                unneeded_shown: None,
                in_page_polls: 5,
                switches_to_page_after_answer: false,
                signs_after_polls: None,
                signed: true,
                fails_after_keeper: false,
                keeper_tx: KEEPER,
                keeper_from: None,
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
            if self.sim.borrow().unneeded_shown.is_some() {
                return self.deposit_with(dollars, &Yielding);
            }
            self.deposit_with(dollars, &NoWait)
        }

        fn deposit_with(
            &self,
            dollars: u64,
            clock: &impl Clock,
        ) -> (Result<Outcome, TxError>, Vec<Step>) {
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
                clock,
                chain_id: CHAIN,
                vault: vault(),
                asset: asset(),
                account: self.account(),
                cancel: Cancel::new(),
                settle_now: Nudge::new(),
                skip_second,
                pace: Pace {
                    unneeded_shown: self
                        .sim
                        .borrow()
                        .unneeded_shown
                        .unwrap_or(Pace::FAST.unneeded_shown),
                    ..Pace::FAST
                },
                on_step: &on_step,
            };
            let result = block_on(flow::deposit(
                &ctx,
                U256::from(dollars * USD),
                "SANCTIONED,US",
            ));
            (result, steps.into_inner())
        }

        /// "Settle now" on request `id`, as the portfolio's button runs it.
        fn settle_now(&self, id: u64) -> (Result<Outcome, TxError>, Vec<Step>) {
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
                settle_now: Nudge::new(),
                skip_second: Nudge::new(),
                pace: Pace::FAST,
                on_step: &on_step,
            };
            let result = block_on(flow::settle_request(&ctx, U256::from(id)));
            (result, steps.into_inner())
        }

        /// A deposit request of the person's, waiting to be settled.
        fn waiting_request(&self) -> &Self {
            let account = self.account();
            self.with(|chain| {
                chain
                    .requests
                    .push((account, 0, 1_010, false, U256::from(500 * USD)));
            })
        }
    }

    struct NoWait;
    impl Clock for NoWait {
        async fn sleep(&self, _millis: u32) {}
    }

    /// A clock whose every wait yields once to the executor, so a future
    /// raced against it is polled at each look.
    struct Yielding;
    impl Clock for Yielding {
        async fn sleep(&self, _millis: u32) {
            let mut yielded = false;
            std::future::poll_fn(move |cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await
        }
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
            self.reads += 1;
            if Some(self.reads) == self.settles_at_any_read {
                self.log_tx = Some(KEEPER);
                self.settle_all();
            }
            if let Some(request) = self.requests.iter().find(|r| !r.3) {
                let earliest = request.2;
                self.time = self.time.max(earliest + 1);
                self.priced = Some(earliest);
            }
            if self.prompt_opened {
                self.reads_since_prompt += 1;
                if self.reads_since_prompt == self.keeper_settles_at_read {
                    let tx = self.keeper_tx;
                    self.log_tx = Some(tx);
                    self.settle_all();
                    if let Some(from) = self.keeper_from {
                        let block = self.block;
                        self.receipts.insert(
                            tx,
                            Receipt {
                                hash: tx,
                                from: Some(from),
                                success: true,
                                block,
                                logs: Vec::new(),
                            },
                        );
                    }
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
                    transaction: self.log_tx,
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
        async fn pending(&self, hash: B256) -> Result<Pending, TxError> {
            Ok(match self.0.borrow().pending_known.get(&hash) {
                Some(nonce) => Pending::Known { nonce: *nonce },
                None => Pending::Unknown,
            })
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
        fn signs_in_page(&self) -> bool {
            self.sim.borrow().signs_in_page
        }
        fn sent_nonce(&self, hash: B256) -> Option<u64> {
            self.sim.borrow().page_signed.get(&hash).copied()
        }
        fn signed_yet(&self) -> bool {
            self.sim.borrow().signed
        }
        async fn sign_statement(&self, statement: &StatementRequest) -> Result<Bytes, TxError> {
            let signature = self.key.sign_hash_sync(&statement.digest()).unwrap();
            Ok(Bytes::from(signature.as_bytes().to_vec()))
        }
        async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
            let selector: [u8; 4] = request.data[..4].try_into().unwrap();
            let mode = self.sim.borrow().settle_mode;
            if selector == IVault::settleCall::SELECTOR
                && let Some((after, runs)) = mode
            {
                {
                    let mut sim = self.sim.borrow_mut();
                    sim.prompt_opened = true;
                    sim.asked.push(request.clone());
                    sim.broadcast.push(OWN);
                    if runs {
                        sim.block += 1;
                        sim.log_tx = Some(OWN);
                        sim.settle_all();
                    }
                }
                let sim = self.sim.clone();
                let mut polls = 0u32;
                std::future::poll_fn(move |cx| {
                    polls += 1;
                    let mut sim = sim.borrow_mut();
                    sim.prompt_polls = polls;
                    if Some(polls) == sim.skip_at_prompt_poll
                        && let Some(skip) = &sim.skip
                    {
                        skip.nudge();
                    }
                    if polls >= after {
                        Poll::Ready(())
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                if self.sim.borrow().switches_to_page_after_answer {
                    self.sim.borrow_mut().signs_in_page = true;
                }
                return Ok(OWN);
            }
            if selector == IVault::settleCall::SELECTOR && self.sim.borrow().signs_in_page {
                // Signed after the page's reads (at once unless the scenario
                // says), then handed to the network at once; the endpoint's
                // reply comes a few polls later.
                let hash = B256::repeat_byte(0x5e);
                if self.sim.borrow().fails_after_keeper {
                    let mut sim = self.sim.borrow_mut();
                    sim.log_tx = Some(KEEPER);
                    sim.settle_all();
                    return Err(TxError::InsufficientGas {
                        need: U256::from(2u8),
                        have: U256::from(1u8),
                    });
                }
                let signs_after = self.sim.borrow().signs_after_polls;
                if let Some(reads) = signs_after {
                    {
                        let mut sim = self.sim.borrow_mut();
                        sim.prompt_opened = true;
                        sim.signed = false;
                    }
                    let mut polls = 0u32;
                    std::future::poll_fn(move |cx| {
                        polls += 1;
                        if polls >= reads {
                            Poll::Ready(())
                        } else {
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                    })
                    .await;
                    self.sim.borrow_mut().signed = true;
                }
                {
                    let mut sim = self.sim.borrow_mut();
                    sim.prompt_opened = true;
                    sim.asked.push(request.clone());
                    sim.broadcast.push(hash);
                    let nonce = sim.nonce.get(&request.from).copied().unwrap_or_default();
                    sim.page_signed.insert(hash, nonce);
                }
                let in_page = self.sim.borrow().in_page_polls;
                let mut polls = 0u32;
                std::future::poll_fn(move |cx| {
                    polls += 1;
                    if polls >= in_page {
                        Poll::Ready(())
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                return Ok(hash);
            }
            if selector == IVault::settleCall::SELECTOR {
                // The prompt stays open in the wallet for a very long time,
                // then is declined. Meanwhile the chain is read.
                self.sim.borrow_mut().prompt_opened = true;
                self.sim.borrow_mut().asked.push(request.clone());
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
            sim.log_tx = Some(hash);
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

    /// The keeper's settle, as the vault's record names it.
    const KEEPER: B256 = B256::repeat_byte(0x4b);
    /// The app's own settle, as the wallet answers it.
    const OWN: B256 = B256::repeat_byte(0x77);

    /// Reads of the request past the flow's long wait (`give_up_after`
    /// looks at the fast pace), as the keeper settling after it.
    fn past_the_long_wait() -> u32 {
        Pace::FAST.give_up_after + 50
    }

    /// A page-signed settle already handed to the network when the request
    /// reads settled (keeper first), its endpoint reply slow, and "Skip"
    /// pressed meanwhile: it has no prompt, so its answer is awaited, it is
    /// never said to be no longer needed, and the outcome names it.
    #[test]
    fn a_page_signed_settle_already_sent_is_named_though_skip_was_pressed() {
        let world = World::new();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.in_page_polls = 8;
            chain.keeper_settles_at_read = 1;
            chain.skip_at_read = Some(1);
            chain.unneeded_shown = Some(100_000);
        });
        let (result, steps) = world.deposit(100);
        let broadcast = world.sim.borrow().broadcast.clone();
        assert_eq!(broadcast.len(), 1, "{steps:?}");
        assert!(
            !steps.contains(&Step::Signing(Prompt::FinishNoLongerNeeded)),
            "a page-signed send was said to be no longer needed: {steps:?}"
        );
        assert!(
            matches!(&result, Ok(Outcome::SentAfterSettled { sent, .. }) if *sent == broadcast[0]),
            "the settle on the network ({:?}) is not named: {result:?}; steps {steps:?}",
            broadcast[0]
        );
    }

    /// Same, without Skip: the short wait for a prompt said to be no longer
    /// needed doesn't apply to it either.
    #[test]
    fn a_page_signed_settle_is_awaited_past_the_unneeded_wait_and_named() {
        let world = World::new();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.in_page_polls = 8;
            chain.keeper_settles_at_read = 1;
            chain.unneeded_shown = Some(2);
        });
        let (result, steps) = world.deposit(100);
        let broadcast = world.sim.borrow().broadcast.clone();
        assert!(
            matches!(&result, Ok(Outcome::SentAfterSettled { sent, .. }) if *sent == broadcast[0]),
            "the settle on the network ({:?}) is not named: {result:?}; steps {steps:?}",
            broadcast[0]
        );
    }

    /// Another settle of the person's own (an earlier nonce, e.g. "Settle
    /// now" for another request in another tab: `settle(256)` settles every
    /// priced request).
    const OTHER_OWN: B256 = B256::repeat_byte(0x79);

    /// A named case and how it sets the chain up.
    type Case = (&'static str, Box<dyn Fn(&mut Chain)>);

    /// The ways the record can name a settle other than the one a browser
    /// wallet answered with: the person's own sped up in the wallet, another
    /// own settle while the app's mined too, another own settle while the
    /// app's is still in the node's pool, another account's, and one whose
    /// sender can't be read.
    fn named_by_another_hash(account: Address) -> Vec<Case> {
        let mined = move |chain: &mut Chain| {
            chain.receipts.insert(
                OWN,
                Receipt {
                    hash: OWN,
                    from: Some(account),
                    success: true,
                    block: 10_000,
                    logs: Vec::new(),
                },
            );
        };
        vec![
            (
                "own sped up",
                Box::new(move |chain: &mut Chain| {
                    chain.keeper_tx = SPED_UP;
                    chain.keeper_from = Some(account);
                }),
            ),
            (
                "another own, ours mined",
                Box::new(move |chain: &mut Chain| {
                    chain.keeper_tx = OTHER_OWN;
                    chain.keeper_from = Some(account);
                    mined(chain);
                }),
            ),
            (
                "another own, ours pending",
                Box::new(move |chain: &mut Chain| {
                    chain.keeper_tx = OTHER_OWN;
                    chain.keeper_from = Some(account);
                    chain.pending_known.insert(OWN, 7);
                }),
            ),
            (
                "another account's",
                Box::new(|chain: &mut Chain| {
                    chain.keeper_from = Some(Address::repeat_byte(0x99));
                }),
            ),
            (
                "sender unread",
                Box::new(|chain: &mut Chain| {
                    chain.keeper_from = None;
                }),
            ),
        ]
    }

    /// A browser wallet's settle, when the record names another
    /// transaction, is never said as sped up (a plain outcome) nor as
    /// another's: whoever sent the named one, the outcome is the neutral
    /// one, naming the settle the wallet answered with.
    #[test]
    fn a_browser_wallets_settle_is_never_said_sped_up_or_anothers_unless_the_record_names_it() {
        let account = World::new().account();
        for (case, setup) in named_by_another_hash(account) {
            let world = World::new();
            world.with(|chain| {
                chain.settle_mode = Some((1, false));
                chain.keeper_settles_at_read = 1;
                setup(chain);
            });
            let (result, steps) = world.deposit(100);
            assert!(
                matches!(&result, Ok(Outcome::SentAfterSettled { sent, by: SettledBy::Unsure, .. }) if *sent == OWN),
                "deposit, {case}: {result:?}; steps {steps:?}"
            );
        }
        for (case, setup) in named_by_another_hash(account) {
            let world = World::new();
            world.waiting_request();
            world.with(|chain| {
                chain.settle_mode = Some((1, false));
                chain.keeper_settles_at_read = 1;
                setup(chain);
            });
            let (result, steps) = world.settle_now(1);
            assert!(
                matches!(&result, Ok(Outcome::SentAfterSettled { sent, by: SettledBy::Unsure, .. }) if *sent == OWN),
                "settle now, {case}: {result:?}; steps {steps:?}"
            );
        }
    }

    /// A receipt's `from` in lowercase or checksummed reads as the same
    /// account.
    #[test]
    fn a_receipts_sender_reads_as_the_same_account_in_any_letter_case() {
        let account = World::new().account();
        let lower = format!("{account:?}").to_lowercase();
        let checksummed = account.to_checksum(None);
        for from in [lower, checksummed] {
            let receipt = app::wallet::rpc::parse_receipt(&serde_json::json!({
                "transactionHash": format!("{:?}", B256::repeat_byte(1)),
                "blockHash": format!("{:?}", B256::repeat_byte(2)),
                "blockNumber": "0x10",
                "status": "0x1",
                "from": from,
                "logs": [],
            }))
            .unwrap()
            .unwrap();
            assert_eq!(receipt.from, Some(account));
        }
    }

    /// A page-signed "Settle now" sent while another account's settle
    /// settled the request is named, and said neutrally: never as having
    /// cost a fee as fact, nor as another's.
    #[test]
    fn a_page_signed_settle_the_record_does_not_name_is_never_said_to_have_cost_a_fee_or_to_be_anothers()
     {
        let world = World::new();
        world.waiting_request();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.in_page_polls = 8;
            chain.keeper_settles_at_read = 1;
            chain.keeper_from = Some(Address::repeat_byte(0x99));
        });
        let (result, steps) = world.settle_now(1);
        assert!(
            matches!(&result, Ok(Outcome::SentAfterSettled { sent, by: SettledBy::Unsure, .. }) if *sent == B256::repeat_byte(0x5e)),
            "{result:?}; steps {steps:?}"
        );
    }

    /// A page-signed second step not yet signed when the keeper settles the
    /// request ends there: nothing is signed or sent, and the deposit is
    /// said as the keeper settled it.
    #[test]
    fn a_page_signed_second_step_not_yet_signed_ends_when_the_request_reads_settled() {
        let world = World::new();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.signs_after_polls = Some(u32::MAX);
            chain.keeper_settles_at_read = 1;
        });
        let (result, steps) = world.deposit(100);
        assert!(world.sim.borrow().broadcast.is_empty(), "{steps:?}");
        assert!(
            matches!(&result, Ok(Outcome::Deposited { hash, .. }) if *hash != B256::ZERO),
            "{result:?}; steps {steps:?}"
        );
    }

    /// "Skip" before a page-signed second step is signed lets it go: nothing
    /// is signed or sent, and the keeper settles the request.
    #[test]
    fn skip_before_a_page_signed_second_step_is_signed_sends_nothing() {
        let world = World::new();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.signs_after_polls = Some(u32::MAX);
            chain.skip_at_read = Some(1);
            chain.keeper_settles_at_read = 3;
        });
        let (result, steps) = world.deposit(100);
        assert!(world.sim.borrow().broadcast.is_empty(), "{steps:?}");
        assert!(
            matches!(&result, Ok(Outcome::Deposited { .. })),
            "{result:?}; steps {steps:?}"
        );
    }

    /// "Settle now" signed in the page, the request settled by the keeper
    /// before it is signed: nothing is sent, and it was settled before.
    #[test]
    fn settle_now_signed_in_the_page_ends_settled_before_when_settled_before_signing() {
        let world = World::new();
        world.waiting_request();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.signs_after_polls = Some(u32::MAX);
            chain.keeper_settles_at_read = 1;
        });
        let (result, steps) = world.settle_now(1);
        assert!(world.sim.borrow().broadcast.is_empty(), "{steps:?}");
        assert!(
            matches!(&result, Ok(Outcome::SettledBefore(_))),
            "{result:?}; steps {steps:?}"
        );
    }

    /// Once signed, a page-signed second step is awaited and named even
    /// though the request read settled while the page was still reading.
    #[test]
    fn a_page_signed_second_step_once_signed_is_awaited_and_named() {
        let world = World::new();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.signs_after_polls = Some(3);
            chain.in_page_polls = 8;
            chain.keeper_settles_at_read = 4;
            chain.unneeded_shown = Some(100_000);
        });
        let (result, steps) = world.deposit(100);
        let broadcast = world.sim.borrow().broadcast.clone();
        assert_eq!(broadcast.len(), 1, "{steps:?}");
        assert!(
            matches!(&result, Ok(Outcome::SentAfterSettled { sent, .. }) if *sent == broadcast[0]),
            "{result:?}; steps {steps:?}"
        );
    }

    /// "Settle now" signed in the page that fails before anything is sent,
    /// the keeper having settled the request meanwhile: it was settled
    /// before, not an error.
    #[test]
    fn settle_now_signed_in_the_page_failing_unsent_after_the_keeper_settled_ends_settled_before() {
        let world = World::new();
        world.waiting_request();
        world.with(|chain| {
            chain.signs_in_page = true;
            chain.fails_after_keeper = true;
        });
        let (result, steps) = world.settle_now(1);
        assert!(world.sim.borrow().broadcast.is_empty(), "{steps:?}");
        assert!(
            matches!(&result, Ok(Outcome::SettledBefore(_))),
            "{result:?}; steps {steps:?}"
        );
    }

    const SPED_UP: B256 = B256::repeat_byte(0x78);

    /// A browser wallet's settle, sped up by the wallet and named by the
    /// record, is still said neutrally when the session has switched to a
    /// wallet that signs in the page before the outcome is read: whether the
    /// page signed a settle is told by its hash, not by the wallet active
    /// now.
    #[test]
    fn a_browser_wallets_settle_is_never_said_anothers_after_the_session_switches_to_a_page_wallet()
    {
        let account = World::new().account();
        let setup = |chain: &mut Chain| {
            chain.settle_mode = Some((1, false));
            chain.keeper_settles_at_read = 1;
            chain.keeper_tx = SPED_UP;
            chain.keeper_from = Some(account);
            chain.switches_to_page_after_answer = true;
        };
        let world = World::new();
        world.with(setup);
        let (deposit, steps) = world.deposit(100);
        assert!(
            matches!(&deposit, Ok(Outcome::SentAfterSettled { sent, by: SettledBy::Unsure, .. }) if *sent == OWN),
            "deposit: {deposit:?}; steps {steps:?}"
        );
        let world = World::new();
        world.waiting_request();
        world.with(setup);
        let (now, steps) = world.settle_now(1);
        assert!(
            matches!(&now, Ok(Outcome::SentAfterSettled { sent, by: SettledBy::Unsure, .. }) if *sent == OWN),
            "settle now: {now:?}; steps {steps:?}"
        );
    }
}
