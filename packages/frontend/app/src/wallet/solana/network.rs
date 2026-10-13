//! The Solana cluster as the wallet code needs it, from the API's
//! `/api/solana/config`: its name, its public endpoint, its dollar, and on a
//! development cluster the account it offers. The programs' ids are compiled
//! into this build (the `devnet` feature names the testnet's): a config that
//! names other programs is refused, so nothing is ever built for one
//! deployment and sent to another.

use chain::solana_ix::{Pubkey, ids};
use shared::{Chain, Config};

use crate::wallet::config_cache::ConfigId;

/// The Solana cluster vaults live on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolNetwork {
    /// `localnet`, `devnet`, `mainnet-beta`.
    pub cluster: String,
    /// "Solana devnet".
    pub label: String,
    /// The public endpoint: what the app reads through, and what the phrase
    /// wallet sends through.
    pub rpc: String,
    /// The dollar.
    pub mint: Pubkey,
    pub asset_symbol: String,
    pub asset_decimals: u8,
    /// On a development cluster, the account the API names for the
    /// development wallet.
    pub dev_account: Option<Pubkey>,
    /// The config it was read from: a flow stops if the cluster it would
    /// sign with isn't the one its review was computed with.
    pub config: ConfigId,
}

/// Why the Solana config can't be used for money.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    NotSolana,
    /// The endpoint isn't one the app may use.
    Endpoint,
    /// The config names other programs than this build's.
    OtherDeployment(&'static str),
    Malformed(&'static str),
}

/// Each program the app sends to, by the name the config gives it, and the
/// id this build compiled in.
const PROGRAMS: [(&str, Pubkey); 5] = [
    ("shonin_vault", ids::VAULT),
    ("shonin_station", ids::STATION),
    ("shonin_publishing", ids::PUBLISHING),
    ("shonin_registry", ids::REGISTRY),
    ("shonin_prices", ids::PRICES),
];

impl SolNetwork {
    /// The cluster a config's text names (as the page holds it, see
    /// [`crate::wallet::config_cache::canonical`]), if this build can use it.
    pub fn from_raw(raw: &str) -> Option<Self> {
        let config: Config = serde_json::from_str(raw).ok()?;
        let network = Self::from_config(&config).ok()?;
        Some(Self {
            config: ConfigId::of(raw),
            ..network
        })
    }

    /// The cluster a parsed config names; its [`SolNetwork::config`] is left
    /// unset (the text it came from isn't known here).
    pub fn from_config(config: &Config) -> Result<Self, Unusable> {
        if config.chain.chain != Chain::Solana {
            return Err(Unusable::NotSolana);
        }
        let addresses = config.addresses.solana().ok_or(Unusable::NotSolana)?;
        for (name, id) in PROGRAMS {
            let named: Pubkey = addresses
                .programs
                .get(name)
                .ok_or(Unusable::Malformed("a program's id"))?
                .parse()
                .map_err(|_| Unusable::Malformed("a program's id"))?;
            if named != id {
                return Err(Unusable::OtherDeployment(name));
            }
        }
        if addresses.token_program != ids::TOKEN.to_string() {
            return Err(Unusable::Malformed("the dollar's token program"));
        }
        let rpc = super::dev::local_rpc(&addresses.cluster).unwrap_or(config.rpc_url.as_str());
        if !crate::wallet::networks::is_usable_endpoint(rpc) {
            return Err(Unusable::Endpoint);
        }
        let dev_account = config
            .dev
            .as_ref()
            .map(|dev| dev.address.parse())
            .transpose()
            .map_err(|_| Unusable::Malformed("the development account"))?;
        Ok(Self {
            cluster: addresses.cluster.clone(),
            label: label(&addresses.cluster),
            rpc: rpc.to_string(),
            mint: addresses
                .mint
                .parse()
                .map_err(|_| Unusable::Malformed("the dollar"))?,
            asset_symbol: config.asset_symbol.clone(),
            asset_decimals: config.asset_decimals,
            dev_account,
            config: ConfigId::default(),
        })
    }

    /// The chain as the Wallet Standard names it: `solana:devnet`.
    pub fn wallet_chain(&self) -> String {
        wallet_chain(&self.cluster)
    }

    /// The cluster's coin, which pays the network fee.
    pub fn coin(&self) -> &'static str {
        "SOL"
    }

    /// A transaction on the explorer.
    pub fn tx_url(&self, signature: &str) -> String {
        format!(
            "https://explorer.solana.com/tx/{signature}{}",
            self.explorer_cluster()
        )
    }

    /// An account on the explorer.
    pub fn address_url(&self, address: &Pubkey) -> String {
        format!(
            "https://explorer.solana.com/address/{address}{}",
            self.explorer_cluster()
        )
    }

    fn explorer_cluster(&self) -> String {
        match self.cluster.as_str() {
            "mainnet" | "mainnet-beta" => String::new(),
            "devnet" | "testnet" => format!("?cluster={}", self.cluster),
            // The explorer reads a local cluster from its own address.
            _ => format!("?cluster=custom&customUrl={}", url_encode(&self.rpc)),
        }
    }
}

/// The Wallet Standard's name for a cluster.
pub fn wallet_chain(cluster: &str) -> String {
    match cluster {
        "mainnet" | "mainnet-beta" => "solana:mainnet".to_string(),
        other => format!("solana:{other}"),
    }
}

fn label(cluster: &str) -> String {
    match cluster {
        "mainnet" | "mainnet-beta" => "Solana".to_string(),
        "localnet" => "Solana (local)".to_string(),
        other => format!("Solana {other}"),
    }
}

fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(rpc: &str, vault: Pubkey) -> Config {
        serde_json::from_value(serde_json::json!({
            "chain": {"chain": "solana", "key": "solana:devnet", "block": 1},
            "rpc_url": rpc,
            "addresses": {
                "cluster": "devnet",
                "programs": {
                    "shonin_vault": vault.to_string(),
                    "shonin_station": ids::STATION.to_string(),
                    "shonin_publishing": ids::PUBLISHING.to_string(),
                    "shonin_registry": ids::REGISTRY.to_string(),
                    "shonin_prices": ids::PRICES.to_string(),
                },
                "mint": Pubkey::new_from_array([7; 32]).to_string(),
                "token_program": ids::TOKEN.to_string(),
                "vault_factory": Pubkey::new_from_array([1; 32]).to_string(),
                "publishing_config": Pubkey::new_from_array([2; 32]).to_string(),
            },
            "asset_symbol": "USDC",
            "asset_decimals": 6,
            "terms": {
                "minimum_tank": 10.0, "publish_fee": 1.0,
                "fees": {"holding_bps": 100, "author_bps": 1000, "platform_bps": 500, "transaction_bps": 10},
                "exit_band_bps": 100, "freeze_after_days": 7.0, "image_timelock_days": 2.0,
                "minimum_deposit": 10.0, "terms_hash": format!("0x{}", "00".repeat(32)),
            },
            "dev": null,
        }))
        .unwrap()
    }

    #[test]
    fn a_devnet_config_gives_its_cluster_endpoint_dollar_and_wallet_chain() {
        let network =
            SolNetwork::from_config(&config("https://api.devnet.solana.com", ids::VAULT)).unwrap();
        assert_eq!(network.cluster, "devnet");
        assert_eq!(network.wallet_chain(), "solana:devnet");
        assert_eq!(network.rpc, "https://api.devnet.solana.com");
        assert_eq!(network.mint, Pubkey::new_from_array([7; 32]));
        assert_eq!(network.dev_account, None);
        assert_eq!(
            network.tx_url("abc"),
            "https://explorer.solana.com/tx/abc?cluster=devnet"
        );
    }

    #[test]
    fn a_config_naming_another_vault_program_is_refused() {
        assert_eq!(
            SolNetwork::from_config(&config(
                "https://api.devnet.solana.com",
                Pubkey::new_from_array([9; 32])
            )),
            Err(Unusable::OtherDeployment("shonin_vault"))
        );
    }

    #[test]
    fn an_endpoint_the_app_may_not_use_is_refused() {
        assert_eq!(
            SolNetwork::from_config(&config("http://rpc.example.com/key", ids::VAULT)),
            Err(Unusable::Endpoint)
        );
    }

    #[test]
    fn mainnet_is_the_wallet_standards_solana_mainnet() {
        assert_eq!(wallet_chain("mainnet-beta"), "solana:mainnet");
        assert_eq!(wallet_chain("localnet"), "solana:localnet");
    }
}
