//! A transaction that is mined is never reported replaced because the
//! endpoint's receipt for it is still preconfirmed.
//!
//! A chain that confirms ahead of its blocks (Base) runs pools of nodes:
//! within one of the app's polls, `eth_getTransactionReceipt` can answer a
//! receipt with a zero block hash (preconfirmed: not yet counted as mined),
//! while `eth_getTransactionCount(latest)` from another node already counts
//! the transaction, and a node that hasn't had it yet answers
//! `eth_getTransactionByHash` with null (the wallet sent it through its own
//! provider). The replaced check ("known once, gone now, its nonce used, and
//! no receipt") must not fire then: a preconfirmed receipt proves the
//! transaction landed.

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

fn receipt_json(block_hash: &str) -> Value {
    json!({
        "transactionHash": format!("{:?}", hash()),
        "status": "0x1",
        "blockNumber": format!("{BLOCK:#x}"),
        "blockHash": block_hash,
        "logs": [],
    })
}

fn preconfirmed() -> Value {
    receipt_json(&format!("0x{}", "00".repeat(32)))
}

fn sealed() -> Value {
    receipt_json(&format!("0x{}", "33".repeat(32)))
}

/// A pool of nodes, answered in the order the app asks: a scripted prefix
/// of (method, answer), then, once the block is sealed everywhere, the
/// settled answers.
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
            "eth_getTransactionReceipt" => sealed(),
            "eth_getTransactionByHash" => json!({"hash": format!("{:?}", hash()), "nonce": "0x5"}),
            "eth_getTransactionCount" => json!("0x6"),
            "eth_blockNumber" => json!(format!("{BLOCK:#x}")),
            other => panic!("unexpected request {other}"),
        })
    }
}

/// The app's reader: the vault's state simulated here, the transaction's
/// fate read through the real JSON-RPC parsing over the pool.
struct PoolReader {
    rpc: RpcReader<Pool>,
}

impl Reader for PoolReader {
    async fn chain_id(&self) -> Result<u64, TxError> {
        Ok(CHAIN)
    }
    async fn call(&self, _to: Address, _data: Bytes) -> Result<Bytes, TxError> {
        // `heldFor`: something to collect.
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

fn run(script: Vec<(&'static str, Value)>) -> (Result<Outcome, TxError>, Vec<Step>) {
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
    (result, steps.into_inner())
}

/// Poll 1: no receipt yet; the node asked has the transaction (nonce 5).
/// Polls 2 and 3: the receipt is preconfirmed (zero block hash) and the
/// node asked about the transaction hasn't had it (null), two looks running;
/// on the second the nonce at `latest` is already 6 and the receipt re-read
/// is still preconfirmed. Poll 4 onward: sealed everywhere. The transaction
/// was mined, and the flow must say so.
#[test]
fn a_mined_transaction_whose_receipt_is_still_preconfirmed_is_never_said_replaced() {
    let script = vec![
        ("eth_getTransactionReceipt", Value::Null),
        (
            "eth_getTransactionByHash",
            json!({"hash": format!("{:?}", hash()), "nonce": "0x5"}),
        ),
        ("eth_getTransactionReceipt", preconfirmed()),
        ("eth_getTransactionByHash", Value::Null),
        ("eth_getTransactionReceipt", preconfirmed()),
        ("eth_getTransactionByHash", Value::Null),
        ("eth_getTransactionCount", json!("0x6")),
        ("eth_getTransactionReceipt", preconfirmed()),
    ];
    let (result, steps) = run(script);
    assert!(
        matches!(result, Ok(Outcome::Collected { hash: h, .. }) if h == hash()),
        "a mined transaction was reported as {result:?}; steps: {steps:?}"
    );
}
