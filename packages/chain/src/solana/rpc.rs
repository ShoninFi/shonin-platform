//! The node's JSON-RPC, spoken directly: each method the platform uses, and
//! nothing else.
//!
//! The URL may be a paid one carrying its key, so it never appears in an
//! error or a log: a failure names the chain's label instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Pubkey;

/// How long one call may take before it is given up on.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The commitment every read and confirmation uses: voted on by a
/// supermajority, which on a single local node is as soon as it lands.
pub const COMMITMENT: &str = "confirmed";

#[derive(Clone)]
pub struct Rpc {
    http: reqwest::Client,
    url: Arc<str>,
    label: Arc<str>,
    next: Arc<AtomicU64>,
}

/// An error the node answered with, as it said it.
#[derive(Clone, Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Value,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the node answered {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// An account as the node holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub lamports: u64,
    pub owner: Pubkey,
    pub executable: bool,
    pub data: Vec<u8>,
}

#[derive(Deserialize)]
struct RawAccount {
    lamports: u64,
    owner: String,
    executable: bool,
    data: (String, String),
}

impl RawAccount {
    fn decode(self) -> Result<Account> {
        Ok(Account {
            lamports: self.lamports,
            owner: parse_pubkey(&self.owner)?,
            executable: self.executable,
            data: base64::engine::general_purpose::STANDARD
                .decode(self.data.0)
                .context("an account's data is not base64")?,
        })
    }
}

/// One transaction's status, as `getSignatureStatuses` says it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub slot: u64,
    pub err: Option<Value>,
    pub confirmation_status: Option<String>,
}

/// One entry of an address's history, newest first.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    pub signature: String,
    pub slot: u64,
    pub err: Option<Value>,
    pub block_time: Option<i64>,
}

/// What simulating a transaction answered.
#[derive(Clone, Debug, Default)]
pub struct Simulated {
    pub err: Option<Value>,
    pub logs: Vec<String>,
    /// The data the last program to set any returned, and that program.
    pub return_data: Option<(Pubkey, Vec<u8>)>,
    pub units: u64,
    /// The accounts asked for, as the transaction left them (none where it
    /// left no account there).
    pub accounts: Vec<Option<Account>>,
}

impl Rpc {
    pub fn new(url: &str, label: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            url: url.into(),
            label: label.into(),
            next: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// One call, its result read as `T`.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer: Value = self
            .http
            .post(&*self.url)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                anyhow!(
                    "{} did not answer {method}: {}",
                    self.label,
                    error.without_url()
                )
            })?
            .json()
            .await
            .map_err(|error| {
                anyhow!(
                    "{} answered {method} with no JSON: {}",
                    self.label,
                    error.without_url()
                )
            })?;
        if let Some(error) = answer.get("error") {
            return Err(RpcError {
                code: error["code"].as_i64().unwrap_or(0),
                message: error["message"].as_str().unwrap_or_default().to_string(),
                data: error.get("data").cloned().unwrap_or(Value::Null),
            }
            .into());
        }
        serde_json::from_value(answer.get("result").cloned().unwrap_or(Value::Null))
            .with_context(|| format!("{} answered {method} in an unexpected shape", self.label))
    }

    pub async fn genesis_hash(&self) -> Result<String> {
        self.call("getGenesisHash", json!([])).await
    }

    pub async fn slot(&self) -> Result<u64> {
        self.call("getSlot", json!([{"commitment": COMMITMENT}]))
            .await
    }

    pub async fn account(&self, address: &Pubkey) -> Result<Option<Account>> {
        #[derive(Deserialize)]
        struct Answer {
            value: Option<RawAccount>,
        }
        let answer: Answer = self
            .call(
                "getAccountInfo",
                json!([address.to_string(), {"encoding": "base64", "commitment": COMMITMENT}]),
            )
            .await?;
        answer.value.map(RawAccount::decode).transpose()
    }

    /// Many accounts at once, in the order asked, a hundred a call.
    pub async fn accounts(&self, addresses: &[Pubkey]) -> Result<Vec<Option<Account>>> {
        #[derive(Deserialize)]
        struct Answer {
            value: Vec<Option<RawAccount>>,
        }
        let mut out = Vec::with_capacity(addresses.len());
        for chunk in addresses.chunks(100) {
            let keys: Vec<String> = chunk.iter().map(ToString::to_string).collect();
            let answer: Answer = self
                .call(
                    "getMultipleAccounts",
                    json!([keys, {"encoding": "base64", "commitment": COMMITMENT}]),
                )
                .await?;
            for account in answer.value {
                out.push(account.map(RawAccount::decode).transpose()?);
            }
        }
        Ok(out)
    }

    /// Every account `program` owns whose data holds each `(offset, bytes)`.
    pub async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Result<Vec<(Pubkey, Account)>> {
        #[derive(Deserialize)]
        struct Keyed {
            pubkey: String,
            account: RawAccount,
        }
        let filters: Vec<Value> = filters
            .iter()
            .map(|(offset, bytes)| {
                json!({"memcmp": {"offset": offset, "bytes": bs58::encode(bytes).into_string()}})
            })
            .collect();
        let answer: Vec<Keyed> = self
            .call(
                "getProgramAccounts",
                json!([program.to_string(), {
                    "encoding": "base64",
                    "commitment": COMMITMENT,
                    "filters": filters,
                }]),
            )
            .await?;
        answer
            .into_iter()
            .map(|keyed| Ok((parse_pubkey(&keyed.pubkey)?, keyed.account.decode()?)))
            .collect()
    }

    pub async fn balance(&self, address: &Pubkey) -> Result<u64> {
        #[derive(Deserialize)]
        struct Answer {
            value: u64,
        }
        let answer: Answer = self
            .call(
                "getBalance",
                json!([address.to_string(), {"commitment": COMMITMENT}]),
            )
            .await?;
        Ok(answer.value)
    }

    pub async fn rent_exempt_minimum(&self, size: usize) -> Result<u64> {
        self.call("getMinimumBalanceForRentExemption", json!([size]))
            .await
    }

    pub async fn latest_blockhash(&self) -> Result<(String, u64)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Value {
            blockhash: String,
            last_valid_block_height: u64,
        }
        #[derive(Deserialize)]
        struct Answer {
            value: Value,
        }
        let answer: Answer = self
            .call("getLatestBlockhash", json!([{"commitment": COMMITMENT}]))
            .await?;
        Ok((answer.value.blockhash, answer.value.last_valid_block_height))
    }

    pub async fn block_height(&self) -> Result<u64> {
        self.call("getBlockHeight", json!([{"commitment": COMMITMENT}]))
            .await
    }

    /// Hand a signed transaction to the node, after it has simulated it: a
    /// transaction that would fail is refused here, with its logs.
    pub async fn send(&self, wire: &[u8]) -> Result<String> {
        self.call(
            "sendTransaction",
            json!([
                base64::engine::general_purpose::STANDARD.encode(wire),
                {
                    "encoding": "base64",
                    "skipPreflight": false,
                    "preflightCommitment": COMMITMENT,
                    "maxRetries": 0,
                }
            ]),
        )
        .await
    }

    pub async fn statuses(&self, signatures: &[String]) -> Result<Vec<Option<Status>>> {
        #[derive(Deserialize)]
        struct Answer {
            value: Vec<Option<Status>>,
        }
        let answer: Answer = self
            .call(
                "getSignatureStatuses",
                json!([signatures, {"searchTransactionHistory": true}]),
            )
            .await?;
        Ok(answer.value)
    }

    /// A landed transaction, whole, as the node keeps it (`json` encoding).
    pub async fn transaction(&self, signature: &str) -> Result<Option<Value>> {
        self.call(
            "getTransaction",
            json!([signature, {
                "encoding": "json",
                "commitment": COMMITMENT,
                "maxSupportedTransactionVersion": 0,
            }]),
        )
        .await
    }

    /// An address's history, newest first: up to `limit` entries before
    /// `before` (exclusive) and after `until` (exclusive).
    pub async fn signatures_for(
        &self,
        address: &Pubkey,
        limit: usize,
        before: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<SignatureInfo>> {
        let mut options = json!({"limit": limit, "commitment": COMMITMENT});
        if let Some(before) = before {
            options["before"] = json!(before);
        }
        if let Some(until) = until {
            options["until"] = json!(until);
        }
        self.call(
            "getSignaturesForAddress",
            json!([address.to_string(), options]),
        )
        .await
    }

    /// Run a transaction without keeping anything, signatures unchecked and
    /// its blockhash replaced: for what it returns and logs.
    pub async fn simulate(&self, wire: &[u8]) -> Result<Simulated> {
        self.simulate_reading(wire, &[]).await
    }

    /// The same, and `addresses` as the transaction would leave them.
    pub async fn simulate_reading(&self, wire: &[u8], addresses: &[Pubkey]) -> Result<Simulated> {
        let mut options = json!({
            "encoding": "base64",
            "sigVerify": false,
            "replaceRecentBlockhash": true,
            "commitment": COMMITMENT,
        });
        if !addresses.is_empty() {
            options["accounts"] = json!({
                "encoding": "base64",
                "addresses": addresses.iter().map(Pubkey::to_string).collect::<Vec<_>>(),
            });
        }
        let answer: Value = self
            .call(
                "simulateTransaction",
                json!([
                    base64::engine::general_purpose::STANDARD.encode(wire),
                    options
                ]),
            )
            .await?;
        let value = &answer["value"];
        let logs = value["logs"]
            .as_array()
            .map(|logs| {
                logs.iter()
                    .filter_map(|line| line.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let return_data = match value.get("returnData") {
            Some(data) if !data.is_null() => {
                let program = parse_pubkey(data["programId"].as_str().unwrap_or_default())?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data["data"][0].as_str().unwrap_or_default())
                    .context("return data is not base64")?;
                Some((program, bytes))
            }
            _ => None,
        };
        let mut accounts = Vec::new();
        for account in value["accounts"].as_array().into_iter().flatten() {
            accounts.push(if account.is_null() {
                None
            } else {
                Some(
                    serde_json::from_value::<RawAccount>(account.clone())
                        .context("a simulated account that doesn't read as one")?
                        .decode()?,
                )
            });
        }
        Ok(Simulated {
            err: value.get("err").cloned().filter(|err| !err.is_null()),
            logs,
            return_data,
            units: value["unitsConsumed"].as_u64().unwrap_or(0),
            accounts,
        })
    }

    /// Coin from the node's own faucet: development clusters only.
    pub async fn airdrop(&self, to: &Pubkey, lamports: u64) -> Result<String> {
        self.call(
            "requestAirdrop",
            json!([to.to_string(), lamports, {"commitment": COMMITMENT}]),
        )
        .await
    }

    /// Move a Surfpool cluster's clock to `unix_seconds`. It only moves
    /// forward, and runs on at real pace from there.
    pub async fn time_travel(&self, unix_seconds: u64) -> Result<()> {
        let _: Value = self
            .call(
                "surfnet_timeTravel",
                json!([{"absoluteTimestamp": unix_seconds * 1_000}]),
            )
            .await?;
        Ok(())
    }
}

pub fn parse_pubkey(text: &str) -> Result<Pubkey> {
    text.parse()
        .map_err(|_| anyhow!("{text:?} is not a Solana address"))
}
