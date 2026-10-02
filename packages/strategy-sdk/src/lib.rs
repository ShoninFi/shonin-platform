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
        pub_export_macro: true,
        default_bindings_module: "strategy_sdk::bindings",
    });
}

pub mod local;
pub mod payload;
pub mod sizing;

pub use bindings::Guest as Agent;
pub use bindings::shonin::agent::types::Verdict;
pub use bindings::shonin::agent::{
    data, drivers, inference, secrets, state, transfers, types, vault, websocket,
};
pub use local::{
    AccountEvent, AccountState, Action, ClaimPending, MarketState, Order, PositionState, Receipt,
    Side,
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
