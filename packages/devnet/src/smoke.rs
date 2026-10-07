//! The smoke check a deploy ends with: read-only, it sends nothing.
//!
//! On each EVM chain of the network, the deployment its file names is on
//! the chain and wired as the file says (the contracts have code, and the
//! station factory serves this deployment's vault factory, dollar and
//! registry). Through the public API: it answers its health, serves every
//! one of those chains with a fresh reading, and has priced a vault. Each
//! check is printed with how long it took, and any that fails fails the
//! whole.

use std::path::Path;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Signing};
use shared::Chain;

use crate::deploy;

/// How old a chain's reading in the API's snapshot may be, on that chain's
/// own clock against this machine's, before the chain counts as not served.
const FRESH: Duration = Duration::from_secs(15 * 60);

/// How long one request to the API may take.
const API_TIMEOUT: Duration = Duration::from_secs(20);

/// What the smoke check is asked to do.
pub struct Options {
    /// The API's public address, e.g. `https://testnet-api.shonin.fi`;
    /// `None` checks the chains alone.
    pub api: Option<String>,
    /// A fresh generation has no vault yet: don't ask for one priced.
    pub allow_no_vault: bool,
}

/// One check's outcome.
struct Check {
    what: String,
    result: Result<String>,
    took: Duration,
}

/// Run every check on `chains` (each vault chain, its key and its config)
/// and the API, print each, and fail if any failed.
pub async fn run(
    network: NetworkName,
    chains: &[(Chain, ChainKey, ChainConfig)],
    deployments_dir: &Path,
    options: &Options,
) -> Result<()> {
    let started = Instant::now();
    let mut checks =
        futures::future::join_all(chains.iter().map(|(chain, key, config)| async move {
            let began = Instant::now();
            let result = deployment(network, key, config, deployments_dir).await;
            Check {
                what: format!("{} ({key}): the deployment on the chain", chain.label()),
                result,
                took: began.elapsed(),
            }
        }))
        .await;
    if let Some(api) = &options.api {
        checks.extend(api_checks(api, chains, options.allow_no_vault).await);
    }
    let mut failed = 0;
    for check in &checks {
        match &check.result {
            Ok(said) => println!(
                "ok    {:>5.1} s  {}: {said}",
                check.took.as_secs_f64(),
                check.what
            ),
            Err(error) => {
                failed += 1;
                println!(
                    "FAIL  {:>5.1} s  {}: {error:#}",
                    check.took.as_secs_f64(),
                    check.what
                );
            }
        }
    }
    println!(
        "smoke check: {} of {} passed in {:.0} s",
        checks.len() - failed,
        checks.len(),
        started.elapsed().as_secs_f64()
    );
    if failed > 0 {
        bail!("{failed} smoke check(s) failed");
    }
    Ok(())
}

/// The deployment `key`'s file names, read back from the chain: its
/// contracts have code and its station factory is initialised for it.
async fn deployment(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
    deployments_dir: &Path,
) -> Result<String> {
    let file = Deployments::file_in(deployments_dir, key);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("no deployment file at {}", file.display()))?;
    let deployments = Deployments::from_json(&text)?;
    if deploy::recorded_setup(&text).is_some() {
        bail!("its setup is unfinished: run the deploy again to carry it on");
    }
    let generation = deploy::recorded_generation(&text);
    let client = chain::args::connect_with(key, config, network, Signing::ReadOnly).await?;
    let contracts: [(&str, Address); 9] = [
        ("dollar", deployments.usd),
        ("attestation registry", deployments.attestation),
        ("prices", deployments.prices),
        ("publishing", deployments.publishing),
        ("strategy registry", deployments.strategies),
        ("review registry", deployments.reviews),
        ("vault factory", deployments.factory),
        ("station factory", deployments.station_factory),
        ("closer", deployments.closer),
    ];
    let present =
        futures::future::join_all(contracts.iter().map(|(_, at)| client.has_code(*at))).await;
    let missing: Vec<String> = contracts
        .iter()
        .zip(present)
        .filter_map(|((name, at), present)| match present {
            Ok(true) => None,
            Ok(false) => Some(format!("{name} {at} has no code")),
            Err(error) => Some(format!("{name} {at} could not be read: {error:#}")),
        })
        .collect();
    if !missing.is_empty() {
        bail!("{}", missing.join("; "));
    }
    let set = deploy::station_factory_set(&client, deployments.station_factory)
        .await?
        .ok_or_else(|| anyhow!("the station factory is not initialised"))?;
    if set.vault_factory != deployments.factory
        || set.asset != deployments.usd
        || set.attestation != deployments.attestation
    {
        bail!(
            "the station factory serves another deployment (vault factory {}, dollar {}, registry {})",
            set.vault_factory,
            set.asset,
            set.attestation
        );
    }
    let head = client.block_number().await?;
    Ok(format!(
        "{} contracts with code, the station factory serves this deployment{}, head {head}",
        contracts.len(),
        generation.map_or_else(String::new, |generation| format!(
            ", generation {generation}"
        )),
    ))
}

/// The API's checks: its health, each chain served and fresh, a vault
/// priced.
async fn api_checks(
    api: &str,
    chains: &[(Chain, ChainKey, ChainConfig)],
    allow_no_vault: bool,
) -> Vec<Check> {
    let api = api.trim_end_matches('/');
    let http = match reqwest::Client::builder().timeout(API_TIMEOUT).build() {
        Ok(http) => http,
        Err(error) => {
            return vec![Check {
                what: "the API".to_string(),
                result: Err(anyhow!(error)),
                took: Duration::ZERO,
            }];
        }
    };
    let mut checks = Vec::new();

    let began = Instant::now();
    let health = async {
        let response = http.get(format!("{api}/health")).send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            bail!("{status}: {}", body.trim());
        }
        Ok(body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
            .chars()
            .take(300)
            .collect::<String>())
    }
    .await;
    checks.push(Check {
        what: format!("the API ({api}): its health"),
        result: health,
        took: began.elapsed(),
    });

    let began = Instant::now();
    let snapshot: Result<shared::api::Snapshot> = async {
        let response = http
            .get(format!("{api}/api/snapshot"))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }
    .await;
    let took = began.elapsed();
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            checks.push(Check {
                what: "the API: its snapshot".to_string(),
                result: Err(error),
                took,
            });
            return checks;
        }
    };
    let now = time::OffsetDateTime::now_utc();
    for (chain, key, _) in chains {
        let began = Instant::now();
        let served = async {
            let info = snapshot
                .chain(*chain)
                .ok_or_else(|| anyhow!("not in the snapshot"))?;
            let as_of = info
                .as_of
                .ok_or_else(|| anyhow!("not read yet (block {})", info.block))?;
            let age = now - as_of.assume_utc();
            if age > time::Duration::try_from(FRESH)? {
                bail!(
                    "its reading is {} minutes old (block {})",
                    age.whole_minutes(),
                    info.block
                );
            }
            let config = http
                .get(format!("{api}/api/{}/config", chain.name()))
                .send()
                .await?;
            if !config.status().is_success() {
                bail!("its config answers {}", config.status());
            }
            Ok(format!(
                "read at block {}, {} s ago by its own clock",
                info.block,
                age.whole_seconds().max(0)
            ))
        }
        .await;
        checks.push(Check {
            what: format!("the API: {} ({key}) served", chain.label()),
            result: served,
            took: began.elapsed(),
        });
    }

    let priced: Vec<String> = snapshot
        .strategies
        .iter()
        .filter(|strategy| strategy.vault.share_price > 0.0 && strategy.vault.shares > 0.0)
        .filter(|strategy| {
            chains
                .iter()
                .any(|(chain, _, _)| *chain == strategy.vault.chain)
        })
        .map(|strategy| {
            format!(
                "{} at {:.6} a share",
                strategy.slug, strategy.vault.share_price
            )
        })
        .collect();
    checks.push(Check {
        what: "the API: a vault priced".to_string(),
        result: match (priced.first(), allow_no_vault) {
            (Some(first), _) => Ok(format!("{first} ({} priced in all)", priced.len())),
            (None, true) => Ok("none yet, allowed for a fresh generation".to_string()),
            (None, false) => Err(anyhow!(
                "no vault on these chains holds shares at a price (a fresh generation has none: \
                 pass --allow-no-vault)"
            )),
        },
        took: Duration::ZERO,
    });
    checks
}
