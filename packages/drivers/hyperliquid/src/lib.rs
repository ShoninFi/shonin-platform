//! Hyperliquid: HyperCore's API and how actions on it are signed.
//!
//! HyperCore takes two kinds of signed action. An *L1 action* (an order, a
//! cancel, `evmUserModify`) is hashed from its MessagePack encoding and the
//! nonce, and signed as a "phantom agent" under the `Exchange` domain; an
//! account's approved agent may sign these. A *user-signed action* (a class
//! transfer between perp and spot, a send) is signed field by field under the
//! `HyperliquidSignTransaction` domain, and only the account's own key can
//! sign one: an agent can't.

pub mod api;
pub mod signing;

pub use api::{Api, HyperCore, SpotToken};
pub use signing::{Network, l1_action_digest, user_action_digest};
