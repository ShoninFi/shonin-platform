//! The reviewer's pieces, shared by the service and by the tooling around it.
//!
//! The service is `src/main.rs`; the comparison harness is `src/bin/calibrate.rs`.
//! Both read source the same way and ask the battery the same way, because a
//! comparison that used its own copies would be measuring itself.

pub mod battery;
pub mod cloud;
pub mod jev;
pub mod messages;
pub mod on_solana;
pub mod source;
