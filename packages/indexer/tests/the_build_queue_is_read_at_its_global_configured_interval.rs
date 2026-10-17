use axum::{Json, Router, routing::get};
use chain::config::ChainConfigs;
use indexer::Indexer;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_build_queue_is_read_at_its_global_configured_interval() {
    for poll in [1_000, 2_000] {
        let attempts = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let recorded = attempts.clone();
        let app = Router::new().route(
            "/jobs",
            get(move || {
                recorded.lock().unwrap().push(Instant::now());
                async { Json(Vec::<shared::model::registry::Job>::new()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let text = include_str!("../../../config/chains.toml").replace(
            "indexer_queue_poll_ms = 2000",
            &format!("indexer_queue_poll_ms = {poll}"),
        );
        let config = ChainConfigs::from_toml(&text, |_| None).unwrap();
        assert_eq!(config.service_rpc_policy().indexer_queue_poll_ms, poll);
        let scratch =
            std::env::temp_dir().join(format!("queue-cadence-{poll}-{}", std::process::id()));
        let indexer = Arc::new(
            Indexer::new(Vec::new(), scratch.clone(), Some(url))
                .with_service_rpc_policy(config.service_rpc_policy().clone())
                .unwrap(),
        );
        let running = indexer.clone();
        let run = tokio::spawn(async move {
            running.run().await;
        });
        let limit = Instant::now() + Duration::from_secs(8);
        while attempts.lock().unwrap().len() < 2 && Instant::now() < limit {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let recorded = attempts.lock().unwrap().clone();
        run.abort();
        server.abort();
        assert!(recorded.len() >= 2, "the queue was not revisited");
        let elapsed = recorded[1].duration_since(recorded[0]);
        assert!(
            elapsed >= Duration::from_millis(poll),
            "the queue was read early: {elapsed:?}"
        );
        let upper = if poll == 1_000 { 1_800 } else { 3_000 };
        assert!(
            elapsed < Duration::from_millis(upper),
            "the configured poll was ignored or delayed: {elapsed:?}"
        );
        assert!(indexer.snapshot.read().await.jobs.is_empty());
        let _ = std::fs::remove_dir_all(scratch);
    }
}

#[test]
fn invalid_global_polls_are_refused_before_the_indexer_runs() {
    for value in [0, 999, 2_001, u64::MAX] {
        let policy = chain::rpc_policy::ServiceRpcPolicy {
            indexer_queue_poll_ms: value,
            ..Default::default()
        };
        let result =
            Indexer::new(Vec::new(), std::env::temp_dir(), None).with_service_rpc_policy(policy);
        let error = result.err().expect("invalid poll accepted").to_string();
        assert!(error.contains("indexer_queue_poll_ms") && error.contains(&value.to_string()));
    }
}
