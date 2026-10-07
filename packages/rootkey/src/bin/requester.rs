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
use shonin_rootkey::launcher::{LAUNCHER_SOCKET, request_token};
use shonin_rootkey::{Purpose, Received, Release, Requester, Sealed, open};

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
    let requester = Requester::new(purpose.clone())?;
    let token = request_token(&args.socket, &requester.nonce()).await?;
    // A token is stamped by a clock seconds ahead of this machine's (and a
    // holder's): wait until it is no longer in the future here, so a holder
    // that allows no skew takes it too.
    if let Some((Some(issued_at), _, _)) = token_times(&token) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        let ahead = issued_at.saturating_sub(now).min(60);
        if ahead > 0 {
            tokio::time::sleep(Duration::from_secs(ahead + 1)).await;
        }
    }
    let request = requester.request(token, None);
    let url = format!("{}/rootkey/exchange", args.holder.trim_end_matches('/'));
    let answer = reqwest::Client::new()
        .post(&url)
        .timeout(Duration::from_secs(60))
        .json(&request)
        .send()
        .await
        .context("posting the exchange request")?;
    let status = answer.status();
    if !status.is_success() {
        let why = answer.text().await.unwrap_or_default();
        // The token's times beside this machine's clock: a refusal over
        // freshness is a matter of clocks, and this says whose.
        let times = token_times(&request.token);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs_f64())
            .unwrap_or(0.0);
        tracing::warn!(
            ?times,
            now,
            "the refused token's iat, nbf and exp, and this machine's clock"
        );
        bail!("the holder refused ({status}): {why}");
    }
    let release: Release = answer.json().await.context("the holder's release")?;
    let Received::Key(pair) = requester.open(&request, &release)? else {
        bail!("the holder released the root, not a key");
    };
    tracing::info!("opened the release");
    let plaintext = open(&pair, sealed, args.aad.as_bytes())?;
    Ok(hex::encode(Sha256::digest(plaintext.as_slice())))
}

/// A token's `iat`, `nbf` and `exp`, read without checking it (it is our own
/// and only logged).
fn token_times(token: &str) -> Option<(Option<u64>, Option<u64>, Option<u64>)> {
    use base64::Engine as _;
    let claims = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let time = |name: &str| value.get(name).and_then(serde_json::Value::as_u64);
    Some((time("iat"), time("nbf"), time("exp")))
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
