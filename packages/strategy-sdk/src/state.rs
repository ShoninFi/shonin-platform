//! The agent's state: a key-value store of its own, through the standard
//! `wasi:keyvalue` interfaces.
//!
//! It holds 64 MiB, counted as the bytes of every key (with its bucket's
//! name) and every value, and a write that would pass that is refused. It
//! starts empty every time the agent starts, after a fault, a move to
//! another machine or anything else, so an agent always rebuilds what
//! matters from the venue: positions, balances and open orders are there
//! anyway. State is for what is cheaper to keep than to work out again.
//!
//! ```ignore
//! let state = strategy_sdk::state::State::open()?;
//! state.set("peak", &strategy_sdk::to_payload(&peak))?;
//! let seen = state.increment("orders", 1)?;
//! ```

use crate::bindings::wasi::keyvalue::{atomics, batch, store};

/// The bucket [`State::open`] opens. Buckets are only names: every bucket
/// an agent opens is its own, and all of them share the one allowance.
pub const BUCKET: &str = "state";

/// A key and its value.
pub type Entry = (String, Vec<u8>);

/// The agent's own bucket.
pub struct State(store::Bucket);

impl State {
    /// The agent's state, in [`BUCKET`].
    pub fn open() -> Result<Self, String> {
        Self::open_bucket(BUCKET)
    }

    /// The agent's state in a bucket of its own naming.
    pub fn open_bucket(name: &str) -> Result<Self, String> {
        store::open(name).map(Self).map_err(said)
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.0.get(key).map_err(said)
    }

    /// Refused, leaving what was there, if it would take the state past its
    /// allowance.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<(), String> {
        self.0.set(key, value).map_err(said)
    }

    pub fn delete(&self, key: &str) -> Result<(), String> {
        self.0.delete(key).map_err(said)
    }

    pub fn exists(&self, key: &str) -> Result<bool, String> {
        self.0.exists(key).map_err(said)
    }

    /// Every key, in order.
    pub fn keys(&self) -> Result<Vec<String>, String> {
        let mut keys = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.0.list_keys(cursor).map_err(said)?;
            keys.extend(page.keys);
            match page.cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(keys),
            }
        }
    }

    /// Add `delta` to the counter at `key` (a little-endian `u64`, zero if
    /// there is none) and say what it is now.
    pub fn increment(&self, key: &str, delta: u64) -> Result<u64, String> {
        atomics::increment(&self.0, key, delta).map_err(said)
    }

    /// Each key's value, in order: `None` where there is none.
    pub fn get_many(&self, keys: &[String]) -> Result<Vec<Option<Entry>>, String> {
        batch::get_many(&self.0, keys).map_err(said)
    }

    pub fn set_many(&self, entries: &[Entry]) -> Result<(), String> {
        batch::set_many(&self.0, entries).map_err(said)
    }

    pub fn delete_many(&self, keys: &[String]) -> Result<(), String> {
        batch::delete_many(&self.0, keys).map_err(said)
    }
}

fn said(error: store::Error) -> String {
    match error {
        store::Error::NoSuchStore => "no such store".to_string(),
        store::Error::AccessDenied => "access denied".to_string(),
        store::Error::Other(why) => why,
    }
}
