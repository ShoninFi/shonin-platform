//! Sends signed in the page (the recovery-phrase wallet) through a public
//! endpoint that may be a pool of nodes behind one URL: each request may be
//! answered by a different node, one of which may lag a block or not have
//! heard of a pooled transaction yet, so no single look proves a send was
//! not taken.
//!
//! The mined-nonce rule: per chain and account, storage keeps the highest
//! nonce the page signed at (H) and the sends signed there. While the
//! endpoint's mined count ("latest") is at or below H, every send signs at
//! H, with higher fees, taking the place of the others there; once "latest"
//! is past H, the send signs at "latest", the new H. A sealed receipt on
//! the endpoint for a send at H shows H used, whatever the count: the send
//! signs at H + 1. "Pending" never
//! chooses a nonce. The only release of an H not shown mined is a reset
//! chain: both counts below H and every send at H unknown for the whole
//! not-taken window. Storage that can't be read or written sends nothing,
//! and sends for one chain and account take turns. A send whose gas at
//! the fee cap it signs exceeds the coin held is never signed: at H, a
//! raise the coin can't pay gives way to the highest cap it can, never
//! below the endpoint's fees now.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use alloy_primitives::{Address, B256, Bytes, keccak256};
use app::tx::error::TxError;
use app::tx::flow::Pace;
use app::tx::port::{Clock, TxRequest};
use app::wallet::phrase::{Fees, LocalKey};
use app::wallet::rpc::{Bounded, Fate, RpcError, RpcReader, Transport, not_ok};
use app::wallet::session::{Highest, Sent, SignedSend, SignedSends, raised, sign_and_send};
use app::wallet::signed::{SendStorage, Unavailable, storage_key};
use futures::executor::block_on;
use serde_json::{Value, json};

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

// The send itself -------------------------------------------------------------

/// A node whose `eth_sendRawTransaction` ends in `send`, and which is asked
/// nothing else.
struct Node {
    send: Result<Value, RpcError>,
}

impl Transport for Node {
    async fn request(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        match method {
            "eth_sendRawTransaction" => self.send.clone(),
            other => panic!("unexpected request {other}"),
        }
    }
}

/// Whatever an endpoint answers a raw send that reached it, the words never
/// make it a refusal: the transaction may sit in some node's pool, so it is
/// followed by its hash.
#[test]
fn any_error_to_a_raw_send_that_reached_the_endpoint_is_followed_whatever_its_words() {
    let raw = Bytes::from(vec![0x02, 0xab, 0xcd]);
    for error in [
        RpcError::from_json(
            &json!({"code": -32000, "message": "nonce too low: next nonce 6, tx nonce 5"}),
        ),
        RpcError::from_json(&json!({"code": -32000, "message": "txpool is full"})),
        RpcError::from_json(&json!({"code": -32000, "message": "upstream request timeout"})),
        not_ok(
            429,
            Some(
                &json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32016, "message": "over rate limit"}}),
            ),
        ),
        not_ok(429, None),
        RpcError::status(502),
        RpcError::lost("the chain's endpoint didn't answer"),
    ] {
        let reader = RpcReader {
            transport: Node {
                send: Err(error.clone()),
            },
        };
        assert_eq!(
            block_on(reader.send_raw(&raw)),
            Ok(keccak256(&raw)),
            "{error:?}"
        );
    }
}

// The pool --------------------------------------------------------------------

/// One URL, many nodes, as the page asks it: the account's nonce at
/// "pending" and at "latest" as set, the hashes some node holds pending or
/// has a receipt for, the account's balance, and every raw send kept. The raw sends numbered in
/// `failing_sends` (from 1) end in `send_error`.
struct Pool {
    pending: Cell<u64>,
    latest: Cell<u64>,
    base_fee: Cell<u64>,
    known: RefCell<Vec<B256>>,
    landed: RefCell<Vec<B256>>,
    tip_fails: Cell<bool>,
    balance: u128,
    send_error: Option<RpcError>,
    failing_sends: Vec<usize>,
    sends: RefCell<Vec<Bytes>>,
    /// How many times "pending" was read, and the count it answers on one
    /// of those reads instead.
    pending_reads: Cell<u32>,
    pending_on_read: Cell<Option<(u32, u64)>>,
}

impl Pool {
    fn at(nonce: u64) -> Self {
        Self {
            pending: Cell::new(nonce),
            latest: Cell::new(nonce),
            base_fee: Cell::new(0x10),
            known: RefCell::default(),
            landed: RefCell::default(),
            tip_fails: Cell::new(false),
            balance: u128::MAX,
            send_error: None,
            failing_sends: Vec::new(),
            sends: RefCell::default(),
            pending_reads: Cell::new(0),
            pending_on_read: Cell::new(None),
        }
    }

    fn failing_first(error: RpcError, nonce: u64) -> Self {
        Self {
            send_error: Some(error),
            failing_sends: vec![1],
            ..Self::at(nonce)
        }
    }

    /// The account's counts, mined and pending alike.
    fn counts(&self, nonce: u64) {
        self.pending.set(nonce);
        self.latest.set(nonce);
    }
}

impl Transport for Pool {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "eth_getTransactionCount" => {
                let nonce = if params[1] == "pending" {
                    let read = self.pending_reads.get() + 1;
                    self.pending_reads.set(read);
                    match self.pending_on_read.get() {
                        Some((at, nonce)) if at == read => nonce,
                        _ => self.pending.get(),
                    }
                } else {
                    self.latest.get()
                };
                Ok(json!(format!("{nonce:#x}")))
            }
            "eth_maxPriorityFeePerGas" if self.tip_fails.get() => {
                Err(RpcError::lost("the chain's endpoint didn't answer"))
            }
            "eth_maxPriorityFeePerGas" => Ok(json!("0x1")),
            "eth_getBlockByNumber" => Ok(json!({
                "baseFeePerGas": format!("{:#x}", self.base_fee.get()),
                "timestamp": "0x1",
            })),
            "eth_getBalance" => Ok(json!(format!("{:#x}", self.balance))),
            // The OP stack's GasPriceOracle: no L1 data fee here.
            "eth_call" => Ok(json!(format!("{:#066x}", 0))),
            "eth_sendRawTransaction" => {
                let raw: Bytes = params[0].as_str().unwrap().parse().unwrap();
                self.sends.borrow_mut().push(raw);
                let count = self.sends.borrow().len();
                match &self.send_error {
                    Some(error) if self.failing_sends.contains(&count) => Err(error.clone()),
                    _ => Ok(Value::Null),
                }
            }
            "eth_getTransactionByHash" => {
                let hash: B256 = params[0].as_str().unwrap().parse().unwrap();
                Ok(if self.known.borrow().contains(&hash) {
                    json!({"hash": format!("{hash:?}"), "nonce": "0xc", "blockNumber": null})
                } else {
                    Value::Null
                })
            }
            "eth_getTransactionReceipt" => {
                let hash: B256 = params[0].as_str().unwrap().parse().unwrap();
                Ok(if self.landed.borrow().contains(&hash) {
                    json!({
                        "transactionHash": format!("{hash:?}"),
                        "status": "0x1",
                        "blockNumber": "0x65",
                        "blockHash": format!("0x{}", "33".repeat(32)),
                        "logs": [],
                    })
                } else {
                    Value::Null
                })
            }
            other => panic!("unexpected request {other}"),
        }
    }
}

const PHRASE: &str = "test test test test test test test test test test test junk";
const OTHER_PHRASE: &str =
    "legal winner thank year wave sausage worth useful legal winner thank yellow";

fn key() -> LocalKey {
    LocalKey::from_phrase(PHRASE).unwrap()
}

fn request(key: &LocalKey, chain_id: u64, data: &[u8]) -> TxRequest {
    TxRequest {
        chain_id,
        from: key.address(),
        to: Address::repeat_byte(0xaa),
        data: Bytes::from(data.to_vec()),
        gas: Some(100_000),
    }
}

fn send<T: Transport>(
    reader: &RpcReader<T>,
    signed: &SignedSends,
    key: &LocalKey,
    request: &TxRequest,
) -> Result<Sent, TxError> {
    block_on(sign_and_send(reader, key, request, signed, &NoWait))
}

fn pool(nonce: u64) -> RpcReader<Pool> {
    RpcReader {
        transport: Pool::at(nonce),
    }
}

/// The record kept for `key` on chain 8453: its nonce and hashes.
fn kept(signed: &SignedSends, key: &LocalKey) -> Option<(u64, Vec<B256>)> {
    signed
        .get(8453, key.address())
        .unwrap()
        .map(|highest| (highest.nonce, highest.hashes()))
}

/// The fees (cap, tip) recorded for each send at the highest nonce.
fn recorded_fees(signed: &SignedSends, key: &LocalKey) -> Vec<(u128, u128)> {
    signed
        .get(8453, key.address())
        .unwrap()
        .unwrap()
        .sends
        .iter()
        .map(|send| match send.fees {
            Fees::Eip1559 {
                max_fee,
                max_priority_fee,
            } => (max_fee, max_priority_fee),
            Fees::Legacy { gas_price } => (gas_price, gas_price),
        })
        .collect()
}

// Choosing the nonce ----------------------------------------------------------

/// A tip that can't be read is never taken as no tip: the send ends before
/// anything is signed, in plain words, recording nothing.
#[test]
fn a_send_whose_tip_cant_be_read_is_never_signed_and_records_nothing() {
    let key = key();
    let pool = pool(5);
    pool.transport.tip_fails.set(true);
    let signed = SignedSends::default();
    let result = send(&pool, &signed, &key, &request(&key, 8453, &[0x01]));
    assert!(
        matches!(&result, Err(TxError::Network(why)) if why.contains("nothing was sent")),
        "{result:?}"
    );
    assert!(pool.transport.sends.borrow().is_empty());
    assert_eq!(kept(&signed, &key), None);
}

/// The first send reaches node A, which pools it at nonce 5, and the answer
/// is lost; nothing is mined. The retry's "pending" read lands on node A
/// (6): it still signs at 5, with higher fees, so the two take one place
/// and at most one lands. The retry knows the first as another try there.
#[test]
fn a_retry_after_a_lost_answer_signs_at_the_same_nonce_whatever_a_node_counts_pending() {
    for first_send in [
        RpcError::status(502),
        RpcError::lost("the chain's endpoint didn't answer"),
        RpcError::from_json(&json!({"code": -32000, "message": "upstream request timeout"})),
    ] {
        let key = key();
        let deposit = request(&key, 8453, &[0x01, 0x02]);
        let pool = RpcReader {
            transport: Pool::failing_first(first_send.clone(), 5),
        };
        let signed = SignedSends::default();
        let first = send(&pool, &signed, &key, &deposit).unwrap();
        assert_eq!(first.nonce, 5, "{first_send:?}");
        assert!(first.others.is_empty());
        pool.transport.pending.set(6);
        let retry = send(&pool, &signed, &key, &deposit).unwrap();
        assert_eq!(
            retry.nonce, 5,
            "{first_send:?}: the retry signed past the first"
        );
        assert_eq!(retry.others, vec![first.hash]);
        assert_ne!(retry.hash, first.hash, "a replacement carries other fees");
        assert_eq!(kept(&signed, &key), Some((5, vec![first.hash, retry.hash])));
    }
}

/// While the chain's mined count doesn't pass the highest nonce, every send,
/// whatever it calls (a deposit, a collect, a retry), signs at that nonce:
/// two sends never stand at different nonces from an unmined one. Each
/// offers fees at least 12.5% above the highest recorded there, so nodes
/// take it as a replacement.
#[test]
fn sends_while_the_highest_nonce_is_unmined_all_sign_at_it_with_rising_fees() {
    let key = key();
    let pool = pool(5);
    let signed = SignedSends::default();
    let calls: [&[u8]; 4] = [&[1], &[2], &[1], &[3]];
    for (index, call) in calls.into_iter().enumerate() {
        // A node pools each one, so "pending" runs ahead; "latest" stays.
        pool.transport.pending.set(6 + index as u64);
        let sent = send(&pool, &signed, &key, &request(&key, 8453, call)).unwrap();
        assert_eq!(sent.nonce, 5, "send {index}");
        assert_eq!(sent.others.len(), index);
    }
    let fees = recorded_fees(&signed, &key);
    for pair in fees.windows(2) {
        let ((cap, tip), (next_cap, next_tip)) = (pair[0], pair[1]);
        assert!(next_cap * 8 >= cap * 9, "{fees:?}");
        assert!(next_tip * 8 >= tip * 9, "{fees:?}");
    }
}

/// A send at the highest nonce never offers less than the endpoint asks now,
/// however far the base fee has risen past the raised fees.
#[test]
fn a_send_at_the_highest_nonce_never_offers_less_than_the_endpoints_fees_now() {
    let key = key();
    let pool = pool(5);
    let signed = SignedSends::default();
    send(&pool, &signed, &key, &request(&key, 8453, &[1])).unwrap();
    pool.transport.base_fee.set(0x1000);
    send(&pool, &signed, &key, &request(&key, 8453, &[1])).unwrap();
    let fees = recorded_fees(&signed, &key);
    assert_eq!(fees[1].0, 2 * 0x1000 + 1, "{fees:?}");
    assert!(fees[1].1 >= 1);
}

/// A deposit's approval signs at 5 and is mined: the chain's mined count
/// shows 6, so the deposit signs at 6, which becomes the record alone. A
/// node whose mined count still shows 5 doesn't hold it back while the
/// endpoint holds a sealed receipt for the approval: H is used, and the
/// deposit signs at 6. Only with neither (the count at 5, no receipt seen,
/// while another node counts the approval pending) does the deposit sign at
/// 5: never past an H the chain doesn't show used, and "pending" never
/// chooses.
#[test]
fn an_approval_at_the_highest_nonce_once_mined_lets_the_deposit_sign_at_the_next() {
    let key = key();
    let approve = TxRequest {
        to: Address::repeat_byte(0xdd),
        ..request(&key, 8453, &[0x09])
    };
    let deposit = request(&key, 8453, &[0x01]);
    for (count_shows_it, receipt_shows_it) in [(true, true), (false, true), (false, false)] {
        let pool = pool(5);
        let signed = SignedSends::default();
        let approval = send(&pool, &signed, &key, &approve).unwrap();
        assert_eq!(approval.nonce, 5);
        if receipt_shows_it {
            pool.transport.landed.borrow_mut().push(approval.hash);
        }
        pool.transport.pending.set(6);
        if count_shows_it {
            pool.transport.latest.set(6);
        }
        let sent = send(&pool, &signed, &key, &deposit).unwrap();
        if count_shows_it || receipt_shows_it {
            assert_eq!(
                sent.nonce, 6,
                "count {count_shows_it}, receipt {receipt_shows_it}"
            );
            assert!(sent.others.is_empty());
            assert_eq!(kept(&signed, &key), Some((6, vec![sent.hash])));
        } else {
            assert_eq!(sent.nonce, 5);
            assert_eq!(sent.others, vec![approval.hash]);
            assert_eq!(
                kept(&signed, &key),
                Some((5, vec![approval.hash, sent.hash]))
            );
        }
    }
}

/// The coin held is checked against the gas at the fee cap the page signs
/// (twice the base fee plus the tip), not the endpoint's gas price: an
/// account that holds gas x gas price but not gas x fee cap is told its
/// coin is short, and nothing is signed, sent or recorded, on the first try
/// or any after. At H, a raise the coin can't pay gives way to the highest
/// cap it can, never below the endpoint's fees now.
#[test]
fn a_page_signed_send_never_signs_a_fee_the_account_cant_pay() {
    let key = key();
    // Gas 100_000 at a cap of 2 x 16 + 1 = 33 wei needs 3_300_000; the
    // endpoint's gas price (17 wei) would need only 1_700_000.
    let pool = RpcReader {
        transport: Pool {
            balance: 2_000_000,
            send_error: Some(RpcError::from_json(&json!({
                "code": -32000,
                "message": "insufficient funds for gas * price + value",
            }))),
            failing_sends: vec![1, 2],
            ..Pool::at(5)
        },
    };
    let signed = SignedSends::default();
    let deposit = request(&key, 8453, &[0x01]);
    for _ in 0..2 {
        assert!(matches!(
            send(&pool, &signed, &key, &deposit),
            Err(TxError::InsufficientGas { need, have })
                if need == alloy_primitives::U256::from(3_300_000u64)
                    && have == alloy_primitives::U256::from(2_000_000u64)
        ));
    }
    assert!(pool.transport.sends.borrow().is_empty());
    assert_eq!(kept(&signed, &key), None);
    // At H the raise is 33 x 1.125 rounded up, 38 (3_800_000); 3_500_000
    // held pays a cap of 35, which is at least today's 33.
    let pool = RpcReader {
        transport: Pool {
            balance: 3_500_000,
            ..Pool::at(5)
        },
    };
    let first = send(&pool, &signed, &key, &deposit).unwrap();
    let second = send(&pool, &signed, &key, &deposit).unwrap();
    assert_eq!(second.nonce, 5);
    assert_eq!(recorded_fees(&signed, &key), vec![(33, 1), (35, 2)]);
    // Short even of today's 33 (3_300_000): nothing is signed or recorded,
    // and the amount said needed is the cost at today's fees.
    let pool = RpcReader {
        transport: Pool {
            balance: 3_200_000,
            ..Pool::at(5)
        },
    };
    assert!(matches!(
        send(&pool, &signed, &key, &deposit),
        Err(TxError::InsufficientGas { need, have })
            if need == alloy_primitives::U256::from(3_300_000u64)
                && have == alloy_primitives::U256::from(3_200_000u64)
    ));
    assert!(pool.transport.sends.borrow().is_empty());
    assert_eq!(
        kept(&signed, &key),
        Some((5, vec![first.hash, second.hash]))
    );
}

/// A send at H signed in a fee spike and then dropped by every node leaves
/// a raise over it that the coin can't pay once the spike passes; "latest"
/// equals H, so no release frees it. The retry still signs at H, at the
/// highest cap the coin pays (at least the endpoint's fees now), on the
/// first try: a node still holding the earlier send refuses it as
/// underpriced, and at most one send at H can land.
#[test]
fn a_raise_past_the_coin_held_at_the_highest_nonce_signs_there_at_the_highest_fee_that_fits() {
    let key = key();
    let deposit = request(&key, 8453, &[0x01]);
    // A spike: base 4096, cap 2 x 4096 + 1 = 8193, costing 819_300_000.
    let pool = RpcReader {
        transport: Pool {
            balance: 900_000_000,
            ..Pool::at(5)
        },
    };
    pool.transport.base_fee.set(4096);
    let signed = SignedSends::default();
    let first = send(&pool, &signed, &key, &deposit).unwrap();
    assert_eq!(first.nonce, 5);
    // The spike passes (cap 33); the raise over 8193 is 9218, costing
    // 921_800_000, past the 900_000_000 held, which pays a cap of 9000.
    pool.transport.base_fee.set(16);
    let retry = send(&pool, &signed, &key, &deposit).unwrap();
    assert_eq!(retry.nonce, 5);
    assert_eq!(retry.others, vec![first.hash]);
    assert_eq!(recorded_fees(&signed, &key), vec![(8193, 1), (9000, 2)]);
    assert_eq!(pool.transport.sends.borrow().len(), 2);
}

/// Short of the network fee, the words say how much it needs and how much
/// the wallet holds, in the chain's coin; a wallet that only said its funds
/// were short (no amounts) gets the plain words.
#[test]
fn a_send_short_of_its_network_fee_says_the_amount_needed_and_the_amount_held() {
    let context = app::tx::error::Context {
        wallet: "your recovery phrase",
        keeps_activity: false,
        chain: "Base",
        wallet_chain: Some("Base"),
        symbol: "USDC",
        decimals: 6,
        native: "ETH",
    };
    let words = TxError::InsufficientGas {
        need: alloy_primitives::U256::from(921_800_000_000_000u64),
        have: alloy_primitives::U256::from(900_000_000_000_000u64),
    }
    .words(&context);
    assert!(words.text.contains("0.0009218 ETH"), "{}", words.text);
    assert!(words.text.contains("holds 0.0009 ETH"), "{}", words.text);
    let unknown = TxError::InsufficientGas {
        need: alloy_primitives::U256::ZERO,
        have: alloy_primitives::U256::ZERO,
    }
    .words(&context);
    assert!(!unknown.text.contains("0.0"), "{}", unknown.text);
    assert!(unknown.text.contains("ETH on Base"), "{}", unknown.text);
}

/// A send recorded at 12 whose mined count reads 0 on a node far behind,
/// while a look shows the nonce within reach ("pending" at or past it, or a
/// send there known), signs at 12: never released.
#[test]
fn a_highest_nonce_any_look_shows_within_reach_is_never_released() {
    let key = key();
    let deposit = request(&key, 31337, &[0x01]);
    for known in [false, true] {
        let pool = pool(12);
        let signed = SignedSends::default();
        let first = send(&pool, &signed, &key, &deposit).unwrap();
        if known {
            pool.transport.counts(0);
            pool.transport.known.borrow_mut().push(first.hash);
        } else {
            pool.transport.pending.set(13);
            pool.transport.latest.set(0);
        }
        let sent = send(&pool, &signed, &key, &deposit).unwrap();
        assert_eq!(sent.nonce, 12, "known: {known}");
    }
}

/// The chain was reset (a dev chain restarted): both of the account's
/// counts fall below the highest nonce and every send there stays unknown
/// to the endpoint for the whole not-taken window. The record is cleared,
/// and the send signs at the chain's count. A look on the window's last
/// poll showing the nonce within reach keeps the record.
#[test]
fn a_reset_chain_releases_the_highest_nonce_only_after_the_whole_window() {
    let key = key();
    let deposit = request(&key, 31337, &[0x01]);
    let window = Pace::LIVE.not_taken_after;
    for reach_on_last in [false, true] {
        let pool = pool(12);
        let signed = SignedSends::default();
        send(&pool, &signed, &key, &deposit).unwrap();
        pool.transport.counts(0);
        let reads = pool.transport.pending_reads.get();
        if reach_on_last {
            pool.transport
                .pending_on_read
                .set(Some((reads + window, 12)));
        }
        let sent = send(&pool, &signed, &key, &deposit).unwrap();
        if reach_on_last {
            assert_eq!(sent.nonce, 12);
        } else {
            assert_eq!(pool.transport.pending_reads.get() - reads, window);
            assert_eq!(sent.nonce, 0);
            assert!(sent.others.is_empty());
            let highest = signed.get(31337, key.address()).unwrap().unwrap();
            assert_eq!((highest.nonce, highest.hashes()), (0, vec![sent.hash]));
        }
    }
}

/// The record is per chain and account: one kept at 9 for one account on
/// one chain (a node counts it pending) moves neither another account nor
/// another chain off the chain's mined count.
#[test]
fn the_highest_nonce_is_kept_per_chain_and_account() {
    let key = key();
    let other = LocalKey::from_phrase(OTHER_PHRASE).unwrap();
    let pool = pool(5);
    pool.transport.pending.set(10);
    let signed = SignedSends::default();
    signed
        .set(
            8453,
            key.address(),
            Some(&Highest {
                nonce: 9,
                sends: vec![SignedSend {
                    hash: B256::repeat_byte(9),
                    to: Address::repeat_byte(0xaa),
                    data: Bytes::new(),
                    fees: Fees::Legacy { gas_price: 1 },
                }],
            }),
        )
        .unwrap();
    let sent = send(&pool, &signed, &other, &request(&other, 8453, &[0x01])).unwrap();
    assert_eq!(sent.nonce, 5, "another account");
    let sent = send(&pool, &signed, &key, &request(&key, 1, &[0x01])).unwrap();
    assert_eq!(sent.nonce, 5, "another chain");
    let sent = send(&pool, &signed, &key, &request(&key, 8453, &[0x01])).unwrap();
    assert_eq!(sent.nonce, 9, "the recorded chain and account");
}

// Sends that never left -------------------------------------------------------

fn not_made() -> RpcError {
    RpcError::unreachable("couldn't encode the request")
}

/// A send that couldn't be made never left: it is a refusal, and its hash
/// is taken off the record. Alone at a new highest nonce, the record before
/// it stands again: none, or the mined one it would have replaced.
#[test]
fn a_send_that_never_left_is_taken_off_the_record_and_the_record_before_it_stands_again() {
    let key = key();
    // Nothing recorded before.
    let pool = RpcReader {
        transport: Pool::failing_first(not_made(), 5),
    };
    let signed = SignedSends::default();
    assert!(send(&pool, &signed, &key, &request(&key, 8453, &[0x01])).is_err());
    assert_eq!(kept(&signed, &key), None);
    // A mined record at 3 before a send at 5.
    let pool = RpcReader {
        transport: Pool {
            failing_sends: vec![2],
            ..Pool::failing_first(not_made(), 3)
        },
    };
    let signed = SignedSends::default();
    let mined = send(&pool, &signed, &key, &request(&key, 8453, &[0x01])).unwrap();
    pool.transport.counts(5);
    assert!(send(&pool, &signed, &key, &request(&key, 8453, &[0x02])).is_err());
    assert_eq!(kept(&signed, &key), Some((3, vec![mined.hash])));
    // The next send signs at the chain's count all the same.
    let sent = send(&pool, &signed, &key, &request(&key, 8453, &[0x02])).unwrap();
    assert_eq!(sent.nonce, 5);
}

/// A retry at the highest nonce that couldn't be made is taken off alone:
/// the first send there, which left, stays recorded, and the next try
/// signs at its nonce again.
#[test]
fn a_retry_that_never_left_leaves_the_sends_that_did_recorded() {
    let key = key();
    let pool = RpcReader {
        transport: Pool {
            failing_sends: vec![2],
            ..Pool::failing_first(not_made(), 5)
        },
    };
    let signed = SignedSends::default();
    let deposit = request(&key, 8453, &[0x01]);
    let first = send(&pool, &signed, &key, &deposit).unwrap();
    assert!(send(&pool, &signed, &key, &deposit).is_err());
    assert_eq!(kept(&signed, &key), Some((5, vec![first.hash])));
    let again = send(&pool, &signed, &key, &deposit).unwrap();
    assert_eq!(again.nonce, 5);
    assert_eq!(again.others, vec![first.hash]);
}

/// An endpoint whose raw sends never answer, as `pool` otherwise.
struct Hung {
    pool: Pool,
}

impl Transport for Hung {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if method == "eth_sendRawTransaction" {
            self.pool.request(method, params.clone()).await?;
            futures::future::pending::<()>().await;
        }
        self.pool.request(method, params).await
    }
}

/// Each request made while a send holds its turn ends in bounded time: one
/// that never answers is said lost, never refused. A raw send that never
/// answers is followed by its hash, and stays recorded.
#[test]
fn a_request_that_never_answers_ends_in_time_as_lost_and_a_send_so_ended_stays_recorded() {
    let hung = Hung { pool: Pool::at(5) };
    let bounded = Bounded {
        inner: &hung,
        clock: &NoWait,
        millis: 20_000,
    };
    let error = block_on(bounded.request("eth_sendRawTransaction", json!(["0x02"]))).unwrap_err();
    assert_eq!(error.fate, Fate::Lost);
    let key = key();
    let hung = Hung { pool: Pool::at(5) };
    let reader = RpcReader {
        transport: Bounded {
            inner: &hung,
            clock: &NoWait,
            millis: 20_000,
        },
    };
    let signed = SignedSends::default();
    let sent = send(&reader, &signed, &key, &request(&key, 8453, &[0x01])).unwrap();
    assert_eq!(sent.nonce, 5);
    assert_eq!(kept(&signed, &key), Some((5, vec![sent.hash])));
}

// Storage, reloads and turns --------------------------------------------------

/// Storage shared by every page that opens it (a reload, another tab).
#[derive(Clone, Default)]
struct Shared {
    items: Rc<RefCell<BTreeMap<String, String>>>,
}

impl SendStorage for Shared {
    fn get(&self, key: &str) -> Result<Option<String>, Unavailable> {
        Ok(self.items.borrow().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), Unavailable> {
        self.items
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn remove(&self, key: &str) -> Result<(), Unavailable> {
        self.items.borrow_mut().remove(key);
        Ok(())
    }
}

/// Deposit A is signed at 5 and its answer is lost. The person reloads (or
/// opens another tab) and tries again: the new page reads the record from
/// storage, so the retry signs at 5 too and at most one of them lands.
#[test]
fn a_retry_after_a_reload_or_in_another_tab_signs_at_the_same_nonce() {
    let key = key();
    let pool = RpcReader {
        transport: Pool::failing_first(RpcError::lost("the chain's endpoint didn't answer"), 5),
    };
    let storage = Shared::default();
    let tab = SignedSends::new(storage.clone());
    let first = send(&pool, &tab, &key, &request(&key, 8453, &[1])).unwrap();
    assert_eq!(first.nonce, 5);
    let text = storage
        .items
        .borrow()
        .get(&storage_key(8453, key.address()))
        .cloned()
        .unwrap();
    assert!(text.contains(&format!("{:?}", first.hash)), "{text}");
    pool.transport.pending.set(6);
    let reloaded = SignedSends::new(storage);
    assert_eq!(kept(&reloaded, &key), Some((5, vec![first.hash])));
    let retry = send(&pool, &reloaded, &key, &request(&key, 8453, &[1])).unwrap();
    assert_eq!(
        retry.nonce, 5,
        "after a reload the retry signed past the first send"
    );
    assert_eq!(retry.others, vec![first.hash]);
}

/// Storage that fails reads, or reads and fails writes (a private window,
/// a blocked origin, a full quota).
#[derive(Clone, Default)]
struct Failing {
    reads_fail: bool,
}

impl SendStorage for Failing {
    fn get(&self, _key: &str) -> Result<Option<String>, Unavailable> {
        if self.reads_fail {
            Err(Unavailable)
        } else {
            Ok(None)
        }
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), Unavailable> {
        Err(Unavailable)
    }
    fn remove(&self, _key: &str) -> Result<(), Unavailable> {
        Err(Unavailable)
    }
}

/// Storage that can't be read, or reads and won't take a write: the send
/// is never made, nothing reaches the endpoint, and the person is told this
/// browser's storage is needed.
#[test]
fn a_page_signed_send_without_storage_that_reads_and_writes_sends_nothing() {
    let key = key();
    for reads_fail in [true, false] {
        let pool = pool(5);
        let signed = SignedSends::new(Failing { reads_fail });
        let result = send(&pool, &signed, &key, &request(&key, 8453, &[1]));
        assert_eq!(
            result,
            Err(TxError::StorageNeeded),
            "reads fail: {reads_fail}"
        );
        assert!(
            pool.transport.sends.borrow().is_empty(),
            "sent without a written record"
        );
    }
}

struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// The pool, reached after yielding `delay` times before the first request
/// and once before each.
struct Slow<'a> {
    pool: &'a Pool,
    delay: Cell<u32>,
}

impl Transport for Slow<'_> {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        while self.delay.get() > 0 {
            self.delay.set(self.delay.get() - 1);
            YieldNow(false).await;
        }
        YieldNow(false).await;
        self.pool.request(method, params).await
    }
}

/// Two flows in one tab (a deposit in vault X, a collect in vault Y, on one
/// chain and account) start together, Y's reads slower. They take turns:
/// X signs at 5 and records it; Y, reading the record after X's turn, signs
/// at 5 too, in X's place, with X among its other tries. Never two nonces.
#[test]
fn concurrent_sends_in_one_tab_take_turns_and_sign_at_the_same_unmined_nonce() {
    let key = key();
    let pool = Pool::at(5);
    let x = RpcReader {
        transport: Slow {
            pool: &pool,
            delay: Cell::new(0),
        },
    };
    let y = RpcReader {
        transport: Slow {
            pool: &pool,
            delay: Cell::new(40),
        },
    };
    let signed = SignedSends::default();
    let deposit_x = request(&key, 8453, &[1]);
    let collect_y = TxRequest {
        to: Address::repeat_byte(0xbb),
        ..request(&key, 8453, &[2])
    };
    let (rx, ry) = block_on(async {
        futures::join!(
            sign_and_send(&x, &key, &deposit_x, &signed, &NoWait),
            sign_and_send(&y, &key, &collect_y, &signed, &NoWait),
        )
    });
    let (sent_x, sent_y) = (rx.unwrap(), ry.unwrap());
    assert_eq!((sent_x.nonce, sent_y.nonce), (5, 5));
    assert_eq!(sent_y.others, vec![sent_x.hash]);
    assert_eq!(
        kept(&signed, &key),
        Some((5, vec![sent_x.hash, sent_y.hash]))
    );
}

/// Nodes take a legacy send's gas price as both its fee cap and its tip, so
/// a send at H after a legacy one there offers a cap and a tip each at least
/// 12.5% above that price; after a send with a fee cap and a tip, a legacy
/// send's one price clears the higher of the two, the cap.
#[test]
fn a_send_at_the_highest_nonce_clears_the_fees_of_a_send_there_of_the_other_kind() {
    let at = |fees: Fees| Highest {
        nonce: 5,
        sends: vec![SignedSend {
            hash: B256::repeat_byte(1),
            to: Address::repeat_byte(0xaa),
            data: Bytes::new(),
            fees,
        }],
    };
    assert_eq!(
        raised(
            Fees::Eip1559 {
                max_fee: 33,
                max_priority_fee: 1,
            },
            &at(Fees::Legacy { gas_price: 40 }),
        ),
        Fees::Eip1559 {
            max_fee: 45,
            max_priority_fee: 45,
        }
    );
    assert_eq!(
        raised(
            Fees::Legacy { gas_price: 17 },
            &at(Fees::Eip1559 {
                max_fee: 40,
                max_priority_fee: 2,
            }),
        ),
        Fees::Legacy { gas_price: 45 }
    );
}
