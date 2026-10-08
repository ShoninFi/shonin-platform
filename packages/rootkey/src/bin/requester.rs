//! `shonin-rootkey-requester`: a test enclave that asks a holder for a
//! purpose's key by attested exchange, and proves it got it by opening a
//! message sealed to that purpose.
//!
//! It runs inside Confidential Space with its settings baked into its image
//! (the holder's URL, the purpose, the sealed file), asks the launcher for a
//! token whose nonce binds a fresh ephemeral key and the purpose, posts the
//! request to the holder's `/rootkey/exchange`, opens the release and then
//! the sealed file. It logs only whether each step worked and `sha256` of
//! the plaintext, never the plaintext or a key; then it idles, so a launcher
//! that restarts stopped containers doesn't ask again and again.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use sha2::{Digest, Sha256};
use shonin_rootkey::launcher::LAUNCHER_SOCKET;
use shonin_rootkey::service::exchange_once;
use shonin_rootkey::{Purpose, Received, Sealed, open};

#[derive(Parser)]
struct Args {
    /// The holder's base URL (its status port).
    #[arg(long, env = "SHONIN_ROOTKEY_HOLDER")]
    holder: String,
    /// The purpose to ask for (`platform:dummy`).
    #[arg(long, env = "SHONIN_ROOTKEY_PURPOSE")]
    purpose: String,
    /// The message sealed to that purpose, baked into the image.
    #[arg(long, env = "SHONIN_ROOTKEY_SEALED")]
    sealed: PathBuf,
    /// The associated data it was sealed with.
    #[arg(long, env = "SHONIN_ROOTKEY_AAD", default_value = "")]
    aad: String,
    /// The launcher's token socket.
    #[arg(long, default_value = LAUNCHER_SOCKET)]
    socket: PathBuf,
    /// How many times to try before giving up.
    #[arg(long, default_value_t = 10)]
    attempts: u32,
}

async fn attempt(args: &Args, purpose: &Purpose, sealed: &Sealed) -> Result<String> {
    let received =
        exchange_once(&reqwest::Client::new(), &args.holder, &args.socket, purpose).await?;
    let Received::Key(pair) = received else {
        bail!("the holder released something other than a key");
    };
    tracing::info!("opened the release");
    let plaintext = open(&pair, sealed, args.aad.as_bytes())?;
    Ok(hex::encode(Sha256::digest(plaintext.as_slice())))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let purpose: Purpose = args.purpose.parse()?;
    let sealed: Sealed = serde_json::from_str(
        &std::fs::read_to_string(&args.sealed)
            .with_context(|| format!("reading {}", args.sealed.display()))?,
    )
    .context("the sealed file is not a sealed message")?;
    tracing::info!(%purpose, holder = %args.holder, "asking for a key by exchange");

    let mut outcome = None;
    for tried in 1..=args.attempts.max(1) {
        match attempt(&args, &purpose, &sealed).await {
            Ok(sha256) => {
                outcome = Some(sha256);
                break;
            }
            Err(error) => {
                tracing::warn!(tried, error = format!("{error:#}"), "the exchange failed");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
    match outcome {
        Some(sha256) => tracing::info!(%purpose, sha256, result = "ok", "opened the sealed file"),
        None => tracing::error!(%purpose, result = "failed", "gave up"),
    }
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
