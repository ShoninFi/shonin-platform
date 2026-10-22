//! "Settle now" (`flow::settle_request`): the request is read again before
//! the guard (account, chain, cancel), so nothing is awaited between the
//! guard and the wallet's prompt; once the wallet is asked, its answer is
//! always followed, and a request settled meanwhile is said as such: by
//! another transaction only when the vault's record names one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::task::Poll;

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use app::tx::error::TxError;
use app::tx::flow::{self, Cancel, Ctx, Nudge, Outcome, Pace, Prompt, SettledBy, Step};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use chain::bindings::vault::{self as b, IVault};
use futures::executor::block_on;

const CHAIN: u64 = 31337;
const OTHER_CHAIN: u64 = 31338;

fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn account() -> Address {
    Address::repeat_byte(0x11)
}

#[derive(Default)]
struct World {
    /// Reads of `requestAt` so far.
    request_reads: Cell<u32>,
    /// From this `requestAt` read on (counted from 1), the request reads done.
    done_from_read: Cell<Option<u32>>,
    /// At this `requestAt` read, the person moves the wallet to OTHER_CHAIN.
    wallet_moves_at_read: Cell<Option<u32>>,
    wallet_chain: Cell<u64>,
    /// The wallet's chain at each send it was asked for.
    sends_on_chain: RefCell<Vec<u64>>,
    /// Transactions the signer handed to the network.
    broadcast: RefCell<Vec<B256>>,
    /// The signer's send answers only on its second poll (a page-signed send
    /// waiting on the endpoint's reply after it broadcast).
    slow_send: Cell<bool>,
    /// The wallet declines instead of sending, once its prompt has been open
    /// for a look.
    declines: Cell<bool>,
    /// The `requestAt` read that fails (a node that doesn't answer).
    failing_read: Cell<Option<u32>>,
    /// The `Deposited` log names the app's own settle, not the keeper's.
    settled_by_own_send: Cell<bool>,
    /// The `Deposited` log names no transaction.
    record_names_none: Cell<bool>,
}

/// The app's own settle, as the signer sends it.
fn own_settle() -> B256 {
    B256::repeat_byte(0x5e)
}

/// The keeper's settle, named by the `Deposited` log once the request is done.
fn keepers_settle() -> B256 {
    B256::repeat_byte(0x4b)
}

struct R(Rc<World>);
struct W(Rc<World>);
struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {
        // Yield once so a pending send gets polled again.
        let mut yielded = false;
        std::future::poll_fn(|cx| {
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

fn ok<T: alloy_sol_types::SolValue>(value: T) -> Result<Bytes, TxError> {
    Ok(Bytes::from(value.abi_encode()))
}

impl Reader for R {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, _to: Address, data: Bytes) -> Result<Bytes, TxError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        let w = &self.0;
        match selector {
            IVault::requestAtCall::SELECTOR => {
                let n = w.request_reads.get() + 1;
                w.request_reads.set(n);
                if w.wallet_moves_at_read.get() == Some(n) {
                    w.wallet_chain.set(OTHER_CHAIN);
                }
                if w.failing_read.get() == Some(n) {
                    return Err(TxError::Network("no answer".to_string()));
                }
                let done = w.done_from_read.get().is_some_and(|from| n >= from);
                ok(b::Request {
                    owner: account(),
                    kind: b::RequestKind::Deposit,
                    earliest: 1_000,
                    done,
                    amount: U256::from(500_000_000u64),
                    r#ref: U256::ZERO,
                })
            }
            IVault::lastPricedCall::SELECTOR => ok((true, U256::ZERO, 1_000u64)),
            IVault::valuationAtCall::SELECTOR => ok(b::Valuation {
                time: 1_000,
                status: b::ValuationStatus::Priced,
                gross: U256::from(1u8),
                grossExit: U256::from(1u8),
                supply: U256::from(1u8),
                fee: U256::ZERO,
                feeExit: U256::ZERO,
                cash: U256::ZERO,
                postsMissing: 0,
            }),
            IVault::openedBlockCall::SELECTOR => ok(0u64),
            _ => panic!("unexpected read {selector:?}"),
        }
    }
    async fn native_balance(&self, _who: Address) -> Result<U256, TxError> {
        Ok(U256::MAX)
    }
    async fn estimate_gas(&self, _request: &TxRequest) -> Result<u64, TxError> {
        Ok(100_000)
    }
    async fn gas_price(&self) -> Result<U256, TxError> {
        Ok(U256::from(1u8))
    }
    /// Only the keeper's settle is seen mined, sent by the keeper's own
    /// account.
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        Ok((hash == keepers_settle()).then(|| Receipt {
            hash,
            from: Some(Address::repeat_byte(0x4e)),
            success: true,
            block: 1,
            logs: Vec::new(),
        }))
    }
    async fn pending(&self, _hash: B256) -> Result<Pending, TxError> {
        Ok(Pending::Unknown)
    }
    async fn mined_nonce(&self, _who: Address) -> Result<u64, TxError> {
        Ok(0)
    }
    async fn chain_time(&self) -> Result<u64, TxError> {
        Ok(2_000)
    }
    async fn block_time(&self, _block: u64) -> Result<u64, TxError> {
        Ok(2_000)
    }
    async fn logs(
        &self,
        address: Address,
        topic0: B256,
        _topic1: Option<B256>,
        _from_block: u64,
    ) -> Result<Vec<Log>, TxError> {
        if topic0 != b::IVault::Deposited::SIGNATURE_HASH || self.0.done_from_read.get().is_none() {
            return Ok(Vec::new());
        }
        let data = b::IVault::Deposited {
            requestId: U256::from(1u8),
            owner: account(),
            assets: U256::from(500_000_000u64),
            shares: U256::from(7u8),
        }
        .encode_log_data();
        Ok(vec![Log {
            address,
            topics: data.topics().to_vec(),
            data: data.data,
            transaction: if self.0.record_names_none.get() {
                None
            } else if self.0.settled_by_own_send.get() {
                Some(own_settle())
            } else {
                Some(keepers_settle())
            },
        }])
    }
}

impl Signer for W {
    fn account(&self) -> Option<Address> {
        Some(account())
    }
    fn name(&self) -> String {
        "Test wallet".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(self.0.wallet_chain.get())
    }
    async fn sign_statement(&self, _statement: &StatementRequest) -> Result<Bytes, TxError> {
        unreachable!()
    }
    async fn send(&self, _request: &TxRequest) -> Result<B256, TxError> {
        let w = self.0.clone();
        w.sends_on_chain.borrow_mut().push(w.wallet_chain.get());
        let hash = own_settle();
        // Handed to the network at once, as a page-signed send is.
        if !w.declines.get() {
            w.broadcast.borrow_mut().push(hash);
        }
        if w.slow_send.get() {
            let mut polled = false;
            std::future::poll_fn(|cx| {
                if polled {
                    Poll::Ready(())
                } else {
                    polled = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
        }
        if w.declines.get() {
            return Err(TxError::Rejected);
        }
        Ok(hash)
    }
}

fn settle_now(world: &Rc<World>) -> (Result<Outcome, TxError>, Vec<Step>) {
    let steps = RefCell::new(Vec::new());
    let on_step = |step: Step| steps.borrow_mut().push(step);
    let reader = R(world.clone());
    let signer = W(world.clone());
    let ctx = Ctx {
        reader: &reader,
        signer: &signer,
        clock: &NoWait,
        chain_id: CHAIN,
        vault: vault(),
        asset: Address::repeat_byte(0xdd),
        account: account(),
        cancel: Cancel::new(),
        settle_now: Nudge::new(),
        skip_second: Nudge::new(),
        pace: Pace::FAST,
        on_step: &on_step,
    };
    let result = block_on(flow::settle_request(&ctx, U256::from(1u8)));
    (result, steps.into_inner())
}

/// The wallet is moved to another chain during the read of the request
/// just before the wallet would be asked (the third `requestAt` read: the
/// flow's opening read, its done check, then the read before the guard).
/// The guard runs after that read, so the wallet is never asked on the
/// wrong chain.
#[test]
fn settle_now_never_asks_a_wallet_that_moved_chain_before_the_guard() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.wallet_moves_at_read.set(Some(3));
    let (result, steps) = settle_now(&world);
    let sends = world.sends_on_chain.borrow().clone();
    assert!(
        sends.is_empty(),
        "the wallet was asked to send on chain {sends:?}; result {result:?}; steps {steps:?}"
    );
    assert!(
        matches!(result, Err(TxError::WrongNetwork { .. })),
        "{result:?}"
    );
}

/// A page-signed send hands the transaction to the network, then waits on
/// the endpoint's reply; meanwhile the request reads settled (another
/// settle, which the vault's record names, landed). The flow takes the
/// wallet's answer, reports the hash it sent, and ends with the neutral
/// line naming that send: never "nothing was sent". The answer came within a look, so no open prompt is said to
/// be no longer needed.
#[test]
fn settle_now_follows_a_send_made_while_another_settled_and_says_it_only_cost_its_fee() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.slow_send.set(true);
    // Reads 1-3: opening read, done check, the read before the guard.
    // Read 4: the first look while the wallet is asked.
    world.done_from_read.set(Some(4));
    let (result, steps) = settle_now(&world);
    let broadcast = world.broadcast.borrow().clone();
    assert_eq!(broadcast.len(), 1, "the signer sent: {broadcast:?}");
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Step::Sent { hash, .. } if *hash == broadcast[0])),
        "the send was never reported: {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::Signing(Prompt::FinishNoLongerNeeded))),
        "a prompt already answered was said to be still open: {steps:?}"
    );
    match result {
        Ok(Outcome::SentAfterSettled { sent, how, by }) => {
            assert_eq!(sent, broadcast[0]);
            assert_eq!(
                by,
                SettledBy::Unsure,
                "a browser wallet's settle is said neutrally when the record names another transaction"
            );
            assert!(
                matches!(*how, Outcome::Deposited { hash, .. } if hash == keepers_settle()),
                "{how:?}"
            );
        }
        other => panic!("{other:?}; steps {steps:?}"),
    }
}

/// The request reads settled while the wallet is asked, and the wallet
/// declines: nothing was sent, and the request is said settled by the
/// keeper, with how it settled.
#[test]
fn settle_now_declined_after_the_keeper_settled_says_it_was_settled_and_nothing_was_sent() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.slow_send.set(true);
    world.declines.set(true);
    world.done_from_read.set(Some(4));
    let (result, steps) = settle_now(&world);
    assert!(world.broadcast.borrow().is_empty());
    assert!(
        matches!(&result, Ok(Outcome::SettledBefore(how))
            if matches!(**how, Outcome::Deposited { hash, .. } if hash == keepers_settle())),
        "{result:?}; steps {steps:?}"
    );
}

/// The read of the request before the guard fails: "Settle now" carries
/// on, as the automatic second step does, and the guard and the vault
/// decide.
#[test]
fn settle_now_carries_on_when_the_read_before_the_guard_fails() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.failing_read.set(Some(3));
    let (result, steps) = settle_now(&world);
    assert_eq!(
        world.broadcast.borrow().len(),
        1,
        "result {result:?}; steps {steps:?}"
    );
}

/// The app's own settle is handed to the network at once and settles the
/// request (the vault's `Deposited` log names it); the signer's reply comes
/// a poll later, after the request already reads settled. It ends as the
/// app's own settle, and nobody is said to have settled it first.
#[test]
fn settle_now_never_says_another_settled_a_request_its_own_send_settled() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.slow_send.set(true);
    world.settled_by_own_send.set(true);
    world.done_from_read.set(Some(4));
    let (result, steps) = settle_now(&world);
    assert!(
        matches!(&result, Ok(Outcome::Deposited { hash, .. }) if *hash == own_settle()),
        "{result:?}; steps {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::Signing(Prompt::FinishNoLongerNeeded))),
        "the prompt was said no longer needed, though the app's own send settled it: {steps:?}"
    );
}

/// The request reads settled before the wallet's answer, and the vault's
/// record names no transaction: the send is still said as made after it
/// read settled, without saying another transaction settled it.
#[test]
fn settle_now_says_a_send_made_after_it_read_settled_when_the_record_names_no_transaction() {
    let world = Rc::new(World::default());
    world.wallet_chain.set(CHAIN);
    world.slow_send.set(true);
    world.record_names_none.set(true);
    world.done_from_read.set(Some(4));
    let (result, steps) = settle_now(&world);
    assert!(
        matches!(
            &result,
            Ok(Outcome::SentAfterSettled { sent, by: SettledBy::NoneNamed, .. }) if *sent == own_settle()
        ),
        "{result:?}; steps {steps:?}"
    );
}
