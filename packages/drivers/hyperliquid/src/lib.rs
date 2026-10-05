//! Hyperliquid: HyperCore's API, how actions on it are signed, and the
//! driver's executor half.
//!
//! HyperCore takes two kinds of signed action. An *L1 action* (an order, a
//! cancel, `evmUserModify`) is hashed from its MessagePack encoding and the
//! nonce, and signed as a "phantom agent" under the `Exchange` domain; an
//! account's approved agent may sign these. A *user-signed action* (a class
//! transfer between perp and spot, a send) is signed field by field under the
//! `HyperliquidSignTransaction` domain, and only the account's own key can
//! sign one: an agent can't.
//!
//! [`payload`] is what a strategy builds and reads; it is all an agent needs,
//! with the default `venue` feature off. [`agent`] is what the executor signs
//! with the vault's agent key, [`events`] the account's fills and state.

pub mod payload;

#[cfg(feature = "venue")]
pub mod agent;
#[cfg(feature = "venue")]
pub mod api;
#[cfg(feature = "venue")]
pub mod events;
#[cfg(feature = "venue")]
pub mod markets;
#[cfg(feature = "venue")]
pub mod signing;

#[cfg(feature = "venue")]
pub use agent::{Allowlist, SIGNABLE, Trader};
#[cfg(feature = "venue")]
pub use api::{Api, HyperCore, SpotToken};
#[cfg(feature = "venue")]
pub use markets::{Market, Markets};
#[cfg(feature = "venue")]
pub use signing::{Network, l1_action_digest, user_action_digest};
