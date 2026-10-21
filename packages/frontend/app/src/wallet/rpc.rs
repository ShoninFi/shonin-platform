//! JSON-RPC to a vault chain: through its public endpoint for reading (and
//! for the local signers' sends), and through a browser wallet's provider
//! for asking it things. The parsing is plain functions over JSON, tested
//! natively; only the transport is the browser's.

use alloy_primitives::{Address, B256, Bytes, U256};
use serde_json::{Value, json};

use crate::tx::error::{Revert, TxError};
use crate::tx::port::{Clock, Log, Pending, Reader, Receipt, ReceiptSeen, TxRequest};

/// Where a JSON-RPC request goes.
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

/// A JSON-RPC or EIP-1193 error, as it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// Revert data, when the node gave it.
    pub data: Option<String>,
    /// How far the request got.
    pub fate: Fate,
}

/// How far a request that ended in an error got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// It couldn't be made, so it never left.
    NotMade,
    /// A node, a wallet, or something in front of a node answered with an
    /// error.
    Answered,
    /// It may have reached a node, and the answer was lost or made no
    /// sense: what it asked may have happened.
    Lost,
}

impl RpcError {
    /// No answer, and the request can't have been handled: it couldn't be
    /// made.
    pub fn unreachable(why: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: why.into(),
            data: None,
            fate: Fate::NotMade,
        }
    }

    /// No answer that makes sense to a request that may have reached the
    /// node: the connection dropped or timed out, a gateway failed, or the
    /// reply was garbled.
    pub fn lost(why: impl Into<String>) -> Self {
        Self {
            fate: Fate::Lost,
            ..Self::unreachable(why)
        }
    }

    /// An endpoint's HTTP status other than 200, after any waiting out of a
    /// 429, whose body held no JSON-RPC error: said by the status alone.
    pub fn status(status: u16) -> Self {
        Self::lost(format!("the chain's endpoint answered {status}"))
    }

    /// Read an error object: `{code, message, data}`. `data` may be the
    /// revert data itself, or an object carrying it.
    pub fn from_json(value: &Value) -> Self {
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no reason given")
            .to_string();
        let data = match value.get("data") {
            Some(Value::String(data)) => Some(data.clone()),
            Some(Value::Object(inner)) => inner
                .get("data")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        };
        Self {
            code,
            message,
            data,
            fate: Fate::Answered,
        }
    }

    /// What it means for a flow.
    pub fn into_tx_error(self) -> TxError {
        let lower = self.message.to_ascii_lowercase();
        match self.code {
            4001 => return TxError::Rejected,
            4100 => return TxError::Locked,
            -32002 => return TxError::AlreadyAsking,
            _ => {}
        }
        if lower.contains("user rejected")
            || lower.contains("user denied")
            || lower.contains("rejected by user")
            || lower.contains("user cancelled")
        {
            return TxError::Rejected;
        }
        if lower.contains("insufficient funds") {
            return TxError::InsufficientGas {
                need: U256::ZERO,
                have: U256::ZERO,
            };
        }
        if let Some(data) = self.data.as_deref().and_then(|data| hex_bytes(data).ok())
            && !data.is_empty()
        {
            return TxError::WouldRevert {
                reason: Revert::from_data(&data),
            };
        }
        if self.code == 3 || lower.contains("revert") {
            return TxError::WouldRevert {
                reason: Revert::from_message(&self.message),
            };
        }
        if self.code == 0 {
            return TxError::Network(self.message);
        }
        TxError::Wallet(self.message)
    }
}

/// `0x`-prefixed hex to bytes.
pub fn hex_bytes(text: &str) -> Result<Vec<u8>, TxError> {
    alloy_primitives::hex::decode(text.trim_start_matches("0x"))
        .map_err(|_| TxError::Network("the chain answered in something other than hex".to_string()))
}

/// A hex quantity (`0x1a`) as a number.
pub fn quantity(value: &Value) -> Result<U256, TxError> {
    let text = value
        .as_str()
        .ok_or_else(|| TxError::Network("the chain left out a number".to_string()))?;
    let digits = text.trim_start_matches("0x");
    if digits.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(digits, 16)
        .map_err(|_| TxError::Network("the chain answered with a malformed number".to_string()))
}

pub fn quantity_u64(value: &Value) -> Result<u64, TxError> {
    u64::try_from(quantity(value)?)
        .map_err(|_| TxError::Network("the chain answered with a number too large".to_string()))
}

fn word(value: &Value) -> Result<B256, TxError> {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| TxError::Network("the chain answered with a malformed hash".to_string()))
}

fn address(value: &Value) -> Result<Address, TxError> {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| TxError::Network("the chain answered with a malformed address".to_string()))
}

/// One log of a receipt or of `eth_getLogs`.
pub fn parse_log(value: &Value) -> Result<Log, TxError> {
    let topics = value
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(|| TxError::Network("a log came without topics".to_string()))?
        .iter()
        .map(word)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Log {
        address: address(&value["address"])?,
        topics,
        data: Bytes::from(hex_bytes(value["data"].as_str().unwrap_or("0x"))?),
        transaction: word(&value["transactionHash"]).ok(),
    })
}

/// Whether a receipt's block is sealed: only a block hash of all zeros (a
/// receipt handed out ahead of its block) or a null one says it isn't; a
/// node that leaves the field out is taken at its block number.
fn sealed(receipt: &Value) -> bool {
    match receipt.get("blockHash") {
        Some(Value::String(hash)) => !hash.trim_start_matches("0x").chars().all(|c| c == '0'),
        Some(Value::Null) => false,
        _ => true,
    }
}

/// Whether a receipt answer is a receipt at all, sealed or not.
pub fn receipt_exists(value: &Value) -> bool {
    !value.is_null() && !value.get("blockNumber").is_none_or(Value::is_null)
}

/// What a receipt answer shows, sealed or not: a status other than 1 is a
/// revert, and a receipt whose status is missing or isn't a number is
/// mined with its outcome unknown.
pub fn receipt_seen_of(value: &Value) -> ReceiptSeen {
    if !receipt_exists(value) {
        return ReceiptSeen::Absent;
    }
    match value.get("status").map(quantity) {
        Some(Ok(status)) if status == U256::from(1u8) => ReceiptSeen::Succeeded,
        Some(Ok(_)) => ReceiptSeen::Reverted,
        _ => ReceiptSeen::StatusUnread,
    }
}

/// A receipt, or `None` while it isn't mined.
pub fn parse_receipt(value: &Value) -> Result<Option<Receipt>, TxError> {
    if value.is_null() {
        return Ok(None);
    }
    // A receipt without a block isn't mined yet (some nodes answer so), and
    // nor is one whose block has no hash yet: a chain that confirms ahead of
    // its blocks (Base's flashblocks) hands out a receipt with a zero block
    // hash before the block is sealed, while a call against the latest block
    // can't see the transaction yet.
    if value.get("blockNumber").is_none_or(Value::is_null) {
        return Ok(None);
    }
    if !sealed(value) {
        return Ok(None);
    }
    let logs = value
        .get("logs")
        .and_then(Value::as_array)
        .map(|logs| logs.iter().map(parse_log).collect::<Result<Vec<_>, _>>())
        .transpose()?
        .unwrap_or_default();
    Ok(Some(Receipt {
        hash: word(&value["transactionHash"])?,
        from: address(&value["from"]).ok(),
        success: quantity(&value["status"])? == U256::from(1u8),
        block: quantity_u64(&value["blockNumber"])?,
        logs,
    }))
}

/// A transaction as `eth_sendTransaction` takes it.
pub fn transaction_json(request: &TxRequest) -> Value {
    let mut tx = json!({
        "from": format!("{:?}", request.from),
        "to": format!("{:?}", request.to),
        "data": format!("{}", request.data),
        "value": "0x0",
        "chainId": format!("{:#x}", request.chain_id),
    });
    if let Some(gas) = request.gas {
        tx["gas"] = json!(format!("{gas:#x}"));
    }
    tx
}

/// A transport whose every request ends within `millis`: one with no
/// answer by then may still have reached a node, so it is lost
/// ([`Fate::Lost`]), never refused (a raw send ending so is followed).
pub struct Bounded<'a, T, C> {
    pub inner: &'a T,
    pub clock: &'a C,
    pub millis: u32,
}

impl<T: Transport, C: Clock> Transport for Bounded<'_, T, C> {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let answer = std::pin::pin!(self.inner.request(method, params));
        let late = std::pin::pin!(self.clock.sleep(self.millis));
        match futures::future::select(answer, late).await {
            futures::future::Either::Left((answer, _)) => answer,
            futures::future::Either::Right(_) => {
                Err(RpcError::lost("the chain's endpoint didn't answer in time"))
            }
        }
    }
}

/// Reading a chain through a transport.
pub struct RpcReader<T> {
    pub transport: T,
}

impl<T: Transport> RpcReader<T> {
    pub async fn ask(&self, method: &str, params: Value) -> Result<Value, TxError> {
        self.transport
            .request(method, params)
            .await
            .map_err(RpcError::into_tx_error)
    }

    /// The account's next nonce, counting its pending transactions.
    pub async fn pending_nonce(&self, who: Address) -> Result<u64, TxError> {
        quantity_u64(
            &self
                .ask(
                    "eth_getTransactionCount",
                    json!([format!("{who:?}"), "pending"]),
                )
                .await?,
        )
    }

    /// `eth_call` at `block` (`"latest"`, or a number in hex).
    async fn call_with(&self, to: Address, data: Bytes, block: Value) -> Result<Bytes, TxError> {
        let answer = self
            .ask(
                "eth_call",
                json!([{"to": format!("{to:?}"), "data": format!("{data}")}, block]),
            )
            .await?;
        let text = answer.as_str().ok_or_else(|| {
            TxError::Network("the chain answered a read with nothing".to_string())
        })?;
        Ok(Bytes::from(hex_bytes(text)?))
    }

    /// The latest block's base fee, if the chain has one (EIP-1559).
    pub async fn base_fee(&self) -> Result<Option<U256>, TxError> {
        let block = self
            .ask("eth_getBlockByNumber", json!(["latest", false]))
            .await?;
        block
            .get("baseFeePerGas")
            .filter(|fee| !fee.is_null())
            .map(quantity)
            .transpose()
    }

    /// The tip the chain suggests.
    pub async fn priority_fee(&self) -> Result<U256, TxError> {
        quantity(&self.ask("eth_maxPriorityFeePerGas", json!([])).await?)
    }

    /// The account's next nonce counting only mined transactions.
    pub async fn latest_nonce(&self, who: Address) -> Result<u64, TxError> {
        quantity_u64(
            &self
                .ask(
                    "eth_getTransactionCount",
                    json!([format!("{who:?}"), "latest"]),
                )
                .await?,
        )
    }

    /// Hand a signed transaction to the endpoint. It is known by the hash
    /// of its signed bytes. Only a request that couldn't be made is a
    /// refusal: it never left. After any other error, whatever its words,
    /// the transaction may have reached a node (an endpoint can be a pool
    /// of nodes, and no one look at it proves a node didn't take it), so it
    /// is followed by its hash. The page never signs past its nonce until
    /// the chain shows that nonce mined ([`super::session::sign_and_send`]),
    /// so a send after it takes its place, and at most one of the two can
    /// land.
    pub async fn send_raw(&self, raw: &Bytes) -> Result<B256, TxError> {
        let hash = alloy_primitives::keccak256(raw);
        match self
            .transport
            .request("eth_sendRawTransaction", json!([format!("{raw}")]))
            .await
        {
            Err(error) if error.fate == Fate::NotMade => Err(error.into_tx_error()),
            _ => Ok(hash),
        }
    }
}

impl<T: Transport> Reader for RpcReader<T> {
    async fn chain_id(&self) -> Result<u64, TxError> {
        quantity_u64(&self.ask("eth_chainId", json!([])).await?)
    }

    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        self.call_with(to, data, json!("latest")).await
    }

    async fn call_at(&self, to: Address, data: Bytes, block: u64) -> Result<Bytes, TxError> {
        self.call_with(to, data, json!(format!("{block:#x}"))).await
    }

    async fn native_balance(&self, who: Address) -> Result<U256, TxError> {
        quantity(
            &self
                .ask("eth_getBalance", json!([format!("{who:?}"), "latest"]))
                .await?,
        )
    }

    async fn estimate_gas(&self, request: &TxRequest) -> Result<u64, TxError> {
        let tx = json!({
            "from": format!("{:?}", request.from),
            "to": format!("{:?}", request.to),
            "data": format!("{}", request.data),
        });
        quantity_u64(&self.ask("eth_estimateGas", json!([tx])).await?)
    }

    async fn gas_price(&self) -> Result<U256, TxError> {
        quantity(&self.ask("eth_gasPrice", json!([])).await?)
    }

    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, TxError> {
        parse_receipt(
            &self
                .ask("eth_getTransactionReceipt", json!([format!("{hash:?}")]))
                .await?,
        )
    }

    async fn receipt_seen(&self, hash: B256) -> Result<bool, TxError> {
        Ok(receipt_exists(
            &self
                .ask("eth_getTransactionReceipt", json!([format!("{hash:?}")]))
                .await?,
        ))
    }

    async fn receipt_seen_as(&self, hash: B256) -> Result<ReceiptSeen, TxError> {
        let value = self
            .ask("eth_getTransactionReceipt", json!([format!("{hash:?}")]))
            .await?;
        Ok(receipt_seen_of(&value))
    }

    async fn pending(&self, hash: B256) -> Result<Pending, TxError> {
        let tx = self
            .ask("eth_getTransactionByHash", json!([format!("{hash:?}")]))
            .await?;
        if tx.is_null() {
            return Ok(Pending::Unknown);
        }
        Ok(Pending::Known {
            nonce: quantity_u64(&tx["nonce"])?,
        })
    }

    async fn mined_nonce(&self, who: Address) -> Result<u64, TxError> {
        self.latest_nonce(who).await
    }

    async fn pending_nonce(&self, who: Address) -> Result<u64, TxError> {
        RpcReader::pending_nonce(self, who).await
    }

    async fn chain_time(&self) -> Result<u64, TxError> {
        let block = self
            .ask("eth_getBlockByNumber", json!(["latest", false]))
            .await?;
        quantity_u64(&block["timestamp"])
    }

    async fn block_number(&self) -> Result<Option<u64>, TxError> {
        Ok(Some(quantity_u64(
            &self.ask("eth_blockNumber", json!([])).await?,
        )?))
    }

    async fn block_time(&self, number: u64) -> Result<u64, TxError> {
        let block = self
            .ask(
                "eth_getBlockByNumber",
                json!([format!("{number:#x}"), false]),
            )
            .await?;
        quantity_u64(&block["timestamp"])
    }

    async fn logs(
        &self,
        address: Address,
        topic0: B256,
        topic1: Option<B256>,
        from_block: u64,
    ) -> Result<Vec<Log>, TxError> {
        let mut topics = vec![json!(format!("{topic0:?}"))];
        if let Some(topic1) = topic1 {
            topics.push(json!(format!("{topic1:?}")));
        }
        let answer = self
            .ask(
                "eth_getLogs",
                json!([{
                    "address": format!("{address:?}"),
                    "topics": topics,
                    "fromBlock": format!("{from_block:#x}"),
                    "toBlock": "latest",
                }]),
            )
            .await?;
        answer
            .as_array()
            .ok_or_else(|| {
                TxError::Network("the chain's logs came in an unexpected shape".to_string())
            })?
            .iter()
            .map(parse_log)
            .collect()
    }
}

/// How long to wait before asking a rate-limited endpoint again, in
/// milliseconds: what it asked for (`Retry-After`, in seconds, at most 8),
/// or half a second doubling each time; `None` once five tries have been
/// waited out (about 15 seconds in all), and the endpoint's answer stands.
pub fn backoff(attempt: u32, retry_after_seconds: Option<u32>) -> Option<u32> {
    if attempt >= 5 {
        return None;
    }
    Some(match retry_after_seconds {
        Some(seconds) => seconds.min(8) * 1_000,
        None => 500u32 << attempt,
    })
}

/// The error of an endpoint's answer whose HTTP status isn't 200, after any
/// waiting out of a 429, from its status and, for a 4xx, its body read as
/// JSON: a 4xx carrying a JSON-RPC error object is shown in its words (some
/// endpoints answer a node's error with a 4xx); anything else is said by the
/// status alone ([`RpcError::status`]). What became of a raw send is never
/// read from either: it is followed ([`RpcReader::send_raw`]).
pub fn not_ok(status: u16, body: Option<&Value>) -> RpcError {
    if (400..500).contains(&status)
        && let Some(error) = body
            .and_then(|body| body.get("error"))
            .filter(|error| error.is_object())
    {
        return RpcError::from_json(error);
    }
    RpcError::status(status)
}

/// A chain's public endpoint, over the browser's `fetch`.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
}

#[cfg(target_arch = "wasm32")]
impl Transport for Endpoint {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut attempt = 0;
        let reply = loop {
            // A request that couldn't be made never left.
            let request = gloo_net::http::Request::post(&self.url)
                .json(&body)
                .map_err(|error| RpcError::unreachable(error.to_string()))?;
            // A fetch that fails may have failed after the request was sent
            // (the connection dropped, the answer timed out).
            let reply = request
                .send()
                .await
                .map_err(|_| RpcError::lost("the chain's endpoint didn't answer"))?;
            // A public endpoint asking us to slow down: wait as it asks, or
            // back off, a few times. Asking again is safe, sends included:
            // a send is the same signed bytes each time, and what became of
            // it is read from the chain afterwards.
            if reply.status() == 429 {
                let asked = reply
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.trim().parse::<u32>().ok());
                if let Some(wait) = backoff(attempt, asked) {
                    gloo_timers::future::TimeoutFuture::new(wait).await;
                    attempt += 1;
                    continue;
                }
            }
            break reply;
        };
        if !reply.ok() {
            // Some endpoints answer a node's JSON-RPC error with a 4xx: its
            // words are what is shown. A body without one (or any other
            // status) is said by the status alone.
            let status = reply.status();
            let body = if (400..500).contains(&status) {
                reply.json::<Value>().await.ok()
            } else {
                None
            };
            return Err(not_ok(status, body.as_ref()));
        }
        // A 200 whose body can't be read: the node had the request.
        let reply: Value = reply
            .json()
            .await
            .map_err(|_| RpcError::lost("the chain's endpoint answered in an unexpected shape"))?;
        if let Some(error) = reply.get("error") {
            return Err(RpcError::from_json(error));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Without a browser nothing is sent: the pre-render reads no chain.
#[cfg(not(target_arch = "wasm32"))]
impl Transport for Endpoint {
    async fn request(&self, _method: &str, _params: Value) -> Result<Value, RpcError> {
        Err(RpcError::unreachable("this build reads no chain"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wallets_rejection_is_a_rejection_whatever_its_wording() {
        let error =
            RpcError::from_json(&json!({"code": 4001, "message": "User rejected the request."}));
        assert_eq!(error.into_tx_error(), TxError::Rejected);
        let error = RpcError::from_json(
            &json!({"code": -32603, "message": "MetaMask Tx Signature: User denied transaction signature."}),
        );
        assert_eq!(error.into_tx_error(), TxError::Rejected);
    }

    #[test]
    fn locked_and_already_asking_wallets_are_told_apart() {
        assert_eq!(
            RpcError::from_json(&json!({"code": 4100, "message": "unauthorized"})).into_tx_error(),
            TxError::Locked
        );
        assert_eq!(
            RpcError::from_json(&json!({"code": -32002, "message": "Request already pending"}))
                .into_tx_error(),
            TxError::AlreadyAsking
        );
    }

    #[test]
    fn a_revert_with_data_is_read_from_the_data() {
        use alloy_sol_types::SolError;
        let data = format!(
            "0x{}",
            alloy_primitives::hex::encode(chain::bindings::vault::IVault::NoStatement::SELECTOR)
        );
        let error =
            RpcError::from_json(&json!({"code": 3, "message": "execution reverted", "data": data}));
        assert_eq!(
            error.into_tx_error(),
            TxError::WouldRevert {
                reason: Revert::Vault("NoStatement")
            }
        );
        // Nested, as some wallets pass it on.
        let error = RpcError::from_json(
            &json!({"code": -32603, "message": "Internal JSON-RPC error.", "data": {"code": 3, "data": data}}),
        );
        assert_eq!(
            error.into_tx_error(),
            TxError::WouldRevert {
                reason: Revert::Vault("NoStatement")
            }
        );
    }

    #[test]
    fn insufficient_funds_for_gas_is_said_as_such() {
        let error = RpcError::from_json(
            &json!({"code": -32000, "message": "insufficient funds for gas * price + value"}),
        );
        assert!(matches!(
            error.into_tx_error(),
            TxError::InsufficientGas { .. }
        ));
    }

    #[test]
    fn a_rate_limited_endpoint_is_asked_again_after_backing_off_then_its_answer_stands() {
        assert_eq!(backoff(0, None), Some(500));
        assert_eq!(backoff(1, None), Some(1_000));
        assert_eq!(backoff(4, None), Some(8_000));
        assert_eq!(backoff(5, None), None);
        assert_eq!(backoff(0, Some(2)), Some(2_000));
        assert_eq!(backoff(0, Some(60)), Some(8_000));
        let total: u32 = (0..5).map(|n| backoff(n, None).unwrap()).sum();
        assert!(total <= 16_000, "{total}");
    }

    #[test]
    fn a_receipt_is_read_with_its_status_block_and_logs() {
        let receipt = json!({
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "status": "0x1",
            "blockNumber": "0x2a",
            "blockHash": format!("0x{}", "33".repeat(32)),
            "logs": [{
                "address": format!("0x{}", "aa".repeat(20)),
                "topics": [format!("0x{}", "22".repeat(32))],
                "data": "0x0102",
            }],
        });
        let receipt = parse_receipt(&receipt).unwrap().unwrap();
        assert!(receipt.success);
        assert_eq!(receipt.block, 42);
        assert_eq!(receipt.logs[0].address, Address::repeat_byte(0xaa));
        assert_eq!(receipt.logs[0].data.as_ref(), &[1, 2]);
        let failed = json!({"transactionHash": format!("0x{}", "11".repeat(32)), "status": "0x0", "blockNumber": "0x1", "blockHash": format!("0x{}", "33".repeat(32)), "logs": []});
        assert!(!parse_receipt(&failed).unwrap().unwrap().success);
        assert_eq!(parse_receipt(&Value::Null).unwrap(), None);
        assert_eq!(parse_receipt(&json!({"blockNumber": null})).unwrap(), None);
    }

    /// A chain that confirms ahead of its blocks hands out a receipt whose
    /// block has no hash yet; until the block is sealed a call against the
    /// latest block can't see the transaction, so it isn't mined yet.
    #[test]
    fn a_receipt_whose_block_is_not_sealed_is_not_mined_yet() {
        let preconfirmed = json!({
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "status": "0x1",
            "blockNumber": "0x2a",
            "blockHash": format!("0x{}", "00".repeat(32)),
            "logs": [],
        });
        assert_eq!(parse_receipt(&preconfirmed).unwrap(), None);
        assert!(receipt_exists(&preconfirmed));
        let mut null_hash = preconfirmed.clone();
        null_hash["blockHash"] = Value::Null;
        assert_eq!(parse_receipt(&null_hash).unwrap(), None);
        // A node that leaves the hash out is taken at its block number.
        let no_hash = json!({
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "status": "0x1",
            "blockNumber": "0x2a",
            "logs": [],
        });
        assert_eq!(parse_receipt(&no_hash).unwrap().unwrap().block, 42);
        assert!(!receipt_exists(&Value::Null));
    }

    #[test]
    fn a_sent_transaction_names_its_chain_and_never_carries_value() {
        let request = TxRequest {
            chain_id: 84532,
            from: Address::repeat_byte(1),
            to: Address::repeat_byte(2),
            data: Bytes::from(vec![0xab]),
            gas: Some(21_000),
        };
        let tx = transaction_json(&request);
        assert_eq!(tx["chainId"], "0x14a34");
        assert_eq!(tx["value"], "0x0");
        assert_eq!(tx["gas"], "0x5208");
        assert_eq!(tx["data"], "0xab");
        assert_eq!(tx["to"], format!("0x{}", "02".repeat(20)));
    }

    /// An endpoint whose `eth_sendRawTransaction` ends in `send`, and which
    /// is asked nothing else.
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

    const RAW: [u8; 3] = [0x02, 0xab, 0xcd];

    fn send_raw_on(send: Result<Value, RpcError>) -> Result<B256, TxError> {
        let reader = RpcReader {
            transport: Node { send },
        };
        futures::executor::block_on(reader.send_raw(&Bytes::from(RAW.to_vec())))
    }

    fn node(message: &str) -> RpcError {
        RpcError::from_json(&json!({"code": -32000, "message": message}))
    }

    #[test]
    fn a_raw_send_the_node_took_is_known_by_the_hash_of_its_signed_bytes() {
        let hash = alloy_primitives::keccak256(RAW);
        assert_eq!(send_raw_on(Ok(json!(format!("{hash:?}")))), Ok(hash));
        assert_eq!(send_raw_on(Ok(Value::Null)), Ok(hash));
    }

    #[test]
    fn any_error_to_a_raw_send_that_was_made_is_followed_whatever_its_words() {
        let hash = alloy_primitives::keccak256(RAW);
        for error in [
            node("nonce too low"),
            node("txpool is full"),
            node("already known"),
            node("request timed out"),
            node("something no node is known to say"),
            RpcError::from_json(&json!({})),
            RpcError::lost("the chain's endpoint didn't answer"),
            RpcError::status(429),
            RpcError::status(502),
            not_ok(
                429,
                Some(&json!({"error": {"code": -32016, "message": "over rate limit"}})),
            ),
        ] {
            assert_eq!(send_raw_on(Err(error.clone())), Ok(hash), "{error:?}");
        }
    }

    #[test]
    fn a_raw_send_that_couldnt_be_made_is_a_refusal() {
        assert!(matches!(
            send_raw_on(Err(RpcError::unreachable("couldn't encode the request"))),
            Err(TxError::Network(_))
        ));
    }

    #[test]
    fn quantities_parse_and_garbage_is_refused() {
        assert_eq!(quantity(&json!("0x0")).unwrap(), U256::ZERO);
        assert_eq!(quantity(&json!("0x")).unwrap(), U256::ZERO);
        assert_eq!(quantity_u64(&json!("0x14a34")).unwrap(), 84532);
        assert!(quantity(&json!("0xzz")).is_err());
        assert!(quantity(&json!(5)).is_err());
    }
}
