//! The attested path end to end, on a local chain: a launcher that hands out
//! PKI tokens (in Google's format, under the fixture's test certificate
//! chain), the executor's attestation asking it for one that binds its keys,
//! the attestation prover proving that token with a real Groth16 proof and
//! posting the seal back, the executor taking the seal only once a `bless`
//! of it simulates on the chain, and then each key in the tree blessed from
//! that one seal, as the keeper sends it, through a Confidential Space
//! checker that trusts the test chain's root.
//!
//! Ignored: it needs `anvil`, the contracts built (`task contracts:build`),
//! Docker (for the Groth16 step) and the prover built with the guest:
//!
//! ```text
//! SHONIN_ATTESTATION_BUILD_GUEST=1 cargo build --release -p shonin-attestation --bin attestation-prover
//! SHONIN_PROVER_BIN=<target>/release/attestation-prover \
//!   cargo test -p shonin-executor --bin executor attested_path -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::providers::Provider;
use alloy::sol_types::SolValue;
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use shonin_attestation_core::fixture;

use crate::attestation::{Attestation, Mode};

/// A process stopped when dropped.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn artifacts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/evm/out")
}

fn creation_code(name: &str) -> Vec<u8> {
    let path = artifacts()
        .join(format!("{name}.sol"))
        .join(format!("{name}.json"));
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the contracts are built"))
            .unwrap();
    alloy::hex::decode(artifact["bytecode"]["object"].as_str().unwrap()).unwrap()
}

/// A launcher on a unix socket that answers every token request, which
/// must ask for a PKI token, with a PKI token of Google's shape, for the
/// nonce asked, under the test chain and dated `iat`.
async fn fake_launcher(socket: &Path, iat: u64) -> crate::AbortOnDrop {
    let _ = std::fs::remove_file(socket);
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let app = axum::Router::new().route(
        "/v1/token",
        axum::routing::post(
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let nonce = body["nonces"][0].as_str().unwrap_or_default().to_string();
                assert_eq!(body["audience"], crate::attestation::AUDIENCE);
                assert_eq!(body["token_type"], "PKI");
                let mut claims = fixture::claims(&fixture::binding(), iat);
                claims["eat_nonce"] = serde_json::json!(nonce);
                fixture::chain::token(
                    &fixture::chain::chain(iat),
                    &fixture::chain::key(fixture::chain::LEAF_SEED),
                    &claims,
                )
            },
        ),
    );
    crate::AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs anvil and the attestation prover (a real Groth16 proof, Docker, minutes); see the module's comment"]
async fn one_real_groth16_proof_of_a_launcher_token_blesses_every_key_in_the_tree() {
    let prover = std::env::var("SHONIN_PROVER_BIN").expect("SHONIN_PROVER_BIN names the prover");
    let scratch = std::env::temp_dir().join(format!("shonin-attested-path-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    // The chain.
    let port = free_port();
    let _anvil = Running(
        Command::new("anvil")
            .args([
                "--port",
                &port.to_string(),
                "--chain-id",
                "31337",
                "--silent",
            ])
            .stdout(Stdio::null())
            .spawn()
            .expect("anvil runs"),
    );
    let url = format!("http://127.0.0.1:{port}");
    let config = chain::ChainConfig {
        label: "anvil".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: Some(2_500.0),
        public_rpc: url.clone(),
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = loop {
        if let Ok(client) = chain::Client::connect(&config, chain::Signing::Unlocked).await
            && client.block_number().await.is_ok()
        {
            break client;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let accounts = client.provider().get_accounts().await.unwrap();
    let (admin, attestor, keeper) = (accounts[0], accounts[1], accounts[2]);

    // The registry, its development checker, and a Confidential Space
    // checker for the pinned guest that trusts the test chain's root; the
    // fixture's image and project allowed. The launcher's tokens are dated a
    // minute before the chain's head (the registry refuses one from after
    // its block).
    let iat = client.block_timestamp().await.unwrap() - 60;
    let mut dev = creation_code("DevAttestationVerifier");
    dev.extend(attestor.abi_encode());
    let dev = client.deploy(admin, dev).await.unwrap();
    let mut registry = creation_code("AttestationRegistry");
    registry.extend((admin, dev, 0u64, 0u64, 72 * 3600u64).abi_encode_params());
    let registry = client.deploy(admin, registry).await.unwrap();
    let proposed = shonin_attestation_deploy::deploy_and_propose(
        &client,
        admin,
        &artifacts(),
        registry,
        None,
        shonin_attestation_deploy::GUEST_IMAGE_ID,
        &[B256::from(fixture::chain::root_hash(iat))],
    )
    .await
    .unwrap();
    let image: B256 = fixture::IMAGE_DIGEST
        .trim_start_matches("sha256:")
        .parse()
        .unwrap();
    client
        .send(
            admin,
            registry,
            IAttestationRegistry::proposeImageCall {
                imageDigest: image,
                kind: ImageKind::Executor,
            },
        )
        .await
        .unwrap();
    client
        .send(
            admin,
            registry,
            IAttestationRegistry::proposeProjectCall {
                projectId: keccak256(fixture::PROJECT),
            },
        )
        .await
        .unwrap();

    // The executor's attestation, holding three vaults' keys on the chain.
    let mut deployments = crate::stand_in::deployments();
    deployments.attestation = registry;
    let attestation = Attestation::new(Mode::Launcher).unwrap();
    attestation.serve_chain(client.clone(), deployments);
    let keys: Vec<Arc<crate::keys::Key>> = (0..3)
        .map(|_| Arc::new(crate::keys::Key::fresh().unwrap()))
        .collect();
    for (i, key) in keys.iter().enumerate() {
        attestation.put(
            31337,
            Address::repeat_byte(0x10 + i as u8),
            B256::repeat_byte(0x5b),
            key.address(),
        );
    }

    // The launcher, and the status port.
    let socket = scratch.join("teeserver.sock");
    let _launcher = fake_launcher(&socket, iat).await;
    let _attesting = crate::AbortOnDrop(tokio::spawn(attestation.clone().run(socket)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let executor_url = format!("http://{}", listener.local_addr().unwrap());
    let routes = crate::attestation::routes(attestation.clone());
    let _status = crate::AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    }));
    let started = Instant::now();
    while !attestation.status().seal_wanted {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no token was asked for"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The prover, against the test chain's root.
    let root = scratch.join("test-root.crt");
    std::fs::write(&root, fixture::chain::root_pem(iat)).unwrap();
    let proving = Instant::now();
    let _prover = Running(
        Command::new(&prover)
            .args([
                "--executor-url",
                &executor_url,
                "--root",
                root.to_str().unwrap(),
                "--poll-seconds",
                "2",
                "--idle-exit-seconds",
                "1200",
            ])
            .spawn()
            .expect("the prover runs"),
    );
    let status = loop {
        let status = attestation.status();
        if status.seal.is_some() {
            break status;
        }
        assert!(
            proving.elapsed() < Duration::from_secs(20 * 60),
            "no seal was taken within twenty minutes"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    println!(
        "the prover's seal was taken {} s after it started",
        proving.elapsed().as_secs()
    );
    assert!(!status.seal_wanted);
    let seal: alloy::primitives::Bytes = status.seal.unwrap().parse().unwrap();
    let journal: alloy::primitives::Bytes = status.journal.unwrap().parse().unwrap();
    assert_eq!(seal.len(), 324);
    assert_eq!(journal.len(), 160);

    // As the keeper sends them: every key in the tree, from the one seal.
    for leaf in &status.leaves {
        let (vault, strategy, key): (Address, B256, Address) = (
            leaf.vault.parse().unwrap(),
            leaf.strategy_hash.parse().unwrap(),
            leaf.key.parse().unwrap(),
        );
        client
            .send(
                keeper,
                registry,
                IAttestationRegistry::blessCall {
                    seal: seal.clone(),
                    journal: journal.clone(),
                    vaultChain: U256::from(leaf.vault_chain),
                    vault,
                    strategyHash: strategy,
                    key,
                    merkleProof: leaf
                        .proof
                        .iter()
                        .map(|node| node.parse().unwrap())
                        .collect(),
                },
            )
            .await
            .unwrap();
        let blessed = client
            .read(
                registry,
                IAttestationRegistry::isBlessedForCall {
                    signer: key,
                    vaultChain: U256::from(leaf.vault_chain),
                    vault,
                    strategyHash: strategy,
                },
            )
            .await
            .unwrap();
        assert!(blessed, "a key in the tree was not blessed");
        let info = client
            .read(registry, IAttestationRegistry::keyInfoCall { signer: key })
            .await
            .unwrap();
        assert_eq!(
            info.verifier, proposed.checker,
            "blessed by another checker"
        );
    }
    assert_eq!(status.leaves.len(), keys.len());
    // The instance's own key names the instance and is blessed for nothing.
    let instance = client
        .read(
            registry,
            IAttestationRegistry::keyInfoCall {
                signer: attestation.instance(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(instance.kind, ImageKind::None));
    let _ = std::fs::remove_dir_all(&scratch);
}
