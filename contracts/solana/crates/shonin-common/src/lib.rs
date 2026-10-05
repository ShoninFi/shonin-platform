//! What the Shonin programs share.
//!
//! The rules that have to read the same in every program live here once: how
//! a cluster is named in digests and leaves, how a blessing's leaf and its
//! Merkle proof are checked, how a machine's signature carried by someone
//! else's transaction is found, whether a program can still be changed, what
//! the time is, and the 256-bit maths a cost needs.

pub mod account;
pub mod checker;
pub mod digest;
pub mod ed25519;
pub mod loader;
pub mod math;
pub mod time;

/// The longest any timelock can be set to, so a slip of the admin's finger
/// can't put everything out of reach for good.
pub const MAX_TIMELOCK: u64 = 365 * 24 * 60 * 60;

/// A day, in seconds.
pub const DAY: u64 = 24 * 60 * 60;

/// The cluster a mainnet build serves. A mainnet build refuses any other, and
/// every other build refuses this one.
pub const MAINNET_CLUSTER: &str = "mainnet-beta";
