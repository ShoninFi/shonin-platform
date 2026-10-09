//! The node's JSON-RPC, spoken directly: each method the platform uses, and
//! nothing else.
//!
//! The URL may be a paid one carrying its key, so it never appears in an
//! error or a log: a failure names the chain's label instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// The commitment a payment is read at: rooted, so never rolled back.
pub const FINALIZED: &str = "finalized";

/// How many times one call is made while the node says it is asked too
/// often, before the call gives up with [`RateLimited`].
const RATE_LIMITED_TRIES: u32 = 4;

/// The first pause after the node says it is asked too often, doubled at
/// each time in a row it says so, up to [`MOST_RATE_PAUSE`].
const FIRST_RATE_PAUSE: Duration = Duration::from_millis(500);
const MOST_RATE_PAUSE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Rpc {
    http: reqwest::Client,
    url: Arc<str>,
    label: Arc<str>,
    next: Arc<AtomicU64>,
    /// When the node last asked to be asked less often: every call made
    /// through this connection (its clones too) waits until then.
    pace: Arc<Mutex<Pace>>,
}

/// The pause the node asked for, shared by every call to it.
#[derive(Debug, Default)]
struct Pace {
    /// No call goes out before this.
    until: Option<Instant>,
    /// How many times in a row the node has said it is asked too often.
    strikes: u32,
}

impl Pace {
    /// The pause after one more refusal for rate, or the one the node asked
    /// for (`Retry-After`) if it asked for longer.
    fn strike(&mut self, asked: Option<Duration>, now: Instant) -> Duration {
        self.strikes = self.strikes.saturating_add(1);
        let doubled = FIRST_RATE_PAUSE.saturating_mul(1 << (self.strikes - 1).min(10));
        let pause = doubled.max(asked.unwrap_or_default()).min(MOST_RATE_PAUSE);
        let until = now + pause;
        self.until = Some(self.until.map_or(until, |was| was.max(until)));
        pause
    }

    fn eased(&mut self) {
        self.strikes = 0;
    }

    /// How long a call must wait before it goes out.
    fn wait(&self, now: Instant) -> Duration {
        self.until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    }
}

/// The node said it is asked too often (HTTP 429, or the same as a JSON-RPC
/// error), and kept saying so through every try of the call. Nothing was
/// done: whoever made the call should slow down rather than count it as
/// something that failed.
#[derive(Clone, Debug)]
pub struct RateLimited {
    pub label: String,
    pub method: String,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} asks to be asked less often (rate-limited on {})",
            self.label, self.method
        )
    }
}

impl std::error::Error for RateLimited {}

/// Whether `error` is, or was caused by, the node limiting the rate of
/// calls: nothing failed, the caller was only asked to slow down.
pub fn rate_limited(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<RateLimited>().is_some())
}

/// What one try of a call came to.
enum Tried {
    Answered(Value),
    /// The node asked to be asked less often, saying how long to wait if it
    /// said.
    Slow(Option<Duration>),
}

/// Whether a JSON-RPC error is the node limiting the rate: the code some
/// nodes use for it (429), or the words.
fn says_rate_limited(error: &Value) -> bool {
    error["code"].as_i64() == Some(429)
        || error["message"]
            .as_str()
            .is_some_and(|message| message.to_ascii_lowercase().contains("too many requests"))
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
            pace: Arc::new(Mutex::new(Pace::default())),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    fn pace(&self) -> std::sync::MutexGuard<'_, Pace> {
        self.pace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One call, its result read as `T`. While the node says it is asked
    /// too often, the call waits and is made again, every other call through
    /// this connection waiting with it, up to [`RATE_LIMITED_TRIES`] times;
    /// then it fails with [`RateLimited`].
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        for _ in 0..RATE_LIMITED_TRIES {
            let wait = self.pace().wait(Instant::now());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            match self.try_call(method, &params).await? {
                Tried::Answered(answer) => {
                    self.pace().eased();
                    if let Some(error) = answer.get("error") {
                        return Err(RpcError {
                            code: error["code"].as_i64().unwrap_or(0),
                            message: error["message"].as_str().unwrap_or_default().to_string(),
                            data: error.get("data").cloned().unwrap_or(Value::Null),
                        }
                        .into());
                    }
                    return serde_json::from_value(
                        answer.get("result").cloned().unwrap_or(Value::Null),
                    )
                    .with_context(|| {
                        format!("{} answered {method} in an unexpected shape", self.label)
                    });
                }
                Tried::Slow(asked) => {
                    let pause = self.pace().strike(asked, Instant::now());
                    tracing::debug!(
                        node = %self.label,
                        method,
                        millis = pause.as_millis() as u64,
                        "the node asks to be asked less often; waiting"
                    );
                }
            }
        }
        Err(RateLimited {
            label: self.label.to_string(),
            method: method.to_string(),
        }
        .into())
    }

    async fn try_call(&self, method: &str, params: &Value) -> Result<Tried> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = self
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
            })?;
        let asked = reply
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let limited = reply.status() == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let answer: Option<Value> = reply.json().await.ok();
        if limited
            || answer
                .as_ref()
                .and_then(|answer| answer.get("error"))
                .is_some_and(says_rate_limited)
        {
            return Ok(Tried::Slow(asked));
        }
        answer
            .map(Tried::Answered)
            .ok_or_else(|| anyhow!("{} answered {method} with no JSON", self.label))
    }

    pub async fn genesis_hash(&self) -> Result<String> {
        self.call("getGenesisHash", json!([])).await
    }

    pub async fn slot(&self) -> Result<u64> {
        self.call("getSlot", json!([{"commitment": COMMITMENT}]))
            .await
    }

    pub async fn account(&self, address: &Pubkey) -> Result<Option<Account>> {
        self.account_at(address, COMMITMENT).await
    }

    /// An account as the node has it at `commitment` (`confirmed`, or
    /// `finalized` for what may never be rolled back).
    pub async fn account_at(&self, address: &Pubkey, commitment: &str) -> Result<Option<Account>> {
        #[derive(Deserialize)]
        struct Answer {
            value: Option<RawAccount>,
        }
        let answer: Answer = self
            .call(
                "getAccountInfo",
                json!([address.to_string(), {"encoding": "base64", "commitment": commitment}]),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A node answering each request with the next of `replies` (the last
    /// again once they run out): an HTTP status and a body. Says how many
    /// requests it was sent.
    async fn a_node(replies: Vec<(u16, &'static str)>) -> (String, Arc<AtomicU64>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(AtomicU64::new(0));
        let counting = asked.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let n = counting.fetch_add(1, Ordering::SeqCst) as usize;
                let (status, body) = replies[n.min(replies.len() - 1)];
                let mut buffer = vec![0u8; 8192];
                let _ = socket.read(&mut buffer).await;
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (url, asked)
    }

    const SLOT: &str = r#"{"jsonrpc":"2.0","id":1,"result":42}"#;
    const LIMITED: &str = r#"{"jsonrpc":"2.0","error":{"code":429,"message":"Too many requests for a specific RPC call"}}"#;

    #[tokio::test]
    async fn a_node_asking_to_be_asked_less_often_is_waited_out_and_asked_again() {
        let (url, asked) = a_node(vec![(429, LIMITED), (200, LIMITED), (200, SLOT)]).await;
        let rpc = Rpc::new(&url, "a node").unwrap();
        let started = Instant::now();
        assert_eq!(rpc.slot().await.unwrap(), 42);
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        // Half a second, then a second: the pause doubled.
        assert!(started.elapsed() >= Duration::from_millis(1_400));
        // Answered, the next call goes straight out.
        let started = Instant::now();
        assert_eq!(rpc.slot().await.unwrap(), 42);
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    #[tokio::test]
    async fn a_node_that_keeps_limiting_the_rate_is_rate_limited_not_a_failure() {
        let (url, asked) = a_node(vec![(429, "")]).await;
        let rpc = Rpc::new(&url, "a node").unwrap();
        let error = rpc.slot().await.unwrap_err();
        assert!(rate_limited(&error), "{error:#}");
        assert!(rate_limited(&error.context("a round")));
        assert_eq!(asked.load(Ordering::SeqCst), u64::from(RATE_LIMITED_TRIES));
        // Any other error the node answers is no rate limit.
        let (url, _) = a_node(vec![(
            200,
            r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"bad"}}"#,
        )])
        .await;
        let error = Rpc::new(&url, "a node").unwrap().slot().await.unwrap_err();
        assert!(!rate_limited(&error));
        assert!(error.downcast_ref::<RpcError>().is_some());
    }

    #[test]
    fn the_pause_doubles_up_to_its_most_and_honours_a_longer_retry_after() {
        let now = Instant::now();
        let mut pace = Pace::default();
        assert_eq!(pace.strike(None, now), Duration::from_millis(500));
        assert_eq!(pace.strike(None, now), Duration::from_secs(1));
        assert_eq!(
            pace.strike(Some(Duration::from_secs(7)), now),
            Duration::from_secs(7)
        );
        for _ in 0..20 {
            pace.strike(None, now);
        }
        assert_eq!(pace.strike(None, now), MOST_RATE_PAUSE);
        assert_eq!(pace.wait(now), MOST_RATE_PAUSE);
        pace.eased();
        assert_eq!(pace.strike(None, now), Duration::from_millis(500));
    }
}
