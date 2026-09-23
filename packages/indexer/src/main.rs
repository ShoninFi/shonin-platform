//! The indexer, and the API the web app reads.
//!
//! It follows the chain, turns each vault's value per share into a record, and
//! serves one snapshot that carries everything a page renders, so every figure
//! on the screen comes from the same moment. It needs no enclave and holds no
//! key: anyone can run their own and compare.

mod read;
mod store;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::vault::IVault;
use chain::{Client, Deployments, Role};
use clap::Parser;
use shared::model::health::Health;
use shared::model::registry::Job;
use shared::{
    Addresses, ChainInfo, Config, DevMode, HealthPost, HeldClaim, Holding, Portfolio, Snapshot,
    Submission, SubmissionAccepted, Terms, Totals,
};
use tokio::sync::RwLock;

use crate::read::History;
use crate::store::Store;

#[derive(Parser)]
#[command(about = "Reads the chain into the record, and serves the public API")]
struct Args {
    #[arg(long, env = "SHONIN_RPC_URL")]
    rpc_url: String,
    #[arg(long)]
    deployments: PathBuf,
    #[arg(long)]
    store: PathBuf,
    /// Where the build service answers, for the queue and for submissions.
    #[arg(long)]
    builder_url: String,
    #[arg(long, default_value_t = 19_081)]
    port: u16,
}

struct Service {
    chain: Client,
    deployments: Deployments,
    store: Store,
    builder: String,
    snapshot: RwLock<Snapshot>,
    history: RwLock<History>,
    /// Health reports, as executors post them.
    health: RwLock<HashMap<Address, Health>>,
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
    let chain = Client::connect(&args.rpc_url).await?;
    let deployments = chain.wait_for_deployments(&args.deployments).await?;

    let service = Arc::new(Service {
        snapshot: RwLock::new(empty_snapshot(&chain)),
        history: RwLock::new(History::default()),
        health: RwLock::new(HashMap::new()),
        store: Store::new(args.store.clone()),
        builder: args.builder_url.clone(),
        chain,
        deployments,
    });

    let refresher = service.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = refresh(&refresher).await {
                tracing::warn!(%error, "could not read the chain");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let app = axum::Router::new()
        .route("/health", get(|| async { "indexer" }))
        .route("/api/snapshot", get(snapshot))
        .route("/api/config", get(config))
        .route("/api/portfolio/{address}", get(portfolio))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job))
        .route("/api/submissions", post(submissions))
        .route("/api/health", post(post_health))
        .route("/api/store/{hash}/{name}", get(store_file))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(service);

    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the API is open");
    axum::serve(listener, app).await?;
    Ok(())
}

fn empty_snapshot(chain: &Client) -> Snapshot {
    Snapshot {
        version: 0,
        as_of: read::moment(0),
        chain: ChainInfo {
            chain: shared::model::common::Chain::Local,
            chain_id: chain.chain_id,
            block: 0,
        },
        strategies: Vec::new(),
        authors: Vec::new(),
        jobs: Vec::new(),
        feed: Vec::new(),
        benchmarks: Vec::new(),
        totals: Totals {
            vault_capital: 0.0,
            trades: 0,
        },
    }
}

/// Read everything again. It is cheap enough to do often and simple enough to
/// be obviously correct, which matters more here than being clever.
async fn refresh(service: &Arc<Service>) -> Result<()> {
    let head = service.chain.block_number().await?;
    let now = service.chain.block_timestamp().await?;
    let health = service.health.read().await.clone();

    let strategies = {
        let mut history = service.history.write().await;
        read::strategies(
            &service.chain,
            &service.deployments,
            &service.store,
            &mut history,
            &health,
            head,
            now,
        )
        .await?
    };
    let authors = read::authors(&service.chain, &service.deployments).await?;
    let feed = read::feed(&service.chain, &service.deployments, &strategies).await?;
    let benchmarks = {
        let mut history = service.history.write().await;
        read::benchmark(&service.chain, &service.deployments, &mut history, head).await?
    };
    let jobs = fetch_jobs(service).await.unwrap_or_default();

    let totals = Totals {
        // Every vault still holding depositors' money, which includes one
        // whose author has published a successor: it carries on as it was.
        vault_capital: strategies
            .iter()
            .filter(|strategy| {
                matches!(
                    strategy.vault.state,
                    shared::model::vault::VaultState::Open
                        | shared::model::vault::VaultState::Frozen
                )
            })
            .map(|strategy| strategy.vault.value)
            .sum(),
        trades: strategies
            .iter()
            .map(|strategy| strategy.trades.len() as u32)
            .sum(),
    };

    let mut snapshot = service.snapshot.write().await;
    snapshot.version += 1;
    snapshot.as_of = read::moment(now);
    snapshot.chain.block = head;
    snapshot.strategies = strategies;
    snapshot.authors = authors;
    snapshot.jobs = jobs;
    snapshot.feed = feed;
    snapshot.benchmarks = benchmarks;
    snapshot.totals = totals;
    Ok(())
}

async fn fetch_jobs(service: &Arc<Service>) -> Result<Vec<Job>> {
    let jobs = reqwest::Client::new()
        .get(format!("{}/jobs", service.builder))
        .send()
        .await?
        .json()
        .await?;
    Ok(jobs)
}

// What the API says ----------------------------------------------------------

async fn snapshot(State(service): State<Arc<Service>>) -> Json<Snapshot> {
    Json(service.snapshot.read().await.clone())
}

async fn config(
    State(service): State<Arc<Service>>,
) -> Result<Json<Config>, (StatusCode, Json<shared::ApiError>)> {
    let terms = terms(&service).await.map_err(|error| {
        tracing::warn!(%error, "could not read the platform's terms");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(shared::ApiError {
                // An app told the wrong terms would state them as facts, so it
                // is told nothing until the chain answers.
                message: "the chain did not answer for the platform's terms".to_string(),
            }),
        )
    })?;
    let snapshot = service.snapshot.read().await;
    let viewer = service.chain.account(Role::Viewer).ok();
    Ok(Json(Config {
        terms,
        chain: snapshot.chain.clone(),
        rpc_url: service.chain.rpc_url.clone(),
        addresses: Addresses {
            usd: format!("{:?}", service.deployments.usd),
            venue: format!("{:?}", service.deployments.venue),
            attestation: format!("{:?}", service.deployments.attestation),
            strategies: format!("{:?}", service.deployments.strategies),
            reviews: format!("{:?}", service.deployments.reviews),
            profiles: format!("{:?}", service.deployments.profiles),
            factory: format!("{:?}", service.deployments.factory),
        },
        asset_symbol: "USDC".to_string(),
        asset_decimals: 6,
        // The chain holds this account's key, which is what makes it a
        // development chain and why the app can offer it as a wallet.
        dev: viewer.map(|address| DevMode {
            address: format!("{address:?}"),
            label: "Development wallet".to_string(),
        }),
    }))
}

async fn portfolio(
    State(service): State<Arc<Service>>,
    UrlPath(address): UrlPath<String>,
) -> Result<Json<Portfolio>, (StatusCode, String)> {
    let who: Address = address.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "that is not an address".to_string(),
        )
    })?;

    let balance = service
        .chain
        .read(
            service.deployments.usd,
            IERC20::balanceOfCall { account: who },
        )
        .await
        .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6)
        .unwrap_or(0.0);

    let snapshot = service.snapshot.read().await;
    let mut positions = Vec::new();
    let mut claims = Vec::new();

    for strategy in &snapshot.strategies {
        let Ok(vault) = strategy.vault.address.parse::<Address>() else {
            continue;
        };
        let held = service
            .chain
            .read(vault, IERC20::balanceOfCall { account: who })
            .await
            .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18)
            .unwrap_or(0.0);
        if held > 0.0 {
            positions.push(Holding {
                slug: strategy.slug.clone(),
                shares: held,
                value: held * strategy.vault.share_price,
                exit_value: held
                    * strategy.vault.share_price
                    * (1.0 - strategy.vault.fees.transaction_bps as f64 / 10_000.0),
                cost: cost_basis(&service.chain, vault, who).await.unwrap_or(0.0),
            });
        }
        for claim in &strategy.vault.claims {
            if claim.owner.eq_ignore_ascii_case(&address)
                && claim.state != shared::model::vault::ClaimState::Paid
            {
                claims.push(HeldClaim {
                    slug: strategy.slug.clone(),
                    id: claim.id as u64,
                    shares: claim.shares,
                    fraction_closed: claim.fraction_closed,
                    state: claim.state,
                    booked: read::moment(0),
                    settled_for: claim.settled_for,
                });
            }
        }
    }

    Ok(Json(Portfolio {
        address,
        balance,
        positions,
        claims,
    }))
}

/// What an address paid for what it still holds: everything it put in, less
/// everything it took out. The vault's own log is the whole of it.
async fn cost_basis(chain: &Client, vault: Address, who: Address) -> Option<f64> {
    use alloy::rpc::types::Filter;
    use alloy::sol_types::SolEvent;
    use chain::bindings::vault::IVault;

    let mut paid = 0.0;
    let deposits = chain
        .logs(
            &Filter::new()
                .address(vault)
                .event_signature(IVault::Deposited::SIGNATURE_HASH)
                .topic1(who.into_word())
                .from_block(0u64),
        )
        .await
        .ok()?;
    for log in deposits {
        if let Ok(event) = log.log_decode::<IVault::Deposited>() {
            paid += dollars(event.inner.data.assets);
        }
    }

    let withdrawals = chain
        .logs(
            &Filter::new()
                .address(vault)
                .event_signature(IVault::Withdrawn::SIGNATURE_HASH)
                .topic1(who.into_word())
                .from_block(0u64),
        )
        .await
        .ok()?;
    for log in withdrawals {
        if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
            paid -= dollars(event.inner.data.assets);
        }
    }
    Some(paid.max(0.0))
}

/// The terms the platform is running under, asked of the contracts that
/// enforce them. They are governed rather than fixed, so the app is told them
/// instead of being built with them.
async fn terms(service: &Arc<Service>) -> Result<Terms> {
    const DAY: f64 = 86_400.0;

    let vaults = service
        .chain
        .read(service.deployments.factory, IVaultFactory::configCall {})
        .await?;
    let timelock = service
        .chain
        .read(
            service.deployments.attestation,
            IAttestationRegistry::timelockCall {},
        )
        .await?;
    let review_fee = service
        .chain
        .read(
            service.deployments.reviews,
            IReviewRegistry::reviewFeeCall {},
        )
        .await?;

    Ok(Terms {
        minimum_tank: dollars(vaults.minimumTank),
        review_fee: dollars(review_fee),
        fees: shared::Fees {
            streaming_bps: vaults.streamingBps as u32,
            author_bps: vaults.authorBps as u32,
            platform_bps: vaults.platformBps as u32,
            transaction_bps: vaults.transactionBps as u32,
        },
        exit_band_bps: vaults.exitBandBps as u32,
        freeze_after_days: vaults.freezeAfter as f64 / DAY,
        image_timelock_days: timelock as f64 / DAY,
        tank_target_days: vaults.tankTargetDays,
    })
}

fn dollars(value: alloy::primitives::U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6
}

async fn jobs(State(service): State<Arc<Service>>) -> Json<Vec<Job>> {
    Json(service.snapshot.read().await.jobs.clone())
}

async fn job(
    State(service): State<Arc<Service>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, StatusCode> {
    let found: Option<Job> = reqwest::Client::new()
        .get(format!("{}/jobs/{id}", service.builder))
        .send()
        .await
        .ok()
        .and_then(|reply| reply.error_for_status().ok())
        .map(|reply| reply.json())
        .ok_or(StatusCode::NOT_FOUND)?
        .await
        .ok();
    found.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// Submissions go straight to the build service; the API only carries them.
async fn submissions(
    State(service): State<Arc<Service>>,
    Json(submission): Json<Submission>,
) -> Result<Json<SubmissionAccepted>, (StatusCode, Json<shared::ApiError>)> {
    let fail = |status: StatusCode, message: String| (status, Json(shared::ApiError { message }));
    let reply = reqwest::Client::new()
        .post(format!("{}/submissions", service.builder))
        .json(&submission)
        .send()
        .await
        .map_err(|error| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("the build service is not answering: {error}"),
            )
        })?;
    let status = reply.status();
    if !status.is_success() {
        let body = reply.text().await.unwrap_or_default();
        return Err(fail(status, body));
    }
    reply
        .json()
        .await
        .map(Json)
        .map_err(|error| fail(StatusCode::BAD_GATEWAY, error.to_string()))
}

/// An executor's report on how a strategy is running.
/// Take a health report, but only from a key the registry blessed for that
/// vault. Anyone may read a report; only the instance running the strategy may
/// write one, or the page would be repeating whatever it was told.
async fn post_health(
    State(service): State<Arc<Service>>,
    Json(post): Json<HealthPost>,
) -> StatusCode {
    let Ok(vault) = post.vault.parse::<Address>() else {
        return StatusCode::BAD_REQUEST;
    };
    let Ok(signer) = recover(&post) else {
        return StatusCode::BAD_REQUEST;
    };
    let bundle = match service.chain.read(vault, IVault::bundleHashCall {}).await {
        Ok(bundle) => bundle,
        Err(error) => {
            tracing::warn!(%vault, %error, "could not read the vault for a health report");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    let blessed = service
        .chain
        .read(
            service.deployments.attestation,
            IAttestationRegistry::isBlessedForCall {
                signer,
                vault,
                strategyHash: bundle,
            },
        )
        .await
        .unwrap_or(false);
    if !blessed {
        tracing::warn!(%vault, %signer, "a health report from a key this vault has not blessed");
        return StatusCode::FORBIDDEN;
    }

    service.health.write().await.insert(vault, post.report);
    StatusCode::ACCEPTED
}

/// Who signed a report, from the signature it carries.
fn recover(post: &HealthPost) -> Result<Address> {
    use alloy::primitives::{B256, Signature};

    let bytes = alloy::hex::decode(&post.signature).context("the signature is not hex")?;
    let signature = Signature::try_from(bytes.as_slice())
        .map_err(|error| anyhow::anyhow!("the signature is malformed: {error}"))?;
    let digest = B256::from(HealthPost::digest(&post.vault, &post.report));
    signature
        .recover_address_from_msg(digest.as_slice())
        .context("recovering the signer")
}

/// Anything the store holds, to whoever has its hash.
async fn store_file(
    State(service): State<Arc<Service>>,
    UrlPath((hash, name)): UrlPath<(String, String)>,
) -> impl IntoResponse {
    if name.contains("..") || name.contains('/') {
        return (StatusCode::BAD_REQUEST, "no").into_response();
    }
    // Sealed source is kept so the reviewer can read it and the build can be
    // repeated, and it is never served.
    if name.ends_with(".sealed.tar") {
        return (StatusCode::FORBIDDEN, "this source is sealed").into_response();
    }
    match service.store.read_path(&hash, &name) {
        Some((bytes, kind)) => ([(header::CONTENT_TYPE, kind)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "nothing here").into_response(),
    }
}
