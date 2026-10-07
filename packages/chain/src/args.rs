//! The arguments every service takes to find its chains, so each one chooses
//! the network and the vault chains the same way.
//!
//! A service serves every vault chain of the network that it can speak to,
//! each with its own client and deployment, unless it is told to serve one.
//! The EVM chains come from [`ChainArgs::targets`] as [`Served`]; the Solana
//! clusters from [`ChainArgs::solana_targets`] as [`SolanaServed`].

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use shared::Chain;

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;

use crate::client::{Client, ROLE_COUNT, Role, Signing, keys_from_mnemonic};
use crate::config::{ChainConfig, ChainConfigs, ChainKey, ChainKind, NetworkName};
use crate::deployments::{Deployments, Roles, SolanaDeployments};
use crate::solana::{Platform, SolanaClient};

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

    /// The network's canonical chain for the root key (its `root` entry in
    /// the chain config), or `None` where the network names none.
    pub fn root_chain(&self) -> Result<Option<ChainKey>> {
        let configs = ChainConfigs::load(&self.chains)?;
        Ok(configs
            .network(self.network)
            .and_then(|network| network.root())
            .map(|(_, key)| key.clone()))
    }

    /// Every EVM vault chain this service serves: the one named, if it is an
    /// EVM one, or every EVM vault chain of the network. A Solana vault chain
    /// named alone leaves none, so the service serves that cluster alone; the
    /// Solana clusters are [`ChainArgs::solana_targets`]'s.
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
            }
        }
        Ok(targets)
    }

    /// The EVM vault chains to connect to, for a service that speaks to them:
    /// refused when there are none, as when a Solana vault chain is named
    /// alone to a service that serves only EVM chains.
    fn evm_targets_to_serve(&self) -> Result<Vec<Target>> {
        let targets = self.targets()?;
        if targets.is_empty() {
            bail!(
                "no EVM vault chain to serve{}: this service speaks only to EVM chains",
                self.vault_chain
                    .map(|chain| format!(" ({chain} is not one)"))
                    .unwrap_or_default()
            );
        }
        Ok(targets)
    }

    /// Every Solana vault chain this service serves: the one named, if it is
    /// a Solana one, or every Solana vault chain of the network. The EVM
    /// chains are [`ChainArgs::targets`]'s.
    ///
    /// With [`WITHOUT_SOLANA`] set, none: the service serves the EVM chains
    /// alone, which any build may, whatever network its Solana program ids
    /// are for. It is a choice made out loud; a service is never left
    /// quietly not serving a cluster of its network.
    pub fn solana_targets(&self) -> Result<Vec<Target>> {
        if without_solana() {
            return Ok(Vec::new());
        }
        let configs = ChainConfigs::load(&self.chains)?;
        let wanted: Vec<Chain> = match self.vault_chain {
            Some(chain) => vec![chain],
            None => Chain::ALL.to_vec(),
        };
        let mut targets: Vec<Target> = Vec::new();
        for chain in wanted {
            let target = self.target(&configs, chain)?;
            if target.key.kind == ChainKind::Solana
                && !targets
                    .iter()
                    .any(|served| served.key.same_chain(&target.key))
            {
                targets.push(target);
            }
        }
        Ok(targets)
    }

    /// Connect to every Solana cluster this service serves, each on its own,
    /// handing each one over once it is connected and its deployment found,
    /// as [`ChainArgs::connect_each`] does for the EVM chains. Refused when a
    /// cluster is to be served by a build whose program ids are not the
    /// network's (see [`BuiltFor`]).
    pub fn connect_each_solana(
        &self,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<SolanaServed>> {
        let targets = self.solana_targets()?;
        if !targets.is_empty() {
            BuiltFor::THIS.serves(self.network)?;
        }
        Ok(each_until_connected(
            targets,
            |target| target.key.canonical(),
            |target| async move { target.serve_solana().await },
            CONNECT_RETRY_FIRST,
        ))
    }

    /// Connect to every chain this service serves and wait for each one's
    /// deployment, all at once.
    pub async fn connect_all(&self) -> Result<Vec<Served>> {
        let mut pending = tokio::task::JoinSet::new();
        for (order, target) in self.evm_targets_to_serve()?.into_iter().enumerate() {
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
            self.evm_targets_to_serve()?,
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

/// Set (to anything but empty or `0`), a service serves the EVM chains of its
/// network alone and no Solana cluster.
pub const WITHOUT_SOLANA: &str = "SHONIN_WITHOUT_SOLANA";

fn without_solana() -> bool {
    std::env::var(WITHOUT_SOLANA).is_ok_and(|value| !value.is_empty() && value != "0")
}

/// The network whose Solana program ids this build compiled in, by the
/// chain crate's `mainnet` and `devnet` features (a testnet build uses
/// `devnet`); with neither, the local ones. A Solana cluster is served only by
/// a build for its network, since a program id is not read from the
/// deployment file but compiled in. The EVM chains' addresses come from their
/// deployment files, so an EVM-only service runs on any build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuiltFor {
    Local,
    Devnet,
    Mainnet,
}

impl BuiltFor {
    /// What this build is for.
    const THIS: BuiltFor = if cfg!(feature = "mainnet") {
        BuiltFor::Mainnet
    } else if cfg!(feature = "devnet") {
        BuiltFor::Devnet
    } else {
        BuiltFor::Local
    };

    /// Whether a build for this serves `network`'s Solana clusters, and if
    /// not, which build does.
    fn serves(self, network: NetworkName) -> Result<()> {
        let needed = match network {
            NetworkName::Local => BuiltFor::Local,
            NetworkName::Testnet => BuiltFor::Devnet,
            NetworkName::Mainnet => BuiltFor::Mainnet,
        };
        if self == needed {
            return Ok(());
        }
        let build = match needed {
            BuiltFor::Local => "without the mainnet and devnet features".to_string(),
            BuiltFor::Devnet => "with the devnet feature".to_string(),
            BuiltFor::Mainnet => "with the mainnet feature".to_string(),
        };
        let this = match self {
            BuiltFor::Local => "the local program ids",
            BuiltFor::Devnet => "the devnet program ids",
            BuiltFor::Mainnet => "the mainnet program ids",
        };
        bail!("this build carries {this}, and the {network} Solana cluster needs a build {build}")
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
    ///
    /// On a testnet without the mnemonic the role accounts are the ones its
    /// deployment names (from `SHONIN_DEPLOYMENTS_JSON` or the deployment
    /// file, if either is there yet), and only the keeper may sign, with
    /// [`TESTNET_KEEPER_KEY`] ([`signing_with_roles`]).
    pub async fn connect(&self) -> Result<Client> {
        let roles = match self.network {
            NetworkName::Testnet if !mnemonic_is_set() => self.recorded_roles()?,
            _ => None,
        };
        connect_with(
            &self.key,
            &self.config,
            self.network,
            signing_with_roles(self.network, roles.as_ref())?,
        )
        .await
    }

    /// The role accounts this chain's deployment names, if it is to hand.
    fn recorded_roles(&self) -> Result<Option<Roles>> {
        if let Some(value) = deployment_from_env(&self.key)? {
            let deployments: Deployments = serde_json::from_value(value).map_err(|error| {
                anyhow!(
                    "{DEPLOYMENTS_JSON}'s deployment for {} is not an EVM one: {error}",
                    self.key
                )
            })?;
            return Ok(Some(deployments.roles));
        }
        Ok(std::fs::read_to_string(self.deployments_file())
            .ok()
            .and_then(|text| Deployments::from_json(&text).ok())
            .map(|deployments| deployments.roles))
    }

    /// This chain's deployment: from `SHONIN_DEPLOYMENTS_JSON` when it is set
    /// (how a machine in the cloud is handed the testnet deployments, which
    /// are never committed), otherwise from the deployment file once it
    /// appears.
    pub async fn wait_for_deployments(&self, client: &Client) -> Result<Deployments> {
        match deployment_from_env(&self.key)? {
            Some(value) => {
                let deployments: Deployments = serde_json::from_value(value).map_err(|error| {
                    anyhow!(
                        "{DEPLOYMENTS_JSON}'s deployment for {} is not an EVM one: {error}",
                        self.key
                    )
                })?;
                if deployments.chain_id != client.chain_id {
                    bail!(
                        "{DEPLOYMENTS_JSON} says {} is chain {}, and it answers as {}",
                        self.key,
                        deployments.chain_id,
                        client.chain_id
                    );
                }
                Ok(deployments)
            }
            None => client.wait_for_deployments(&self.deployments_file()).await,
        }
    }

    /// Connect to this target's Solana cluster, waiting for it to answer,
    /// and check it is the cluster its key names.
    pub async fn connect_solana(&self) -> Result<SolanaClient> {
        connect_solana(self.network, &self.key, &self.config).await
    }

    /// Connect to this Solana cluster and wait for its deployment, set up
    /// and checked against the cluster and the programs this build speaks
    /// to.
    pub async fn serve_solana(&self) -> Result<SolanaServed> {
        let client = self.connect_solana().await?;
        let deployments = match deployment_from_env(&self.key)? {
            Some(value) => {
                let deployments: SolanaDeployments =
                    serde_json::from_value(value).map_err(|error| {
                        anyhow!(
                            "{DEPLOYMENTS_JSON}'s deployment for {} is not a Solana one: {error}",
                            self.key
                        )
                    })?;
                client.check_deployments(&deployments).await?;
                deployments
            }
            None => {
                client
                    .wait_for_deployments(&self.deployments_file())
                    .await?
            }
        };
        let platform = Platform::from_deployments(&deployments)?;
        Ok(SolanaServed {
            vault_chain: self.vault_chain,
            key: self.key.clone(),
            client,
            deployments,
            platform,
            public_rpc: self.config.public_rpc.clone(),
        })
    }
}

/// One chain's deployment from `SHONIN_DEPLOYMENTS_JSON`, when it is set
/// (how a machine in the cloud is handed the testnet deployments, which are
/// never committed): the entry whose `chain_key` is `key`, as it is, for the
/// reader to read as its kind of chain's.
fn deployment_from_env(key: &ChainKey) -> Result<Option<serde_json::Value>> {
    let Some(json) = std::env::var(DEPLOYMENTS_JSON)
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        return Ok(None);
    };
    let all: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|error| anyhow!("{DEPLOYMENTS_JSON} is not a list of deployments: {error}"))?;
    let wanted = key.to_string();
    all.into_iter()
        .find(|entry| entry["chain_key"].as_str() == Some(wanted.as_str()))
        .map(Some)
        .ok_or_else(|| anyhow!("{DEPLOYMENTS_JSON} has no deployment for {wanted}"))
}

/// One Solana vault chain a service serves: its client, its deployment and
/// the platform's instructions on it.
#[derive(Clone)]
pub struct SolanaServed {
    pub vault_chain: Chain,
    pub key: ChainKey,
    pub client: SolanaClient,
    pub deployments: SolanaDeployments,
    pub platform: Platform,
    /// The cluster's RPC as the chain config file gives it, which may be
    /// shown to anyone.
    pub public_rpc: String,
}

/// Connect to a Solana cluster of a network, and check it is the cluster
/// its key names. The role keys are the network's: the development ones
/// locally, the team's on devnet, none on mainnet.
pub async fn connect_solana(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
) -> Result<SolanaClient> {
    let client = SolanaClient::connect(network, key, config).await?;
    tracing::info!(%network, chain = %key, label = config.label, "connected");
    Ok(client)
}

/// Every served chain's deployment, handed over whole in the environment as a
/// JSON list.
pub const DEPLOYMENTS_JSON: &str = "SHONIN_DEPLOYMENTS_JSON";

/// The mnemonic the testnet role accounts are derived from.
pub const TESTNET_EVM_MNEMONIC: &str = "SHONIN_TESTNET_EVM_MNEMONIC";

/// The keeper's own testnet key (a `0x` private key), for a keeper that
/// runs without the mnemonic: it signs as the keeper and nothing else.
pub const TESTNET_KEEPER_KEY: &str = "SHONIN_TESTNET_KEEPER_KEY";

fn mnemonic_is_set() -> bool {
    std::env::var(TESTNET_EVM_MNEMONIC).is_ok_and(|v| !v.trim().is_empty())
}

/// Who signs on a network: the node on local chains, the developer's
/// mnemonic on testnets, and nobody on mainnet, where clients only read.
/// A testnet without the mnemonic signs nothing here; see
/// [`signing_with_roles`].
pub fn signing_for(network: NetworkName) -> Result<Signing> {
    signing_with_roles(network, None)
}

/// Who signs on a network, given the role accounts its deployment names.
///
/// On a testnet with [`TESTNET_EVM_MNEMONIC`] set, every role signs with the
/// key it derives. Without it, the role accounts are `roles` (none if no
/// deployment is to hand) and the only key held is the keeper's, from
/// [`TESTNET_KEEPER_KEY`], refused unless it is the account the deployment
/// names as the keeper. With neither, the client only reads, and a send or
/// a signature as a role fails at the call.
pub fn signing_with_roles(network: NetworkName, roles: Option<&Roles>) -> Result<Signing> {
    match network {
        NetworkName::Local => Ok(Signing::Unlocked),
        NetworkName::Testnet => {
            if let Some(phrase) = std::env::var(TESTNET_EVM_MNEMONIC)
                .ok()
                .filter(|v| !v.trim().is_empty())
            {
                return Ok(Signing::Keys(keys_from_mnemonic(&phrase, ROLE_COUNT)?));
            }
            let keeper_key = std::env::var(TESTNET_KEEPER_KEY)
                .ok()
                .filter(|v| !v.trim().is_empty());
            roles_signing(roles, keeper_key.as_deref())
        }
        NetworkName::Mainnet => Ok(Signing::ReadOnly),
    }
}

/// The role accounts in role order ([`Role::account_index`]).
fn role_accounts(roles: &Roles) -> Vec<Address> {
    let mut accounts = vec![Address::ZERO; ROLE_COUNT];
    for (role, account) in [
        (Role::Admin, roles.admin),
        (Role::Platform, roles.platform),
        (Role::Attestor, roles.attestor),
        (Role::Builder, roles.builder),
        (Role::Reviewer, roles.reviewer),
        (Role::Executor, roles.executor),
        (Role::Keeper, roles.keeper),
        (Role::Operator, roles.operator),
        (Role::Author, roles.author),
        (Role::Viewer, roles.viewer),
    ] {
        accounts[role.account_index()] = account;
    }
    accounts
}

/// A testnet's signing without the mnemonic: `roles` for the accounts, and
/// the keeper's key alone if one is given and it is the deployment's keeper.
/// The key never appears in an error.
fn roles_signing(roles: Option<&Roles>, keeper_key: Option<&str>) -> Result<Signing> {
    let accounts = roles.map(role_accounts).unwrap_or_default();
    let Some(text) = keeper_key else {
        return Ok(Signing::Roles {
            accounts,
            keys: Vec::new(),
        });
    };
    let key: PrivateKeySigner = text
        .trim()
        .parse()
        .map_err(|_| anyhow!("{TESTNET_KEEPER_KEY} is not a 0x private key"))?;
    let Some(roles) = roles else {
        bail!(
            "{TESTNET_KEEPER_KEY} is checked against the keeper the deployment names, and no deployment is to hand: deploy first, or pass {DEPLOYMENTS_JSON}"
        );
    };
    if key.address() != roles.keeper {
        bail!(
            "{TESTNET_KEEPER_KEY} is the key of {}, and the deployment's keeper is {}: refused",
            key.address(),
            roles.keeper
        );
    }
    Ok(Signing::Roles {
        accounts,
        keys: vec![key],
    })
}

/// Connect to any EVM chain of a network, and check it is the chain its key
/// says.
pub async fn connect_to(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
) -> Result<Client> {
    connect_with(key, config, network, signing_for(network)?).await
}

/// [`connect_to`], signing as `signing` says.
pub async fn connect_with(
    key: &ChainKey,
    config: &ChainConfig,
    network: NetworkName,
    signing: Signing,
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
    let client = Client::connect(config, signing).await?;
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

    /// Chain args over a network of two EVM vault chains and a Solana one.
    fn args_naming(vault_chain: Option<Chain>) -> (ChainArgs, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "shonin-chain-args-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let chains = dir.join("chains.toml");
        std::fs::write(
            &chains,
            r#"
            [chains."evm:31337"]
            label = "Base (local)"
            rpc = "http://127.0.0.1:19100"
            dollar = "USDC"

            [chains."evm:31338"]
            label = "Hyperliquid (local)"
            rpc = "http://127.0.0.1:19101"
            dollar = "USDC"

            [chains."evm:31339"]
            label = "Robinhood (local)"
            rpc = "http://127.0.0.1:19102"
            dollar = "USDC"

            [chains."solana:localnet"]
            label = "Solana (local)"
            rpc = "http://127.0.0.1:19110"
            dollar = "USDC"

            [networks.local]
            base = "evm:31337"
            hyperliquid = "evm:31338"
            robinhood = "evm:31339"
            solana = "solana:localnet"
            "#,
        )
        .unwrap();
        let args = ChainArgs {
            network: NetworkName::Local,
            chains,
            deployments_dir: dir.join("deployments"),
            vault_chain,
        };
        (args, dir)
    }

    fn roles() -> Roles {
        let account = |byte| Address::repeat_byte(byte);
        Roles {
            admin: account(1),
            platform: account(2),
            attestor: account(3),
            builder: account(4),
            reviewer: account(5),
            executor: account(6),
            keeper: account(7),
            operator: account(8),
            author: account(9),
            viewer: account(10),
        }
    }

    #[test]
    fn without_the_mnemonic_the_role_accounts_are_the_deployment_s_and_no_key_is_held() {
        let Signing::Roles { accounts, keys } = roles_signing(Some(&roles()), None).unwrap() else {
            panic!("not role accounts");
        };
        assert!(keys.is_empty());
        assert_eq!(accounts.len(), ROLE_COUNT);
        assert_eq!(accounts[Role::Admin.account_index()], roles().admin);
        assert_eq!(accounts[Role::Keeper.account_index()], roles().keeper);
        assert_eq!(accounts[Role::Attestor.account_index()], roles().attestor);
        assert_eq!(accounts[Role::Platform.account_index()], roles().platform);
    }

    #[test]
    fn the_keeper_key_is_held_only_when_it_is_the_deployment_s_keeper() {
        let key = PrivateKeySigner::random();
        let text = format!("0x{}", alloy::hex::encode(key.to_bytes()));
        let mut ours = roles();
        ours.keeper = key.address();
        let Signing::Roles { keys, .. } = roles_signing(Some(&ours), Some(&text)).unwrap() else {
            panic!("not role accounts");
        };
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].address(), key.address());

        // Another account's key is refused, and the error never shows it.
        let error = format!(
            "{:#}",
            roles_signing(Some(&roles()), Some(&text)).err().unwrap()
        );
        assert!(error.contains("refused"), "{error}");
        assert!(!error.contains(&text[2..]), "the key is in the error");
        // No deployment to check it against: refused too.
        let error = format!("{:#}", roles_signing(None, Some(&text)).err().unwrap());
        assert!(!error.contains(&text[2..]), "the key is in the error");
        // Not a key at all: refused without echoing it.
        let error = format!(
            "{:#}",
            roles_signing(Some(&ours), Some("0xnot-a-key"))
                .err()
                .unwrap()
        );
        assert!(!error.contains("not-a-key"), "{error}");
    }

    #[test]
    fn a_solana_vault_chain_named_alone_is_served_with_no_evm_chain() {
        let (args, dir) = args_naming(Some(Chain::Solana));
        assert!(args.targets().unwrap().is_empty());
        let solana = args.solana_targets().unwrap();
        assert_eq!(solana.len(), 1);
        assert_eq!(solana[0].vault_chain, Chain::Solana);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_evm_vault_chain_is_served_when_none_is_named_and_solana_only_as_a_cluster() {
        let (args, dir) = args_naming(None);
        let evm: Vec<Chain> = args
            .targets()
            .unwrap()
            .iter()
            .map(|target| target.vault_chain)
            .collect();
        assert!(!evm.is_empty());
        assert!(!evm.contains(&Chain::Solana));
        let solana: Vec<Chain> = args
            .solana_targets()
            .unwrap()
            .iter()
            .map(|target| target.vault_chain)
            .collect();
        assert_eq!(solana, vec![Chain::Solana]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_service_speaking_only_to_evm_chains_refuses_a_solana_vault_chain_named_alone() {
        let (args, dir) = args_naming(Some(Chain::Solana));
        let error = format!("{:#}", args.connect_each().err().unwrap());
        assert!(error.contains("solana"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_solana_cluster_is_served_only_by_a_build_for_its_network() {
        use BuiltFor::*;
        for (built, network, serves) in [
            (Local, NetworkName::Local, true),
            (Local, NetworkName::Testnet, false),
            (Local, NetworkName::Mainnet, false),
            (Devnet, NetworkName::Local, false),
            (Devnet, NetworkName::Testnet, true),
            (Devnet, NetworkName::Mainnet, false),
            (Mainnet, NetworkName::Local, false),
            (Mainnet, NetworkName::Testnet, false),
            (Mainnet, NetworkName::Mainnet, true),
        ] {
            assert_eq!(
                built.serves(network).is_ok(),
                serves,
                "{built:?} on {network}"
            );
        }
    }

    #[test]
    fn a_refused_build_is_told_which_feature_to_build_with() {
        let testnet = format!(
            "{}",
            BuiltFor::Local.serves(NetworkName::Testnet).unwrap_err()
        );
        assert!(testnet.contains("devnet feature"), "{testnet}");
        let mainnet = format!(
            "{}",
            BuiltFor::Devnet.serves(NetworkName::Mainnet).unwrap_err()
        );
        assert!(mainnet.contains("mainnet feature"), "{mainnet}");
        let local = format!(
            "{}",
            BuiltFor::Devnet.serves(NetworkName::Local).unwrap_err()
        );
        assert!(
            local.contains("without the mainnet and devnet features"),
            "{local}"
        );
    }

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
