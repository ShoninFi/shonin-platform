//! A Solana look the node answers as of a slot older than one the keeper
//! has already seen is not acted on: no vault's round is made on it, nothing
//! is read for one, and the keeper says the node lags; once the node is
//! caught up, the rounds go on.
//!
//! A stand-in cluster answers the looks (the clock at the wall's time, the
//! factory, one open vault) at a slot that rises, then for a while at an
//! older one, then rises again. The keeper runs as a process against it.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

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
    /// The slot answered: rising, unless the node lags.
    slot: AtomicU64,
    lagging: AtomicBool,
    /// Calls other than a look's own read, while the node lags.
    asked_while_lagging: std::sync::Mutex<Vec<String>>,
}

impl Cluster {
    fn slot(&self) -> u64 {
        if self.lagging.load(Ordering::SeqCst) {
            5
        } else {
            self.slot.fetch_add(1, Ordering::SeqCst)
        }
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, Value> {
        let program = chain::solana::programs::vault::ID.to_string();
        if self.lagging.load(Ordering::SeqCst) && method != "getMultipleAccounts" {
            self.asked_while_lagging
                .lock()
                .unwrap()
                .push(method.to_string());
        }
        let account = |address: &str| -> Value {
            if address == CLOCK {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                let mut clock = vec![0u8; 40];
                clock[32..40].copy_from_slice(&now.to_le_bytes());
                raw(&Pubkey::default().to_string(), &clock)
            } else if address == vs::factory_address().to_string() {
                raw(&program, &self.factory)
            } else if address == self.vault.0.to_string() {
                raw(&program, &self.vault.1)
            } else {
                Value::Null
            }
        };
        match method {
            "getGenesisHash" => Ok(json!("stand-in-genesis")),
            "getAccountInfo" => Ok(
                json!({"context": {"slot": self.slot()}, "value": account(params[0].as_str().unwrap_or_default())}),
            ),
            "getMultipleAccounts" => {
                let values: Vec<Value> = params[0]
                    .as_array()
                    .map(|all| {
                        all.iter()
                            .map(|address| account(address.as_str().unwrap_or_default()))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({"context": {"slot": self.slot()}, "value": values}))
            }
            "getProgramAccounts" => {
                let filter = params[1]["filters"][0]["memcmp"]["bytes"]
                    .as_str()
                    .unwrap_or_default();
                let found = if filter == bs58::encode(Vault::DISCRIMINATOR).into_string() {
                    json!([{"pubkey": self.vault.0.to_string(), "account": raw(&program, &self.vault.1)}])
                } else {
                    json!([])
                };
                Ok(json!({"context": {"slot": self.slot()}, "value": found}))
            }
            "getBalance" => {
                Ok(json!({"context": {"slot": self.slot()}, "value": 10_000_000_000u64}))
            }
            _ => Err(json!({"code": -32601, "message": format!("not here: {method}")})),
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
                let reply = match cluster.answer(
                    request["method"].as_str().unwrap_or_default(),
                    &request["params"],
                ) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
                };
                let reply = reply.to_string();
                let http = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
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
    fn said(&self) -> String {
        std::fs::read_to_string(self.dir.join("keeper.log")).unwrap_or_default()
    }
}

fn start(rpc: &str) -> Keeper {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = std::env::temp_dir().join(format!(
        "keeper-solana-lagging-{}-{port}",
        std::process::id()
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
    let child = Command::new(env!("CARGO_BIN_EXE_keeper"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "info,keeper::solana=debug")
        .env("NO_COLOR", "1")
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
    Keeper { child, dir }
}

/// How many looks the keeper says it made.
fn looks(said: &str) -> usize {
    said.lines()
        .filter(|line| {
            line.contains("looked at the cluster")
                || line.contains("read the cluster itself this round")
        })
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_look_answered_from_an_older_slot_than_one_seen_is_not_acted_on() {
    let strategy_id = 3;
    let vault_address = vs::vault_address(strategy_id).0;
    let mut factory: Factory = zeroed(Factory::INIT_SPACE);
    factory.vault_count = 1;
    let mut vault: Vault = zeroed(Vault::INIT_SPACE);
    vault.strategy_id = strategy_id;
    vault.state = VaultState::Open;
    vault.timers.slot_interval = 10;
    vault.timers.slot_window = 3;
    let cluster = Arc::new(Cluster {
        factory: encode(&factory),
        vault: (vault_address, encode(&vault)),
        slot: AtomicU64::new(100),
        lagging: AtomicBool::new(false),
        asked_while_lagging: std::sync::Mutex::default(),
    });
    let rpc = serve(cluster.clone()).await;
    let keeper = start(&rpc);

    // The vault is kept, look after look.
    let started = std::time::Instant::now();
    while looks(&keeper.said()) < 3 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the keeper never looked:\n{}",
            keeper.said()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The node lags: every answer as of a slot below the ones seen.
    cluster.lagging.store(true, Ordering::SeqCst);
    let before = looks(&keeper.said());
    tokio::time::sleep(Duration::from_secs(12)).await;
    let said = keeper.said();
    assert_eq!(
        looks(&said),
        before,
        "a round was made on a look from an older slot:\n{said}"
    );
    let asked = cluster.asked_while_lagging.lock().unwrap().clone();
    assert!(
        asked.is_empty(),
        "the keeper read for a round while the node lagged: {asked:?}"
    );
    assert!(
        said.contains("the node lags"),
        "the lag is not said:\n{said}"
    );

    // Caught up: the rounds go on.
    cluster.lagging.store(false, Ordering::SeqCst);
    cluster.slot.store(1_000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    while looks(&keeper.said()) == before {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the rounds never went on once the node caught up:\n{}",
            keeper.said()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
