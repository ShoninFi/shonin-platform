//! What an agent says to the `hyperliquid` driver, and what it hears back:
//! the types live in the strategy SDK, where an agent finds them, and are the
//! same types here, so the executor half decodes exactly what an agent
//! encodes.

pub use strategy_sdk::hyperliquid::*;
