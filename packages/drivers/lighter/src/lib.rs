//! Lighter on Robinhood Chain: its API, how its transactions are signed, the
//! attested read of a station's accounts, and the driver's executor half.
//!
//! [`payload`] is what a strategy builds and reads; it is all an agent needs,
//! with the default `venue` feature off. [`trader`] is what the executor
//! signs with the station's Lighter key, [`report`] what an attested image
//! posts so the station can be valued.

pub mod payload;

#[cfg(feature = "venue")]
pub mod api;
#[cfg(feature = "venue")]
pub mod markets;
#[cfg(feature = "venue")]
pub mod report;
#[cfg(feature = "venue")]
pub mod trader;

#[cfg(feature = "venue")]
pub use api::Api;
#[cfg(feature = "venue")]
pub use markets::{Market, Markets};
#[cfg(feature = "venue")]
pub use trader::{Key, SIGNABLE, Trader};
