//! JSON-RPC to a vault chain: through its public endpoint for reading (and
//! for the local signers' sends), and through a browser wallet's provider
//! for asking it things. The parsing is plain functions over JSON, tested
//! natively; only the transport is the browser's.

use alloy_primitives::{Address, B256, Bytes, U256};
use serde_json::{Value, json};

use crate::tx::error::{Revert, TxError};
use crate::tx::port::{Log, Pending, Reader, Receipt, TxRequest};

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
}

impl RpcError {
    /// No answer at all (the network, a refused fetch).
    pub fn unreachable(why: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: why.into(),
            data: None,
        }
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
    })
}

/// A receipt, or `None` while it isn't mined.
pub fn parse_receipt(value: &Value) -> Result<Option<Receipt>, TxError> {
    if value.is_null() {
        return Ok(None);
    }
    // A receipt without a block isn't mined yet (some nodes answer so).
    if value.get("blockNumber").is_none_or(Value::is_null) {
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

    /// A tip the chain suggests; zero where it can't say.
    pub async fn priority_fee(&self) -> U256 {
        match self.ask("eth_maxPriorityFeePerGas", json!([])).await {
            Ok(value) => quantity(&value).unwrap_or_default(),
            Err(_) => U256::ZERO,
        }
    }

    pub async fn send_raw(&self, raw: &Bytes) -> Result<B256, TxError> {
        word(
            &self
                .ask("eth_sendRawTransaction", json!([format!("{raw}")]))
                .await?,
        )
    }
}

impl<T: Transport> Reader for RpcReader<T> {
    async fn chain_id(&self) -> Result<u64, TxError> {
        quantity_u64(&self.ask("eth_chainId", json!([])).await?)
    }

    async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, TxError> {
        let answer = self
            .ask(
                "eth_call",
                json!([{"to": format!("{to:?}"), "data": format!("{data}")}, "latest"]),
            )
            .await?;
        let text = answer.as_str().ok_or_else(|| {
            TxError::Network("the chain answered a read with nothing".to_string())
        })?;
        Ok(Bytes::from(hex_bytes(text)?))
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
        quantity_u64(
            &self
                .ask(
                    "eth_getTransactionCount",
                    json!([format!("{who:?}"), "latest"]),
                )
                .await?,
        )
    }

    async fn chain_time(&self) -> Result<u64, TxError> {
        let block = self
            .ask("eth_getBlockByNumber", json!(["latest", false]))
            .await?;
        quantity_u64(&block["timestamp"])
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

/// A chain's public endpoint, over the browser's `fetch`.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
}

#[cfg(target_arch = "wasm32")]
impl Transport for Endpoint {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let request = gloo_net::http::Request::post(&self.url)
            .json(&body)
            .map_err(|error| RpcError::unreachable(error.to_string()))?;
        let reply = request
            .send()
            .await
            .map_err(|_| RpcError::unreachable("the chain's endpoint didn't answer"))?;
        if !reply.ok() {
            return Err(RpcError::unreachable(format!(
                "the chain's endpoint answered {}",
                reply.status()
            )));
        }
        let reply: Value = reply.json().await.map_err(|_| {
            RpcError::unreachable("the chain's endpoint answered in an unexpected shape")
        })?;
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
    fn a_receipt_is_read_with_its_status_block_and_logs() {
        let receipt = json!({
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "status": "0x1",
            "blockNumber": "0x2a",
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
        let failed = json!({"transactionHash": format!("0x{}", "11".repeat(32)), "status": "0x0", "blockNumber": "0x1", "logs": []});
        assert!(!parse_receipt(&failed).unwrap().unwrap().success);
        assert_eq!(parse_receipt(&Value::Null).unwrap(), None);
        assert_eq!(parse_receipt(&json!({"blockNumber": null})).unwrap(), None);
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

    #[test]
    fn quantities_parse_and_garbage_is_refused() {
        assert_eq!(quantity(&json!("0x0")).unwrap(), U256::ZERO);
        assert_eq!(quantity(&json!("0x")).unwrap(), U256::ZERO);
        assert_eq!(quantity_u64(&json!("0x14a34")).unwrap(), 84532);
        assert!(quantity(&json!("0xzz")).is_err());
        assert!(quantity(&json!(5)).is_err());
    }
}
