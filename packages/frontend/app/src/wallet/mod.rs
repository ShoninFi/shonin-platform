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
pub mod signed;
pub mod solana;

pub use machine::{Connection, WalletId};

/// The recovery-phrase wallet's name, as lines say it.
pub const PHRASE_WALLET: &str = "your recovery phrase wallet";
/// The development wallet's name, as lines say it.
pub const DEV_WALLET: &str = "the development wallet";

/// The wallet that sent a transaction, as its words need it: its name, and
/// whether it keeps its own list of what it sent, decided by its kind (a
/// browser wallet does; the recovery-phrase and development wallets sign in
/// the page and keep none), never by its name, which a browser wallet
/// announces itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sender {
    pub name: String,
    pub keeps_activity: bool,
}

pub use network::Network;
pub use run::{Action, Running, VaultRef};
pub use session::{Session, SessionSigner, WalletOption, get};
