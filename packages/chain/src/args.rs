//! The arguments every service takes to find its chain, so each one chooses
//! the network and the vault chain the same way.
//!
//! Serving several chains at once is later work: for now a service serves one
//! vault chain, `base` unless told otherwise.

use std::path::PathBuf;

use anyhow::{Result, bail};
use shared::Chain;

use crate::client::Client;
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

    /// Connect to the chain, waiting for it to answer.
    pub async fn connect(&self) -> Result<Client> {
        if self.key.kind != ChainKind::Evm {
            bail!(
                "{} is a {} chain, and the services only speak to EVM chains so far",
                self.key,
                self.key.kind.name()
            );
        }
        let client = Client::connect(&self.config).await?;
        if let Some(expected) = self.key.evm_chain_id()
            && expected != client.chain_id
        {
            bail!(
                "{} answers as chain {}, not {expected}: check its rpc in the chain config",
                self.config.rpc,
                client.chain_id
            );
        }
        tracing::info!(network = %self.network, chain = %self.key, label = self.config.label, "connected");
        Ok(client)
    }
}
