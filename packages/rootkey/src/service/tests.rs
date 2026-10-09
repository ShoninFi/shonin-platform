use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use serde_json::{Value, json};
use shonin_attestation_core::fixture;

use super::attestation::{due, service_leaf, tests_support};
use super::*;
use crate::chain_view::ImageKind;
use crate::{Root, Service};

const CHAIN_A: u64 = 84_532;
const CHAIN_B: u64 = 998;
const TTL: u64 = 72 * 3_600;
const NOW: u64 = 1_800_000_000;
const SOLANA_KEY: chain::solana::Pubkey = chain::solana::Pubkey::new_from_array([7; 32]);
const DEVNET: &str = "solana:devnet";
const DEVNET_VALUE: [u8; 32] = [0xD5; 32];

fn seed(byte: u8) -> [u8; 32] {
    [byte; 32]
}

#[test]
fn an_identity_is_the_same_for_the_same_seed_and_another_for_another() {
    let one = Identity::from_seed(seed(1));
    assert_eq!(one.address(), Identity::from_seed(seed(1)).address());
    assert_ne!(one.address(), Identity::from_seed(seed(2)).address());
    assert_eq!(one.signer().address(), one.address());
}

#[test]
fn a_service_s_solana_key_is_its_seed_s_alone_and_never_its_signer() {
    let one = Identity::from_seed(seed(1));
    assert_eq!(
        one.solana_address(),
        Identity::from_seed(seed(1)).solana_address()
    );
    assert_ne!(
        one.solana_address(),
        Identity::from_seed(seed(2)).solana_address()
    );
    use chain::solana::Signer as _;
    assert_eq!(one.solana().pubkey(), one.solana_address());
    // Neither the EVM signer's secret nor the state key: a label of its own.
    let secret = one.solana().to_bytes();
    assert_ne!(&secret[..32], one.signer().to_bytes().as_slice());
    assert!(!format!("{one:?}").contains(&one.solana_address().to_string()));
}

#[test]
fn a_state_blob_opens_only_under_its_own_identity_and_name() {
    let identity = Identity::from_seed(seed(1));
    let sealed = identity.seal_state("queue", b"the queue").unwrap();
    assert_eq!(
        identity.open_state("queue", &sealed).unwrap().as_slice(),
        b"the queue"
    );
    // Two seals of the same thing differ (a fresh nonce each), and both open.
    let again = identity.seal_state("queue", b"the queue").unwrap();
    assert_ne!(sealed, again);
    assert!(identity.open_state("queue", &again).is_ok());

    // Under another name: the same key, so corrupt, not another's.
    assert_eq!(
        identity.open_state("pending", &sealed).unwrap_err(),
        StateError::Corrupt
    );
    // Under another seed: another key's, to be set aside.
    assert_eq!(
        Identity::from_seed(seed(2))
            .open_state("queue", &sealed)
            .unwrap_err(),
        StateError::OtherKey
    );
    // Altered anywhere past its header, or cut short.
    for at in [HEADER_LEN, HEADER_LEN + NONCE_LEN, sealed.len() - 1] {
        let mut tampered = sealed.clone();
        tampered[at] ^= 1;
        assert_eq!(
            identity.open_state("queue", &tampered).unwrap_err(),
            StateError::Corrupt,
            "byte {at}"
        );
    }
    assert_eq!(
        identity.open_state("queue", &sealed[..20]).unwrap_err(),
        StateError::Corrupt
    );
    let mut versioned = sealed.clone();
    versioned[0] = 2;
    assert_eq!(
        identity.open_state("queue", &versioned).unwrap_err(),
        StateError::Corrupt
    );
}

#[test]
fn a_request_s_signature_recovers_its_signer_and_nothing_else() {
    let identity = Identity::from_seed(seed(1));
    let signature = identity
        .sign_request("PUT", "/internal/state/builder/queue", b"body", 1_000)
        .unwrap();
    let bytes = alloy::hex::decode(&signature).unwrap();
    assert_eq!(bytes.len(), 65);
    assert!(signature.starts_with("0x"));
    assert_eq!(
        request_signer(
            "PUT",
            "/internal/state/builder/queue",
            b"body",
            1_000,
            &bytes
        )
        .unwrap(),
        identity.address()
    );
    // Any part changed recovers someone else, or nothing.
    for (method, path, body, at) in [
        ("GET", "/internal/state/builder/queue", &b"body"[..], 1_000),
        ("PUT", "/internal/state/builder/other", b"body", 1_000),
        ("PUT", "/internal/state/builder/queue", b"bodY", 1_000),
        ("PUT", "/internal/state/builder/queue", b"body", 1_001),
    ] {
        assert_ne!(
            request_signer(method, path, body, at, &bytes).ok(),
            Some(identity.address()),
            "{method} {path} {at}"
        );
    }
    assert!(request_signer("PUT", "/", b"", 0, &bytes[..64]).is_err());
}

/// The digest is spelled out so a verifier in any language can be checked
/// against it.
#[test]
fn the_request_digest_is_the_documented_encoding() {
    use sha2::{Digest, Sha256};
    let mut bytes = b"shonin:internal-request:v1".to_vec();
    bytes.extend_from_slice(b"POST\0/internal/wake/reviewer\0");
    bytes.extend_from_slice(&Sha256::digest(b"{}"));
    bytes.extend_from_slice(&7u64.to_be_bytes());
    assert_eq!(
        request_digest("POST", "/internal/wake/reviewer", b"{}", 7),
        alloy::primitives::keccak256(bytes)
    );
    // The separators keep a method and a path from running together.
    assert_ne!(
        request_digest("POS", "T/x", b"", 0),
        request_digest("POST", "/x", b"", 0)
    );
}

#[test]
fn no_secret_of_an_identity_appears_in_its_debug_output() {
    let raw = seed(9);
    let identity = Identity::from_seed(raw);
    let secrets = [
        alloy::hex::encode(raw),
        alloy::hex::encode(identity.signer().to_bytes()),
        alloy::hex::encode(identity.state_key.as_slice()),
    ];
    let said = [format!("{identity:?}"), format!("{identity:#?}")];
    for text in &said {
        assert!(text.contains(&identity.address().to_string()), "{text}");
        for secret in &secrets {
            assert!(!text.to_lowercase().contains(secret), "a secret in {text}");
        }
        assert!(!text.contains(&format!("{:?}", raw)));
    }
    // Nor in a refusal to open.
    let error = identity.open_state("x", b"short").unwrap_err();
    for secret in &secrets {
        assert!(!format!("{error} {error:?}").contains(secret.as_str()));
    }
}

#[test]
fn only_a_seed_makes_an_identity() {
    let root = Root::for_tests(1);
    let pair = root.key_pair(&Purpose::Builder).unwrap();
    assert!(Identity::from_received(Received::Key(pair)).is_err());
    let seed = root.service_seed(ImageKind::Builder, B256::ZERO, B256::ZERO);
    let expected = Identity::from_seed(*seed);
    let got = Identity::from_received(Received::Seed(seed)).unwrap();
    assert_eq!(got.address(), expected.address());
}

// The attestation ----------------------------------------------------------

/// OpenZeppelin's `MerkleProof.verify`, as the registry runs it.
fn oz_verify(proof: &[B256], root: B256, leaf: B256) -> bool {
    proof.iter().fold(leaf, |computed, sibling| {
        let (low, high) = if computed <= *sibling {
            (computed, *sibling)
        } else {
            (*sibling, computed)
        };
        alloy::primitives::keccak256([low.as_slice(), high.as_slice()].concat())
    }) == root
}

fn attestation(chains: &[u64]) -> ServiceAttestation {
    let identity = Identity::from_seed(seed(1));
    let attestation = ServiceAttestation::new(
        ImageKind::Builder,
        identity.address(),
        identity.solana_address(),
    )
    .unwrap();
    for chain in chains {
        attestation.serve_chain_id(*chain);
    }
    attestation
}

#[test]
fn only_a_builder_or_a_reviewer_attests_as_a_service() {
    for kind in [ImageKind::Executor, ImageKind::Unwinder, ImageKind::Holder] {
        assert!(ServiceAttestation::new(kind, Address::repeat_byte(1), SOLANA_KEY).is_err());
    }
    assert!(
        ServiceAttestation::new(ImageKind::Reviewer, Address::repeat_byte(1), SOLANA_KEY).is_ok()
    );
}

#[test]
fn a_service_leaf_proves_under_the_root_by_the_registry_s_rule() {
    let attestation = attestation(&[CHAIN_A, CHAIN_B, 46_630]);
    let key = attestation.key();
    let status = attestation.status();
    let root: B256 = status.binding.vaults_root.parse().unwrap();
    assert_eq!(
        status.binding.evm_signer.parse::<Address>().unwrap(),
        attestation.instance()
    );
    assert_eq!(status.nonce, attestation.binding().nonce());
    assert_eq!(status.leaves.len(), 3);
    for view in &status.leaves {
        // The registry's leaf for a key that serves no vault.
        let leaf = chain::attest::key_leaf(
            alloy::primitives::U256::ZERO,
            Address::ZERO,
            B256::ZERO,
            alloy::primitives::U256::from(view.chain_id),
            key,
        );
        assert_eq!(view.leaf.parse::<B256>().unwrap(), leaf);
        assert_eq!(service_leaf(view.chain_id, key), leaf);
        assert_eq!(view.vault_chain, 0);
        assert_eq!(view.vault.parse::<Address>().unwrap(), Address::ZERO);
        assert_eq!(view.strategy_hash.parse::<B256>().unwrap(), B256::ZERO);
        assert_eq!(view.key.parse::<Address>().unwrap(), key);
        let proof: Vec<B256> = view.proof.iter().map(|p| p.parse().unwrap()).collect();
        assert!(oz_verify(&proof, root, leaf), "chain {}", view.chain_id);
        // And on no other chain.
        assert!(!oz_verify(
            &proof,
            root,
            service_leaf(view.chain_id + 1, key)
        ));
    }
}

/// The status reads as the executor's: the same fields, no root, and the
/// keeper's and the prover's readers find what they need.
#[test]
fn the_status_is_in_the_executor_s_shape() {
    let attestation = attestation(&[CHAIN_A]);
    let json = serde_json::to_value(attestation.status()).unwrap();
    let mut fields: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "binding",
            "journal",
            "leaves",
            "mode",
            "nonce",
            "root",
            "seal",
            "seal_wanted",
            "solana",
            "token",
            "token_issued_at"
        ]
    );
    assert_eq!(json["root"], Value::Null);
    assert_eq!(json["mode"], "launcher");
    let leaf = &json["leaves"][0];
    assert_eq!(leaf["vault"], format!("0x{}", "00".repeat(20)));
    assert_eq!(leaf["strategy_hash"], format!("0x{}", "00".repeat(32)));
    assert_eq!(leaf["vault_chain"], 0);
    let mut leaf_fields: Vec<&str> = leaf
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    leaf_fields.sort_unstable();
    assert_eq!(
        leaf_fields,
        [
            "chain_id",
            "key",
            "leaf",
            "proof",
            "strategy_hash",
            "vault",
            "vault_chain"
        ]
    );
    assert_eq!(attestation.development().status().mode, "development");
}

#[test]
fn a_blessing_is_due_unblessed_or_at_half_its_length() {
    assert!(due(false, NOW + TTL, NOW, TTL));
    assert!(!due(true, NOW + TTL, NOW, TTL));
    assert!(!due(true, NOW + TTL / 2, NOW, TTL));
    assert!(due(true, NOW + TTL / 2 - 1, NOW, TTL));
}

#[test]
fn ready_busy_and_renew_at_follow_what_the_chains_say() {
    let attestation = attestation(&[CHAIN_A, CHAIN_B]);
    // Nothing read yet: busy, ready nowhere, no renewal known.
    assert!(attestation.busy());
    assert!(!attestation.ready(CHAIN_A));
    assert_eq!(attestation.renew_at(), None);

    attestation.note(CHAIN_A, true, NOW + TTL, Some(NOW), TTL);
    attestation.note(CHAIN_B, false, 0, Some(NOW), TTL);
    assert!(attestation.ready(CHAIN_A));
    assert!(!attestation.ready(CHAIN_B));
    assert!(attestation.busy());
    assert_eq!(attestation.renew_at(), Some(NOW + TTL - TTL / 3));

    attestation.note(CHAIN_B, true, NOW + TTL - 100, Some(NOW), TTL);
    assert!(!attestation.busy());
    assert_eq!(attestation.renew_at(), Some(NOW + TTL - 100 - TTL / 3));

    // A chain that can't be read keeps what it last said.
    attestation.note(CHAIN_A, false, 0, None, 0);
    assert!(attestation.ready(CHAIN_A));
    assert!(!attestation.busy());

    // Down to half: busy again, and still ready while it lasts.
    attestation.note(CHAIN_A, true, NOW + TTL / 2 - 1, Some(NOW), TTL);
    assert!(attestation.busy());
    assert!(attestation.ready(CHAIN_A));
    assert!(!attestation.ready(46_630));
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A token from the builder image `digest`, for `nonce`, issued at `iat`.
fn token_for(nonce: &str, digest: B256, iat: u64) -> String {
    let mut claims = fixture::claims(&Default::default(), iat);
    claims["eat_nonce"] = json!([nonce]);
    claims["submods"]["container"]["image_digest"] =
        json!(format!("sha256:{}", alloy::hex::encode(digest)));
    fixture::sign(&claims)
}

fn project() -> B256 {
    B256::from(shonin_attestation_core::project_id(fixture::PROJECT))
}

#[test]
fn a_token_is_wanted_while_due_and_unanswered_and_a_seal_is_kept_only_for_its_token() {
    let attestation = attestation(&[CHAIN_A]);
    let digest = B256::repeat_byte(0xB1);
    // Nothing read: nothing is asked for on a guess.
    assert!(!tests_support::wanted(&attestation, NOW));
    attestation.note(CHAIN_A, false, 0, Some(NOW), TTL);
    assert!(tests_support::wanted(&attestation, NOW));
    assert!(!attestation.status().seal_wanted);

    let nonce = attestation.binding().nonce();
    tests_support::hold(&attestation, token_for(&nonce, digest, NOW)).unwrap();
    let status = attestation.status();
    assert!(status.seal_wanted);
    assert_eq!(status.token_issued_at, Some(NOW));
    // A token waiting for its seal is not asked for again until it ages.
    assert!(!tests_support::wanted(&attestation, NOW + 60));
    assert!(tests_support::wanted(&attestation, NOW + 40 * 60));
    // A token for another tree is not held.
    assert!(tests_support::hold(&attestation, token_for("00", digest, NOW)).is_err());

    // A seal for that token, naming the instance, the tree and the token's
    // image, project and time.
    let root = B256::from(attestation.binding().vaults_root);
    let post = |image: B256, signer: Address, root: B256, issued: u64, nonce: &str| SealPost {
        nonce: nonce.into(),
        seal: "0x01".into(),
        journal: alloy::hex::encode_prefixed(tests_support::journal(
            image,
            project(),
            signer,
            root,
            issued,
        )),
    };
    let instance = attestation.instance();
    for (bad, why) in [
        (post(digest, instance, root, NOW, "00"), "nonce"),
        (post(B256::ZERO, instance, root, NOW, &nonce), "image"),
        (
            post(digest, Address::repeat_byte(3), root, NOW, &nonce),
            "instance",
        ),
        (post(digest, instance, B256::ZERO, NOW, &nonce), "tree"),
        (post(digest, instance, root, NOW - 1, &nonce), "token"),
    ] {
        assert!(
            tests_support::accept_unsimulated(&attestation, &bad).is_err(),
            "a seal for another {why} was kept"
        );
    }
    tests_support::accept_unsimulated(&attestation, &post(digest, instance, root, NOW, &nonce))
        .unwrap();
    let status = attestation.status();
    assert!(!status.seal_wanted);
    assert!(status.seal.is_some() && status.journal.is_some());
    // The seal answers until the chain applies it.
    assert!(!tests_support::wanted(&attestation, NOW + 60));
    attestation.note(CHAIN_A, true, NOW + TTL, Some(NOW + 120), TTL);
    assert!(!attestation.busy());
    assert!(!tests_support::wanted(&attestation, NOW + 120));
    // Half its length later, it is due, and that seal, applied, answers
    // nothing: a fresh token is wanted.
    attestation.note(CHAIN_A, true, NOW + TTL, Some(NOW + TTL / 2 + 1), TTL);
    assert!(attestation.busy());
    assert!(tests_support::wanted(&attestation, NOW + TTL / 2 + 1));
}

#[tokio::test]
async fn a_posted_seal_is_not_kept_while_a_chain_cannot_simulate_it() {
    let attestation = attestation(&[CHAIN_A]);
    attestation.note(CHAIN_A, false, 0, Some(NOW), TTL);
    let digest = B256::repeat_byte(0xB1);
    let nonce = attestation.binding().nonce();
    tests_support::hold(&attestation, token_for(&nonce, digest, NOW)).unwrap();
    let post = SealPost {
        nonce,
        seal: "0x01".into(),
        journal: alloy::hex::encode_prefixed(tests_support::journal(
            digest,
            project(),
            attestation.instance(),
            B256::from(attestation.binding().vaults_root),
            NOW,
        )),
    };
    // The chain was named by id alone: there is nothing to simulate on.
    assert!(matches!(
        attestation.accept(post).await,
        Err(Refusal::Unavailable(_))
    ));
    assert!(attestation.status().seal.is_none());
}

// Solana ---------------------------------------------------------------------

fn on_devnet(chains: &[u64]) -> ServiceAttestation {
    let attestation = attestation(chains);
    attestation.serve_cluster_value(DEVNET.into(), DEVNET_VALUE);
    attestation
}

#[test]
fn a_solana_leaf_names_the_solana_key_on_its_cluster_alone_after_the_evm_leaves() {
    let attestation = on_devnet(&[CHAIN_A, CHAIN_B]);
    let status = attestation.status();
    let root: B256 = status.binding.vaults_root.parse().unwrap();
    // The instance's Solana key is the token's Solana signer: neither zero
    // nor the key that is blessed.
    let signer: B256 = status.binding.solana_signer.parse().unwrap();
    assert_eq!(signer.0, attestation.solana_instance().to_bytes());
    assert_ne!(signer, B256::ZERO);
    assert_ne!(attestation.solana_instance(), attestation.solana_key());
    // The EVM leaves are where they were, and still prove.
    assert_eq!(status.leaves.len(), 2);
    for view in &status.leaves {
        let proof: Vec<B256> = view.proof.iter().map(|p| p.parse().unwrap()).collect();
        assert!(oz_verify(&proof, root, view.leaf.parse().unwrap()));
    }
    let [cluster] = status.solana.as_slice() else {
        panic!("one cluster");
    };
    assert_eq!(cluster.cluster, DEVNET);
    assert!(cluster.journal.is_none() && cluster.seal.is_none());
    let [leaf] = cluster.leaves.as_slice() else {
        panic!("one leaf");
    };
    // The Solana registry's leaf for a key serving no vault, blessed on this
    // cluster.
    let expected = B256::from(chain::solana::programs::common::digest::blessing_leaf(
        &[0; 32],
        &[0; 32],
        &[0; 32],
        &DEVNET_VALUE,
        &attestation.solana_key().to_bytes(),
    ));
    assert_eq!(leaf.leaf.parse::<B256>().unwrap(), expected);
    assert_eq!(leaf.key, attestation.solana_key().to_string());
    assert_eq!(leaf.vault, chain::solana::Pubkey::default().to_string());
    assert_eq!(leaf.strategy_hash, format!("0x{}", "00".repeat(32)));
    assert!(leaf.due);
    let proof: Vec<B256> = leaf.proof.iter().map(|p| p.parse().unwrap()).collect();
    assert!(oz_verify(&proof, root, expected));
    // For no other cluster, and not for the instance's key.
    assert!(!oz_verify(
        &proof,
        root,
        solana_service_leaf(&[0xD6; 32], &attestation.solana_key())
    ));
    assert!(!oz_verify(
        &proof,
        root,
        solana_service_leaf(&DEVNET_VALUE, &attestation.solana_instance())
    ));
}

#[test]
fn a_cluster_is_ready_once_it_says_the_solana_key_is_blessed_and_wants_renewal_as_a_chain_does() {
    let attestation = on_devnet(&[CHAIN_A]);
    attestation.note(CHAIN_A, true, NOW + TTL, Some(NOW), TTL);
    // The cluster not read yet: busy, and not ready there.
    assert!(attestation.busy());
    assert!(!attestation.ready_cluster(DEVNET));
    assert!(attestation.ready(CHAIN_A));
    attestation.note_cluster(DEVNET, false, 0, Some(NOW), TTL);
    assert!(tests_support::wanted(&attestation, NOW));
    attestation.note_cluster(DEVNET, true, NOW + TTL - 50, Some(NOW), TTL);
    assert!(attestation.ready_cluster(DEVNET));
    assert!(!attestation.busy());
    assert_eq!(attestation.renew_at(), Some(NOW + TTL - 50 - TTL / 3));
    assert!(!attestation.ready_cluster("solana:mainnet"));
    // Down to half on the cluster alone: busy again.
    attestation.note_cluster(DEVNET, true, NOW + TTL / 2 - 1, Some(NOW), TTL);
    assert!(attestation.busy());
}

/// A seal as the prover makes it for `attestation`'s current tree, with
/// `solana_signer` in its tail.
fn proved(attestation: &ServiceAttestation, digest: B256, solana_signer: [u8; 32]) -> SealPost {
    let canonical = shonin_attestation_core::Journal {
        image_digest: digest.0,
        project_id: project().0,
        vaults_root: attestation.binding().vaults_root,
        issued_at: NOW,
        evm_signer: attestation.instance().into_array(),
        solana_signer,
        key_hash: [5; 32],
    };
    let mut seal = vec![9u8; chain::solana::attest::GROTH16_SEAL_LENGTH];
    seal.extend_from_slice(&canonical.evm_seal_tail());
    SealPost {
        nonce: attestation.binding().nonce(),
        seal: alloy::hex::encode_prefixed(seal),
        journal: alloy::hex::encode_prefixed(canonical.evm()),
    }
}

#[test]
fn a_seal_naming_another_solana_key_is_refused_and_one_naming_the_instance_s_is_published_for_the_cluster()
 {
    let attestation = on_devnet(&[CHAIN_A]);
    let digest = B256::repeat_byte(0xB1);
    let nonce = attestation.binding().nonce();
    tests_support::hold(&attestation, token_for(&nonce, digest, NOW)).unwrap();
    for other in [[0; 32], attestation.solana_key().to_bytes(), [3; 32]] {
        assert!(
            tests_support::accept_unsimulated(&attestation, &proved(&attestation, digest, other))
                .is_err()
        );
    }
    let instance = attestation.solana_instance().to_bytes();
    tests_support::accept_unsimulated(&attestation, &proved(&attestation, digest, instance))
        .unwrap();
    let status = attestation.status();
    let cluster = &status.solana[0];
    let journal = alloy::hex::decode(cluster.journal.as_ref().unwrap()).unwrap();
    // The Solana registry's own journal: the same instance, tree, image and
    // time the EVM journal states.
    let read = <chain::solana::programs::registry::state::Journal as
        chain::solana::programs::anchor_lang::AnchorDeserialize>::deserialize(
        &mut journal.as_slice(),
    )
    .unwrap();
    assert_eq!(read.signer, attestation.solana_instance());
    assert_eq!(read.vaults_root, attestation.binding().vaults_root);
    assert_eq!(read.image_digest, digest.0);
    assert_eq!(read.issued_at, NOW);
    assert!(matches!(
        cluster.seal,
        Some(chain::solana::attest::SolanaSeal::Proof { .. })
    ));
}

#[tokio::test]
async fn a_seal_is_not_kept_for_a_cluster_alone_that_cannot_simulate_it() {
    let attestation = attestation(&[]);
    attestation.serve_cluster_value(DEVNET.into(), DEVNET_VALUE);
    let digest = B256::repeat_byte(0xB1);
    let nonce = attestation.binding().nonce();
    tests_support::hold(&attestation, token_for(&nonce, digest, NOW)).unwrap();
    let instance = attestation.solana_instance().to_bytes();
    // Named by its value alone: nothing to simulate on, and no EVM chain
    // that would take the seal, so it would bless the key nowhere.
    assert!(matches!(
        attestation
            .accept(proved(&attestation, digest, instance))
            .await,
        Err(Refusal::Unavailable(_))
    ));
    assert!(attestation.status().solana[0].seal.is_none());
}

// Obtaining the seed by exchange -------------------------------------------

/// A launcher on a unix socket that gives a token from `digest`'s image for
/// whatever nonce it is asked.
async fn launcher(dir: &std::path::Path, digest: B256) -> std::path::PathBuf {
    let socket = dir.join("teeserver.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let app = axum::Router::new().route(
        "/v1/token",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| async move {
            let nonce = body["nonces"][0].as_str().unwrap().to_string();
            token_for(&nonce, digest, unix_now() - 5)
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    socket
}

#[tokio::test]
async fn a_service_obtains_the_identity_of_its_own_image_by_exchange() {
    use crate::tests::StandIn;

    let chain = StandIn::new();
    let root = Root::for_tests(7);
    let digest = crate::tests::digest_of(ImageKind::Builder);
    let expected = Identity::from_seed(*root.service_seed(ImageKind::Builder, digest, project()));
    chain.record_root(Some(root.signer_address()));
    let holder = Service::new(chain.clone());
    holder.set_google_keys(vec![fixture::test_jwk()]);
    holder.hold(root);
    holder.set_serving(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::routes(holder);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // A unix socket's path is short (about a hundred bytes), shorter than
    // some systems' temporary directory.
    let base = if std::path::Path::new("/tmp").is_dir() {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!("shonin-sv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = launcher(&dir, digest).await;

    let got = identity(&url, &socket).await.unwrap();
    assert_eq!(got.address(), expected.address());
    // What it seals, the identity derived here opens: the same state key.
    let sealed = got.seal_state("queue", b"q").unwrap();
    assert_eq!(
        expected.open_state("queue", &sealed).unwrap().as_slice(),
        b"q"
    );

    // A reviewer's image is given another seed.
    let reviewer = crate::tests::digest_of(ImageKind::Reviewer);
    let other_dir = dir.join("r");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other_socket = launcher(&other_dir, reviewer).await;
    let theirs = identity(&url, &other_socket).await.unwrap();
    assert_ne!(theirs.address(), got.address());
    assert_eq!(
        theirs.open_state("queue", &sealed).unwrap_err(),
        StateError::OtherKey
    );
    let _ = std::fs::remove_dir_all(&dir);
}
