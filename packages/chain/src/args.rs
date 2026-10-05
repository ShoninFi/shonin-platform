//! The arguments every service takes to find its chains, so each one chooses
//! the network and the vault chains the same way.
//!
//! A service serves every vault chain of the network that it can speak to
//! (the EVM ones), each with its own client and deployment, unless it is told
//! to serve one.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use shared::Chain;

use crate::client::{Client, ROLE_COUNT, Signing, keys_from_mnemonic};
use crate::config::{ChainConfig, ChainConfigs, ChainKey, ChainKind, NetworkName};
use crate::deployments::Deployments;

#[derive(clap::Args, Clone, Debug)]
pub struct ChainArgs {
    /// Which chains: local, testnet or mainnet.
    #[arg(long, env = "SHONIN_NETWORK", default_value = "local")]
    pub network: NetworkName,
    /// Every chain the platform knows, and what each network means.
    #[arg(long, env = "SHONIN_CHAINS", default_value = "config/chains.toml")]
    pub chains: PathBuf,
    /// Where each chain's deployment file is, one per chain.
    #[arg(
        long,
        env = "SHONIN_DEPLOYMENTS_DIR",
        default_value = ".shonin/local/deployments"
    )]
    pub deployments_dir: PathBuf,
    /// Serve only this vault chain, instead of every one of the network.
    #[arg(long, env = "SHONIN_VAULT_CHAIN")]
    pub vault_chain: Option<Chain>,
}

/// One vault chain a service serves: its client and its deployment.
#[derive(Clone)]
pub struct Served {
    pub vault_chain: Chain,
    pub key: ChainKey,
    pub client: Client,
    pub deployments: Deployments,
    /// The native coin's price in dollars, where the chain's config gives one.
    pub native_usd: Option<f64>,
    /// The chain's RPC as the chain config file gives it, which may be shown
    /// to anyone; the one the client uses may be a paid URL carrying a key.
    pub public_rpc: String,
}

/// What the arguments came to: the chain to talk to, and where its
/// deployment is recorded.
#[derive(Clone, Debug)]
pub struct Target {
    pub network: NetworkName,
    pub vault_chain: Chain,
    pub key: ChainKey,
    pub config: ChainConfig,
    /// Every chain, for anything that looks beyond its own.
    pub configs: ChainConfigs,
    pub deployments_dir: PathBuf,
}

impl ChainArgs {
    /// The one vault chain named, or base: for the tools that act on a single
    /// chain, such as the seed.
    pub fn resolve(&self) -> Result<Target> {
        let configs = ChainConfigs::load(&self.chains)?;
        self.target(&configs, self.vault_chain.unwrap_or(Chain::Base))
    }

    /// Every vault chain this service serves: the one named, or every one of
    /// the network whose chain the services speak to. Solana is listed and
    /// skipped, because no service speaks to it yet.
    pub fn targets(&self) -> Result<Vec<Target>> {
        let configs = ChainConfigs::load(&self.chains)?;
        let wanted: Vec<Chain> = match self.vault_chain {
            Some(chain) => vec![chain],
            None => Chain::ALL.to_vec(),
        };
        let mut targets = Vec::new();
        for chain in wanted {
            let target = self.target(&configs, chain)?;
            // The config refuses a chain named twice; should one get through,
            // it is still served once, under the first vault chain naming it.
            if let Some(first) = targets
                .iter()
                .find(|served: &&Target| served.key.same_chain(&target.key))
            {
                tracing::warn!(chain = %target.key, first = %first.vault_chain, also = %target.vault_chain, "a chain named for two vault chains is served once");
                continue;
            }
            if target.key.kind == ChainKind::Evm {
                targets.push(target);
            } else if self.vault_chain.is_some() {
                bail!(
                    "{} is a {} chain, and the services only speak to EVM chains",
                    target.key,
                    target.key.kind.name()
                );
            } else {
                tracing::info!(chain = %target.key, "not served: the services only speak to EVM chains");
            }
        }
        Ok(targets)
    }

    /// Connect to every chain this service serves and wait for each one's
    /// deployment, all at once.
    pub async fn connect_all(&self) -> Result<Vec<Served>> {
        let mut pending = tokio::task::JoinSet::new();
        for (order, target) in self.targets()?.into_iter().enumerate() {
            pending.spawn(async move { Ok::<_, anyhow::Error>((order, target.serve().await?)) });
        }
        let mut served = Vec::new();
        while let Some(done) = pending.join_next().await {
            served.push(done??);
        }
        served.sort_by_key(|(order, _)| *order);
        Ok(served.into_iter().map(|(_, served)| served).collect())
    }

    /// Connect to every chain this service serves, each on its own, and
    /// hand each one over as soon as it is connected and its deployment
    /// found. A chain that can't be (its node down, its deployment missing)
    /// is said at warn and tried again, waiting longer each time, until it
    /// is: one chain out of reach doesn't keep the service from the others.
    /// The receiver ends once every chain has been handed over.
    pub fn connect_each(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Served>> {
        Ok(each_until_connected(
            self.targets()?,
            |target| target.key.canonical(),
            |target| async move { target.serve().await },
            CONNECT_RETRY_FIRST,
        ))
    }

    fn target(&self, configs: &ChainConfigs, vault_chain: Chain) -> Result<Target> {
        let (key, config) = configs.resolve(self.network, vault_chain)?;
        Ok(Target {
            network: self.network,
            vault_chain,
            key: key.clone(),
            config: config.clone(),
            deployments_dir: self.deployments_dir.clone(),
            configs: configs.clone(),
        })
    }
}

/// How long a chain that could not be connected to is first waited on
/// before it is tried again. Each try after waits twice as long as the last,
/// up to [`CONNECT_RETRY_MOST`].
const CONNECT_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(5);
const CONNECT_RETRY_MOST: std::time::Duration = std::time::Duration::from_secs(300);

/// Try `connect` on every target at once, each until it succeeds, and send
/// each success on as it comes. A failure is said at warn, with how long
/// until the next try. A target named twice is connected once.
fn each_until_connected<T, S, F, Fut>(
    targets: Vec<T>,
    name: impl Fn(&T) -> String,
    connect: F,
    first_wait: std::time::Duration,
) -> tokio::sync::mpsc::UnboundedReceiver<S>
where
    T: Clone + Send + 'static,
    S: Send + 'static,
    F: Fn(T) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<S>> + Send,
{
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut named = std::collections::HashSet::new();
    for target in targets {
        let chain = name(&target);
        if !named.insert(chain.clone()) {
            continue;
        }
        let connect = connect.clone();
        let send = send.clone();
        tokio::spawn(async move {
            let mut wait = first_wait;
            loop {
                match connect(target.clone()).await {
                    Ok(served) => {
                        let _ = send.send(served);
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(
                            chain,
                            error = format!("{error:#}"),
                            retry_in_secs = wait.as_secs(),
                            "could not serve this chain; the others are served meanwhile"
                        );
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(CONNECT_RETRY_MOST.max(first_wait));
                    }
                }
            }
        });
    }
    receive
}

impl Target {
    /// Connect to this chain and wait for its deployment.
    pub async fn serve(&self) -> Result<Served> {
        let client = self.connect().await?;
        let deployments = self.wait_for_deployments(&client).await?;
        Ok(Served {
            vault_chain: self.vault_chain,
            key: self.key.clone(),
            client,
            deployments,
            native_usd: self.config.native_usd,
            public_rpc: self.config.public_rpc.clone(),
        })
    }

    /// The deployment file for this chain.
    pub fn deployments_file(&self) -> PathBuf {
        Deployments::file_in(&self.deployments_dir, &self.key)
    }

    /// Connect to this target's chain, waiting for it to answer.
    pub async fn connect(&self) -> Result<Client> {
        connect_to(self.network, &self.key, &self.config).await
    }

    /// This chain's deployment: from `SHONIN_DEPLOYMENTS_JSON` when it is set
    /// (how a machine in the cloud is handed the testnet deployments, which
    /// are never committed), otherwise from the deployment file once it
    /// appears.
    pub async fn wait_for_deployments(&self, client: &Client) -> Result<Deployments> {
        match std::env::var(DEPLOYMENTS_JSON)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(json) => {
                let all: Vec<Deployments> = serde_json::from_str(&json).map_err(|error| {
                    anyhow!("{DEPLOYMENTS_JSON} is not a list of deployments: {error}")
                })?;
                let key = self.key.to_string();
                let deployments = all
                    .into_iter()
                    .find(|deployments| deployments.chain_key == key)
                    .ok_or_else(|| anyhow!("{DEPLOYMENTS_JSON} has no deployment for {key}"))?;
                if deployments.chain_id != client.chain_id {
                    bail!(
                        "{DEPLOYMENTS_JSON} says {key} is chain {}, and it answers as {}",
                        deployments.chain_id,
                        client.chain_id
                    );
                }
                Ok(deployments)
            }
            None => client.wait_for_deployments(&self.deployments_file()).await,
        }
    }
}

/// Every served chain's deployment, handed over whole in the environment as a
/// JSON list.
pub const DEPLOYMENTS_JSON: &str = "SHONIN_DEPLOYMENTS_JSON";

/// The mnemonic the testnet role accounts are derived from.
pub const TESTNET_EVM_MNEMONIC: &str = "SHONIN_TESTNET_EVM_MNEMONIC";

/// Who signs on a network: the node on local chains, the developer's
/// mnemonic on testnets, and nobody on mainnet, where clients only read.
pub fn signing_for(network: NetworkName) -> Result<Signing> {
    match network {
        NetworkName::Local => Ok(Signing::Unlocked),
        NetworkName::Testnet => {
            let phrase = std::env::var(TESTNET_EVM_MNEMONIC)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "the testnet role accounts come from {TESTNET_EVM_MNEMONIC}; set it in .env"
                    )
                })?;
            Ok(Signing::Keys(keys_from_mnemonic(&phrase, ROLE_COUNT)?))
        }
        NetworkName::Mainnet => Ok(Signing::ReadOnly),
    }
}

/// Connect to any EVM chain of a network, and check it is the chain its key
/// says.
pub async fn connect_to(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
) -> Result<Client> {
    if key.kind != ChainKind::Evm {
        bail!(
            "{key} is a {} chain, and the services only speak to EVM chains so far",
            key.kind.name()
        );
    }
    // A key read from the chain config is always a number; one built by
    // hand is held to the same rule, so no node goes unchecked.
    let expected = key
        .evm_chain_id()
        .ok_or_else(|| anyhow!("{key} names no EVM chain id"))?;
    let client = Client::connect(config, signing_for(network)?).await?;
    if expected != client.chain_id {
        // Named by its label: the URL may be a paid one, carrying its key.
        bail!(
            "{} answers as chain {}, not {expected}: check its rpc in the chain config",
            config.label,
            client.chain_id
        );
    }
    tracing::info!(%network, chain = %key, label = config.label, "connected");
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn a_chain_out_of_reach_holds_up_none_of_the_others_and_is_tried_until_it_connects() {
        // "down" fails its first three tries; "up" connects at once.
        let tries = Arc::new(Mutex::new(0));
        let counted = tries.clone();
        let mut connected = each_until_connected(
            vec!["down", "up"],
            |name| name.to_string(),
            move |name: &'static str| {
                let tries = counted.clone();
                async move {
                    if name == "down" {
                        let mut tries = tries.lock().unwrap();
                        *tries += 1;
                        if *tries <= 3 {
                            bail!("no answer");
                        }
                    }
                    Ok(name)
                }
            },
            Duration::from_secs(5),
        );
        let started = tokio::time::Instant::now();
        assert_eq!(connected.recv().await, Some("up"));
        assert!(started.elapsed() < Duration::from_secs(1));
        // Tried again after 5, 10 and 20 seconds.
        assert_eq!(connected.recv().await, Some("down"));
        assert_eq!(*tries.lock().unwrap(), 4);
        assert_eq!(started.elapsed(), Duration::from_secs(35));
        // Every chain handed over: nothing more comes.
        assert_eq!(connected.recv().await, None);
    }

    #[tokio::test]
    async fn a_chain_named_twice_is_connected_and_handed_over_once() {
        let tries = Arc::new(Mutex::new(0));
        let counted = tries.clone();
        let mut connected = each_until_connected(
            vec!["one", "one"],
            |name| name.to_string(),
            move |name: &'static str| {
                let tries = counted.clone();
                async move {
                    *tries.lock().unwrap() += 1;
                    Ok(name)
                }
            },
            Duration::from_secs(5),
        );
        assert_eq!(connected.recv().await, Some("one"));
        assert_eq!(connected.recv().await, None);
        assert_eq!(*tries.lock().unwrap(), 1);
    }
}
