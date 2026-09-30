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
            pending.spawn(async move {
                let client = target.connect().await?;
                let deployments = target.wait_for_deployments(&client).await?;
                Ok::<_, anyhow::Error>((
                    order,
                    Served {
                        vault_chain: target.vault_chain,
                        key: target.key,
                        client,
                        deployments,
                    },
                ))
            });
        }
        let mut served = Vec::new();
        while let Some(done) = pending.join_next().await {
            served.push(done??);
        }
        served.sort_by_key(|(order, _)| *order);
        Ok(served.into_iter().map(|(_, served)| served).collect())
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

impl Target {
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
    let client = Client::connect(config, signing_for(network)?).await?;
    if let Some(expected) = key.evm_chain_id()
        && expected != client.chain_id
    {
        bail!(
            "{} answers as chain {}, not {expected}: check its rpc in the chain config",
            config.rpc,
            client.chain_id
        );
    }
    tracing::info!(%network, chain = %key, label = config.label, "connected");
    Ok(client)
}
