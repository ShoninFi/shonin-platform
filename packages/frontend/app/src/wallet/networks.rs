//! The vault chains a wallet is asked to be on: what each is called, its
//! public endpoint (the one the API names, which is the chain config file's
//! own and never a keyed override), its explorer and its coin, which a wallet
//! needs to add a chain it doesn't know (EIP-3085).

use alloy_primitives::{Address, B256};
use serde_json::json;

/// A chain's coin, as `wallet_addEthereumChain` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coin {
    pub name: &'static str,
    pub symbol: &'static str,
    pub decimals: u8,
}

const ETH: Coin = Coin {
    name: "Ether",
    symbol: "ETH",
    decimals: 18,
};
const HYPE: Coin = Coin {
    name: "HYPE",
    symbol: "HYPE",
    decimals: 18,
};

/// What the app knows of an EVM chain beyond the API's config: its explorer
/// and its coin. The endpoints come from the chain config through the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub chain_id: u64,
    /// The name a wallet shows when it adds the chain.
    pub name: &'static str,
    pub explorer: Option<&'static str>,
    pub coin: Coin,
}

/// Every EVM chain a vault can be on: the local ones `task dev` starts, the
/// testnets and the mainnets.
pub const KNOWN: [Known; 9] = [
    Known {
        chain_id: 31337,
        name: "Base (local)",
        explorer: None,
        coin: ETH,
    },
    Known {
        chain_id: 31338,
        name: "Hyperliquid (local)",
        explorer: None,
        coin: HYPE,
    },
    Known {
        chain_id: 31339,
        name: "Robinhood Chain (local)",
        explorer: None,
        coin: ETH,
    },
    Known {
        chain_id: 84532,
        name: "Base Sepolia",
        explorer: Some("https://sepolia.basescan.org"),
        coin: ETH,
    },
    Known {
        chain_id: 998,
        name: "Hyperliquid EVM Testnet",
        explorer: Some("https://testnet.purrsec.info"),
        coin: HYPE,
    },
    Known {
        chain_id: 46630,
        name: "Robinhood Chain Testnet",
        explorer: Some("https://explorer.testnet.chain.robinhood.com"),
        coin: ETH,
    },
    Known {
        chain_id: 8453,
        name: "Base",
        explorer: Some("https://basescan.org"),
        coin: ETH,
    },
    Known {
        chain_id: 999,
        name: "HyperEVM",
        explorer: Some("https://hyperevmscan.io"),
        coin: HYPE,
    },
    Known {
        chain_id: 4663,
        name: "Robinhood Chain",
        explorer: Some("https://explorer.chain.robinhood.com"),
        coin: ETH,
    },
];

pub fn known(chain_id: u64) -> Option<&'static Known> {
    KNOWN.iter().find(|known| known.chain_id == chain_id)
}

/// The coin that pays the network fee on a chain: ETH unless known otherwise.
pub fn coin(chain_id: u64) -> Coin {
    known(chain_id).map(|known| known.coin).unwrap_or(ETH)
}

/// A chain's name for people: "Base Sepolia". Unknown chains by number.
pub fn chain_name(chain_id: u64) -> String {
    known(chain_id)
        .map(|known| known.name.to_string())
        .unwrap_or_else(|| format!("chain {chain_id}"))
}

/// A transaction's page on the chain's explorer.
pub fn tx_url(chain_id: u64, hash: B256) -> Option<String> {
    Some(format!("{}/tx/{hash:?}", known(chain_id)?.explorer?))
}

/// An address's page on the chain's explorer.
pub fn address_url(chain_id: u64, address: Address) -> Option<String> {
    Some(format!(
        "{}/address/{address:?}",
        known(chain_id)?.explorer?
    ))
}

/// The chain id as a wallet takes it: `0x` and lower-case hex.
pub fn hex_id(chain_id: u64) -> String {
    format!("{chain_id:#x}")
}

/// What `wallet_addEthereumChain` is given: the public endpoint and the
/// explorer, never anything keyed.
pub fn add_chain_params(chain_id: u64, label: &str, rpc: &str) -> serde_json::Value {
    let known = known(chain_id);
    let coin = coin(chain_id);
    let mut params = json!({
        "chainId": hex_id(chain_id),
        "chainName": known.map(|known| known.name).unwrap_or(label),
        "nativeCurrency": {
            "name": coin.name,
            "symbol": coin.symbol,
            "decimals": coin.decimals,
        },
        "rpcUrls": [rpc],
    });
    if let Some(explorer) = known.and_then(|known| known.explorer) {
        params["blockExplorerUrls"] = json!([explorer]);
    }
    params
}

/// Whether an endpoint may be handed to a browser wallet or used for local
/// signing: https anywhere, or plain http only to this machine (the local
/// chains). A keyed URL is never in the app's config to begin with; this is
/// the last check before one would be used.
pub fn is_usable_endpoint(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty();
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        return host == "127.0.0.1" || host == "localhost";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_a_wallet_doesnt_know_is_added_with_its_public_endpoint_explorer_and_coin() {
        let params = add_chain_params(84532, "Base Sepolia", "https://sepolia.base.org");
        assert_eq!(params["chainId"], "0x14a34");
        assert_eq!(params["chainName"], "Base Sepolia");
        assert_eq!(params["nativeCurrency"]["symbol"], "ETH");
        assert_eq!(params["nativeCurrency"]["decimals"], 18);
        assert_eq!(params["rpcUrls"][0], "https://sepolia.base.org");
        assert_eq!(
            params["blockExplorerUrls"][0],
            "https://sepolia.basescan.org"
        );
        // A local chain has no explorer to name.
        let params = add_chain_params(31338, "Hyperliquid (local)", "http://127.0.0.1:19221");
        assert!(params.get("blockExplorerUrls").is_none());
        assert_eq!(params["nativeCurrency"]["symbol"], "HYPE");
    }

    #[test]
    fn transaction_links_go_to_the_chains_own_explorer() {
        let hash = B256::repeat_byte(0xab);
        assert_eq!(
            tx_url(84532, hash).unwrap(),
            format!("https://sepolia.basescan.org/tx/0x{}", "ab".repeat(32))
        );
        assert_eq!(tx_url(31337, hash), None);
        assert_eq!(tx_url(1, hash), None);
    }

    #[test]
    fn only_https_or_this_machine_is_an_endpoint_the_app_uses() {
        assert!(is_usable_endpoint("https://sepolia.base.org"));
        assert!(is_usable_endpoint("http://127.0.0.1:19220"));
        assert!(is_usable_endpoint("http://localhost:19220/"));
        assert!(!is_usable_endpoint("http://rpc.example.com"));
        assert!(!is_usable_endpoint("http://127.0.0.1.example.com"));
        assert!(!is_usable_endpoint("ws://127.0.0.1:19231"));
        assert!(!is_usable_endpoint("https://"));
        assert!(!is_usable_endpoint(""));
    }
}
