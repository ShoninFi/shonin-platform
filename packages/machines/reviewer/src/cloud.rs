//! What a machine that runs on demand keeps off itself, and how it asks the
//! platform's API for it.
//!
//! The build service and the reviewer each start with a fresh disk and may
//! power off whenever they are idle, so what must outlive them (the queue,
//! where payments were read to, archives of unfinished jobs, reviews waiting
//! to be posted) is kept on the API's internal port as blobs: sealed by the
//! machine, under its own key, before they leave it, and each write signed,
//! so only the machine can read them and nobody else can change them.
//!
//! A blob that does not open under the machine's key was sealed under an
//! earlier one (a new image or a new root): it is set aside under a name of
//! its own, kept for whoever wants to look at it, and the machine carries on
//! without it, saying so loudly. A blob under the machine's own key that does
//! not open is damage, and stops the machine rather than lose what it held.
//!
//! The same port takes the built files into the API's store, the asks to
//! wake a machine, and each machine's wake plan: whether it stopped busy,
//! when its blessing must be renewed, and how far it has read payments.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, keccak256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use zeroize::Zeroizing;

/// The headers a signed request carries its time and its signature in, and
/// the digest it is signed under (the URL's path alone, no host or query).
pub use shonin_rootkey::service::{AT_HEADER, SIGNATURE_HEADER, request_digest};

/// How far a signed request's time may be from the receiver's clock.
pub const MOST_SKEW_SECONDS: u64 = 120;

/// Who signed a request, from its signature over [`request_digest`] as an
/// Ethereum signed message.
pub fn request_signer(
    method: &str,
    path: &str,
    body: &[u8],
    at: u64,
    signature: &[u8],
) -> Result<Address> {
    shonin_rootkey::service::request_signer(method, path, body, at, signature)
        .map_err(|_| anyhow!("the signature recovers no signer"))
}

/// Seconds since 1970, by this machine's clock.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Why a blob did not open.
#[derive(Debug, PartialEq, Eq)]
pub enum Unopened {
    /// It was sealed under another key.
    OtherKey,
    /// It was sealed under this key, and is damaged.
    Corrupt,
}

/// Sealing a machine's state under its own key, the blob's name bound in.
pub trait Sealing: Send + Sync {
    fn seal_state(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>>;
    fn open_state(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, Unopened>;
}

impl Sealing for shonin_rootkey::service::Identity {
    fn seal_state(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        shonin_rootkey::service::Identity::seal_state(self, name, plain)
            .map_err(|error| anyhow!("the state {name} could not be sealed: {error}"))
    }

    fn open_state(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, Unopened> {
        use shonin_rootkey::service::StateError;

        shonin_rootkey::service::Identity::open_state(self, name, sealed).map_err(|error| {
            match error {
                StateError::OtherKey => Unopened::OtherKey,
                StateError::Corrupt => Unopened::Corrupt,
            }
        })
    }
}

/// The key a machine signs its requests with: one it holds, or (on a
/// development chain) a role key the node holds for it.
#[derive(Clone)]
pub enum Signer {
    Key(PrivateKeySigner),
    Node {
        client: chain::Client,
        address: Address,
    },
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signer({})", self.address())
    }
}

impl Signer {
    pub fn address(&self) -> Address {
        match self {
            Self::Key(key) => key.address(),
            Self::Node { address, .. } => *address,
        }
    }

    /// Sign `digest` as an Ethereum signed message.
    pub async fn sign(&self, digest: B256) -> Result<Bytes> {
        match self {
            Self::Key(key) => Ok(key.sign_message_sync(digest.as_slice())?.as_bytes().into()),
            Self::Node { client, address } => client.sign(*address, digest).await,
        }
    }
}

/// What a machine tells the API about itself, for the waker: whether it
/// stopped busy, when its blessing is next due, and how far it has read
/// payments on each chain, by chain id.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakePlan {
    pub busy: bool,
    pub renew_at: Option<u64>,
    pub payments_seen: BTreeMap<String, u64>,
}

/// The API's internal port, as one machine (`builder` or `reviewer`) uses
/// it.
#[derive(Clone)]
pub struct Internal {
    base: reqwest::Url,
    machine: &'static str,
    http: reqwest::Client,
    signer: Signer,
    sealing: Arc<dyn Sealing>,
}

impl std::fmt::Debug for Internal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Internal")
            .field("base", &self.base.as_str())
            .field("machine", &self.machine)
            .field("signer", &self.signer)
            .finish()
    }
}

impl Internal {
    pub fn new(
        base: &str,
        machine: &'static str,
        signer: Signer,
        sealing: Arc<dyn Sealing>,
    ) -> Result<Self> {
        let base = reqwest::Url::parse(base)
            .with_context(|| format!("the state URL {base} is not a URL"))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            base,
            machine,
            http,
            signer,
            sealing,
        })
    }

    pub fn signer(&self) -> &Signer {
        &self.signer
    }

    fn url(&self, path: &str) -> Result<reqwest::Url> {
        Ok(self.base.join(path)?)
    }

    /// A request to `path`, carrying `body`, signed when it changes anything.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        wait: Duration,
    ) -> Result<reqwest::Response> {
        let url = self.url(path)?;
        let mut request = self.http.request(method.clone(), url.clone()).timeout(wait);
        if method != Method::GET {
            let at = now();
            let digest = request_digest(method.as_str(), url.path(), &body, at);
            let signature = self.signer.sign(digest).await?;
            request = request
                .header(AT_HEADER, at.to_string())
                .header(SIGNATURE_HEADER, alloy::hex::encode_prefixed(&signature));
        }
        if !body.is_empty() {
            request = request.body(body);
        }
        request.send().await.map_err(|error| {
            anyhow!(
                "the API's internal port did not answer: {}",
                error.without_url()
            )
        })
    }

    fn state_path(&self, name: &str) -> String {
        format!("/internal/state/{}/{name}", self.machine)
    }

    /// The blob `name`, opened: `None` if there is none, or if it was sealed
    /// under another key (it is then set aside, and that is said). Damage
    /// under this machine's own key is an error.
    pub async fn read_state(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let answer = self
            .send(
                Method::GET,
                &self.state_path(name),
                Vec::new(),
                Duration::from_secs(120),
            )
            .await?;
        match answer.status() {
            StatusCode::NOT_FOUND => return Ok(None),
            status if !status.is_success() => {
                bail!("the API answered {status} for the state {name}")
            }
            _ => {}
        }
        let sealed = answer
            .bytes()
            .await
            .map_err(|error| anyhow!("the state {name} was cut short: {}", error.without_url()))?;
        match self.sealing.open_state(name, &sealed) {
            Ok(plain) => Ok(Some(plain)),
            Err(Unopened::OtherKey) => {
                let aside = format!("{name}-orphan-{}", now());
                self.put_state_raw(&aside, sealed.to_vec()).await?;
                self.remove_state(name).await?;
                tracing::error!(
                    name,
                    aside,
                    machine = self.machine,
                    "the state was sealed under another key (a new image or a new root): it is \
                     set aside, and this machine starts without it"
                );
                Ok(None)
            }
            Err(Unopened::Corrupt) => bail!(
                "the state {name} is sealed under this machine's key and does not open: it is \
                 damaged, so the machine is not starting rather than lose what it holds"
            ),
        }
    }

    /// Seal `plain` and keep it as the blob `name`, replacing what was there.
    pub async fn write_state(&self, name: &str, plain: &[u8]) -> Result<()> {
        let sealed = self.sealing.seal_state(name, plain)?;
        self.put_state_raw(name, sealed).await
    }

    async fn put_state_raw(&self, name: &str, sealed: Vec<u8>) -> Result<()> {
        let answer = self
            .send(
                Method::PUT,
                &self.state_path(name),
                sealed,
                Duration::from_secs(120),
            )
            .await?;
        let status = answer.status();
        anyhow::ensure!(
            status.is_success(),
            "the API answered {status} keeping the state {name}"
        );
        Ok(())
    }

    /// Remove the blob `name`; one already gone is no failure.
    pub async fn remove_state(&self, name: &str) -> Result<()> {
        let answer = self
            .send(
                Method::DELETE,
                &self.state_path(name),
                Vec::new(),
                Duration::from_secs(60),
            )
            .await?;
        let status = answer.status();
        anyhow::ensure!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "the API answered {status} removing the state {name}"
        );
        Ok(())
    }

    /// Tell the API this machine's wake plan. A plan the API refuses for
    /// want of the key's blessing is [`NotBlessed`].
    pub async fn put_plan(&self, plan: &WakePlan) -> Result<()> {
        let answer = self
            .send(
                Method::PUT,
                &format!("/internal/wake/{}/plan", self.machine),
                serde_json::to_vec(plan)?,
                Duration::from_secs(30),
            )
            .await?;
        let status = answer.status();
        if status == StatusCode::FORBIDDEN {
            return Err(NotBlessed.into());
        }
        anyhow::ensure!(
            status.is_success(),
            "the API answered {status} to the wake plan"
        );
        Ok(())
    }

    /// Ask the API to wake `machine`.
    pub async fn wake(&self, machine: &str) -> Result<()> {
        let answer = self
            .send(
                Method::POST,
                &format!("/internal/wake/{machine}"),
                Vec::new(),
                Duration::from_secs(30),
            )
            .await?;
        let status = answer.status();
        anyhow::ensure!(
            status.is_success(),
            "the API answered {status} to the ask to wake {machine}"
        );
        Ok(())
    }

    /// Put `bytes` into the API's store as `name` under `hash` (as the store
    /// writes it, `sha256-<hex>`). Kept, or held already, is done; anything
    /// else is an error.
    pub async fn push_store(&self, hash: &str, name: &str, bytes: Vec<u8>) -> Result<()> {
        let answer = self
            .send(
                Method::PUT,
                &format!("/internal/store/{hash}/{name}"),
                bytes,
                Duration::from_secs(10 * 60),
            )
            .await?;
        let status = answer.status();
        anyhow::ensure!(
            status.is_success(),
            "the API's store answered {status} for {hash}/{name}"
        );
        Ok(())
    }
}

/// The turns of a machine's work loops (reading payments, taking jobs,
/// posting reviews), closed before it writes its state down and exits: once
/// closed, every turn under way has ended and no other starts, so nothing
/// changes the state after it is written.
pub struct Turns(tokio::sync::RwLock<bool>);

impl Default for Turns {
    fn default() -> Self {
        Self(tokio::sync::RwLock::new(true))
    }
}

impl Turns {
    /// A turn of a work loop, held while it lasts; none once closed, and the
    /// loop then ends.
    pub async fn turn(&self) -> Option<tokio::sync::RwLockReadGuard<'_, bool>> {
        let open = self.0.read().await;
        (*open).then_some(open)
    }

    /// Close: wait for every turn under way to end, and start no other.
    pub async fn close(&self) {
        *self.0.write().await = false;
    }
}

/// The API's refusal of a wake plan signed by a key it does not count as
/// blessed yet: the API takes plans only from a key blessed on the canonical
/// chain.
#[derive(Debug)]
pub struct NotBlessed;

impl std::fmt::Display for NotBlessed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the API takes the wake plan once the key is blessed")
    }
}

impl std::error::Error for NotBlessed {}

/// The attestation of a machine that is no vault's: its key blessed on each
/// chain it serves, by tokens from Confidential Space's launcher, and its
/// status served for the prover and the keeper.
pub trait Attesting: Send + Sync {
    /// Get the key blessed on this chain too, and keep it blessed.
    fn serve_chain(&self, client: chain::Client, deployments: chain::Deployments);
    /// Whether the key is blessed on the chain with this id, as last read.
    fn ready(&self, chain_id: u64) -> bool;
    /// When the blessing must next be renewed, on whichever chain is first.
    fn renew_at(&self) -> Option<u64>;
    /// Whether a blessing is due and not yet done.
    fn busy(&self) -> bool;
    /// The status routes the prover and the keeper read.
    fn routes(&self) -> axum::Router;
}

impl Attesting for shonin_rootkey::service::ServiceAttestation {
    fn serve_chain(&self, client: chain::Client, deployments: chain::Deployments) {
        shonin_rootkey::service::ServiceAttestation::serve_chain(self, client, deployments)
    }

    fn ready(&self, chain_id: u64) -> bool {
        shonin_rootkey::service::ServiceAttestation::ready(self, chain_id)
    }

    fn renew_at(&self) -> Option<u64> {
        shonin_rootkey::service::ServiceAttestation::renew_at(self)
    }

    fn busy(&self) -> bool {
        shonin_rootkey::service::ServiceAttestation::busy(self)
    }

    fn routes(&self) -> axum::Router {
        shonin_rootkey::service::ServiceAttestation::routes(self)
    }
}

/// A machine as the key holder makes it in the attested image: its own
/// signing key, its state's sealing, and its attestation.
pub struct Launched {
    pub key: PrivateKeySigner,
    pub sealing: Arc<dyn Sealing>,
    pub attestation: Arc<dyn Attesting>,
}

/// The machine of `kind` as the key holder at `holder` makes it, asked by
/// attested exchange with a token from the launcher at `socket`.
pub async fn launch(
    kind: shonin_rootkey::ImageKind,
    holder: &str,
    socket: &std::path::Path,
) -> Result<Launched> {
    let identity = shonin_rootkey::service::identity(holder, socket).await?;
    let attestation = shonin_rootkey::service::ServiceAttestation::new(kind, identity.address())?;
    tokio::spawn(attestation.clone().run(socket.to_path_buf()));
    Ok(Launched {
        key: identity.signer(),
        sealing: Arc::new(identity),
        attestation: Arc::new(attestation),
    })
}

/// The image this machine runs, as Confidential Space's launcher at `socket`
/// attests it: the image digest a token it gives now names. The token is
/// asked for with a nonce naming the machine's own key `signer`, and is
/// read here only, never sent on.
pub async fn attested_image(socket: &std::path::Path, signer: Address) -> Result<B256> {
    let token = shonin_rootkey::launcher::request_token(socket, &format!("{signer:x}")).await?;
    token_image(&token)
}

/// The image digest a launcher token's claims name
/// (`submods.container.image_digest`, `sha256:<hex>`), read without checking
/// the token: it came from the launcher over its own socket.
fn token_image(token: &str) -> Result<B256> {
    let claims = token.split('.').nth(1).context("the token is not a JWT")?;
    // The claims are URL-safe base64, which the standard alphabet reads once
    // its two other characters are put back.
    let standard: String = claims
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    let bytes = shared::base64::decode(&standard).map_err(|error| anyhow!(error))?;
    let claims: serde_json::Value =
        serde_json::from_slice(&bytes).context("the token's claims are not JSON")?;
    claims
        .pointer("/submods/container/image_digest")
        .and_then(serde_json::Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .context("the token names no image digest")?
        .parse()
        .context("the token's image digest is not 32 bytes")
}

/// The API's internal port for a machine in development mode: its requests
/// signed by its role key, `role` of the chain the arguments resolve to (the
/// node may hold it), and its state sealed under a key derived from that
/// role key's signature over a fixed label, so the same role key opens it
/// again.
pub async fn development(
    url: &str,
    machine: &'static str,
    chain_args: &chain::ChainArgs,
    role: fn(&chain::Roles) -> Address,
) -> Result<Internal> {
    let target = chain_args.resolve()?;
    let client = target.connect().await?;
    let deployments = client
        .deployments(&chain::Deployments::file_in(
            &target.deployments_dir,
            &target.key,
        ))
        .await?;
    let signer = Signer::Node {
        client,
        address: role(&deployments.roles),
    };
    let signature = signer
        .sign(keccak256(format!("shonin:development-state:v1:{machine}")))
        .await?;
    // The signature is the same each time (deterministic ECDSA), so the
    // seed, and the sealing it gives, is too.
    let seed = Zeroizing::new(keccak256(&signature).0);
    let identity =
        shonin_rootkey::service::Identity::from_received(shonin_rootkey::Received::Seed(seed))?;
    Internal::new(url, machine, signer, Arc::new(identity))
}

/// Roughly what one of a service key's larger sends uses, as the keeper
/// stocks keys for.
pub const ONE_SEND_GAS: u128 = 300_000;

/// Whether `key` holds gas for two of its larger sends at today's fees on
/// `chain`: below that, a paid job could fail for want of gas, so no work is
/// taken there.
pub async fn holds_gas(chain: &chain::Client, key: Address) -> Result<bool> {
    use alloy::providers::Provider as _;

    let per_gas = match chain.provider().estimate_eip1559_fees().await {
        Ok(fees) => fees.max_fee_per_gas,
        Err(_) => chain
            .provider()
            .get_gas_price()
            .await
            .context("reading the gas price")?,
    };
    let needed = alloy::primitives::U256::from(per_gas.saturating_mul(ONE_SEND_GAS * 2));
    Ok(chain.balance(key).await? >= needed)
}

/// When a machine last had anything to do, and so whether it has been idle
/// long enough to exit.
pub struct Idle {
    after: Option<Duration>,
    since: std::sync::Mutex<std::time::Instant>,
}

impl Idle {
    /// Idle exit after `after` with nothing to do; never, without it.
    pub fn new(after: Option<Duration>) -> Self {
        Self {
            after,
            since: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// Note whether the machine has anything to do now, and say whether it
    /// should exit: only once it has had nothing to do for the whole while.
    pub fn should_exit(&self, busy: bool) -> bool {
        let mut since = self.since.lock().unwrap_or_else(|e| e.into_inner());
        if busy {
            *since = std::time::Instant::now();
            return false;
        }
        self.after.is_some_and(|after| since.elapsed() >= after)
    }
}

#[cfg(test)]
pub mod stand_in {
    //! A stand-in for the API's internal port, and a stand-in sealing.

    use super::*;
    use axum::body::Bytes as Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method as HttpMethod, StatusCode as Status, Uri};
    use std::sync::Mutex;

    /// Sealing under `key`: the key's tag, then the plaintext, then a tag
    /// over the key, the name and the plaintext. Not secret, but it tells
    /// another key from damage, as the real sealing does.
    pub struct Sealed(pub [u8; 32]);

    impl Sealing for Sealed {
        fn seal_state(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
            let mut out = keccak256(self.0)[..8].to_vec();
            out.extend_from_slice(plain);
            out.extend_from_slice(
                keccak256([&self.0[..], name.as_bytes(), plain].concat()).as_slice(),
            );
            Ok(out)
        }

        fn open_state(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, Unopened> {
            if sealed.len() < 40 || sealed[..8] != keccak256(self.0)[..8] {
                return Err(Unopened::OtherKey);
            }
            let plain = &sealed[8..sealed.len() - 32];
            let tag = keccak256([&self.0[..], name.as_bytes(), plain].concat());
            if sealed[sealed.len() - 32..] != tag[..] {
                return Err(Unopened::Corrupt);
            }
            Ok(Zeroizing::new(plain.to_vec()))
        }
    }

    /// What the stand-in API holds and was asked.
    #[derive(Default)]
    pub struct Api {
        pub blobs: Mutex<BTreeMap<String, Vec<u8>>>,
        pub plans: Mutex<Vec<WakePlan>>,
        pub wakes: Mutex<Vec<String>>,
        pub store: Mutex<BTreeMap<String, Vec<u8>>>,
        /// Who signed each changing request, by method and path.
        pub signers: Mutex<Vec<(String, String, Address)>>,
        /// Answer every changing request with this, if set.
        pub refuse: Mutex<Option<u16>>,
    }

    async fn any(
        State(api): State<Arc<Api>>,
        method: HttpMethod,
        uri: Uri,
        headers: HeaderMap,
        body: Body,
    ) -> (Status, Vec<u8>) {
        let path = uri.path().to_string();
        if method != HttpMethod::GET {
            if let Some(code) = *api.refuse.lock().unwrap() {
                return (Status::from_u16(code).unwrap(), Vec::new());
            }
            let at: u64 = headers[AT_HEADER].to_str().unwrap().parse().unwrap();
            let signature =
                alloy::hex::decode(headers[SIGNATURE_HEADER].to_str().unwrap()).unwrap();
            let Ok(signer) = request_signer(method.as_str(), &path, &body, at, &signature) else {
                return (Status::UNAUTHORIZED, Vec::new());
            };
            api.signers
                .lock()
                .unwrap()
                .push((method.to_string(), path.clone(), signer));
        }
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (method.as_str(), parts.as_slice()) {
            ("GET", ["internal", "state", machine, name]) => {
                match api.blobs.lock().unwrap().get(&format!("{machine}/{name}")) {
                    Some(bytes) => (Status::OK, bytes.clone()),
                    None => (Status::NOT_FOUND, Vec::new()),
                }
            }
            ("PUT", ["internal", "state", machine, name]) => {
                api.blobs
                    .lock()
                    .unwrap()
                    .insert(format!("{machine}/{name}"), body.to_vec());
                (Status::NO_CONTENT, Vec::new())
            }
            ("DELETE", ["internal", "state", machine, name]) => {
                match api
                    .blobs
                    .lock()
                    .unwrap()
                    .remove(&format!("{machine}/{name}"))
                {
                    Some(_) => (Status::NO_CONTENT, Vec::new()),
                    None => (Status::NOT_FOUND, Vec::new()),
                }
            }
            ("PUT", ["internal", "wake", _, "plan"]) => {
                api.plans
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                (Status::NO_CONTENT, Vec::new())
            }
            ("POST", ["internal", "wake", machine]) => {
                api.wakes.lock().unwrap().push(machine.to_string());
                (Status::NO_CONTENT, Vec::new())
            }
            ("PUT", ["internal", "store", hash, name]) => {
                api.store
                    .lock()
                    .unwrap()
                    .insert(format!("{hash}/{name}"), body.to_vec());
                (Status::CREATED, Vec::new())
            }
            _ => (Status::NOT_FOUND, Vec::new()),
        }
    }

    /// Serve a stand-in API; where it is, and what it holds.
    pub async fn serve() -> (String, Arc<Api>) {
        let api = Arc::new(Api::default());
        let app = axum::Router::new().fallback(any).with_state(api.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, api)
    }
}

#[cfg(test)]
mod tests {
    use super::stand_in::{Sealed, serve};
    use super::*;

    fn internal(url: &str, key: [u8; 32], signer: &PrivateKeySigner) -> Internal {
        Internal::new(
            url,
            "builder",
            Signer::Key(signer.clone()),
            Arc::new(Sealed(key)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn state_written_is_read_back_and_every_write_is_signed() {
        let (url, api) = serve().await;
        let key = PrivateKeySigner::random();
        let one = internal(&url, [1; 32], &key);
        assert!(one.read_state("queue").await.unwrap().is_none());
        one.write_state("queue", b"the queue").await.unwrap();
        // A machine started afresh under the same key reads it back.
        let again = internal(&url, [1; 32], &key);
        assert_eq!(
            again.read_state("queue").await.unwrap().unwrap().as_slice(),
            b"the queue"
        );
        let signers = api.signers.lock().unwrap();
        assert!(!signers.is_empty());
        assert!(
            signers.iter().all(|(_, path, signer)| {
                *signer == key.address() && path == "/internal/state/builder/queue"
            }),
            "{signers:?}"
        );
    }

    /// The image a launcher token names is read from its claims.
    #[test]
    fn the_image_is_read_from_the_token_s_claims() {
        let digest = "ab".repeat(32);
        let claims = serde_json::json!({
            "iat": 1,
            "submods": {"container": {"image_digest": format!("sha256:{digest}")}},
        });
        let encoded: String = shared::base64::encode(claims.to_string().as_bytes())
            .trim_end_matches('=')
            .chars()
            .map(|c| match c {
                '+' => '-',
                '/' => '_',
                other => other,
            })
            .collect();
        let token = format!("eyJhbGciOiJSUzI1NiJ9.{encoded}.c2lnbmF0dXJl");
        assert_eq!(
            token_image(&token).unwrap(),
            digest.parse::<B256>().unwrap()
        );
        assert!(token_image("eyJ9.e30.c2ln").is_err(), "names no image");
        assert!(token_image("not a token").is_err());
    }

    /// Closing the turns waits for the one under way to end, and no turn
    /// starts after.
    #[tokio::test]
    async fn once_closed_no_turn_is_under_way_or_starts() {
        let turns = Arc::new(Turns::default());
        let turn = turns.turn().await.expect("open");
        let closing = tokio::spawn({
            let turns = turns.clone();
            async move { turns.close().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!closing.is_finished(), "closed while a turn was under way");
        drop(turn);
        closing.await.unwrap();
        assert!(turns.turn().await.is_none());
    }

    /// A wake plan the API refuses as signed by a key it does not count as
    /// blessed is told apart from any other refusal.
    #[tokio::test]
    async fn a_plan_refused_for_want_of_a_blessing_is_told_apart() {
        let (url, api) = serve().await;
        let one = internal(&url, [1; 32], &PrivateKeySigner::random());
        let plan = WakePlan {
            busy: false,
            renew_at: None,
            payments_seen: BTreeMap::new(),
        };
        *api.refuse.lock().unwrap() = Some(403);
        let refused = one.put_plan(&plan).await.unwrap_err();
        assert!(refused.is::<NotBlessed>(), "{refused:#}");
        *api.refuse.lock().unwrap() = Some(500);
        let failed = one.put_plan(&plan).await.unwrap_err();
        assert!(!failed.is::<NotBlessed>(), "{failed:#}");
        *api.refuse.lock().unwrap() = None;
        one.put_plan(&plan).await.unwrap();
        assert_eq!(api.plans.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_blob_under_another_key_is_set_aside_not_fatal() {
        let (url, api) = serve().await;
        let key = PrivateKeySigner::random();
        internal(&url, [1; 32], &key)
            .write_state("queue", b"old")
            .await
            .unwrap();
        let sealed_before = api.blobs.lock().unwrap()["builder/queue"].clone();

        // A new image: a new key.
        let fresh = internal(&url, [2; 32], &key);
        assert!(fresh.read_state("queue").await.unwrap().is_none());
        let blobs = api.blobs.lock().unwrap();
        assert!(
            !blobs.contains_key("builder/queue"),
            "the old blob is still in the way"
        );
        let aside: Vec<_> = blobs
            .iter()
            .filter(|(name, _)| name.starts_with("builder/queue-orphan-"))
            .collect();
        assert_eq!(aside.len(), 1, "the old blob was not set aside");
        assert_eq!(
            aside[0].1, &sealed_before,
            "what was set aside is not as it was"
        );
        let name = aside[0].0.trim_start_matches("builder/");
        assert!(
            name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{name} is not a name the API takes"
        );
    }

    /// With the key holder's own sealing: a blob sealed under one image's
    /// identity is set aside by another's, and read back by its own.
    #[tokio::test]
    async fn a_blob_under_another_service_identity_is_set_aside() {
        use shonin_rootkey::service::Identity;

        let (url, api) = serve().await;
        let at = |seed: u8| {
            let identity = Identity::from_seed([seed; 32]);
            Internal::new(
                &url,
                "reviewer",
                Signer::Key(identity.signer()),
                Arc::new(identity),
            )
            .unwrap()
        };
        at(1).write_state("pending", b"[]").await.unwrap();
        assert_eq!(
            at(1)
                .read_state("pending")
                .await
                .unwrap()
                .unwrap()
                .as_slice(),
            b"[]"
        );
        assert!(at(2).read_state("pending").await.unwrap().is_none());
        assert!(
            api.blobs
                .lock()
                .unwrap()
                .keys()
                .any(|name| name.starts_with("reviewer/pending-orphan-"))
        );
    }

    #[tokio::test]
    async fn a_damaged_blob_under_this_key_stops_the_machine() {
        let (url, api) = serve().await;
        let key = PrivateKeySigner::random();
        let one = internal(&url, [1; 32], &key);
        one.write_state("queue", b"the queue").await.unwrap();
        api.blobs.lock().unwrap().get_mut("builder/queue").unwrap()[9] ^= 1;
        assert!(one.read_state("queue").await.is_err());
        assert!(api.blobs.lock().unwrap().contains_key("builder/queue"));
    }

    #[test]
    fn a_request_is_signed_over_its_method_path_body_and_time() {
        let key = PrivateKeySigner::random();
        let digest = request_digest("PUT", "/internal/state/builder/queue", b"x", 7);
        let signature = key.sign_message_sync(digest.as_slice()).unwrap().as_bytes();
        let signer = |method, path, body: &[u8], at| {
            request_signer(method, path, body, at, &signature).unwrap()
        };
        assert_eq!(
            signer("PUT", "/internal/state/builder/queue", b"x", 7),
            key.address()
        );
        assert_ne!(
            signer("PUT", "/internal/state/builder/queue", b"y", 7),
            key.address()
        );
        assert_ne!(
            signer("PUT", "/internal/state/builder/queue", b"x", 8),
            key.address()
        );
        assert_ne!(
            signer("DELETE", "/internal/state/builder/queue", b"x", 7),
            key.address()
        );
        assert_ne!(
            signer("PUT", "/internal/state/builder/other", b"x", 7),
            key.address()
        );
    }

    #[test]
    fn idle_exit_happens_only_after_a_whole_idle_while() {
        let idle = Idle::new(Some(Duration::from_millis(200)));
        assert!(!idle.should_exit(false));
        std::thread::sleep(Duration::from_millis(250));
        assert!(!idle.should_exit(true), "busy, it exited");
        assert!(!idle.should_exit(false), "idle for no time, it exited");
        std::thread::sleep(Duration::from_millis(250));
        assert!(idle.should_exit(false));
        assert!(!Idle::new(None).should_exit(false));
    }
}
