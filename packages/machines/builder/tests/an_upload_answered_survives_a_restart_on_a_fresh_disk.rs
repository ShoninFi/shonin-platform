//! An upload that was answered must outlive the build service: the author
//! pays against the reference in the answer, and a payment for an upload the
//! service no longer holds is a payment lost. Run on demand, the build
//! service starts each time on a fresh disk, so the upload's archive and the
//! queue naming it are kept off the machine, on the API's internal port,
//! sealed under the service's own key, before the upload is answered.
//!
//! Here the build service keeps its state on a stand-in for that port. An
//! upload is answered; the service is stopped, and started again on an
//! empty disk with the same port: the upload is still there, waiting for
//! its payment, with its archive.

mod black_box;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use shared::model::registry::JobState;

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
            )
            .delete(
                |State(blobs): State<Blobs>, Path((machine, name)): Path<(String, String)>| async move {
                    match blobs.lock().unwrap().remove(&format!("{machine}/{name}")) {
                        Some(_) => StatusCode::NO_CONTENT,
                        None => StatusCode::NOT_FOUND,
                    }
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

/// The builder, keeping its state at `url`, open and taking uploads.
async fn started(url: &str) -> black_box::Builder {
    let builder =
        black_box::spawn_with(black_box::node(0x10, None), &[], &["--state-url", url]).await;
    let begun = Instant::now();
    while !builder
        .said
        .lock()
        .unwrap()
        .contains("taking uploads and building jobs on this chain")
    {
        assert!(
            begun.elapsed() < Duration::from_secs(60),
            "the builder never opened:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_answered_survives_a_restart_on_a_fresh_disk() {
    let (url, blobs) = internal_port().await;
    let author = Address::repeat_byte(0xa1);
    let archive = black_box::project("kept");

    let first = started(&url).await;
    let (status, body) = first.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}\n{}", first.said.lock().unwrap());
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    {
        let blobs = blobs.lock().unwrap();
        let queue = blobs
            .get("builder/queue")
            .expect("the queue was not kept off the machine before the upload was answered");
        assert!(
            !String::from_utf8_lossy(queue).contains(&accepted.job_id),
            "what is kept off the machine is not sealed"
        );
        assert!(
            blobs.contains_key(&format!("builder/archive-{}", accepted.job_id)),
            "the upload's archive was not kept off the machine before it was answered"
        );
    }
    drop(first);

    // A fresh disk, the same port.
    let second = started(&url).await;
    let job = second
        .job(&accepted.job_id)
        .await
        .expect("the upload answered before the restart is gone");
    assert_eq!(job.state, JobState::AwaitingPayment);
    let brought = second
        .store()
        .join("archives")
        .join(format!("{}.tar", accepted.job_id));
    assert_eq!(
        std::fs::read(&brought).ok(),
        Some(archive),
        "the upload's archive was not brought back"
    );
}
