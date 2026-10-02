//! The web app's server.
//!
//! It runs the reference indexer in-process and serves what it holds: one
//! snapshot that carries everything a page renders, the platform's terms as
//! the contracts state them, a depositor's portfolio, the build queue, and
//! the store's files by hash. Submissions pass through to the build service
//! and health reports are taken only from a key blessed for their vault.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
use chain::bindings::publishing::IPublishing;
use chain::bindings::vault::IVault;
use chain::{Client, Role, Served};
use clap::Parser;
use indexer::Indexer;
use shared::model::registry::Job;
use shared::{
    Addresses, Chain, ChainInfo, Config, DevMode, HealthPost, HeldClaim, Holding, Portfolio,
    Snapshot, Terms, Upload, UploadAccepted,
};

#[derive(Parser)]
#[command(about = "The web app's server: the record, the queue and the terms")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// Where the build service answers, for the queue and for submissions.
    #[arg(long)]
    builder_url: String,
    /// Where the executor serves each vault's signed usage total.
    #[arg(long)]
    executor_url: String,
    #[arg(long, default_value_t = 19_081)]
    port: u16,
}

struct Service {
    indexer: Arc<Indexer>,
    builder: String,
    executor: String,
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
    let indexer = Arc::new(Indexer::new(
        served,
        args.store.clone(),
        Some(args.builder_url.clone()),
    ));
    let reader = indexer.clone();
    tokio::spawn(async move { reader.run().await });

    let service = Arc::new(Service {
        indexer,
        builder: args.builder_url.clone(),
        executor: args.executor_url.clone(),
    });

    let app = axum::Router::new()
        .route("/health", get(|| async { "api" }))
        .route("/api/snapshot", get(snapshot))
        // Across every vault chain: the record, and the one build queue.
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job))
        // One vault chain, named in the path.
        .route("/api/{chain}/config", get(config))
        .route("/api/{chain}/portfolio/{address}", get(portfolio))
        .route("/api/{chain}/uploads", post(uploads))
        .route("/api/{chain}/health", post(post_health))
        .route("/api/{chain}/usage/{vault}", get(usage))
        .route("/api/store/{hash}/{name}", get(store_file))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(service);

    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the API is open");
    axum::serve(listener, app).await?;
    Ok(())
}

// What the API says ----------------------------------------------------------

async fn snapshot(State(service): State<Arc<Service>>) -> Json<Snapshot> {
    Json(service.indexer.snapshot.read().await.clone())
}

/// The vault chain a path names, if this API serves it.
fn served(
    service: &Service,
    chain: Chain,
) -> Result<&Served, (StatusCode, Json<shared::ApiError>)> {
    service.indexer.on(chain).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(shared::ApiError {
                message: format!("this platform does not serve {} here", chain.label()),
            }),
        )
    })
}

async fn config(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
) -> Result<Json<Config>, (StatusCode, Json<shared::ApiError>)> {
    let served = served(&service, chain)?;
    let terms = terms(chain, &served.client, &served.deployments)
        .await
        .map_err(|error| {
            tracing::warn!(%chain, %error, "could not read the platform's terms");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(shared::ApiError {
                    // An app told the wrong terms would state them as facts,
                    // so it is told nothing until the chain answers.
                    message: format!("{} did not answer for the platform's terms", chain.label()),
                }),
            )
        })?;
    let info = service
        .indexer
        .snapshot
        .read()
        .await
        .chain(chain)
        .cloned()
        .unwrap_or(ChainInfo {
            chain,
            chain_id: served.client.chain_id,
            block: 0,
        });
    let deployments = &served.deployments;
    // Only a development chain holds its accounts' keys, so only there can the
    // app offer one as a wallet.
    let viewer = served
        .client
        .is_development()
        .then(|| served.client.account(Role::Viewer).ok())
        .flatten();
    Ok(Json(Config {
        chain: info,
        rpc_url: served.client.rpc_url.clone(),
        addresses: Addresses {
            usd: format!("{:?}", deployments.usd),
            publishing: format!("{:?}", deployments.publishing),
            prices: format!("{:?}", deployments.prices),
            venue: format!("{:?}", deployments.venue),
            attestation: format!("{:?}", deployments.attestation),
            strategies: format!("{:?}", deployments.strategies),
            reviews: format!("{:?}", deployments.reviews),
            profiles: format!("{:?}", deployments.profiles),
            factory: format!("{:?}", deployments.factory),
            station_factory: format!("{:?}", deployments.station_factory),
        },
        asset_symbol: chain.dollar().to_string(),
        asset_decimals: 6,
        terms,
        dev: viewer.map(|address| DevMode {
            address: format!("{address:?}"),
            label: "Development wallet".to_string(),
        }),
    }))
}

async fn portfolio(
    State(service): State<Arc<Service>>,
    UrlPath((chain, address)): UrlPath<(Chain, String)>,
) -> Result<Json<Portfolio>, (StatusCode, Json<shared::ApiError>)> {
    let served = served(&service, chain)?;
    let who: Address = address.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(shared::ApiError {
                message: "that is not an address".to_string(),
            }),
        )
    })?;
    let client = &served.client;

    let balance = client
        .read(
            served.deployments.usd,
            IERC20::balanceOfCall { account: who },
        )
        .await
        .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6)
        .unwrap_or(0.0);

    let snapshot = service.indexer.snapshot.read().await;
    let mut positions = Vec::new();
    let mut claims = Vec::new();

    for strategy in snapshot
        .strategies
        .iter()
        .filter(|strategy| strategy.vault.chain == chain)
    {
        let Ok(vault) = strategy.vault.address.parse::<Address>() else {
            continue;
        };
        let held = client
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
                cost: cost_basis(client, vault, who).await.unwrap_or(0.0),
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
                    booked: claim.booked.midnight(),
                    settled_for: claim.settled_for,
                });
            }
        }
    }

    Ok(Json(Portfolio {
        chain,
        address,
        balance,
        positions,
        claims,
    }))
}

/// What an address paid for what it still holds: everything it put in, less
/// everything it took out. The vault's own log is the whole of it.
async fn cost_basis(chain: &Client, vault: Address, who: Address) -> Option<f64> {
    use alloy::primitives::B256;
    use alloy::rpc::types::Filter;
    use alloy::sol_types::SolEvent;
    use chain::bindings::vault::IVault;

    // The owner is the second indexed field of a deposit or a withdrawal
    // (after the request's id), and the first of a fallback's payments.
    let logs = |signature: B256, owner_topic: usize| {
        let mut filter = Filter::new()
            .address(vault)
            .event_signature(signature)
            .from_block(0u64);
        filter = if owner_topic == 2 {
            filter.topic2(who.into_word())
        } else {
            filter.topic1(who.into_word())
        };
        async move { chain.logs(&filter).await.ok() }
    };

    let mut paid = 0.0;
    for log in logs(IVault::Deposited::SIGNATURE_HASH, 2).await? {
        if let Ok(event) = log.log_decode::<IVault::Deposited>() {
            paid += dollars(event.inner.data.assets);
        }
    }
    for log in logs(IVault::Withdrawn::SIGNATURE_HASH, 2).await? {
        if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
            paid -= dollars(event.inner.data.assets);
        }
    }
    for log in logs(IVault::ProRataTaken::SIGNATURE_HASH, 1).await? {
        if let Ok(event) = log.log_decode::<IVault::ProRataTaken>() {
            paid -= dollars(event.inner.data.assets);
        }
    }
    for log in logs(IVault::UnitsPaid::SIGNATURE_HASH, 1).await? {
        if let Ok(event) = log.log_decode::<IVault::UnitsPaid>() {
            paid -= dollars(event.inner.data.assets);
        }
    }
    Some(paid.max(0.0))
}

/// The terms the platform is running under, asked of the contracts that
/// enforce them. They are governed rather than fixed, so the app is told them
/// instead of being built with them.
/// The terms one vault chain's contracts enforce.
async fn terms(on: Chain, chain: &Client, deployments: &chain::Deployments) -> Result<Terms> {
    const DAY: f64 = 86_400.0;

    let vaults = chain
        .read(deployments.factory, IVaultFactory::configCall {})
        .await
        .with_context(|| format!("reading the terms on {on}"))?;
    let timelock = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::timelockCall {},
        )
        .await
        .with_context(|| format!("reading the terms on {on}"))?;
    let publish_fee = chain
        .read(deployments.publishing, IPublishing::feeCall {})
        .await
        .with_context(|| format!("reading the terms on {on}"))?;
    let terms_hash = chain
        .read(deployments.factory, IVaultFactory::termsCall {})
        .await
        .with_context(|| format!("reading the terms on {on}"))?;

    Ok(Terms {
        minimum_tank: dollars(vaults.minimumTank),
        publish_fee: dollars(publish_fee),
        fees: shared::Fees {
            holding_bps: vaults.holdingBps as u32,
            author_bps: vaults.authorBps as u32,
            platform_bps: vaults.platformBps as u32,
            transaction_bps: vaults.transactionBps as u32,
        },
        exit_band_bps: vaults.exitBandBps as u32,
        freeze_after_days: vaults.timers.freezeAfter as f64 / DAY,
        image_timelock_days: timelock as f64 / DAY,
        minimum_deposit: dollars(vaults.minimumDeposit),
        terms_hash: format!("{terms_hash:?}"),
    })
}

fn dollars(value: alloy::primitives::U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6
}

async fn jobs(State(service): State<Arc<Service>>) -> Json<Vec<Job>> {
    Json(service.indexer.snapshot.read().await.jobs.clone())
}

/// One job, as fresh as the build service has it, with what the chain adds.
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
    let job = found.ok_or(StatusCode::NOT_FOUND)?;
    let job = match service.indexer.on(job.chain) {
        Some(served) => {
            indexer::read::job(
                &served.client,
                &served.deployments,
                &service.indexer.store,
                job,
            )
            .await
        }
        None => job,
    };
    Ok(Json(job))
}

/// Uploads go straight to the build service; the API only carries them.
async fn uploads(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    Json(upload): Json<Upload>,
) -> Result<Json<UploadAccepted>, (StatusCode, Json<shared::ApiError>)> {
    served(&service, chain)?;
    let fail = |status: StatusCode, message: String| (status, Json(shared::ApiError { message }));
    let reply = reqwest::Client::new()
        .post(format!("{}/{chain}/uploads", service.builder))
        .json(&upload)
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
    UrlPath(chain): UrlPath<Chain>,
    Json(post): Json<HealthPost>,
) -> StatusCode {
    let Ok(vault) = post.vault.parse::<Address>() else {
        return StatusCode::BAD_REQUEST;
    };
    let Ok(signer) = recover(&post) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(served) = service.indexer.on(chain) else {
        return StatusCode::NOT_FOUND;
    };
    let bundle = match served.client.read(vault, IVault::bundleHashCall {}).await {
        Ok(bundle) => bundle,
        Err(error) => {
            tracing::warn!(%vault, %error, "could not read the vault for a health report");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    let blessed = served
        .client
        .read(
            served.deployments.attestation,
            IAttestationRegistry::isBlessedForCall {
                signer,
                vaultChain: alloy::primitives::U256::from(served.deployments.chain_id),
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

    service
        .indexer
        .health
        .write()
        .await
        .insert((chain, vault), post.report);
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

/// A vault's running usage total, as its executor signed it: what an author
/// withdrawing from the tank settles first. Carried, not checked: the vault's
/// contract checks the signature.
async fn usage(
    State(service): State<Arc<Service>>,
    UrlPath((chain, vault)): UrlPath<(Chain, String)>,
) -> impl IntoResponse {
    let Some(served) = service.indexer.on(chain) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let url = format!(
        "{}/usage/{}/{}",
        service.executor, served.deployments.chain_id, vault
    );
    match reqwest::get(&url).await {
        Ok(reply) if reply.status().is_success() => match reply.text().await {
            Ok(body) => (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        },
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
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
    match service.indexer.store.read_path(&hash, &name) {
        Some((bytes, kind)) => ([(header::CONTENT_TYPE, kind)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "nothing here").into_response(),
    }
}
