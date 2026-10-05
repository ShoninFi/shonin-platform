//! A network naming one chain for two vault chains is refused, and one chain
//! is one chain whatever the spelling of its id: `evm:031337` and `evm:+31337`
//! would both connect to chain 31337 and have it served twice (two sweeps
//! finding the same vaults, two keys for one vault at one nonce, two meters
//! writing one vault's total). So an EVM key is its chain id in plain
//! decimal, and nothing else: a key that names no id at all (`evm:local`)
//! could never be checked against the node it reaches.

use chain::config::{ChainConfigs, NetworkName};

fn no_env(_: &str) -> Option<String> {
    None
}

fn two_vault_chains_on(first: &str, second: &str) -> String {
    format!(
        r#"
        [chains."{first}"]
        label = "One local node"
        rpc = "http://127.0.0.1:19100"
        dollar = "USDC"

        [chains."{second}"]
        label = "The same node, spelt again"
        rpc = "http://127.0.0.1:19100"
        dollar = "USDG"

        [networks.local]
        base = "{first}"
        robinhood = "{second}"
        "#
    )
}

#[test]
fn one_chain_id_spelt_two_ways_is_not_served_for_two_vault_chains() {
    for second in ["evm:031337", "evm:+31337"] {
        let text = two_vault_chains_on("evm:31337", second);
        if let Ok(configs) = ChainConfigs::from_toml(&text, no_env) {
            let network = configs.network(NetworkName::Local).unwrap();
            let ids: Vec<(String, Option<u64>)> = network
                .iter()
                .map(|(chain, key)| (chain.to_string(), key.evm_chain_id()))
                .collect();
            panic!(
                "chain 31337 is named for two vault chains under the keys evm:31337 and {second}, \
                 and the config was accepted: {ids:?}"
            );
        }
    }
}

#[test]
fn an_evm_key_that_names_no_chain_id_is_refused() {
    let text = two_vault_chains_on("evm:local-a", "evm:local-b");
    assert!(
        ChainConfigs::from_toml(&text, no_env).is_err(),
        "EVM keys with no chain id were accepted, and no node could be checked against them"
    );
}

#[test]
fn two_distinct_chain_ids_are_both_served() {
    let text = two_vault_chains_on("evm:31337", "evm:31339");
    let configs = ChainConfigs::from_toml(&text, no_env).unwrap();
    assert_eq!(
        configs.network(NetworkName::Local).unwrap().iter().count(),
        2
    );
}
