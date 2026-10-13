//! Local chains for the differential runs: anvil processes mining at a set
//! block time on the wall clock, and plain JSON-RPC to them.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, Bytes, U256};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// A block time a run is made at, and the local chain whose defaults apply
/// to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockTime {
    /// Base's stand-in, `evm:31337`.
    TwoSeconds,
    /// Hyperliquid's stand-in, `evm:31338`.
    OneSecond,
    /// Robinhood Chain's stand-in, `evm:31339`.
    QuarterSecond,
}

impl BlockTime {
    pub fn chain_id(self) -> u64 {
        match self {
            BlockTime::TwoSeconds => 31337,
            BlockTime::OneSecond => 31338,
            BlockTime::QuarterSecond => 31339,
        }
    }

    /// The vault chain `--vault-chain` names for it.
    pub fn vault_chain(self) -> &'static str {
        match self {
            BlockTime::TwoSeconds => "base",
            BlockTime::OneSecond => "hyperliquid",
            BlockTime::QuarterSecond => "robinhood",
        }
    }

    pub fn millis(self) -> u64 {
        match self {
            BlockTime::TwoSeconds => 2_000,
            BlockTime::OneSecond => 1_000,
            BlockTime::QuarterSecond => 250,
        }
    }

    /// The block interval the keeper measures from two blocks' whole-second
    /// times: never below a second.
    pub fn measured_seconds(self) -> u64 {
        (self.millis() / 1_000).max(1)
    }

    fn anvil_arg(self) -> &'static str {
        match self {
            BlockTime::TwoSeconds => "2",
            BlockTime::OneSecond => "1",
            BlockTime::QuarterSecond => "0.25",
        }
    }
}

/// An anvil process, killed when dropped.
pub struct Anvil {
    child: Child,
    pub url: String,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Anvil {
    /// Start an anvil mining every `block_time` from now, with the chain id
    /// of that block time's chain. Every anvil started so has the same
    /// accounts and the same genesis but its time, which is `genesis` when
    /// given (anvil then keeps its clock that far from the wall). A block's
    /// time is the one before it and the block time, so a pause in mining, or
    /// a return to an earlier block, leaves the chain's clock behind the
    /// wall for good.
    pub fn start(block_time: BlockTime, genesis: Option<u64>) -> Result<Anvil> {
        let port = free_port();
        let mut command = Command::new("anvil");
        if let Some(genesis) = genesis {
            command.args(["--timestamp", &genesis.to_string()]);
        }
        let child = command
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--chain-id", &block_time.chain_id().to_string()])
            .args(["--block-time", block_time.anvil_arg()])
            .args(["--accounts", "10", "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting anvil")?;
        Ok(Anvil {
            child,
            url: format!("http://127.0.0.1:{port}"),
        })
    }

    pub fn rpc(&self) -> Rpc {
        Rpc::new(&self.url)
    }
}

pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("its address").port()
}

/// Plain JSON-RPC to one node.
#[derive(Clone)]
pub struct Rpc {
    http: reqwest::Client,
    url: String,
}

/// One transaction as a block lists it, with its receipt's status.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MinedTx {
    pub hash: B256,
    pub from: Address,
    pub to: Option<Address>,
    pub input: Bytes,
    pub value: U256,
    pub block: u64,
    pub timestamp: u64,
    pub index: u64,
    pub status: bool,
}

impl Rpc {
    pub fn new(url: &str) -> Rpc {
        Rpc {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("an HTTP client"),
            url: url.to_string(),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let reply: Value = self
            .http
            .post(&self.url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .with_context(|| format!("asking {method}"))?
            .json()
            .await
            .with_context(|| format!("reading the answer to {method}"))?;
        if let Some(error) = reply.get("error") {
            bail!("{method}: {error}");
        }
        Ok(reply["result"].clone())
    }

    /// Wait for the node to answer, for at most `limit`.
    pub async fn wait_up(&self, limit: Duration) -> Result<()> {
        let started = Instant::now();
        loop {
            if self.call("eth_chainId", json!([])).await.is_ok() {
                return Ok(());
            }
            if started.elapsed() > limit {
                bail!("the node did not answer within {limit:?}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn accounts(&self) -> Result<Vec<Address>> {
        Ok(serde_json::from_value(
            self.call("eth_accounts", json!([])).await?,
        )?)
    }

    pub async fn head(&self) -> Result<u64> {
        quantity(&self.call("eth_blockNumber", json!([])).await?)
    }

    /// A block's number and time; `None` for the latest.
    pub async fn block_time(&self, number: Option<u64>) -> Result<(u64, u64)> {
        let tag = number.map_or("latest".to_string(), |n| format!("0x{n:x}"));
        let block = self
            .call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        Ok((quantity(&block["number"])?, quantity(&block["timestamp"])?))
    }

    pub async fn nonce(&self, of: Address) -> Result<u64> {
        quantity(
            &self
                .call("eth_getTransactionCount", json!([of, "latest"]))
                .await?,
        )
    }

    /// Send a transaction the node signs, numbered `nonce`, and say its
    /// hash. Numbered here: a node left to number a burst of sends can give
    /// two the same number when it mines a block meanwhile, and drop one.
    /// Its fee cap is far above the base fee, so a run of full setup blocks,
    /// which raise the base fee, never strands one.
    pub async fn send(
        &self,
        from: Address,
        nonce: u64,
        to: Option<Address>,
        data: &[u8],
        gas: u64,
    ) -> Result<B256> {
        let mut tx = json!({
            "from": from,
            "nonce": format!("0x{nonce:x}"),
            "input": Bytes::copy_from_slice(data),
            "gas": format!("0x{gas:x}"),
            "maxFeePerGas": "0x174876e800",
            "maxPriorityFeePerGas": "0x3b9aca00",
        });
        if let Some(to) = to {
            tx["to"] = json!(to);
        }
        Ok(serde_json::from_value(
            self.call("eth_sendTransaction", json!([tx])).await?,
        )?)
    }

    /// Wait for a transaction's receipt; the receipt.
    pub async fn receipt(&self, hash: B256, limit: Duration) -> Result<Value> {
        let started = Instant::now();
        loop {
            let receipt = self
                .call("eth_getTransactionReceipt", json!([hash]))
                .await?;
            if !receipt.is_null() {
                return Ok(receipt);
            }
            if started.elapsed() > limit {
                bail!("no receipt for {hash} within {limit:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Every transaction `from` sent in blocks `first..=last`, in chain
    /// order.
    pub async fn sent_by(&self, from: Address, first: u64, last: u64) -> Result<Vec<MinedTx>> {
        let mut sent = Vec::new();
        for number in first..=last {
            let block = self
                .call(
                    "eth_getBlockByNumber",
                    json!([format!("0x{number:x}"), true]),
                )
                .await?;
            let timestamp = quantity(&block["timestamp"])?;
            for tx in block["transactions"].as_array().into_iter().flatten() {
                let sender: Address = serde_json::from_value(tx["from"].clone())?;
                if sender != from {
                    continue;
                }
                let hash: B256 = serde_json::from_value(tx["hash"].clone())?;
                let receipt = self.receipt(hash, Duration::from_secs(5)).await?;
                sent.push(MinedTx {
                    hash,
                    from: sender,
                    to: serde_json::from_value(tx["to"].clone()).unwrap_or(None),
                    input: serde_json::from_value(tx["input"].clone())?,
                    value: serde_json::from_value(tx["value"].clone())?,
                    block: number,
                    timestamp,
                    index: quantity(&tx["transactionIndex"])?,
                    status: quantity(&receipt["status"])? == 1,
                });
            }
        }
        Ok(sent)
    }
}

pub fn quantity(value: &Value) -> Result<u64> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("not a quantity: {value}"))?;
    u64::from_str_radix(text.trim_start_matches("0x"), 16)
        .with_context(|| format!("not a quantity: {text}"))
}

/// Now, in whole seconds of the wall clock: what anvil stamps its blocks
/// with.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
}

/// Sleep until the wall clock reaches `at`, in seconds since 1970.
pub async fn sleep_until_unix(at: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970");
    let target = Duration::from_secs(at);
    if target > now {
        tokio::time::sleep(target - now).await;
    }
}
