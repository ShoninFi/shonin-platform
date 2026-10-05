//! `GET /api/store/{hash}/{name}` hands both segments, percent-decoded, to
//! the store, which joins them onto its root. Anything but a hash and a plain
//! file name could name a path outside the store, so the store reads nothing
//! for it, whoever asks.

use std::path::PathBuf;

use indexer::store::{Store, is_file_name, is_hash};

fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-store-{what}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const DIGITS: &str = "9c41a8b7d6e5f40312ab9c41a8b7d6e5f40312ab9c41a8b7d6e5f40312ab0000";

#[test]
fn a_file_under_its_hash_is_read() {
    let root = scratch("kept");
    std::fs::create_dir_all(root.join(format!("sha256-{DIGITS}"))).unwrap();
    std::fs::write(
        root.join(format!("sha256-{DIGITS}")).join("report.json"),
        b"{}",
    )
    .unwrap();
    let store = Store::new(root);

    for hash in [format!("sha256:{DIGITS}"), format!("sha256-{DIGITS}")] {
        let (bytes, kind) = store.read_path(&hash, "report.json").unwrap();
        assert_eq!((bytes.as_slice(), kind), (&b"{}"[..], "application/json"));
    }
}

#[test]
fn a_hash_that_is_an_absolute_path_reads_nothing() {
    let outside = scratch("outside");
    std::fs::write(
        outside.join("secret.env"),
        b"SHONIN_TESTNET_EVM_MNEMONIC=...",
    )
    .unwrap();
    let store = Store::new(scratch("store"));

    // What the route hands over for `/api/store/<outside, percent-encoded>/secret.env`.
    let hash = outside.to_str().unwrap();
    assert!(store.read_path(hash, "secret.env").is_none());
    assert!(!is_hash(hash));
}

#[test]
fn a_hash_of_dot_dot_reads_nothing_of_the_store_s_parent() {
    let parent = scratch("parent");
    let root = parent.join("store");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        parent.join(".env"),
        b"SHONIN_RPC_EVM_8453=https://paid.example/key",
    )
    .unwrap();
    std::fs::write(
        parent.join("env"),
        b"SHONIN_RPC_EVM_8453=https://paid.example/key",
    )
    .unwrap();
    let store = Store::new(root);

    assert!(store.read_path("..", ".env").is_none());
    assert!(store.read_path("..", "env").is_none());
    assert!(store.read_path(".", "../env").is_none());
}

#[test]
fn only_a_hash_and_a_plain_file_name_are_taken() {
    let refused_hashes = [
        String::new(),
        "..".to_string(),
        ".".to_string(),
        "/etc".to_string(),
        "%2Fetc".to_string(),
        "%2E%2E".to_string(),
        "..%2F..".to_string(),
        "sha256:..".to_string(),
        format!("sha256:{DIGITS}/.."),
        format!("sha256:../{}", &DIGITS[3..]),
        format!("sha256:{}", DIGITS.to_uppercase()),
        format!("sha256:{}", &DIGITS[1..]),
        format!("sha256:{DIGITS}0"),
        format!("sha256:{DIGITS}\0"),
        format!("/sha256:{DIGITS}"),
        DIGITS.to_string(),
        format!("md5:{DIGITS}"),
    ];
    for hash in &refused_hashes {
        assert!(!is_hash(hash), "{hash:?} was taken as a hash");
    }
    assert!(is_hash(&format!("sha256:{DIGITS}")));

    let refused_names = [
        "",
        ".",
        "..",
        ".env",
        "../env",
        "/etc/passwd",
        "a/b",
        "a\\b",
        "%2Fetc",
        "%2E%2E",
        "report.json\0",
    ];
    for name in refused_names {
        assert!(!is_file_name(name), "{name:?} was taken as a file name");
    }
    for name in [
        "component.wasm",
        "report.json",
        "source.tar",
        "manifest.json",
    ] {
        assert!(is_file_name(name), "{name:?} was refused");
    }

    let root = scratch("names");
    std::fs::create_dir_all(root.join(format!("sha256-{DIGITS}"))).unwrap();
    std::fs::write(root.join("loose.json"), b"{}").unwrap();
    let store = Store::new(root);
    assert!(
        store
            .read_path(&format!("sha256:{DIGITS}"), "../loose.json")
            .is_none()
    );
}
