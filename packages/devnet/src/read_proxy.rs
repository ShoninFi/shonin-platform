//! A local Surfpool fork of mainnet-beta for rehearsing the Solana mainnet
//! deploy, with a read-only forwarder as its datasource.
//!
//! Surfpool fetches every account it hasn't seen from its datasource. Pointed
//! straight at a public endpoint over one long-lived connection, its fetches
//! stall part way through a deploy; through this forwarder each call is one
//! fresh request. The forwarder listens on this machine alone, passes on
//! only methods that read (`get…`, and a few more that change nothing), and
//! refuses everything else, so nothing the fork does can reach the public
//! cluster as a send.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// Mainnet-beta's public endpoint, the default datasource.
pub const PUBLIC_UPSTREAM: &str = "https://api.mainnet-beta.solana.com";

/// Methods that read and are not named `get…`.
const OTHER_READS: [&str; 2] = ["isBlockhashValid", "minimumLedgerSlot"];

/// Whether `method` only reads: `get` followed by a capital (`getAccountInfo`,
/// `getMultipleAccounts`, …), or one of the few other reads. Sends,
/// simulations, airdrops and subscriptions are refused.
pub fn is_read(method: &str) -> bool {
    OTHER_READS.contains(&method)
        || method
            .strip_prefix("get")
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_ascii_uppercase())
}

/// The methods a JSON-RPC body calls (one call or a batch); `None` if it
/// isn't JSON-RPC.
pub fn methods(body: &Value) -> Option<Vec<String>> {
    let calls: Vec<&Value> = match body {
        Value::Array(calls) if !calls.is_empty() => calls.iter().collect(),
        Value::Object(_) => vec![body],
        _ => return None,
    };
    calls
        .iter()
        .map(|call| call.get("method")?.as_str().map(str::to_string))
        .collect()
}

/// The JSON-RPC error answering every call in `body` that is refused.
fn refusal(body: &Value) -> Value {
    let error = |call: &Value| {
        json!({
            "jsonrpc": "2.0",
            "id": call.get("id").cloned().unwrap_or(Value::Null),
            "error": {"code": -32601, "message": "refused: this forwarder passes on reads alone"}
        })
    };
    match body {
        Value::Array(calls) => Value::Array(calls.iter().map(error).collect()),
        call => error(call),
    }
}

#[derive(Clone)]
struct Upstream {
    url: String,
    http: reqwest::Client,
}

async fn forward(State(upstream): State<Upstream>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    let Some(called) = methods(&parsed) else {
        return (StatusCode::BAD_REQUEST, "not JSON-RPC").into_response();
    };
    if let Some(refused) = called.iter().find(|method| !is_read(method)) {
        tracing::warn!(method = %refused, "refused: not a read");
        return ([("connection", "close")], axum::Json(refusal(&parsed))).into_response();
    }
    let started = std::time::Instant::now();
    for attempt in 1..=6 {
        let sent = upstream
            .http
            .post(&upstream.url)
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
            .await;
        match sent {
            Ok(response) if response.status().is_success() => {
                if let Ok(bytes) = response.bytes().await {
                    let took = started.elapsed();
                    if took > Duration::from_secs(2) {
                        tracing::info!(methods = %called.join(","), seconds = took.as_secs(), "a slow read");
                    }
                    return (
                        StatusCode::OK,
                        // One request per connection: Surfpool's fetches
                        // stall on a connection kept open.
                        [
                            ("content-type", "application/json"),
                            ("connection", "close"),
                        ],
                        bytes,
                    )
                        .into_response();
                }
            }
            Ok(response) => {
                tracing::warn!(status = %response.status(), attempt, methods = %called.join(","), "the datasource refused a read")
            }
            Err(error) => {
                tracing::warn!(attempt, methods = %called.join(","), timeout = error.is_timeout(), "the datasource didn't answer a read")
            }
        }
        // A public endpoint limits each address's rate: wait it out.
        tokio::time::sleep(Duration::from_secs(attempt)).await;
    }
    (
        StatusCode::BAD_GATEWAY,
        [("connection", "close")],
        "the datasource didn't answer",
    )
        .into_response()
}

/// Serve the forwarder on 127.0.0.1:`port` until the task ends.
pub async fn serve(port: u16, upstream: String) -> Result<tokio::task::JoinHandle<()>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .build()?;
    let app = Router::new()
        .route("/", post(forward))
        .with_state(Upstream {
            url: upstream,
            http,
        });
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding the forwarder to {address}"))?;
    Ok(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }))
}

/// `solana-mainnet-fork`: the forwarder on 127.0.0.1:`proxy_port`, and
/// Surfpool on 127.0.0.1:`port` (its websocket on `ws_port`) forking
/// mainnet-beta through it. Runs until Surfpool stops or the task is
/// stopped; both stop together.
pub async fn fork(port: u16, ws_port: u16, proxy_port: u16, upstream: &str) -> Result<()> {
    if !upstream.starts_with("https://") {
        bail!("the fork's datasource must be an https endpoint");
    }
    let proxy = serve(proxy_port, upstream.to_string()).await?;
    let datasource = format!("http://127.0.0.1:{proxy_port}");
    println!(
        "The read-only forwarder is on {datasource}. Surfpool forks mainnet-beta on \
         http://127.0.0.1:{port} (websocket ws://127.0.0.1:{ws_port}). Rehearse with ON_FORK=1."
    );
    let mut surfpool = tokio::process::Command::new("surfpool")
        .args(["start", "--rpc-url", &datasource])
        .args(["--host", "127.0.0.1"])
        .args(["--port", &port.to_string()])
        .args(["--ws-port", &ws_port.to_string()])
        .args([
            "--no-deploy",
            "--airdrop-amount",
            "0",
            "--no-tui",
            "--no-studio",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting Surfpool: is it installed?")?;
    // Stopped by Ctrl-C or a kill: Surfpool is stopped with it.
    let stopped = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("a signal handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    };
    let status = tokio::select! {
        status = surfpool.wait() => status?,
        _ = stopped => {
            let _ = surfpool.kill().await;
            proxy.abort();
            return Ok(());
        }
    };
    proxy.abort();
    if !status.success() {
        bail!("Surfpool stopped: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fork_s_forwarder_passes_on_reads_alone() {
        for read in [
            "getAccountInfo",
            "getMultipleAccounts",
            "getLatestBlockhash",
            "getGenesisHash",
            "getProgramAccounts",
            "isBlockhashValid",
            "minimumLedgerSlot",
        ] {
            assert!(is_read(read), "{read}");
        }
        for write in [
            "sendTransaction",
            "simulateTransaction",
            "requestAirdrop",
            "accountSubscribe",
            "get",
            "getaccountinfo",
            "",
            "surfnet_setAccount",
        ] {
            assert!(!is_read(write), "{write}");
        }
    }

    #[test]
    fn a_batch_with_any_call_that_is_not_a_read_is_refused_whole() {
        let batch = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo", "params": []},
            {"jsonrpc": "2.0", "id": 2, "method": "sendTransaction", "params": []}
        ]);
        let called = methods(&batch).unwrap();
        assert!(!called.iter().all(|m| is_read(m)));
        let answer = refusal(&batch);
        assert_eq!(answer.as_array().unwrap().len(), 2);
        assert!(answer[1]["error"].is_object());
        assert_eq!(
            methods(&json!({"jsonrpc": "2.0", "id": 1, "method": "getSlot"})).unwrap(),
            vec!["getSlot".to_string()]
        );
        // Not JSON-RPC: no method to judge.
        assert!(methods(&json!([])).is_none());
        assert!(methods(&json!([{"id": 1}])).is_none());
        assert!(methods(&json!("getSlot")).is_none());
    }
}
