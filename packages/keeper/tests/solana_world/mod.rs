//! A Solana cluster the keeper keeps as a process: Surfpool with the
//! platform's programs set up on it (three venues, a driver at each, every
//! role played by the development keys; see the chain crate's
//! `three_venues`), vaults activated on it, the deployment file and chain
//! config the keeper reads, a stand-in executor, and the keeper's own sends
//! read back from the cluster.
//!
//! Without Surfpool 1.2.0 or later, the Solana CLI or the built programs
//! (`task contracts:solana:build`), [`World::new`] says so and answers
//! nothing, so a test passes, as CI has no cluster.
//!
//! The keeper run is this build's, or the one `SHONIN_BASELINE_KEEPER` names
//! where a test compares two keepers. Each world's cluster runs on its own
//! ports, and every process it starts is stopped when it is dropped.

#![allow(dead_code)]

#[path = "../../../chain/tests/three_venues/mod.rs"]
pub mod three_venues;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chain::solana::platform::VaultAccounts;
use chain::solana::programs::anchor_lang::{AccountDeserialize, AccountSerialize, Discriminator};
use chain::solana::programs::{station, vault};
use chain::solana::rpc::Account;
use chain::solana::{Keypair, Pubkey, Signer};
use chain::{Role, SolanaDeployments};
use serde_json::{Value, json};

pub use three_venues::{Cluster, DEPOSIT, Platformed};

/// The bounds a Solana cluster's clock is held to, as the checked-in chain
/// config sets them for every cluster.
const SOLANA_CLOCK: &str = "head_lag_max_ms = 10000\nhead_ahead_max_ms = 5000\n";

/// A cluster set up for the keeper, its files and a stand-in executor.
pub struct World {
    pub p: Platformed,
    pub dir: PathBuf,
    executor: tokio::task::JoinHandle<()>,
    pub executor_url: String,
    /// Last: the cluster is stopped once the rest is dropped.
    pub cluster: Cluster,
}

impl Drop for World {
    fn drop(&mut self) {
        self.executor.abort();
        // Where `SHONIN_WORLD_LOGS` names a directory, the keepers' logs are
        // kept there for a look after the test.
        if let Some(keep) = std::env::var_os("SHONIN_WORLD_LOGS") {
            let into = PathBuf::from(keep).join(self.dir.file_name().unwrap_or_default());
            let _ = std::fs::create_dir_all(&into);
            for entry in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "log") {
                    let _ = std::fs::copy(entry.path(), into.join(entry.file_name()));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A keeper process, stopped when dropped.
pub struct Keeper {
    child: Child,
    pub port: u16,
    pub log: PathBuf,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Keeper {
    /// Its log so far.
    pub fn said(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Its health: the status and what it says.
    pub async fn health(&self) -> Option<(u16, String)> {
        let reply = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/health", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let status = reply.status().as_u16();
        Some((status, reply.text().await.unwrap_or_default()))
    }

    /// Its count of requests to the cluster so far, by method.
    pub async fn requests(&self) -> Vec<(String, u64)> {
        let Ok(reply) = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/debug/rpc", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        else {
            return Vec::new();
        };
        let snapshot: Value = reply.json().await.unwrap_or_default();
        snapshot["counters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["chain"].as_str() == Some("solana:localnet"))
            .map(|row| {
                (
                    row["method"].as_str().unwrap_or_default().to_string(),
                    row["requests"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    }
}

/// One of the keeper's transactions as it landed.
#[derive(Clone, Debug)]
pub struct Sent {
    pub slot: u64,
    /// The cluster's time of its block.
    pub time: i64,
    pub failed: bool,
    /// What each of its instructions is, and the vault it names, if any.
    pub calls: Vec<(&'static str, Option<Pubkey>)>,
}

impl Sent {
    /// Whether it carries `what` for `vault`.
    pub fn is(&self, what: &str, vault: &Pubkey) -> bool {
        !self.failed
            && self
                .calls
                .iter()
                .any(|(name, of)| *name == what && of.as_ref() == Some(vault))
    }
}

/// The keeper binary a world runs: this build's.
pub fn this_keeper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_keeper"))
}

/// The keeper `SHONIN_BASELINE_KEEPER` names, if one is given.
pub fn baseline_keeper() -> Option<PathBuf> {
    std::env::var_os("SHONIN_BASELINE_KEEPER")
        .map(PathBuf::from)
        .filter(|path| path.exists())
}

/// What each instruction of the platform's programs the keeper sends is
/// called, by its program and discriminator.
fn named(program: &Pubkey, data: &[u8]) -> &'static str {
    use chain::solana::programs::anchor_lang::Discriminator as D;
    let Some(head) = data.get(..8) else {
        return "other";
    };
    let vault_calls: [(&[u8], &'static str); 16] = [
        (vault::instruction::RecordValuation::DISCRIMINATOR, "value"),
        (vault::instruction::Settle::DISCRIMINATOR, "settle"),
        (
            vault::instruction::SettleClaim::DISCRIMINATOR,
            "settle a claim",
        ),
        (
            vault::instruction::SettleClaimPart::DISCRIMINATOR,
            "settle part of a claim",
        ),
        (vault::instruction::Freeze::DISCRIMINATOR, "freeze"),
        (
            vault::instruction::FreezeUnits::DISCRIMINATOR,
            "freeze for units",
        ),
        (vault::instruction::Evict::DISCRIMINATOR, "evict"),
        (vault::instruction::Close::DISCRIMINATOR, "close"),
        (vault::instruction::Keep::DISCRIMINATOR, "keep"),
        (
            vault::instruction::CloseRequest::DISCRIMINATOR,
            "take back rent",
        ),
        (
            vault::instruction::CloseClaim::DISCRIMINATOR,
            "take back rent",
        ),
        (
            vault::instruction::ServeProRata::DISCRIMINATOR,
            "serve units",
        ),
        (
            vault::instruction::RefreshRestricted::DISCRIMINATOR,
            "refresh the restricted list",
        ),
        (
            vault::instruction::ClaimClosed::DISCRIMINATOR,
            "pay a closed vault's claim",
        ),
        (
            vault::instruction::SettleClosed::DISCRIMINATOR,
            "settle a closed vault's request",
        ),
        (
            vault::instruction::CollectHeld::DISCRIMINATOR,
            "collect held",
        ),
    ];
    if *program == vault::ID {
        return vault_calls
            .iter()
            .find(|(discriminator, _)| *discriminator == head)
            .map_or("vault, other", |(_, name)| name);
    }
    if *program == station::ID {
        let station_calls: [(&[u8], &'static str); 3] = [
            (station::instruction::Stop::DISCRIMINATOR, "stop"),
            (station::instruction::Unwind::DISCRIMINATOR, "unwind"),
            (
                station::instruction::UnwindStopped::DISCRIMINATOR,
                "unwind the stopped station",
            ),
        ];
        return station_calls
            .iter()
            .find(|(discriminator, _)| *discriminator == head)
            .map_or("station, other", |(_, name)| name);
    }
    "other"
}

impl World {
    /// A cluster of its own, set up, its keeper not yet started; `None`
    /// (said) when it can't be had here. `settings` are the cluster's
    /// `[rpc.chains."solana:localnet"]` lines.
    pub async fn new(name: &str, settings: &str) -> Option<Self> {
        let cluster = three_venues::start(&[])?;
        let p = Platformed::set_up(&cluster).await;
        let dir = std::env::temp_dir().join(format!(
            "keeper-solana-{name}-{}-{}",
            std::process::id(),
            three_venues::free_port()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (executor, executor_url) = stand_in_executor().await;
        let world = World {
            p,
            dir,
            executor,
            executor_url,
            cluster,
        };
        world.write_files(settings);
        Some(world)
    }

    fn write_files(&self, settings: &str) {
        let rpc = &self.cluster.rpc;
        std::fs::write(
            self.dir.join("chains.toml"),
            format!(
                "[chains.\"solana:localnet\"]\nlabel = \"surfpool\"\nrpc = \"{rpc}\"\ndollar = \"USDC\"\n\n[networks.local]\nsolana = \"solana:localnet\"\n\n[rpc.chains.\"solana:localnet\"]\n{SOLANA_CLOCK}{settings}\n"
            ),
        )
        .unwrap();
        let client = &self.p.client;
        let keys = client.keys().unwrap();
        let role = |role: Role| keys.address(role).to_string();
        let deployments = SolanaDeployments {
            chain_key: "solana:localnet".into(),
            cluster: "localnet".into(),
            rpc_url: rpc.clone(),
            genesis_hash: client.genesis_hash.clone(),
            deployed_slot: 0,
            programs: chain::solana::programs::compiled()
                .into_iter()
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .filter(|(name, _)| {
                    !matches!(name.as_str(), "shonin_cs_verifier" | "shonin_orca_driver")
                })
                .collect(),
            mint: Some(self.p.platform.mint.to_string()),
            token_program: Some(chain::solana::programs::anchor_spl::token::ID.to_string()),
            checker: self.p.platform.checker.map(|checker| checker.to_string()),
            roles: Some(chain::SolanaRoles {
                deployer: keys.deployer.pubkey().to_string(),
                admin: role(Role::Admin),
                platform: role(Role::Platform),
                attestor: role(Role::Attestor),
                builder: role(Role::Builder),
                reviewer: role(Role::Reviewer),
                executor: role(Role::Executor),
                keeper: role(Role::Keeper),
                operator: role(Role::Operator),
                author: role(Role::Author),
                viewer: role(Role::Viewer),
            }),
        };
        std::fs::write(self.dir.join("solana-localnet.json"), deployments.to_json()).unwrap();
    }

    pub fn role(&self, role: Role) -> &Keypair {
        self.p.role(role)
    }

    /// A strategy published and its vault activated on the three drivers.
    pub async fn vault(&self) -> (u64, VaultAccounts) {
        let (client, platform) = (&self.p.client, &self.p.platform);
        let author = self.role(Role::Author);
        let (id, _) = self.p.publish().await;
        let (_, statement) = platform
            .restricted_for(client, &author.pubkey(), id, &self.p.drivers)
            .await
            .unwrap();
        let factory = platform.factory(client).await.unwrap();
        let activate = platform
            .ix_activate(
                client,
                &author.pubkey(),
                id,
                &self.p.drivers,
                chain::solana::programs::prices::Allowances::default(),
                500,
                factory.terms.minimum_tank,
                statement,
                vault::factory::terms_hash(&factory.terms),
            )
            .await
            .unwrap();
        client.send(vec![activate], &[author]).await.unwrap();
        (id, platform.vault_accounts(id))
    }

    /// The viewer asks to deposit `assets` (request nonce `nonce`).
    pub async fn deposit(&self, v: &VaultAccounts, assets: u64, nonce: u64) {
        let (client, platform) = (&self.p.client, &self.p.platform);
        let viewer = self.role(Role::Viewer);
        let statement = platform
            .vault(client, &v.vault)
            .await
            .unwrap()
            .restricted_hash;
        let deposit = platform.ix_request_deposit(
            &v.vault,
            &viewer.pubkey(),
            &viewer.pubkey(),
            assets,
            Some(statement),
            nonce,
        );
        client.send(vec![deposit], &[viewer]).await.unwrap();
    }

    /// The viewer asks for `shares` back (request nonce `nonce`).
    pub async fn redeem(&self, v: &VaultAccounts, shares: u128, nonce: u64) {
        let viewer = self.role(Role::Viewer);
        let redeem = self.p.platform.ix_request_redeem(
            &v.vault,
            &viewer.pubkey(),
            shares,
            &viewer.pubkey(),
            nonce,
        );
        self.p.client.send(vec![redeem], &[viewer]).await.unwrap();
    }

    /// The viewer's shares in the vault.
    pub async fn shares(&self, v: &VaultAccounts) -> u128 {
        self.p
            .platform
            .shares(&self.p.client, &v.vault, &self.role(Role::Viewer).pubkey())
            .await
            .unwrap()
    }

    /// The cluster's clock.
    pub async fn now(&self) -> u64 {
        self.p.client.now().await.unwrap()
    }

    /// The account of type `T` at `address` rewritten by `change`, as
    /// Surfpool lets a test write any account.
    pub async fn rewrite<T: AccountDeserialize + AccountSerialize>(
        &self,
        address: &Pubkey,
        change: impl FnOnce(&mut T),
    ) {
        let rpc = &self.p.client.rpc;
        let account = rpc.account(address).await.unwrap().expect("an account");
        let mut value = T::try_deserialize(&mut &account.data[..]).unwrap();
        change(&mut value);
        let mut data = Vec::new();
        value.try_serialize(&mut data).unwrap();
        data.resize(account.data.len().max(data.len()), 0);
        rpc.set_account(address, &Account { data, ..account })
            .await
            .unwrap();
    }

    /// The keeper `binary` started against the cluster, `log` its tracing
    /// filter, `tag` naming its files.
    pub fn keeper(&self, binary: &Path, tag: &str, log: &str) -> Keeper {
        let port = three_venues::free_port();
        let file = self.dir.join(format!("keeper-{tag}.log"));
        let store = self.dir.join(format!("store-{tag}"));
        std::fs::create_dir_all(&store).unwrap();
        let child = Command::new(binary)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("RUST_LOG", log)
            .args(["--network", "local", "--vault-chain", "solana"])
            .arg("--chains")
            .arg(self.dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&self.dir)
            .arg("--store")
            .arg(&store)
            .args(["--api-url", "http://127.0.0.1:1"])
            .args(["--executor-url", &self.executor_url])
            .args(["--port", &port.to_string()])
            .stdout(std::fs::File::create(&file).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Keeper {
            child,
            port,
            log: file,
        }
    }

    /// Wait until `done` holds, reading the cluster every half second, for
    /// at most `limit`.
    pub async fn until<F, Fut>(&self, limit: Duration, mut done: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let started = Instant::now();
        while started.elapsed() < limit {
            if done().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        false
    }

    /// Every transaction the keeper's key paid for, oldest first, each
    /// instruction named with the vault it names (one of `vaults`).
    pub async fn sends(&self, vaults: &[Pubkey]) -> Vec<Sent> {
        let rpc = &self.p.client.rpc;
        let keeper = self.role(Role::Keeper).pubkey();
        let mut signatures = Vec::new();
        let mut before: Option<String> = None;
        loop {
            let page = rpc
                .signatures_for(&keeper, 1_000, before.as_deref(), None)
                .await
                .unwrap();
            let Some(last) = page.last() else { break };
            before = Some(last.signature.clone());
            let more = page.len() == 1_000;
            signatures.extend(page);
            if !more {
                break;
            }
        }
        signatures.reverse();
        let mut sent = Vec::new();
        for info in signatures {
            let Some(record) = self.p.client.transaction(&info.signature).await.unwrap() else {
                continue;
            };
            let calls = record
                .instructions
                .iter()
                .filter(|call| call.program == vault::ID || call.program == station::ID)
                .map(|call| {
                    (
                        named(&call.program, &call.data),
                        call.accounts
                            .iter()
                            .find(|account| vaults.contains(account))
                            .copied(),
                    )
                })
                .collect();
            sent.push(Sent {
                slot: record.slot,
                time: record.block_time.unwrap_or_default(),
                failed: record.err.is_some(),
                calls,
            });
        }
        sent
    }
}

/// An executor that serves no vault's total and publishes no key: every
/// ask is answered, with a 404.
async fn stand_in_executor() -> (tokio::task::JoinHandle<()>, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..n]).to_string();
                let (status, body) = if head.starts_with("GET /attestation") {
                    ("200 OK", json!({"solana": []}).to_string())
                } else {
                    ("404 Not Found", String::new())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (task, url)
}

/// Whether `vault::state::Vault`'s discriminator is the one an account at
/// `address` carries.
pub fn is_vault(account: &Account) -> bool {
    account.data.get(..8) == Some(vault::state::Vault::DISCRIMINATOR)
}
