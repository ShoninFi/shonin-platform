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
//!
//! Given `--attest-once` in the attested image, it only shows that the
//! image attests: it asks the launcher for one token, binding a key made for
//! the purpose and no vault, checks it as the key exchange checks one
//! (Google's signature, by a key the canonical chain's allowed checkers
//! trust, and the nonce), says the image digest and project it names, and
//! exits.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Finishes the withdrawals the contracts can't finish alone")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    /// Where bundles are, by hash.
    #[arg(long, required_unless_present = "attest_once")]
    store: Option<PathBuf>,
    /// Where the health reports go.
    #[arg(long, required_unless_present = "attest_once")]
    api_url: Option<String>,
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// How the image is attested: `development`, or `launcher` (Confidential
    /// Space's launcher).
    #[arg(long, env = "SHONIN_ATTESTATION", value_enum, default_value_t = Attestation::Development)]
    attestation: Attestation,
    /// Check that the image attests, once, and exit (launcher mode only).
    #[arg(long)]
    attest_once: bool,
    /// The launcher's token socket.
    #[arg(long, hide = true, default_value = shonin_rootkey::launcher::LAUNCHER_SOCKET)]
    launcher_socket: PathBuf,
}

/// How the image is attested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Attestation {
    Development,
    Launcher,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    if args.attest_once {
        if args.attestation != Attestation::Launcher {
            bail!("--attest-once checks the launcher's token: it needs --attestation launcher");
        }
        return attest_once(&args).await;
    }
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

/// Ask the launcher for a token binding a fresh key and an empty tree, check
/// it, and say what it names.
async fn attest_once(args: &Args) -> Result<()> {
    use shonin_rootkey::ChainView as _;

    let root = args
        .chain_args
        .root_chain()?
        .context("the network names no canonical chain to read the checkers from")?;
    let view = shonin_rootkey::RegistryView::connect(
        &args.chain_args.chains,
        &root,
        &args.chain_args.deployments_dir,
    )
    .await?;

    // A key made for this one token, serving no vault: its tree is empty.
    let instance = alloy::signers::local::PrivateKeySigner::random();
    let binding = shonin_attestation_core::Binding {
        evm_signer: instance.address().into_array(),
        solana_signer: [0; 32],
        vaults_root: chain::attest::merkle_root(&[]).0,
    };
    let nonce = binding.nonce();
    let token = shonin_rootkey::launcher::request_token(&args.launcher_socket, &nonce).await?;

    let http = reqwest::Client::new();
    let google_keys = shonin_rootkey::launcher::fetch_google_keys(&http).await?;
    let at = view.pin(view.chain_id()).await?;
    let trusted = view.trusted_google_keys(at).await?;
    // The launcher's clock may be a little ahead of ours.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 5;
    let attested =
        shonin_rootkey::exchange::check_token(&token, &google_keys, &trusted, now, &nonce)
            .map_err(|error| anyhow::anyhow!("the launcher's token does not check: {error}"))?;
    tracing::info!(
        image_digest = %attested.image_digest,
        project = project_named(&token).unwrap_or_default(),
        project_id = %attested.project_id,
        key_hash = %alloy::hex::encode(attested.key_hash),
        result = "ok",
        "the image attests"
    );
    Ok(())
}

/// The project a token names, read without checking it again: it has been.
fn project_named(token: &str) -> Option<String> {
    use base64::Engine as _;

    let claims = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .pointer("/submods/gce/project_id")?
        .as_str()
        .map(str::to_string)
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
