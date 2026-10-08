//! A file pushed into the store (`Store::put_checked`) is kept only if it is
//! what the hash it is filed under says, only under a public name, never a
//! sealed one, and a component only once the record of its build is there to
//! check it against. A file held already is left as it is.

use std::path::PathBuf;

use indexer::store::{Put, Store};
use shared::manifest::hash_bytes;

fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shonin-store-push-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `sha256:<hex>` as the builder pushes it, `sha256-<hex>`.
fn filed(hash: &str) -> String {
    hash.replace(':', "-")
}

#[test]
fn a_pushed_file_not_matching_its_hash_is_not_kept() {
    let root = scratch();
    let store = Store::new(root.clone());
    let source = b"the author's source".to_vec();
    let hash = filed(&hash_bytes(&source));
    let other = filed(&hash_bytes(b"something else"));

    assert_eq!(
        store.put_checked(&other, "source.tar", &source),
        Put::Mismatch
    );
    assert!(!root.join(&other).join("source.tar").exists());

    assert_eq!(store.put_checked(&hash, "source.tar", &source), Put::Kept);
    assert_eq!(
        std::fs::read(root.join(&hash).join("source.tar")).unwrap(),
        source
    );
    // Again, the same bytes: held already.
    assert_eq!(store.put_checked(&hash, "source.tar", &source), Put::Held);
    // No temporary file is left beside it.
    let left: Vec<_> = std::fs::read_dir(root.join(&hash))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["source.tar"]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_pushed_component_is_checked_against_its_build_s_record_and_needs_it_first() {
    let root = scratch();
    let store = Store::new(root.clone());
    let bundle = b"a bundle".to_vec();
    let component = b"\0asm a component".to_vec();
    let bundle_hash = hash_bytes(&bundle);
    let dir = filed(&bundle_hash);
    let hashes = serde_json::json!({
        "bundle": bundle_hash,
        "component": hash_bytes(&component),
    })
    .to_string();

    assert_eq!(
        store.put_checked(&dir, "component.wasm", &component),
        Put::OutOfOrder
    );
    assert_eq!(
        store.put_checked(&dir, "hashes.json", hashes.as_bytes()),
        Put::Kept
    );
    assert_eq!(
        store.put_checked(&dir, "component.wasm", b"\0asm another"),
        Put::Mismatch
    );
    assert_eq!(
        store.put_checked(&dir, "component.wasm", &component),
        Put::Kept
    );
    assert_eq!(store.put_checked(&dir, "bundle.tar", &bundle), Put::Kept);
    // A record naming another bundle is not this bundle's.
    let other = filed(&hash_bytes(b"another bundle"));
    assert_eq!(
        store.put_checked(&other, "hashes.json", hashes.as_bytes()),
        Put::Mismatch
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_sealed_or_unknown_name_or_a_path_outside_the_store_is_refused() {
    let root = scratch();
    let store = Store::new(root.clone());
    let source = b"sealed source".to_vec();
    let hash = filed(&hash_bytes(&source));
    for name in [
        "source.sealed.tar",
        "source.SEALED.tar",
        "notes.txt",
        "../source.tar",
        ".source.tar",
    ] {
        assert_eq!(
            store.put_checked(&hash, name, &source),
            Put::Refused,
            "{name}"
        );
    }
    for hash in ["../../etc", "sha256-ABC", "/tmp/x"] {
        assert_eq!(
            store.put_checked(hash, "source.tar", &source),
            Put::Refused,
            "{hash}"
        );
    }
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(root);
}
