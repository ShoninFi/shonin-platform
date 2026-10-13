//! A JSON-RPC proxy between a keeper and its anvil: it passes everything
//! through and counts each method, and its knobs, which may be turned while
//! a run goes on, make the node misbehave the ways an endpoint does: a head
//! that lags, a slow or failing `eth_call`, a receipt that comes late, a log
//! that goes missing.
//!
//! On a local chain the node signs the keeper's sends, and anvil gives a
//! send without a gas limit the block's whole limit rather than estimating
//! it, so a send the contract refuses is mined and reverts, a block each. A
//! keeper on a public chain signs its own sends and estimates each first,
//! and one whose estimate reverts never goes out. The proxy does what that
//! client does: it estimates a send that names no gas limit, answers the
//! estimate's refusal as the send's, and otherwise sends it with the
//! estimate. It also passes the keeper's sends to the node one at a time:
//! anvil numbers a send itself, and two arriving together (a round's and the
//! keys task's) can be given one number, one of them then turned away, where
//! a keeper signing its own numbers each in turn.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

/// Which calls a knob applies to: every one, or only those to a contract,
/// a function, or both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calls {
    pub to: Option<Address>,
    pub selector: Option<[u8; 4]>,
}

impl Calls {
    fn matches(&self, params: &Value) -> bool {
        let tx = &params[0];
        let to: Option<Address> = tx["to"].as_str().and_then(|to| to.parse().ok());
        let data = tx
            .get("input")
            .or_else(|| tx.get("data"))
            .and_then(Value::as_str)
            .unwrap_or("0x");
        let selector = alloy::hex::decode(data).ok().and_then(|bytes| {
            bytes
                .get(..4)
                .map(|four| [four[0], four[1], four[2], four[3]])
        });
        self.to.is_none_or(|wanted| to == Some(wanted))
            && self.selector.is_none_or(|wanted| selector == Some(wanted))
    }
}

/// How the proxy misbehaves now. Every knob is off by default.
#[derive(Clone, Debug, Default)]
pub struct Knobs {
    /// Answer as a node this many blocks behind the head would: the head it
    /// reports, the latest block, and reads at the latest block.
    pub head_lag_blocks: u64,
    /// Hold these `eth_call`s back this long before passing them on.
    pub call_delay: Option<(Calls, Duration)>,
    /// Fail these `eth_call`s with a node error (not a revert).
    pub call_failure: Option<Calls>,
    /// Answer a transaction's receipt as not there yet until this long after
    /// it was first asked for.
    pub receipt_delay: Duration,
    /// Leave out of `eth_getLogs` answers the logs of this contract (any,
    /// when `None`) with this first topic (any, when `None`).
    pub dropped_logs: Option<(Option<Address>, Option<B256>)>,
    /// Fail every read of a block header by its number (the head's still
    /// answers): a log page's ends can't be checked.
    pub failed_headers: bool,
}

#[derive(Default)]
struct Seen {
    /// Requests passed on, by method.
    counts: BTreeMap<String, u64>,
    /// When each transaction's receipt was first asked for.
    receipts_asked: HashMap<String, Instant>,
}

/// The proxy's handle: its URL, its knobs and its counts.
#[derive(Clone)]
pub struct Proxy {
    pub url: String,
    knobs: Arc<Mutex<Knobs>>,
    seen: Arc<Mutex<Seen>>,
}

struct Shared {
    upstream: String,
    /// Held while a send is estimated and passed on.
    sending: tokio::sync::Mutex<()>,
    http: reqwest::Client,
    knobs: Arc<Mutex<Knobs>>,
    seen: Arc<Mutex<Seen>>,
}

impl Proxy {
    /// Start a proxy in front of `upstream`, every knob off.
    pub async fn start(upstream: &str) -> Proxy {
        let knobs = Arc::new(Mutex::new(Knobs::default()));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = Arc::new(Shared {
            upstream: upstream.to_string(),
            sending: tokio::sync::Mutex::new(()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("an HTTP client"),
            knobs: knobs.clone(),
            seen: seen.clone(),
        });
        let app = Router::new().route("/", post(handle)).with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port for the proxy");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Proxy { url, knobs, seen }
    }

    /// Turn the knobs, now: requests that arrive from here on see them.
    pub fn set(&self, change: impl FnOnce(&mut Knobs)) {
        change(&mut self.knobs.lock().expect("the knobs"));
    }

    /// Requests passed on so far, by method.
    pub fn counts(&self) -> BTreeMap<String, u64> {
        self.seen.lock().expect("the counts").counts.clone()
    }
}

async fn handle(State(shared): State<Arc<Shared>>, Json(body): Json<Value>) -> Json<Value> {
    Json(match body {
        Value::Array(batch) => {
            let mut answers = Vec::new();
            for one in batch {
                answers.push(one_request(&shared, one).await);
            }
            Value::Array(answers)
        }
        one => one_request(&shared, one).await,
    })
}

async fn one_request(shared: &Shared, request: Value) -> Value {
    if request["method"] == json!("eth_sendTransaction") {
        let _one_at_a_time = shared.sending.lock().await;
        return one_answer(shared, request).await;
    }
    one_answer(shared, request).await
}

async fn one_answer(shared: &Shared, mut request: Value) -> Value {
    let knobs = shared.knobs.lock().expect("the knobs").clone();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let id = request["id"].clone();
    if let Ok(mut seen) = shared.seen.lock() {
        *seen.counts.entry(method.clone()).or_default() += 1;
    }
    let refuse = |message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}});
    match method.as_str() {
        "eth_sendTransaction" if request["params"][0].get("gas").is_none() => {
            let mut tx = request["params"][0].clone();
            if let Some(fields) = tx.as_object_mut() {
                fields.remove("nonce");
            }
            let estimate =
                json!({"jsonrpc": "2.0", "id": 0, "method": "eth_estimateGas", "params": [tx]});
            match forward(shared, &estimate).await {
                Ok(answer) if answer.get("error").is_some() => {
                    return json!({"jsonrpc": "2.0", "id": id, "error": answer["error"]});
                }
                Ok(answer) => request["params"][0]["gas"] = answer["result"].clone(),
                Err(error) => {
                    return refuse(&format!("the proxy could not reach the node: {error}"));
                }
            }
        }
        "eth_call" | "eth_estimateGas" => {
            if method == "eth_call" {
                if let Some(calls) = &knobs.call_failure
                    && calls.matches(&request["params"])
                {
                    return refuse("the proxy failed this call");
                }
                if let Some((calls, delay)) = &knobs.call_delay
                    && calls.matches(&request["params"])
                {
                    tokio::time::sleep(*delay).await;
                }
            }
            let block_at = 1;
            let tag = request["params"].get(block_at).cloned();
            if knobs.head_lag_blocks > 0
                && (tag.is_none() || tag == Some(json!("latest")))
                && let Some(lagged) = lagged_head(shared, knobs.head_lag_blocks).await
                && let Some(params) = request["params"].as_array_mut()
            {
                if params.len() <= block_at {
                    params.push(json!(lagged));
                } else {
                    params[block_at] = json!(lagged);
                }
            }
        }
        "eth_blockNumber" if knobs.head_lag_blocks > 0 => {
            if let Some(lagged) = lagged_head(shared, knobs.head_lag_blocks).await {
                return json!({"jsonrpc": "2.0", "id": id, "result": lagged});
            }
        }
        "eth_getBlockByNumber"
            if knobs.failed_headers
                && request["params"][0]
                    .as_str()
                    .is_some_and(|tag| tag.starts_with("0x")) =>
        {
            return refuse("the proxy failed this header read");
        }
        "eth_getBlockByNumber" if knobs.head_lag_blocks > 0 => {
            if request["params"][0] == json!("latest")
                && let Some(lagged) = lagged_head(shared, knobs.head_lag_blocks).await
            {
                request["params"][0] = json!(lagged);
            }
        }
        "eth_getTransactionReceipt" if !knobs.receipt_delay.is_zero() => {
            let hash = request["params"][0]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let first = shared
                .seen
                .lock()
                .map(|mut seen| *seen.receipts_asked.entry(hash).or_insert_with(Instant::now))
                .unwrap_or_else(|_| Instant::now());
            if first.elapsed() < knobs.receipt_delay {
                return json!({"jsonrpc": "2.0", "id": id, "result": null});
            }
        }
        _ => {}
    }
    let mut answer = match forward(shared, &request).await {
        Ok(answer) => answer,
        Err(error) => return refuse(&format!("the proxy could not reach the node: {error}")),
    };
    if method == "eth_getLogs"
        && let Some((address, topic)) = &knobs.dropped_logs
        && let Some(logs) = answer["result"].as_array_mut()
    {
        logs.retain(|log| {
            let from: Option<Address> = log["address"].as_str().and_then(|a| a.parse().ok());
            let first: Option<B256> = log["topics"][0].as_str().and_then(|t| t.parse().ok());
            let dropped =
                address.is_none_or(|a| from == Some(a)) && topic.is_none_or(|t| first == Some(t));
            !dropped
        });
    }
    answer
}

/// The head less `lag` blocks, as a hex quantity.
async fn lagged_head(shared: &Shared, lag: u64) -> Option<String> {
    let request = json!({"jsonrpc": "2.0", "id": 0, "method": "eth_blockNumber", "params": []});
    let answer = forward(shared, &request).await.ok()?;
    let head = u64::from_str_radix(answer["result"].as_str()?.trim_start_matches("0x"), 16).ok()?;
    Some(format!("0x{:x}", head.saturating_sub(lag)))
}

async fn forward(shared: &Shared, request: &Value) -> Result<Value, reqwest::Error> {
    shared
        .http
        .post(&shared.upstream)
        .json(request)
        .send()
        .await?
        .json()
        .await
}
