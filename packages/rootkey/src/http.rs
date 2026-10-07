//! The root key service's routes, for a holder (the executor, for now) to
//! mount beside its own:
//!
//! - `POST /rootkey/exchange`: an [`ExchangeRequest`] in, a [`Release`] out,
//!   or the reason it was refused;
//! - `GET /rootkey/published/{purpose}`: the purpose's [`PublishedKey`];
//! - `GET /rootkey`: the root signer's address, whether this instance
//!   holds the root, and whether it serves it ([`RootStatus`]).
//!
//! A root that is held is served only once the holder says so
//! ([`Service::set_serving`]): once the canonical chain records its signer as
//! the live root key. Until then, and whenever the record stops being live,
//! the exchange and the published keys are refused (503): a root nobody can
//! check against the chain is never used. The holder looks at the record
//! only every so often, so every exchange and every published key also reads
//! it from the chain as it is answered, uncached: a root cleared or revoked
//! releases nothing from the next request on.
//!
//! Nothing here logs or answers with a secret: a refusal says only which
//! check failed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::Jwk;

use crate::chain_view::ChainView;
use crate::exchange::{ExchangeRequest, Release, authorise};
use crate::published::PublishedKey;
use crate::purpose::Purpose;
use crate::root::Root;
use crate::{Error, KeyPair};

struct Inner<V> {
    root: RwLock<Option<Arc<Root>>>,
    /// Whether what the root derives is served: the canonical chain records
    /// the held root's signer as the live root key.
    serving: AtomicBool,
    google_keys: RwLock<Vec<Jwk>>,
    /// Woken when a token names a Google key not among those fetched, so
    /// the holder fetches them again.
    keys_wanted: tokio::sync::Notify,
    view: V,
}

/// The holder's side: the root, if held, whether it is served, Google's keys
/// as last fetched, and the canonical chain.
pub struct Service<V> {
    inner: Arc<Inner<V>>,
}

impl<V> Clone for Service<V> {
    fn clone(&self) -> Self {
        Service {
            inner: self.inner.clone(),
        }
    }
}

/// `GET /rootkey`.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct RootStatus {
    /// The canonical chain, whose registry records the signer.
    pub chain_id: u64,
    /// Whether this instance holds the root.
    pub holds_root: bool,
    /// The root signer's address, while it holds the root.
    pub signer: Option<Address>,
    /// Whether the exchange and the published keys are served: the chain
    /// records that signer as the live root key.
    #[serde(default)]
    pub serving: bool,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

impl<V: ChainView> Service<V> {
    /// A service that holds no root yet.
    pub fn new(view: V) -> Service<V> {
        Service {
            inner: Arc::new(Inner {
                root: RwLock::new(None),
                serving: AtomicBool::new(false),
                google_keys: RwLock::new(Vec::new()),
                keys_wanted: tokio::sync::Notify::new(),
                view,
            }),
        }
    }

    /// Hold `root`: made here, or received by an exchange. It is not served
    /// until the holder says the chain records it ([`Service::set_serving`]).
    pub fn hold(&self, root: Root) {
        let mut held = self.inner.root.write().expect("the root's lock");
        self.inner.serving.store(false, Ordering::SeqCst);
        *held = Some(Arc::new(root));
    }

    /// Let the root go: the chain records another signer, or none since
    /// this one was cleared. It is wiped once nothing in flight uses it.
    pub fn forget(&self) {
        let mut held = self.inner.root.write().expect("the root's lock");
        self.inner.serving.store(false, Ordering::SeqCst);
        *held = None;
    }

    /// Serve what the root derives, or stop: on once the canonical chain
    /// records the held root's signer as the live root key, off whenever it
    /// doesn't. Without a root it stays off.
    pub fn set_serving(&self, serving: bool) {
        let held = self.inner.root.read().expect("the root's lock");
        self.inner
            .serving
            .store(serving && held.is_some(), Ordering::SeqCst);
    }

    pub fn is_serving(&self) -> bool {
        self.inner.serving.load(Ordering::SeqCst)
    }

    pub fn holds_root(&self) -> bool {
        self.root().is_some()
    }

    /// The root, while it is held and served, and the canonical chain, read
    /// now, still records its signer as the live root key.
    async fn served_root(&self) -> Result<Arc<Root>, Error> {
        let root = self.root().ok_or(Error::NoRoot)?;
        if !self.is_serving() {
            return Err(Error::NotServing);
        }
        match self.inner.view.live_root_signer().await {
            Ok(Some(signer)) if signer == root.signer_address() => Ok(root),
            Ok(_) => Err(Error::NotServing),
            Err(_) => Err(Error::ChainUnavailable),
        }
    }

    /// Wait until a token names a Google key not among those last fetched.
    pub async fn keys_wanted(&self) {
        self.inner.keys_wanted.notified().await;
    }

    fn root(&self) -> Option<Arc<Root>> {
        self.inner.root.read().expect("the root's lock").clone()
    }

    /// Google's signing keys as just fetched (see
    /// [`crate::launcher::fetch_google_keys`]). Only those an allowed
    /// checker on the canonical chain trusts are used.
    pub fn set_google_keys(&self, keys: Vec<Jwk>) {
        *self.inner.google_keys.write().expect("the keys' lock") = keys;
    }

    pub fn view(&self) -> &V {
        &self.inner.view
    }

    /// The root signer's address, while the root is held.
    pub fn signer(&self) -> Option<Address> {
        self.root().map(|root| root.signer_address())
    }

    /// The key pair for `purpose`, for the holder's own use (opening what is
    /// sealed to it).
    pub fn key_pair(&self, purpose: &Purpose) -> Result<KeyPair, Error> {
        self.root().ok_or(Error::NoRoot)?.key_pair(purpose)
    }

    /// The purpose's public key, signed for the canonical chain, while the
    /// root is served.
    pub async fn published(&self, purpose: &Purpose) -> Result<PublishedKey, Error> {
        self.served_root()
            .await?
            .publish(purpose, self.inner.view.chain_id())
    }

    pub fn status(&self) -> RootStatus {
        RootStatus {
            chain_id: self.inner.view.chain_id(),
            holds_root: self.holds_root(),
            signer: self.signer(),
            serving: self.is_serving(),
        }
    }

    /// Decide an exchange request at the holder's clock.
    pub async fn exchange(&self, request: &ExchangeRequest) -> Result<Release, Error> {
        self.exchange_at(request, unix_now()).await
    }

    /// Decide an exchange request at `now`, while the root is served.
    pub async fn exchange_at(&self, request: &ExchangeRequest, now: u64) -> Result<Release, Error> {
        let root = self.served_root().await?;
        let keys = self
            .inner
            .google_keys
            .read()
            .expect("the keys' lock")
            .clone();
        let decided = authorise(&root, &keys, request, now, &self.inner.view).await;
        if matches!(
            decided,
            Err(Error::Token(shonin_attestation_core::Error::UnknownKey))
        ) {
            self.inner.keys_wanted.notify_one();
        }
        decided
    }
}

/// The status a refusal is answered with.
pub fn status_of(error: &Error) -> StatusCode {
    match error {
        Error::Malformed(_) | Error::RootHasNoKeyPair => StatusCode::BAD_REQUEST,
        Error::ChainUnavailable
        | Error::VaultChainNotRead(_)
        | Error::NoRoot
        | Error::NotServing
        | Error::Randomness
        | Error::Seal => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::FORBIDDEN,
    }
}

fn refuse(error: Error) -> Response {
    (status_of(&error), error.to_string()).into_response()
}

/// The routes, to merge into the holder's status port.
pub fn routes<V: ChainView + 'static>(service: Service<V>) -> axum::Router {
    let exchanging = service.clone();
    let publishing = service.clone();
    axum::Router::new()
        .route(
            "/rootkey",
            axum::routing::get(move || {
                let service = service.clone();
                async move { Json(service.status()) }
            }),
        )
        .route(
            "/rootkey/published/{purpose}",
            axum::routing::get(move |Path(purpose): Path<String>| {
                let service = publishing.clone();
                async move {
                    let purpose: Purpose = match purpose.parse() {
                        Ok(purpose) => purpose,
                        Err(error) => return refuse(error),
                    };
                    match service.published(&purpose).await {
                        Ok(published) => Json(published).into_response(),
                        Err(error) => refuse(error),
                    }
                }
            }),
        )
        .route(
            "/rootkey/exchange",
            axum::routing::post(move |Json(request): Json<ExchangeRequest>| {
                let service = exchanging.clone();
                async move {
                    match service.exchange(&request).await {
                        Ok(release) => {
                            tracing::info!(purpose = %request.purpose, "released a key by exchange");
                            Json(release).into_response()
                        }
                        Err(error) => {
                            tracing::info!(
                                purpose = %request.purpose,
                                why = %error,
                                "refused an exchange"
                            );
                            refuse(error)
                        }
                    }
                }
            }),
        )
}
