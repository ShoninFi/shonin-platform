//! The arguments every service takes to find its chain, so each one chooses
//! the network and the vault chain the same way.
//!
//! Serving several chains at once is later work: for now a service serves one
//! vault chain, `base` unless told otherwise.

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
    /// The vault chain this service serves.
    #[arg(long, env = "SHONIN_VAULT_CHAIN", default_value = "base")]
    pub vault_chain: Chain,
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
    pub fn resolve(&self) -> Result<Target> {
        let configs = ChainConfigs::load(&self.chains)?;
        let (key, config) = configs.resolve(self.network, self.vault_chain)?;
        Ok(Target {
            network: self.network,
            vault_chain: self.vault_chain,
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

    /// This chain's deployment: from `SHONIN_DEPLOYMENT_JSON` when it is set
    /// (how a machine in the cloud is handed a testnet deployment, which is
    /// never committed), otherwise from the deployment file once it appears.
    pub async fn wait_for_deployments(&self, client: &Client) -> Result<Deployments> {
        match std::env::var(DEPLOYMENT_JSON)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(json) => {
                let deployments = Deployments::from_json(&json)
                    .map_err(|error| anyhow!("{DEPLOYMENT_JSON} is not a deployment: {error}"))?;
                if deployments.chain_id != client.chain_id {
                    bail!(
                        "{DEPLOYMENT_JSON} is for chain {}, and this is chain {}",
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

/// A chain's deployment, handed over whole in the environment.
pub const DEPLOYMENT_JSON: &str = "SHONIN_DEPLOYMENT_JSON";

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
