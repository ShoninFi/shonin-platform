//! Where everything a build produced is kept, addressed by its own hash.
//!
//! Source, bundles, build logs and review reports all live here. A hash is the
//! whole address: anyone with the hash from the chain can ask for the bytes
//! and check for themselves that they are the bytes the hash names.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The directory one hash's files live in.
pub fn directory(store: &Path, hash: &str) -> PathBuf {
    store.join(hash.replace(':', "-"))
}

pub fn put(store: &Path, hash: &str, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let directory = directory(store, hash);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("making {}", directory.display()))?;
    let path = directory.join(name);
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn put_json<T: serde::Serialize>(
    store: &Path,
    hash: &str,
    name: &str,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("writing to the store")?;
    put(store, hash, name, &bytes)?;
    Ok(())
}
