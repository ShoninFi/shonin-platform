//! Wallets on Solana, beside the EVM ones: browser wallets through the
//! Wallet Standard (Phantom first, with `window.phantom.solana` as the
//! fallback), the recovery-phrase wallet signing with ed25519 in the page,
//! and the development wallet. One session holds the Solana connection as
//! signals; flows reach the wallet through [`session::SessionSigner`], which
//! implements [`crate::tx::solana::port::SolSigner`]. Both an EVM and a
//! Solana wallet may be connected at once.

pub mod dev;
pub mod key;
pub mod machine;
pub mod network;
pub mod rpc;
pub mod session;
#[cfg(target_arch = "wasm32")]
pub mod standard;

pub use machine::{SolConnection, SolWalletId};
pub use network::SolNetwork;
pub use session::{SolSession, SolWalletOption, get};

use crate::tx::solana::SolError;

/// What Phantom's older provider answered, read: it takes no chain and
/// sends to whichever network Phantom is set to, so a blockhash it doesn't
/// know means that network isn't the vault's. Nothing landed.
pub fn legacy_error(error: SolError) -> SolError {
    match error {
        SolError::Wallet(why) if why.to_ascii_lowercase().contains("blockhash") => {
            SolError::WrongCluster
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_s_older_provider_on_another_network_is_refused_as_the_wrong_network() {
        assert_eq!(
            legacy_error(SolError::Wallet(
                "Transaction simulation failed: Blockhash not found".to_string()
            )),
            SolError::WrongCluster
        );
        assert_eq!(legacy_error(SolError::Rejected), SolError::Rejected);
        assert_eq!(
            legacy_error(SolError::Wallet("insufficient funds".to_string())),
            SolError::Wallet("insufficient funds".to_string())
        );
    }
}
