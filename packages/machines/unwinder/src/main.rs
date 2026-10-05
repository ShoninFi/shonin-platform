//! The attested unwinder.
//!
//! A separate, smaller image with no agent in it. It finishes the withdrawals,
//! kills and retirements the contracts can't finish by themselves: it closes
//! a vault's positions with reduce-only orders and sends the money only to the
//! vault's own addresses.
//!
//! The local chain's venue settles inside the contracts, so there is nothing
//! for it to finish yet. It connects, waits for the deployment, and answers
//! "are you up?" beside the other machines.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Finishes the withdrawals the contracts can't finish alone")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    /// Where bundles are, by hash.
    #[arg(long)]
    store: PathBuf,
    /// Where the health reports go.
    #[arg(long)]
    api_url: String,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    let served = args.chain_args.connect_all().await?;
    tracing::info!(image = %chain::attest::image_digest("unwinder"), "the unwinder is up");
    for served in &served {
        tracing::info!(
            chain = %served.key,
            "no venue on this chain needs unwinding outside the contracts; nothing to serve yet"
        );
    }

    if args.port != 0 {
        serve_status(SocketAddr::from(([127, 0, 0, 1], args.port))).await;
    } else {
        std::future::pending::<()>().await;
    }
    Ok(())
}

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "unwinder" }));
    match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => {
            let _ = axum::serve(listener, app).await;
        }
        Err(error) => tracing::warn!(%error, "no status endpoint"),
    }
}
