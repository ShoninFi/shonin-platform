//! A decision reads the chain at one block, never older than one an earlier
//! decision read it at. The head that sets that floor comes from whichever
//! node answers `eth_blockNumber`; the pinned reads go to whichever nodes
//! have reached the block. A node on a losing fork, a node misreporting its
//! head, or a backend misrouted behind the same name can say a head the
//! chain never reaches, or reaches only much later.
//!
//! Such a head decides nothing: every read pinned to it is refused as not
//! reached, and the decision fails closed. But it must not raise the floor
//! for every later decision: a block nothing was ever read at was never
//! acted on, so once the nodes answer honestly again the holder decides on
//! the chain as it is. Otherwise one wrong answer stops every exchange and
//! every published key until the holder restarts, and a restart of the only
//! holder loses the root.
//!
//! Likewise a checker's Google keys: a checker that names none (it has no
//! such function, and the call reverts) adds none, but a read of them that
//! no node answered fails the decision as the chain being unavailable, not
//! as the token's key being untrusted.

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

/// The canonical chain as its RPC name answers: the head it says it is at,
/// and the newest block any node behind the name has actually reached and
/// can answer a pinned read at.
#[derive(Clone, Copy)]
struct View {
    said_head: u64,
    reached: u64,
}

/// How the checker answers a read of its Google keys.
#[derive(Clone, Copy, PartialEq)]
enum CheckerKeys {
    /// It names the fixture's test key.
    Named,
    /// It has no such function: the call reverts.
    Reverts,
    /// The node fails the read itself.
    Unanswered,
}

struct Node {
    view: Mutex<View>,
    root_signer: Address,
    checker_keys: Mutex<CheckerKeys>,
}

fn answer(node: &Node, request: &Value) -> Value {
    let view = *node.view.lock().unwrap();
    let id = request["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let refuse = || json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}});
    let not_reached = || json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "header not found"}});
    match request["method"].as_str().unwrap_or_default() {
        "eth_chainId" => ok(json!(format!("0x{CANONICAL:x}"))),
        "eth_blockNumber" => ok(json!(format!("0x{:x}", view.said_head))),
        "eth_getBlockByNumber" => ok(json!({
            "number": format!("0x{:x}", view.reached),
            "timestamp": format!("0x{:x}", NOW),
            "hash": format!("{:#x}", B256::repeat_byte(view.reached as u8)),
            "parentHash": format!("{:#x}", B256::ZERO),
            "transactions": [],
        })),
        "eth_call" => {
            // A read pinned to a block no node has reached is refused, as a
            // real node refuses it.
            let pinned = request["params"][1]
                .as_str()
                .and_then(|tag| tag.strip_prefix("0x"))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok());
            if pinned.is_some_and(|block| block > view.reached) {
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
                    match *node.checker_keys.lock().unwrap() {
                        CheckerKeys::Named => IGoogleKeys::trustedKeysCall::abi_encode_returns(
                            &vec![B256::from(fixture::test_jwk().key_hash())],
                        ),
                        CheckerKeys::Reverts => return refuse(),
                        CheckerKeys::Unanswered => {
                            return json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": -32603, "message": "internal error"}});
                        }
                    }
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
async fn a_head_no_node_can_serve_fails_one_decision_not_every_later_one() {
    let root = Root::generate().unwrap();
    let node = Arc::new(Node {
        view: Mutex::new(View {
            said_head: 20,
            reached: 20,
        }),
        root_signer: root.signer_address(),
        checker_keys: Mutex::new(CheckerKeys::Named),
    });
    let service = Service::new(a_node(node.clone()).await);
    service.set_google_keys(google_keys());
    service.hold(root);
    service.set_serving(true);

    // The chain at block 20, everything allowed: released.
    assert!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .is_ok()
    );

    // One answer to `eth_blockNumber` names a head far beyond anything the
    // nodes have: a node on a losing fork, or one misreporting. No read at
    // that block is answered, so nothing is decided on it.
    *node.view.lock().unwrap() = View {
        said_head: 1_000_000,
        reached: 20,
    };
    assert_eq!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .err(),
        Some(Error::ChainUnavailable)
    );

    // The nodes answer honestly again, one block on. Nothing was ever read
    // at the head that was said, so no decision was acted on at it, and the
    // holder decides on the chain as it is.
    *node.view.lock().unwrap() = View {
        said_head: 21,
        reached: 21,
    };
    let decided = service
        .exchange_at(&an_executor_asks_for_the_root(), NOW)
        .await;
    assert!(
        decided.is_ok(),
        "one wrong head that no read ever confirmed stops every later exchange: {:?}",
        decided.err()
    );
    // And a published key, which reads the same chain, is served again too.
    assert!(
        service
            .published(&Purpose::platform("dummy").unwrap())
            .await
            .is_ok(),
        "one wrong head that no read ever confirmed stops every later published key"
    );
}

#[tokio::test]
async fn a_checker_whose_keys_no_node_answered_fails_the_decision_as_unavailable() {
    let root = Root::generate().unwrap();
    let node = Arc::new(Node {
        view: Mutex::new(View {
            said_head: 20,
            reached: 20,
        }),
        root_signer: root.signer_address(),
        checker_keys: Mutex::new(CheckerKeys::Unanswered),
    });
    let service = Service::new(a_node(node.clone()).await);
    service.set_google_keys(google_keys());
    service.hold(root);
    service.set_serving(true);

    // The node fails the read of the checker's keys: nothing is decided.
    assert_eq!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .err(),
        Some(Error::ChainUnavailable)
    );

    // A checker that names no keys (the call reverts) adds none: the token's
    // key is trusted by no allowed checker.
    *node.checker_keys.lock().unwrap() = CheckerKeys::Reverts;
    assert_eq!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .err(),
        Some(Error::UntrustedGoogleKey)
    );

    // The checker names the key: released.
    *node.checker_keys.lock().unwrap() = CheckerKeys::Named;
    assert!(
        service
            .exchange_at(&an_executor_asks_for_the_root(), NOW)
            .await
            .is_ok()
    );
}
