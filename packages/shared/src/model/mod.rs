//! The domain model: the things the platform talks about, in the shape the
//! frontend, the backend and the executor all read them in.
//!
//! Plain data with `serde` derives, and no dependency on a browser, a chain
//! client or a runtime, so it compiles for native targets and for wasm alike.

pub mod activity;
pub mod attestation;
pub mod bundle;
pub mod common;
pub mod health;
pub mod registry;
pub mod review;
pub mod strategy;
pub mod vault;
pub mod venue;
