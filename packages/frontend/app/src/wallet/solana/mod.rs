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
