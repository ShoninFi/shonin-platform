//! A network naming one chain for two vault chains (`base = "evm:31337"` and
//! `robinhood = "evm:31337"`, say, for a devnet run on one node) would have
//! each service serve that chain twice: two sweeps finding the same vaults,
//! two keys for one vault sending at the same nonce, two meters writing one
//! vault's total. The chain config is refused when it is read, naming both.

#![cfg(feature = "args")]

use chain::config::NetworkName;

#[test]
fn a_chain_named_for_two_vault_chains_is_not_served_twice() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-chain-twice-{}-{}",
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
        label = "One local node"
        rpc = "http://127.0.0.1:19100"
        dollar = "USDC"

        [chains."evm:31338"]
        label = "Hyperliquid (local)"
        rpc = "http://127.0.0.1:19101"
        dollar = "USDC"

        [chains."solana:localnet"]
        label = "Solana (local)"
        rpc = "http://127.0.0.1:19110"
        dollar = "USDC"

        [networks.local]
        base = "evm:31337"
        hyperliquid = "evm:31338"
        robinhood = "evm:31337"
        solana = "solana:localnet"
        "#,
    )
    .unwrap();

    let args = chain::ChainArgs {
        network: NetworkName::Local,
        chains,
        deployments_dir: dir.join("deployments"),
        vault_chain: None,
    };
    let error = match args.targets() {
        Ok(targets) => panic!(
            "one chain is served for two vault chains: {:?}",
            targets
                .iter()
                .map(|t| format!("{} -> {}", t.vault_chain, t.key))
                .collect::<Vec<_>>()
        ),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("evm:31337") && error.contains("base") && error.contains("robinhood"),
        "the refusal names the chain and both vault chains: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
