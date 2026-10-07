//! The web app's server.
//!
//! It runs the reference indexer in-process and serves what it holds: one
//! snapshot that carries everything a page renders, the platform's terms as
//! the contracts state them, a depositor's portfolio, the build queue, and
//! the store's files by hash. Submissions pass through to the build service
//! and health reports are taken only from a key blessed for their vault. The
//! root key's status and its published keys are relayed from the executor.

mod rootkey;
mod solana;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::publishing::IPublishing;
use chain::{Client, Role, Served, SolanaServed};
use clap::Parser;
use futures::{StreamExt, TryStreamExt};
use indexer::{Indexer, Lookup};
use shared::model::registry::Job;
use shared::{
    Addresses, Chain, ChainInfo, Config, DevMode, EvmAddresses, HealthPost, HeldClaim, Holding,
    Portfolio, Snapshot, Terms, Upload, UploadAccepted,
};

#[derive(Parser)]
#[command(about = "The web app's server: the record, the queue and the terms")]
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
    /// Where the build service answers, for the queue and for submissions.
    /// Without one the snapshot has no queue and uploads are refused.
    #[arg(long)]
    builder_url: Option<String>,
    /// Where the executor serves each vault's signed usage total.
    #[arg(long, env = "SHONIN_EXECUTOR_URL")]
    executor_url: String,
    #[arg(long, default_value_t = 19_081)]
    port: u16,
    /// The address to listen on: 0.0.0.0 in a container.
    #[arg(long, default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
}

struct Service {
    indexer: Arc<Indexer>,
    /// None where no build service is attached: no queue, no uploads.
    builder: Option<String>,
    executor: String,
    /// For asking the executor and the build service, bounded so one that
    /// stops answering can't hold a request open for good.
    http: reqwest::Client,
    /// How long an upload may take to be taken by the build service.
    upload_timeout: std::time::Duration,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    // The port is bound at once, and each chain is served from the moment
    // it connects: one chain out of reach holds up none of the others, and
    // is answered for as not connected yet.
    let connections = indexer::connections(&args.chain_args)?;
    let mut indexer = Indexer::new(
        connections.expected.clone(),
        args.store.clone(),
        args.builder_url.clone(),
    );
    if let Some(url) = args.store_url.as_deref().filter(|url| !url.is_empty()) {
        indexer = indexer.with_store_url(url);
    }
    let indexer = Arc::new(indexer);
    indexer.take(connections);
    if args.builder_url.is_none() {
        tracing::info!("no build service is attached: no queue, and uploads are refused");
    }
    let reader = indexer.clone();
    tokio::spawn(async move { reader.run().await });

    let service = Arc::new(Service {
        indexer,
        builder: args.builder_url.clone(),
        executor: args.executor_url.clone(),
        http: indexer::http(),
        upload_timeout: UPLOAD_TIMEOUT,
    });

    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/api/snapshot", get(snapshot))
        // Across every vault chain: the record, and the one build queue.
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job))
        // One vault chain, named in the path.
        .route("/api/{chain}/config", get(config))
        .route("/api/{chain}/portfolio/{address}", get(portfolio))
        .route(
            "/api/{chain}/uploads",
            post(uploads).layer(axum::extract::DefaultBodyLimit::max(
                shared::archive::MOST_UPLOAD_BYTES,
            )),
        )
        .route("/api/{chain}/health", post(post_health))
        .route("/api/{chain}/health/{vault}/signed", get(signed_health))
        .route("/api/{chain}/usage/{vault}", get(usage))
        .route("/api/store/{hash}/{name}", get(store_file))
        // The root key service's public reads, from the executor.
        .route("/api/rootkey", get(rootkey::status))
        .route("/api/rootkey/published/{purpose}", get(rootkey::published))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(service);

    let address = SocketAddr::from((args.listen, args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the API is open");
    axum::serve(listener, app).await?;
    Ok(())
}

// What the API says ----------------------------------------------------------

/// Up, and how far it has read on each chain: 503 while a chain it serves is
/// not connected, or has not been read lately.
async fn health(State(service): State<Arc<Service>>) -> (StatusCode, String) {
    service.indexer.health_answer("api")
}

async fn snapshot(State(service): State<Arc<Service>>) -> Json<Snapshot> {
    Json(service.indexer.snapshot.read().await.clone())
}

/// A vault chain the API serves, whichever kind it is.
enum On {
    Evm(Box<Served>),
    Solana(Box<SolanaServed>),
}

/// The vault chain a path names, if this API serves it and it has
/// connected. One not connected yet is unavailable (503), not unknown.
fn served(service: &Service, chain: Chain) -> Result<On, (StatusCode, Json<shared::ApiError>)> {
    let refused =
        |status: StatusCode, message: String| (status, Json(shared::ApiError { message }));
    match service.indexer.lookup(chain) {
        Lookup::Served(served) => Ok(On::Evm(served)),
        Lookup::Solana(served) => Ok(On::Solana(served)),
        Lookup::NotConnected => Err(refused(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{} is not connected yet", chain.label()),
        )),
        Lookup::NotServed => Err(refused(
            StatusCode::NOT_FOUND,
            format!("this platform does not serve {} here", chain.label()),
        )),
    }
}

async fn config(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
) -> Result<Json<Config>, (StatusCode, Json<shared::ApiError>)> {
    let served = match served(&service, chain)? {
        On::Evm(served) => served,
        On::Solana(served) => return solana::config(&service, chain, &served).await.map(Json),
    };
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
            key: served.key.to_string(),
            chain_id: Some(served.client.chain_id),
            block: 0,
            as_of: None,
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
        // The file's own URL, never an override: a paid one carries its key.
        rpc_url: served.public_rpc.clone(),
        addresses: Addresses::Evm(EvmAddresses {
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
        }),
        asset_symbol: chain.dollar().to_string(),
        asset_decimals: 6,
        terms,
        dev: viewer.map(|address| DevMode {
            address: format!("{address:?}"),
            label: "Development wallet".to_string(),
        }),
    }))
}

/// How long a portfolio may take to be read from the chain: it is a read
/// or two per vault, and a node slow enough to take longer is answered for
/// as not answering rather than holding the request open.
const PORTFOLIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How many of a portfolio's vaults are read from the chain at once.
const PORTFOLIO_READS_AT_ONCE: usize = 16;

async fn portfolio(
    State(service): State<Arc<Service>>,
    UrlPath((chain, address)): UrlPath<(Chain, String)>,
) -> Result<Json<Portfolio>, (StatusCode, Json<shared::ApiError>)> {
    tokio::time::timeout(PORTFOLIO_TIMEOUT, read_portfolio(&service, chain, address))
        .await
        .unwrap_or_else(|_| {
            tracing::warn!(%chain, "a portfolio was not read in time");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(shared::ApiError {
                    message: format!("{} did not answer in time", chain.label()),
                }),
            ))
        })
}

async fn read_portfolio(
    service: &Service,
    chain: Chain,
    address: String,
) -> Result<Json<Portfolio>, (StatusCode, Json<shared::ApiError>)> {
    let served = match served(service, chain)? {
        On::Evm(served) => served,
        On::Solana(served) => return solana::portfolio(service, chain, &served, address).await,
    };
    let who: Address = address.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(shared::ApiError {
                message: "that is not an address".to_string(),
            }),
        )
    })?;
    let client = &served.client;
    // A balance that could not be read is not a balance of nothing: the
    // portfolio is not answered rather than shown empty.
    let unread = |error: anyhow::Error| {
        tracing::warn!(%chain, %error, "a portfolio balance could not be read");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(shared::ApiError {
                message: format!("{} did not answer", chain.label()),
            }),
        )
    };

    let balance = client
        .read(
            served.deployments.usd,
            IERC20::balanceOfCall { account: who },
        )
        .await
        .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6)
        .map_err(unread)?;

    // The chain's strategies are copied out and the record let go before
    // any chain is read: a slow node holds up this request alone, never the
    // indexer or anyone reading the record.
    let strategies: Vec<_> = service
        .indexer
        .snapshot
        .read()
        .await
        .strategies
        .iter()
        .filter(|strategy| strategy.vault.chain == chain)
        // A vault never read whole has no share price to value a holding
        // at: it is shown, as not read yet, in the record alone.
        .filter(|strategy| !indexer::read::never_read(strategy))
        .cloned()
        .collect();
    // The vaults are read several at a time ([`PORTFOLIO_READS_AT_ONCE`]),
    // so a portfolio across many vaults isn't every read end to end.
    let reads: Vec<_> = strategies
        .iter()
        .map(|strategy| holding_of(client, strategy, who, served.deployments.deployed_block))
        .collect();
    let held: Vec<Option<(f64, f64)>> = futures::stream::iter(reads)
        .buffered(PORTFOLIO_READS_AT_ONCE)
        .try_collect()
        .await
        .map_err(unread)?;

    let mut positions = Vec::new();
    let mut claims = Vec::new();
    for (strategy, held) in strategies.iter().zip(held) {
        let Some((held, cost)) = held else {
            continue;
        };
        if held > 0.0 {
            positions.push(Holding {
                slug: strategy.slug.clone(),
                shares: held,
                value: held * strategy.vault.share_price,
                exit_value: held
                    * strategy.vault.share_price
                    * (1.0 - strategy.vault.fees.transaction_bps as f64 / 10_000.0),
                cost,
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

/// What `who` holds of a strategy's vault, in shares, and what it paid for
/// that, if the vault's address reads as one.
async fn holding_of(
    client: &Client,
    strategy: &shared::Strategy,
    who: Address,
    deployed_block: u64,
) -> anyhow::Result<Option<(f64, f64)>> {
    let Ok(vault) = strategy.vault.address.parse::<Address>() else {
        return Ok(None);
    };
    let held = client
        .read(vault, IERC20::balanceOfCall { account: who })
        .await
        .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18)?;
    let cost = if held > 0.0 {
        cost_basis(client, vault, who, deployed_block)
            .await
            .unwrap_or(0.0)
    } else {
        0.0
    };
    Ok(Some((held, cost)))
}

/// What an address paid for what it still holds: everything it put in, less
/// everything it took out. The vault's own log is the whole of it.
///
/// What it took out is found three ways, since the vault says so three ways:
///
/// - a withdrawal, or a claim paid while the vault ran: `Withdrawn` under the
///   owner (and, for a claim, a `ClaimPaid` right after it);
/// - a claim paid once the vault closed (`claimClosed`): a `ClaimPaid` alone,
///   which names only the claim, so the owner is the one its `ClaimBooked`
///   names;
/// - shares redeemed once the vault closed (`redeemClosed`): a `Withdrawn`
///   under the receiver, whoever that is, so it is counted for the holder
///   whose shares were burnt for it in the same transaction, and not for the
///   receiver.
///
/// Shares never move from one holder to another (the vault refuses
/// `transfer`), so no cost moves with them.
///
/// The log is read from `since`, the block the platform's contracts were
/// deployed in, since no vault is older than its factory; it is read in one
/// request per event, filtered to the vault and, where the event names one,
/// the owner. A node that caps the block range of a log request refuses it
/// once the chain has run past that cap since the deployment, and the cost
/// then reads as nothing rather than as a wrong figure.
async fn cost_basis(chain: &Client, vault: Address, who: Address, since: u64) -> Option<f64> {
    use std::collections::HashSet;

    use alloy::primitives::{B256, U256};
    use alloy::rpc::types::{Filter, Log};
    use alloy::sol_types::SolEvent;
    use chain::bindings::vault::IVault;

    let base = |signature: B256| {
        Filter::new()
            .address(vault)
            .event_signature(signature)
            .from_block(since)
    };
    let logs = |filter: Filter| async move { chain.logs(&filter).await.ok() };
    // Where a log sits: its transaction, and its place in it.
    let at = |log: &Log| (log.transaction_hash, log.log_index);
    let before = |log: &Log| {
        (
            log.transaction_hash,
            log.log_index.and_then(|i| i.checked_sub(1)),
        )
    };

    let mut paid = 0.0;
    for log in logs(base(IVault::Deposited::SIGNATURE_HASH).topic2(who.into_word())).await? {
        if let Ok(event) = log.log_decode::<IVault::Deposited>() {
            paid += dollars(event.inner.data.assets);
        }
    }

    // Every claim paid, and where the log just before each sits: a claim paid
    // while the vault ran follows its own `Withdrawn`.
    let claims_paid = logs(base(IVault::ClaimPaid::SIGNATURE_HASH)).await?;
    let before_a_claim_paid: HashSet<_> = claims_paid.iter().map(before).collect();

    let withdrawn = logs(base(IVault::Withdrawn::SIGNATURE_HASH).topic2(who.into_word())).await?;
    for log in &withdrawn {
        let Ok(event) = log.log_decode::<IVault::Withdrawn>() else {
            continue;
        };
        // A closed vault's redemption names its receiver: it is counted
        // below, for whoever held the shares.
        if event.inner.data.requestId.is_zero() && !before_a_claim_paid.contains(&at(log)) {
            continue;
        }
        paid -= dollars(event.inner.data.assets);
    }

    // A claim paid once the vault closed, with no `Withdrawn` before it: the
    // owner's if the owner booked it.
    let booked: HashSet<U256> =
        logs(base(IVault::ClaimBooked::SIGNATURE_HASH).topic2(who.into_word()))
            .await?
            .iter()
            .filter_map(|log| log.log_decode::<IVault::ClaimBooked>().ok())
            .map(|event| event.inner.data.claimId)
            .collect();
    let withdrawals: HashSet<_> = withdrawn.iter().map(at).collect();
    for log in &claims_paid {
        let Ok(event) = log.log_decode::<IVault::ClaimPaid>() else {
            continue;
        };
        if booked.contains(&event.inner.data.claimId) && !withdrawals.contains(&before(log)) {
            paid -= dollars(event.inner.data.assets);
        }
    }

    // Every redemption a closed vault paid, to whomever: a `Withdrawn` for no
    // request with no claim paid after it.
    let mut redeemed_closed = Vec::new();
    for log in logs(base(IVault::Withdrawn::SIGNATURE_HASH).topic1(B256::ZERO)).await? {
        if before_a_claim_paid.contains(&at(&log)) {
            continue;
        }
        if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
            redeemed_closed.push((event.inner.data, at(&log)));
        }
    }
    // This holder's part: the redemption each of its burns paid for, the
    // first after the burn in the same transaction for the same shares.
    if !redeemed_closed.is_empty() {
        let burnt = logs(
            base(IVault::Transfer::SIGNATURE_HASH)
                .topic1(who.into_word())
                .topic2(B256::ZERO),
        )
        .await?;
        let mut counted = HashSet::new();
        for log in &burnt {
            let Ok(event) = log.log_decode::<IVault::Transfer>() else {
                continue;
            };
            let (hash, index) = at(log);
            let paid_for = redeemed_closed
                .iter()
                .filter(|(redeemed, (in_tx, after))| {
                    *in_tx == hash
                        && redeemed.shares == event.inner.data.value
                        && after.zip(index).is_some_and(|(after, index)| after > index)
                        && !counted.contains(&(*in_tx, *after))
                })
                .min_by_key(|(_, (_, after))| *after);
            if let Some((redeemed, place)) = paid_for {
                counted.insert(*place);
                paid -= dollars(redeemed.assets);
            }
        }
    }

    for log in logs(base(IVault::ProRataTaken::SIGNATURE_HASH).topic1(who.into_word())).await? {
        if let Ok(event) = log.log_decode::<IVault::ProRataTaken>() {
            paid -= dollars(event.inner.data.assets);
        }
    }
    for log in logs(base(IVault::UnitsPaid::SIGNATURE_HASH).topic1(who.into_word())).await? {
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
/// 404 only when the build service says there is no such job; 503 when it
/// says it can't answer for now; 502 when it can't be reached, or answers
/// anything else.
async fn job(
    State(service): State<Arc<Service>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, StatusCode> {
    // The id goes into the build service's URL, so only an id the build
    // service could have given is passed on.
    if !is_job_id(&id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // No build service, no jobs.
    let Some(builder) = &service.builder else {
        return Err(StatusCode::NOT_FOUND);
    };
    let reply = service
        .http
        .get(format!("{builder}/jobs/{id}"))
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the build service could not be asked for a job");
            StatusCode::BAD_GATEWAY
        })?;
    let status = reply.status();
    match status {
        StatusCode::NOT_FOUND => return Err(StatusCode::NOT_FOUND),
        StatusCode::SERVICE_UNAVAILABLE => return Err(StatusCode::SERVICE_UNAVAILABLE),
        status if !status.is_success() => {
            tracing::warn!(%status, "the build service failed to answer for a job");
            return Err(StatusCode::BAD_GATEWAY);
        }
        _ => {}
    }
    let job: Job = reply.json().await.map_err(|error| {
        tracing::warn!(%error, "the build service's answer for a job could not be read");
        StatusCode::BAD_GATEWAY
    })?;
    Ok(Json(service.indexer.complete(job).await))
}

/// A job's id as the build service gives it: a number, in plain digits.
fn is_job_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// How long an upload may take to reach the build service and be taken:
/// it is unpacked and checked there before it is answered, so well past the
/// timeout of any other ask.
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Uploads go straight to the build service; the API only carries them. The
/// build service's refusal of an upload (a manifest it won't take, source
/// over the limit) is the author's to read and is passed on; anything else
/// that goes wrong is logged here and answered in general terms, so nothing
/// about the services behind the API reaches a caller.
async fn uploads(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    upload: Result<Json<Upload>, JsonRejection>,
) -> Result<Json<UploadAccepted>, (StatusCode, Json<shared::ApiError>)> {
    served(&service, chain)?;
    let fail = |status: StatusCode, message: &str| {
        (
            status,
            Json(shared::ApiError {
                message: message.to_string(),
            }),
        )
    };
    let Some(builder) = &service.builder else {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, NO_UPLOADS));
    };
    let Json(upload) = upload.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            fail(rejection.status(), &shared::archive::upload_too_large())
        } else {
            fail(rejection.status(), &rejection.body_text())
        }
    })?;
    let reply = service
        .http
        .post(format!("{builder}/{chain}/uploads"))
        .timeout(service.upload_timeout)
        .json(&upload)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "an upload could not be passed to the build service");
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "the build service is not answering; try again shortly",
            )
        })?;
    let status = reply.status();
    if status.is_client_error() {
        let body = reply.text().await.unwrap_or_default();
        return Err((status, Json(shared::ApiError { message: body })));
    }
    if !status.is_success() {
        let body = reply.text().await.unwrap_or_default();
        tracing::warn!(%status, body, "the build service failed an upload");
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "the build service could not take the upload; try again shortly",
        ));
    }
    reply.json().await.map(Json).map_err(|error| {
        tracing::warn!(%error, "the build service's answer to an upload could not be read");
        fail(
            StatusCode::BAD_GATEWAY,
            "the build service's answer could not be read; try again shortly",
        )
    })
}

/// What an upload is told where no build service is attached.
const NO_UPLOADS: &str = "this platform takes no uploads here: no build service is attached to it";

/// An executor's report on how a strategy is running.
/// Take a health report, but only from a key the registry blessed for that
/// vault. Anyone may read a report; only the instance running the strategy may
/// write one, or the page would be repeating whatever it was told.
async fn post_health(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    Json(post): Json<HealthPost>,
) -> StatusCode {
    service.indexer.take_health(chain, post).await.status()
}

/// The latest report taken for a vault, as its executor posted it, signature
/// and all, so whoever reads it can check the signer and the time for
/// themselves. 404 if none was taken since this API started.
async fn signed_health(
    State(service): State<Arc<Service>>,
    UrlPath((chain, vault)): UrlPath<(Chain, String)>,
) -> Result<Json<HealthPost>, (StatusCode, Json<shared::ApiError>)> {
    let refused = |status: StatusCode, message: &str| {
        (
            status,
            Json(shared::ApiError {
                message: message.to_string(),
            }),
        )
    };
    let vault = match served(&service, chain)? {
        On::Evm(_) => vault
            .parse::<Address>()
            .ok()
            .map(|vault| shared::ChainAddress::Evm(vault.0.0)),
        On::Solana(_) => vault
            .parse::<chain::solana::Pubkey>()
            .ok()
            .map(|vault| shared::ChainAddress::Solana(vault.to_bytes())),
    }
    .ok_or_else(|| refused(StatusCode::BAD_REQUEST, "that is not an address"))?;
    service
        .indexer
        .signed_health(chain, vault)
        .await
        .map(Json)
        .ok_or_else(|| {
            refused(
                StatusCode::NOT_FOUND,
                "no report has been taken for this vault",
            )
        })
}

/// A vault's running usage total, as its executor signed it: what an author
/// withdrawing from the tank settles first. Carried, not checked: the vault's
/// contract checks the signature. The vault is read as an address, so what
/// is asked of the executor is only ever a path of its own.
async fn usage(
    State(service): State<Arc<Service>>,
    UrlPath((chain, vault)): UrlPath<(Chain, String)>,
) -> impl IntoResponse {
    let on = match served(&service, chain) {
        Ok(on) => on,
        Err(refused) => return refused.into_response(),
    };
    // The vault is read as an address of its chain's kind and written back
    // as one, so what is asked of the executor is only ever a path of its
    // own.
    let url = match on {
        On::Evm(served) => match vault.parse::<Address>() {
            Ok(vault) => format!(
                "{}/usage/{}/{}",
                service.executor, served.deployments.chain_id, vault
            ),
            Err(_) => return (StatusCode::BAD_REQUEST, "that is not an address").into_response(),
        },
        On::Solana(served) => match vault.parse::<chain::solana::Pubkey>() {
            Ok(vault) => format!(
                "{}/usage/{}/{}",
                service.executor, served.client.cluster, vault
            ),
            Err(_) => return (StatusCode::BAD_REQUEST, "that is not an address").into_response(),
        },
    };
    ask_executor(&service.http, &url).await
}

/// What the executor answered for a vault's total, passed on. A 404 is the
/// executor serving no total for the vault; an executor that can't be
/// reached, or answers anything else, is unavailable (502), so the withdraw
/// screen can say so rather than that there is no total.
async fn ask_executor(http: &reqwest::Client, url: &str) -> axum::response::Response {
    let unavailable = || (StatusCode::BAD_GATEWAY, "the executor is unavailable").into_response();
    match http.get(url).send().await {
        Ok(reply) if reply.status().is_success() => match reply.text().await {
            Ok(body) => (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            Err(_) => unavailable(),
        },
        Ok(reply) if reply.status() == reqwest::StatusCode::NOT_FOUND => (
            StatusCode::NOT_FOUND,
            "the executor serves no total for this vault",
        )
            .into_response(),
        Ok(_) | Err(_) => unavailable(),
    }
}

/// Anything the store holds, to whoever has its hash.
async fn store_file(
    State(service): State<Arc<Service>>,
    UrlPath((hash, name)): UrlPath<(String, String)>,
) -> impl IntoResponse {
    // The store checks these too; a request for anything but a hash and a
    // plain file name is refused before it gets there.
    if !indexer::store::is_hash(&hash) || !indexer::store::is_file_name(&name) {
        return (StatusCode::BAD_REQUEST, "no").into_response();
    }
    // Sealed source is kept so the reviewer can read it and the build can be
    // repeated, and it is never served, however its name is spelt. The store
    // serves only the names meant to be public, and a sealed strategy's
    // bundle only to the attested roles.
    if indexer::store::is_sealed(&name) {
        return (StatusCode::FORBIDDEN, "this source is sealed").into_response();
    }
    match service.indexer.store.read_path(&hash, &name) {
        Some((bytes, kind)) => ([(header::CONTENT_TYPE, kind)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "nothing here").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An executor answering every vault's total with `status`.
    async fn an_executor_answering(status: StatusCode) -> String {
        let app = axum::Router::new().route(
            "/usage/{chain}/{vault}",
            get(move || async move {
                if status.is_success() {
                    (status, r#"{"total": "1000"}"#).into_response()
                } else {
                    status.into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn asked(executor: &str) -> StatusCode {
        let http = reqwest::Client::new();
        ask_executor(&http, &format!("{executor}/usage/31337/0x11"))
            .await
            .status()
    }

    #[tokio::test]
    async fn an_executor_that_is_unavailable_is_not_passed_on_as_no_total() {
        for down in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::BAD_GATEWAY,
        ] {
            let executor = an_executor_answering(down).await;
            assert_eq!(asked(&executor).await, StatusCode::BAD_GATEWAY, "{down}");
        }
        // Nothing listens there at all.
        assert_eq!(asked("http://127.0.0.1:1").await, StatusCode::BAD_GATEWAY);

        // A vault the executor serves no total for is still a 404, and one it
        // serves is passed on.
        let executor = an_executor_answering(StatusCode::NOT_FOUND).await;
        assert_eq!(asked(&executor).await, StatusCode::NOT_FOUND);
        let executor = an_executor_answering(StatusCode::OK).await;
        assert_eq!(asked(&executor).await, StatusCode::OK);
    }

    /// A server that notes every request's path and body and answers each
    /// with `reply`.
    async fn a_recording_server(
        reply: fn(&str) -> (StatusCode, String),
    ) -> (String, Arc<std::sync::Mutex<Vec<(String, String)>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let noted = seen.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri, body: String| {
            let noted = noted.clone();
            async move {
                let answer = reply(&body);
                noted.lock().unwrap().push((uri.path().to_string(), body));
                (
                    answer.0,
                    [(header::CONTENT_TYPE, "application/json")],
                    answer.1,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn a_service(builder: String) -> Arc<Service> {
        let store = std::env::temp_dir().join(format!("shonin-api-store-{}", std::process::id()));
        Arc::new(Service {
            indexer: Arc::new(Indexer::new(Vec::new(), store, None)),
            builder: Some(builder),
            executor: "http://127.0.0.1:1".to_string(),
            http: indexer::http(),
            upload_timeout: std::time::Duration::from_secs(3),
        })
    }

    #[tokio::test]
    async fn a_job_id_is_passed_to_the_build_service_only_as_plain_digits() {
        let (builder, seen) = a_recording_server(|_| (StatusCode::NOT_FOUND, String::new())).await;
        let service = a_service(builder);
        for id in [
            "",
            "../health",
            "..",
            "8800/../../jobs",
            "8800?all=1",
            "8800#x",
            "-1",
            "+8800",
            " 8800",
            "123456789012345678901",
        ] {
            let answer = job(State(service.clone()), UrlPath(id.to_string())).await;
            assert_eq!(answer.err(), Some(StatusCode::BAD_REQUEST), "{id:?}");
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );

        let answer = job(State(service.clone()), UrlPath("8800".to_string())).await;
        assert_eq!(answer.err(), Some(StatusCode::NOT_FOUND));
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(paths, ["/jobs/8800"]);
    }

    /// An answer to the JSON-RPC request in `body`, as a node with no logs
    /// on chain 31337 would give it.
    fn a_node_with_no_logs(body: &str) -> (StatusCode, String) {
        let id = body
            .split("\"id\":")
            .nth(1)
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "0".to_string());
        let result = if body.contains("eth_chainId") {
            "\"0x7a69\""
        } else {
            "[]"
        };
        (
            StatusCode::OK,
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
        )
    }

    #[tokio::test]
    async fn a_holding_s_cost_is_read_from_the_deployment_not_from_the_first_block() {
        let (node, seen) = a_recording_server(a_node_with_no_logs).await;
        let config = chain::ChainConfig {
            label: "A node".to_string(),
            rpc: node.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: None,
            confirmations: None,
            public_rpc: node,
            logs_rpc: None,
            log_range: None,
        };
        let client = Client::connect(&config, chain::Signing::ReadOnly)
            .await
            .unwrap();
        let cost = cost_basis(
            &client,
            Address::repeat_byte(0x11),
            Address::repeat_byte(0x22),
            1234,
        )
        .await;
        assert_eq!(cost, Some(0.0));
        let asked: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, body)| body.contains("eth_getLogs"))
            .map(|(_, body)| body.clone())
            .collect();
        assert_eq!(asked.len(), 7, "{asked:?}");
        for body in asked {
            assert!(body.contains(r#""fromBlock":"0x4d2""#), "{body}");
        }
    }
    // A vault's log, as a node holding it answers for it ------------------

    /// A node on chain 31337 holding `logs`, answering a log request with
    /// those that match its address and topics, as a node would.
    async fn a_node_holding(logs: Vec<alloy::rpc::types::Log>) -> String {
        let logs: Vec<serde_json::Value> = logs
            .iter()
            .map(|log| serde_json::to_value(log).unwrap())
            .collect();
        let app = axum::Router::new().fallback(move |body: String| {
            let logs = logs.clone();
            async move {
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let result = match request["method"].as_str() {
                    Some("eth_chainId") => serde_json::json!("0x7a69"),
                    Some("eth_getLogs") => {
                        let filter = &request["params"][0];
                        serde_json::Value::Array(
                            logs.into_iter()
                                .filter(|log| matches(filter, log))
                                .collect(),
                        )
                    }
                    _ => serde_json::json!([]),
                };
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                        .to_string(),
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// Whether a log matches a log request's address and topics.
    fn matches(filter: &serde_json::Value, log: &serde_json::Value) -> bool {
        let same = |a: &serde_json::Value, b: &serde_json::Value| {
            a.as_str().map(str::to_lowercase) == b.as_str().map(str::to_lowercase)
        };
        let one_of = |wanted: &serde_json::Value, value: &serde_json::Value| match wanted {
            serde_json::Value::Null => true,
            serde_json::Value::Array(any) => any.iter().any(|w| same(w, value)),
            _ => same(wanted, value),
        };
        if !one_of(&filter["address"], &log["address"]) {
            return false;
        }
        let topics = log["topics"].as_array().unwrap();
        filter["topics"]
            .as_array()
            .map(|wanted| {
                wanted.iter().enumerate().all(|(i, wanted)| {
                    wanted.is_null() || topics.get(i).is_some_and(|topic| one_of(wanted, topic))
                })
            })
            .unwrap_or(true)
    }

    const VAULT: Address = Address::repeat_byte(0x11);
    const HOLDER: Address = Address::repeat_byte(0x22);
    const SOMEONE_ELSE: Address = Address::repeat_byte(0x33);

    /// One of the vault's events, in transaction `tx` at `index`.
    fn logged(
        event: impl alloy::sol_types::SolEvent,
        tx: u8,
        index: u64,
    ) -> alloy::rpc::types::Log {
        alloy::rpc::types::Log {
            inner: alloy::primitives::Log {
                address: VAULT,
                data: event.encode_log_data(),
            },
            block_hash: Some(alloy::primitives::B256::repeat_byte(tx)),
            block_number: Some(tx as u64),
            block_timestamp: None,
            transaction_hash: Some(alloy::primitives::B256::repeat_byte(tx)),
            transaction_index: Some(0),
            log_index: Some(index),
            removed: false,
        }
    }

    fn usd(dollars: u64) -> alloy::primitives::U256 {
        alloy::primitives::U256::from(dollars * 1_000_000)
    }

    fn shares(count: u64) -> alloy::primitives::U256 {
        alloy::primitives::U256::from(count) * alloy::primitives::U256::from(10u64.pow(18))
    }

    fn deposited(owner: Address, dollars: u64, tx: u8) -> alloy::rpc::types::Log {
        use chain::bindings::vault::IVault;
        logged(
            IVault::Deposited {
                requestId: alloy::primitives::U256::from(tx),
                owner,
                assets: usd(dollars),
                shares: shares(dollars),
            },
            tx,
            0,
        )
    }

    async fn cost_of(logs: Vec<alloy::rpc::types::Log>, who: Address) -> Option<f64> {
        let node = a_node_holding(logs).await;
        let config = chain::ChainConfig {
            label: "A node".to_string(),
            rpc: node.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: None,
            confirmations: None,
            public_rpc: node,
            logs_rpc: None,
            log_range: None,
        };
        let client = Client::connect(&config, chain::Signing::ReadOnly)
            .await
            .unwrap();
        cost_basis(&client, VAULT, who, 0).await
    }

    #[tokio::test]
    async fn a_claim_paid_once_the_vault_closed_comes_off_its_owner_s_cost_once() {
        use alloy::primitives::U256;
        use chain::bindings::vault::IVault;

        let logs = vec![
            deposited(HOLDER, 100, 1),
            deposited(SOMEONE_ELSE, 50, 2),
            // A claim paid while the vault ran: its `Withdrawn` and its
            // `ClaimPaid`, one after the other.
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(8),
                    owner: HOLDER,
                    shares: shares(20),
                },
                3,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::from(4),
                    owner: HOLDER,
                    shares: shares(20),
                    assets: usd(20),
                    feeShares: U256::ZERO,
                },
                4,
                3,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(8),
                    assets: usd(20),
                },
                4,
                4,
            ),
            // Two claims paid once the vault closed (`claimClosed`): a burn,
            // then the `ClaimPaid` alone.
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(9),
                    owner: HOLDER,
                    shares: shares(30),
                },
                5,
                0,
            ),
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(10),
                    owner: SOMEONE_ELSE,
                    shares: shares(10),
                },
                5,
                1,
            ),
            logged(
                IVault::Transfer {
                    from: VAULT,
                    to: Address::ZERO,
                    value: shares(30),
                },
                6,
                0,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(9),
                    assets: usd(30),
                },
                6,
                2,
            ),
            logged(
                IVault::Transfer {
                    from: VAULT,
                    to: Address::ZERO,
                    value: shares(10),
                },
                7,
                0,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(10),
                    assets: usd(10),
                },
                7,
                2,
            ),
        ];
        assert_eq!(cost_of(logs.clone(), HOLDER).await, Some(50.0));
        assert_eq!(cost_of(logs, SOMEONE_ELSE).await, Some(40.0));
    }

    #[tokio::test]
    async fn a_closed_vault_s_redemption_comes_off_the_holder_s_cost_not_the_receiver_s() {
        use alloy::primitives::U256;
        use chain::bindings::vault::IVault;

        let logs = vec![
            deposited(HOLDER, 100, 1),
            deposited(SOMEONE_ELSE, 50, 2),
            // `redeemClosed(5 shares, someone else)`, sent by the holder: the
            // holder's shares burnt, the dollars to the receiver.
            logged(
                IVault::Transfer {
                    from: HOLDER,
                    to: Address::ZERO,
                    value: shares(5),
                },
                3,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::ZERO,
                    owner: SOMEONE_ELSE,
                    shares: shares(5),
                    assets: usd(40),
                    feeShares: U256::ZERO,
                },
                3,
                2,
            ),
            // And one the receiver redeems for themselves.
            logged(
                IVault::Transfer {
                    from: SOMEONE_ELSE,
                    to: Address::ZERO,
                    value: shares(1),
                },
                4,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::ZERO,
                    owner: SOMEONE_ELSE,
                    shares: shares(1),
                    assets: usd(8),
                    feeShares: U256::ZERO,
                },
                4,
                2,
            ),
        ];
        assert_eq!(cost_of(logs.clone(), HOLDER).await, Some(60.0));
        assert_eq!(cost_of(logs, SOMEONE_ELSE).await, Some(42.0));
    }

    // The build service, for an upload ------------------------------------

    /// A build service that takes every connection and never says a word.
    async fn a_builder_that_never_answers() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                if let Ok((socket, _)) = listener.accept().await {
                    held.push(socket);
                }
            }
        });
        format!("http://{address}")
    }

    fn deployments() -> chain::Deployments {
        let one = "0x0000000000000000000000000000000000000001";
        let mut fields = vec![
            r#""chain_key":"evm:31337""#.to_string(),
            r#""chain_id":31337"#.to_string(),
            r#""rpc_url":"""#.to_string(),
            r#""deployed_block":0"#.to_string(),
        ];
        for name in [
            "usd",
            "venue",
            "attestation_verifier",
            "attestation",
            "publishing",
            "prices",
            "strategies",
            "reviews",
            "profiles",
            "factory",
            "station_factory",
            "driver",
            "bridge",
        ] {
            fields.push(format!(r#""{name}":"{one}""#));
        }
        let roles: Vec<String> = [
            "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper",
            "operator", "author", "viewer",
        ]
        .iter()
        .map(|role| format!(r#""{role}":"{one}""#))
        .collect();
        fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
        chain::Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
    }

    /// A service reading base from a node with no logs, passing uploads to
    /// `builder`.
    async fn a_service_on_base(builder: String) -> Arc<Service> {
        let node = a_node_holding(Vec::new()).await;
        let config = chain::ChainConfig {
            label: "A node".to_string(),
            rpc: node.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: None,
            confirmations: None,
            public_rpc: node,
            logs_rpc: None,
            log_range: None,
        };
        let client = Client::connect(&config, chain::Signing::ReadOnly)
            .await
            .unwrap();
        let service = a_service(builder);
        service
            .indexer
            .add(Served {
                vault_chain: Chain::Base,
                key: "evm:31337".parse().unwrap(),
                client,
                deployments: deployments(),
                native_usd: None,
                public_rpc: String::new(),
            })
            .await;
        service
    }

    fn an_upload() -> Upload {
        Upload {
            author: format!("{HOLDER:?}"),
            archive_base64: String::new(),
            sealed: false,
            package: None,
        }
    }

    /// The same service with no build service attached, as in the cloud.
    fn without_a_builder(service: Arc<Service>) -> Arc<Service> {
        Arc::new(Service {
            indexer: service.indexer.clone(),
            builder: None,
            executor: service.executor.clone(),
            http: indexer::http(),
            upload_timeout: service.upload_timeout,
        })
    }

    /// With no build service attached, an upload is refused at once and
    /// says why, and a job is one there isn't.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_no_build_service_an_upload_is_refused_at_once_and_says_why() {
        let service = without_a_builder(a_service_on_base(String::new()).await);
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            uploads(
                State(service.clone()),
                UrlPath(Chain::Base),
                Ok(Json(an_upload())),
            ),
        )
        .await
        .expect("an upload with nowhere to go was answered at once");
        let (status, Json(said)) = answer.expect_err("the upload is refused");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(said.message, NO_UPLOADS);
        let answer = job(State(service), UrlPath("8800".to_string())).await;
        assert_eq!(answer.err(), Some(StatusCode::NOT_FOUND));
    }

    /// A vault with no report taken has no signed report to give, and what
    /// isn't an address is refused before anything is looked up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vault_with_no_report_taken_has_no_signed_report() {
        let service = a_service_on_base(String::new()).await;
        let answer = signed_health(
            State(service.clone()),
            UrlPath((Chain::Base, format!("{VAULT:?}"))),
        )
        .await;
        assert_eq!(
            answer.err().map(|(status, _)| status),
            Some(StatusCode::NOT_FOUND)
        );
        let answer = signed_health(
            State(service),
            UrlPath((Chain::Base, "../../health".to_string())),
        )
        .await;
        assert_eq!(
            answer.err().map(|(status, _)| status),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_upload_to_a_build_service_that_never_answers_is_given_up_on() {
        let service = a_service_on_base(a_builder_that_never_answers().await).await;
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            uploads(State(service), UrlPath(Chain::Base), Ok(Json(an_upload()))),
        )
        .await
        .expect("the upload was still waiting on the build service after 45 s");
        assert_eq!(
            answer.err().map(|(status, _)| status),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
    }

    /// Whatever goes wrong between the API and the build service, the caller
    /// is told in general terms: nothing names where the build service
    /// answers or what it said about itself. Its refusal of the upload
    /// itself is the author's to read, and is passed on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_upload_that_goes_wrong_says_nothing_of_the_services_behind_the_api() {
        let internal = "internal-builder.example";
        let (failing, _) = a_recording_server(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "panicked at http://internal-builder.example/src/main.rs".to_string(),
            )
        })
        .await;
        let (garbled, _) = a_recording_server(|_| (StatusCode::OK, "not json".to_string())).await;
        // Nothing listens on the first port.
        let unreachable = "http://127.0.0.1:1".to_string();
        for builder in [unreachable.clone(), failing, garbled] {
            let service = a_service_on_base(builder.clone()).await;
            let answer = uploads(State(service), UrlPath(Chain::Base), Ok(Json(an_upload()))).await;
            let (status, Json(said)) = answer.expect_err("the upload is refused");
            assert!(status.is_server_error(), "{builder}: {status}");
            let address = builder.trim_start_matches("http://");
            assert!(
                !said.message.contains(address)
                    && !said.message.contains(internal)
                    && !said.message.contains("127.0.0.1"),
                "{builder}: the caller was told {:?}",
                said.message
            );
        }

        let (refusing, _) = a_recording_server(|_| {
            (
                StatusCode::BAD_REQUEST,
                "shonin.toml puts the vault on robinhood".to_string(),
            )
        })
        .await;
        let service = a_service_on_base(refusing).await;
        let answer = uploads(State(service), UrlPath(Chain::Base), Ok(Json(an_upload()))).await;
        let (status, Json(said)) = answer.expect_err("the upload is refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(said.message, "shonin.toml puts the vault on robinhood");
    }
}
