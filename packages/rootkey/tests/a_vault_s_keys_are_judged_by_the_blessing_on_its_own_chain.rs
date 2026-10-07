//! A vault's venue and bundle keys go to the executor blessed for that vault,
//! whichever chain of the network the vault lives on.
//!
//! A vault key is blessed on the vault's own chain: the executor's leaf for
//! it names that chain, `bless` is sent to that chain's registry, and the
//! canonical chain's registry never hears of it. So the holder reads whether
//! the proving key is blessed for the vault, and which image it came from,
//! from the registry on the vault's chain, while the asker's image kind, its
//! project and the checkers whose Google keys sign tokens are judged on the
//! canonical chain. A vault on a chain whose registry the holder doesn't read
//! is refused, never answered from another chain's registry.
//!
//! Each chain here is a stand-in node answering its own registry's reads, and
//! the holder reads them through the same network view the executor uses.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolCall;
use chain::bindings::attestation::{IAttestationRegistry, Image, ImageKind, KeyInfo};
use serde_json::{Value, json};
use shonin_attestation_core::{Binding, Jwk, fixture};
use shonin_rootkey::{
    Error, NetworkView, Purpose, Received, RegistryView, Requester, Root, authorise,
};

const NOW: u64 = 1_800_000_000;
/// The canonical chain, whose registry lists images, projects and checkers.
const CANONICAL: u64 = 84_532;
/// Another EVM chain of the same network, with vaults of its own.
const OTHER: u64 = 998;
/// A chain of the network whose registry this holder doesn't read.
const UNREAD: u64 = 999;

alloy::sol! {
    interface IGoogleKeys {
        function trustedKeys() external view returns (bytes32[] memory);
    }
}

fn executor_image() -> B256 {
    B256::repeat_byte(0xE1)
}

fn vault() -> Address {
    Address::repeat_byte(0x0A)
}

fn strategy() -> B256 {
    B256::repeat_byte(0x5B)
}

fn checker() -> Address {
    Address::repeat_byte(0xCC)
}

fn project() -> B256 {
    B256::from(shonin_attestation_core::project_id(fixture::PROJECT))
}

/// One chain's `AttestationRegistry`, as its node answers it.
#[derive(Default)]
struct Registry {
    /// Whether it lists the network's images, projects and checkers: the
    /// canonical chain's does; another chain's is asked only about its
    /// blessings here, and refuses anything else, so an answer from it about
    /// the asker would fail the test.
    lists: bool,
    blessed: HashSet<(Address, u64, Address, B256)>,
    key_images: HashMap<Address, B256>,
}

impl Registry {
    /// What `bless` records on this chain for a key made in the executor's
    /// image, for a vault on `vault_chain`.
    fn bless(&mut self, key: Address, vault_chain: u64) {
        self.blessed.insert((key, vault_chain, vault(), strategy()));
        self.key_images.insert(key, executor_image());
    }
}

fn answer(chain_id: u64, registry: &Registry, request: &Value) -> Value {
    let id = request["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let refuse = || json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}});
    match request["method"].as_str().unwrap_or_default() {
        "eth_chainId" => ok(json!(format!("0x{chain_id:x}"))),
        "eth_blockNumber" => ok(json!("0x10")),
        "eth_call" => {
            let call = &request["params"][0];
            let to: Address = call["to"].as_str().unwrap_or_default().parse().unwrap();
            let data = call["input"]
                .as_str()
                .or_else(|| call["data"].as_str())
                .unwrap_or_default();
            let input = alloy::hex::decode(data).unwrap_or_default();
            let Some(selector) = input
                .get(..4)
                .and_then(|head| <[u8; 4]>::try_from(head).ok())
            else {
                return refuse();
            };
            let out = match selector {
                IAttestationRegistry::isBlessedForCall::SELECTOR => {
                    let asked = IAttestationRegistry::isBlessedForCall::abi_decode(&input).unwrap();
                    let vault_chain = u64::try_from(asked.vaultChain).unwrap();
                    IAttestationRegistry::isBlessedForCall::abi_encode_returns(
                        &registry.blessed.contains(&(
                            asked.signer,
                            vault_chain,
                            asked.vault,
                            asked.strategyHash,
                        )),
                    )
                }
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    let asked = IAttestationRegistry::keyInfoCall::abi_decode(&input).unwrap();
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: registry
                            .key_images
                            .get(&asked.signer)
                            .copied()
                            .unwrap_or_default(),
                        projectId: B256::ZERO,
                        kind: ImageKind::Executor,
                        strategyHash: B256::ZERO,
                        vaultChain: U256::ZERO,
                        vault: Address::ZERO,
                        trustRoot: B256::ZERO,
                        verifier: Address::ZERO,
                        blessedUntil: 0,
                    })
                }
                _ if !registry.lists => return refuse(),
                IAttestationRegistry::verifiersCall::SELECTOR => {
                    IAttestationRegistry::verifiersCall::abi_encode_returns(&vec![checker()])
                }
                IAttestationRegistry::isVerifierAllowedCall::SELECTOR => {
                    IAttestationRegistry::isVerifierAllowedCall::abi_encode_returns(&true)
                }
                IGoogleKeys::trustedKeysCall::SELECTOR if to == checker() => {
                    IGoogleKeys::trustedKeysCall::abi_encode_returns(&vec![B256::from(
                        fixture::test_jwk().key_hash(),
                    )])
                }
                IAttestationRegistry::isImageAllowedCall::SELECTOR => {
                    let asked =
                        IAttestationRegistry::isImageAllowedCall::abi_decode(&input).unwrap();
                    IAttestationRegistry::isImageAllowedCall::abi_encode_returns(
                        &(asked.imageDigest == executor_image()),
                    )
                }
                IAttestationRegistry::imageOfCall::SELECTOR => {
                    IAttestationRegistry::imageOfCall::abi_encode_returns(&Image {
                        kind: ImageKind::Executor,
                        allowedAt: 1,
                        revoked: false,
                    })
                }
                IAttestationRegistry::isProjectAllowedCall::SELECTOR => {
                    let asked =
                        IAttestationRegistry::isProjectAllowedCall::abi_decode(&input).unwrap();
                    IAttestationRegistry::isProjectAllowedCall::abi_encode_returns(
                        &(asked.projectId == project()),
                    )
                }
                _ => return refuse(),
            };
            ok(json!(alloy::hex::encode_prefixed(out)))
        }
        _ => json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": "not answered here"}}),
    }
}

/// A node for `chain_id` answering `registry`'s reads, and a registry reader
/// over it.
async fn a_chain(chain_id: u64, registry: Arc<Mutex<Registry>>) -> RegistryView {
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
            let registry = registry.clone();
            async move { axum::Json(answer(chain_id, &registry.lock().unwrap(), &request)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = chain::ChainConfig {
        label: format!("chain {chain_id}"),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = chain::Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap();
    RegistryView::new(client, Address::repeat_byte(0xA7), chain_id)
}

/// The network as the holder reads it: the canonical chain's registry and
/// the other chain's, both connected (the unread chain's is not). Its
/// registries, for a test to bless keys in.
async fn a_network() -> (NetworkView, Arc<Mutex<Registry>>, Arc<Mutex<Registry>>) {
    let canonical = Arc::new(Mutex::new(Registry {
        lists: true,
        ..Registry::default()
    }));
    let other = Arc::new(Mutex::new(Registry::default()));
    let view = NetworkView::new(CANONICAL);
    view.connect(a_chain(CANONICAL, canonical.clone()).await);
    view.connect(a_chain(OTHER, other.clone()).await);
    (view, canonical, other)
}

fn google_keys() -> Vec<Jwk> {
    let mut keys = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06).expect("Google's JWKS parses");
    keys.push(fixture::test_jwk());
    keys
}

/// An executor's token for an exchange: its nonce `nonce`, from the executor
/// image, issued 30 seconds ago.
fn executor_token(nonce: &str) -> String {
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!([nonce]);
    claims["submods"]["container"]["image_digest"] =
        json!(format!("sha256:{}", hex::encode(executor_image())));
    fixture::sign(&claims)
}

/// Ask the holder for `purpose` as an executor proving with `vault_key` for
/// the sample vault on `vault_chain`.
async fn ask(
    network: &NetworkView,
    root: &Root,
    purpose: Purpose,
    vault_chain: u64,
    vault_key: &PrivateKeySigner,
) -> Result<Received, Error> {
    let requester = Requester::new(purpose).unwrap();
    let proof = requester
        .prove(vault_key, vault_chain, vault(), strategy())
        .unwrap();
    let request = requester.request(executor_token(&requester.nonce()), Some(proof));
    let release = authorise(root, &google_keys(), &request, NOW, network).await?;
    requester.open(&request, &release)
}

fn the_vault_s_keys(vault_chain: u64) -> [Purpose; 2] {
    [
        Purpose::venue(vault_chain, vault(), "hyperliquid").unwrap(),
        Purpose::Bundle(strategy()),
    ]
}

/// The executor of a vault on the canonical chain: its key is blessed there,
/// and it is given the vault's venue key and its bundle's key.
#[tokio::test]
async fn a_vault_on_the_canonical_chain_gets_its_keys() {
    let root = Root::generate().unwrap();
    let (network, canonical, _) = a_network().await;
    let vault_key = PrivateKeySigner::random();
    canonical
        .lock()
        .unwrap()
        .bless(vault_key.address(), CANONICAL);

    for purpose in the_vault_s_keys(CANONICAL) {
        let got = ask(&network, &root, purpose.clone(), CANONICAL, &vault_key).await;
        assert!(
            matches!(got, Ok(Received::Key(_))),
            "{purpose} was refused for a vault on the canonical chain: {got:?}"
        );
    }
}

/// The executor of a vault on another chain: its key is blessed on that
/// chain alone, and the canonical chain's registry has never heard of it.
/// The vault's venue credentials and bundle secrets are still its to open:
/// the blessing and the key's image are read from the vault's chain, while
/// the asker's image, project and the checkers are read from the canonical
/// chain (the other chain's node refuses those questions).
#[tokio::test]
async fn a_vault_on_another_chain_gets_its_keys_from_that_chain_s_blessing() {
    let root = Root::generate().unwrap();
    let (network, canonical, other) = a_network().await;
    let vault_key = PrivateKeySigner::random();
    other.lock().unwrap().bless(vault_key.address(), OTHER);
    assert!(canonical.lock().unwrap().blessed.is_empty());

    for purpose in the_vault_s_keys(OTHER) {
        let got = ask(&network, &root, purpose.clone(), OTHER, &vault_key).await;
        assert!(
            matches!(got, Ok(Received::Key(_))),
            "{purpose} was refused for a vault on chain {OTHER}, whose executor is blessed there: {got:?}"
        );
    }
}

/// A key blessed on one chain for one vault opens nothing for another vault
/// on that chain, nor for the same vault named on another chain, whose
/// registry has no such blessing.
#[tokio::test]
async fn a_key_blessed_on_one_chain_for_one_vault_gets_no_other_vault_s_keys() {
    let root = Root::generate().unwrap();
    let (network, _, other) = a_network().await;
    let vault_key = PrivateKeySigner::random();
    other.lock().unwrap().bless(vault_key.address(), OTHER);

    // The same vault, named on the canonical chain.
    let got = ask(
        &network,
        &root,
        Purpose::venue(CANONICAL, vault(), "hyperliquid").unwrap(),
        CANONICAL,
        &vault_key,
    )
    .await;
    assert!(matches!(got, Err(Error::NotBlessed)), "{got:?}");

    // Another vault on the key's own chain.
    let other_vault = Address::repeat_byte(0x0B);
    let purpose = Purpose::venue(OTHER, other_vault, "hyperliquid").unwrap();
    let requester = Requester::new(purpose).unwrap();
    let proof = requester
        .prove(&vault_key, OTHER, other_vault, strategy())
        .unwrap();
    let request = requester.request(executor_token(&requester.nonce()), Some(proof));
    let got = authorise(&root, &google_keys(), &request, NOW, &network).await;
    assert!(matches!(got, Err(Error::NotBlessed)), "{got:?}");
}

/// A vault on a chain whose registry the holder doesn't read is refused for
/// that, even when another chain's registry would say yes: its blessing is
/// never answered from another chain.
#[tokio::test]
async fn a_vault_on_a_chain_the_holder_does_not_read_is_refused() {
    let root = Root::generate().unwrap();
    let (network, canonical, other) = a_network().await;
    let vault_key = PrivateKeySigner::random();
    // Both read registries would bless the key for the vault named on the
    // unread chain.
    canonical.lock().unwrap().bless(vault_key.address(), UNREAD);
    other.lock().unwrap().bless(vault_key.address(), UNREAD);

    for purpose in the_vault_s_keys(UNREAD) {
        let got = ask(&network, &root, purpose.clone(), UNREAD, &vault_key).await;
        assert!(
            matches!(got, Err(Error::VaultChainNotRead(UNREAD))),
            "{purpose}: {got:?}"
        );
    }
}
