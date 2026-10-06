//! A paid RPC URL carries its key, so no log line a service writes names it,
//! at any level `RUST_LOG` asks for. The HTTP transport opens a `debug` span
//! carrying the URL around every request; the services' shared log filter
//! holds the HTTP stack at `info`, so even `trace` leaves it out, and no
//! directive can open that span another way: not one scoped to a span (which
//! enables everything inside the spans it names, the transport's included),
//! nor one whose target is the start of an HTTP crate's name.
//!
//! A subscriber is process-wide, as a service's is, so this file holds one
//! test: a test binary of its own. Needs the `args` feature, as every service
//! has it.

#![cfg(feature = "args")]

mod stand_in;

use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, reload};

const SECRET: &str = "sk-live-0123456789abcdef";

/// Everything the subscriber wrote, for the test to read back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Connect with the key in the URL, and in the URL of the endpoints for log
/// queries, and make an ordinary read and a log query, as the meter, the
/// reporter and the keeper do every few seconds.
async fn read_through(paid: &str, logs_paid: &str) {
    let mut config = stand_in::config(paid);
    config.logs_rpc = Some(logs_paid.to_string());
    let client = chain::Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap();
    client.block_number().await.unwrap();
    client
        .logs(
            &alloy::rpc::types::Filter::new()
                .from_block(1u64)
                .to_block(2u64),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rpc_key_never_reaches_the_log_whatever_level_is_asked_for() {
    let captured = Captured::default();
    let writer = captured.clone();
    let (filter, reloading) = reload::Layer::new(chain::log_filter_from("trace"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .with_filter(filter),
        )
        .init();

    let url = stand_in::start(stand_in::node(31337)).await;
    let paid = format!("{url}/?apikey={SECRET}");
    let logs_url = stand_in::answering(
        Some(31337),
        Arc::new(|method, _| match method {
            "eth_getLogs" => stand_in::Said::Result(serde_json::json!([])),
            // A log read checks the endpoint's head first.
            "eth_blockNumber" => stand_in::Said::Result(serde_json::json!("0x10")),
            _ => stand_in::unknown_method(),
        }),
    )
    .await;
    let logs_paid = format!("{logs_url}/?apikey={SECRET}");

    read_through(&paid, &logs_paid).await;
    tracing::debug!("the reads are done");
    let log = captured.take();
    assert!(
        log.contains("the reads are done"),
        "debug was not on, so the test proves nothing:\n{log}"
    );
    assert!(!log.contains(SECRET), "the RPC key reached the log:\n{log}");

    // Directives that reach the transport's span some other way: through a
    // span of the service's own the read is made in, or a target that is the
    // start of the transport's.
    let mut leaked = Vec::new();
    for spec in [
        "keeper[round]=trace",
        "[round]=trace",
        "alloy[ReqwestTransport]=debug",
        "alloy_transport[ReqwestTransport]=trace",
        "a[{url}]=debug",
        "alloy=trace",
        "alloy_transport=trace",
    ] {
        reloading
            .modify(|filter| *filter = chain::log_filter_from(spec))
            .unwrap();
        captured.take();
        let round = tracing::info_span!(target: "keeper", "round");
        read_through(&paid, &logs_paid).instrument(round).await;
        let log = captured.take();
        if log.contains(SECRET) {
            leaked.push(format!("RUST_LOG={spec}:\n{log}"));
        }
    }
    assert!(
        leaked.is_empty(),
        "the RPC key reached the log:\n{}",
        leaked.join("\n")
    );

    // The same reads with nothing held back do write the key: what the
    // filter holds back is what would have carried it.
    reloading
        .modify(|filter| *filter = EnvFilter::new("trace"))
        .unwrap();
    read_through(&paid, &logs_paid).await;
    let log = captured.take();
    assert!(
        log.contains(SECRET),
        "the transport no longer logs its URL; this test needs another way to see it:\n{log}"
    );
}
