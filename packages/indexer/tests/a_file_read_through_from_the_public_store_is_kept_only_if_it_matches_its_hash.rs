//! A store with a public copy (`--store-url`) fetches a file it doesn't hold
//! from `<base>/sha256-<hash>/<name>`, and keeps it only if it is what the
//! hash it is filed under says. Nothing sealed is ever asked for, a file the
//! copy didn't give is not asked for again at once, and the route that
//! serves the store's files to anyone reads only the disk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use indexer::store::Store;
use shared::manifest::hash_bytes;

/// A directory of its own for each store: tests run at once, and a clock
/// alone can give two of them the same name.
fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shonin-store-remote-{}-{}-{}",
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

/// A public copy holding `files` (by `sha256-<hex>/<name>` path) and noting
/// every path asked for.
async fn a_public_copy(files: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let noted = asked.clone();
    let files = Arc::new(files);
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let noted = noted.clone();
        let files = files.clone();
        async move {
            let path = uri.path().trim_start_matches('/').to_string();
            noted.lock().unwrap().push(path.clone());
            match files.get(&path) {
                Some(bytes) => (axum::http::StatusCode::OK, bytes.clone()),
                None => (axum::http::StatusCode::NOT_FOUND, Vec::new()),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, asked)
}

fn digits(hash: &str) -> &str {
    hash.strip_prefix("sha256:").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_matches_its_hash_is_fetched_and_kept_and_one_that_does_not_is_refused() {
    let source = b"the author's source".to_vec();
    let good = hash_bytes(&source);
    let liar = hash_bytes(b"something else entirely");
    let mut files = HashMap::new();
    files.insert(
        format!("sha256-{}/source.tar", digits(&good)),
        source.clone(),
    );
    // Served under a hash it isn't.
    files.insert(
        format!("sha256-{}/source.tar", digits(&liar)),
        source.clone(),
    );
    let (url, asked) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &format!("{url}/"));

    assert_eq!(store.bytes(&good, "source.tar"), Some(source.clone()));
    let kept = root.join(format!("sha256-{}/source.tar", digits(&good)));
    assert_eq!(std::fs::read(&kept).unwrap(), source);

    assert_eq!(store.bytes(&liar, "source.tar"), None);
    assert!(
        !root
            .join(format!("sha256-{}/source.tar", digits(&liar)))
            .exists()
    );

    // Kept: read from the disk from now on, without asking again.
    let before = asked.lock().unwrap().len();
    assert_eq!(store.bytes(&good, "source.tar"), Some(source));
    // Refused: not asked for again until a while has passed.
    assert_eq!(store.bytes(&liar, "source.tar"), None);
    let after = asked.lock().unwrap().clone();
    assert_eq!(after.len(), before, "{after:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_component_is_kept_only_if_its_build_record_names_its_hash() {
    let bundle = b"the component and its models".to_vec();
    let bundle_hash = hash_bytes(&bundle);
    let component = b"the component".to_vec();
    let record = |component_hash: &str| {
        format!(r#"{{"bundle": "{bundle_hash}", "component": "{component_hash}"}}"#).into_bytes()
    };
    let directory = format!("sha256-{}", digits(&bundle_hash));

    // The record names the component's own hash: taken.
    let mut files = HashMap::new();
    files.insert(format!("{directory}/component.wasm"), component.clone());
    files.insert(
        format!("{directory}/hashes.json"),
        record(&hash_bytes(&component)),
    );
    let (url, _) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &url);
    assert_eq!(
        store.bytes(&bundle_hash, "component.wasm"),
        Some(component.clone())
    );
    let _ = std::fs::remove_dir_all(root);

    // The record names another component: refused.
    let mut files = HashMap::new();
    files.insert(format!("{directory}/component.wasm"), component.clone());
    files.insert(
        format!("{directory}/hashes.json"),
        record(&hash_bytes(b"another component")),
    );
    let (url, _) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &url);
    assert_eq!(store.bytes(&bundle_hash, "component.wasm"), None);
    let _ = std::fs::remove_dir_all(root);

    // A record filed under another bundle: refused, and so is the
    // component it would vouch for.
    let mut files = HashMap::new();
    files.insert(format!("{directory}/component.wasm"), component.clone());
    files.insert(
        format!("{directory}/hashes.json"),
        format!(
            r#"{{"bundle": "{}", "component": "{}"}}"#,
            hash_bytes(b"another bundle"),
            hash_bytes(&component)
        )
        .into_bytes(),
    );
    let (url, _) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &url);
    assert_eq!(store.bytes(&bundle_hash, "hashes.json"), None);
    assert_eq!(store.bytes(&bundle_hash, "component.wasm"), None);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_sealed_is_asked_of_the_public_copy_and_the_public_route_reads_only_the_disk() {
    let source = b"sealed source".to_vec();
    let hash = hash_bytes(&source);
    let report = b"{}".to_vec();
    let report_hash = hash_bytes(&report);
    let mut files = HashMap::new();
    files.insert(
        format!("sha256-{}/source.sealed.tar", digits(&hash)),
        source.clone(),
    );
    files.insert(
        format!("sha256-{}/report.json", digits(&report_hash)),
        report.clone(),
    );
    let (url, asked) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &url);

    for name in [
        "source.sealed.tar",
        "source.SEALED.tar",
        "queue.json",
        "payments.json",
    ] {
        assert_eq!(store.bytes(&hash, name), None, "{name}");
    }
    // What anyone may ask the API for is read from the disk alone.
    assert!(store.read_path(&report_hash, "report.json").is_none());
    let none = asked.lock().unwrap().clone();
    assert!(none.is_empty(), "{none:?}");

    // Once the record has read it through, it is served.
    assert_eq!(
        store.bytes(&report_hash, "report.json"),
        Some(report.clone())
    );
    assert_eq!(
        store.read_path(&report_hash, "report.json").unwrap().0,
        report
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bundle_the_record_named_is_served_read_through_and_no_other() {
    let source = b"published source".to_vec();
    let source_hash = hash_bytes(&source);
    let component = b"\0asm a component".to_vec();
    let bundle = b"a bundle".to_vec();
    let bundle_hash = hash_bytes(&bundle);
    let hashes = serde_json::json!({
        "bundle": bundle_hash,
        "component": hash_bytes(&component),
        "source": source_hash,
    })
    .to_string()
    .into_bytes();
    let unnamed = hash_bytes(b"a bundle nobody named");
    let mut files = HashMap::new();
    for (hash, name, bytes) in [
        (&bundle_hash, "component.wasm", component.clone()),
        (&bundle_hash, "bundle.tar", bundle.clone()),
        (&unnamed, "component.wasm", component.clone()),
    ] {
        files.insert(format!("sha256-{}/{name}", digits(hash)), bytes);
    }
    let (url, asked) = a_public_copy(files).await;
    let root = scratch();
    let store = Store::with_remote(root.clone(), &url);

    // Not named yet: nothing is served, and nothing asked of the copy.
    assert!(store.read_path(&bundle_hash, "component.wasm").is_none());
    assert!(asked.lock().unwrap().is_empty());

    // The record read the build's record and the source (here, on disk).
    for (hash, name, bytes) in [
        (&bundle_hash, "hashes.json", &hashes),
        (&source_hash, "source.tar", &source),
    ] {
        let dir = root.join(format!("sha256-{}", digits(hash)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    assert_eq!(
        store.read_path(&bundle_hash, "component.wasm").unwrap().0,
        component
    );
    assert_eq!(
        store.read_path(&bundle_hash, "bundle.tar").unwrap().0,
        bundle
    );
    // A bundle the record never named is still not asked for.
    assert!(store.read_path(&unnamed, "component.wasm").is_none());
    let asked = asked.lock().unwrap().clone();
    assert!(
        !asked.iter().any(|path| path.contains(digits(&unnamed))),
        "{asked:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}
