//! The development wallet, and what only a development build compiles in.
//!
//! On a development chain (the API says so by naming an account in its
//! config) the wallet is that account, whose key the chain itself holds: the
//! app asks the chain to sign. Elsewhere, a build with the `dev-wallet`
//! feature signs with the test wallet's recovery phrase, read from
//! `SHONIN_DEV_WALLET_MNEMONIC` when the app is compiled. Only
//! `task frontend:app:dev` turns the feature on. Task loads `.env` for every
//! command, release builds included, so the feature is the gate, never
//! whether the variable is set; and the release build refuses a dist that
//! carries the phrase.

/// The test wallet's phrase, compiled in by the `dev-wallet` feature only.
#[cfg(feature = "dev-wallet")]
pub const PHRASE: Option<&str> = option_env!("SHONIN_DEV_WALLET_MNEMONIC");
#[cfg(not(feature = "dev-wallet"))]
pub const PHRASE: Option<&str> = None;

/// Whether this build offers the development wallet at all: only a build
/// with the `dev` or `dev-wallet` feature, and the crate's own tests. A
/// release build never offers it, whatever account an API's config names.
pub const DEV_WALLET: bool = cfg!(any(test, feature = "dev", feature = "dev-wallet"));

/// Whether this build offers the recovery-phrase wallet: only a build that
/// names a test network (`SHONIN_APP_NETWORK=testnet` or `local` at build
/// time), a development build, and the crate's own tests. Any other build,
/// a mainnet one or one that names nothing, leaves it out.
pub const PHRASE_WALLET: bool =
    cfg!(any(test, feature = "dev-wallet")) || phrase_wallet_for(option_env!("SHONIN_APP_NETWORK"));

/// Whether a build naming `network` offers the recovery-phrase wallet.
pub const fn phrase_wallet_for(network: Option<&str>) -> bool {
    match network {
        Some(network) => {
            network.eq_ignore_ascii_case("testnet") || network.eq_ignore_ascii_case("local")
        }
        None => false,
    }
}

/// The local chains' endpoints as this machine runs them: a development
/// build reads the `SHONIN_RPC_EVM_<id>` the dev task was started with, so
/// the app reaches the same anvils as the API when the ports are moved. Only
/// for the local chains, and only an endpoint on this machine.
#[cfg(feature = "dev-wallet")]
pub fn local_rpc(chain_id: u64) -> Option<&'static str> {
    let url = match chain_id {
        31337 => option_env!("SHONIN_RPC_EVM_31337"),
        31338 => option_env!("SHONIN_RPC_EVM_31338"),
        31339 => option_env!("SHONIN_RPC_EVM_31339"),
        _ => None,
    }?;
    url.starts_with("http://127.0.0.1:")
        .then_some(url)
        .or_else(|| url.starts_with("http://localhost:").then_some(url))
}

#[cfg(not(feature = "dev-wallet"))]
pub fn local_rpc(_chain_id: u64) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phrase_wallet_is_offered_only_by_a_build_that_names_a_test_network() {
        assert!(phrase_wallet_for(Some("testnet")));
        assert!(phrase_wallet_for(Some("local")));
        assert!(!phrase_wallet_for(Some("mainnet")));
        assert!(!phrase_wallet_for(Some("")));
        assert!(!phrase_wallet_for(None));
    }
}
