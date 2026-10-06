//! The reference indexer, on its own: it reads the chain into the record,
//! takes the health reports executors sign, and answers "are you up?". The
//! web app's API runs the same indexer in-process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use clap::Parser;
use indexer::Indexer;

#[derive(Parser)]
#[command(about = "Reads the chain into the record")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// A public copy of the store to read through to for any file not on
    /// disk: fetched from `<url>/sha256-<hash>/<name>`, checked against its
    /// hash, then kept.
    #[arg(long, env = "SHONIN_STORE_URL")]
    store_url: Option<String>,
    /// Where the build service answers, for the queue. Optional: without it
    /// the record is the chain alone.
    #[arg(long)]
    builder_url: Option<String>,
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// The address to listen on with `--port`.
    #[arg(long, default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    // Each chain is read from the moment it connects: one out of reach
    // holds up none of the others.
    let connections = indexer::connections(&args.chain_args)?;
    let mut indexer = Indexer::new(connections.expected.clone(), args.store, args.builder_url);
    if let Some(url) = args.store_url.as_deref().filter(|url| !url.is_empty()) {
        indexer = indexer.with_store_url(url);
    }
    let indexer = Arc::new(indexer);
    indexer.take(connections);

    if args.port != 0 {
        let app = axum::Router::new()
            .route("/health", get(health))
            .route("/api/{chain}/health", post(post_health))
            .with_state(indexer.clone());
        let address = SocketAddr::from((args.listen, args.port));
        let listener = tokio::net::TcpListener::bind(address).await?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
    }

    indexer.run().await;
    Ok(())
}

/// A health report from the instance running a vault's strategy, checked
/// the same way the web app's API checks it.
async fn post_health(
    State(indexer): State<Arc<Indexer>>,
    Path(chain): Path<shared::Chain>,
    Json(post): Json<shared::HealthPost>,
) -> StatusCode {
    indexer.take_health(chain, post).await.status()
}

/// Up, and how far it has read on each chain: 503 while a chain it is to
/// read is not connected, or has not been read lately.
async fn health(State(indexer): State<Arc<Indexer>>) -> (StatusCode, String) {
    indexer.health_answer("indexer")
}
