//! Following a send signed in the page. Its own receipt seen is its
//! outcome. The endpoint showing it unknown with its nonce unused for a
//! while is "not taken", and safe to try again: the page signs the retry at
//! that same nonce. Its nonce used with no receipt of its own, over polls in
//! a row, sends the follow to the page's other tries at that nonce: one
//! found mined is said as that other try (gone through, refused, or mined
//! with an outcome that can't be read), never as this action's outcome;
//! none found is "another transaction used this nonce".

use std::cell::Cell;

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::SolValue;
use app::tx::error::TxError;
use app::tx::flow::{self, Cancel, Ctx, Nudge, Pace, Step};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use futures::executor::block_on;

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

const CHAIN: u64 = 8453;

fn account() -> Address {
    Address::repeat_byte(0x42)
}

fn sent() -> B256 {
    B256::repeat_byte(0x11)
}

fn other() -> B256 {
    B256::repeat_byte(0x22)
}

/// What the endpoint shows on one poll.
#[derive(Clone, Copy)]
struct Look {
    pending: Pending,
    /// The account's nonce at "pending", or the lookup failing.
    pending_nonce: Option<u64>,
    mined_nonce: u64,
    /// Whether the receipt reads find the followed send's receipt.
    receipt: bool,
    /// Whether the page's other try at the same nonce has a receipt.
    other_mined: bool,
    /// Whether that other try's receipt shows it reverted.
    other_reverted: bool,
    /// Whether that other try's receipt has no status that can be read.
    other_status_unread: bool,
}

const UNSEEN: Look = Look {
    pending: Pending::Unknown,
    pending_nonce: Some(5),
    mined_nonce: 5,
    receipt: false,
    other_mined: false,
    other_reverted: false,
    other_status_unread: false,
};

/// Used: the nonce mined past 5, the send unknown.
const USED: Look = Look {
    pending_nonce: Some(6),
    mined_nonce: 6,
    ..UNSEEN
};

/// An endpoint showing `look(poll)` on each poll.
struct Chain {
    look: fn(u32) -> Look,
    polls: Cell<u32>,
}

impl Chain {
    fn now(&self) -> Look {
        (self.look)(self.polls.get())
    }
}

impl Reader for Chain {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, _to: Address, _data: Bytes) -> Result<Bytes, TxError> {
        Ok(Bytes::from(U256::from(1_000_000u64).abi_encode()))
    }
    async fn native_balance(&self, _who: Address) -> Result<U256, TxError> {
        Ok(U256::from(1u128 << 64))
    }
    async fn estimate_gas(&self, _request: &TxRequest) -> Result<u64, TxError> {
        Ok(100_000)
    }
    async fn gas_price(&self) -> Result<U256, TxError> {
        Ok(U256::from(1_000_000_000u64))
    }
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        if hash == sent() {
            self.polls.set(self.polls.get() + 1);
        }
        let look = self.now();
        if hash == other() && look.other_status_unread {
            return Ok(None);
        }
        let reverted = hash == other() && look.other_reverted;
        Ok(self.receipt_seen(hash).await?.then(|| Receipt {
            hash,
            from: Some(account()),
            success: !reverted,
            block: 101,
            logs: Vec::new(),
        }))
    }
    async fn receipt_seen(&self, hash: B256) -> Result<bool, TxError> {
        let look = self.now();
        Ok(if hash == other() {
            look.other_mined
        } else {
            look.receipt
        })
    }
    async fn pending(&self, _hash: B256) -> Result<Pending, TxError> {
        Ok(self.now().pending)
    }
    async fn mined_nonce(&self, _who: Address) -> Result<u64, TxError> {
        Ok(self.now().mined_nonce)
    }
    async fn pending_nonce(&self, _who: Address) -> Result<u64, TxError> {
        self.now()
            .pending_nonce
            .ok_or_else(|| TxError::Network("no answer".to_string()))
    }
    async fn chain_time(&self) -> Result<u64, TxError> {
        Ok(1_000)
    }
    async fn block_time(&self, _block: u64) -> Result<u64, TxError> {
        Ok(1_000)
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

/// A wallet whose send is `sent()`: signed in the page at nonce 5 and sent
/// through the flow's endpoint (`in_the_page`), or sent by a browser wallet
/// through its own node. `others`: whether the page signed `other()` at
/// nonce 5 too.
struct Wallet {
    in_the_page: bool,
    others: bool,
}

impl Signer for Wallet {
    fn account(&self) -> Option<Address> {
        Some(account())
    }
    fn name(&self) -> String {
        "a wallet".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn sign_statement(&self, _statement: &StatementRequest) -> Result<Bytes, TxError> {
        unreachable!("no statement in this flow")
    }
    async fn send(&self, _request: &TxRequest) -> Result<B256, TxError> {
        Ok(sent())
    }
    fn sent_nonce(&self, hash: B256) -> Option<u64> {
        (self.in_the_page && hash == sent()).then_some(5)
    }
    fn other_tries(&self, hash: B256) -> Vec<B256> {
        if self.others && hash == sent() {
            vec![other()]
        } else {
            Vec::new()
        }
    }
}

fn ctx<'a, R: Reader>(
    chain: &'a R,
    wallet: &'a Wallet,
    on_step: &'a dyn Fn(Step),
) -> Ctx<'a, R, Wallet, NoWait> {
    Ctx {
        reader: chain,
        signer: wallet,
        clock: &NoWait,
        chain_id: CHAIN,
        vault: Address::repeat_byte(0xaa),
        asset: Address::repeat_byte(0xdd),
        account: account(),
        cancel: Cancel::new(),
        settle_now: Nudge::new(),
        skip_second: Nudge::new(),
        pace: Pace::FAST,
        on_step,
    }
}

/// Follow a send on a chain showing `look`: what the flow ends as, and how
/// many polls it took.
fn follow_with(
    in_the_page: bool,
    others: bool,
    look: fn(u32) -> Look,
) -> (Result<flow::Outcome, TxError>, u32) {
    let chain = Chain {
        look,
        polls: Cell::new(0),
    };
    let wallet = Wallet {
        in_the_page,
        others,
    };
    let on_step = |_: Step| {};
    let result = block_on(flow::collect_held(&ctx(&chain, &wallet, &on_step)));
    (result, chain.polls.get())
}

fn follow(in_the_page: bool, look: fn(u32) -> Look) -> (Result<flow::Outcome, TxError>, u32) {
    follow_with(in_the_page, false, look)
}

#[test]
fn a_send_from_the_page_the_endpoint_keeps_showing_never_taken_ends_as_not_taken_after_a_short_window()
 {
    let (result, polls) = follow(true, |_| UNSEEN);
    assert_eq!(result, Err(TxError::NotTaken { hash: sent() }));
    assert_eq!(polls, Pace::FAST.not_taken_after);
    assert!(!TxError::NotTaken { hash: sent() }.may_have_moved());
}

#[test]
fn a_look_that_fails_starts_the_not_taken_window_again() {
    let (result, polls) = follow(true, |poll| {
        if poll == 2 {
            Look {
                pending_nonce: None,
                ..UNSEEN
            }
        } else {
            UNSEEN
        }
    });
    assert_eq!(result, Err(TxError::NotTaken { hash: sent() }));
    assert_eq!(polls, 2 + Pace::FAST.not_taken_after);
}

#[test]
fn a_send_from_the_page_the_endpoint_holds_pending_is_never_said_not_taken() {
    let (result, polls) = follow(true, |_| Look {
        pending: Pending::Known { nonce: 5 },
        pending_nonce: Some(6),
        ..UNSEEN
    });
    assert_eq!(result, Err(TxError::Stuck { hash: sent() }));
    assert_eq!(polls, Pace::FAST.give_up_after);
}

/// A browser wallet sends through its own node: the flow's endpoint not
/// knowing its transaction proves nothing, so it is never said not taken.
#[test]
fn a_browser_wallets_send_the_endpoint_never_sees_is_never_said_not_taken() {
    let (result, polls) = follow(false, |_| UNSEEN);
    assert_eq!(result, Err(TxError::Dropped { hash: sent() }));
    assert_eq!(polls, Pace::FAST.give_up_after);
}

/// Its nonce used, no receipt of its own, and no other try of the page's
/// there: another transaction used the nonce.
#[test]
fn a_send_from_the_page_whose_nonce_another_transaction_used_is_said_replaced() {
    for others in [false, true] {
        let (result, _) = follow_with(true, others, |_| USED);
        assert_eq!(
            result,
            Err(TxError::Replaced { hash: sent() }),
            "others: {others}"
        );
    }
}

/// The send is mined on poll 2, but that poll's receipt reads land on a
/// node a block behind (no receipt) while its nonce read lands on a fresh
/// one (6); from poll 3 the receipt is there.
#[test]
fn a_page_signed_send_mined_while_one_node_lags_is_never_said_replaced() {
    let (result, _) = follow(true, |poll| match poll {
        0 | 1 => UNSEEN,
        2 => USED,
        _ => Look {
            receipt: true,
            ..USED
        },
    });
    assert!(
        matches!(result, Ok(flow::Outcome::Collected { hash, .. }) if hash == sent()),
        "{result:?}"
    );
}

/// Replaced needs looks in a row that agree: one look showing the nonce
/// unused starts the count again.
#[test]
fn a_contrary_look_starts_the_replaced_count_again() {
    let (result, polls) = follow(true, |poll| match poll {
        3 => Look {
            mined_nonce: 5,
            ..USED
        },
        _ => USED,
    });
    assert_eq!(result, Err(TxError::Replaced { hash: sent() }));
    // Unknown twice from poll 1, so used from poll 2: poll 3 resets, and the
    // verdict waits for `replaced_after` agreeing polls after it.
    assert_eq!(polls, 3 + Pace::FAST.replaced_after);
}

/// The nonce was used by another of the page's tries there, which went
/// through: said as that other try, with its link, never as this action's
/// own success (whatever that try called, the same or another).
#[test]
fn a_send_whose_nonce_another_try_of_the_page_used_is_never_given_that_tries_success() {
    let (result, _) = follow_with(true, true, |_| Look {
        other_mined: true,
        ..USED
    });
    assert_eq!(
        result,
        Err(TxError::OtherTryMined {
            hash: other(),
            refused: Some(false)
        })
    );
    assert!(result.unwrap_err().may_have_moved());
}

/// That other try was refused when mined: said as refused, nothing moved
/// but its fee.
#[test]
fn a_send_whose_nonce_another_try_used_and_the_chain_refused_says_that_try_was_refused() {
    let (result, _) = follow_with(true, true, |_| Look {
        other_mined: true,
        other_reverted: true,
        ..USED
    });
    assert_eq!(
        result,
        Err(TxError::OtherTryMined {
            hash: other(),
            refused: Some(true)
        })
    );
    assert!(!result.unwrap_err().may_have_moved());
}

/// That other try's receipt has no status that can be read: said as mined
/// with its outcome unknown, never as gone through nor refused.
#[test]
fn a_send_whose_nonce_another_try_used_with_no_readable_status_says_neither_outcome() {
    let (result, _) = follow_with(true, true, |_| Look {
        other_mined: true,
        other_status_unread: true,
        ..USED
    });
    assert_eq!(
        result,
        Err(TxError::OtherTryMined {
            hash: other(),
            refused: None
        })
    );
}

/// An endpoint showing the followed send's nonce used by someone (mined and
/// pending 6), the followed send unknown with no receipt, and the other
/// try's receipt reads answering `other(look)` on each look at it.
struct OtherChain {
    other: fn(u32) -> Result<bool, TxError>,
    other_looks: Cell<u32>,
}

impl Reader for OtherChain {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, _to: Address, _data: Bytes) -> Result<Bytes, TxError> {
        Ok(Bytes::from(U256::from(1_000_000u64).abi_encode()))
    }
    async fn native_balance(&self, _who: Address) -> Result<U256, TxError> {
        Ok(U256::from(1u128 << 64))
    }
    async fn estimate_gas(&self, _request: &TxRequest) -> Result<u64, TxError> {
        Ok(100_000)
    }
    async fn gas_price(&self) -> Result<U256, TxError> {
        Ok(U256::from(1_000_000_000u64))
    }
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        Ok(self.receipt_seen(hash).await?.then(|| Receipt {
            hash,
            from: Some(account()),
            success: true,
            block: 101,
            logs: Vec::new(),
        }))
    }
    async fn receipt_seen(&self, hash: B256) -> Result<bool, TxError> {
        if hash == other() {
            let look = self.other_looks.get();
            self.other_looks.set(look + 1);
            (self.other)(look)
        } else {
            Ok(false)
        }
    }
    async fn pending(&self, _hash: B256) -> Result<Pending, TxError> {
        Ok(Pending::Unknown)
    }
    async fn mined_nonce(&self, _who: Address) -> Result<u64, TxError> {
        Ok(6)
    }
    async fn pending_nonce(&self, _who: Address) -> Result<u64, TxError> {
        Ok(6)
    }
    async fn chain_time(&self) -> Result<u64, TxError> {
        Ok(1_000)
    }
    async fn block_time(&self, _block: u64) -> Result<u64, TxError> {
        Ok(1_000)
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

fn follow_other(reads: fn(u32) -> Result<bool, TxError>) -> (Result<flow::Outcome, TxError>, u32) {
    let chain = OtherChain {
        other: reads,
        other_looks: Cell::new(0),
    };
    let wallet = Wallet {
        in_the_page: true,
        others: true,
    };
    let on_step = |_: Step| {};
    let result = block_on(flow::collect_held(&ctx(&chain, &wallet, &on_step)));
    (result, chain.other_looks.get())
}

/// The other try's first receipt read lands on a node a block behind: its
/// receipt is looked for as often as the followed send's, so it is found,
/// never missed for "another transaction used this nonce".
#[test]
fn another_try_that_was_mined_is_never_missed_on_one_lagging_read() {
    let (result, _) = follow_other(|look| Ok(look > 0));
    assert_eq!(
        result,
        Err(TxError::OtherTryMined {
            hash: other(),
            refused: Some(false)
        })
    );
}

/// While the other try's receipt can't be looked up, what used the nonce
/// isn't known: never said replaced. With its receipt found nowhere on
/// every look, it is.
#[test]
fn a_replaced_verdict_waits_until_every_other_try_at_the_nonce_is_looked_up() {
    let (result, _) = follow_other(|_| Err(TxError::Network("no answer".to_string())));
    assert!(
        !matches!(result, Err(TxError::Replaced { .. })),
        "{result:?}"
    );
    let (result, looks) = follow_other(|_| Ok(false));
    assert_eq!(result, Err(TxError::Replaced { hash: sent() }));
    assert_eq!(looks, 2 * Pace::FAST.replaced_after);
}
