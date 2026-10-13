//! Wallets on the EVM chains: browser wallets (EIP-1193, found by EIP-6963
//! with `window.ethereum` as the fallback), the recovery-phrase wallet that
//! signs in the page, and the development wallet. One session holds the
//! connection as signals; flows reach the wallet through
//! [`session::SessionSigner`], which implements [`crate::tx::Signer`].
//!
//! Solana wallets (the Wallet Standard) slot in beside these as further
//! [`machine::WalletId`]s and their own signer; nothing here assumes every
//! wallet is an EVM one beyond the EVM adapters themselves.

pub mod config_cache;
pub mod dev;
#[cfg(target_arch = "wasm32")]
pub mod eip1193;
pub mod machine;
pub mod network;
pub mod networks;
pub mod phrase;
pub mod recent;
pub mod rpc;
pub mod run;
pub mod session;
pub mod solana;

pub use machine::{Connection, WalletId};
pub use network::Network;
pub use run::{Action, Running, VaultRef};
pub use session::{Session, SessionSigner, WalletOption, get};
