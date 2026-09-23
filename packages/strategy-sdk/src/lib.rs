//! The Rust SDK for strategy authors.
//!
//! A strategy is one long-lived async program. The host starts [`run`] once
//! and never schedules it again: whatever the strategy does, it does inside
//! that call, awaiting the streams and the venues it cares about.
//!
//! Everything it can reach is an import of the `shonin:strategy` world, and
//! the only way out to a venue is [`venues::submit`]. A strategy holds no key
//! and signs nothing; the executor does that, and the venue and the vault's
//! contract between them make sure those signatures can only ever move the
//! vault's own money between the vault's own accounts.

#![allow(clippy::too_many_arguments)]

/// The generated bindings for the `shonin:strategy` world.
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "strategy",
        // Only the functions the world itself declares `async` may be lowered
        // that way; naming them one by one keeps the two in step.
        async: [
            "shonin:strategy/venues@0.1.0#submit",
            "shonin:strategy/venues@0.1.0#account-state",
            "shonin:strategy/data@0.1.0#query",
            "shonin:strategy/websocket@0.1.0#connect",
            "shonin:strategy/websocket@0.1.0#[method]connection.send",
            "shonin:strategy/inference@0.1.0#infer",
            "run",
        ],
        pub_export_macro: true,
        default_bindings_module: "strategy_sdk::bindings",
    });
}

pub mod payload;
pub mod venue;

pub use bindings::Guest as Strategy;
pub use bindings::shonin::strategy::types::Verdict;
pub use bindings::shonin::strategy::{
    data, inference, secrets, state, types, vault, venues, websocket,
};
pub use payload::{Decimal, decimal, from_payload, to_payload};
pub use venue::{
    Account, AccountEvent, AccountState, Action, ClaimPending, MarketState, Order, PositionState,
    Receipt, Side,
};

/// Declare the strategy the host should run.
///
/// ```ignore
/// struct MyStrategy;
/// impl strategy_sdk::Strategy for MyStrategy {
///     async fn run(config: Vec<u8>) -> Result<(), String> { Ok(()) }
/// }
/// strategy_sdk::export_strategy!(MyStrategy);
/// ```
#[macro_export]
macro_rules! export_strategy {
    ($name:ident) => {
        $crate::bindings::export!($name with_types_in $crate::bindings);
    };
}
