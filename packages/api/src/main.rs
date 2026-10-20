//! The web app's server.
//!
//! It runs the reference indexer in-process and serves what it holds: one
//! snapshot that carries everything a page renders, the platform's terms as
//! the contracts state them, a depositor's portfolio, the build queue, and
//! the store's files by hash. Submissions pass through to the build service
//! and health reports are taken only from a key blessed for their vault. The
//! root key's status and its published keys are relayed from the executor.
//!
//! On a port of its own, never public ([`internal`]), it keeps what the
//! machines that run on demand keep off themselves, and tells the waker when
//! to start them.

mod internal;
mod rootkey;
mod solana;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use alloy::primitives::Address;
use anyhow::Result;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chain::bindings::erc20::IERC20;
use chain::{Role, Served, SolanaServed};
use clap::Parser;
use indexer::{Indexer, Lookup};
use shared::model::registry::Job;
use shared::{
    Addresses, Chain, ChainInfo, Config, DevMode, EvmAddresses, HealthPost, HeldClaim, Holding,
    Portfolio, Snapshot, Upload, UploadAccepted,
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
    #[arg(long, env = "SHONIN_BUILDER_URL")]
    builder_url: Option<String>,
    /// Where the executor serves each vault's signed usage total.
    #[arg(long, env = "SHONIN_EXECUTOR_URL")]
    executor_url: String,
    #[arg(long, default_value_t = 19_081)]
    port: u16,
    /// The address to listen on: 0.0.0.0 in a container.
    #[arg(long, default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
    /// The internal port, for the machines that run on demand: their state,
    /// their built files, and when to start them. Never carried to the
    /// public name. Without it there is none.
    #[arg(long, env = "SHONIN_INTERNAL_PORT")]
    internal_port: Option<u16>,
    /// The address the internal port listens on: 0.0.0.0 on the services
    /// machine, where the cloud's firewall lets only those machines reach it.
    #[arg(long, env = "SHONIN_INTERNAL_LISTEN", default_value = "127.0.0.1")]
    internal_listen: std::net::IpAddr,
    /// Where the API keeps what is its own: the machines' state, their wake
    /// plans, and the build queue as last given. By default `api-state`
    /// beside the store.
    #[arg(long, env = "SHONIN_API_STATE")]
    state: Option<PathBuf>,
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
    /// The published root keys' relays to the executor.
    rootkey: rootkey::Relays,
    /// The asks to start the machines that run on demand.
    wake: Arc<internal::Wake>,
}

#[tokio::main]
async fn main() -> Result<()> {
    chain::rpc_counts::start(
        chain::rpc_counts::Component::Api,
        format!(
            "{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            std::process::id()
        ),
    );
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let mut args = Args::parse();
    // An image built without a build service names it empty.
    args.builder_url = args.builder_url.filter(|url| !url.is_empty());
    let state = args.state.clone().unwrap_or_else(|| {
        args.store
            .parent()
            .map(|parent| parent.join("api-state"))
            .unwrap_or_else(|| PathBuf::from("api-state"))
    });
    // The port is bound at once, and each chain is served from the moment
    // it connects: one chain out of reach holds up none of the others, and
    // is answered for as not connected yet.
    let connections = indexer::connections(&args.chain_args)?;
    let mut indexer = Indexer::new(
        connections.expected.clone(),
        args.store.clone(),
        args.builder_url.clone(),
    )
    .with_service_rpc_policy(connections.service_rpc_policy.clone())?
    .with_chains(connections.chains.clone());
    if let Some(url) = args.store_url.as_deref().filter(|url| !url.is_empty()) {
        indexer = indexer.with_store_url(url);
    }
    if args.builder_url.is_some() {
        indexer = indexer.with_jobs_cache(state.join("jobs-cache.json"));
    }
    let indexer = Arc::new(indexer);
    indexer.take(connections);
    if args.builder_url.is_none() {
        tracing::info!("no build service is attached: no queue, and uploads are refused");
    }
    let reader = indexer.clone();
    tokio::spawn(async move { reader.run().await });

    let wake = Arc::new(internal::Wake::new(Some(state.join("plans"))));
    if let Some(port) = args.internal_port {
        // The registry that blesses the machines' keys: the network's
        // canonical chain.
        let canonical = chain::ChainConfigs::load(&args.chain_args.chains)?
            .network(args.chain_args.network)
            .and_then(|network| network.root())
            .map(|(chain, _)| chain)
            .unwrap_or(Chain::Base);
        let reads = Arc::new(internal::ChainReads {
            indexer: indexer.clone(),
            canonical,
        });
        let internal = Arc::new(internal::Internal::new(
            indexer.clone(),
            reads,
            wake.clone(),
            state.join("blobs"),
        ));
        let address = SocketAddr::from((args.internal_listen, port));
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(%address, "the internal port is open");
        let app = internal::routes(internal);
        tokio::spawn(async move {
            if let Err(error) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!(%error, "the internal port stopped");
            }
        });
    }

    let service = Arc::new(Service {
        indexer,
        builder: args.builder_url.clone(),
        executor: args.executor_url.clone(),
        http: indexer::http(),
        upload_timeout: UPLOAD_TIMEOUT,
        rootkey: rootkey::Relays::default(),
        wake,
    });

    let app = axum::Router::new()
        .route("/health", get(health))
        .route(
            "/debug/rpc",
            get(|| async { axum::Json(chain::rpc_counts::snapshot()) }),
        )
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

/// The chain's config for the app: where to reach it, the platform's
/// addresses on it, and the terms the platform runs under there, as the
/// record last read them (in the background, so the route never waits on
/// the chain), with when they were read.
async fn config(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
) -> Result<Json<Config>, (StatusCode, Json<shared::ApiError>)> {
    let on = served(&service, chain)?;
    // An app told the wrong terms would state them as facts, so it is told
    // nothing until the chain has answered for them once.
    let (terms, terms_as_of) = service.indexer.terms(chain).await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(shared::ApiError {
                message: format!(
                    "{} has not answered for the platform's terms yet",
                    chain.label()
                ),
            }),
        )
    })?;
    let served = match on {
        On::Evm(served) => served,
        On::Solana(served) => {
            return Ok(Json(
                solana::config(&service, chain, &served, terms, terms_as_of).await,
            ));
        }
    };
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
            catching_up: false,
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
        terms_as_of: Some(terms_as_of),
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
    // A balance that could not be read is not a balance of nothing: the
    // portfolio is not answered rather than shown empty.
    let unread = |error: anyhow::Error| {
        tracing::warn!(%chain, error = format!("{error:#}"), "a portfolio balance could not be read");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(shared::ApiError {
                message: format!("{} did not answer", chain.label()),
            }),
        )
    };

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
    let vaults: Vec<Option<Address>> = strategies
        .iter()
        .map(|strategy| strategy.vault.address.parse::<Address>().ok())
        .collect();
    // Every balance in one observation, at the block the record was read at:
    // the shares are then counted at the share price and against the cost
    // the record has as of that block. Before the record has been read, at
    // the head.
    let mut plan = chain::follow::Plan::default();
    plan.read(
        served.deployments.usd,
        &IERC20::balanceOfCall { account: who },
    );
    for vault in vaults.iter().flatten() {
        plan.read(*vault, &IERC20::balanceOfCall { account: who });
    }
    let head = service.indexer.record_head(chain).await;
    let pinned = match head {
        Some(head) => service
            .indexer
            .observe(chain, &plan, chain::follow::At::Pinned(head))
            .await
            .inspect_err(|error| tracing::debug!(%chain, error = format!("{error:#}"), "the balances could not be read at the record's block; read at the head"))
            .ok(),
        None => None,
    };
    // Read at the head instead, the costs are unknown: they are as of the
    // record's block, the balances are not.
    let costs_apply = pinned.is_some();
    let observed = match pinned {
        Some(observed) => observed,
        None => service
            .indexer
            .observe(chain, &plan, chain::follow::At::Latest)
            .await
            .map_err(unread)?,
    };
    let mut balances = Vec::with_capacity(plan.reads.len());
    for index in 0..plan.reads.len() {
        balances.push(
            observed
                .decode::<IERC20::balanceOfCall>(index)
                .map_err(unread)?,
        );
    }
    let mut balances = balances.into_iter();
    let balance = balances
        .next()
        .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6)
        .unwrap_or(0.0);
    let owner = shared::ChainAddress::Evm(who.0.0);
    let mut held: Vec<Option<(f64, Option<f64>)>> = Vec::with_capacity(vaults.len());
    for vault in &vaults {
        let Some(vault) = vault else {
            held.push(None);
            continue;
        };
        let shares = balances
            .next()
            .map(|value| value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18)
            .unwrap_or(0.0);
        // What it paid, from the record's own reading of the vault's
        // history; unknown, never nothing, while that isn't read through.
        let cost = if !costs_apply {
            None
        } else if shares > 0.0 {
            service
                .indexer
                .cost(chain, &shared::ChainAddress::Evm(vault.0.0), &owner)
                .await
        } else {
            Some(0.0)
        };
        held.push(Some((shares, cost)));
    }

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

async fn jobs(State(service): State<Arc<Service>>) -> Json<Vec<Job>> {
    Json(service.indexer.snapshot.read().await.jobs.clone())
}

/// One job, as fresh as the build service has it, with what the chain adds.
/// 404 only when the build service says there is no such job; 503 when it
/// says it can't answer for now; 502 when it answers anything else. A build
/// service that can't be reached (it is off until there is work) is answered
/// for from the queue as it last gave it, and 503 for a job not in it.
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
        .await;
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            tracing::debug!(%error, "the build service could not be asked for a job");
            return last_known(&service, &id).await;
        }
    };
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

/// A job as the queue was last given, for when the build service is off.
async fn last_known(service: &Service, id: &str) -> Result<Json<Job>, StatusCode> {
    service
        .indexer
        .snapshot
        .read()
        .await
        .jobs
        .iter()
        .find(|job| job.id == id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// A job's id as the build service gives it: a number, in plain digits.
fn is_job_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// How long an upload may take to reach the build service and be taken:
/// it is unpacked and checked there before it is answered, so well past the
/// timeout of any other ask.
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a caller is told to wait before trying an upload again while
/// the build service starts.
const RETRY_AFTER_SECONDS: u64 = 30;

/// A refusal: its status, what the caller is told, and when to try again if
/// it should.
#[derive(Debug)]
struct Refused {
    status: StatusCode,
    message: String,
    retry_after: Option<u64>,
}

impl IntoResponse for Refused {
    fn into_response(self) -> axum::response::Response {
        let body = Json(shared::ApiError {
            message: self.message,
        });
        match self.retry_after {
            Some(seconds) => (
                self.status,
                [(header::RETRY_AFTER, seconds.to_string())],
                body,
            )
                .into_response(),
            None => (self.status, body).into_response(),
        }
    }
}

impl From<(StatusCode, Json<shared::ApiError>)> for Refused {
    fn from((status, Json(error)): (StatusCode, Json<shared::ApiError>)) -> Refused {
        Refused {
            status,
            message: error.message,
            retry_after: None,
        }
    }
}

/// Uploads go straight to the build service; the API only carries them. The
/// build service's refusal of an upload (a manifest it won't take, source
/// over the limit) is the author's to read and is passed on; anything else
/// that goes wrong is logged here and answered in general terms, so nothing
/// about the services behind the API reaches a caller.
///
/// The build service runs only while there is work: one that can't be
/// reached is asked to be started (the waker reads the ask), and the caller
/// is told at once to try again shortly (503, `Retry-After`), never held
/// while it starts.
async fn uploads(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    upload: Result<Json<Upload>, JsonRejection>,
) -> Result<Json<UploadAccepted>, Refused> {
    served(&service, chain)?;
    let fail = |status: StatusCode, message: &str| Refused {
        status,
        message: message.to_string(),
        retry_after: None,
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
            tracing::info!(%error, "an upload could not be passed to the build service: asking for it to be started");
            service.wake.ask(internal::Machine::Builder);
            Refused {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: STARTING.to_string(),
                retry_after: Some(RETRY_AFTER_SECONDS),
            }
        })?;
    let status = reply.status();
    if status.is_client_error() {
        let body = reply.text().await.unwrap_or_default();
        return Err(fail(status, &body));
    }
    if status == StatusCode::SERVICE_UNAVAILABLE {
        // Up, and not taking uploads yet (its key not blessed yet, say).
        return Err(Refused {
            status,
            message: STARTING.to_string(),
            retry_after: Some(RETRY_AFTER_SECONDS),
        });
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

/// What an upload is told while the build service starts.
const STARTING: &str = "the build service is starting; try again shortly";

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
#[path = "../../indexer/tests/stand_in/mod.rs"]
mod stand_in;

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
            rootkey: rootkey::Relays::default(),
            wake: Arc::new(internal::Wake::new(None)),
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
        let client = chain::Client::connect(&config, chain::Signing::ReadOnly)
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
            rootkey: rootkey::Relays::default(),
            wake: service.wake.clone(),
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
        let said = answer.expect_err("the upload is refused");
        assert_eq!(said.status, StatusCode::SERVICE_UNAVAILABLE);
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
    async fn an_upload_to_a_build_service_that_is_off_answers_503_at_once_and_asks_for_it() {
        // Nothing listens there: the build service's machine is off.
        let service = a_service_on_base("http://127.0.0.1:1".to_string()).await;
        let plan = |service: &Service| {
            internal::wanted(
                service.wake.asks_builder(),
                None,
                0,
                Some(&Default::default()),
            )
        };
        assert!(!plan(&service), "nobody asked for it yet");
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            uploads(
                State(service.clone()),
                UrlPath(Chain::Base),
                Ok(Json(an_upload())),
            ),
        )
        .await
        .expect("the upload was answered at once, not held while the build service starts");
        let refused = answer.expect_err("the upload is refused for now");
        assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.retry_after, Some(RETRY_AFTER_SECONDS));
        assert_eq!(refused.message, STARTING);
        let answer = refused.into_response();
        assert_eq!(
            answer.headers().get(header::RETRY_AFTER).unwrap(),
            &RETRY_AFTER_SECONDS.to_string()
        );
        assert!(
            plan(&service),
            "the waker is asked to start the build service"
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
            answer.err().map(|refused| refused.status),
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
            let said = answer.expect_err("the upload is refused");
            let status = said.status;
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
        let said = answer.expect_err("the upload is refused");
        assert_eq!(said.status, StatusCode::BAD_REQUEST);
        assert_eq!(said.message, "shonin.toml puts the vault on robinhood");
    }

    // What a holder paid, from the record -----------------------------------

    /// What a holder paid for a holding is read from the record's own
    /// reading of the vault's history, paged as narrowly as the endpoint
    /// asks (Base's caps a log query at 200 blocks), with no log read of its
    /// own: until that history has been read up to the head, it is unknown,
    /// never shown as nothing paid.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_holding_s_cost_is_unknown_until_its_vault_s_history_is_read_through() {
        use std::sync::atomic::{AtomicU64, Ordering};

        use chain::bindings::vault::IVault;
        use serde_json::{Value, json};

        const HEAD: u64 = 60_010;
        const DEPOSITED_IN: u64 = 55_000;
        let holder = Address::repeat_byte(0xd0);
        let the_deposit = stand_in::logged(
            IVault::Deposited {
                requestId: alloy::primitives::U256::from(1),
                owner: holder,
                assets: alloy::primitives::U256::from(100_000_000u64),
                shares: alloy::primitives::U256::from(10u64)
                    * alloy::primitives::U256::from(10u64.pow(18)),
            },
            DEPOSITED_IN,
            0,
        );
        let log_reads = Arc::new(AtomicU64::new(0));
        let counted = log_reads.clone();
        let answer: stand_in::Answer = Arc::new(move |request: &Value| {
            match request["method"].as_str() {
                Some("eth_getLogs") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let (from, to) = stand_in::log_range(request);
                    Some(if to - from >= 200 {
                        Err("block range too large, max is 200".to_string())
                    } else {
                        Ok(json!(stand_in::matching(
                            request,
                            std::slice::from_ref(&the_deposit)
                        )))
                    })
                }
                Some("eth_call")
                    if stand_in::input(request)
                        .starts_with(&stand_in::selector::<IERC20::balanceOfCall>()) =>
                {
                    let to = request["params"][0]["to"].as_str().unwrap_or_default();
                    // Ten shares of the vault, and five dollars.
                    let held: u128 = if to.to_ascii_lowercase().ends_with(stand_in::VAULT) {
                        10 * 10u128.pow(18)
                    } else {
                        5_000_000
                    };
                    Some(Ok(json!(format!("0x{held:064x}"))))
                }
                _ => None,
            }
        });
        let indexer = stand_in::an_indexer_on(
            stand_in::Chain::at(HEAD),
            stand_in::deployments(),
            answer,
            "holding-cost",
        )
        .await;
        let service = Arc::new(Service {
            indexer: indexer.clone(),
            builder: None,
            executor: "http://127.0.0.1:1".to_string(),
            http: indexer::http(),
            upload_timeout: std::time::Duration::from_secs(3),
            rootkey: rootkey::Relays::default(),
            wake: Arc::new(internal::Wake::new(None)),
        });
        let who = format!("{holder:?}");
        let cost = |portfolio: &Portfolio| portfolio.positions.first().map(|held| held.cost);

        // The first refresh reads the history part of the way.
        indexer.refresh().await.unwrap();
        assert!(indexer.snapshot.read().await.chains[0].catching_up);
        let Json(portfolio) = read_portfolio(&service, Chain::Base, who.clone())
            .await
            .unwrap();
        assert_eq!(portfolio.positions.len(), 1);
        assert_eq!(
            cost(&portfolio),
            Some(None),
            "unknown while the history is read"
        );

        // The next reads it to the head, past the deposit.
        indexer.refresh().await.unwrap();
        assert!(!indexer.snapshot.read().await.chains[0].catching_up);
        let before = log_reads.load(Ordering::SeqCst);
        let Json(portfolio) = read_portfolio(&service, Chain::Base, who).await.unwrap();
        assert_eq!(cost(&portfolio), Some(Some(100.0)));
        assert_eq!(
            log_reads.load(Ordering::SeqCst),
            before,
            "a portfolio read logs of its own"
        );
        let json = serde_json::to_value(&portfolio).unwrap();
        assert_eq!(json["positions"][0]["cost"], json!(100.0));
        assert_eq!(json["balance"], json!(5.0));
    }

    /// The platform's terms are served from the record, read with it in the
    /// background and dated by the block they were read at: the config route
    /// reads nothing of the chain, so a slow chain never holds it up, and
    /// before the record has read them it says so rather than guess.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_config_s_terms_are_served_from_the_record_with_their_read_time() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let asked = Arc::new(AtomicU64::new(0));
        let counted = asked.clone();
        let indexer = stand_in::an_indexer_on(
            stand_in::Chain::at(stand_in::HEAD),
            stand_in::deployments(),
            Arc::new(move |_: &serde_json::Value| {
                counted.fetch_add(1, Ordering::SeqCst);
                None
            }),
            "config-terms",
        )
        .await;
        let service = Arc::new(Service {
            indexer: indexer.clone(),
            builder: None,
            executor: "http://127.0.0.1:1".to_string(),
            http: indexer::http(),
            upload_timeout: std::time::Duration::from_secs(3),
            rootkey: rootkey::Relays::default(),
            wake: Arc::new(internal::Wake::new(None)),
        });
        let refused = config(State(service.clone()), UrlPath(Chain::Base)).await;
        assert_eq!(
            refused.err().map(|(status, _)| status),
            Some(StatusCode::SERVICE_UNAVAILABLE),
            "terms never read are not answered"
        );

        indexer.refresh().await.unwrap();
        let before = asked.load(Ordering::SeqCst);
        let Json(config) = config(State(service), UrlPath(Chain::Base)).await.unwrap();
        assert_eq!(
            asked.load(Ordering::SeqCst),
            before,
            "the route read the chain"
        );
        assert_eq!(
            config.terms_as_of,
            Some(indexer::read::moment(stand_in::TIME)),
            "dated by the block the record read them at"
        );
    }
}
