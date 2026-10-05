//! The Rust SDK for agent authors.
//!
//! An agent is one long-lived async program. The host starts its `run` once
//! and never schedules it again: whatever the agent does, it does inside that
//! call, awaiting the streams and the drivers it cares about.
//!
//! Everything it can reach is an import of the `shonin:agent` world, and the
//! only way out to a venue is [`drivers::submit`]. An agent holds no key and
//! signs nothing; the executor does that, and the venue and the vault's
//! contracts between them make sure those signatures can only ever move the
//! vault's own money between the vault's own accounts.
//!
//! An agent's settings are constants in its own code, so `run` takes no
//! arguments.

#![allow(clippy::too_many_arguments)]

/// The generated bindings for the `shonin:agent` world.
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "agent",
        // Only the functions the world itself declares `async` may be lowered
        // that way; naming them one by one keeps the two in step.
        async: [
            "shonin:agent/drivers@0.1.0#submit",
            "shonin:agent/drivers@0.1.0#account-state",
            "shonin:agent/transfers@0.1.0#move",
            "shonin:agent/transfers@0.1.0#bridge",
            "shonin:agent/transfers@0.1.0#balances",
            "shonin:agent/data@0.1.0#query",
            "shonin:agent/websocket@0.1.0#connect",
            "shonin:agent/websocket@0.1.0#[method]connection.send",
            "shonin:agent/inference@0.1.0#infer",
            "run",
        ],
        // The standard WASI packages the world imports are generated here
        // too, so an agent needs nothing beyond this crate to use them.
        generate_all,
        pub_export_macro: true,
        default_bindings_module: "strategy_sdk::bindings",
    });
}

pub mod local;
pub mod payload;
pub mod sizing;
pub mod state;
pub mod streams;

pub use bindings::Guest as Agent;
pub use bindings::shonin::agent::types::Verdict;
pub use bindings::shonin::agent::{
    data, drivers, inference, secrets, transfers, types, vault, websocket,
};
/// Requests to any host, outbound only, if the manifest declares `network`:
/// the standard `wasi:http` interfaces.
pub use bindings::wasi::http;
/// The agent's state: the standard `wasi:keyvalue` interfaces, 64 MiB,
/// empty every time the agent starts. [`state`] wraps them for the common
/// case.
pub use bindings::wasi::keyvalue;
pub use local::{
    AccountEvent, AccountState, Action, MarketState, Order, PositionState, Receipt, Side,
};
pub use payload::{Decimal, decimal, from_payload, millionths, to_payload};
pub use sizing::{Limits, size_for};

/// Declare the agent the host should run.
///
/// ```ignore
/// struct MyAgent;
/// impl strategy_sdk::Agent for MyAgent {
///     async fn run() -> Result<(), String> { Ok(()) }
/// }
/// strategy_sdk::export_agent!(MyAgent);
/// ```
#[macro_export]
macro_rules! export_agent {
    ($name:ident) => {
        $crate::bindings::export!($name with_types_in $crate::bindings);
    };
}

impl bindings::shonin::agent::vault::Claim {
    /// What [`Claim::fraction`](bindings::shonin::agent::vault::Claim) is
    /// out of: 10^18 is the whole vault.
    pub const WHOLE: u64 = 1_000_000_000_000_000_000;

    /// The fraction of every position being closed for the claim, as a
    /// decimal between 0 and 1.
    pub fn share(&self) -> Decimal {
        Decimal::from(self.fraction) / Decimal::from(Self::WHOLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bindings::shonin::agent::vault::Claim;

    #[test]
    fn a_claim_s_fraction_reads_as_a_share_of_the_whole_vault() {
        let quarter = Claim {
            id: 3,
            fraction: Claim::WHOLE / 4,
        };
        assert_eq!(quarter.share(), decimal("0.25"));
        let whole = Claim {
            id: 4,
            fraction: Claim::WHOLE,
        };
        assert_eq!(whole.share(), decimal("1"));
    }

    #[test]
    fn a_feed_and_its_values_read_back_as_they_were_written() {
        use streams::{Book, HyperliquidFeed, HyperliquidValue, Level};
        let feed = HyperliquidFeed::Book {
            coin: "BTC".to_string(),
        };
        assert_eq!(
            from_payload::<HyperliquidFeed>(&to_payload(&feed)),
            Ok(feed)
        );
        let value = HyperliquidValue::Book(Book {
            market: "BTC".to_string(),
            bids: vec![Level {
                price: decimal("64000.5"),
                size: decimal("0.1"),
                orders: Some(2),
            }],
            asks: Vec::new(),
        });
        assert_eq!(
            from_payload::<HyperliquidValue>(&to_payload(&value)),
            Ok(value)
        );
    }
}
