//! The reference indexer, on its own: it reads the chain into the record and
//! answers "are you up?". The web app's API runs the same indexer in-process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::State;
use axum::routing::get;
use clap::Parser;
use indexer::Indexer;

#[derive(Parser)]
#[command(about = "Reads the chain into the record")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// Where the build service answers, for the queue. Optional: without it
    /// the record is the chain alone.
    #[arg(long)]
    builder_url: Option<String>,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let served = args.chain_args.connect_all().await?;
    let indexer = Arc::new(Indexer::new(served, args.store, args.builder_url));
    for reader in &indexer.readers {
        tracing::info!(chain = %reader.served.key, "the indexer is reading");
    }

    if args.port != 0 {
        let app = axum::Router::new()
            .route("/health", get(health))
            .with_state(indexer.clone());
        let address = SocketAddr::from(([127, 0, 0, 1], args.port));
        let listener = tokio::net::TcpListener::bind(address).await?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
    }

    indexer.run().await;
    Ok(())
}

/// Up, and how far it has read on each chain.
async fn health(State(indexer): State<Arc<Indexer>>) -> String {
    let snapshot = indexer.snapshot.read().await;
    let blocks: Vec<String> = snapshot
        .chains
        .iter()
        .map(|info| format!("{} block {}", info.chain, info.block))
        .collect();
    format!("indexer: {}", blocks.join(", "))
}
