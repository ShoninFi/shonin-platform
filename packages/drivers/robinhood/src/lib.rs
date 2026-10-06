//! Robinhood Chain: its stock tokens, held in a Safe the station owns and
//! traded on Uniswap through a Zodiac Roles v2 role, and the driver's
//! executor half.
//!
//! [`payload`] is what a strategy builds and reads; it is all an agent needs,
//! with the default `venue` feature off. [`trader`] is what the executor
//! signs with the vault's key, [`listings`] the chain's contracts.

pub mod payload;

#[cfg(feature = "venue")]
pub mod contracts;
#[cfg(feature = "venue")]
pub mod listings;
#[cfg(feature = "venue")]
pub mod trader;

#[cfg(feature = "venue")]
pub use trader::{Bounds, RoleCall, Trader};
