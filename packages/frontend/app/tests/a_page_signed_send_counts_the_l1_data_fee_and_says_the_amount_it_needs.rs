//! A send signed in the page counts the L1 data fee where the chain charges
//! one, both when it fits its fees to the coin held and when it refuses for
//! want of coin, and the amount it says is needed is the amount it needs.

use std::cell::{Cell, RefCell};

use alloy_consensus::{Transaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Address, Bytes, U256};
use app::tx::error::TxError;
use app::tx::port::{Clock, TxRequest};
use app::wallet::phrase::LocalKey;
use app::wallet::rpc::{RpcError, RpcReader, Transport};
use app::wallet::session::{SignedSends, sign_and_send};
use futures::executor::block_on;
use serde_json::{Value, json};

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

/// The OP stack's GasPriceOracle.
const ORACLE: &str = "0x420000000000000000000000000000000000000f";

/// An OP-stack endpoint (Base, Base Sepolia): its pool refuses a send whose
/// gas x fee cap plus the L1 data fee is more than the balance, with the
/// node's words (an answered error, so the page follows it by its hash).
struct OpNode {
    latest: u64,
    base_fee: Cell<u64>,
    balance: Cell<u128>,
    l1_fee: u128,
    /// The oracle answers with an error.
    oracle_fails: bool,
    /// Every call made to the oracle.
    oracle_calls: Cell<u32>,
    accepted: RefCell<Vec<Bytes>>,
    refused: RefCell<Vec<Bytes>>,
}

impl OpNode {
    fn new(base_fee: u64, balance: u128, l1_fee: u128) -> Self {
        Self {
            latest: 5,
            base_fee: Cell::new(base_fee),
            balance: Cell::new(balance),
            l1_fee,
            oracle_fails: false,
            oracle_calls: Cell::new(0),
            accepted: RefCell::default(),
            refused: RefCell::default(),
        }
    }
}

impl Transport for OpNode {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "eth_getTransactionCount" => Ok(json!(format!("{:#x}", self.latest))),
            "eth_maxPriorityFeePerGas" => Ok(json!("0x1")),
            "eth_getBlockByNumber" => Ok(json!({
                "baseFeePerGas": format!("{:#x}", self.base_fee.get()),
                "timestamp": "0x1",
            })),
            "eth_getBalance" => Ok(json!(format!("{:#x}", self.balance.get()))),
            "eth_call" => {
                let to = params[0]["to"].as_str().unwrap().to_lowercase();
                assert_eq!(to, ORACLE, "a call to anything but the oracle");
                self.oracle_calls.set(self.oracle_calls.get() + 1);
                if self.oracle_fails {
                    return Err(RpcError::from_json(&json!({
                        "code": -32000,
                        "message": "execution reverted",
                    })));
                }
                Ok(json!(format!("{:#066x}", self.l1_fee)))
            }
            "eth_sendRawTransaction" => {
                let raw: Bytes = params[0].as_str().unwrap().parse().unwrap();
                let tx = TxEnvelope::decode_2718(&mut raw.as_ref()).unwrap();
                let cost = u128::from(tx.gas_limit()) * tx.max_fee_per_gas() + self.l1_fee;
                if cost > self.balance.get() {
                    self.refused.borrow_mut().push(raw);
                    Err(RpcError::from_json(&json!({
                        "code": -32000,
                        "message": "insufficient funds for gas * price + value + l1 fee",
                    })))
                } else {
                    self.accepted.borrow_mut().push(raw);
                    Ok(Value::Null)
                }
            }
            "eth_getTransactionByHash" | "eth_getTransactionReceipt" => Ok(Value::Null),
            other => panic!("unexpected request {other}"),
        }
    }
}

const PHRASE: &str = "test test test test test test test test test test test junk";

fn deposit(key: &LocalKey, chain_id: u64) -> TxRequest {
    TxRequest {
        chain_id,
        from: key.address(),
        to: Address::repeat_byte(0xaa),
        data: Bytes::from(vec![0x01]),
        gas: Some(100_000),
    }
}

/// On Base, a send at the highest nonce signed in a fee spike and dropped
/// leaves a raise the coin can't pay once the spike passes. The send fitted
/// to the coin leaves room for the L1 data fee, so the pool, which charges
/// it too, takes it.
#[test]
fn a_fitted_send_on_an_op_stack_chain_leaves_room_for_the_l1_data_fee() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        // A small L1 data fee, far below Base's usual.
        transport: OpNode::new(4096, 900_000_000, 1_000_000),
    };
    let signed = SignedSends::default();
    let request = deposit(&key, 8453);
    // The spike-time send (cap 8193: 819_300_000 + 1_000_000 fits).
    block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)).unwrap();
    assert_eq!(node.transport.accepted.borrow().len(), 1);
    // ... dropped by every node; the spike passes.
    node.transport.base_fee.set(16);
    block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)).unwrap();
    assert!(node.transport.refused.borrow().is_empty());
    assert_eq!(node.transport.accepted.borrow().len(), 2);
}

/// Refused for want of coin, a page-signed send states the gas at the cap
/// it signs with plus the L1 data fee; holding exactly that, it goes.
#[test]
fn holding_the_amount_a_page_signed_send_said_it_needs_lets_it_go() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode::new(16, 1, 1_000_000),
    };
    let signed = SignedSends::default();
    let request = deposit(&key, 8453);
    let need = match block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)) {
        Err(TxError::InsufficientGas { need, .. }) => need,
        other => panic!("{other:?}"),
    };
    // Gas x (2 x base + tip), and the L1 data fee with its margin.
    assert_eq!(need, U256::from(100_000u64 * 33 + 1_250_000));
    assert!(node.transport.accepted.borrow().is_empty());
    assert!(node.transport.refused.borrow().is_empty());
    node.transport.balance.set(u128::try_from(need).unwrap());
    let result = block_on(sign_and_send(&node, &key, &request, &signed, &NoWait));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(node.transport.accepted.borrow().len(), 1);
}

/// On an OP-stack chain, an L1 data fee that can't be read leaves the cost
/// of the send unknown: nothing is signed into the record or sent, and the
/// words say so plainly.
#[test]
fn an_op_stack_send_whose_l1_data_fee_cant_be_read_sends_nothing() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode {
            oracle_fails: true,
            ..OpNode::new(16, 900_000_000, 1_000_000)
        },
    };
    let signed = SignedSends::default();
    let request = deposit(&key, 84532);
    let result = block_on(sign_and_send(&node, &key, &request, &signed, &NoWait));
    match result {
        Err(TxError::Network(words)) => assert!(words.contains("nothing was sent"), "{words}"),
        other => panic!("{other:?}"),
    }
    assert!(node.transport.accepted.borrow().is_empty());
    assert!(node.transport.refused.borrow().is_empty());
    assert!(signed.get(84532, key.address()).unwrap().is_none());
}

/// A chain that charges no L1 data fee is never asked for one.
#[test]
fn a_send_on_a_chain_without_an_l1_data_fee_never_reads_one() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode::new(16, 900_000_000, 0),
    };
    let signed = SignedSends::default();
    let request = deposit(&key, 31337);
    block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)).unwrap();
    assert_eq!(node.transport.oracle_calls.get(), 0);
    assert_eq!(node.transport.accepted.borrow().len(), 1);
}
