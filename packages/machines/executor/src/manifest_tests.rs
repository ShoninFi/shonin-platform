//! Registered manifests remain distinct even when their components are shared.

use super::*;

fn hash(value: &str) -> B256 {
    value.strip_prefix("sha256:").unwrap().parse().unwrap()
}

fn canonical(id: u64, bundle: B256, component: &[u8]) -> shared::Canonical {
    shared::Canonical {
        strategy_id: id,
        vault_chain: shared::Chain::Solana,
        author: "author".into(),
        bundle_hash: format!("sha256:{}", alloy::hex::encode(bundle)),
        component_hash: shared::manifest::hash_bytes(component),
        source_hash: shared::manifest::hash_bytes(b"source"),
        sealed: false,
        world: "shonin:agent@0.1.0".into(),
        toolchain: "rust".into(),
        builder_image: "sha256:builder".into(),
        manifest: shared::Manifest::from_toml(
            // A driver offered on the testnet: the stand-in venue is
            // offered on the local chains alone.
            "[vault]\nchain = \"solana\"\n[description]\ntitle = \"Carry\"\ntext = \"An agent\"\n[drivers.orca]\n",
        ).unwrap(),
    }
}

#[tokio::test]
async fn a_cached_component_does_not_replace_the_registered_manifest() {
    let component = b"the shared component";
    let bundle = B256::repeat_byte(0x42);
    let first = canonical(1, bundle, component);
    let second = canonical(2, bundle, component);
    let first_hash = hash(&first.hash());
    let second_hash = hash(&second.hash());
    assert_ne!(first_hash, second_hash);
    let store =
        std::env::temp_dir().join(format!("shonin-distinct-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(
        store::path(&store, bundle, "component.wasm")
            .parent()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(store::path(&store, bundle, "component.wasm"), component).unwrap();
    std::fs::create_dir_all(
        store::path(&store, first_hash, "manifest.json")
            .parent()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        store::path(&store, first_hash, "manifest.json"),
        serde_json::to_vec(&first).unwrap(),
    )
    .unwrap();
    assert!(recorded_in(&store, bundle, second_hash, bundle, "testnet").is_err());

    let fetched = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requested = fetched.clone();
    let served = serde_json::to_vec(&second).unwrap();
    let route = format!("/sha256-{}/manifest.json", alloy::hex::encode(second_hash));
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let requested = requested.clone();
        let served = served.clone();
        let route = route.clone();
        async move {
            requested.fetch_add(1, Ordering::SeqCst);
            if uri.path() == route {
                (axum::http::StatusCode::OK, served)
            } else {
                (axum::http::StatusCode::NOT_FOUND, Vec::new())
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    assert!(
        fetch_registered(&store, &base, second_hash, bundle)
            .await
            .unwrap()
    );
    assert_eq!(
        fetched.load(Ordering::SeqCst),
        1,
        "only the missing manifest is fetched"
    );
    let recorded = recorded_in(&store, bundle, second_hash, bundle, "testnet").unwrap();
    assert_eq!(component_of(&store, bundle, &recorded).unwrap(), component);
    assert!(
        fetch_registered(&store, &base, second_hash, bundle)
            .await
            .unwrap()
    );
    assert_eq!(
        fetched.load(Ordering::SeqCst),
        1,
        "validated cached files need no HTTP call"
    );

    let other_bundle = B256::repeat_byte(0x43);
    assert!(
        fetch_registered(&store, &base, second_hash, other_bundle)
            .await
            .is_err()
    );
    assert!(recorded_in(&store, other_bundle, second_hash, bundle, "testnet").is_err());
    // The untrusted disk cannot swap in another strategy's valid manifest.
    std::fs::write(
        store::path(&store, second_hash, "manifest.json"),
        serde_json::to_vec(&first).unwrap(),
    )
    .unwrap();
    assert!(recorded_in(&store, bundle, second_hash, bundle, "testnet").is_err());
    let _ = std::fs::remove_dir_all(&store);
}

#[tokio::test]
async fn an_unregistered_public_manifest_is_not_cached_and_can_be_retried() {
    let component = b"the shared component";
    let bundle = B256::repeat_byte(0x42);
    let first = canonical(1, bundle, component);
    let second = canonical(2, bundle, component);
    let registered = hash(&second.hash());
    let bad = serde_json::to_vec(&first).unwrap();
    let good = serde_json::to_vec(&second).unwrap();
    let corrected = std::sync::Arc::new(AtomicBool::new(false));
    let answer = corrected.clone();
    let app = axum::Router::new().fallback(move || {
        let answer = answer.clone();
        let bad = bad.clone();
        let good = good.clone();
        async move {
            if answer.load(Ordering::SeqCst) {
                good
            } else {
                bad
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let store =
        std::env::temp_dir().join(format!("shonin-refused-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(
        store::path(&store, bundle, "component.wasm")
            .parent()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(store::path(&store, bundle, "component.wasm"), component).unwrap();
    assert!(
        fetch_registered(&store, &base, registered, bundle)
            .await
            .is_err()
    );
    assert!(!store::path(&store, registered, "manifest.json").exists());
    corrected.store(true, Ordering::SeqCst);
    assert!(
        fetch_registered(&store, &base, registered, bundle)
            .await
            .unwrap()
    );
    assert!(recorded_in(&store, bundle, registered, bundle, "testnet").is_ok());
    let _ = std::fs::remove_dir_all(&store);
}
