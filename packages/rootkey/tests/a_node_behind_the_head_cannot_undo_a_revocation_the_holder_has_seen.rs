//! Once the holder has refused an exchange because the canonical chain, read
//! at some block, no longer allows the asker's image, an answer from a node
//! behind that block (the same RPC name fronting several nodes) must not
//! release anything to that image: a revocation the holder has already seen
//! is never undone by an older view of the chain.
//!
//! Each decision reads the chain at one block: the head when it begins, and
//! never one older than a block an earlier decision read. A node behind that
//! block says it hasn't reached it, as a real node does, and nothing is
//! decided on its word.

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256};
use alloy::sol_types::SolCall;
use chain::bindings::attestation::{IAttestationRegistry, Image, ImageKind, RootKeyRecord};
use serde_json::{Value, json};
use shonin_attestation_core::{Binding, Jwk, fixture};
use shonin_rootkey::{Error, Purpose, RegistryView, Requester, Root, Service};

const NOW: u64 = 1_800_000_000;
const CANONICAL: u64 = 84_532;

alloy::sol! {
    interface IGoogleKeys {
        function trustedKeys() external view returns (bytes32[] memory);
    }
}

fn executor_image() -> B256 {
    B256::repeat_byte(0xE1)
}

fn checker() -> Address {
    Address::repeat_byte(0xCC)
}

fn project() -> B256 {
    B256::from(shonin_attestation_core::project_id(fixture::PROJECT))
}

/// The canonical chain as one node sees it: the block it is at, and whether
/// the executor image is still allowed at that block.
#[derive(Clone, Copy)]
struct View {
    head: u64,
    image_allowed: bool,
}

struct Node {
    view: Mutex<View>,
    root_signer: Address,
}

fn answer(node: &Node, request: &Value) -> Value {
    let view = *node.view.lock().unwrap();
    let id = request["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let refuse = || json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}});
    let not_reached = || json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "header not found"}});
    match request["method"].as_str().unwrap_or_default() {
        "eth_chainId" => ok(json!(format!("0x{CANONICAL:x}"))),
        "eth_blockNumber" => ok(json!(format!("0x{:x}", view.head))),
        "eth_getBlockByNumber" => ok(json!({
            "number": format!("0x{:x}", view.head),
            "timestamp": format!("0x{:x}", NOW),
            "hash": format!("{:#x}", B256::repeat_byte(view.head as u8)),
            "parentHash": format!("{:#x}", B256::ZERO),
            "transactions": [],
        })),
        "eth_call" => {
            // A read pinned to a block this node hasn't reached is refused,
            // as a real node refuses it.
            let pinned = request["params"][1]
                .as_str()
                .and_then(|tag| tag.strip_prefix("0x"))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok());
            if pinned.is_some_and(|block| block > view.head) {
                return not_reached();
            }
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
                        &(asked.imageDigest == executor_image() && view.image_allowed),
                    )
                }
                IAttestationRegistry::imageOfCall::SELECTOR => {
                    IAttestationRegistry::imageOfCall::abi_encode_returns(&Image {
                        kind: ImageKind::Executor,
                        allowedAt: 1,
                        revoked: !view.image_allowed,
                    })
                }
                IAttestationRegistry::isProjectAllowedCall::SELECTOR => {
                    let asked =
                        IAttestationRegistry::isProjectAllowedCall::abi_decode(&input).unwrap();
                    IAttestationRegistry::isProjectAllowedCall::abi_encode_returns(
                        &(asked.projectId == project()),
                    )
                }
                IAttestationRegistry::isRootKeyLiveCall::SELECTOR => {
                    IAttestationRegistry::isRootKeyLiveCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::rootKeyCall::SELECTOR => {
                    IAttestationRegistry::rootKeyCall::abi_encode_returns(&RootKeyRecord {
                        signer: node.root_signer,
                        issuedAt: NOW,
                        verifier: checker(),
                        setAt: NOW,
                        imageDigest: executor_image(),
                        projectId: project(),
                    })
                }
                _ => return refuse(),
            };
            ok(json!(alloy::hex::encode_prefixed(out)))
        }
        _ => json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": "not answered here"}}),
    }
}

async fn a_node(node: Arc<Node>) -> RegistryView {
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
            let node = node.clone();
            async move { axum::Json(answer(&node, &request)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = chain::ChainConfig {
        label: "canonical".to_string(),
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
    RegistryView::new(client, Address::repeat_byte(0xA7), CANONICAL)
}

fn google_keys() -> Vec<Jwk> {
    let mut keys = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06).expect("Google's JWKS parses");
    keys.push(fixture::test_jwk());
    keys
}

/// An executor's request for the root: a token from the executor image,
/// issued 30 seconds ago, its nonce binding a fresh ephemeral key.
fn an_executor_asks_for_the_root() -> shonin_rootkey::ExchangeRequest {
    let requester = Requester::new(Purpose::Root).unwrap();
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!([requester.nonce()]);
    claims["submods"]["container"]["image_digest"] =
        json!(format!("sha256:{}", hex::encode(executor_image())));
    requester.request(fixture::sign(&claims), None)
}

#[tokio::test]
async fn a_node_behind_the_head_cannot_undo_a_revocation_the_holder_has_seen() {
    let root = Root::generate().unwrap();
    let node = Arc::new(Node {
        view: Mutex::new(View {
            head: 20,
            image_allowed: false,
        }),
        root_signer: root.signer_address(),
    });
    let service = Service::new(a_node(node.clone()).await);
    service.set_google_keys(google_keys());
    service.hold(root);
    service.set_serving(true);

    // The chain at block 20: the executor image was revoked. Refused.
    assert_eq!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .err(),
        Some(Error::ImageNotAllowed)
    );

    // The next request is answered by a node ten blocks behind, which still
    // has the image allowed. The holder has already seen block 20 say
    // otherwise: nothing may be released on the older view.
    *node.view.lock().unwrap() = View {
        head: 10,
        image_allowed: true,
    };
    let decided = service
        .exchange_at(&an_executor_asks_for_the_root(), NOW)
        .await;
    assert_eq!(
        decided.err(),
        Some(Error::ChainUnavailable),
        "the root was released to an image the holder had already seen revoked, on the word of a node behind the head"
    );

    // The node reaches past block 20, the image still revoked: refused as
    // before, read at the newer block.
    *node.view.lock().unwrap() = View {
        head: 21,
        image_allowed: false,
    };
    assert_eq!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .err(),
        Some(Error::ImageNotAllowed)
    );
}
