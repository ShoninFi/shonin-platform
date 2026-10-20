//! What the account held of each finite asset when it first looked: the
//! mark it buys back to after selling into a pool.
//!
//! The first balance seen of an asset is its home, written to a small JSON
//! file at once, so a restart (a new image, a reboot) keeps buying back to
//! the same mark instead of taking whatever it holds then as the new one.
//! Without a file the homes last as long as the process. To start again
//! from what the account holds now (after a top-up meant as new inventory,
//! say), delete the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use alloy::primitives::U256;
use anyhow::{Context, Result};

/// Every asset's home, by a key naming its chain and the asset.
#[derive(Debug, Default)]
pub struct Homes {
    path: Option<PathBuf>,
    held: BTreeMap<String, U256>,
}

impl Homes {
    /// The homes written at `path`, or none yet; with no path, kept in
    /// memory alone.
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let mut held = BTreeMap::new();
        if let Some(path) = &path {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let written: BTreeMap<String, String> = serde_json::from_str(&text)
                        .with_context(|| format!("reading {}", path.display()))?;
                    for (key, amount) in written {
                        let amount: U256 = amount.parse().with_context(|| {
                            format!("{}: {key}'s home is not a number", path.display())
                        })?;
                        held.insert(key, amount);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("reading {}", path.display()));
                }
            }
        }
        Ok(Self { path, held })
    }

    /// The key an asset's home is kept under: its chain and its address
    /// (or symbol).
    pub fn key(chain: &str, asset: &str) -> String {
        format!("{chain}/{asset}")
    }

    /// The home of `key`, which is `balance` if it has none yet (written
    /// down at once).
    pub fn home(&mut self, key: &str, balance: U256) -> U256 {
        if let Some(home) = self.held.get(key) {
            return *home;
        }
        self.held.insert(key.to_string(), balance);
        if let Err(error) = self.save() {
            tracing::warn!(
                error = format!("{error:#}"),
                "the homes could not be written: a restart would take the balances then as new homes"
            );
        }
        balance
    }

    /// The home of `key`, if it has one.
    pub fn get(&self, key: &str) -> Option<U256> {
        self.held.get(key).copied()
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("making {}", parent.display()))?;
        }
        let written: BTreeMap<&String, String> = self
            .held
            .iter()
            .map(|(key, amount)| (key, amount.to_string()))
            .collect();
        // Whole or not at all: written beside it, then moved over it.
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_string_pretty(&written)? + "\n")
            .with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, path).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_is_the_first_balance_seen_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("testnet-market-homes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("homes.json");
        let key = Homes::key("evm:84532", "0xweth");
        {
            let mut homes = Homes::load(Some(path.clone())).unwrap();
            assert_eq!(homes.home(&key, U256::from(50u8)), U256::from(50u8));
            // Later balances don't move it.
            assert_eq!(homes.home(&key, U256::from(20u8)), U256::from(50u8));
        }
        // A new process reads it back.
        let mut again = Homes::load(Some(path.clone())).unwrap();
        assert_eq!(again.get(&key), Some(U256::from(50u8)));
        assert_eq!(again.home(&key, U256::from(7u8)), U256::from(50u8));
        // Deleting the file starts again from what is held.
        std::fs::remove_file(&path).unwrap();
        let mut fresh = Homes::load(Some(path)).unwrap();
        assert_eq!(fresh.home(&key, U256::from(7u8)), U256::from(7u8));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_file_the_homes_last_as_long_as_the_process() {
        let mut homes = Homes::load(None).unwrap();
        let key = Homes::key("solana:devnet", "SOL");
        assert_eq!(homes.home(&key, U256::from(3u8)), U256::from(3u8));
        assert_eq!(homes.home(&key, U256::from(9u8)), U256::from(3u8));
    }
}
