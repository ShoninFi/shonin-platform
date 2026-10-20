//! The API's internal port: what the machines that run on demand (the build
//! service and the reviewer) keep off themselves, and what tells the waker
//! when to start them.
//!
//! It is never carried to the public name: it listens on a port of its own
//! (`--internal-port`), open to those machines alone. Every request that
//! changes anything is signed ([`AT_HEADER`], [`SIGNATURE_HEADER`]) by a key
//! the canonical chain's registry blesses as the kind of machine the route
//! is for (`isBlessedAs`), within [`MOST_SKEW_SECONDS`] of this clock, and
//! each signed request is taken once, whatever form its signature is written
//! in.
//!
//! | route | what |
//! | --- | --- |
//! | `PUT /internal/store/{hash}/{name}` | a built file or a report, kept only if it is what its hash says (201 kept, 200 held already, 422 not its hash, 409 a component before its build's record, 400 a name the store doesn't take) |
//! | `GET\|PUT\|DELETE /internal/state/{machine}/{name}` | a machine's sealed state, opaque here, written whole or not at all (200, 204, 404); reading needs no signature |
//! | `POST /internal/wake/{machine}` | ask for a machine to be started (204) |
//! | `PUT /internal/wake/{machine}/plan` | a machine's wake plan (204) |
//! | `GET /internal/wake` | which machines the waker should start, by [`wanted`]; answered on this machine alone |

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::publishing::IPublishing;
use futures::future::BoxFuture;
use indexer::store::Put;
use indexer::{Indexer, Lookup};
use shared::Chain;

pub use shonin_rootkey::service::{AT_HEADER, SIGNATURE_HEADER};

/// How far a signed request's time may be from this clock, either way.
pub const MOST_SKEW_SECONDS: u64 = 120;

/// How long a blessing read from the chain is believed.
const BLESSED_FOR: Duration = Duration::from_secs(60);

/// How long a key read as not blessed is believed: shorter, so a machine
/// blessed a moment ago is not kept out for long.
const UNBLESSED_FOR: Duration = Duration::from_secs(10);

/// How long an ask to wake a machine stands.
pub const ASK_STANDS: Duration = Duration::from_secs(600);

/// The most a pushed file may hold: well past any bundle the builder makes.
pub const MOST_STORE_BYTES: usize = 256 * 1024 * 1024;

/// The most a machine's state blob may hold.
pub const MOST_STATE_BYTES: usize = 64 * 1024 * 1024;

/// How long one chain may take to say how many payments it has taken.
const COUNT_TIMEOUT: Duration = Duration::from_secs(10);

/// A machine that runs on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Machine {
    Builder,
    Reviewer,
}

impl Machine {
    pub const ALL: [Machine; 2] = [Machine::Builder, Machine::Reviewer];

    pub fn name(self) -> &'static str {
        match self {
            Machine::Builder => "builder",
            Machine::Reviewer => "reviewer",
        }
    }

    fn parse(name: &str) -> Option<Machine> {
        Machine::ALL
            .into_iter()
            .find(|machine| machine.name() == name)
    }

    /// The kind the registry blesses this machine's key as.
    pub fn kind(self) -> ImageKind {
        match self {
            Machine::Builder => ImageKind::Builder,
            Machine::Reviewer => ImageKind::Reviewer,
        }
    }
}

/// What a machine says of itself for the waker: whether it stopped busy,
/// when its blessing is next to be renewed, and how many payments it had
/// read on each chain, by chain id.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub busy: bool,
    pub renew_at: Option<u64>,
    pub payments_seen: BTreeMap<String, u64>,
}

/// What the internal port reads from the chains.
pub trait Reads: Send + Sync {
    /// Whether `signer` is blessed as `machine`'s kind on the canonical
    /// chain, now.
    fn blessed(&self, signer: Address, machine: Machine) -> BoxFuture<'_, Result<bool>>;
    /// How many payments each chain's publishing contract has taken, by
    /// chain id: the chains that answered in time.
    fn payments(&self) -> BoxFuture<'_, BTreeMap<String, u64>>;
}

/// The chains as the indexer has connected them.
pub struct ChainReads {
    pub indexer: Arc<Indexer>,
    /// The vault chain whose registry is the authority on blessings.
    pub canonical: Chain,
}

impl Reads for ChainReads {
    fn blessed(&self, signer: Address, machine: Machine) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async move {
            let Lookup::Served(served) = self.indexer.lookup(self.canonical) else {
                anyhow::bail!("the canonical chain is not connected");
            };
            served
                .client
                .read(
                    served.deployments.attestation,
                    IAttestationRegistry::isBlessedAsCall {
                        signer,
                        kind: machine.kind(),
                    },
                )
                .await
                .context("reading isBlessedAs")
        })
    }

    fn payments(&self) -> BoxFuture<'_, BTreeMap<String, u64>> {
        Box::pin(async move {
            // Each chain under the name the build service's plan gives it: an
            // EVM chain by its id, a Solana cluster by its chain key.
            let reads = Chain::ALL.into_iter().filter_map(|chain| {
                let lookup = self.indexer.lookup(chain);
                if !matches!(lookup, Lookup::Served(_) | Lookup::Solana(_)) {
                    return None;
                }
                Some(async move {
                    // The record's count, when it was read within one
                    // refresh: no read of its own.
                    let name = match &lookup {
                        Lookup::Served(served) => served.client.chain_id.to_string(),
                        Lookup::Solana(served) => served.key.to_string(),
                        _ => return None,
                    };
                    if let Some(within) = self.indexer.refresh_every(chain)
                        && let Some(count) = self.indexer.payments(chain, within).await
                    {
                        return Some((name, count));
                    }
                    let count = tokio::time::timeout(COUNT_TIMEOUT, async {
                        match lookup {
                            Lookup::Served(served) => served
                                .client
                                .read(served.deployments.publishing, IPublishing::countCall {})
                                .await
                                .map(|count| {
                                    (
                                        served.client.chain_id.to_string(),
                                        u64::try_from(count).unwrap_or(u64::MAX),
                                    )
                                }),
                            Lookup::Solana(served) => served
                                .platform
                                .publishing_config(&served.client)
                                .await
                                .map(|config| (served.key.to_string(), config.count)),
                            _ => Err(anyhow::anyhow!("not connected")),
                        }
                    })
                    .await;
                    match count {
                        Ok(Ok(counted)) => Some(counted),
                        Ok(Err(error)) => {
                            tracing::debug!(%chain, error = format!("{error:#}"), "the payments taken could not be counted");
                            None
                        }
                        Err(_) => None,
                    }
                })
            });
            futures::future::join_all(reads)
                .await
                .into_iter()
                .flatten()
                .collect()
        })
    }
}

/// The asks to wake each machine and each machine's plan. The plans are
/// kept on disk, so a restart of the API forgets none.
pub struct Wake {
    asks: Mutex<HashMap<Machine, Instant>>,
    plans: Mutex<HashMap<Machine, Plan>>,
    dir: Option<PathBuf>,
}

impl Wake {
    /// The asks and plans, the plans kept in `dir` (read from it now) if
    /// given.
    pub fn new(dir: Option<PathBuf>) -> Wake {
        let mut plans = HashMap::new();
        if let Some(dir) = &dir {
            for machine in Machine::ALL {
                let path = dir.join(format!("{}.json", machine.name()));
                if let Some(plan) = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Plan>(&bytes).ok())
                {
                    plans.insert(machine, plan);
                }
            }
        }
        Wake {
            asks: Mutex::new(HashMap::new()),
            plans: Mutex::new(plans),
            dir,
        }
    }

    /// Ask for `machine` to be started.
    pub fn ask(&self, machine: Machine) {
        self.asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(machine, Instant::now());
    }

    fn asked(&self, machine: Machine) -> bool {
        self.asks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&machine)
            .is_some_and(|at| at.elapsed() < ASK_STANDS)
    }

    /// Whether the build service was asked for lately.
    #[cfg(test)]
    pub fn asks_builder(&self) -> bool {
        self.asked(Machine::Builder)
    }

    fn plan(&self, machine: Machine) -> Option<Plan> {
        self.plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&machine)
            .cloned()
    }

    fn set_plan(&self, machine: Machine, plan: Plan) -> Result<()> {
        if let Some(dir) = &self.dir {
            let bytes = serde_json::to_vec(&plan)?;
            write_whole(&dir.join(format!("{}.json", machine.name())), &bytes)?;
        }
        self.plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(machine, plan);
        Ok(())
    }
}

/// Whether the waker should start a machine: it was asked for in the last
/// [`ASK_STANDS`], it stopped busy, its blessing is due for renewal, or (for
/// the build service, `payments` given) a chain has taken a payment past
/// what it read. A chain its plan doesn't name is one it doesn't serve, once
/// it has sent a plan; before that, any payment at all wakes it.
pub fn wanted(
    asked: bool,
    plan: Option<&Plan>,
    now: u64,
    payments: Option<&BTreeMap<String, u64>>,
) -> bool {
    if asked {
        return true;
    }
    if let Some(plan) = plan
        && (plan.busy || plan.renew_at.is_some_and(|at| now >= at))
    {
        return true;
    }
    let Some(payments) = payments else {
        return false;
    };
    payments.iter().any(
        |(chain, &count)| match plan.map(|plan| plan.payments_seen.get(chain)) {
            Some(Some(&seen)) => count > seen,
            Some(None) => false,
            None => count > 0,
        },
    )
}

/// Why a request was refused: its status and what the caller is told.
type Refusal = (StatusCode, &'static str);

/// The internal port's state.
pub struct Internal {
    pub indexer: Arc<Indexer>,
    pub reads: Arc<dyn Reads>,
    pub wake: Arc<Wake>,
    /// Where the machines' state blobs are kept, one directory per machine.
    blobs: PathBuf,
    blessings: Mutex<HashMap<(Address, Machine), (bool, Instant)>>,
    /// Each signed request taken in the last while, by its signer and the
    /// digest signed, so none is taken twice: not by the signature's bytes,
    /// since one signature can be written in more than one form that
    /// recovers the same signer.
    taken: Mutex<HashMap<(Address, B256), u64>>,
}

impl Internal {
    pub fn new(
        indexer: Arc<Indexer>,
        reads: Arc<dyn Reads>,
        wake: Arc<Wake>,
        blobs: PathBuf,
    ) -> Internal {
        Internal {
            indexer,
            reads,
            wake,
            blobs,
            blessings: Mutex::new(HashMap::new()),
            taken: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `signer` is blessed as `machine`, as last read (within
    /// [`BLESSED_FOR`]) or read now.
    async fn blessed(&self, signer: Address, machine: Machine) -> Result<bool> {
        let known = self
            .blessings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(signer, machine))
            .copied();
        if let Some((blessed, at)) = known {
            let believed = if blessed { BLESSED_FOR } else { UNBLESSED_FOR };
            if at.elapsed() < believed {
                return Ok(blessed);
            }
        }
        let blessed = self.reads.blessed(signer, machine).await?;
        let mut blessings = self.blessings.lock().unwrap_or_else(|e| e.into_inner());
        blessings.retain(|_, (_, at)| at.elapsed() < BLESSED_FOR);
        blessings.insert((signer, machine), (blessed, Instant::now()));
        Ok(blessed)
    }

    /// The signer of a request, if it is signed as the internal port wants
    /// and the signer is blessed as one of `machines`.
    async fn signed_by(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
        machines: &[Machine],
    ) -> Result<Address, Refusal> {
        let unsigned = |why: &'static str| (StatusCode::UNAUTHORIZED, why);
        let at: u64 = headers
            .get(AT_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| unsigned("not signed: no time"))?;
        let signature = headers
            .get(SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| alloy::hex::decode(value).ok())
            .filter(|bytes| bytes.len() == 65)
            .ok_or_else(|| unsigned("not signed: no signature"))?;
        let now = unix_now();
        if now.abs_diff(at) > MOST_SKEW_SECONDS {
            return Err(unsigned("signed too long ago, or ahead of this clock"));
        }
        let signer = shonin_rootkey::service::request_signer(
            method.as_str(),
            uri.path(),
            body,
            at,
            &signature,
        )
        .map_err(|_| unsigned("the signature recovers no signer"))?;
        let digest = shonin_rootkey::service::request_digest(method.as_str(), uri.path(), body, at);
        let mut blessed_as = None;
        for &machine in machines {
            match self.blessed(signer, machine).await {
                Ok(true) => {
                    blessed_as = Some(machine);
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(error = format!("{error:#}"), "a blessing could not be read");
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        "the chain could not be asked who signed this",
                    ));
                }
            }
        }
        if blessed_as.is_none() {
            tracing::warn!(%signer, path = uri.path(), "a request to the internal port signed by a key not blessed for it");
            return Err((StatusCode::FORBIDDEN, "the signer is not blessed for this"));
        }
        // Each signed request is taken once: the same request sent again,
        // its signature written the same or otherwise, is refused, however
        // soon.
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        taken.retain(|_, at| now.abs_diff(*at) <= 2 * MOST_SKEW_SECONDS);
        if taken.insert((signer, digest), at).is_some() {
            return Err(unsigned("this request was taken already"));
        }
        Ok(signer)
    }

    fn blob(&self, machine: Machine, name: &str) -> PathBuf {
        self.blobs.join(machine.name()).join(name)
    }
}

/// The internal port's routes.
pub fn routes(internal: Arc<Internal>) -> axum::Router {
    axum::Router::new()
        .route(
            "/internal/store/{hash}/{name}",
            put(push_store).layer(axum::extract::DefaultBodyLimit::max(MOST_STORE_BYTES)),
        )
        .route(
            "/internal/state/{machine}/{name}",
            get(read_state)
                .put(write_state)
                .delete(remove_state)
                .layer(axum::extract::DefaultBodyLimit::max(MOST_STATE_BYTES)),
        )
        .route("/internal/wake", get(wake_now))
        .route("/internal/wake/{machine}", post(ask_wake))
        .route("/internal/wake/{machine}/plan", put(put_plan))
        .with_state(internal)
}

/// A blob's name as the machines write them: lowercase letters, digits and
/// `-`, at most 64.
pub fn is_blob_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

async fn push_store(
    State(internal): State<Arc<Internal>>,
    UrlPath((hash, name)): UrlPath<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signer = match internal
        .signed_by(&method, &uri, &headers, &body, &Machine::ALL)
        .await
    {
        Ok(signer) => signer,
        Err(refused) => return refused.into_response(),
    };
    let indexer = internal.indexer.clone();
    let (hash_kept, name_kept) = (hash.clone(), name.clone());
    let put = tokio::task::spawn_blocking(move || {
        indexer.store.put_checked(&hash_kept, &name_kept, &body)
    })
    .await
    .unwrap_or(Put::Failed);
    match put {
        Put::Kept => {
            tracing::info!(%signer, hash, name, "a file pushed to the store was kept");
            (StatusCode::CREATED, "kept").into_response()
        }
        Put::Held => (StatusCode::OK, "held already").into_response(),
        Put::Mismatch => {
            tracing::warn!(%signer, hash, name, "a file pushed to the store did not match its hash, and was not kept");
            (StatusCode::UNPROCESSABLE_ENTITY, "not what its hash says").into_response()
        }
        Put::OutOfOrder => (
            StatusCode::CONFLICT,
            "a component is taken only after its build's hashes.json",
        )
            .into_response(),
        Put::Refused => (StatusCode::BAD_REQUEST, "not a name the store takes").into_response(),
        Put::Failed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the file could not be kept",
        )
            .into_response(),
    }
}

/// The machine and blob a state route names, or why not.
fn state_target(machine: &str, name: &str) -> Result<(Machine, String), Refusal> {
    let machine = Machine::parse(machine).ok_or((StatusCode::NOT_FOUND, "no such machine"))?;
    if !is_blob_name(name) {
        return Err((StatusCode::BAD_REQUEST, "not a state's name"));
    }
    Ok((machine, name.to_string()))
}

async fn read_state(
    State(internal): State<Arc<Internal>>,
    UrlPath((machine, name)): UrlPath<(String, String)>,
) -> Response {
    let (machine, name) = match state_target(&machine, &name) {
        Ok(target) => target,
        Err(refused) => return refused.into_response(),
    };
    match tokio::fs::read(internal.blob(machine, &name)).await {
        Ok(bytes) => (StatusCode::OK, bytes).into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "no such state").into_response()
        }
        Err(error) => {
            tracing::warn!(%error, machine = machine.name(), name, "a state could not be read");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the state could not be read",
            )
                .into_response()
        }
    }
}

async fn write_state(
    State(internal): State<Arc<Internal>>,
    UrlPath((machine, name)): UrlPath<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (machine, name) = match state_target(&machine, &name) {
        Ok(target) => target,
        Err(refused) => return refused.into_response(),
    };
    if let Err(refused) = internal
        .signed_by(&method, &uri, &headers, &body, &[machine])
        .await
    {
        return refused.into_response();
    }
    let path = internal.blob(machine, &name);
    match tokio::task::spawn_blocking(move || write_whole(&path, &body)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => {
            tracing::warn!(
                error = format!("{error:#}"),
                machine = machine.name(),
                name,
                "a state could not be kept"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the state could not be kept",
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the state could not be kept",
        )
            .into_response(),
    }
}

async fn remove_state(
    State(internal): State<Arc<Internal>>,
    UrlPath((machine, name)): UrlPath<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (machine, name) = match state_target(&machine, &name) {
        Ok(target) => target,
        Err(refused) => return refused.into_response(),
    };
    if let Err(refused) = internal
        .signed_by(&method, &uri, &headers, &body, &[machine])
        .await
    {
        return refused.into_response();
    }
    match tokio::fs::remove_file(internal.blob(machine, &name)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "no such state").into_response()
        }
        Err(error) => {
            tracing::warn!(%error, machine = machine.name(), name, "a state could not be removed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the state could not be removed",
            )
                .into_response()
        }
    }
}

/// An ask to start a machine, from either machine (the build service asks
/// for the reviewer).
async fn ask_wake(
    State(internal): State<Arc<Internal>>,
    UrlPath(machine): UrlPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(machine) = Machine::parse(&machine) else {
        return (StatusCode::NOT_FOUND, "no such machine").into_response();
    };
    if let Err(refused) = internal
        .signed_by(&method, &uri, &headers, &body, &Machine::ALL)
        .await
    {
        return refused.into_response();
    }
    internal.wake.ask(machine);
    StatusCode::NO_CONTENT.into_response()
}

async fn put_plan(
    State(internal): State<Arc<Internal>>,
    UrlPath(machine): UrlPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(machine) = Machine::parse(&machine) else {
        return (StatusCode::NOT_FOUND, "no such machine").into_response();
    };
    if let Err(refused) = internal
        .signed_by(&method, &uri, &headers, &body, &[machine])
        .await
    {
        return refused.into_response();
    }
    let Ok(plan) = serde_json::from_slice::<Plan>(&body) else {
        return (StatusCode::BAD_REQUEST, "not a wake plan").into_response();
    };
    match internal.wake.set_plan(machine, plan) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                machine = machine.name(),
                "a wake plan could not be kept"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the plan could not be kept",
            )
                .into_response()
        }
    }
}

/// Which machines the waker should start, for the waker beside the API on
/// this machine alone.
async fn wake_now(
    State(internal): State<Arc<Internal>>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
) -> Response {
    if !from.ip().is_loopback() {
        return (StatusCode::FORBIDDEN, "asked on this machine alone").into_response();
    }
    let now = unix_now();
    let payments = internal.reads.payments().await;
    let wake = &internal.wake;
    let answer: BTreeMap<&str, bool> = Machine::ALL
        .into_iter()
        .map(|machine| {
            let payments = (machine == Machine::Builder).then_some(&payments);
            (
                machine.name(),
                wanted(
                    wake.asked(machine),
                    wake.plan(machine).as_ref(),
                    now,
                    payments,
                ),
            )
        })
        .collect();
    axum::Json(answer).into_response()
}

/// Write `bytes` to `path` whole or not at all: aside, then moved into place.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().context("a path with no directory")?;
    std::fs::create_dir_all(directory)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("a path with no name")?;
    let aside = directory.join(format!(".{name}.{}.part", std::process::id()));
    std::fs::write(&aside, bytes)?;
    std::fs::rename(&aside, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&aside);
    })?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use shonin_rootkey::service::Identity;
    use tower::ServiceExt;

    use super::*;

    /// Chains on which `blessed` are blessed as their machines, and which
    /// have taken `payments`.
    struct Stand {
        blessed: HashSet<(Address, Machine)>,
        payments: BTreeMap<String, u64>,
    }

    impl Reads for Stand {
        fn blessed(&self, signer: Address, machine: Machine) -> BoxFuture<'_, Result<bool>> {
            let blessed = self.blessed.contains(&(signer, machine));
            Box::pin(async move { Ok(blessed) })
        }

        fn payments(&self) -> BoxFuture<'_, BTreeMap<String, u64>> {
            let payments = self.payments.clone();
            Box::pin(async move { payments })
        }
    }

    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shonin-api-internal-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn builder_key() -> Identity {
        Identity::from_seed([1; 32])
    }

    fn reviewer_key() -> Identity {
        Identity::from_seed([2; 32])
    }

    fn stranger_key() -> Identity {
        Identity::from_seed([3; 32])
    }

    /// The internal port, its directory, the builder's and the reviewer's
    /// keys blessed as their kinds, and the payments the chains have taken.
    fn a_port(payments: &[(&str, u64)]) -> (Arc<Internal>, axum::Router, PathBuf) {
        let dir = scratch();
        let reads = Arc::new(Stand {
            blessed: [
                (builder_key().address(), Machine::Builder),
                (reviewer_key().address(), Machine::Reviewer),
            ]
            .into_iter()
            .collect(),
            payments: payments
                .iter()
                .map(|(chain, count)| (chain.to_string(), *count))
                .collect(),
        });
        let indexer = Arc::new(Indexer::new(Vec::new(), dir.join("store"), None));
        let internal = Arc::new(Internal::new(
            indexer,
            reads,
            Arc::new(Wake::new(Some(dir.join("plans")))),
            dir.join("blobs"),
        ));
        let app = routes(internal.clone());
        (internal, app, dir)
    }

    /// A request, signed by `key` at `at` if given.
    fn request(
        method: &str,
        path: &str,
        body: &[u8],
        signed: Option<(&Identity, u64)>,
    ) -> Request<Body> {
        let mut request = Request::builder().method(method).uri(path);
        if let Some((key, at)) = signed {
            request = request.header(AT_HEADER, at.to_string()).header(
                SIGNATURE_HEADER,
                key.sign_request(method, path, body, at).unwrap(),
            );
        }
        request.body(Body::from(body.to_vec())).unwrap()
    }

    async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
        let answer = app
            .clone()
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
            .oneshot(request)
            .await
            .unwrap();
        let status = answer.status();
        let body = axum::body::to_bytes(answer.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    const QUEUE: &str = "/internal/state/builder/queue";

    #[tokio::test]
    async fn an_unsigned_or_unblessed_write_is_refused_and_changes_nothing() {
        let (_, app, dir) = a_port(&[]);
        let now = unix_now();
        let blob = dir.join("blobs/builder/queue");

        // Unsigned.
        let (status, _) = send(&app, request("PUT", QUEUE, b"sealed", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Signed by a key nobody blessed.
        let (status, _) = send(
            &app,
            request("PUT", QUEUE, b"sealed", Some((&stranger_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Signed by the reviewer, for the builder's state.
        let (status, _) = send(
            &app,
            request("PUT", QUEUE, b"sealed", Some((&reviewer_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Signed by the builder too long ago.
        let (status, _) = send(
            &app,
            request(
                "PUT",
                QUEUE,
                b"sealed",
                Some((&builder_key(), now - MOST_SKEW_SECONDS - 5)),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Signed by the builder over other bytes than those sent.
        let mut altered = request("PUT", QUEUE, b"sealed", Some((&builder_key(), now)));
        *altered.body_mut() = Body::from("altered");
        let (status, _) = send(&app, altered).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the signature recovers another key"
        );
        // Signed by the builder for another path.
        let mut elsewhere = request(
            "PUT",
            "/internal/state/builder/payments",
            b"sealed",
            Some((&builder_key(), now)),
        );
        *elsewhere.uri_mut() = QUEUE.parse().unwrap();
        let (status, _) = send(&app, elsewhere).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!blob.exists());

        // Removing is a write too.
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"kept").unwrap();
        let (status, _) = send(
            &app,
            request("DELETE", QUEUE, b"", Some((&stranger_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(std::fs::read(&blob).unwrap(), b"kept");

        // A store push, a wake ask and a plan are refused unsigned as well.
        let path = format!("/internal/store/sha256-{}/source.tar", "a".repeat(64));
        assert_eq!(
            send(&app, request("PUT", &path, b"x", None)).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&app, request("POST", "/internal/wake/reviewer", b"", None))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(
                &app,
                request(
                    "POST",
                    "/internal/wake/reviewer",
                    b"",
                    Some((&stranger_key(), now))
                )
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let plan = br#"{"busy":true,"renew_at":null,"payments_seen":{}}"#;
        assert_eq!(
            send(
                &app,
                request("PUT", "/internal/wake/builder/plan", plan, None)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_machine_s_state_is_kept_whole_read_back_unsigned_and_each_signature_taken_once() {
        let (_, app, dir) = a_port(&[]);
        let now = unix_now();
        let put = request("PUT", QUEUE, b"sealed queue", Some((&builder_key(), now)));
        let again = request("PUT", QUEUE, b"sealed queue", Some((&builder_key(), now)));
        assert_eq!(send(&app, put).await.0, StatusCode::NO_CONTENT);
        // The very same request, sent again: refused.
        assert_eq!(send(&app, again).await.0, StatusCode::UNAUTHORIZED);

        let (status, body) = send(&app, request("GET", QUEUE, b"", None)).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::OK, b"sealed queue".as_slice())
        );
        assert_eq!(
            send(
                &app,
                request("GET", "/internal/state/reviewer/pending", b"", None)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        // No temporary file is left beside it.
        let left: Vec<_> = std::fs::read_dir(dir.join("blobs/builder"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["queue"]);

        // A name the machines don't write, and a machine there isn't.
        for path in [
            "/internal/state/builder/Queue",
            "/internal/state/builder/..",
            "/internal/state/builder/queue.json",
        ] {
            let (status, _) = send(
                &app,
                request("PUT", path, b"x", Some((&builder_key(), now))),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        }
        assert_eq!(
            send(
                &app,
                request("GET", "/internal/state/executor/queue", b"", None)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );

        assert_eq!(
            send(
                &app,
                request("DELETE", QUEUE, b"", Some((&builder_key(), now)))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            send(&app, request("GET", QUEUE, b"", None)).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(
                &app,
                request("DELETE", QUEUE, b"", Some((&builder_key(), now + 1)))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A signed request taken once is refused again with its signature
    /// written in any other form that recovers the same signer: `v` as 0 or
    /// 1 in place of 27 or 28, and `s` as `n - s` with the parity flipped.
    #[tokio::test]
    async fn a_request_taken_is_refused_again_whatever_form_its_signature_is_written_in() {
        use alloy::primitives::U256;

        // The secp256k1 group order.
        let n = U256::from_str_radix(
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141",
            16,
        )
        .unwrap();
        let other_v = |signature: &[u8]| {
            let mut out = signature.to_vec();
            out[64] = match signature[64] {
                27 => 0,
                28 => 1,
                0 => 27,
                1 => 28,
                other => other,
            };
            out
        };
        let flipped_s = |signature: &[u8]| {
            let mut out = signature.to_vec();
            let s = U256::from_be_slice(&signature[32..64]);
            out[32..64].copy_from_slice(&(n - s).to_be_bytes::<32>());
            out[64] = match signature[64] {
                27 => 28,
                28 => 27,
                0 => 1,
                1 => 0,
                other => other,
            };
            out
        };

        let (_, app, dir) = a_port(&[]);
        let now = unix_now();
        let signature = alloy::hex::decode(
            builder_key()
                .sign_request("PUT", QUEUE, b"sealed", now)
                .unwrap(),
        )
        .unwrap();
        let sent = |signature: Vec<u8>| {
            Request::builder()
                .method("PUT")
                .uri(QUEUE)
                .header(AT_HEADER, now.to_string())
                .header(SIGNATURE_HEADER, alloy::hex::encode(signature))
                .body(Body::from(b"sealed".to_vec()))
                .unwrap()
        };
        assert_eq!(
            send(&app, sent(signature.clone())).await.0,
            StatusCode::NO_CONTENT
        );
        for (how, written) in [
            ("v written the other way", other_v(&signature)),
            ("s flipped to n - s", flipped_s(&signature)),
            ("both", other_v(&flipped_s(&signature))),
        ] {
            let (status, body) = send(&app, sent(written)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{how}");
            assert_eq!(
                String::from_utf8_lossy(&body),
                "this request was taken already",
                "{how}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_pushed_file_not_matching_its_hash_is_not_kept() {
        let (_, app, dir) = a_port(&[]);
        let now = unix_now();
        let source = b"the author's source".to_vec();
        let hash = shared::manifest::hash_bytes(&source).replace(':', "-");
        let other = shared::manifest::hash_bytes(b"other").replace(':', "-");

        let path = format!("/internal/store/{other}/source.tar");
        let (status, _) = send(
            &app,
            request("PUT", &path, &source, Some((&builder_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!dir.join("store").join(&other).join("source.tar").exists());

        let path = format!("/internal/store/{hash}/source.tar");
        let (status, _) = send(
            &app,
            request("PUT", &path, &source, Some((&builder_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        // The reviewer may push too; held already.
        let (status, _) = send(
            &app,
            request("PUT", &path, &source, Some((&reviewer_key(), now))),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            std::fs::read(dir.join("store").join(&hash).join("source.tar")).unwrap(),
            source
        );

        // Never a sealed name.
        let path = format!("/internal/store/{hash}/source.sealed.tar");
        let (status, _) = send(
            &app,
            request("PUT", &path, &source, Some((&builder_key(), now + 1))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A component before its build's record.
        let path = format!("/internal/store/{hash}/component.wasm");
        let (status, _) = send(
            &app,
            request("PUT", &path, b"\0asm", Some((&builder_key(), now + 2))),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_wake_rule_wakes_for_an_ask_a_busy_stop_a_renewal_or_an_unread_payment() {
        let now = 1_000_000;
        let plan = |busy: bool, renew_at: Option<u64>, seen: &[(&str, u64)]| Plan {
            busy,
            renew_at,
            payments_seen: seen.iter().map(|(c, n)| (c.to_string(), *n)).collect(),
        };
        let paid = |taken: &[(&str, u64)]| -> BTreeMap<String, u64> {
            taken.iter().map(|(c, n)| (c.to_string(), *n)).collect()
        };
        let idle = plan(false, Some(now + 100), &[("84532", 3), ("998", 0)]);

        // Asleep, nothing new.
        assert!(!wanted(
            false,
            Some(&idle),
            now,
            Some(&paid(&[("84532", 3), ("998", 0)]))
        ));
        // An ask.
        assert!(wanted(true, Some(&idle), now, Some(&paid(&[]))));
        // It stopped busy.
        assert!(wanted(false, Some(&plan(true, None, &[])), now, None));
        // Its blessing is due.
        assert!(wanted(false, Some(&idle), now + 100, None));
        assert!(!wanted(false, Some(&idle), now + 99, None));
        // A payment past what it read, on one chain.
        assert!(wanted(
            false,
            Some(&idle),
            now,
            Some(&paid(&[("84532", 3), ("998", 1)]))
        ));
        // A chain it doesn't serve wakes nothing; a chain that didn't answer
        // is no payment.
        assert!(!wanted(
            false,
            Some(&idle),
            now,
            Some(&paid(&[("46630", 9)]))
        ));
        assert!(!wanted(false, Some(&idle), now, Some(&paid(&[]))));
        // Never heard from: any payment wakes it, and nothing else does.
        assert!(wanted(false, None, now, Some(&paid(&[("84532", 1)]))));
        assert!(!wanted(false, None, now, Some(&paid(&[("84532", 0)]))));
        // The reviewer has no payments clause.
        assert!(!wanted(false, None, now, None));
    }

    #[tokio::test]
    async fn the_waker_is_told_by_the_rule_on_this_machine_alone_and_plans_outlive_a_restart() {
        let (internal, app, dir) = a_port(&[("84532", 4)]);
        let now = unix_now();
        let ask = |app: axum::Router| async move {
            let (status, body) = send(&app, request("GET", "/internal/wake", b"", None)).await;
            assert_eq!(status, StatusCode::OK);
            serde_json::from_slice::<BTreeMap<String, bool>>(&body).unwrap()
        };
        // Never heard from, and a payment taken: the builder.
        let told = ask(app.clone()).await;
        assert_eq!(told.get("builder"), Some(&true));
        assert_eq!(told.get("reviewer"), Some(&false));

        // The builder read all four, and the reviewer stopped busy.
        let plan = br#"{"busy":false,"renew_at":null,"payments_seen":{"84532":4}}"#;
        let put = request(
            "PUT",
            "/internal/wake/builder/plan",
            plan,
            Some((&builder_key(), now)),
        );
        assert_eq!(send(&app, put).await.0, StatusCode::NO_CONTENT);
        let plan = br#"{"busy":true,"renew_at":null,"payments_seen":{}}"#;
        let put = request(
            "PUT",
            "/internal/wake/reviewer/plan",
            plan,
            Some((&reviewer_key(), now)),
        );
        assert_eq!(send(&app, put).await.0, StatusCode::NO_CONTENT);
        let told = ask(app.clone()).await;
        assert_eq!(told.get("builder"), Some(&false));
        assert_eq!(told.get("reviewer"), Some(&true));
        // Not a plan.
        let put = request(
            "PUT",
            "/internal/wake/builder/plan",
            b"{}",
            Some((&builder_key(), now)),
        );
        assert_eq!(send(&app, put).await.0, StatusCode::BAD_REQUEST);

        // The builder asks for the reviewer; nobody asked for the builder.
        let post = request(
            "POST",
            "/internal/wake/builder",
            b"",
            Some((&builder_key(), now + 1)),
        );
        assert_eq!(send(&app, post).await.0, StatusCode::NO_CONTENT);
        assert_eq!(ask(app.clone()).await.get("builder"), Some(&true));

        // From anywhere but this machine, nothing is said.
        let answer = app
            .clone()
            .layer(MockConnectInfo(SocketAddr::from(([10, 164, 0, 7], 40000))))
            .oneshot(request("GET", "/internal/wake", b"", None))
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::FORBIDDEN);

        // A restart of the API keeps the plans (and forgets the asks).
        drop(internal);
        let wake = Wake::new(Some(dir.join("plans")));
        assert!(wake.plan(Machine::Reviewer).is_some_and(|plan| plan.busy));
        assert_eq!(
            wake.plan(Machine::Builder).map(|plan| plan.payments_seen),
            Some([("84532".to_string(), 4)].into_iter().collect())
        );
        assert!(!wake.asked(Machine::Builder));
        let _ = std::fs::remove_dir_all(dir);
    }
}
