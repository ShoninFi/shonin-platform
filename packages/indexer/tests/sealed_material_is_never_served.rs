//! `GET /api/store/{hash}/{name}` serves what the store's `read_path` gives
//! it. A sealed strategy's source, bundle and models are kept for the
//! attested roles alone; the source sits under `source.sealed.tar`, which on
//! a case-insensitive filesystem (macOS's default, where the devnet runs) any
//! other spelling of the name opens. The store serves only the names meant
//! to be public, spelt exactly so, never a sealed one however it is spelt,
//! and a bundle's files only where its source was published.

use std::path::{Path, PathBuf};

use indexer::store::{Store, is_file_name, is_hash, is_sealed};

const SOURCE: &str = "5ea1ed0000000000000000000000000000000000000000000000000000000000";
const BUNDLE: &str = "b0d1e00000000000000000000000000000000000000000000000000000000000";

/// A directory of its own for each store: tests run at once, and a clock
/// alone can give two of them the same name.
fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shonin-store-sealed-{}-{}-{}",
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

/// A store holding a strategy's source under `source_name` and the bundle
/// built from it, its build recording that source.
fn a_store(source_name: &str) -> (PathBuf, Store) {
    let root = scratch();
    let write = |hash: &str, name: &str, bytes: &[u8]| {
        let directory = root.join(format!("sha256-{hash}"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), bytes).unwrap();
    };
    write(SOURCE, source_name, b"the author's source");
    write(BUNDLE, "component.wasm", b"the component");
    write(BUNDLE, "bundle.tar", b"the component and its models");
    write(
        BUNDLE,
        "hashes.json",
        format!(r#"{{"bundle": "sha256:{BUNDLE}", "source": "sha256:{SOURCE}"}}"#).as_bytes(),
    );
    let store = Store::new(root.clone());
    (root, store)
}

/// What the route serves for a name: nothing it refuses before the store,
/// and otherwise what the store reads.
fn served(store: &Store, hash: &str, name: &str) -> bool {
    is_hash(hash) && is_file_name(name) && !is_sealed(name) && store.read_path(hash, name).is_some()
}

fn tidy(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_sealed_source_is_not_served_under_any_spelling_of_its_name() {
    let (root, store) = a_store("source.sealed.tar");
    let hash = format!("sha256:{SOURCE}");
    for name in [
        "source.sealed.tar",
        "source.SEALED.tar",
        "source.sealed.TAR",
        "Source.Sealed.Tar",
        "SOURCE.SEALED.TAR",
    ] {
        assert!(
            !served(&store, &hash, name),
            "GET /api/store/{hash}/{name} serves the sealed source"
        );
        assert!(
            store.read_path(&hash, name).is_none(),
            "the store reads the sealed source for {name}"
        );
    }
    tidy(&root);
}

#[test]
fn only_the_public_names_spelt_exactly_are_served() {
    let (root, store) = a_store("source.tar");
    let source = format!("sha256:{SOURCE}");
    let bundle = format!("sha256:{BUNDLE}");
    assert!(served(&store, &source, "source.tar"));
    for name in ["component.wasm", "bundle.tar", "hashes.json"] {
        assert!(served(&store, &bundle, name), "{name} was not served");
    }
    // Another spelling of a public name opens the same file on this
    // filesystem, and is not one of the names the store serves.
    for name in ["Source.tar", "SOURCE.TAR", "Component.wasm", "HASHES.json"] {
        for hash in [&source, &bundle] {
            assert!(!served(&store, hash, name), "{name} was served");
        }
    }
    // Nor is anything else a hash's directory may hold.
    std::fs::write(
        root.join(format!("sha256-{BUNDLE}")).join("notes.txt"),
        b"kept, not published",
    )
    .unwrap();
    assert!(!served(&store, &bundle, "notes.txt"));
    tidy(&root);
}

#[test]
fn a_sealed_strategys_bundle_and_models_are_not_served() {
    let (root, store) = a_store("source.sealed.tar");
    let bundle = format!("sha256:{BUNDLE}");
    for name in ["component.wasm", "bundle.tar"] {
        assert!(
            !served(&store, &bundle, name),
            "the bundle of a sealed strategy is served as {name}"
        );
    }
    // Its hashes are on the chain anyway.
    assert!(served(&store, &bundle, "hashes.json"));
    tidy(&root);
}

#[test]
fn a_bundle_whose_build_names_no_source_is_taken_for_a_sealed_one() {
    let (root, store) = a_store("source.tar");
    std::fs::remove_file(root.join(format!("sha256-{BUNDLE}")).join("hashes.json")).unwrap();
    assert!(!served(
        &store,
        &format!("sha256:{BUNDLE}"),
        "component.wasm"
    ));
    tidy(&root);
}
