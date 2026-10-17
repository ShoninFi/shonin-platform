//! A keeper run as a process, as an operator runs it: `--network local`,
//! one vault chain, its deployment from `SHONIN_DEPLOYMENTS_JSON`, the
//! executor stand-in's URL. The node signs for the keeper: it is the fifth
//! of the node's accounts.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use alloy::primitives::Address;
use chain::{Deployments, Roles};
use serde_json::Value;

use super::node::{BlockTime, free_port};
use super::world::World;

/// A keeper process, killed when dropped.
pub struct Keeper {
    pub who: String,
    child: Child,
    port: u16,
    pub log: PathBuf,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How to start one keeper.
pub struct Start<'a> {
    /// Its name in the tables: "baseline", "baseline again", "candidate".
    pub who: &'a str,
    pub binary: &'a Path,
    pub block: BlockTime,
    /// Where its node answers: the proxy in front of its anvil.
    pub rpc: &'a str,
    pub executor: &'a str,
    /// `[rpc.*]` tables to add to its chain config, if any.
    pub rpc_tables: &'a str,
    pub world: &'a World,
    /// Where its config and log go.
    pub dir: &'a Path,
}

impl Keeper {
    pub fn start(start: Start) -> std::io::Result<Keeper> {
        std::fs::create_dir_all(start.dir)?;
        let config = start.dir.join("chains.toml");
        std::fs::write(&config, chains_toml(&start))?;
        let log = start.dir.join("keeper.log");
        let port = free_port();
        let deployments = vec![deployments(start.world)];
        let child = Command::new(start.binary)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("RUST_LOG", "info,keeper=debug")
            .env(
                "SHONIN_DEPLOYMENTS_JSON",
                serde_json::to_string(&deployments).expect("deployments as JSON"),
            )
            .args([
                "--network",
                "local",
                "--vault-chain",
                start.block.vault_chain(),
            ])
            .arg("--chains")
            .arg(&config)
            .arg("--deployments-dir")
            .arg(start.dir)
            .arg("--store")
            .arg(start.dir)
            .args(["--api-url", "http://127.0.0.1:1"])
            .args(["--executor-url", start.executor])
            .args(["--port", &port.to_string()])
            .stdout(std::fs::File::create(&log)?)
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Keeper {
            who: start.who.to_string(),
            child,
            port,
            log,
        })
    }

    /// Whether it has exited, and how.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// What its `/debug/rpc` counts, if it answers.
    pub async fn rpc_counts(&self) -> Option<Value> {
        reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/debug/rpc", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()
    }

    /// Its health's status and text, if it answers.
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

    pub fn logged(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn chains_toml(start: &Start) -> String {
    let id = start.block.chain_id();
    let vault_chain = start.block.vault_chain();
    let mut text = format!(
        "[chains.\"evm:{id}\"]\nlabel = \"stand-in evm:{id}\"\nrpc = \"{}\"\ndollar = \"USDC\"\nconfirmations = 0\n\n\
         [networks.local]\n{vault_chain} = \"evm:{id}\"\nroot = \"{vault_chain}\"\n\n",
        start.rpc
    );
    text.push_str(start.rpc_tables);
    text
}

/// The deployment the keeper is handed: the stand-ins where the keeper reads
/// them, nothing anywhere else.
fn deployments(world: &World) -> Deployments {
    let nobody = Address::repeat_byte(0x55);
    Deployments {
        chain_key: format!("evm:{}", world.chain_id),
        chain_id: world.chain_id,
        rpc_url: String::new(),
        deployed_block: 0,
        usd: nobody,
        venue: nobody,
        attestation_verifier: nobody,
        attestation: world.registry,
        publishing: nobody,
        prices: nobody,
        strategies: nobody,
        reviews: nobody,
        profiles: nobody,
        factory: world.factory,
        station_factory: nobody,
        driver: world.plain_driver,
        bridge: nobody,
        closer: world.closer,
        roles: Roles {
            admin: world.deployer,
            platform: nobody,
            attestor: nobody,
            builder: nobody,
            reviewer: nobody,
            executor: nobody,
            keeper: world.keeper,
            operator: nobody,
            author: nobody,
            viewer: nobody,
        },
    }
}
