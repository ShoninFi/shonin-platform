//! A review is posted on the chain by its hashes: the source read and the
//! report written (`reportHash`). The report itself is what a strategy's page
//! shows, and anyone checking the review needs the bytes that hash to it.
//!
//! On a laptop the reviewer writes `report.json` into its store on disk, and
//! the store is mirrored to the public one by hand. In the cloud the
//! reviewer runs on a machine that starts with a fresh disk and powers off
//! when idle, and its state is kept on the API's internal port instead: the
//! report is put into the API's store
//! (`PUT /internal/store/{hash}/report.json`) before the reviewer answers
//! with its hash or anything names it on the chain, so the chain never names
//! bytes the platform doesn't have. A report the store would not take is no
//! answer (ask again later), never a go-ahead.
//!
//! Here the reviewer keeps its state on a stand-in of the API's internal
//! port (`--state-url`), reads a project and clears it; then the same with a
//! store that takes nothing.

mod black_box;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};

#[derive(Default)]
struct Api {
    blobs: Mutex<BTreeMap<String, Vec<u8>>>,
    store: Mutex<BTreeMap<String, Vec<u8>>>,
    /// The store takes nothing, as one that is down.
    store_down: AtomicBool,
}

/// A stand-in internal port: it keeps every blob and every file it is
/// given, and asks nothing of a signature.
async fn any(State(api): State<Arc<Api>>, method: Method, uri: Uri, body: Bytes) -> StatusCode {
    let path = uri.path().to_string();
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["internal", "state", machine, name]) => {
            match api.blobs.lock().unwrap().get(&format!("{machine}/{name}")) {
                Some(_) => StatusCode::OK,
                None => StatusCode::NOT_FOUND,
            }
        }
        ("PUT", ["internal", "state", machine, name]) => {
            api.blobs
                .lock()
                .unwrap()
                .insert(format!("{machine}/{name}"), body.to_vec());
            StatusCode::NO_CONTENT
        }
        ("DELETE", ["internal", "state", _, _]) => StatusCode::NO_CONTENT,
        ("PUT", ["internal", "wake", _, "plan"]) | ("POST", ["internal", "wake", _]) => {
            StatusCode::NO_CONTENT
        }
        ("PUT", ["internal", "store", hash, name]) => {
            if api.store_down.load(Ordering::SeqCst) {
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
            api.store
                .lock()
                .unwrap()
                .insert(format!("{hash}/{name}"), body.to_vec());
            StatusCode::CREATED
        }
        _ => StatusCode::NOT_FOUND,
    }
}

async fn serve(store_down: bool) -> (String, Arc<Api>) {
    let api = Arc::new(Api::default());
    api.store_down.store(store_down, Ordering::SeqCst);
    let app = axum::Router::new().fallback(any).with_state(api.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, api)
}

/// A model that clears everything it reads, every battery question answered.
fn a_model_that_clears() -> std::path::PathBuf {
    let answers: Vec<serde_json::Value> = shonin_reviewer::battery::Battery::pinned()
        .questions
        .iter()
        .map(|question| {
            serde_json::json!({
                "id": question.id,
                "probability": if question.good { 0.95 } else { 0.05 },
                "confidence": 0.9,
            })
        })
        .collect();
    let reply = serde_json::json!({ "structured_output": {
        "summary": "A plain agent.", "findings": [], "limits": [], "answers": answers,
    }})
    .to_string();
    assert!(!reply.contains('\''));
    black_box::a_claude(&format!("cat > /dev/null\nprintf '%s' '{reply}'"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_is_in_the_api_s_store_before_its_hash_leaves_the_machine() {
    let (url, api) = serve(false).await;
    let bin = a_model_that_clears();
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        args: vec!["--state-url", &url],
        ..Default::default()
    })
    .await;
    let (status, body) = reviewer.review(7, &black_box::project("report")).await;
    assert_eq!(status, 200, "{body}\n{}", reviewer.said.lock().unwrap());
    let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
    let report_hash = answer["report_hash"]
        .as_str()
        .expect("the answer names the report's hash")
        .replace(':', "-");
    assert!(
        api.blobs.lock().unwrap().contains_key("reviewer/pending"),
        "the reviewer keeps no state on the internal port, so the test proves nothing"
    );

    // In the store by the time the hash is answered with.
    let wanted = format!("{report_hash}/report.json");
    let kept = api.store.lock().unwrap().get(&wanted).cloned();
    let Some(kept) = kept else {
        panic!(
            "the reviewer answered with {wanted}, which the chain will name, but the report \
             is not in the API's store, where the platform keeps what the chain names; the \
             reviewer's own disk is gone when it powers off. Store holds: {:?}\n{}",
            api.store.lock().unwrap().keys().collect::<Vec<_>>(),
            reviewer.said.lock().unwrap()
        );
    };
    let hashed = shared::manifest::hash_bytes(&kept).replace(':', "-");
    assert_eq!(
        hashed, report_hash,
        "the report kept is not the one answered with"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_the_api_s_store_does_not_take_is_no_clearance() {
    let (url, api) = serve(true).await;
    let bin = a_model_that_clears();
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        args: vec!["--state-url", &url],
        ..Default::default()
    })
    .await;
    // A reviewer that may keep its state writes down the reviews waiting,
    // none yet, once at start: what is there before the review must be
    // what is there after it.
    let started = std::time::Instant::now();
    let before = loop {
        if let Some(blob) = api.blobs.lock().unwrap().get("reviewer/pending").cloned() {
            break Some(blob);
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            break None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let (status, body) = reviewer.review(7, &black_box::project("store-down")).await;
    assert_eq!(
        status,
        503,
        "a report the store did not take was answered all the same: {body}\n{}",
        reviewer.said.lock().unwrap()
    );
    assert!(
        !body.contains("clearance"),
        "a go-ahead was given for a report the store did not take: {body}"
    );
    assert_eq!(
        api.blobs.lock().unwrap().get("reviewer/pending").cloned(),
        before,
        "a review waits to be posted whose report the store did not take"
    );
}
