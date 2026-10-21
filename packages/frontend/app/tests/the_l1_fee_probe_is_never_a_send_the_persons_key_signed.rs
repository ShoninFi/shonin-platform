//! The L1 data fee probe is read with `eth_call` to the endpoint. It is
//! never a transaction the person's key signed: anything that sees the call
//! (the endpoint, its logs, a relay) could broadcast it, and it is never on
//! the record, so the page would neither know nor follow it. It still has
//! the send's type, fields and length, so the oracle prices it the same.

use std::cell::{Cell, RefCell};

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{Transaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolCall;
use app::tx::error::TxError;
use app::tx::port::{Clock, TxRequest};
use app::wallet::phrase::{Fees, LocalKey, Unsigned};
use app::wallet::rpc::{RpcError, RpcReader, Transport};
use app::wallet::session::{SignedSends, sign_and_send};
use futures::executor::block_on;
use serde_json::{Value, json};

alloy_sol_types::sol! {
    function getL1Fee(bytes data) external view returns (uint256);
}

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

struct OpNode {
    latest: u64,
    base_fee: Cell<u64>,
    balance: Cell<u128>,
    l1_fee: u128,
    oracle_fails: bool,
    /// The transaction bytes handed to the oracle in each `eth_call`.
    probes: RefCell<Vec<Bytes>>,
    accepted: RefCell<Vec<Bytes>>,
}

impl OpNode {
    fn new(base_fee: u64, balance: u128, l1_fee: u128) -> Self {
        Self {
            latest: 5,
            base_fee: Cell::new(base_fee),
            balance: Cell::new(balance),
            l1_fee,
            oracle_fails: false,
            probes: RefCell::default(),
            accepted: RefCell::default(),
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
                let input: Bytes = params[0]["data"]
                    .as_str()
                    .or(params[0]["input"].as_str())
                    .unwrap()
                    .parse()
                    .unwrap();
                let call = getL1FeeCall::abi_decode(&input).unwrap();
                self.probes.borrow_mut().push(call.data);
                if self.oracle_fails {
                    return Err(RpcError::from_json(&json!({
                        "code": -32000,
                        "message": "request timed out",
                    })));
                }
                Ok(json!(format!("{:#066x}", self.l1_fee)))
            }
            "eth_sendRawTransaction" => {
                let raw: Bytes = params[0].as_str().unwrap().parse().unwrap();
                let tx = TxEnvelope::decode_2718(&mut raw.as_ref()).unwrap();
                let cost = u128::from(tx.gas_limit()) * tx.max_fee_per_gas() + self.l1_fee;
                if cost > self.balance.get() {
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

/// Whether `raw` is a transaction `key` validly signed (signing is
/// deterministic, so re-signing its fields gives the same bytes).
fn signed_by(key: &LocalKey, raw: &Bytes) -> Option<u64> {
    let tx = TxEnvelope::decode_2718(&mut raw.as_ref()).ok()?;
    let fees = match &tx {
        TxEnvelope::Legacy(_) => Fees::Legacy {
            gas_price: tx.gas_price().unwrap(),
        },
        _ => Fees::Eip1559 {
            max_fee: tx.max_fee_per_gas(),
            max_priority_fee: tx.max_priority_fee_per_gas().unwrap(),
        },
    };
    let again = key.sign_transaction(&Unsigned {
        chain_id: tx.chain_id()?,
        nonce: tx.nonce(),
        gas: tx.gas_limit(),
        fees,
        to: tx.to()?,
        data: tx.input().clone(),
    });
    (&again == raw).then(|| tx.nonce())
}

/// Refused for want of coin ("nothing was sent"), the page has still handed
/// the endpoint a valid send at nonce 5. Once the person tops up (what the
/// refusal asks), anyone holding that probe can land it; the page, with no
/// record, signs its next send at 6: two sends land where the person meant
/// one, and the "refused" deposit is one of them.
#[test]
fn a_send_refused_for_want_of_coin_hands_no_valid_send_to_the_endpoint() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode::new(16, 1, 1_000_000),
    };
    let signed = SignedSends::default();
    let result = block_on(sign_and_send(
        &node,
        &key,
        &deposit(&key, 8453),
        &signed,
        &NoWait,
    ));
    assert!(
        matches!(result, Err(TxError::InsufficientGas { .. })),
        "{result:?}"
    );
    assert!(signed.get(8453, key.address()).unwrap().is_none());
    assert!(
        !node.transport.probes.borrow().is_empty(),
        "the L1 data fee was read"
    );
    let leaked: Vec<u64> = node
        .transport
        .probes
        .borrow()
        .iter()
        .filter_map(|probe| signed_by(&key, probe))
        .collect();
    assert!(
        leaked.is_empty(),
        "a send that sent nothing handed the endpoint a valid signed send at nonce(s) {leaked:?}"
    );
}

/// The oracle read times out after the endpoint got it: "nothing was sent",
/// yet a valid send left the page.
#[test]
fn a_send_whose_l1_fee_read_fails_hands_no_valid_send_to_the_endpoint() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode {
            oracle_fails: true,
            ..OpNode::new(16, 900_000_000, 1_000_000)
        },
    };
    let signed = SignedSends::default();
    let result = block_on(sign_and_send(
        &node,
        &key,
        &deposit(&key, 84532),
        &signed,
        &NoWait,
    ));
    assert!(matches!(result, Err(TxError::Network(_))), "{result:?}");
    assert!(
        !node.transport.probes.borrow().is_empty(),
        "the L1 data fee was read"
    );
    let leaked: Vec<u64> = node
        .transport
        .probes
        .borrow()
        .iter()
        .filter_map(|probe| signed_by(&key, probe))
        .collect();
    assert!(
        leaked.is_empty(),
        "a send that sent nothing handed the endpoint a valid signed send at nonce(s) {leaked:?}"
    );
}

/// At H with a fitted cap, the probe is signed at the raised fees, the send
/// at the fitted ones: a second valid send at H, off the record. If it is
/// the one that lands, the follow of the recorded send can't name it as the
/// page's own try (it isn't in `other_tries`) and says "replaced" although
/// the deposit went through: a retry then deposits twice.
#[test]
fn every_valid_send_that_leaves_the_page_is_on_the_record() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode::new(4096, 900_000_000, 1_000_000),
    };
    let signed = SignedSends::default();
    let request = deposit(&key, 8453);
    block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)).unwrap();
    node.transport.base_fee.set(16);
    block_on(sign_and_send(&node, &key, &request, &signed, &NoWait)).unwrap();
    let recorded = signed.get(8453, key.address()).unwrap().unwrap().hashes();
    assert_eq!(node.transport.probes.borrow().len(), 2);
    for probe in node.transport.probes.borrow().iter() {
        if signed_by(&key, probe).is_some() {
            let hash = alloy_primitives::keccak256(probe);
            assert!(
                recorded.contains(&hash),
                "a valid send at nonce {:?} left the page in an eth_call and is not on the record",
                signed_by(&key, probe)
            );
        }
    }
}

/// The probe is the recorded send's type, chain, nonce, gas, fees, target
/// and data, with the same encoded length: only the signer differs.
#[test]
fn the_probe_has_the_sends_type_fields_and_length_but_another_signer() {
    let key = LocalKey::from_phrase(PHRASE).unwrap();
    let node = RpcReader {
        transport: OpNode::new(16, 900_000_000, 1_000_000),
    };
    let signed = SignedSends::default();
    block_on(sign_and_send(
        &node,
        &key,
        &deposit(&key, 8453),
        &signed,
        &NoWait,
    ))
    .unwrap();
    let probes = node.transport.probes.borrow();
    let accepted = node.transport.accepted.borrow();
    assert_eq!((probes.len(), accepted.len()), (1, 1));
    let probe = TxEnvelope::decode_2718(&mut probes[0].as_ref()).unwrap();
    let send = TxEnvelope::decode_2718(&mut accepted[0].as_ref()).unwrap();
    assert_eq!(probe.tx_type(), send.tx_type());
    assert_eq!(probe.chain_id(), send.chain_id());
    assert_eq!(probe.nonce(), send.nonce());
    assert_eq!(probe.gas_limit(), send.gas_limit());
    assert_eq!(probe.max_fee_per_gas(), send.max_fee_per_gas());
    assert_eq!(
        probe.max_priority_fee_per_gas(),
        send.max_priority_fee_per_gas()
    );
    assert_eq!(probe.to(), send.to());
    assert_eq!(probe.input(), send.input());
    assert_eq!(probes[0].len(), accepted[0].len());
    let probe_from = probe.recover_signer().unwrap();
    assert_eq!(send.recover_signer().unwrap(), key.address());
    assert_ne!(probe_from, key.address());
}
