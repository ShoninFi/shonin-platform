//! Fake-node regression: queued jobs take reviews and activation from their cluster.
use axum::{Json, Router, extract::State, routing::post};
use base64::Engine;
use chain::solana::programs::{
    anchor_lang::{AccountDeserialize, AccountSerialize, Discriminator, Space},
    publishing, vault,
};
use chain::solana::{Platform, Pubkey, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind, SolanaDeployments, SolanaServed};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::{
    Chain,
    model::registry::{Job, JobState},
};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Node {
    accounts: Mutex<std::collections::HashMap<String, Value>>,
    calls: AtomicUsize,
    delay_ms: AtomicU64,
}
fn zeroed<T: AccountDeserialize + Discriminator>(len: usize) -> T {
    let mut data = vec![0; 8 + len];
    data[..8].copy_from_slice(T::DISCRIMINATOR);
    T::try_deserialize(&mut &data[..]).unwrap()
}
fn account<T: AccountSerialize>(value: &T, owner: Pubkey) -> Value {
    let mut data = vec![];
    value.try_serialize(&mut data).unwrap();
    json!({"lamports":1000000,"owner":owner.to_string(),"executable":false,
        "data":[base64::engine::general_purpose::STANDARD.encode(data),"base64"],"rentEpoch":0})
}
async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Json<Value> {
    let result = match request["method"].as_str().unwrap() {
        "getGenesisHash" => json!("stand-in-genesis"),
        "getAccountInfo" => {
            node.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(
                node.delay_ms.load(Ordering::SeqCst),
            ))
            .await;
            let value = node
                .accounts
                .lock()
                .unwrap()
                .get(request["params"][0].as_str().unwrap())
                .cloned()
                .unwrap_or(Value::Null);
            json!({"context":{"slot":42},"value":value})
        }
        method => panic!("unexpected {method}"),
    };
    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
}
async fn setup() -> (Indexer, Arc<Node>) {
    let node = Arc::new(Node::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().route("/", post(rpc)).with_state(node.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config = ChainConfig {
        label: "stand-in".into(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: url.clone(),
        logs_rpc: None,
        log_range: None,
    };
    let key = ChainKey::new(ChainKind::Solana, "localnet");
    let client = SolanaClient::connect_with(&key, &config, None)
        .await
        .unwrap();
    let deployments = SolanaDeployments::from_json("{\"programs\":{}}").unwrap();
    let platform = Platform {
        cluster_name: "localnet".into(),
        cluster: client.cluster_value(),
        mint: Pubkey::new_unique(),
        admin: Pubkey::new_unique(),
        platform: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        checker: None,
    };
    let indexer = Indexer::new(
        vec![Chain::Solana],
        std::env::temp_dir().join(format!("queue-fix-{}", std::process::id())),
        None,
    );
    indexer
        .add_solana(SolanaServed {
            vault_chain: Chain::Solana,
            key,
            client,
            deployments,
            platform,
            public_rpc: url,
        })
        .await;
    (indexer, node)
}
fn job(id: u64, state: JobState) -> Job {
    Job {
        id: format!("job-{id}"),
        chain: Chain::Solana,
        strategy_id: Some(id),
        title: "test".into(),
        author: Pubkey::new_unique().to_string(),
        state,
        submitted: indexer::read::moment(1_800_000_000),
        duration_seconds: 0,
        source_hash: String::new(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    }
}
fn post_review(node: &Node, id: u64, refused: bool) {
    let mut review: publishing::state::Review = zeroed(publishing::state::Review::INIT_SPACE);
    review.strategy_id = id;
    review.reviewed_at = 1_800_000_000;
    review.refused = refused;
    node.accounts.lock().unwrap().insert(
        chain::solana::platform::review_address(id).to_string(),
        account(&review, publishing::ID),
    );
}
#[tokio::test]
async fn posted_reviews_including_refusals_complete_jobs_and_activation_is_cached() {
    let (indexer, node) = setup().await;
    let ready = job(7, JobState::Ready);
    let missing = indexer.complete(ready.clone()).await;
    assert_eq!(missing.state, JobState::Ready);
    assert!(missing.review.is_none());
    post_review(&node, 7, false);
    let reviewed = indexer.complete(ready.clone()).await;
    assert!(reviewed.review.is_some());
    assert_eq!(reviewed.state, JobState::Ready);
    assert_eq!(
        reviewed.review.unwrap().summary,
        "The report is not in the store."
    );
    let mut activated: vault::state::Vault = zeroed(vault::state::Vault::INIT_SPACE);
    activated.strategy_id = 7;
    node.accounts.lock().unwrap().insert(
        vault::state::vault_address(7).0.to_string(),
        account(&activated, vault::ID),
    );
    let completed = indexer.complete(ready.clone()).await;
    assert_eq!(completed.state, JobState::Activated);
    assert_eq!(completed.strategy_slug.as_deref(), Some("solana-7"));
    let calls = node.calls.load(Ordering::SeqCst);
    node.accounts.lock().unwrap().clear();
    assert_eq!(indexer.complete(ready).await.state, JobState::Activated);
    assert_eq!(
        node.calls.load(Ordering::SeqCst),
        calls,
        "settled job must not read again"
    );
    post_review(&node, 8, true);
    let refused = indexer.complete(job(8, JobState::Refused)).await;
    assert_eq!(refused.state, JobState::Refused);
    assert!(
        refused.review.unwrap().refused,
        "refusal exists without registered strategy or vault"
    );
    let unknown = indexer.complete(job(9, JobState::Ready)).await;
    assert!(
        unknown.review.is_none(),
        "another strategy's review cannot complete this job"
    );
    assert!(unknown.strategy_slug.is_none());
}

#[tokio::test]
async fn unposted_and_slow_reviews_are_retried_without_becoming_settled() {
    let (indexer, node) = setup().await;
    let refused = job(10, JobState::Refused);
    let mut unposted: publishing::state::Review = zeroed(publishing::state::Review::INIT_SPACE);
    unposted.strategy_id = 10;
    node.accounts.lock().unwrap().insert(
        chain::solana::platform::review_address(10).to_string(),
        account(&unposted, publishing::ID),
    );
    assert!(
        indexer.complete(refused.clone()).await.review.is_none(),
        "an unposted account cannot stop refusal retries"
    );
    post_review(&node, 10, true);
    let retry = indexer.complete(refused).await;
    assert!(retry.review.unwrap().refused);
    post_review(&node, 11, false);
    node.delay_ms.store(10_000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let timed_out = indexer.complete(job(11, JobState::Ready)).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "single job retains its 3s read bound"
    );
    assert!(timed_out.review.is_none());
    node.delay_ms.store(0, Ordering::SeqCst);
    assert!(
        indexer
            .complete(job(11, JobState::Ready))
            .await
            .review
            .is_some(),
        "timed-out completion retries"
    );
}
