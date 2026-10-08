//! The reviewer.
//!
//! The build service hands it each paid submission before anything is built.
//! It reads the source and the author's description with a pinned model, and
//! rates the code in six areas from a battery of narrow questions. Code found
//! to be malicious is refused, and the refusal is recorded on the chain;
//! anything else gets the reviewer's signed go-ahead, without which the
//! registry takes no build, and its review is posted once the build is
//! registered. The prompt, the schema and the battery are in this repository,
//! so an author can run the same review over their own code first.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chain::bindings::reviews::{IReviewRegistry, Review as SignedReview};
use chain::bindings::strategies::IStrategyRegistry;
use chain::{NetworkName, Served};
use clap::Parser;
use serde::{Deserialize, Serialize};
use shared::manifest::{Manifest, hash_bytes};
use shared::model::review::{Finding, Rating, Report};
use shared::{Chain, ReviewAnswer, ReviewRequest};
use shonin_reviewer::battery::{Answer, Battery};
use shonin_reviewer::{cloud, jev, messages, source};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// The prompt and the schema this image is pinned to. Changing either would
/// be an image change, behind the same timelock as any other.
const PROMPT: &str = include_str!("../review/prompt.md");
const SCHEMA: &str = include_str!("../review/schema.json");

/// The longest the pinned model is given to answer one review. The build
/// service waits 30 minutes for the reviewer before it asks again, so the
/// model is stopped well before that: a model that hangs is answered 503
/// (ask again later), and asking again never piles up model processes.
const MOST_MODEL_SECONDS: u64 = 20 * 60;

/// The environment the pinned model's program is given besides `PATH` and
/// `HOME`, each only if the reviewer has it: the model's own credentials,
/// and the user name the program finds a stored login under. Nothing else
/// of the reviewer's reaches it.
const MODEL_ENVIRONMENT: [&str; 4] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "USER",
    "LOGNAME",
];

#[derive(Parser)]
#[command(about = "Reads and rates an agent's code before it is built")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// The model this image is pinned to. Without it, on a local chain or a
    /// testnet, the reviewer lets everything through and rates every area
    /// "couldn't tell", saying why; on mainnet it refuses to start.
    #[arg(long, env = "SHONIN_REVIEW_MODEL")]
    model: Option<String>,
    /// The key for the model that answers the battery. Without it the pinned
    /// model answers the battery itself.
    #[arg(long, env = "SHONIN_CALIBRATOR_KEY")]
    calibrator_key: Option<String>,
    #[arg(long, env = "SHONIN_CALIBRATOR_URL")]
    calibrator_url: Option<String>,
    #[arg(long, env = "SHONIN_CALIBRATOR_MODEL")]
    calibrator_model: Option<String>,
    #[arg(long, default_value_t = 19_086)]
    port: u16,
    /// The address to listen on: this machine alone unless told otherwise.
    #[arg(long, env = "SHONIN_LISTEN", default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
    /// How the reviewer's key is attested: `development`, by the role key
    /// and the development checker; `launcher`, by Confidential Space's
    /// launcher, with a key derived for this image by the key holder.
    #[arg(long, env = "SHONIN_ATTESTATION", value_enum, default_value_t = Attestation::Development)]
    attestation: Attestation,
    /// The key holder's status port, asked for this image's keys by
    /// attested exchange (launcher mode).
    #[arg(long, env = "SHONIN_HOLDER_URL")]
    holder_url: Option<String>,
    /// Where the reviewer's state is kept off the machine; without it, in
    /// the store as files.
    #[arg(long, env = "SHONIN_STATE_URL")]
    state_url: Option<String>,
    /// Exit once idle this many seconds; without it, never.
    #[arg(long, env = "SHONIN_IDLE_EXIT_SECONDS")]
    idle_exit_seconds: Option<u64>,
    /// The model's key, sealed to the reviewer's model purpose. Opened at
    /// start by attested exchange (launcher mode), and the model is then
    /// asked over its API; without it, the model's program on this machine
    /// answers, as on a laptop.
    #[arg(long, env = "SHONIN_MODEL_KEY_SEALED")]
    model_key_sealed: Option<PathBuf>,
    /// The launcher's token socket.
    #[arg(long, hide = true, default_value = shonin_rootkey::launcher::LAUNCHER_SOCKET)]
    launcher_socket: PathBuf,
    /// How long, in seconds, the pinned model is given to answer one review.
    /// Shortened only by tests that run the reviewer as a process.
    #[arg(long, hide = true, default_value_t = MOST_MODEL_SECONDS)]
    model_seconds: u64,
}

/// How the reviewer's key is attested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Attestation {
    Development,
    Launcher,
}

/// What the pinned model is asked to return.
#[derive(Clone, Serialize, Deserialize)]
struct Prose {
    summary: String,
    findings: Vec<ProseFinding>,
    limits: Vec<String>,
    answers: Vec<Answer>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProseFinding {
    area: shared::Area,
    severity: shared::Severity,
    summary: String,
    detail: String,
    file: String,
    line: u32,
}

/// A review the reviewer gave its go-ahead for, waiting for the build to be
/// registered so it can be posted. Kept on disk, so a restart posts it all
/// the same.
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    chain: Chain,
    strategy_id: u64,
    source_hash: B256,
    report_hash: B256,
    model_id_hash: B256,
    ratings: [u8; 6],
    finding_count: u16,
    /// When it was cleared, in seconds since 1970 (zero for one written
    /// before this was kept).
    #[serde(default)]
    cleared_at: u64,
}

/// How long a cleared review keeps the reviewer from idling while it waits
/// for its build: past this, its build has failed or is long delayed, and
/// the reviewer posts it when it is next up.
const PENDING_KEEPS_AWAKE: u64 = 60 * 60;

struct Reviewer {
    /// Every vault chain it reviews for, connected yet or not.
    configured: Vec<Chain>,
    /// Those of them connected and blessed on so far (see [`take_on`]). A
    /// review asked for on one not yet connected is answered 503, which the
    /// build service takes for "ask again later".
    connected: std::sync::RwLock<BTreeMap<Chain, Arc<Served>>>,
    store: PathBuf,
    model: Option<String>,
    /// The pinned model's API, when the reviewer holds its key; otherwise
    /// the model's program on this machine answers.
    api: Option<messages::Client>,
    /// How long the model is given to answer one review.
    model_wait: Duration,
    calibrator: Option<jev::Client>,
    battery: Battery,
    pending: Mutex<Vec<Pending>>,
    pending_file: PathBuf,
    /// The reviewer's own key, in the attested image; the role key the node
    /// holds otherwise.
    key: Option<PrivateKeySigner>,
    /// The key's attestation, in the attested image.
    attestation: Option<Arc<dyn cloud::Attesting>>,
    /// Where the reviewer's state is kept off the machine, if it is.
    internal: Option<cloud::Internal>,
    /// The chain id of the canonical chain, whose registry the API reads to
    /// count the key blessed: the API takes no write until it is.
    canonical: Option<u64>,
    /// Each signed review request taken so far, by its signer and what it
    /// signed, with the time it was signed at, kept while that time is
    /// within the allowed skew: the same signed request sent again is
    /// refused, however its signature is spelt.
    taken: std::sync::Mutex<BTreeMap<(Address, B256), u64>>,
    /// Reviews being made now.
    in_flight: AtomicUsize,
    /// When the reviewer last had anything to do.
    idle: cloud::Idle,
    /// The work loops' turns, closed before the state is written at exit.
    turns: cloud::Turns,
}

/// Why a chain can't be acted on.
#[derive(Debug, PartialEq, Eq)]
enum Unserved {
    /// It is not one this reviewer reviews for.
    NotServed,
    /// It is, and is not connected yet.
    NotConnected,
}

impl std::fmt::Display for Unserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotServed => "this reviewer does not serve that chain",
            Self::NotConnected => "that chain is not connected yet",
        })
    }
}

impl std::error::Error for Unserved {}

/// A review request that can't be read as one: its archive isn't base64, or
/// holds no manifest at the path named, or no source the reviewer reads.
/// Answered 422. Every other way a review goes unmade is the reviewer's own
/// trouble, answered 503.
#[derive(Debug)]
struct Unreadable(String);

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreadable {}

fn unreadable(error: impl std::fmt::Display) -> anyhow::Error {
    Unreadable(format!("{error:#}")).into()
}

impl Reviewer {
    /// Whether the reviewer has anything to do: a review being made, a
    /// recent go-ahead whose review is not posted yet, or a blessing due.
    async fn busy(&self) -> bool {
        let now = cloud::now();
        self.in_flight.load(Ordering::SeqCst) > 0
            || self
                .pending
                .lock()
                .await
                .iter()
                .any(|review| now.saturating_sub(review.cleared_at) < PENDING_KEEPS_AWAKE)
            || self.attestation.as_ref().is_some_and(|a| a.busy())
    }

    /// The reviewer's address on a chain.
    fn address(&self, on: &Served) -> Address {
        match &self.key {
            Some(key) => key.address(),
            None => on.deployments.roles.reviewer,
        }
    }

    /// Send `call` to `to` on a chain as the reviewer.
    async fn send<C: alloy::sol_types::SolCall>(
        &self,
        on: &Served,
        to: Address,
        call: C,
    ) -> Result<()> {
        match &self.key {
            Some(key) => on.client.with_key(key.clone())?.send(to, call).await?,
            None => {
                on.client
                    .send(on.deployments.roles.reviewer, to, call)
                    .await?
            }
        };
        Ok(())
    }

    /// Whether the API would take the wake plan: in the attested image, once
    /// the key is blessed on the canonical chain; anywhere else, always.
    fn plan_taken(&self) -> bool {
        match (&self.attestation, self.canonical) {
            (Some(attestation), Some(chain_id)) => attestation.ready(chain_id),
            _ => true,
        }
    }

    /// Whether the state kept off the machine can be written, and is the
    /// reviewer's own: the key is blessed, and whatever was found sealed
    /// under another key is set aside. Until then no review is made, since
    /// its go-ahead could not be kept.
    fn keeps_state(&self) -> bool {
        self.internal
            .as_ref()
            .is_none_or(|internal| internal.writable() && internal.settled())
    }

    fn on(&self, chain: Chain) -> Result<Arc<Served>, Unserved> {
        if !self.configured.contains(&chain) {
            return Err(Unserved::NotServed);
        }
        self.connected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&chain)
            .cloned()
            .ok_or(Unserved::NotConnected)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    if args.model.is_none() && args.chain_args.network == NetworkName::Mainnet {
        bail!("a review model is required on mainnet: set SHONIN_REVIEW_MODEL");
    }
    if args.attestation == Attestation::Launcher && args.holder_url.is_none() {
        bail!("launcher mode needs the key holder: set SHONIN_HOLDER_URL");
    }
    // The attested image answers the battery with its pinned model alone:
    // source is sent nowhere the image does not name.
    if args.attestation == Attestation::Launcher
        && (args.calibrator_key.is_some()
            || args.calibrator_url.is_some()
            || args.calibrator_model.is_some())
    {
        bail!(
            "launcher mode takes no calibrator: unset SHONIN_CALIBRATOR_KEY, \
             SHONIN_CALIBRATOR_URL and SHONIN_CALIBRATOR_MODEL"
        );
    }
    let api = match &args.model_key_sealed {
        Some(sealed) => Some(model_api(&args, sealed).await?),
        None => None,
    };
    let launched = match (args.attestation, &args.holder_url) {
        (Attestation::Launcher, Some(holder)) => Some(
            cloud::launch(
                shonin_rootkey::ImageKind::Reviewer,
                holder,
                &args.launcher_socket,
            )
            .await?,
        ),
        _ => None,
    };
    let targets = args.chain_args.targets()?;
    // The network's root chain, or base where it names none, as the API
    // takes it.
    let canonical = args
        .chain_args
        .root_chain()?
        .and_then(|key| key.evm_chain_id())
        .or_else(|| {
            targets
                .iter()
                .find(|target| target.vault_chain == Chain::Base)
                .and_then(|target| target.key.evm_chain_id())
        });
    // In the attested image the API takes the reviewer's writes only once
    // its key is blessed on the canonical chain: none is sent before.
    let internal = match &args.state_url {
        Some(url) => Some(match &launched {
            Some(launched) => cloud::Internal::new(
                url,
                "reviewer",
                cloud::Signer::Key(launched.key.clone()),
                launched.sealing.clone(),
            )?
            .writes_once({
                let attestation = launched.attestation.clone();
                move || canonical.is_none_or(|chain_id| attestation.ready(chain_id))
            }),
            None => {
                cloud::development(url, "reviewer", &args.chain_args, |roles| roles.reviewer)
                    .await?
            }
        }),
        None => None,
    };
    let configured = targets.iter().map(|target| target.vault_chain).collect();
    // Each chain is taken on as soon as it connects: one out of reach (its
    // node down, its deployment missing) is tried again in the background,
    // and keeps the reviewer from no other.
    let mut connecting = args.chain_args.connect_each()?;

    let calibrator = jev::Client::configured(
        args.calibrator_key.clone(),
        args.calibrator_url.clone(),
        args.calibrator_model.clone(),
    );
    match (&args.model, &calibrator) {
        (Some(model), _) if api.is_some() => tracing::info!(
            model,
            "the reviewer is up; the model is asked over its API with the key it holds"
        ),
        (Some(model), Some(client)) => {
            tracing::info!(model, calibrator = client.model(), "the reviewer is up")
        }
        (Some(model), None) => tracing::info!(
            model,
            "the reviewer is up; the model answers the battery itself"
        ),
        (None, _) => tracing::warn!(
            "no review model: every area will read couldn't tell, and the report says why"
        ),
    }

    // Each review waiting here is a paid job's: one that is there but can't
    // be read whole stops the reviewer, rather than be taken for nothing
    // waiting and lose every review in it.
    let pending_file = args.store.join("pending-reviews.json");
    let pending = match &internal {
        Some(internal) => match internal.read_state(PENDING).await? {
            Some(bytes) => serde_json::from_slice(&bytes).context(
                "the reviews waiting to be posted are kept but not whole, so the reviewer is \
                 not starting rather than lose them",
            )?,
            None => Vec::new(),
        },
        None => read_pending(&pending_file)?,
    };
    let reviewer = Arc::new(Reviewer {
        configured,
        connected: Default::default(),
        store: args.store,
        model: args.model,
        api,
        model_wait: Duration::from_secs(args.model_seconds),
        calibrator,
        battery: Battery::pinned(),
        pending: Mutex::new(pending),
        pending_file,
        key: launched.as_ref().map(|launched| launched.key.clone()),
        attestation: launched
            .as_ref()
            .map(|launched| launched.attestation.clone()),
        internal,
        canonical,
        taken: Default::default(),
        in_flight: AtomicUsize::new(0),
        idle: cloud::Idle::new(args.idle_exit_seconds.map(Duration::from_secs)),
        turns: cloud::Turns::default(),
    });
    if let Some(key) = &reviewer.key {
        tracing::info!(key = %key.address(), "the reviewer's own key, derived for this image");
    }

    let joining = reviewer.clone();
    tokio::spawn(async move {
        while let Some(served) = connecting.recv().await {
            tokio::spawn(take_on(joining.clone(), served));
        }
    });

    let poster = reviewer.clone();
    tokio::spawn(async move {
        loop {
            let Some(turn) = poster.turns.turn().await else {
                return;
            };
            post_pending(&poster).await;
            drop(turn);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let attested = reviewer
        .attestation
        .as_ref()
        .map(|attestation| attestation.routes())
        .unwrap_or_default();
    let app = axum::Router::new()
        .route("/health", get(|| async { "reviewer" }))
        // A review request carries the whole archive of a paid upload, as
        // the upload did: it is taken up to the same size.
        .route(
            "/{chain}/reviews",
            post(review).layer(axum::extract::DefaultBodyLimit::max(
                shared::archive::MOST_UPLOAD_BYTES,
            )),
        )
        .with_state(reviewer.clone())
        .merge(attested);
    let address = SocketAddr::from((args.listen, args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the reviewer is open");
    let stopping = Arc::new(tokio::sync::Notify::new());
    let stop = stopping.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.notified().await })
            .await
    });
    watch_idle(&reviewer).await;
    // Idle: stop listening, let what is being answered finish, end the
    // posting of reviews, write down what is waiting and the wake plan, and
    // exit.
    stopping.notify_one();
    let _ = server.await;
    reviewer.turns.close().await;
    let pending = reviewer.pending.lock().await;
    match write_pending(&reviewer, &pending).await {
        // Before the blessing no review is made, so none waits unwritten.
        Err(error) if error.is::<cloud::NotBlessed>() => tracing::info!("{error}"),
        written => written?,
    }
    drop(pending);
    // Busy as it stands now: a review cleared while closing and not yet
    // posted has the waker start the reviewer again.
    let busy = reviewer.busy().await;
    if let Some(internal) = &reviewer.internal
        && reviewer.plan_taken()
    {
        match internal.put_plan(&plan(&reviewer, busy)).await {
            Err(error) if error.is::<cloud::NotBlessed>() => tracing::info!("{error}"),
            told => told?,
        }
    }
    tracing::info!("idle: the reviewer exits");
    Ok(())
}

/// The name the reviews waiting to be posted are kept under.
const PENDING: &str = "pending";

/// The reviewer's wake plan.
fn plan(reviewer: &Reviewer, busy: bool) -> cloud::WakePlan {
    cloud::WakePlan {
        busy,
        renew_at: reviewer.attestation.as_ref().and_then(|a| a.renew_at()),
        payments_seen: BTreeMap::new(),
    }
}

/// Watch whether the reviewer has anything to do, telling the API's waker
/// each time that changes, and return once it has been idle long enough to
/// exit (never, without an idle exit). No plan is sent until the API would
/// take it (see [`Reviewer::plan_taken`]); one it refuses for want of the
/// blessing is said once, at info, and sent again next time round. Once the
/// API takes the reviewer's writes, what was found sealed under another key
/// is set aside and the reviews waiting are written down (see [`settle`]).
async fn watch_idle(reviewer: &Reviewer) {
    let mut told = None;
    let mut said_not_blessed = false;
    let mut said_waiting = false;
    let mut settled = false;
    loop {
        if !settled {
            settled = settle(reviewer).await;
        }
        let busy = reviewer.busy().await;
        let plan = plan(reviewer, busy);
        let taken = reviewer.plan_taken();
        if reviewer.internal.is_some() && !taken && !said_waiting {
            tracing::info!(
                chain_id = reviewer.canonical,
                "the wake plan is sent once the key is blessed on the canonical chain"
            );
            said_waiting = true;
        }
        if let Some(internal) = &reviewer.internal
            && told.as_ref() != Some(&plan)
            && taken
        {
            match internal.put_plan(&plan).await {
                Ok(()) => told = Some(plan),
                Err(error) if error.is::<cloud::NotBlessed>() => {
                    if !said_not_blessed {
                        tracing::info!("{error}");
                        said_not_blessed = true;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "the wake plan could not be told"
                    )
                }
            }
        }
        if reviewer.idle.should_exit(busy) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Once the API takes the reviewer's writes, set aside what was found sealed
/// under another key, and write down the reviews waiting to be posted, a
/// write of which may have been refused before. Whether that is done: what
/// fails is said, and tried again next time round.
async fn settle(reviewer: &Reviewer) -> bool {
    let Some(internal) = &reviewer.internal else {
        return true;
    };
    if !internal.writable() {
        return false;
    }
    if let Err(error) = internal.set_aside().await {
        tracing::warn!(
            error = format!("{error:#}"),
            "the state sealed under another key could not be set aside; trying again"
        );
        return false;
    }
    let pending = reviewer.pending.lock().await;
    if let Err(error) = write_pending(reviewer, &pending).await {
        tracing::warn!(error = format!("{error:#}"), "trying again");
        return false;
    }
    tracing::info!("the key is blessed and the state kept is the reviewer's own: reviews are made");
    true
}

/// How often a chain waiting to be taken on is looked at again: whether the
/// key is blessed there and holds gas.
const BLESSING_LOOKED_AT_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(10)
};

/// Take a chain on once it is connected: get the key blessed there, since
/// each chain's registry decides for itself which keys may post a review
/// there, and only then review for it. Blessing is tried again until it is
/// done, waiting longer each time.
async fn take_on(reviewer: Arc<Reviewer>, served: Served) {
    let chain = served.vault_chain;
    if let (Some(attestation), Some(key)) = (&reviewer.attestation, &reviewer.key) {
        // In the attested image the key is blessed by the launcher's token,
        // and no review is taken on a chain until it is, and until the key
        // holds gas for the sends a review needs.
        attestation.serve_chain(served.client.clone(), served.deployments.clone());
        let mut said = false;
        loop {
            let ready = attestation.ready(served.client.chain_id);
            let gas = ready
                && cloud::holds_gas(&served.client, key.address())
                    .await
                    .unwrap_or(false);
            if gas {
                break;
            }
            if !said {
                tracing::info!(%chain, blessed = ready, "waiting for the key's blessing and gas");
                said = true;
            }
            tokio::time::sleep(BLESSING_LOOKED_AT_EVERY).await;
        }
        reviewer
            .connected
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chain, Arc::new(served));
        tracing::info!(%chain, "reviewing on this chain");
        return;
    }
    let key = served.deployments.roles.reviewer;
    let mut wait = Duration::from_secs(5);
    while let Err(error) =
        chain::attest::bless_service(&served.client, &served.deployments, "reviewer", key).await
    {
        tracing::warn!(%chain, error = format!("{error:#}"), seconds = wait.as_secs(), "the key could not be blessed; trying again");
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(300));
    }
    chain::attest::keep_blessed(
        served.client.clone(),
        served.deployments.clone(),
        "reviewer",
        key,
        chain::attest::Sender::Unlocked(key),
        alloy::primitives::Address::ZERO,
        B256::ZERO,
        || {},
    );
    reviewer
        .connected
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(chain, Arc::new(served));
    tracing::info!(%chain, "reviewing on this chain");
}

async fn review(
    State(reviewer): State<Arc<Reviewer>>,
    UrlPath(chain): UrlPath<Chain>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<ReviewAnswer>, (StatusCode, String)> {
    let _working = Working::start(&reviewer);
    // Its go-ahead could not be kept yet: the build service asks again.
    if !reviewer.keeps_state() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "the reviewer is starting and makes reviews once its key is blessed: ask again \
             shortly"
                .to_string(),
        ));
    }
    // Asked about a chain not connected yet, the build service puts the job
    // back in its queue and asks again later.
    match reviewer.on(chain) {
        Ok(_) => {}
        Err(Unserved::NotConnected) => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{chain} is not connected yet: ask again shortly"),
            ));
        }
        Err(Unserved::NotServed) => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("this reviewer does not serve {chain}"),
            ));
        }
    }
    // In the attested image, only a blessed build service may ask: a review
    // spends the model's key, and its go-ahead is what the registry takes.
    if reviewer.key.is_some() {
        let on = reviewer
            .on(chain)
            .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
        asked_by_the_builder(&reviewer.taken, &on, uri.path(), &headers, &body).await?;
    }
    let request: ReviewRequest = serde_json::from_slice(&body).map_err(|error| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("not a review request: {error}"),
        )
    })?;
    review_one(&reviewer, chain, request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = unanswered(&error);
            tracing::warn!(
                status = status.as_u16(),
                error = format!("{error:#}"),
                "a review could not be made"
            );
            (status, format!("{error:#}"))
        })
}

/// A review being made, counted while it is.
struct Working<'a>(&'a Reviewer);

impl<'a> Working<'a> {
    fn start(reviewer: &'a Reviewer) -> Self {
        reviewer.in_flight.fetch_add(1, Ordering::SeqCst);
        Self(reviewer)
    }
}

impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Check that a review request at `path` was signed, within the allowed
/// skew of now, by a key blessed as a build service on the request's chain,
/// and that its signature has not been taken before. Unsigned, wrongly
/// signed or sent again is 401, signed by any other key 403, and a registry
/// that could not be read 503.
async fn asked_by_the_builder(
    taken: &std::sync::Mutex<BTreeMap<(Address, B256), u64>>,
    on: &Served,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<(), (StatusCode, String)> {
    use chain::bindings::attestation::{IAttestationRegistry, ImageKind};

    let unsigned = |why: &str| {
        (
            StatusCode::UNAUTHORIZED,
            format!("not a signed request: {why}"),
        )
    };
    let at: u64 = headers
        .get(cloud::AT_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| unsigned("no time"))?;
    if cloud::now().abs_diff(at) > cloud::MOST_SKEW_SECONDS {
        return Err(unsigned("signed too long ago, or in the future"));
    }
    let signature = headers
        .get(cloud::SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| alloy::hex::decode(text).ok())
        .ok_or_else(|| unsigned("no signature"))?;
    let signer = cloud::request_signer("POST", path, body, at, &signature)
        .map_err(|_| unsigned("the signature recovers no signer"))?;
    let blessed = on
        .client
        .read(
            on.deployments.attestation,
            IAttestationRegistry::isBlessedAsCall {
                signer,
                kind: ImageKind::Builder,
            },
        )
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("the registry could not be read: {error:#}"),
            )
        })?;
    if !blessed {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{signer} is not blessed as a build service on this chain"),
        ));
    }
    let signed = cloud::request_digest("POST", path, body, at);
    take_once(taken, (signer, signed), at, cloud::now())
        .map_err(|()| unsigned("this request was taken already"))
}

/// Take a request signed at `at` once: refused if it was taken already.
/// Requests signed outside the allowed skew of `now` are forgotten, since
/// one signed then is refused for its time alone.
fn take_once(
    taken: &std::sync::Mutex<BTreeMap<(Address, B256), u64>>,
    request: (Address, B256),
    at: u64,
    now: u64,
) -> Result<(), ()> {
    let mut taken = taken.lock().unwrap_or_else(|e| e.into_inner());
    taken.retain(|_, signed_at| now.abs_diff(*signed_at) <= cloud::MOST_SKEW_SECONDS);
    match taken.insert(request, at) {
        None => Ok(()),
        Some(_) => Err(()),
    }
}

/// What a review that could not be made is answered with. Only a verdict
/// ends a paid job, so nothing here is one: the reviewer's own trouble (its
/// model could not be started, gave no answer or none in time, its node
/// refused a send, its store could not be written) is 503, ask again later;
/// a chain it does not serve is 404; a request it can't read as one is 422.
fn unanswered(error: &anyhow::Error) -> StatusCode {
    if error.downcast_ref::<Unreadable>().is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else if error.downcast_ref::<Unserved>() == Some(&Unserved::NotServed) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Read one submission and answer: a go-ahead, or a refusal on the chain.
async fn review_one(
    reviewer: &Reviewer,
    chain: Chain,
    request: ReviewRequest,
) -> Result<ReviewAnswer> {
    let on = reviewer.on(chain)?;
    let archive = shared::base64::decode(&request.archive_base64).map_err(unreadable)?;
    // The reviewer hashes what it received itself, and signs for that.
    let source_hash = hash_bytes(&archive);
    let manifest = manifest_in(&archive, &request.manifest_path).map_err(unreadable)?;
    let source = source::from_archive(&archive).map_err(unreadable)?;
    let dependencies = shared::dependencies::listed(&archive);

    let (prose, answers, answered_by) = match &reviewer.model {
        Some(model) => {
            let prose = ask(
                reviewer,
                model,
                &reviewer.battery,
                &manifest,
                &dependencies,
                &source,
                request.sealed,
            )
            .await?;
            // The battery's answers come from the calibrator when there is
            // one and it answers the battery whole, and from the pinned
            // model otherwise.
            let calibrated = match &reviewer.calibrator {
                Some(client) => match client.ask(&reviewer.battery, &source).await {
                    Ok((calibrator, answers)) => match reviewer.battery.answered_whole(&answers) {
                        Ok(()) => Some((format!("{model}+{calibrator}"), answers)),
                        Err(why) => {
                            tracing::warn!(%why, "the calibrator's answers are not taken; the model's own stand");
                            None
                        }
                    },
                    Err(error) => {
                        tracing::warn!(%error, "the battery went unanswered; the model's own answers stand");
                        None
                    }
                },
                None => None,
            };
            let (answered_by, answers) = match calibrated {
                Some(calibrated) => calibrated,
                None => {
                    // A report that leaves any question unanswered is no
                    // answer: the build service asks again.
                    reviewer
                        .battery
                        .answered_whole(&prose.answers)
                        .map_err(|why| {
                            anyhow!("the reviewer's model gave no whole answer: {why}")
                        })?;
                    (model.clone(), prose.answers.clone())
                }
            };
            (prose, answers, answered_by)
        }
        None => (unread(), Vec::new(), "none".to_string()),
    };
    let (ratings, refused) = reviewer.battery.rate(&answers);

    let findings: Vec<Finding> = prose
        .findings
        .into_iter()
        .take(50)
        .map(|finding| Finding {
            area: finding.area,
            severity: finding.severity,
            summary: finding.summary,
            detail: finding.detail,
            // Sealed source is reviewed all the same; its report just never
            // says where in the code anything is.
            file: (!request.sealed && !finding.file.is_empty()).then_some(finding.file),
            line: (!request.sealed && finding.line != 0).then_some(finding.line),
        })
        .collect();
    let mut limits = prose.limits;
    if limits.is_empty() {
        limits.push("A review is one reading of the code, not a proof about it.".to_string());
    }
    let report = Report {
        ratings,
        refused,
        sealed: request.sealed,
        summary: prose.summary,
        findings,
        limits,
        answered_by: answered_by.clone(),
        answers: serde_json::to_value(&answers)?,
    };
    let body = serde_json::to_vec_pretty(&report)?;
    let report_hash = hash_bytes(&body);
    write(&reviewer.store, &report_hash, "report.json", &body)?;
    // Off the machine, the report is put into the API's store before the
    // chain names it or the build service is told its hash: this machine's
    // disk is gone when it powers off. Not put, no review is made.
    if let Some(internal) = &reviewer.internal {
        internal
            .push_store(&report_hash.replace(':', "-"), "report.json", body.clone())
            .await
            .context("the report could not be put into the API's store")?;
    }

    let source_bytes = to_b256(&source_hash)?;
    let report_bytes = to_b256(&report_hash)?;
    let model_id_hash = keccak256(answered_by.as_bytes());
    let codes = ratings.map(Rating::code);
    let finding_count = report.findings.len() as u16;

    if refused {
        reviewer
            .send(
                &on,
                on.deployments.reviews,
                IReviewRegistry::submitRefusalCall {
                    strategyId: U256::from(request.strategy_id),
                    review: signed(
                        B256::ZERO,
                        source_bytes,
                        report_bytes,
                        model_id_hash,
                        codes,
                        finding_count,
                    ),
                },
            )
            .await
            .context("the registry would not record the refusal")?;
        tracing::info!(%chain, id = request.strategy_id, "refused as malicious");
        return Ok(ReviewAnswer {
            source_hash,
            report_hash,
            clearance: None,
        });
    }

    let clearance = match &reviewer.key {
        Some(key) => on
            .client
            .with_key(key.clone())?
            .sign(chain::digest::clearance(
                on.client.chain_id,
                on.deployments.strategies,
                U256::from(request.strategy_id),
                source_bytes,
            ))?,
        None => {
            clearance(
                &on.client,
                on.deployments.strategies,
                reviewer.address(&on),
                request.strategy_id,
                source_bytes,
            )
            .await?
        }
    };

    let mut pending = reviewer.pending.lock().await;
    pending.push(Pending {
        chain,
        strategy_id: request.strategy_id,
        source_hash: source_bytes,
        report_hash: report_bytes,
        model_id_hash,
        ratings: codes,
        finding_count,
        cleared_at: cloud::now(),
    });
    // A go-ahead whose review isn't written down would never be posted once
    // the reviewer restarts: none is given until it is.
    if let Err(error) = write_pending(reviewer, &pending).await {
        pending.pop();
        return Err(error);
    }
    tracing::info!(%chain, id = request.strategy_id, "read, and cleared to build");
    Ok(ReviewAnswer {
        source_hash,
        report_hash,
        clearance: Some(format!("{clearance}")),
    })
}

/// Post every review whose build has been registered since it was cleared.
/// Each is tried on its own: one whose chain can't be read, isn't connected
/// yet, or whose post the registry would not take is said and left waiting,
/// to be tried again next time, and holds up none after it.
async fn post_pending(reviewer: &Reviewer) {
    let waiting = reviewer.pending.lock().await.clone();
    let mut done = Vec::new();
    for (index, review) in waiting.iter().enumerate() {
        match post_one(reviewer, review).await {
            Ok(true) => done.push(index),
            Ok(false) => {}
            Err(error) if error.downcast_ref() == Some(&Unserved::NotConnected) => {
                tracing::debug!(chain = %review.chain, id = review.strategy_id, "a review waits for its chain to connect");
            }
            Err(error) => {
                tracing::warn!(chain = %review.chain, id = review.strategy_id, error = format!("{error:#}"), "could not post a review; trying again later");
            }
        }
    }
    if !done.is_empty() {
        let mut pending = reviewer.pending.lock().await;
        let posted: Vec<(Chain, u64)> = done
            .iter()
            .map(|&i| (waiting[i].chain, waiting[i].strategy_id))
            .collect();
        pending.retain(|review| !posted.contains(&(review.chain, review.strategy_id)));
        // Not written, the list on disk still holds what was posted: each is
        // found posted already and dropped again next time.
        if let Err(error) = write_pending(reviewer, &pending).await {
            tracing::warn!(error = format!("{error:#}"), "trying again later");
        }
    }
}

/// Post one review if its build has been registered: whether it is done
/// with (posted now, or already), or still waits for its build.
async fn post_one(reviewer: &Reviewer, review: &Pending) -> Result<bool> {
    let on = reviewer.on(review.chain)?;
    let id = U256::from(review.strategy_id);
    let reviews = on.deployments.reviews;
    let already = on
        .client
        .read(reviews, IReviewRegistry::reviewOfCall { strategyId: id })
        .await?;
    if already.review.reviewedAt != 0 {
        return Ok(true);
    }
    let bundle = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::bundleHashOfCall { strategyId: id },
        )
        .await?;
    // Not built yet, or never will be: a failed build has nothing to post
    // against, and its review stays in the store.
    if bundle == B256::ZERO {
        return Ok(false);
    }
    reviewer
        .send(
            &on,
            reviews,
            IReviewRegistry::submitReviewCall {
                strategyId: id,
                review: signed(
                    bundle,
                    review.source_hash,
                    review.report_hash,
                    review.model_id_hash,
                    review.ratings,
                    review.finding_count,
                ),
            },
        )
        .await
        .context("the registry would not take the review")?;
    tracing::info!(chain = %review.chain, id = review.strategy_id, "review posted");
    Ok(true)
}

fn signed(
    bundle: B256,
    source: B256,
    report: B256,
    model: B256,
    ratings: [u8; 6],
    findings: u16,
) -> SignedReview {
    SignedReview {
        bundleHash: bundle,
        sourceHash: source,
        // The registry records the image it knows the reviewer key by, and
        // the time it received this; what is sent here is only a courtesy.
        reviewerImage: chain::attest::image_digest("reviewer"),
        modelIdHash: model,
        // What the model was asked, the shape it had to answer in, and the
        // questions the ratings come from, together.
        promptHash: keccak256(
            [
                PROMPT.as_bytes(),
                SCHEMA.as_bytes(),
                shonin_reviewer::battery::BATTERY.as_bytes(),
            ]
            .concat(),
        ),
        reportHash: report,
        ratings,
        findingCount: findings,
        reviewedAt: 0,
    }
}

/// The crates.io crates a project locks, as the model is told of them.
fn dependencies_named(dependencies: &[(String, String)]) -> String {
    if dependencies.is_empty() {
        return "It depends on no crates.io crate.".into();
    }
    format!(
        "It depends on these crates.io crates, which are not in the source below: each is on \
         the platform's list of crates it reviewed itself, by version and checksum, and a \
         project may depend on no others. Crates inside the project are in the source, and \
         shonin-strategy-sdk is the platform's own SDK.\n{}",
        dependencies
            .iter()
            .map(|(name, version)| format!("- {name} {version}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// What a review says when there is no model to read with.
fn unread() -> Prose {
    Prose {
        summary: "No review model is configured on this network, so the code was not read.".into(),
        findings: Vec::new(),
        limits: vec!["Nothing was read: every area reads couldn't tell.".into()],
        answers: Vec::new(),
    }
}

/// Run the pinned model over the description and the source, with the schema
/// it must answer in. The crates.io crates the project locks are named, not
/// shown: the platform reviewed them itself, and a project may depend on no
/// others (see [`shared::dependencies`]).
async fn ask(
    reviewer: &Reviewer,
    model: &str,
    battery: &Battery,
    manifest: &Manifest,
    dependencies: &[(String, String)],
    source: &str,
    sealed: bool,
) -> Result<Prose> {
    let description = format!(
        "Title: {}\n\n{}\n\nClaims:\n{}\n\nThe source is {}.\n\n{}",
        manifest.description.title,
        manifest.description.text,
        manifest
            .description
            .claims
            .iter()
            .map(|claim| format!("- {claim}"))
            .collect::<Vec<_>>()
            .join("\n"),
        if sealed { "sealed" } else { "published" },
        dependencies_named(dependencies),
    );
    let (system, asked) = prompt(battery, &description);

    if let Some(api) = &reviewer.api {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA)?;
        let report = api
            .ask(&system, &format!("{asked}\n\n{source}\n"), &schema)
            .await?;
        return serde_json::from_value(report)
            .context("the reviewer's model answered outside its schema");
    }
    let stdout = run_model(
        model,
        reviewer.model_wait,
        format!("{system}{asked}\n\n{source}\n").into_bytes(),
    )
    .await?;
    let reply: serde_json::Value = serde_json::from_slice(&stdout)
        .context("the reviewer's model said something unexpected")?;
    let structured = reply
        .get("structured_output")
        .cloned()
        .context("the reviewer's model answered outside its schema")?;
    serde_json::from_value(structured).context("the report did not match the schema")
}

/// Where the prompt hands over to what the author wrote.
const AUTHOR_S_PART: &str = "## The author's description";

/// The prompt in two parts: what the reviewer asks, with the battery in it,
/// and what the author wrote, headed as the prompt heads it, to which the
/// source is added. Together they are the whole prompt, as pinned.
fn prompt(battery: &Battery, description: &str) -> (String, String) {
    let at = PROMPT
        .find(AUTHOR_S_PART)
        .expect("the prompt heads the author's description");
    let (system, asked) = PROMPT.split_at(at);
    (
        system.replace("{battery}", &battery.as_prompt()),
        asked.replace("{description}", description),
    )
}

/// The pinned model's API, with its key opened from `sealed` by attested
/// exchange. The key is opened once, at start, and held in memory only.
async fn model_api(args: &Args, sealed: &Path) -> Result<messages::Client> {
    let Some(model) = args.model.clone() else {
        bail!("a sealed model key is given but no model: set SHONIN_REVIEW_MODEL");
    };
    let (Attestation::Launcher, Some(holder_url)) = (args.attestation, &args.holder_url) else {
        bail!("the sealed model key is opened only in launcher mode, from the key holder");
    };
    let sealed: shonin_rootkey::Sealed = serde_json::from_str(
        &std::fs::read_to_string(sealed)
            .with_context(|| format!("reading {}", sealed.display()))?,
    )
    .context("the sealed model key is not a sealed message")?;
    if sealed.purpose.to_string() != MODEL_KEY_PURPOSE {
        bail!(
            "the sealed model key is sealed to {}, not {MODEL_KEY_PURPOSE}",
            sealed.purpose
        );
    }
    let received =
        shonin_rootkey::service::obtain(holder_url, &args.launcher_socket, sealed.purpose.clone())
            .await?;
    let shonin_rootkey::Received::Key(pair) = received else {
        bail!("the holder released something other than the model's key pair");
    };
    let plaintext =
        shonin_rootkey::open(&pair, &sealed, b"").context("the sealed model key did not open")?;
    let key = messages::ModelKey::from_plaintext(&plaintext)?;
    tracing::info!(purpose = %sealed.purpose, "opened the model's key");
    Ok(messages::Client::new(
        key,
        model,
        Duration::from_secs(args.model_seconds),
    ))
}

/// The purpose the model's key is sealed to.
const MODEL_KEY_PURPOSE: &str = "platform:reviewer-model";

/// Run the pinned model's program over `input`, and what it printed once it
/// exited answering. It acts on text the author wrote, so it is given as
/// little as it can answer with: none of the reviewer's environment but a
/// `PATH`, a `HOME` and the model's own credentials ([`MODEL_ENVIRONMENT`]),
/// no tools, a working directory of its own that starts empty and is removed
/// after, and no session kept. It leads a process group of its own, and
/// given no answer within `wait` the whole group is killed.
async fn run_model(model: &str, wait: Duration, input: Vec<u8>) -> Result<Vec<u8>> {
    let directory = Scratch::new().context("the reviewer's model has no directory to run in")?;
    let mut command = tokio::process::Command::new("claude");
    command
        .args([
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            SCHEMA,
            "--model",
            model,
            "--tools",
            "",
            "--no-session-persistence",
        ])
        .env_clear()
        .current_dir(&directory.0)
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in ["PATH", "HOME"].into_iter().chain(MODEL_ENVIRONMENT) {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command
        .spawn()
        .context("the reviewer's model could not be started")?;
    let group = child.id();

    let talk = async {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
        }
        let mut stdout = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_end(&mut stdout).await?;
        }
        let status = child.wait().await?;
        anyhow::Ok((status, stdout))
    };
    match tokio::time::timeout(wait, talk).await {
        Ok(Ok((status, stdout))) if status.success() => Ok(stdout),
        Ok(Ok(_)) => bail!("the reviewer's model did not answer"),
        Ok(Err(error)) => {
            stop(&mut child, group).await;
            Err(error.context("the reviewer's model did not answer"))
        }
        Err(_) => {
            stop(&mut child, group).await;
            bail!(
                "the reviewer's model did not answer within {} seconds",
                wait.as_secs()
            )
        }
    }
}

/// Stop the model's program and everything it started: its whole group,
/// signalled while its leader is not yet reaped, so the group's id can't
/// have been given to anyone else.
async fn stop(child: &mut tokio::process::Child, group: Option<u32>) {
    use rustix::process::{Pid, Signal, kill_process_group};

    if let Some(leader) = group
        .and_then(|group| i32::try_from(group).ok())
        .and_then(Pid::from_raw)
    {
        match kill_process_group(leader, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            // macOS answers so for a group whose only member is its leader,
            // ended and not yet reaped: there is nothing left to kill.
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) => {}
            Err(error) => {
                tracing::error!(%error, "the reviewer's model could not be stopped");
            }
        }
    }
    let _ = child.kill().await;
}

/// A directory of the reviewer's own, made empty and readable by it alone
/// under an unguessable name, and removed with everything in it once done.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        use std::io::Read;
        use std::os::unix::fs::DirBuilderExt;

        let mut unguessable = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut unguessable)?;
        let directory =
            std::env::temp_dir().join(format!("shonin-review-{}", alloy::hex::encode(unguessable)));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        Ok(Self(directory))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The reviewer's go-ahead for a source: its signature over the registry's
/// clearance digest, worked out here as the registry writes it and never read
/// from the node, which could otherwise hand over the digest of anything the
/// reviewer's signature counts for and have it signed. The chain id is the
/// configured one, checked against the node when it connected.
async fn clearance(
    client: &chain::Client,
    registry: alloy::primitives::Address,
    reviewer: alloy::primitives::Address,
    strategy_id: u64,
    source_hash: B256,
) -> Result<alloy::primitives::Bytes> {
    let digest = chain::digest::clearance(
        client.chain_id,
        registry,
        U256::from(strategy_id),
        source_hash,
    );
    client.sign(reviewer, digest).await
}

/// The manifest at `path` inside a packed project: `./shonin.toml` and
/// `shonin.toml` are the same file, as the builder reads them.
fn manifest_in(archive: &[u8], path: &str) -> Result<Manifest> {
    let wanted = parts(Path::new(path));
    let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
    for entry in reader.entries()?.flatten() {
        let mut entry = entry;
        if entry.header().entry_type() == tar::EntryType::Regular && parts(&entry.path()?) == wanted
        {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)?;
            return Manifest::from_toml(&text).map_err(|error| anyhow!(error));
        }
    }
    bail!("no {path} in the project")
}

/// A path's named parts, so `./a/b` and `a/b` are the same path.
fn parts(path: &Path) -> Vec<std::ffi::OsString> {
    path.components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect()
}

fn write(store: &Path, hash: &str, name: &str, bytes: &[u8]) -> Result<()> {
    let directory = store.join(hash.replace(':', "-"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(name), bytes)?;
    Ok(())
}

/// The reviews waiting to be posted, as last written down: none if nothing
/// was ever written, and an error, naming the file, if it is there but can't
/// be read whole.
fn read_pending(path: &Path) -> Result<Vec<Pending>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "{} could not be read, so the reviewer is not starting rather than lose \
                     the reviews waiting in it to be posted",
                    path.display()
                )
            });
        }
    };
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "{} is there but is not whole, so the reviewer is not starting rather than lose \
             the reviews waiting in it to be posted: restore it, or a backup of it",
            path.display()
        )
    })
}

/// Write down the reviews waiting to be posted, whole or not at all: off
/// the machine when the state is kept there, and in the store otherwise.
async fn write_pending(reviewer: &Reviewer, pending: &[Pending]) -> Result<()> {
    let bytes = serde_json::to_vec(pending)?;
    match &reviewer.internal {
        Some(internal) => internal.write_state(PENDING, &bytes).await,
        None => write_atomically(&reviewer.pending_file, &bytes).map_err(anyhow::Error::from),
    }
    .context("the reviews waiting to be posted could not be written down")
}

/// Replace a file whole or not at all: written beside it, flushed to disk,
/// then renamed over it, so a machine that stops part way leaves the old
/// file or the new one, never the start of one.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let fresh = directory.join(name);
    let mut file = std::fs::File::create(&fresh)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&fresh, path)?;
    // The rename is kept once the directory is.
    std::fs::File::open(directory)?.sync_all()
}

fn to_b256(hash: &str) -> Result<B256> {
    let bytes = shared::manifest::hash_to_bytes(hash).context("a hash")?;
    Ok(B256::from(bytes))
}

/// Keep the areas and severities the schema names in step with the model the
/// rest of the platform reads, and every battery question in the prompt.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_request_is_taken_once_while_its_time_holds() {
        let taken = std::sync::Mutex::new(BTreeMap::new());
        let request = (Address::repeat_byte(1), B256::repeat_byte(2));
        let at = 1_000_000;
        assert_eq!(take_once(&taken, request, at, at), Ok(()));
        assert_eq!(take_once(&taken, request, at, at + 1), Err(()));
        assert_eq!(
            take_once(&taken, request, at, at + cloud::MOST_SKEW_SECONDS),
            Err(())
        );
        // The same request from another signer is its own.
        let other = (Address::repeat_byte(3), request.1);
        assert_eq!(take_once(&taken, other, at, at + 2), Ok(()));
        // Past the skew it is forgotten, and refused for its time instead.
        take_once(&taken, request, at, at + 3 * cloud::MOST_SKEW_SECONDS).ok();
        assert!(
            taken
                .lock()
                .unwrap()
                .keys()
                .all(|(signer, _)| *signer != Address::repeat_byte(3))
        );
    }
    use shared::{Area, Severity};

    fn schema() -> serde_json::Value {
        serde_json::from_str(SCHEMA).expect("the schema parses")
    }

    #[test]
    fn every_area_the_schema_names_is_one_the_platform_knows() {
        let names = schema()["properties"]["findings"]["items"]["properties"]["area"]["enum"]
            .as_array()
            .expect("the areas are listed")
            .clone();
        assert_eq!(names.len(), Area::ALL.len());
        for name in names {
            serde_json::from_value::<Area>(name).expect("the platform knows it");
        }
    }

    #[test]
    fn every_severity_the_schema_names_is_one_the_platform_knows() {
        let names = schema()["properties"]["findings"]["items"]["properties"]["severity"]["enum"]
            .as_array()
            .expect("the severities are listed")
            .clone();
        for name in names {
            serde_json::from_value::<Severity>(name).expect("the platform knows it");
        }
    }

    #[test]
    fn the_prompt_carries_the_battery_and_the_description() {
        assert!(PROMPT.contains("{battery}") && PROMPT.contains("{description}"));
        let listed = Battery::pinned().as_prompt();
        for question in Battery::pinned().questions {
            assert!(listed.contains(&question.id));
        }
    }

    /// The model over its API is asked exactly what the model's program is:
    /// the prompt's two parts are the whole prompt, and only the author's
    /// part carries the description.
    #[test]
    fn the_two_parts_of_the_prompt_are_the_whole_prompt() {
        let battery = Battery::pinned();
        let (system, asked) = prompt(&battery, "THE DESCRIPTION");
        assert_eq!(
            format!("{system}{asked}"),
            PROMPT
                .replace("{battery}", &battery.as_prompt())
                .replace("{description}", "THE DESCRIPTION")
        );
        assert!(asked.starts_with(AUTHOR_S_PART) && asked.contains("THE DESCRIPTION"));
        assert!(!system.contains("THE DESCRIPTION"));
    }

    /// A packed project holding `shonin.toml` at its root.
    fn packed(path: &str) -> Vec<u8> {
        let text = "[vault]\nchain = \"base\"\n[description]\ntitle = \"t\"\ntext = \"x\"\n";
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, text.as_bytes())
            .unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn a_manifest_named_from_the_project_s_root_is_read_however_it_is_spelt() {
        for packed_as in ["shonin.toml", "./shonin.toml"] {
            for asked_as in ["shonin.toml", "./shonin.toml"] {
                let manifest = manifest_in(&packed(packed_as), asked_as);
                assert!(
                    manifest.is_ok(),
                    "{asked_as} was not found in a project packing {packed_as}: {manifest:?}"
                );
            }
        }
    }

    /// The go-ahead is signed over the clearance digest the registry checks,
    /// whatever the node says that digest is.
    #[tokio::test]
    async fn the_go_ahead_is_signed_under_a_digest_the_reviewer_works_out_itself() {
        use alloy::signers::local::PrivateKeySigner;
        use alloy::sol_types::SolCall;

        // A node that answers every `clearanceDigest` with the digest of
        // another strategy's source.
        let lie = keccak256("another strategy's source");
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<serde_json::Value>| async move {
                let result = match request["method"].as_str().unwrap_or_default() {
                    "eth_chainId" => serde_json::json!("0x7a69"),
                    "eth_call" => serde_json::json!(alloy::hex::encode_prefixed(
                        IStrategyRegistry::clearanceDigestCall::abi_encode_returns(&lie)
                    )),
                    _ => serde_json::Value::Null,
                };
                Json(serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = PrivateKeySigner::random();
        let reviewer = key.address();
        let client = chain::Client::connect(
            &chain::ChainConfig {
                label: "stand-in".to_string(),
                rpc: url.clone(),
                ws: None,
                dollar: "USDC".to_string(),
                native_usd: None,
                public_rpc: url,
                logs_rpc: None,
                log_range: None,
                confirmations: None,
            },
            chain::Signing::Keys(vec![key]),
        )
        .await
        .unwrap();

        let registry = alloy::primitives::Address::repeat_byte(0x22);
        let source = B256::repeat_byte(0xab);
        let signature = clearance(&client, registry, reviewer, 7, source)
            .await
            .unwrap();
        let signature = alloy::primitives::Signature::try_from(signature.as_ref()).unwrap();
        let signer_of = |digest: B256| signature.recover_address_from_msg(digest).unwrap();
        assert_ne!(
            signer_of(lie),
            reviewer,
            "the reviewer signed the digest the node handed it"
        );
        assert_eq!(
            signer_of(chain::digest::clearance(
                31_337,
                registry,
                U256::from(7u64),
                source
            )),
            reviewer,
            "the go-ahead is not over the registry's clearance digest of this source"
        );
    }

    /// A stand-in node answering as chain `chain_id`: `eth_call` with what
    /// `call` makes of the request (`None` for a refusal), and every other
    /// method but `eth_chainId` with a refusal. Every call's input is noted
    /// in `asked`. Where it is.
    async fn a_node(
        chain_id: u64,
        call: fn(&[u8]) -> Option<String>,
        asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    ) -> String {
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<serde_json::Value>| {
                let asked = asked.clone();
                async move {
                    let id = request["id"].clone();
                    let answer = match request["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => Some(format!("0x{chain_id:x}")),
                        "eth_call" => {
                            let tx = &request["params"][0];
                            let input = alloy::hex::decode(
                                tx["input"].as_str().or(tx["data"].as_str()).unwrap_or("0x"),
                            )
                            .unwrap_or_default();
                            asked.lock().unwrap().push(input.clone());
                            call(&input)
                        }
                        _ => None,
                    };
                    Json(match answer {
                        Some(result) => {
                            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": 3, "message": "execution reverted"},
                        }),
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// The chain at `url` as a reviewer serves it, as `vault_chain`.
    async fn served(url: &str, vault_chain: Chain, chain_id: u64) -> Served {
        let client = chain::Client::connect(
            &chain::ChainConfig {
                label: "stand-in".to_string(),
                rpc: url.to_string(),
                ws: None,
                dollar: "USDC".to_string(),
                native_usd: None,
                public_rpc: url.to_string(),
                logs_rpc: None,
                log_range: None,
                confirmations: None,
            },
            chain::Signing::ReadOnly,
        )
        .await
        .unwrap();
        let one = "0x0000000000000000000000000000000000000001";
        let mut fields = vec![
            format!(r#""chain_key":"evm:{chain_id}""#),
            format!(r#""chain_id":{chain_id}"#),
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
        Served {
            vault_chain,
            key: format!("evm:{chain_id}").parse().unwrap(),
            client,
            deployments: chain::Deployments::from_json(&format!("{{{}}}", fields.join(",")))
                .unwrap(),
            native_usd: None,
            public_rpc: String::new(),
        }
    }

    fn waiting(chain: Chain, strategy_id: u64) -> Pending {
        Pending {
            chain,
            strategy_id,
            source_hash: B256::repeat_byte(1),
            report_hash: B256::repeat_byte(2),
            model_id_hash: B256::repeat_byte(3),
            ratings: [1; 6],
            finding_count: 0,
            cleared_at: 0,
        }
    }

    /// One pending review whose chain can't be read, and one whose post the
    /// registry would not take, hold up none after them: a later chain's
    /// review is still looked at, and is done with, and the two are kept to
    /// be tried again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_review_that_cannot_be_posted_holds_up_none_after_it() {
        use alloy::sol_types::SolCall;

        // Base: strategy 1's review can't be read; strategy 2's build is
        // registered and its post is refused.
        let base_asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Default::default();
        let base = a_node(
            31_337,
            |input| {
                let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
                let id = U256::from_be_slice(input.get(4..36)?);
                match selector {
                    IReviewRegistry::reviewOfCall::SELECTOR if id == U256::from(1) => None,
                    // Never reviewed: every word zero.
                    IReviewRegistry::reviewOfCall::SELECTOR => {
                        Some(format!("0x{}", "0".repeat(64 * 40)))
                    }
                    IStrategyRegistry::bundleHashOfCall::SELECTOR => {
                        Some(format!("0x{}", "42".repeat(32)))
                    }
                    _ => None,
                }
            },
            base_asked.clone(),
        )
        .await;
        // Robinhood: strategy 3's review is posted already.
        let robinhood_asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Default::default();
        let robinhood = a_node(
            46_630,
            |input| {
                (input.get(..4)? == IReviewRegistry::reviewOfCall::SELECTOR)
                    .then(|| format!("0x{}", format!("{:064x}", 1).repeat(40)))
            },
            robinhood_asked.clone(),
        )
        .await;

        let store =
            std::env::temp_dir().join(format!("shonin-reviewer-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let reviewer = Reviewer {
            configured: vec![Chain::Base, Chain::Robinhood],
            connected: std::sync::RwLock::new(BTreeMap::from([
                (
                    Chain::Base,
                    Arc::new(served(&base, Chain::Base, 31_337).await),
                ),
                (
                    Chain::Robinhood,
                    Arc::new(served(&robinhood, Chain::Robinhood, 46_630).await),
                ),
            ])),
            pending_file: store.join("pending-reviews.json"),
            store,
            model: None,
            api: None,
            model_wait: Duration::from_secs(MOST_MODEL_SECONDS),
            calibrator: None,
            battery: Battery::pinned(),
            pending: Mutex::new(vec![
                waiting(Chain::Base, 1),
                waiting(Chain::Base, 2),
                waiting(Chain::Robinhood, 3),
            ]),
            key: None,
            attestation: None,
            internal: None,
            canonical: None,
            taken: Default::default(),
            in_flight: AtomicUsize::new(0),
            idle: cloud::Idle::new(None),
            turns: cloud::Turns::default(),
        };

        post_pending(&reviewer).await;

        assert!(
            robinhood_asked
                .lock()
                .unwrap()
                .iter()
                .any(|input| input.starts_with(&IReviewRegistry::reviewOfCall::SELECTOR)),
            "a later chain's pending review was never looked at"
        );
        let left: Vec<(Chain, u64)> = reviewer
            .pending
            .lock()
            .await
            .iter()
            .map(|review| (review.chain, review.strategy_id))
            .collect();
        assert_eq!(
            left,
            [(Chain::Base, 1), (Chain::Base, 2)],
            "the posted review is done with, and the two that could not be are kept"
        );
        assert!(
            base_asked
                .lock()
                .unwrap()
                .iter()
                .any(|input| input.starts_with(&IStrategyRegistry::bundleHashOfCall::SELECTOR)),
            "the review after an unreadable one on the same chain was never looked at"
        );
        let _ = std::fs::remove_dir_all(&reviewer.store);
    }

    /// The build service's key in the tests of who may ask for a review.
    fn the_builder() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::repeat_byte(7)).unwrap()
    }

    /// A node whose registry blesses [`the_builder`] as a build service, and
    /// no other key.
    fn blesses_the_builder(input: &[u8]) -> Option<String> {
        use alloy::sol_types::SolCall;
        use chain::bindings::attestation::{IAttestationRegistry, ImageKind};

        let call = IAttestationRegistry::isBlessedAsCall::abi_decode(input).ok()?;
        let blessed =
            call.signer == the_builder().address() && matches!(call.kind, ImageKind::Builder);
        Some(alloy::hex::encode_prefixed(
            IAttestationRegistry::isBlessedAsCall::abi_encode_returns(&blessed),
        ))
    }

    fn signed(key: &PrivateKeySigner, path: &str, body: &[u8], at: u64) -> axum::http::HeaderMap {
        use alloy::signers::SignerSync;

        let digest = cloud::request_digest("POST", path, body, at);
        let signature = key.sign_message_sync(digest.as_slice()).unwrap().as_bytes();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(cloud::AT_HEADER, at.to_string().parse().unwrap());
        headers.insert(
            cloud::SIGNATURE_HEADER,
            alloy::hex::encode_prefixed(signature).parse().unwrap(),
        );
        headers
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unsigned_or_wrongly_signed_review_request_is_refused() {
        let url = a_node(31_337, blesses_the_builder, Default::default()).await;
        let on = served(&url, Chain::Base, 31_337).await;
        let path = "/base/reviews";
        let body = br#"{"strategy_id":7}"#;
        let now = cloud::now();
        let status = |outcome: Result<(), (StatusCode, String)>| outcome.err().map(|(s, _)| s);
        let taken = std::sync::Mutex::new(BTreeMap::new());

        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &Default::default(), body).await),
            Some(StatusCode::UNAUTHORIZED),
            "an unsigned request was taken"
        );
        let stale = signed(&the_builder(), path, body, now - 200);
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &stale, body).await),
            Some(StatusCode::UNAUTHORIZED),
            "a request signed long ago was taken"
        );
        let ahead = signed(&the_builder(), path, body, now + 200);
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &ahead, body).await),
            Some(StatusCode::UNAUTHORIZED),
            "a request signed in the future was taken"
        );
        let other_body = signed(&the_builder(), path, b"something else", now);
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &other_body, body).await),
            Some(StatusCode::FORBIDDEN),
            "a signature over another body was taken"
        );
        let other_chain = signed(&the_builder(), "/robinhood/reviews", body, now);
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &other_chain, body).await),
            Some(StatusCode::FORBIDDEN),
            "a signature for another chain's path was taken"
        );
        let stranger = signed(&PrivateKeySigner::random(), path, body, now);
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &stranger, body).await),
            Some(StatusCode::FORBIDDEN),
            "a key not blessed as a build service was taken"
        );
        let builder = signed(&the_builder(), path, body, now);
        assert!(
            asked_by_the_builder(&taken, &on, path, &builder, body)
                .await
                .is_ok()
        );
        assert_eq!(
            status(asked_by_the_builder(&taken, &on, path, &builder, body).await),
            Some(StatusCode::UNAUTHORIZED),
            "the same signed request was taken twice"
        );
    }

    /// The reviewer is busy while a review is being made, while a go-ahead
    /// given within the hour waits to be posted, and not for one older.
    #[tokio::test]
    async fn the_reviewer_is_idle_only_with_nothing_to_do() {
        let reviewer = Reviewer {
            configured: vec![Chain::Base],
            connected: Default::default(),
            store: PathBuf::new(),
            model: None,
            api: None,
            model_wait: Duration::from_secs(1),
            calibrator: None,
            battery: Battery::pinned(),
            pending: Mutex::new(Vec::new()),
            pending_file: PathBuf::new(),
            key: None,
            attestation: None,
            internal: None,
            canonical: None,
            taken: Default::default(),
            in_flight: AtomicUsize::new(0),
            idle: cloud::Idle::new(None),
            turns: cloud::Turns::default(),
        };
        assert!(!reviewer.busy().await);
        {
            let _working = Working::start(&reviewer);
            assert!(reviewer.busy().await, "idle while a review is being made");
        }
        assert!(!reviewer.busy().await);
        let mut recent = waiting(Chain::Base, 1);
        recent.cleared_at = cloud::now() - 60;
        reviewer.pending.lock().await.push(recent);
        assert!(
            reviewer.busy().await,
            "idle while a recent review waits to be posted"
        );
        reviewer.pending.lock().await[0].cleared_at = cloud::now() - PENDING_KEEPS_AWAKE - 1;
        assert!(
            !reviewer.busy().await,
            "kept awake by a review whose build never came"
        );
    }

    /// An attestation whose blessing is ready when the test says so.
    #[derive(Default)]
    struct Blessing(std::sync::atomic::AtomicBool);

    impl cloud::Attesting for Blessing {
        fn serve_chain(&self, _: chain::Client, _: chain::Deployments) {}
        fn ready(&self, _: u64) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn renew_at(&self) -> Option<u64> {
            None
        }
        fn busy(&self) -> bool {
            !self.0.load(Ordering::SeqCst)
        }
        fn routes(&self) -> axum::Router {
            axum::Router::new()
        }
    }

    /// A node answering the gas price as one wei and every balance as the
    /// test says.
    async fn a_node_holding(balance: Arc<std::sync::atomic::AtomicU64>) -> String {
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<serde_json::Value>| {
                let balance = balance.clone();
                async move {
                    let id = request["id"].clone();
                    let result = match request["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => serde_json::json!("0x7a69"),
                        "eth_gasPrice" => serde_json::json!("0x1"),
                        "eth_getBalance" => {
                            serde_json::json!(format!("0x{:x}", balance.load(Ordering::SeqCst)))
                        }
                        _ => {
                            return Json(serde_json::json!({
                                "jsonrpc": "2.0", "id": id,
                                "error": {"code": -32601, "message": "no"},
                            }));
                        }
                    };
                    Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// In the attested image, no review is taken on a chain until the key is
    /// blessed there, and holds gas for two of its larger sends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_review_is_taken_on_a_chain_until_the_key_is_blessed_and_holds_gas() {
        let balance = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let url = a_node_holding(balance.clone()).await;
        let blessing = Arc::new(Blessing::default());
        let reviewer = Arc::new(Reviewer {
            configured: vec![Chain::Base],
            connected: Default::default(),
            store: PathBuf::new(),
            model: None,
            api: None,
            model_wait: Duration::from_secs(1),
            calibrator: None,
            battery: Battery::pinned(),
            pending: Mutex::new(Vec::new()),
            pending_file: PathBuf::new(),
            key: Some(PrivateKeySigner::random()),
            attestation: Some(blessing.clone()),
            internal: None,
            canonical: None,
            taken: Default::default(),
            in_flight: AtomicUsize::new(0),
            idle: cloud::Idle::new(None),
            turns: cloud::Turns::default(),
        });
        tokio::spawn(take_on(
            reviewer.clone(),
            served(&url, Chain::Base, 31_337).await,
        ));
        let connected = || reviewer.on(Chain::Base).is_ok();
        let settle = Duration::from_millis(500);

        tokio::time::sleep(settle).await;
        assert!(!connected(), "taken on before the key was blessed");
        blessing.0.store(true, Ordering::SeqCst);
        tokio::time::sleep(settle).await;
        assert!(!connected(), "taken on before the key held gas");
        // Not quite two sends' worth at one wei a unit of gas.
        balance.store(2 * cloud::ONE_SEND_GAS as u64 - 1, Ordering::SeqCst);
        tokio::time::sleep(settle).await;
        assert!(!connected(), "taken on with gas for less than two sends");
        balance.store(2 * cloud::ONE_SEND_GAS as u64, Ordering::SeqCst);
        let started = std::time::Instant::now();
        while !connected() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "never taken on, blessed and with gas"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn with_no_model_every_area_reads_couldnt_tell() {
        let (ratings, refused) = Battery::pinned().rate(&unread().answers);
        assert_eq!(ratings, [Rating::CouldntTell; 6]);
        assert!(!refused);
    }

    /// The model is told which reviewed crates a project depends on, by
    /// name and version, since their source is not in what it reads.
    #[test]
    fn the_model_is_told_every_crate_the_project_depends_on() {
        let told = dependencies_named(&[
            ("futures".to_string(), "0.3.34".to_string()),
            ("serde".to_string(), "1.0.229".to_string()),
        ]);
        assert!(told.contains("- futures 0.3.34\n- serde 1.0.229"), "{told}");
        assert!(dependencies_named(&[]).contains("no crates.io crate"));
    }
}
