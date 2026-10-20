//! A build service that takes the connection and never answers (hung, or a
//! proxy holding it) holds a refresh for no longer than a request's timeout:
//! the record of every chain keeps moving, and so does the API's snapshot,
//! queue and every portfolio read from it. One that stops answering leaves
//! the queue it last gave, marked stale and dated as it was.

mod stand_in;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use chain::Served;
use indexer::Indexer;

/// Base, on a node at block 0x10 with no vaults and no logs.
async fn served() -> Served {
    stand_in::served_on(
        stand_in::Chain::at(stand_in::HEAD),
        stand_in::deployments_from(0),
        stand_in::answering_no_vaults(),
    )
    .await
}

/// A build service that takes every connection and never says a word.
async fn a_builder_that_never_answers() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        }
    });
    format!("http://{address}")
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-builder-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A build service that isn't there at all (refused at once)
/// holds nothing up, and base's block is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_is_down_holds_nothing_up() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base],
        scratch(),
        Some("http://127.0.0.1:9".to_string()),
    )
    .with_rpc_policy(stand_in::policy());
    indexer.add(served().await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| info.block);
    assert_eq!(block, Some(16));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_never_answers_does_not_freeze_every_chain_s_record() {
    let builder = a_builder_that_never_answers().await;
    let indexer = Arc::new(
        Indexer::new(vec![shared::Chain::Base], scratch(), Some(builder))
            .with_rpc_policy(stand_in::policy()),
    );
    indexer.add(served().await).await;

    // Well past any request's own timeout.
    let refreshed = tokio::time::timeout(Duration::from_secs(45), indexer.refresh()).await;
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| info.block);
    assert!(
        refreshed.is_ok(),
        "a refresh was still waiting on the build service after 45 s; base's block in the \
         snapshot is {block:?} although its node answers at once"
    );
}

/// A build service that lists one job on base the first time it is asked,
/// and fails every time after.
async fn a_builder_that_answers_once() -> String {
    let job = shared::model::registry::Job {
        id: "8801".to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: "job 8801".to_string(),
        author: "someone".to_string(),
        state: shared::model::registry::JobState::Ready,
        submitted: time::PrimitiveDateTime::new(
            time::Date::from_ordinal_date(2026, 276).unwrap(),
            time::Time::MIDNIGHT,
        ),
        duration_seconds: 0,
        source_hash: "sha256:00".to_string(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    };
    let body = serde_json::to_string(&vec![job]).unwrap();
    let asked = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let app = Router::new().route(
        "/jobs",
        get(move || {
            let body = body.clone();
            let asked = asked.clone();
            async move {
                if asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    (axum::http::StatusCode::OK, body)
                } else {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new())
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_stops_answering_leaves_its_last_queue_marked_stale() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base],
        scratch(),
        Some(a_builder_that_answers_once().await),
    )
    .with_rpc_policy(stand_in::policy());
    indexer.add(served().await).await;
    indexer.refresh().await.unwrap();
    let (jobs, as_of, stale) = {
        let snapshot = indexer.snapshot.read().await;
        (
            snapshot.jobs.len(),
            snapshot.jobs_as_of,
            snapshot.jobs_stale,
        )
    };
    assert_eq!(jobs, 1);
    assert!(as_of.is_some() && !stale);

    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        ["8801"],
        "the queue went blank when the build service failed"
    );
    assert!(snapshot.jobs_stale, "the queue kept is not marked stale");
    assert_eq!(
        snapshot.jobs_as_of, as_of,
        "the queue kept is not dated as it was"
    );
}
