//! Which chains exist, where they answer, and which of them a network means.
//!
//! `config/chains.toml` lists every chain the platform knows, keyed
//! `kind:id` (`evm:8453`, `solana:mainnet-beta`), and maps each network
//! (`local`, `testnet`, `mainnet`) to the chain behind each vault chain name.
//! An endpoint can be overridden from the environment, because a paid RPC URL
//! carries a key and so never goes in the file.
//!
//! Parsing and lookup work on every target, so the web app can read the same
//! file the services do; only [`ChainConfigs::load`] touches the filesystem
//! and the process environment, and it sits behind the `rpc` feature.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use shared::Chain;

/// What kind of chain it is, which decides how it is spoken to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChainKind {
    Evm,
    Solana,
}

impl ChainKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Evm => "evm",
            Self::Solana => "solana",
        }
    }
}

impl FromStr for ChainKind {
    type Err = ConfigError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "evm" => Ok(Self::Evm),
            "solana" => Ok(Self::Solana),
            other => Err(ConfigError::new(format!(
                "no chain kind is called {other:?}: evm or solana"
            ))),
        }
    }
}

/// A chain, named the way `chains.toml` names it: `kind:id`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainKey {
    pub kind: ChainKind,
    /// An EVM chain id, or a Solana cluster's name.
    pub id: String,
}

impl ChainKey {
    pub fn new(kind: ChainKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// The EVM chain id, which a node reports and a deployment file records.
    pub fn evm_chain_id(&self) -> Option<u64> {
        match self.kind {
            ChainKind::Evm => self.id.parse().ok(),
            ChainKind::Solana => None,
        }
    }

    /// Whether two keys name one chain: an EVM key by its number, so
    /// `evm:31337` and a hand-built `evm:031337` are the same chain, any
    /// other key by its name.
    pub fn same_chain(&self, other: &ChainKey) -> bool {
        self.canonical() == other.canonical()
    }

    /// The key as the chain it names, whatever the spelling: what services
    /// dedupe their chains by.
    pub fn canonical(&self) -> String {
        match self.evm_chain_id() {
            Some(id) => format!("{}:{id}", self.kind.name()),
            None => self.to_string(),
        }
    }

    /// The key as a file name: `evm-31337`, `solana-localnet`.
    pub fn file_stem(&self) -> String {
        format!("{}-{}", self.kind.name(), self.id)
    }

    /// The part of an override variable's name that names this chain:
    /// `EVM_8453`, `SOLANA_MAINNET_BETA`.
    pub fn env_suffix(&self) -> String {
        let id: String = self
            .id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}_{id}", self.kind.name().to_ascii_uppercase())
    }
}

impl fmt::Display for ChainKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.name(), self.id)
    }
}

impl FromStr for ChainKey {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (kind, id) = text
            .split_once(':')
            .ok_or_else(|| ConfigError::new(format!("{text:?} is not a chain key (kind:id)")))?;
        if id.is_empty() {
            return Err(ConfigError::new(format!(
                "{text:?} has no id after the kind"
            )));
        }
        let kind: ChainKind = kind.parse()?;
        // An EVM key is the number the node answers as, written plainly: a
        // name could never be checked against the node, and a second
        // spelling of one number (`031337`, `+31337`) would let one chain be
        // served under two keys.
        if kind == ChainKind::Evm && !is_plain_decimal_u64(id) {
            return Err(ConfigError::new(format!(
                "{text:?}: an EVM chain's id is its chain id in plain decimal, with no sign or leading zero"
            )));
        }
        Ok(Self::new(kind, id))
    }
}

/// Digits only, no leading zero (bar `0` itself), and within a u64.
fn is_plain_decimal_u64(id: &str) -> bool {
    id.bytes().all(|b| b.is_ascii_digit())
        && (id == "0" || !id.starts_with('0'))
        && id.parse::<u64>().is_ok()
}

impl Serialize for ChainKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ChainKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a chain answers, and what it is called.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainConfig {
    pub label: String,
    pub rpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws: Option<String>,
    /// The dollar a vault on this chain settles in.
    pub dollar: String,
    /// What the chain's native coin is taken to be worth in dollars, for
    /// turning the gas an executor spends into the author's bill. A chain
    /// without one bills no gas at all: an author never pays for gas nobody
    /// priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_usd: Option<f64>,
    /// How many blocks deep a log must be before it is acted on, so one in a
    /// block a reorg replaces, or one a node behind the head has not got
    /// yet, is not read past. Unset, the reader's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmations: Option<u64>,
    /// The `rpc` the file gives, before any override from the environment:
    /// the one that may be handed to a browser. An override is usually a
    /// paid URL, which carries its key.
    #[serde(skip)]
    pub public_rpc: String,
}

impl ChainConfig {
    /// The first of the chain's endpoints. `rpc` may name several, separated
    /// by spaces, which the client spreads its requests over; a tool that
    /// takes one URL, or a browser, is given the first.
    pub fn first_rpc(&self) -> &str {
        self.rpc.split_whitespace().next().unwrap_or_default()
    }
}

/// Which set of chains: the ones `task dev` starts, the public testnets, or
/// the mainnets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NetworkName {
    Local,
    Testnet,
    Mainnet,
}

impl NetworkName {
    pub const ALL: [NetworkName; 3] = [Self::Local, Self::Testnet, Self::Mainnet];

    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

impl fmt::Display for NetworkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for NetworkName {
    type Err = ConfigError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|network| network.name() == name)
            .ok_or_else(|| {
                ConfigError::new(format!(
                    "no network is called {name:?}: local, testnet or mainnet"
                ))
            })
    }
}

/// One network: the chain each vault chain name means in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub name: NetworkName,
    chains: BTreeMap<Chain, ChainKey>,
}

impl Network {
    pub fn get(&self, chain: Chain) -> Option<&ChainKey> {
        self.chains.get(&chain)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Chain, &ChainKey)> {
        self.chains.iter().map(|(chain, key)| (*chain, key))
    }
}

/// Every chain the platform knows, and every network.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainConfigs {
    chains: BTreeMap<ChainKey, ChainConfig>,
    networks: BTreeMap<NetworkName, Network>,
}

/// The file, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    chains: BTreeMap<String, ChainConfig>,
    #[serde(default)]
    networks: BTreeMap<String, BTreeMap<String, String>>,
}

impl ChainConfigs {
    /// Read `chains.toml`, applying overrides from `env`: a function from a
    /// variable's name to its value, so a caller decides where they come
    /// from (the process environment, or nothing at all).
    pub fn from_toml(
        text: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let file: File = toml::from_str(text)
            .map_err(|error| ConfigError::new(format!("chains.toml: {error}")))?;

        let mut chains: BTreeMap<ChainKey, ChainConfig> = BTreeMap::new();
        for (key, mut config) in file.chains {
            let key: ChainKey = key.parse()?;
            if let Some(other) = chains.keys().find(|known| known.same_chain(&key)) {
                return Err(ConfigError::new(format!(
                    "chains.toml lists one chain as both {other} and {key}"
                )));
            }
            // The executor turns gas into the author's bill at this price:
            // anything but a finite number at or above zero would bill
            // nonsense (a NaN slips past every cap compared against it).
            if let Some(price) = config.native_usd
                && !(price.is_finite() && price >= 0.0)
            {
                return Err(ConfigError::new(format!(
                    "chains.toml: {key}'s native_usd is {price}; it must be a finite number of dollars, at least zero"
                )));
            }
            let suffix = key.env_suffix();
            config.public_rpc = config.first_rpc().to_string();
            if let Some(rpc) = env(&format!("SHONIN_RPC_{suffix}")).filter(|v| !v.is_empty()) {
                config.rpc = rpc;
            }
            if let Some(ws) = env(&format!("SHONIN_WS_{suffix}")).filter(|v| !v.is_empty()) {
                config.ws = Some(ws);
            }
            chains.insert(key, config);
        }

        let mut networks = BTreeMap::new();
        for (name, entries) in file.networks {
            let name: NetworkName = name.parse()?;
            let mut map: BTreeMap<Chain, ChainKey> = BTreeMap::new();
            for (vault_chain, key) in entries {
                let vault_chain: Chain = vault_chain.parse().map_err(ConfigError::new)?;
                let key: ChainKey = key.parse()?;
                if !chains.contains_key(&key) {
                    return Err(ConfigError::new(format!(
                        "network {name} names {key} for {vault_chain}, and no chain has that key"
                    )));
                }
                // A chain named for two vault chains would be served twice:
                // two sweeps, two keys at one nonce, two meters for a vault.
                if let Some((other, _)) = map.iter().find(|(_, named)| named.same_chain(&key)) {
                    return Err(ConfigError::new(format!(
                        "network {name} names {key} for both {other} and {vault_chain}; each vault chain needs a chain of its own"
                    )));
                }
                map.insert(vault_chain, key);
            }
            networks.insert(name, Network { name, chains: map });
        }

        Ok(Self { chains, networks })
    }

    /// Read the file at `path`, with overrides from the process environment.
    #[cfg(feature = "rpc")]
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the chain config at {}", path.display()))?;
        Ok(Self::from_toml(&text, |name| std::env::var(name).ok())?)
    }

    pub fn get(&self, key: &ChainKey) -> Option<&ChainConfig> {
        self.chains.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ChainKey, &ChainConfig)> {
        self.chains.iter()
    }

    pub fn network(&self, name: NetworkName) -> Option<&Network> {
        self.networks.get(&name)
    }

    /// The chain a vault chain name means on a network, and its config.
    pub fn resolve(
        &self,
        network: NetworkName,
        chain: Chain,
    ) -> Result<(&ChainKey, &ChainConfig), ConfigError> {
        let key = self
            .network(network)
            .ok_or_else(|| ConfigError::new(format!("chains.toml has no network {network}")))?
            .get(chain)
            .ok_or_else(|| {
                ConfigError::new(format!(
                    "network {network} does not say which chain {chain} is"
                ))
            })?;
        let config = self
            .get(key)
            .ok_or_else(|| ConfigError::new(format!("no chain has the key {key}")))?;
        Ok((key, config))
    }
}

/// Something wrong with the chain config, said plainly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
        [chains."evm:31337"]
        label = "Base (local)"
        rpc = "http://127.0.0.1:19100"
        dollar = "USDC"

        [chains."evm:8453"]
        label = "Base"
        rpc = "https://mainnet.base.org"
        dollar = "USDC"

        [chains."solana:mainnet-beta"]
        label = "Solana"
        rpc = "https://api.mainnet-beta.solana.com"
        ws = "wss://api.mainnet-beta.solana.com"
        dollar = "USDC"

        [networks.local]
        base = "evm:31337"

        [networks.mainnet]
        base = "evm:8453"
        solana = "solana:mainnet-beta"
    "#;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_key_reads_and_prints_as_kind_colon_id() {
        let key: ChainKey = "evm:8453".parse().unwrap();
        assert_eq!(key, ChainKey::new(ChainKind::Evm, "8453"));
        assert_eq!(key.to_string(), "evm:8453");
        assert_eq!(key.evm_chain_id(), Some(8453));
        assert_eq!(key.file_stem(), "evm-8453");

        let key: ChainKey = "solana:mainnet-beta".parse().unwrap();
        assert_eq!(key.kind, ChainKind::Solana);
        assert_eq!(key.evm_chain_id(), None);
        assert_eq!(key.env_suffix(), "SOLANA_MAINNET_BETA");
    }

    #[test]
    fn a_malformed_key_is_refused() {
        assert!("8453".parse::<ChainKey>().is_err());
        assert!("evm:".parse::<ChainKey>().is_err());
        assert!("cosmos:hub".parse::<ChainKey>().is_err());
        for id in [
            "local",
            "031337",
            "+31337",
            "-1",
            " 1",
            "18446744073709551616",
        ] {
            assert!(format!("evm:{id}").parse::<ChainKey>().is_err(), "{id}");
        }
        assert!("evm:0".parse::<ChainKey>().is_ok());
        assert!("evm:18446744073709551615".parse::<ChainKey>().is_ok());
    }

    #[test]
    fn a_network_resolves_a_vault_chain_to_its_config() {
        let configs = ChainConfigs::from_toml(TOML, no_env).unwrap();
        let (key, config) = configs.resolve(NetworkName::Local, Chain::Base).unwrap();
        assert_eq!(key.to_string(), "evm:31337");
        assert_eq!(config.rpc, "http://127.0.0.1:19100");

        let (key, config) = configs
            .resolve(NetworkName::Mainnet, Chain::Solana)
            .unwrap();
        assert_eq!(key.to_string(), "solana:mainnet-beta");
        assert_eq!(
            config.ws.as_deref(),
            Some("wss://api.mainnet-beta.solana.com")
        );

        assert!(configs.resolve(NetworkName::Local, Chain::Solana).is_err());
        assert!(configs.resolve(NetworkName::Testnet, Chain::Base).is_err());
        assert_eq!(configs.iter().count(), 3);
    }

    #[test]
    fn the_environment_overrides_an_endpoint() {
        let env = |name: &str| match name {
            "SHONIN_RPC_EVM_8453" => Some("https://paid.example/key".to_string()),
            "SHONIN_WS_EVM_8453" => Some("wss://paid.example/key".to_string()),
            "SHONIN_RPC_EVM_31337" => Some(String::new()),
            _ => None,
        };
        let configs = ChainConfigs::from_toml(TOML, env).unwrap();
        let base = configs.get(&"evm:8453".parse().unwrap()).unwrap();
        assert_eq!(base.rpc, "https://paid.example/key");
        assert_eq!(base.ws.as_deref(), Some("wss://paid.example/key"));
        // What may be shown to anyone is still the file's own.
        assert_eq!(base.public_rpc, "https://mainnet.base.org");
        // An empty variable is the same as an unset one.
        let local = configs.get(&"evm:31337".parse().unwrap()).unwrap();
        assert_eq!(local.rpc, "http://127.0.0.1:19100");
    }

    #[test]
    fn a_network_naming_an_unknown_chain_is_refused() {
        let text = r#"
            [chains."evm:1"]
            label = "One"
            rpc = "http://one"
            dollar = "USDC"

            [networks.local]
            base = "evm:2"
        "#;
        assert!(ChainConfigs::from_toml(text, no_env).is_err());
        let text = text.replace("base = \"evm:2\"", "polygon = \"evm:1\"");
        assert!(ChainConfigs::from_toml(&text, no_env).is_err());
    }

    #[test]
    fn a_network_naming_one_chain_for_two_vault_chains_is_refused() {
        let text = r#"
            [chains."evm:1"]
            label = "One"
            rpc = "http://one"
            dollar = "USDC"

            [networks.local]
            base = "evm:1"
            robinhood = "evm:1"
        "#;
        let error = ChainConfigs::from_toml(text, no_env)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("evm:1") && error.contains("base") && error.contains("robinhood"),
            "{error}"
        );
    }

    #[test]
    fn the_committed_file_parses_and_every_network_is_complete() {
        let text = include_str!("../../../config/chains.toml");
        let configs = ChainConfigs::from_toml(text, no_env).unwrap();
        for network in NetworkName::ALL {
            for chain in Chain::ALL {
                let (key, config) = configs.resolve(network, chain).unwrap();
                assert_eq!(config.dollar, chain.dollar(), "{network} {chain} {key}");
                let expected = if chain == Chain::Solana {
                    ChainKind::Solana
                } else {
                    ChainKind::Evm
                };
                assert_eq!(key.kind, expected, "{network} {chain}");
            }
        }
    }
}
