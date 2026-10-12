//! A transaction is said replaced only when two looks running agree: known
//! once, then unknown twice, its nonce used, and no receipt on two reads; a
//! single contradictory look is never enough.

use std::cell::RefCell;
use std::collections::VecDeque;

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::SolValue;
use app::tx::error::TxError;
use app::tx::flow::{self, Cancel, Ctx, Nudge, Outcome, Pace, Step};
use app::tx::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use app::wallet::rpc::{RpcError, RpcReader, Transport};
use futures::executor::block_on;
use serde_json::{Value, json};

const CHAIN: u64 = 8453;
const BLOCK: u64 = 0x64;

fn account() -> Address {
    Address::repeat_byte(0x42)
}
fn vault() -> Address {
    Address::repeat_byte(0xaa)
}
fn asset() -> Address {
    Address::repeat_byte(0xdd)
}
fn hash() -> B256 {
    B256::repeat_byte(0x11)
}

// A transaction that really was replaced ----------------------------------

/// A pool of nodes answered in the order the app asks: a scripted prefix
/// of (method, answer), then settled answers.
struct Pool {
    script: RefCell<VecDeque<(&'static str, Value)>>,
}

impl Transport for Pool {
    async fn request(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        if let Some((expected, answer)) = self.script.borrow_mut().pop_front() {
            assert_eq!(
                method, expected,
                "the app asked {method}, the script expected {expected}"
            );
            return Ok(answer);
        }
        Ok(match method {
            // Replaced for good: no node ever has it or its receipt again.
            "eth_getTransactionReceipt" => Value::Null,
            "eth_getTransactionByHash" => Value::Null,
            "eth_getTransactionCount" => json!("0x6"),
            "eth_blockNumber" => json!(format!("{BLOCK:#x}")),
            other => panic!("unexpected request {other}"),
        })
    }
}

struct PoolReader {
    rpc: RpcReader<Pool>,
}

impl Reader for PoolReader {
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
        self.rpc.receipt(hash).await
    }
    async fn receipt_seen(&self, hash: B256) -> Result<bool, TxError> {
        self.rpc.receipt_seen(hash).await
    }
    async fn pending(&self, hash: B256) -> Result<Pending, TxError> {
        self.rpc.pending(hash).await
    }
    async fn mined_nonce(&self, who: Address) -> Result<u64, TxError> {
        self.rpc.mined_nonce(who).await
    }
    async fn block_number(&self) -> Result<Option<u64>, TxError> {
        self.rpc.block_number().await
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

struct Wallet;

impl Signer for Wallet {
    fn account(&self) -> Option<Address> {
        Some(account())
    }
    fn name(&self) -> String {
        "MetaMask".to_string()
    }
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn sign_statement(&self, _statement: &StatementRequest) -> Result<Bytes, TxError> {
        unreachable!("no statement in this flow")
    }
    async fn send(&self, _request: &TxRequest) -> Result<B256, TxError> {
        Ok(hash())
    }
}

struct NoWait;

impl Clock for NoWait {
    async fn sleep(&self, _millis: u32) {}
}

fn run(script: Vec<(&'static str, Value)>) -> (Result<Outcome, TxError>, Vec<Step>, usize) {
    let reader = PoolReader {
        rpc: RpcReader {
            transport: Pool {
                script: RefCell::new(script.into()),
            },
        },
    };
    let steps = RefCell::new(Vec::new());
    let on_step = |step: Step| steps.borrow_mut().push(step);
    let ctx = Ctx {
        reader: &reader,
        signer: &Wallet,
        clock: &NoWait,
        chain_id: CHAIN,
        vault: vault(),
        asset: asset(),
        account: account(),
        cancel: Cancel::new(),
        settle_now: Nudge::new(),
        skip_second: Nudge::new(),
        pace: Pace::FAST,
        on_step: &on_step,
    };
    let result = block_on(flow::collect_held(&ctx));
    let left = reader.rpc.transport.script.borrow().len();
    (result, steps.into_inner(), left)
}

/// Poll 1: no receipt; the node has the transaction (nonce 5). From poll 2
/// on, the wallet has replaced it: no node has it or any receipt for it,
/// and the nonce at `latest` is 6. The hardened check must still call it
/// replaced, on the second look running, and never wait out the long wait.
#[test]
fn a_transaction_gone_on_two_looks_with_its_nonce_used_and_no_receipt_twice_is_still_said_replaced()
{
    let script = vec![
        ("eth_getTransactionReceipt", Value::Null),
        (
            "eth_getTransactionByHash",
            json!({"hash": format!("{:?}", hash()), "nonce": "0x5"}),
        ),
        // Poll 2: gone once; no verdict yet.
        ("eth_getTransactionReceipt", Value::Null),
        ("eth_getTransactionByHash", Value::Null),
        // Poll 3: gone twice running; the nonce is used; no receipt, asked twice.
        ("eth_getTransactionReceipt", Value::Null),
        ("eth_getTransactionByHash", Value::Null),
        ("eth_getTransactionCount", json!("0x6")),
        ("eth_getTransactionReceipt", Value::Null),
        ("eth_getTransactionReceipt", Value::Null),
    ];
    let (result, steps, left) = run(script);
    assert!(
        matches!(result, Err(TxError::Replaced { hash: h }) if h == hash()),
        "a replaced transaction was reported as {result:?}; steps: {steps:?}"
    );
    assert_eq!(left, 0, "the verdict came before the script ran out");
    // Never said unseen: the verdict came on the third poll.
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::Sent { unseen: true, .. })),
        "{steps:?}"
    );
}

/// A pool where one node answers `eth_getTransactionByHash` with null once
/// between two nodes that have the transaction, while the nonce at `latest`
/// has moved on: one look is never enough for the verdict.
#[test]
fn a_transaction_gone_on_one_look_only_is_not_said_replaced() {
    let script = vec![
        ("eth_getTransactionReceipt", Value::Null),
        (
            "eth_getTransactionByHash",
            json!({"hash": format!("{:?}", hash()), "nonce": "0x5"}),
        ),
        ("eth_getTransactionReceipt", Value::Null),
        ("eth_getTransactionByHash", Value::Null),
        ("eth_getTransactionReceipt", Value::Null),
        (
            "eth_getTransactionByHash",
            json!({"hash": format!("{:?}", hash()), "nonce": "0x5"}),
        ),
        // Sealed now.
        (
            "eth_getTransactionReceipt",
            json!({
                "transactionHash": format!("{:?}", hash()),
                "status": "0x1",
                "blockNumber": format!("{BLOCK:#x}"),
                "blockHash": format!("0x{}", "33".repeat(32)),
                "logs": [],
            }),
        ),
    ];
    let (result, steps, _) = run(script);
    assert!(
        matches!(result, Ok(Outcome::Collected { hash: h, .. }) if h == hash()),
        "{result:?}; steps: {steps:?}"
    );
}
