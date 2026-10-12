mod stand_in;

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::{SolCall, SolValue};
use axum::{Json, Router, routing::post};
use chain::bindings::{
    factory::IVaultFactory,
    strategies::{IStrategyRegistry, Strategy},
    vault::IVault,
};
use chain::{ChainConfig, Served};
use indexer::Indexer;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    head: u64,
    branch: u8,
    registration_reads: u64,
    state_reads: u64,
    registration_after: u64,
    vaults: u8,
    bad_author: bool,
    bad_bundle: bool,
}
fn vault(id: u8) -> Address {
    Address::repeat_byte(id)
}
fn author() -> Address {
    Address::repeat_byte(0x44)
}
fn bundle() -> B256 {
    B256::repeat_byte(0x33)
}
fn hash(branch: u8, number: u64) -> B256 {
    let mut bytes = [if number < 3 { 1 } else { branch + 1 }; 32];
    bytes[24..].copy_from_slice(&number.to_be_bytes());
    B256::from(bytes)
}
fn header(branch: u8, number: u64) -> Value {
    let mut b = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
    b.header.inner.number = number;
    b.header.inner.timestamp = 1_700_000_000 + number;
    b.header.hash = hash(branch, number);
    b.header.inner.parent_hash = if number == 0 {
        B256::ZERO
    } else {
        hash(branch, number - 1)
    };
    serde_json::to_value(b).unwrap()
}
fn encoded<C: SolCall>(value: C::Return) -> Value {
    json!(format!(
        "0x{}",
        alloy::hex::encode(C::abi_encode_returns(&value))
    ))
}
async fn reader(
    name: &str,
    state: Arc<Mutex<State>>,
    limits: &[(&str, u64)],
) -> (Indexer, chain::Client) {
    let served = state.clone();
    let app = Router::new().route("/", post(move |Json(request): Json<Value>| {
        let served = served.clone();
        async move {
            let mut s = served.lock().unwrap();
            let method = request["method"].as_str().unwrap();
            let id = request["id"].clone();
            let result = match method {
                "eth_chainId" => json!("0x7a69"),
                "eth_blockNumber" => json!(format!("{:#x}", s.head)),
                "eth_getBlockByNumber" => {
                    let tag = request["params"][0].as_str().unwrap();
                    let n = if matches!(tag,"latest"|"pending") {s.head} else {u64::from_str_radix(tag.trim_start_matches("0x"),16).unwrap()};
                    header(s.branch,n)
                },
                "eth_getBlockByHash" => {
                    let h: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                    let n = u64::from_be_bytes(h.as_slice()[24..].try_into().unwrap());
                    let branch = h.as_slice()[0].saturating_sub(1);
                    if h == hash(branch,n) {header(branch,n)} else {Value::Null}
                },
                "eth_call" => {
                    let read_number = if let Some(tag) = request["params"][1]["blockHash"].as_str() {
                        let h: B256 = tag.parse().unwrap();
                        let n = u64::from_be_bytes(h.as_slice()[24..].try_into().unwrap());
                        if h != hash(s.branch,n) {
                            return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"block is not canonical"}}));
                        }
                        n
                    } else {
                        s.head
                    };
                    let input = request["params"][0]["input"].as_str().or_else(||request["params"][0]["data"].as_str()).unwrap();
                    let raw = alloy::hex::decode(input).unwrap();
                    if raw.starts_with(&IVaultFactory::vaultsCall::SELECTOR) {
                        encoded::<IVaultFactory::vaultsCall>((7..7+s.vaults).map(vault).collect())
                    } else if raw.starts_with(&IVault::strategyIdCall::SELECTOR) {
                        let at: Address = request["params"][0]["to"].as_str().unwrap().parse().unwrap();
                        encoded::<IVault::strategyIdCall>(U256::from(at.as_slice()[0]))
                    } else if raw.starts_with(&IVault::authorCall::SELECTOR) {
                        encoded::<IVault::authorCall>(if s.bad_author {Address::repeat_byte(0x55)} else {author()})
                    } else if raw.starts_with(&IVault::bundleHashCall::SELECTOR) {
                        encoded::<IVault::bundleHashCall>(if s.bad_bundle {B256::repeat_byte(0x66)} else {bundle()})
                    } else if raw.starts_with(&IStrategyRegistry::strategyOfCall::SELECTOR) {
                        s.registration_reads += 1;
                        let ready = read_number >= s.registration_after;
                        encoded::<IStrategyRegistry::strategyOfCall>(Strategy {
                            author: if ready {author()} else {Address::ZERO},
                            sourceHash: if ready {B256::repeat_byte(if s.branch==0 {0x11} else {0x22})} else {B256::ZERO},
                            bundleHash: if ready {bundle()} else {B256::ZERO},
                            manifestHash:B256::ZERO,
                            registeredAt:if ready {1_700_000_003} else {0},
                            chains:Vec::new(),bridges:Vec::new(),restricted:Vec::new(),
                        })
                    } else {
                        if raw.starts_with(&IVault::stateCall::SELECTOR) {s.state_reads += 1;}
                        json!(format!("0x{}","0".repeat(64*128)))
                    }
                },
                "eth_getCode" => json!("0x01"),
                "eth_getBalance"|"eth_getTransactionCount" => json!("0x0"),
                "eth_getLogs" => json!([]),
                _ => Value::Null,
            };
            Json(json!({"jsonrpc":"2.0","id":id,"result":result}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: "record fixture".into(),
        rpc: url.clone(),
        public_rpc: url.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = stand_in::client_with_limits(&config, limits).await;
    let dir = std::env::temp_dir().join(format!("registered-record-{name}-{}", std::process::id()));
    let indexer = Indexer::new(vec![shared::Chain::Base], dir, None);
    indexer
        .add(Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse().unwrap(),
            client: client.clone(),
            deployments: stand_in::deployments(),
            native_usd: None,
            public_rpc: url,
        })
        .await;
    (indexer, client)
}
async fn refresh(indexer: &Indexer, expected_source: u8, expected_vaults: usize) {
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(snapshot.strategies.len(), expected_vaults);
    assert!(snapshot.strategies.iter().all(|s| s.stale_as_of.is_none()));
    for strategy in &snapshot.strategies {
        assert_eq!(
            strategy.bundle.source_hash,
            format!("sha256:{:x}", B256::repeat_byte(expected_source))
        );
    }
}
#[tokio::test]
async fn completed_registration_is_cached_but_current_vault_state_is_read_again() {
    let state = Arc::new(Mutex::new(State {
        head: 4,
        vaults: 1,
        ..Default::default()
    }));
    let (indexer, _) = reader("forward", state.clone(), &[]).await;
    refresh(&indexer, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 1);
    let old = state.lock().unwrap().state_reads;
    state.lock().unwrap().head = 5;
    refresh(&indexer, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 1);
    assert!(state.lock().unwrap().state_reads > old);
    state.lock().unwrap().branch = 1;
    refresh(&indexer, 0x22, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 2);
}
#[tokio::test]
async fn an_absent_registration_is_read_again_when_the_head_advances() {
    let state = Arc::new(Mutex::new(State {
        head: 4,
        registration_after: 5,
        vaults: 1,
        ..Default::default()
    }));
    let (indexer, _) = reader("absent", state.clone(), &[]).await;
    refresh(&indexer, 0, 1).await;
    state.lock().unwrap().head = 5;
    refresh(&indexer, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 2);
    state.lock().unwrap().head = 6;
    refresh(&indexer, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 2);
}
#[tokio::test]
async fn disabled_and_too_small_caches_keep_the_ordinary_registration_reads() {
    for (name, limits) in [
        ("entries", vec![("immutable_cache_entries", 0)]),
        ("bytes", vec![("immutable_cache_bytes", 0)]),
        ("small", vec![("immutable_cache_bytes", 1)]),
    ] {
        let state = Arc::new(Mutex::new(State {
            head: 4,
            vaults: 1,
            ..Default::default()
        }));
        let (indexer, _) = reader(name, state.clone(), &limits).await;
        refresh(&indexer, 0x11, 1).await;
        state.lock().unwrap().head = 5;
        refresh(&indexer, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, 2);
    }
}
#[tokio::test]
async fn cache_entry_bound_preserves_reads_for_another_strategy() {
    let state = Arc::new(Mutex::new(State {
        head: 4,
        vaults: 2,
        ..Default::default()
    }));
    let (indexer, _) = reader("capacity", state.clone(), &[("immutable_cache_entries", 1)]).await;
    refresh(&indexer, 0x11, 2).await;
    assert_eq!(state.lock().unwrap().registration_reads, 2);
    state.lock().unwrap().head = 5;
    refresh(&indexer, 0x11, 2).await;
    assert_eq!(state.lock().unwrap().registration_reads, 3);
}

fn completed_record() -> Strategy {
    Strategy {
        author: author(),
        sourceHash: B256::repeat_byte(0x11),
        bundleHash: bundle(),
        manifestHash: B256::ZERO,
        registeredAt: 1_700_000_003,
        chains: Vec::new(),
        bridges: Vec::new(),
        restricted: Vec::new(),
    }
}
#[tokio::test]
async fn exact_encoded_byte_limit_includes_one_whole_record_only() {
    let bytes = completed_record().abi_encoded_size() as u64;
    assert_eq!(bytes, 384);
    for (name, cap, want) in [
        ("bytes-below", bytes - 1, 2),
        ("bytes-exact", bytes, 1),
        ("bytes-above", bytes + 1, 1),
    ] {
        let state = Arc::new(Mutex::new(State {
            head: 4,
            vaults: 1,
            ..Default::default()
        }));
        let (idx, _) = reader(name, state.clone(), &[("immutable_cache_bytes", cap)]).await;
        refresh(&idx, 0x11, 1).await;
        state.lock().unwrap().head = 5;
        refresh(&idx, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, want, "cap={cap}");
    }
    let state = Arc::new(Mutex::new(State {
        head: 4,
        vaults: 2,
        ..Default::default()
    }));
    let (idx, _) = reader(
        "bytes-two",
        state.clone(),
        &[("immutable_cache_bytes", bytes)],
    )
    .await;
    refresh(&idx, 0x11, 2).await;
    state.lock().unwrap().head = 5;
    refresh(&idx, 0x11, 2).await;
    assert_eq!(state.lock().unwrap().registration_reads, 3);
}
#[tokio::test]
async fn rollback_before_registration_removes_the_positive_cache() {
    let state = Arc::new(Mutex::new(State {
        head: 5,
        vaults: 1,
        registration_after: 5,
        ..Default::default()
    }));
    let (idx, _) = reader("rollback", state.clone(), &[]).await;
    refresh(&idx, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 1);
    state.lock().unwrap().head = 4;
    refresh(&idx, 0, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 2);
    state.lock().unwrap().head = 5;
    refresh(&idx, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 3);
    state.lock().unwrap().head = 6;
    refresh(&idx, 0x11, 1).await;
    assert_eq!(state.lock().unwrap().registration_reads, 3);
}
#[tokio::test]
async fn fresh_identity_mismatch_evicts_even_when_branch_has_not_changed() {
    for which in ["author", "bundle"] {
        let state = Arc::new(Mutex::new(State {
            head: 4,
            vaults: 1,
            ..Default::default()
        }));
        let (idx, _) = reader(which, state.clone(), &[]).await;
        refresh(&idx, 0x11, 1).await;
        {
            let mut s = state.lock().unwrap();
            s.head = 5;
            if which == "author" {
                s.bad_author = true;
            } else {
                s.bad_bundle = true;
            }
        }
        refresh(&idx, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, 2);
        state.lock().unwrap().head = 6;
        refresh(&idx, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, 3);
        {
            let mut s = state.lock().unwrap();
            s.head = 7;
            s.bad_author = false;
            s.bad_bundle = false;
        }
        refresh(&idx, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, 4);
        state.lock().unwrap().head = 8;
        refresh(&idx, 0x11, 1).await;
        assert_eq!(state.lock().unwrap().registration_reads, 4);
    }
}
#[test]
fn policy_entry_and_byte_extremes_are_either_sound_or_refused_by_name() {
    for entries in [0, 1, 1024, 65535, 65536] {
        for bytes in [0, 1, 384, 1048576, 67108863, 67108864] {
            let p = chain::rpc_policy::RpcPolicy {
                immutable_cache_entries: entries,
                immutable_cache_bytes: bytes,
                ..Default::default()
            };
            p.validate("registrations").unwrap();
        }
    }
    for (field, entries, bytes) in [
        ("immutable_cache_entries", 65537, 1048576),
        ("immutable_cache_entries", u64::MAX, 1048576),
        ("immutable_cache_bytes", 1024, 67108865),
        ("immutable_cache_bytes", 1024, u64::MAX),
    ] {
        let p = chain::rpc_policy::RpcPolicy {
            immutable_cache_entries: entries,
            immutable_cache_bytes: bytes,
            ..Default::default()
        };
        assert!(
            p.validate("registrations")
                .unwrap_err()
                .to_string()
                .contains(field)
        );
    }
}
#[tokio::test]
async fn ordinary_and_legacy_scopes_do_not_claim_a_bound_record_observation() {
    let state = Arc::new(Mutex::new(State {
        head: 4,
        vaults: 1,
        ..Default::default()
    }));
    let (_, client) = reader("unbound", state, &[]).await;
    assert_eq!(client.record_observation(), None);
    assert_eq!(
        client.verifying_logs(4, hash(0, 4)).record_observation(),
        None
    );
    let head = client.head_details().await.unwrap();
    let branch = Arc::new(tokio::sync::Mutex::new(
        chain::branch::BranchProofs::default(),
    ));
    branch
        .lock()
        .await
        .select(&client, head.clone())
        .await
        .unwrap();
    assert_eq!(
        client
            .verifying_branch(&head, branch, vec![])
            .record_observation(),
        Some((4, hash(0, 4)))
    );
}

#[tokio::test]
async fn unbound_public_record_reads_do_not_retain_registry_results() {
    for legacy in [false, true] {
        let state = Arc::new(Mutex::new(State {
            head: 4,
            vaults: 1,
            ..Default::default()
        }));
        let (_, client) = reader(
            if legacy {
                "public-legacy"
            } else {
                "public-plain"
            },
            state.clone(),
            &[],
        )
        .await;
        let client = if legacy {
            client.verifying_logs(4, hash(0, 4))
        } else {
            client
        };
        let mut history = indexer::read::History::default();
        let store = indexer::store::Store::new(
            std::env::temp_dir().join(format!("unbound-record-{}", std::process::id())),
        );
        for _ in 0..2 {
            let records = indexer::read::strategies(
                &client,
                shared::Chain::Base,
                &stand_in::deployments(),
                &store,
                &mut history,
                &Default::default(),
                4,
                1_700_000_004,
            )
            .await
            .unwrap();
            assert_eq!(records.len(), 1);
            assert!(records[0].stale_as_of.is_none());
            assert_eq!(
                records[0].bundle.source_hash,
                format!("sha256:{:x}", B256::repeat_byte(0x11))
            );
        }
        assert_eq!(state.lock().unwrap().registration_reads, 2);
    }
}
