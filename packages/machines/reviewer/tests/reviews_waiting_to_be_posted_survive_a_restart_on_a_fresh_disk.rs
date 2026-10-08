//! A go-ahead whose review isn't posted yet must outlive the reviewer: the
//! registry takes the build on that go-ahead, and the review is posted once
//! the build is registered. Run on demand, the reviewer starts each time on
//! a fresh disk, so what waits to be posted is kept off the machine, on the
//! API's internal port, sealed under the reviewer's own key, before the
//! go-ahead is given.
//!
//! Here the reviewer keeps its state on a stand-in for that port. It clears
//! strategy 7; the reviewer is stopped, and started again on an empty disk
//! with the same port: it finds the review waiting, and tries to post it.

mod black_box;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};

type Blobs = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

/// A stand-in for the API's internal port: state blobs kept as they are
/// sent, and wake plans taken.
async fn internal_port() -> (String, Blobs) {
    let blobs: Blobs = Default::default();
    let app = axum::Router::new()
        .route(
            "/internal/state/{machine}/{name}",
            get(
                |State(blobs): State<Blobs>, Path((machine, name)): Path<(String, String)>| async move {
                    match blobs.lock().unwrap().get(&format!("{machine}/{name}")) {
                        Some(bytes) => (StatusCode::OK, bytes.clone()),
                        None => (StatusCode::NOT_FOUND, Vec::new()),
                    }
                },
            )
            .put(
                |State(blobs): State<Blobs>,
                 Path((machine, name)): Path<(String, String)>,
                 body: Bytes| async move {
                    blobs
                        .lock()
                        .unwrap()
                        .insert(format!("{machine}/{name}"), body.to_vec());
                    StatusCode::NO_CONTENT
                },
            ),
        )
        .route(
            "/internal/wake/{machine}/plan",
            put(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(blobs.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, blobs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviews_waiting_to_be_posted_survive_a_restart_on_a_fresh_disk() {
    let (url, blobs) = internal_port().await;

    let first = black_box::start(black_box::Options {
        node: true,
        args: vec!["--state-url", &url],
        ..Default::default()
    })
    .await;
    let (status, body) = first.review(7, &black_box::project("kept")).await;
    assert_eq!(status, 200, "{body}\n{}", first.said.lock().unwrap());
    let kept = blobs
        .lock()
        .unwrap()
        .get("reviewer/pending")
        .cloned()
        .expect("the review waiting to be posted was not kept off the machine");
    assert!(
        !String::from_utf8_lossy(&kept).contains("strategy_id"),
        "what is kept off the machine is not sealed"
    );
    drop(first);

    // A fresh disk, the same port.
    let second = black_box::start(black_box::Options {
        node: true,
        args: vec!["--state-url", &url],
        ..Default::default()
    })
    .await;
    assert!(
        second
            .wait_until_said("id=7", Duration::from_secs(30))
            .await,
        "the review waiting to be posted was not found after the restart:\n{}",
        second.said.lock().unwrap()
    );
    assert!(!second.store().join("pending-reviews.json").exists());
}
