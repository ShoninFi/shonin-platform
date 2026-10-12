//! One vault chain as the wallet code needs it, from the API's
//! `/api/{chain}/config`: its id, its public endpoint, its dollar, and on a
//! development chain the account it offers.

use alloy_primitives::Address;
use shared::{Chain, Config};

use super::{dev, networks};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub chain: Chain,
    pub chain_id: u64,
    /// "Base Sepolia", "Hyperliquid".
    pub label: String,
    /// The public endpoint: what the app reads through, what a local signer
    /// sends through, and what a wallet is given to add the chain.
    pub rpc: String,
    pub asset: Address,
    pub asset_symbol: String,
    pub asset_decimals: u8,
    /// On a development chain, the account it holds the key for.
    pub dev_account: Option<Address>,
}

/// Why a chain can't be used for money.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// Not an EVM chain (Solana comes with its own wallets).
    NotEvm,
    /// The config's endpoint isn't one the app may use.
    Endpoint,
    /// Something in the config didn't read.
    Malformed(&'static str),
}

impl Network {
    pub fn from_config(config: &Config) -> Result<Self, Unusable> {
        let evm = config.addresses.evm().ok_or(Unusable::NotEvm)?;
        let chain_id = config.chain.chain_id.ok_or(Unusable::NotEvm)?;
        let rpc = dev::local_rpc(chain_id).unwrap_or(config.rpc_url.as_str());
        if !networks::is_usable_endpoint(rpc) {
            return Err(Unusable::Endpoint);
        }
        let asset = evm
            .usd
            .parse()
            .map_err(|_| Unusable::Malformed("the dollar's address"))?;
        let dev_account = config
            .dev
            .as_ref()
            .map(|dev| dev.address.parse())
            .transpose()
            .map_err(|_| Unusable::Malformed("the development account"))?;
        Ok(Self {
            chain: config.chain.chain,
            chain_id,
            label: config.chain.chain.label().to_string(),
            rpc: rpc.to_string(),
            asset,
            asset_symbol: config.asset_symbol.clone(),
            asset_decimals: config.asset_decimals,
            dev_account,
        })
    }

    /// The chain's coin, which pays the network fee.
    pub fn coin(&self) -> &'static str {
        networks::coin(self.chain_id).symbol
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(rpc: &str) -> Config {
        serde_json::from_value(serde_json::json!({
            "chain": {"chain": "base", "key": "evm:84532", "chain_id": 84532, "block": 1},
            "rpc_url": rpc,
            "addresses": {
                "usd": format!("0x{}", "2b".repeat(20)),
                "publishing": format!("0x{}", "01".repeat(20)),
                "prices": format!("0x{}", "01".repeat(20)),
                "venue": format!("0x{}", "01".repeat(20)),
                "attestation": format!("0x{}", "01".repeat(20)),
                "strategies": format!("0x{}", "01".repeat(20)),
                "reviews": format!("0x{}", "01".repeat(20)),
                "profiles": format!("0x{}", "01".repeat(20)),
                "factory": format!("0x{}", "01".repeat(20)),
                "station_factory": format!("0x{}", "01".repeat(20)),
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
    fn a_testnet_config_gives_its_chain_id_endpoint_and_dollar() {
        let network = Network::from_config(&config("https://sepolia.base.org")).unwrap();
        assert_eq!(network.chain_id, 84532);
        assert_eq!(network.rpc, "https://sepolia.base.org");
        assert_eq!(network.asset, Address::repeat_byte(0x2b));
        assert_eq!(network.asset_decimals, 6);
        assert_eq!(network.dev_account, None);
        assert_eq!(network.coin(), "ETH");
    }

    #[test]
    fn an_endpoint_the_app_may_not_use_is_refused() {
        assert_eq!(
            Network::from_config(&config("http://rpc.example.com/key")),
            Err(Unusable::Endpoint)
        );
    }
}
