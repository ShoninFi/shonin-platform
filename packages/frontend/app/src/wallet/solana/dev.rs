//! The development wallet on Solana, and what only a development build
//! compiles in.
//!
//! No Solana node signs for anyone, so the development wallet is always a
//! key signing in the page:
//!
//! - On a development cluster (the API names an account in its config), the
//!   key of the published development phrase whose address is that account
//!   (the viewer role's, `m/44'/501'/16'/0'`). The address is checked
//!   against the API's before it is used.
//! - Elsewhere, the test wallet's phrase (`SHONIN_DEV_WALLET_MNEMONIC`) at
//!   `m/44'/501'/0'/0'`.
//!
//! Both are compiled in only by the `dev-wallet` feature, which only
//! `task frontend:app:dev` turns on.

use chain::solana_ix::Pubkey;

use super::key::SolKey;

/// The phrase every development chain publishes its accounts under.
#[cfg(feature = "dev-wallet")]
const DEVELOPMENT_PHRASE: Option<&str> =
    Some("test test test test test test test test test test test junk");
#[cfg(not(feature = "dev-wallet"))]
const DEVELOPMENT_PHRASE: Option<&str> = None;

/// Where the published phrase's roles start, and the viewer's index among
/// them: the account a development cluster offers.
const ROLE_BASE: u32 = 10;
const VIEWER: u32 = 6;

/// The development wallet's key for a cluster whose API names `offered` as
/// its development account (or none), if this build has one. A development
/// cluster's key is used only if its address is the one offered.
pub fn key(offered: Option<Pubkey>) -> Option<SolKey> {
    match offered {
        Some(account) => {
            let key = SolKey::from_phrase(DEVELOPMENT_PHRASE?, ROLE_BASE + VIEWER).ok()?;
            (key.pubkey() == account).then_some(key)
        }
        None => crate::wallet::dev::PHRASE
            .filter(|phrase| !phrase.trim().is_empty())
            .and_then(|phrase| SolKey::from_phrase(phrase, 0).ok()),
    }
}

/// The local cluster's endpoint as this machine runs it: a development
/// build reads the `SHONIN_RPC_SOLANA_LOCALNET` the dev task was started
/// with, so the app reaches the same cluster as the API when the ports move.
/// Only for the local cluster, and only an endpoint on this machine.
#[cfg(feature = "dev-wallet")]
pub fn local_rpc(cluster: &str) -> Option<&'static str> {
    if cluster != "localnet" {
        return None;
    }
    let url = option_env!("SHONIN_RPC_SOLANA_LOCALNET")?;
    (url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")).then_some(url)
}

#[cfg(not(feature = "dev-wallet"))]
pub fn local_rpc(_cluster: &str) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_cluster_key_is_used_only_for_the_account_it_offers() {
        let offered: Pubkey = "4fNNXcemx1jQ1crbXfADJ8tdkB5sTccjRp6bH6HJFnV5"
            .parse()
            .unwrap();
        match DEVELOPMENT_PHRASE {
            Some(_) => {
                assert_eq!(key(Some(offered)).map(|k| k.pubkey()), Some(offered));
                assert!(key(Some(Pubkey::new_from_array([1; 32]))).is_none());
            }
            None => assert!(key(Some(offered)).is_none()),
        }
    }
}
