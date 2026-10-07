//! Reading the content store through from its public copy.
//!
//! The store is content-addressed (`sha256-<hash>/<name>`), so a file can be
//! fetched from anywhere and believed only for its hash. A file not on disk is
//! fetched from `<base>/sha256-<hash>/<name>`, checked as it would be on disk,
//! and kept only if it passes: a file that fails is never written, so a bad
//! copy can't stand in the way of a good one later. What reads it afterwards
//! checks it again, as it does every file on disk.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use alloy::primitives::B256;
use anyhow::{Context, Result, bail};

/// The most a store file fetched may be: a component is a few megabytes.
const MOST: usize = 64 * 1024 * 1024;

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_default()
});

/// Where a stored file lives.
pub fn path(store: &Path, hash: B256, name: &str) -> PathBuf {
    store
        .join(format!("sha256-{}", alloy::hex::encode(hash)))
        .join(name)
}

/// Make sure `<store>/sha256-<hash>/<name>` is on disk: fetched from `base`
/// if it isn't, and kept only if `check` passes over its bytes. Says whether
/// the file is there now; a file the public store doesn't have is no error.
pub async fn read_through(
    store: &Path,
    base: Option<&str>,
    hash: B256,
    name: &str,
    check: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<bool> {
    let path = path(store, hash, name);
    if path.exists() {
        return Ok(true);
    }
    let Some(base) = base else {
        return Ok(false);
    };
    let url = format!(
        "{}/sha256-{}/{name}",
        base.trim_end_matches('/'),
        alloy::hex::encode(hash)
    );
    let answer = HTTP
        .get(&url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    let answer = answer
        .error_for_status()
        .with_context(|| format!("fetching {url}"))?;
    if answer
        .content_length()
        .is_some_and(|length| length > MOST as u64)
    {
        bail!("{url} is larger than any store file");
    }
    let bytes = answer
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?;
    if bytes.len() > MOST {
        bail!("{url} is larger than any store file");
    }
    check(&bytes).with_context(|| format!("{url} is not the file its hash names"))?;
    keep(&path, &bytes)?;
    tracing::info!(file = %path.display(), "fetched from the public store and checked");
    Ok(true)
}

/// Write a file whole or not at all: to a temporary name beside it, then
/// renamed into place.
fn keep(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("a store file has a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let temporary = dir.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)
        .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("keeping {}", path.display()))
}

/// A recorded manifest is the one whose canonical hash the registry holds.
pub fn manifest_check(manifest_hash: B256) -> impl FnOnce(&[u8]) -> Result<()> {
    move |bytes| {
        let canonical: shared::Canonical =
            serde_json::from_slice(bytes).context("the manifest is not a recorded manifest")?;
        let hash = format!("sha256:{}", alloy::hex::encode(manifest_hash));
        anyhow::ensure!(
            canonical.hash() == hash,
            "the manifest hashes to {}",
            canonical.hash()
        );
        Ok(())
    }
}

/// A component is the one whose hash its manifest records.
pub fn component_check(component_hash: String) -> impl FnOnce(&[u8]) -> Result<()> {
    move |bytes| {
        let found = shared::manifest::hash_bytes(bytes);
        anyhow::ensure!(
            found == component_hash,
            "the component hashes to {found}, and its manifest records {component_hash}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public store that serves `files` by path.
    async fn public(files: Vec<(String, Vec<u8>)>) -> (String, crate::AbortOnDrop) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = std::sync::Arc::new(files);
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let files = files.clone();
            async move {
                match files.iter().find(|(path, _)| *path == uri.path()) {
                    Some((_, bytes)) => (axum::http::StatusCode::OK, bytes.clone()),
                    None => (axum::http::StatusCode::NOT_FOUND, Vec::new()),
                }
            }
        });
        let server = crate::AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        (base, server)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shonin-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_file_fetched_is_kept_only_if_it_matches_its_hash() {
        let good = b"the component".to_vec();
        let recorded = shared::manifest::hash_bytes(&good);
        let bundle = B256::repeat_byte(0x42);
        let other = B256::repeat_byte(0x43);
        let at = |hash: B256| format!("/sha256-{}/component.wasm", alloy::hex::encode(hash));
        let (base, _server) = public(vec![
            (at(bundle), good.clone()),
            (at(other), b"something else".to_vec()),
        ])
        .await;
        let store = scratch("hash");

        let fetched = read_through(
            &store,
            Some(&base),
            bundle,
            "component.wasm",
            component_check(recorded.clone()),
        )
        .await
        .unwrap();
        assert!(fetched);
        assert_eq!(
            std::fs::read(path(&store, bundle, "component.wasm")).unwrap(),
            good
        );

        // Bytes that are not what the manifest records are refused and never
        // written.
        let refused = read_through(
            &store,
            Some(&base),
            other,
            "component.wasm",
            component_check(recorded.clone()),
        )
        .await;
        assert!(refused.is_err());
        assert!(!path(&store, other, "component.wasm").exists());
        assert!(
            std::fs::read_dir(store.join(format!("sha256-{}", alloy::hex::encode(other))))
                .map(|entries| entries.count() == 0)
                .unwrap_or(true)
        );

        // Not in the public store: nothing there, and no error.
        let missing = read_through(
            &store,
            Some(&base),
            B256::repeat_byte(9),
            "component.wasm",
            component_check(recorded),
        )
        .await
        .unwrap();
        assert!(!missing);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[tokio::test]
    async fn without_a_public_store_only_the_disk_is_read() {
        let store = scratch("local");
        let found = read_through(&store, None, B256::repeat_byte(1), "manifest.json", |_| {
            Ok(())
        })
        .await
        .unwrap();
        assert!(!found);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn a_manifest_is_believed_only_for_the_hash_the_registry_holds() {
        assert!(manifest_check(B256::repeat_byte(1))(b"{}").is_err());
        assert!(manifest_check(B256::repeat_byte(1))(b"not json").is_err());
    }
}
