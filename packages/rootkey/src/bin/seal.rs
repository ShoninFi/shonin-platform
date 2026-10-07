//! `shonin-seal`: seal a file to one purpose's published key, after checking
//! the key against the root the canonical chain records.
//!
//! ```text
//! shonin-seal --network testnet --deployments .shonin/testnet/deployments \
//!   --published https://.../api/rootkey/published/platform:dummy \
//!   --purpose platform:dummy --in secret.txt --out secret.sealed.json
//! ```
//!
//! The canonical chain is the network's `root` in the chain config, or the
//! one `--chain` names. `task rootkey:seal` runs it with the network's API
//! and deployments.
//!
//! It refuses unless the registry's root-key record is live, and unless the
//! published key is signed by the recorded signer, for that purpose and that
//! chain. The plaintext is read from `--in` (`-` for standard input) and is
//! never printed.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use shonin_rootkey::{ChainView, PublishedKey, Purpose, RegistryView, seal};
use zeroize::Zeroizing;

/// The most a published key's answer may be: its JSON is a few hundred
/// bytes.
const MOST_PUBLISHED_BYTES: usize = 16 * 1024;

#[derive(Parser)]
#[command(about = "Seal a file to a purpose's published key, checked against the chain's root")]
struct Args {
    /// The canonical chain, as `config/chains.toml` names it (`evm:84532`).
    /// Without it, the network's `root`.
    #[arg(long)]
    chain: Option<String>,
    /// The network whose canonical chain is meant, when `--chain` names
    /// none.
    #[arg(long, env = "SHONIN_NETWORK", default_value = "local")]
    network: chain::NetworkName,
    /// The chain config.
    #[arg(long, env = "SHONIN_CHAINS", default_value = "config/chains.toml")]
    chains: PathBuf,
    /// The directory of deployment files (one per chain, `evm-84532.json`).
    #[arg(long)]
    deployments: PathBuf,
    /// The published key: an `http(s)://` URL or a file of its JSON.
    #[arg(long)]
    published: String,
    /// The purpose to seal to (`platform:dummy`, `builder`,
    /// `bundle:0x...`, `venue:<chain>:0x<vault>:<venue>`).
    #[arg(long)]
    purpose: String,
    /// The plaintext: a file, or `-` for standard input.
    #[arg(long = "in")]
    input: String,
    /// Where the sealed JSON goes.
    #[arg(long)]
    out: PathBuf,
    /// Associated data the opener must give again (empty by default).
    #[arg(long, default_value = "")]
    aad: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let purpose: Purpose = args.purpose.parse()?;
    if purpose == Purpose::Root {
        bail!("nothing is sealed to the root itself: name a purpose");
    }
    let chain: chain::ChainKey = match &args.chain {
        Some(named) => named.parse().map_err(|error| anyhow::anyhow!("{error}"))?,
        None => chain::ChainConfigs::load(&args.chains)?
            .network(args.network)
            .and_then(|network| network.root())
            .map(|(_, key)| key.clone())
            .with_context(|| {
                format!(
                    "the {} network names no canonical chain for the root key (`root` in {})",
                    args.network,
                    args.chains.display()
                )
            })?,
    };

    let view = RegistryView::connect(&args.chains, &chain, &args.deployments).await?;
    let record = view.root_key().await?;
    if record.signer.is_zero() || !view.is_root_key_live().await? {
        bail!(
            "the root key recorded on {chain} is not live: nothing can be sealed to it until a root is recorded and its image, project and checker are allowed"
        );
    }

    let published: PublishedKey = if args.published.starts_with("http://")
        || args.published.starts_with("https://")
    {
        // A bounded fetch: the key is checked below, so where it came from
        // needs no trust, only an answer of reasonable size in reasonable
        // time.
        let answer = reqwest::Client::new()
            .get(&args.published)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("fetching the published key")?
            .error_for_status()
            .context("fetching the published key")?;
        let body = shonin_rootkey::body::read_at_most(answer, MOST_PUBLISHED_BYTES)
            .await
            .context("fetching the published key")?;
        serde_json::from_slice(&body).context("the published key is not a published key's JSON")?
    } else {
        let text = std::fs::read_to_string(&args.published)
            .with_context(|| format!("reading {}", args.published))?;
        serde_json::from_str(&text).context("the published key is not a published key's JSON")?
    };
    published.verify(record.signer, view.chain_id(), &purpose)?;

    let mut plaintext = Zeroizing::new(Vec::new());
    if args.input == "-" {
        std::io::stdin()
            .read_to_end(&mut plaintext)
            .context("reading standard input")?;
    } else {
        std::fs::File::open(&args.input)
            .and_then(|mut file| file.read_to_end(&mut plaintext))
            .with_context(|| format!("reading {}", args.input))?;
    }

    let sealed = seal(&published, &plaintext, args.aad.as_bytes())?;
    let json = serde_json::to_string_pretty(&sealed).expect("a sealed message serialises");
    std::fs::write(&args.out, json + "\n")
        .with_context(|| format!("writing {}", args.out.display()))?;
    eprintln!(
        "sealed {} bytes to {purpose} under the root signer {} on {chain}: {}",
        plaintext.len(),
        record.signer,
        args.out.display()
    );
    Ok(())
}
