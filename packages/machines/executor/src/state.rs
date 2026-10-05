//! An agent's state: a key-value store of its own, served as the standard
//! `wasi:keyvalue` interfaces.
//!
//! It is held in the runner's memory and nowhere else, so it starts empty
//! every time the agent starts: after a fault, an allowance change, a move to
//! another machine, anything. An agent that needs something after a restart
//! reads it back from the venue, which is where anything that matters is.
//!
//! It holds at most [`LIMIT`] bytes, counted as the bytes of every key, with
//! the name of the bucket it is in, and of every value. A write that would
//! take it past that is refused and changes nothing; the agent is told why
//! and decides what to drop. Buckets are only names: an agent may open as
//! many as it likes, and they share the one allowance.

use std::collections::BTreeMap;

/// The most an agent's state may hold.
pub const LIMIT: usize = 64 * 1024 * 1024;

/// The most keys one `list-keys` call hands back; the cursor carries on.
pub const PAGE: usize = 1_000;

/// An agent's store.
#[derive(Debug)]
pub struct Store {
    /// By bucket, then by key.
    entries: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// What the entries count as, held at or under `limit`.
    held: usize,
    limit: usize,
}

/// An open bucket, as the agent holds it.
#[derive(Clone, Debug)]
pub struct Bucket {
    pub name: String,
}

/// Why a write was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It would take the store past its allowance.
    Full {
        held: usize,
        wanted: usize,
        limit: usize,
    },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Full {
                held,
                wanted,
                limit,
            } => write!(
                f,
                "the state holds {held} of its {limit} bytes, and this would take it to {wanted}"
            ),
        }
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::with_limit(LIMIT)
    }
}

/// What one entry counts as.
fn size(bucket: &str, key: &str, value: &[u8]) -> usize {
    bucket
        .len()
        .saturating_add(key.len())
        .saturating_add(value.len())
}

impl Store {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            held: 0,
            limit,
        }
    }

    /// What the entries count as now.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.held
    }

    pub fn get(&self, bucket: &str, key: &str) -> Option<&Vec<u8>> {
        self.entries.get(bucket)?.get(key)
    }

    /// Set `key` to `value`, or refuse and change nothing if that would take
    /// the store past its limit. Replacing a value counts only the
    /// difference.
    pub fn set(&mut self, bucket: &str, key: &str, value: Vec<u8>) -> Result<(), Refused> {
        let old = self
            .get(bucket, key)
            .map(|old| size(bucket, key, old))
            .unwrap_or(0);
        let new = size(bucket, key, &value);
        let wanted = self.held.saturating_sub(old).saturating_add(new);
        if wanted > self.limit {
            return Err(Refused::Full {
                held: self.held,
                wanted,
                limit: self.limit,
            });
        }
        self.entries
            .entry(bucket.to_string())
            .or_default()
            .insert(key.to_string(), value);
        self.held = wanted;
        Ok(())
    }

    pub fn delete(&mut self, bucket: &str, key: &str) {
        let Some(entries) = self.entries.get_mut(bucket) else {
            return;
        };
        if let Some(old) = entries.remove(key) {
            self.held = self.held.saturating_sub(size(bucket, key, &old));
        }
        if entries.is_empty() {
            self.entries.remove(bucket);
        }
    }

    /// A page of keys, in order, from the `cursor`th on, and where the next
    /// page starts if there is one.
    pub fn keys(&self, bucket: &str, cursor: Option<u64>) -> (Vec<String>, Option<u64>) {
        let Some(entries) = self.entries.get(bucket) else {
            return (Vec::new(), None);
        };
        let from = cursor.unwrap_or(0) as usize;
        let keys: Vec<String> = entries.keys().skip(from).take(PAGE).cloned().collect();
        let next = from + keys.len();
        let more = next < entries.len();
        (keys, more.then_some(next as u64))
    }

    /// Add `delta` to the counter at `key`, a little-endian `u64` (zero if
    /// there is none), wrapping as a `u64` does, and say what it is now. A
    /// value that isn't eight bytes is not a counter, and is refused.
    pub fn increment(&mut self, bucket: &str, key: &str, delta: u64) -> Result<u64, String> {
        let now = match self.get(bucket, key) {
            None => 0,
            Some(bytes) => u64::from_le_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| format!("{key} holds {} bytes, not a counter", bytes.len()))?,
            ),
        };
        let next = now.wrapping_add(delta);
        self.set(bucket, key, next.to_le_bytes().to_vec())
            .map_err(|refused| refused.to_string())?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_holds_sixty_four_mebibytes_counted_by_key_and_value() {
        assert_eq!(LIMIT, 64 * 1024 * 1024);
        let mut store = Store::default();
        // Sixty-three values of a MiB less their keys fit, with room for one
        // more small one.
        let mib = 1024 * 1024;
        for at in 0..64 {
            let key = format!("k{at:02}");
            let value = vec![0u8; mib - key.len() - "b".len()];
            store.set("b", &key, value).unwrap();
        }
        assert_eq!(store.held(), LIMIT);
        // Full: one more byte anywhere is refused, and nothing changes.
        let refused = store.set("b", "x", Vec::new()).unwrap_err();
        assert_eq!(
            refused,
            Refused::Full {
                held: LIMIT,
                wanted: LIMIT + 2,
                limit: LIMIT
            }
        );
        assert!(store.get("b", "x").is_none());
        assert_eq!(store.held(), LIMIT);
        // The bucket's name counts too: a key in a longer-named bucket is
        // bigger.
        assert!(store.set("bucket", "", Vec::new()).is_err());
    }

    #[test]
    fn a_write_past_the_limit_is_refused_and_leaves_what_was_there() {
        let mut store = Store::with_limit(16);
        store.set("", "key", b"value".to_vec()).unwrap();
        assert_eq!(store.held(), 8);
        // Replacing counts only the difference: 3 + 13 = 16 fits exactly.
        store.set("", "key", vec![1; 13]).unwrap();
        assert_eq!(store.held(), 16);
        // One more is refused, and the old value stays.
        assert!(store.set("", "key", vec![2; 14]).is_err());
        assert_eq!(store.get("", "key"), Some(&vec![1; 13]));
        // Deleting gives the room back.
        store.delete("", "key");
        assert_eq!(store.held(), 0);
        store.set("", "other", vec![0; 11]).unwrap();
    }

    #[test]
    fn a_counter_that_would_pass_the_limit_is_refused() {
        let mut store = Store::with_limit(10);
        assert_eq!(store.increment("", "n", 2).unwrap(), 2);
        assert_eq!(store.increment("", "n", 3).unwrap(), 5);
        assert!(store.increment("", "nn", 1).is_err(), "11 bytes fit in 10");
        store.set("", "n", b"no".to_vec()).unwrap();
        assert!(
            store.increment("", "n", 1).is_err(),
            "two bytes are not a counter"
        );
    }

    #[test]
    fn keys_come_a_page_at_a_time_in_order() {
        let mut store = Store::default();
        for at in 0..(PAGE + 5) {
            store.set("b", &format!("{at:05}"), Vec::new()).unwrap();
        }
        let (first, cursor) = store.keys("b", None);
        assert_eq!(first.len(), PAGE);
        assert_eq!(first[0], "00000");
        let (rest, end) = store.keys("b", cursor);
        assert_eq!(rest.len(), 5);
        assert_eq!(rest[4], format!("{:05}", PAGE + 4));
        assert_eq!(end, None);
        assert_eq!(store.keys("nothing", None), (Vec::new(), None));
    }
}
