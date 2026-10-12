//! The depositor flows on the EVM chains, through their public ports, over a
//! small simulated vault chain and wallet: what happens after the money has
//! moved, the fallback on a deposit, a claim on a closed vault, and the
//! statement's signature in the form the vault takes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use app::tx::error::{Context, TxError};
use app::tx::flow::{self, Cancel, Ctx, Fallback, Nudge, Outcome, Pace, Step};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use app::tx::read::ClaimState;
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

const CHAIN: u64 = 31337;
const OTHER_CHAIN: u64 = 31338;
const USD: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;

fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn asset() -> Address {
    Address::repeat_byte(0xdd)
}

/// How the wallet behaves at one particular prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wallet {
    Approves,
    /// Declines every `settle` (the second step) and nothing else.
    DeclinesSettle,
    /// Is moved to another chain by the person as soon as the request
    /// transaction has been sent (the countdown is running).
    SwitchesChainAfterRequest,
    /// Signs the statement with the parity byte as 0 or 1, not 27 or 28.
    SignsWithParityByte,
}

struct Chain {
    time: u64,
    block: u64,
    state: b::VaultState,
    restricted: Vec<B256>,
    restricted_text: String,
    statement_of: HashMap<Address, B256>,
    usd: HashMap<Address, U256>,
    allowance: HashMap<(Address, Address), U256>,
    shares: HashMap<Address, U256>,
    /// (owner, kind, earliest, done, amount); kind 0 deposit, 1 redeem.
    requests: Vec<(Address, u8, u64, bool, U256)>,
    claims: Vec<b::Claim>,
    /// The time of the newest priced valuation; the keeper prices the first
    /// waiting request's slot when it is polled, and never settles.
    priced: Option<u64>,
    receipts: HashMap<B256, Receipt>,
    logs: Vec<(u64, Log)>,
    nonce: HashMap<Address, u64>,
    sent: Vec<TxRequest>,
    wallet_chain: u64,
    wallet_account: Address,
    wallet: Wallet,
}

type Sim = Rc<RefCell<Chain>>;

struct World {
    sim: Sim,
    key: PrivateKeySigner,
}

impl World {
    fn new(wallet: Wallet) -> Self {
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
        // The statement is already recorded for the list as it stands, so
        // a deposit asks for the request straight away.
        let mut statement_of = HashMap::new();
        statement_of.insert(account, keccak256(restricted.abi_encode()));
        let chain = Chain {
            time: 1_000,
            block: 100,
            state: b::VaultState::Open,
            restricted,
            restricted_text: "SANCTIONED,US".to_string(),
            statement_of,
            usd,
            allowance,
            shares,
            requests: Vec::new(),
            claims: Vec::new(),
            priced: None,
            receipts: HashMap::new(),
            logs: Vec::new(),
            nonce: HashMap::new(),
            sent: Vec::new(),
            wallet_chain: CHAIN,
            wallet_account: account,
            wallet,
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
            settle_now: Nudge::new(),
            pace: Pace::FAST,
            on_step: &on_step,
        };
        let result = block_on(flow(&ctx));
        (result, steps.into_inner())
    }

    fn sent_selectors(&self) -> Vec<[u8; 4]> {
        self.sim
            .borrow()
            .sent
            .iter()
            .map(|tx| tx.data[..4].try_into().unwrap())
            .collect()
    }
}

struct NoWait;
impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

fn ok<T: SolValue>(value: T) -> Result<Bytes, TxError> {
    Ok(Bytes::from(value.abi_encode()))
}

/// Whether a 65-byte signature is one the vault's own check (an `ecrecover`
/// behind OpenZeppelin's `ECDSA`) accepts: `v` must be 27 or 28, and must
/// recover to the depositor.
fn vault_accepts(signature: &[u8], digest: B256, depositor: Address) -> bool {
    signature.len() == 65
        && (signature[64] == 27 || signature[64] == 28)
        && flow::recovers_to(signature, digest, depositor)
}

impl Chain {
    fn request_valuation(&mut self) -> b::Valuation {
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

    /// The keeper, on each look at a request: prices its slot, settles
    /// nothing (someone else may).
    fn tick(&mut self) {
        if let Some(request) = self.requests.iter().find(|r| !r.3) {
            let earliest = request.2;
            self.time = self.time.max(earliest + 1);
            self.priced = Some(earliest);
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
            },
        ));
    }

    fn call(&mut self, to: Address, data: &[u8]) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if to == asset() {
            return match selector {
                chain::bindings::erc20::IERC20::balanceOfCall::SELECTOR => {
                    let call =
                        chain::bindings::erc20::IERC20::balanceOfCall::abi_decode(data).unwrap();
                    ok(self.usd.get(&call.account).copied().unwrap_or_default())
                }
                chain::bindings::erc20::IERC20::allowanceCall::SELECTOR => {
                    let call =
                        chain::bindings::erc20::IERC20::allowanceCall::abi_decode(data).unwrap();
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
            _ => panic!("unexpected vault read {selector:?}"),
        }
    }

    /// What the vault would do with a transaction: `Ok` with its logs, or
    /// the revert it would raise. Used by the gas estimate and the send.
    fn simulate(&self, request: &TxRequest) -> Result<(), &'static str> {
        let data = &request.data;
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        if request.to == asset() {
            return Ok(());
        }
        let hash = keccak256(self.restricted.abi_encode());
        match selector {
            IVault::requestDepositCall::SELECTOR => {
                let call = IVault::requestDepositCall::abi_decode(data).unwrap();
                if !call.statement.is_empty() {
                    let digest = chain::statement::digest(
                        CHAIN,
                        vault(),
                        call.receiver,
                        &self.restricted_text,
                    );
                    if !vault_accepts(&call.statement, digest, call.receiver) {
                        return Err("BadSignature");
                    }
                } else if self.statement_of.get(&call.receiver) != Some(&hash) {
                    return Err("NoStatement");
                }
                Ok(())
            }
            IVault::takeProRataCall::SELECTOR => {
                let call = IVault::takeProRataCall::abi_decode(data).unwrap();
                let (owner, _, earliest, done, _) =
                    self.requests[usize::try_from(call.requestId).unwrap() - 1];
                if done || owner != request.from {
                    return Err("NotAllowed");
                }
                if self.time < earliest + 86_400 {
                    return Err("NotYet");
                }
                Ok(())
            }
            IVault::settleClaimCall::SELECTOR => {
                if matches!(self.state, b::VaultState::Closed) {
                    return Err("WrongState");
                }
                Ok(())
            }
            IVault::claimClosedCall::SELECTOR => {
                if !matches!(self.state, b::VaultState::Closed) {
                    return Err("WrongState");
                }
                let call = IVault::claimClosedCall::abi_decode(data).unwrap();
                let claim = &self.claims[usize::try_from(call.claimId).unwrap() - 1];
                if !matches!(
                    claim.state,
                    b::ClaimState::Pending | b::ClaimState::Waiting | b::ClaimState::Queued
                ) {
                    return Err("NotAllowed");
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn execute(&mut self, request: &TxRequest) {
        let data = &request.data;
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        let from = request.from;
        match selector {
            IVault::requestDepositCall::SELECTOR => {
                let call = IVault::requestDepositCall::abi_decode(data).unwrap();
                if !call.statement.is_empty() {
                    let hash = keccak256(self.restricted.abi_encode());
                    self.statement_of.insert(call.receiver, hash);
                }
                *self.usd.get_mut(&from).unwrap() -= call.assets;
                *self.usd.get_mut(&vault()).unwrap() += call.assets;
                self.request(call.receiver, 0, call.assets);
                if self.wallet == Wallet::SwitchesChainAfterRequest {
                    self.wallet_chain = OTHER_CHAIN;
                }
            }
            IVault::requestRedeemCall::SELECTOR => {
                let call = IVault::requestRedeemCall::abi_decode(data).unwrap();
                *self.shares.get_mut(&from).unwrap() -= call.shares;
                self.request(call.receiver, 1, call.shares);
                if self.wallet == Wallet::SwitchesChainAfterRequest {
                    self.wallet_chain = OTHER_CHAIN;
                }
            }
            IVault::settleCall::SELECTOR => {
                // Every waiting request settles at a dollar a share.
                let waiting: Vec<usize> = (0..self.requests.len())
                    .filter(|&i| !self.requests[i].3)
                    .collect();
                for i in waiting {
                    self.requests[i].3 = true;
                    let (owner, kind, _, _, amount) = self.requests[i];
                    let id = U256::from(i + 1);
                    if kind == 0 {
                        self.log(
                            vault(),
                            IVault::Deposited {
                                requestId: id,
                                owner,
                                assets: amount,
                                shares: amount * U256::from(1_000_000_000_000u64),
                            },
                        );
                    } else {
                        self.log(
                            vault(),
                            IVault::Withdrawn {
                                requestId: id,
                                owner,
                                shares: amount,
                                assets: amount / U256::from(1_000_000_000_000u64),
                                feeShares: U256::ZERO,
                            },
                        );
                    }
                }
            }
            IVault::takeProRataCall::SELECTOR => {
                let call = IVault::takeProRataCall::abi_decode(data).unwrap();
                let index = usize::try_from(call.requestId).unwrap() - 1;
                let (owner, kind, _, _, amount) = self.requests[index];
                self.requests[index].3 = true;
                if kind == 0 {
                    // A deposit nothing priced goes back to whoever paid it:
                    // the vault records a refund, not a fallback.
                    *self.usd.get_mut(&vault()).unwrap() -= amount;
                    *self.usd.get_mut(&owner).unwrap() += amount;
                    self.log(
                        vault(),
                        IVault::Refunded {
                            requestId: call.requestId,
                            to: owner,
                            assets: amount,
                        },
                    );
                } else {
                    self.log(
                        vault(),
                        IVault::ProRataTaken {
                            owner,
                            shares: amount,
                            assets: U256::from(400_000u64),
                            units: U256::from(7u8),
                        },
                    );
                }
            }
            IVault::claimClosedCall::SELECTOR => {
                let call = IVault::claimClosedCall::abi_decode(data).unwrap();
                let claim = &mut self.claims[usize::try_from(call.claimId).unwrap() - 1];
                claim.state = b::ClaimState::Paid;
                claim.paid = U256::from(900u64 * USD);
                let paid = claim.paid;
                self.log(
                    vault(),
                    IVault::ClaimPaid {
                        claimId: call.claimId,
                        assets: paid,
                    },
                );
            }
            _ => {}
        }
    }

    fn request(&mut self, owner: Address, kind: u8, amount: U256) {
        let earliest = (self.time + 5).div_ceil(10) * 10;
        self.requests.push((owner, kind, earliest, false, amount));
        self.log(
            vault(),
            IVault::Requested {
                requestId: U256::from(self.requests.len()),
                owner,
                kind: if kind == 0 {
                    b::RequestKind::Deposit
                } else {
                    b::RequestKind::Redeem
                },
                earliest,
                amount,
            },
        );
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
    async fn estimate_gas(&self, request: &TxRequest) -> Result<u64, TxError> {
        match self.0.borrow().simulate(request) {
            Ok(()) => Ok(100_000),
            Err(name) => Err(TxError::WouldRevert {
                reason: app::tx::error::Revert::Vault(name),
            }),
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
        let mut bytes = signature.as_bytes().to_vec();
        if self.sim.borrow().wallet == Wallet::SignsWithParityByte {
            // Some wallets return the recovery id itself (0 or 1) rather
            // than the Ethereum convention of 27 or 28.
            bytes[64] -= 27;
        }
        Ok(Bytes::from(bytes))
    }
    async fn send(&self, request: &TxRequest) -> Result<B256, TxError> {
        let mut sim = self.sim.borrow_mut();
        let selector: [u8; 4] = request.data[..4].try_into().unwrap();
        if sim.wallet == Wallet::DeclinesSettle && selector == IVault::settleCall::SELECTOR {
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
        let success = sim.simulate(request).is_ok();
        if success {
            sim.execute(request);
        }
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
                success,
                block,
                logs,
            },
        );
        Ok(hash)
    }
}

fn words(error: &TxError) -> String {
    let words = error.words(&Context {
        wallet: "MetaMask",
        chain: "Base",
        wallet_chain: Some("Hyperliquid"),
        symbol: "USDC",
        decimals: 6,
        native: "ETH",
    });
    format!("{} {}", words.title, words.text)
}

/// A flow whose request landed on the chain has moved money, whatever
/// happens to its second step: the words shown must never say nothing was
/// sent, and "Try again" must not invite the same request again.
fn assert_not_reported_as_never_sent(result: &Result<Outcome, TxError>, steps: &[Step]) {
    let requested = steps.iter().any(|step| matches!(step, Step::Requested(_)));
    assert!(requested, "the request was sent and reported: {steps:?}");
    if let Err(error) = result {
        let said = words(error);
        assert!(
            error.after_send(),
            "the request is on the chain, but the error is one the page treats as before any send: {error:?}"
        );
        assert!(
            error.may_have_moved(),
            "the page would offer to send it again: {error:?}"
        );
        assert!(
            !said.contains("Nothing was sent")
                && !said.contains("No money moved")
                && !said.contains("no request was made"),
            "the request is on the chain and the money is in escrow, but the page says: {said:?}"
        );
    }
}

#[test]
fn a_deposit_whose_second_step_is_declined_is_never_said_to_have_sent_nothing() {
    // The person deposits $100. The wallet approves the request; the keeper
    // prices it but nobody settles, so the app asks the wallet to settle;
    // the person declines that prompt (or presses Cancel in the panel).
    let world = World::new(Wallet::DeclinesSettle);
    let before = world.sim.borrow().usd[&world.account()];
    let (result, steps) = world.run(|ctx| Box::pin(flow::deposit(ctx, U256::from(100 * USD))));

    // The deposit request is on the chain and its dollars are in escrow.
    let sim = world.sim.borrow();
    assert_eq!(sim.requests.len(), 1);
    assert!(
        !sim.requests[0].3,
        "the request stands, waiting to be settled"
    );
    assert_eq!(sim.usd[&world.account()], before - U256::from(100 * USD));
    drop(sim);

    assert_not_reported_as_never_sent(&result, &steps);
    // The page shows it priced and waiting, with the second step offered
    // again ("Settle now") and why the app's own wasn't sent.
    assert!(steps.iter().any(|step| matches!(
        step,
        Step::Priced {
            unsent: Some(TxError::Rejected),
            ..
        }
    )));
}

#[test]
fn a_withdrawal_whose_wallet_moves_chain_during_the_countdown_is_never_said_to_have_sent_nothing() {
    // The person withdraws 1,000 shares. The request lands; while the
    // countdown runs they switch their wallet to another chain. The second
    // step can't be sent from there, but the request stands and the keeper
    // settles it: the page must not call this a withdrawal that never left.
    let world = World::new(Wallet::SwitchesChainAfterRequest);
    let (result, steps) =
        world.run(|ctx| Box::pin(flow::withdraw(ctx, U256::from(1_000u128 * WAD))));
    let sim = world.sim.borrow();
    assert_eq!(sim.requests.len(), 1, "the request was sent");
    assert_eq!(
        sim.shares[&world.account()],
        U256::from(4_000u128 * WAD),
        "the shares are pending in the vault"
    );
    drop(sim);
    assert_not_reported_as_never_sent(&result, &steps);
}

#[test]
fn the_fallback_on_a_deposit_ends_as_the_refund_the_vault_records() {
    // A deposit of $250 nothing priced for a day. The portfolio offers the
    // fallback; the contract sends the deposit back and records a refund.
    let world = World::new(Wallet::Approves);
    let account = world.account();
    world.with(|chain| {
        chain
            .requests
            .push((account, 0, 1_000, false, U256::from(250 * USD)));
        chain.time = 1_000 + 86_400 + 1;
    });
    let before = world.sim.borrow().usd[&account];
    let (result, _) =
        world.run(|ctx| Box::pin(flow::take_cash(ctx, Fallback::Request(U256::from(1u8)))));
    let sim = world.sim.borrow();
    assert!(
        sim.requests[0].3,
        "the request was taken through the fallback"
    );
    assert_eq!(
        sim.usd[&account],
        before + U256::from(250 * USD),
        "the deposit came back"
    );
    drop(sim);
    assert!(
        matches!(result, Ok(Outcome::Refunded { assets, .. }) if assets == U256::from(250 * USD)),
        "the deposit was sent back, so the flow should say so, not fail: {result:?}"
    );
}

#[test]
fn a_claim_queued_before_the_vault_closed_is_collected_from_its_cash() {
    // A claim was queued (the cash covered it) and then the vault closed
    // before the valuation that would have paid it. The closed vault pays
    // its claims through `claimClosed`, which takes a queued claim; the
    // claim page should offer that, and the flow should send it.
    let world = World::new(Wallet::Approves);
    let account = world.account();
    world.with(|chain| {
        chain.state = b::VaultState::Closed;
        chain.claims.push(b::Claim {
            owner: account,
            shares: U256::from(1_000u128 * WAD),
            bookedAt: 500,
            lastAttemptAt: 900,
            state: b::ClaimState::Queued,
            paid: U256::ZERO,
        });
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::settle_claim(ctx, U256::from(1u8))));
    assert!(
        world
            .sent_selectors()
            .contains(&IVault::claimClosedCall::SELECTOR),
        "the closed vault's door for a claim should have been used; sent: {:?}, result {result:?}",
        world.sent_selectors()
    );
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
}

#[test]
fn a_statement_signed_with_a_parity_byte_of_zero_or_one_is_sent_as_the_vault_takes_it() {
    // The person has not signed yet. Their wallet returns a valid signature
    // whose last byte is the recovery id (0 or 1), as some wallets do. The
    // vault's check accepts only 27 or 28; the app should send the Ethereum
    // form, not refuse a good signature as a revert.
    let world = World::new(Wallet::SignsWithParityByte);
    let account = world.account();
    world.with(|chain| {
        chain.statement_of.clear();
        chain.wallet_account = account;
    });
    let (result, _) = world.run(|ctx| Box::pin(flow::deposit(ctx, U256::from(100 * USD))));
    let sent = world.sim.borrow().sent.clone();
    let deposit = sent
        .iter()
        .find(|tx| tx.data[..4] == IVault::requestDepositCall::SELECTOR)
        .unwrap_or_else(|| panic!("the deposit was never sent: {result:?}"));
    let call = IVault::requestDepositCall::abi_decode(&deposit.data).unwrap();
    assert_eq!(call.statement.len(), 65);
    assert!(
        call.statement[64] == 27 || call.statement[64] == 28,
        "the statement's parity byte was sent as {}, which the vault refuses",
        call.statement[64]
    );
    assert!(result.is_ok(), "{result:?}");
}
