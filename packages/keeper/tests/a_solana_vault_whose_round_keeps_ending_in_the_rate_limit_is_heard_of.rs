//! A Solana vault whose own round keeps failing is in the keeper's health,
//! whether the node answers its reads with an error or with its rate limit:
//! a vault left round after round, its valuations, claims and bill waiting,
//! is said either way, after as many rounds.
//!
//! A stand-in cluster answers the vault scan (the factory, one open vault,
//! no claims) and answers the vault round's first read, the cluster's clock,
//! with an ordinary error in one test and with 429 in the other. The keeper
//! runs as a process against it.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chain::solana::Pubkey;
use chain::solana::programs::anchor_lang::{
    AccountDeserialize, AccountSerialize, Discriminator, Space,
};
use chain::solana::programs::vault::state::{self as vs, Factory, Vault, VaultState};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLOCK: &str = "SysvarC1ock11111111111111111111111111111111";

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn zeroed<T: AccountDeserialize + Discriminator>(len: usize) -> T {
    let mut bytes = vec![0u8; 8 + len];
    bytes[..8].copy_from_slice(T::DISCRIMINATOR);
    T::try_deserialize(&mut &bytes[..]).expect("zeroed account")
}

fn encode<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    out
}

fn raw(owner: &str, data: &[u8]) -> Value {
    json!({
        "lamports": 1_000_000u64, "owner": owner, "executable": false,
        "data": [b64(data), "base64"], "rentEpoch": 0,
    })
}

struct Cluster {
    factory: Vec<u8>,
    vault: (Pubkey, Vec<u8>),
    /// Whether the vault's own reads are refused for rate (429) rather than
    /// with an ordinary error.
    limited: bool,
    clock_reads: AtomicU64,
    scans: AtomicU64,
}

impl Cluster {
    /// The answer: `Ok(result)`, or `Err((http status, error))`.
    fn answer(&self, method: &str, params: &Value) -> Result<Value, (u16, Value)> {
        let program = chain::solana::programs::vault::ID.to_string();
        match method {
            "getGenesisHash" => Ok(json!("stand-in-genesis")),
            "getAccountInfo" if params[0].as_str() == Some(CLOCK) => {
                self.clock_reads.fetch_add(1, Ordering::SeqCst);
                if self.limited {
                    Err((
                        429,
                        json!({"code": 429, "message": "Too many requests for a specific RPC call"}),
                    ))
                } else {
                    Err((200, json!({"code": -32000, "message": "the node failed"})))
                }
            }
            "getAccountInfo" if params[0].as_str() == Some(&vs::factory_address().to_string()) => {
                Ok(json!({"context": {"slot": 1}, "value": raw(&program, &self.factory)}))
            }
            "getAccountInfo" => Ok(json!({"context": {"slot": 1}, "value": null})),
            "getProgramAccounts" => {
                self.scans.fetch_add(1, Ordering::SeqCst);
                let filter = params[1]["filters"][0]["memcmp"]["bytes"]
                    .as_str()
                    .unwrap_or_default();
                if filter == bs58::encode(Vault::DISCRIMINATOR).into_string() {
                    Ok(
                        json!([{"pubkey": self.vault.0.to_string(), "account": raw(&program, &self.vault.1)}]),
                    )
                } else {
                    Ok(json!([]))
                }
            }
            "getMultipleAccounts" => {
                self.scans.fetch_add(1, Ordering::SeqCst);
                let values: Vec<Value> = params[0]
                    .as_array()
                    .map(|all| {
                        all.iter()
                            .map(|address| {
                                let address = address.as_str().unwrap_or_default();
                                if address == vs::factory_address().to_string() {
                                    raw(&program, &self.factory)
                                } else if address == self.vault.0.to_string() {
                                    raw(&program, &self.vault.1)
                                } else {
                                    Value::Null
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({"context": {"slot": 2}, "value": values}))
            }
            "getBalance" => Ok(json!({"context": {"slot": 1}, "value": 10_000_000_000u64})),
            _ => Err((
                200,
                json!({"code": -32601, "message": format!("not here: {method}")}),
            )),
        }
    }
}

async fn serve(cluster: Arc<Cluster>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let cluster = cluster.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 65536];
                let body = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buffer).to_string();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let length = text[..split]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buffer.len() >= split + 4 + length {
                            break buffer[split + 4..split + 4 + length].to_vec();
                        }
                    }
                };
                let Ok(request) = serde_json::from_slice::<Value>(&body) else {
                    return;
                };
                let id = request["id"].clone();
                let (status, reply) = match cluster.answer(
                    request["method"].as_str().unwrap_or_default(),
                    &request["params"],
                ) {
                    Ok(result) => (200, json!({"jsonrpc": "2.0", "id": id, "result": result})),
                    Err((status, error)) => {
                        (status, json!({"jsonrpc": "2.0", "id": id, "error": error}))
                    }
                };
                let reply = reply.to_string();
                let http = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(http.as_bytes()).await;
            });
        }
    });
    url
}

struct Keeper {
    child: std::process::Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Keeper {
    async fn health(&self) -> Option<(u16, String)> {
        let reply = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/health", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let status = reply.status().as_u16();
        Some((status, reply.text().await.unwrap_or_default()))
    }
}

fn start(test: &str, rpc: &str) -> Keeper {
    let dir = std::env::temp_dir().join(format!(
        "keeper-rate-limited-vault-{test}-{}-{}",
        std::process::id(),
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"solana:localnet\"]\nlabel = \"stand-in cluster\"\nrpc = \"{rpc}\"\ndollar = \"USDC\"\n\n[networks.local]\nsolana = \"solana:localnet\"\n"
        ),
    )
    .unwrap();
    let anyone = Pubkey::default().to_string();
    let keeper =
        chain::solana::keys::RoleKeys::from_mnemonic(chain::solana::keys::DEVELOPMENT_MNEMONIC)
            .unwrap()
            .address(chain::Role::Keeper)
            .to_string();
    std::fs::write(
        dir.join("solana-localnet.json"),
        json!({
            "chain_key": "solana:localnet", "cluster": "localnet", "programs": {}, "mint": anyone,
            "roles": {
                "deployer": anyone, "admin": anyone, "platform": anyone, "attestor": anyone,
                "builder": anyone, "reviewer": anyone, "executor": anyone, "keeper": keeper,
                "operator": anyone, "author": anyone, "viewer": anyone,
            },
        })
        .to_string(),
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = Command::new(env!("CARGO_BIN_EXE_keeper"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "info")
        .args(["--network", "local", "--vault-chain", "solana"])
        .arg("--chains")
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(&dir)
        .arg("--store")
        .arg(&dir)
        .args([
            "--api-url",
            "http://127.0.0.1:1",
            "--executor-url",
            "http://127.0.0.1:1",
        ])
        .args(["--port", &port.to_string()])
        .stdout(std::fs::File::create(dir.join("keeper.log")).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Keeper { child, port, dir }
}

/// Whether, within `limit`, the keeper's health named the vault as not kept;
/// how many times the vault's round read the clock meanwhile; the health
/// last read; and the keeper's log, read once `linger` has passed after.
async fn vault_named_not_kept(
    limited: bool,
    limit: Duration,
    linger: Duration,
) -> (bool, u64, Option<(u16, String)>, String) {
    let strategy_id = 3;
    let vault_address = vs::vault_address(strategy_id).0;
    let mut factory: Factory = zeroed(Factory::INIT_SPACE);
    factory.vault_count = 1;
    let mut vault: Vault = zeroed(Vault::INIT_SPACE);
    vault.strategy_id = strategy_id;
    vault.state = VaultState::Open;
    let cluster = Arc::new(Cluster {
        factory: encode(&factory),
        vault: (vault_address, encode(&vault)),
        limited,
        clock_reads: AtomicU64::new(0),
        scans: AtomicU64::new(0),
    });
    let rpc = serve(cluster.clone()).await;
    let keeper = start(if limited { "limited" } else { "failing" }, &rpc);
    let named = AtomicBool::new(false);
    let started = Instant::now();
    let mut last = None;
    while started.elapsed() < limit {
        last = keeper.health().await;
        if last
            .as_ref()
            .is_some_and(|(_, said)| said.contains("vaults not kept"))
        {
            named.store(true, Ordering::SeqCst);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tokio::time::sleep(linger).await;
    let said = std::fs::read_to_string(keeper.dir.join("keeper.log")).unwrap_or_default();
    (
        named.load(Ordering::SeqCst),
        cluster.clock_reads.load(Ordering::SeqCst),
        last,
        said,
    )
}

/// A vault whose round fails on an ordinary error is named.
#[tokio::test(flavor = "multi_thread")]
async fn a_vault_whose_round_keeps_failing_is_named_in_the_health() {
    let (named, reads, last, _) =
        vault_named_not_kept(false, Duration::from_secs(90), Duration::ZERO).await;
    assert!(reads > 0, "the vault's round never ran: {last:?}");
    assert!(named, "after {reads} clock reads the health was {last:?}");
}

/// A vault whose round ends in the node's rate limit every time is named
/// as well, with why.
#[tokio::test(flavor = "multi_thread")]
async fn a_vault_whose_round_keeps_ending_in_the_rate_limit_is_named_in_the_health() {
    let (named, reads, last, _) =
        vault_named_not_kept(true, Duration::from_secs(90), Duration::ZERO).await;
    assert!(reads > 0, "the vault's round never ran: {last:?}");
    assert!(
        named,
        "every round of the vault ended in the node's rate limit ({reads} clock reads \
         refused) and the health was {last:?}"
    );
    let (status, said) = last.expect("the health answered");
    assert_eq!(status, 503, "{said}");
    assert!(said.contains("the node's rate limit"), "{said}");
}

/// A vault whose round waits on the node's rate limit says so at info, with
/// why, at once and then no more than once in a few minutes, however many
/// rounds it waits: a wait is never silent, and never a flood.
#[tokio::test(flavor = "multi_thread")]
async fn a_vault_whose_round_waits_on_the_rate_limit_says_why_at_info_once_per_throttle() {
    let (named, reads, last, said) =
        vault_named_not_kept(true, Duration::from_secs(90), Duration::from_secs(10)).await;
    assert!(named, "the health was {last:?}");
    assert!(reads >= 4, "the vault's round waited only {reads} times");
    let waits: Vec<&str> = said
        .lines()
        .filter(|line| line.contains("a due duty waits"))
        .collect();
    let limited: Vec<&&str> = waits
        .iter()
        .filter(|line| line.contains("the node limits the rate"))
        .collect();
    assert_eq!(
        limited.len(),
        1,
        "after {reads} rounds that waited on the rate limit, it was said {} times:\n{said}",
        limited.len()
    );
    assert!(
        limited[0].contains("INFO"),
        "not said at info: {}",
        limited[0]
    );
    let vault = vs::vault_address(3).0.to_string();
    assert!(
        limited[0].contains(&vault),
        "the vault is not named: {}",
        limited[0]
    );
}
