//! The root key, held in the executor.
//!
//! The canonical chain's registry records one root key's signer
//! (`rootKey()`), set once from an attested proof and cleared only by the
//! admin. The executor reads that record and acts on it:
//!
//! | the record | the executor |
//! | --- | --- |
//! | empty, nothing held | makes a root, and puts its signer's leaf in the instance's tree, so the next attestation carries it; the keeper relays `setRootKey` |
//! | empty, a root held that was never recorded | waits for the keeper to relay it |
//! | empty, a root held that was recorded | the admin cleared it: lets it go, and makes a new one |
//! | this instance's signer, live | serves the exchange and the published keys |
//! | this instance's signer, not live | serves nothing derived, and says why |
//! | another signer | lets go of any root it holds, asks its peers for the recorded one, and without one serves nothing, looking again every few minutes |
//!
//! A root is never made while a record names another signer, so two roots
//! are never in use at once; and nothing derived from a root is served until
//! the chain records its signer, so a root nobody can check is never used.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::{Client, Deployments};
use shonin_rootkey::{
    ChainView, ImageKind, Purpose, Received, RegistryView, Release, Root, Service,
};

use crate::attestation::{Attestation, Mode};

/// How often the record is looked at while this instance's root waits for
/// it, or was just made.
const WAITING: Duration = Duration::from_secs(15);
/// How often it is looked at while the root is served, or held and not live.
/// Serving itself never waits on this: each exchange and each published key
/// reads the record again as it is answered.
const HOLDING: Duration = Duration::from_secs(60);
/// How often it is looked at while another signer is recorded.
const ELSEWHERE: Duration = Duration::from_secs(180);
/// How long Google's keys are kept before they are fetched again.
const GOOGLE_KEYS_EVERY: Duration = Duration::from_secs(3600);
/// The least time between two fetches of Google's keys, however often a
/// token names one not among them.
const GOOGLE_KEYS_AT_MOST: Duration = Duration::from_secs(60);
/// The most a peer's answer to an exchange may be.
const MOST_RELEASE_BYTES: usize = 64 * 1024;

/// The canonical chain as the root key service reads it: its id from the
/// chain config at once, its registry once the chain is connected. Until
/// then every read fails, and nothing is released.
#[derive(Clone)]
pub struct Canonical {
    chain_id: u64,
    view: Arc<OnceLock<RegistryView>>,
}

impl Canonical {
    pub fn new(chain_id: u64) -> Canonical {
        Canonical {
            chain_id,
            view: Default::default(),
        }
    }

    /// The registry, now the chain is connected.
    pub fn connect(&self, view: RegistryView) {
        let _ = self.view.set(view);
    }

    fn view(&self) -> Result<&RegistryView> {
        self.view
            .get()
            .context("the canonical chain is not connected yet")
    }
}

impl ChainView for Canonical {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn trusted_google_keys(&self) -> Result<Vec<[u8; 32]>> {
        self.view()?.trusted_google_keys().await
    }

    async fn image_kind(&self, image_digest: B256) -> Result<Option<ImageKind>> {
        self.view()?.image_kind(image_digest).await
    }

    async fn project_allowed(&self, project_id: B256) -> Result<bool> {
        self.view()?.project_allowed(project_id).await
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> Result<bool> {
        self.view()?
            .is_blessed_for(key, vault_chain, vault, strategy_hash)
            .await
    }

    async fn key_image(&self, key: Address) -> Result<Option<B256>> {
        self.view()?.key_image(key).await
    }

    async fn live_root_signer(&self) -> Result<Option<Address>> {
        self.view()?.live_root_signer().await
    }
}

/// The root key's record on the canonical chain, as the holder acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Record {
    /// Zero while none is recorded.
    pub signer: Address,
    /// `isRootKeyLive()`: recorded, and its image, project and checker all
    /// still allowed.
    pub live: bool,
    pub image_allowed: bool,
    pub project_allowed: bool,
    /// Whether the checker it was recorded through is still allowed: if
    /// not, a fresh attestation through another moves it.
    pub checker_allowed: bool,
    /// When the attestation it was recorded from runs out: its issue time
    /// and a blessing's TTL.
    pub until: u64,
    /// The chain's time when it was read: whatever cleared it came no later.
    pub at: u64,
}

impl Record {
    /// Why a recorded root is not live, said plainly.
    fn why_not_live(&self) -> String {
        let mut why = Vec::new();
        if !self.image_allowed {
            why.push("its image");
        }
        if !self.project_allowed {
            why.push("its project");
        }
        if !self.checker_allowed {
            why.push("its checker");
        }
        if why.is_empty() {
            "it is not live".to_string()
        } else {
            format!("{} no longer allowed", why.join(" and "))
        }
    }
}

/// Where the record is read.
pub trait Records: Send + Sync {
    fn record(&self) -> impl Future<Output = Result<Record>> + Send;
}

/// The canonical chain's registry.
pub struct OnChain {
    pub client: Client,
    pub deployments: Deployments,
}

impl Records for OnChain {
    async fn record(&self) -> Result<Record> {
        let registry = self.deployments.attestation;
        let read = &self.client;
        let at = read.block_timestamp().await?;
        let record = read
            .read(registry, IAttestationRegistry::rootKeyCall {})
            .await?;
        if record.signer == Address::ZERO {
            return Ok(Record {
                at,
                ..Record::default()
            });
        }
        let live = read
            .read(registry, IAttestationRegistry::isRootKeyLiveCall {})
            .await?;
        let image_allowed = read
            .read(
                registry,
                IAttestationRegistry::isImageAllowedCall {
                    imageDigest: record.imageDigest,
                },
            )
            .await?;
        let project_allowed = read
            .read(
                registry,
                IAttestationRegistry::isProjectAllowedCall {
                    projectId: record.projectId,
                },
            )
            .await?;
        let checker_allowed = read
            .read(
                registry,
                IAttestationRegistry::isVerifierAllowedCall {
                    verifier: record.verifier,
                },
            )
            .await?;
        let ttl = read
            .read(registry, IAttestationRegistry::blessingTtlCall {})
            .await?;
        Ok(Record {
            signer: record.signer,
            live,
            image_allowed,
            project_allowed,
            checker_allowed,
            until: record.issuedAt.saturating_add(ttl),
            at,
        })
    }
}

/// Other holders, asked for the root the chain records.
pub trait Peers: Send + Sync {
    /// The root whose signer is `signer`, from whichever peer releases it.
    fn ask(&self, signer: Address) -> impl Future<Output = Option<Root>> + Send;
}

/// The peers baked into the image's settings, asked by attested exchange
/// with a token from the launcher. In development mode there is no launcher
/// to ask, so none is asked.
pub struct Exchange {
    pub urls: Vec<String>,
    pub mode: Mode,
    pub socket: PathBuf,
    pub http: reqwest::Client,
}

impl Exchange {
    async fn ask_one(&self, url: &str) -> Result<Root> {
        let requester = shonin_rootkey::Requester::new(Purpose::Root)?;
        let token =
            shonin_rootkey::launcher::request_token(&self.socket, &requester.nonce()).await?;
        let request = requester.request(token, None);
        let answer = self
            .http
            .post(format!("{}/rootkey/exchange", url.trim_end_matches('/')))
            .timeout(Duration::from_secs(30))
            .json(&request)
            .send()
            .await
            .context("asking for the root")?;
        let status = answer.status();
        if !status.is_success() {
            bail!("the peer refused ({status})");
        }
        let body = answer.bytes().await.context("reading the peer's release")?;
        if body.len() > MOST_RELEASE_BYTES {
            bail!("the peer's release is too long");
        }
        let release: Release =
            serde_json::from_slice(&body).context("the peer's answer is not a release")?;
        match requester.open(&request, &release)? {
            Received::Root(root) => Ok(root),
            Received::Key(_) => bail!("the peer released a key, not the root"),
        }
    }
}

impl Peers for Exchange {
    async fn ask(&self, signer: Address) -> Option<Root> {
        if self.urls.is_empty() || self.mode != Mode::Launcher {
            return None;
        }
        for url in &self.urls {
            match self.ask_one(url).await {
                Ok(root) if root.signer_address() == signer => return Some(root),
                Ok(_) => {
                    tracing::warn!(
                        peer = url,
                        "a peer released a root the chain does not record"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        peer = url,
                        error = format!("{error:#}"),
                        "a peer did not release the root"
                    );
                }
            }
        }
        None
    }
}

/// What one look at the record came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// No record and nothing held: a root was made.
    Made(Address),
    /// No record: the root held waits for the chain to record it.
    Waiting(Address),
    /// The held root was recorded and the record is gone: it was cleared,
    /// and a new root was made.
    Cleared { old: Address, new: Address },
    /// The record names the held root, and is live: served.
    Serving(Address),
    /// The record names the held root, and is not live: nothing served.
    NotLive(Address),
    /// The record names another signer, which a peer released.
    Received(Address),
    /// The record names another signer, and no peer released it.
    Elsewhere(Address),
}

impl Look {
    /// How long until the next look.
    fn wait(self) -> Duration {
        match self {
            Look::Made(_) | Look::Waiting(_) | Look::Cleared { .. } => WAITING,
            Look::Serving(_) | Look::NotLive(_) | Look::Received(_) => HOLDING,
            Look::Elsewhere(_) => ELSEWHERE,
        }
    }
}

/// The root's holder: the record, the root key service, and the instance's
/// tree.
pub struct Holder<V, R, P> {
    pub service: Service<V>,
    pub attestation: Attestation,
    /// The canonical chain's id.
    pub chain_id: u64,
    pub records: R,
    pub peers: P,
    /// The held root's signer, once the chain has recorded it: a record
    /// found empty after that means the root was cleared.
    recorded: Option<Address>,
    /// The record as last read.
    last: Record,
}

impl<V: ChainView, R: Records, P: Peers> Holder<V, R, P> {
    pub fn new(
        service: Service<V>,
        attestation: Attestation,
        chain_id: u64,
        records: R,
        peers: P,
    ) -> Self {
        Holder {
            service,
            attestation,
            chain_id,
            records,
            peers,
            recorded: None,
            last: Record::default(),
        }
    }

    /// Make a root, hold it unserved, and put its signer in the tree, to be
    /// attested after the record was read empty (and so after any clear).
    fn make(&mut self) -> Result<Address> {
        let root = Root::generate()?;
        let signer = root.signer_address();
        self.service.hold(root);
        self.recorded = None;
        self.attestation.date_after(self.last.at.saturating_add(1));
        self.attestation.put_root(self.chain_id, signer);
        Ok(signer)
    }

    /// Let go of the root held, and its leaf.
    fn let_go(&mut self) {
        self.service.forget();
        self.recorded = None;
        self.attestation.remove_root();
    }

    /// Read the record once, and act on it. A record that can't be read
    /// changes nothing.
    pub async fn look(&mut self) -> Result<Look> {
        let record = self.records.record().await?;
        self.last = record;
        let held = self.service.signer();
        if record.signer == Address::ZERO {
            self.service.set_serving(false);
            return Ok(match held {
                // Recorded once and now gone: cleared by the admin. That
                // signer is refused for good, so a new root is made.
                Some(old) if self.recorded == Some(old) => {
                    self.let_go();
                    Look::Cleared {
                        old,
                        new: self.make()?,
                    }
                }
                Some(signer) => {
                    self.attestation.note_root(signer, true, 0);
                    Look::Waiting(signer)
                }
                None => Look::Made(self.make()?),
            });
        }
        if held == Some(record.signer) {
            self.recorded = Some(record.signer);
            self.hold_recorded(&record);
            return Ok(if record.live {
                Look::Serving(record.signer)
            } else {
                Look::NotLive(record.signer)
            });
        }
        // Another signer is recorded: whatever is held here is not the root,
        // and none is made while that record stands.
        if held.is_some() {
            self.let_go();
        }
        match self.peers.ask(record.signer).await {
            Some(root) if root.signer_address() == record.signer => {
                self.service.hold(root);
                self.recorded = Some(record.signer);
                self.hold_recorded(&record);
                Ok(Look::Received(record.signer))
            }
            _ => Ok(Look::Elsewhere(record.signer)),
        }
    }

    /// The held root is the one recorded: its leaf stays in the tree (wanted
    /// again only if its checker is retired), and it is served while the
    /// record is live.
    fn hold_recorded(&self, record: &Record) {
        self.attestation.put_root(self.chain_id, record.signer);
        self.attestation
            .note_root(record.signer, !record.checker_allowed, record.until);
        self.service.set_serving(record.live);
    }

    /// Look at the record for as long as the executor runs, saying each
    /// change.
    pub async fn run(mut self) {
        let mut last: Option<Look> = None;
        let mut unread = crate::Throttle::default();
        loop {
            let wait = match self.look().await {
                Ok(look) => {
                    if last != Some(look) {
                        say(look, &self.last);
                    }
                    last = Some(look);
                    look.wait()
                }
                Err(error) => {
                    if unread.due(std::time::Instant::now()) {
                        tracing::warn!(
                            error = format!("{error:#}"),
                            "could not read the root key's record; nothing changes until it can be"
                        );
                    }
                    WAITING
                }
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// The log line for a look that changed something.
fn say(look: Look, record: &Record) {
    match look {
        Look::Made(signer) => tracing::info!(
            %signer,
            "made a root key: its signer is in the instance's tree, for the canonical chain to record; nothing derived from it is served until then"
        ),
        Look::Waiting(signer) => tracing::info!(
            %signer,
            "the root key waits for the canonical chain to record its signer"
        ),
        Look::Cleared { old, new } => tracing::warn!(
            %old,
            %new,
            "the root key's record was cleared: its root is let go and a new one is made"
        ),
        Look::Serving(signer) => tracing::info!(
            %signer,
            "the canonical chain records this instance's root key: serving it"
        ),
        Look::NotLive(signer) => tracing::warn!(
            %signer,
            why = record.why_not_live(),
            "the root key's record is not live: nothing derived from it is served"
        ),
        Look::Received(signer) => tracing::info!(
            %signer,
            "received the recorded root key from a peer"
        ),
        Look::Elsewhere(signer) => tracing::warn!(
            %signer,
            "the root key is held elsewhere or lost: an admin clear starts a new one"
        ),
    }
}

/// Keep Google's signing keys fresh for the exchange: fetched at start,
/// every hour, and whenever a token names a key not among them (at most
/// once a minute). Only those an allowed checker trusts are ever used.
pub async fn keep_google_keys<V: ChainView>(service: Service<V>) {
    let http = reqwest::Client::new();
    let mut said = crate::Throttle::default();
    loop {
        let wait = match shonin_rootkey::launcher::fetch_google_keys(&http).await {
            Ok(keys) => {
                service.set_google_keys(keys);
                GOOGLE_KEYS_EVERY
            }
            Err(error) => {
                if said.due(std::time::Instant::now()) {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "could not fetch Google's signing keys; trying again"
                    );
                }
                GOOGLE_KEYS_AT_MOST * 5
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = service.keys_wanted() => {}
        }
        tokio::time::sleep(GOOGLE_KEYS_AT_MOST).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A record as the test sets it.
    #[derive(Clone, Default)]
    struct Stand(Arc<Mutex<Option<Record>>>);

    impl Stand {
        fn set(&self, record: Record) {
            *self.0.lock().unwrap() = Some(record);
        }
        fn unreadable(&self) {
            *self.0.lock().unwrap() = None;
        }
    }

    impl Records for Stand {
        async fn record(&self) -> Result<Record> {
            self.0
                .lock()
                .unwrap()
                .context("the stand-in chain does not answer")
        }
    }

    /// Peers that release nothing, or the root they are given, noting how
    /// often they were asked.
    #[derive(Default)]
    struct Asked {
        root: Mutex<Option<Root>>,
        asked: Mutex<usize>,
    }

    impl Peers for Arc<Asked> {
        async fn ask(&self, _: Address) -> Option<Root> {
            *self.asked.lock().unwrap() += 1;
            self.root.lock().unwrap().take()
        }
    }

    /// The canonical chain as the root key service reads it, over the same
    /// stand-in record the holder looks at.
    struct StandView(Stand);

    impl ChainView for StandView {
        fn chain_id(&self) -> u64 {
            CANONICAL
        }
        async fn trusted_google_keys(&self) -> Result<Vec<[u8; 32]>> {
            Ok(Vec::new())
        }
        async fn image_kind(&self, _: B256) -> Result<Option<ImageKind>> {
            Ok(None)
        }
        async fn project_allowed(&self, _: B256) -> Result<bool> {
            Ok(false)
        }
        async fn is_blessed_for(&self, _: Address, _: u64, _: Address, _: B256) -> Result<bool> {
            Ok(false)
        }
        async fn key_image(&self, _: Address) -> Result<Option<B256>> {
            Ok(None)
        }
        async fn live_root_signer(&self) -> Result<Option<Address>> {
            let record = self.0.record().await?;
            Ok((record.live && record.signer != Address::ZERO).then_some(record.signer))
        }
    }

    const CANONICAL: u64 = 31_337;

    fn holder() -> (Holder<StandView, Stand, Arc<Asked>>, Stand, Arc<Asked>) {
        let stand = Stand::default();
        let peers = Arc::new(Asked::default());
        let service = Service::new(StandView(stand.clone()));
        let attestation = Attestation::new(Mode::Development).unwrap();
        (
            Holder::new(
                service,
                attestation,
                CANONICAL,
                stand.clone(),
                peers.clone(),
            ),
            stand,
            peers,
        )
    }

    fn recorded(signer: Address) -> Record {
        Record {
            signer,
            live: true,
            image_allowed: true,
            project_allowed: true,
            checker_allowed: true,
            until: 1_800_000_000,
            at: 1_700_000_000,
        }
    }

    async fn published_status<V: ChainView>(service: &Service<V>) -> bool {
        service
            .published(&Purpose::platform("dummy").unwrap())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn with_no_record_a_root_is_made_and_its_signer_s_leaf_bound() {
        let (mut holder, stand, _) = holder();
        stand.set(Record::default());
        let before = holder.attestation.binding().nonce();
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        assert_eq!(holder.service.signer(), Some(signer));
        let leaf = holder.attestation.root_leaf().expect("the root's leaf");
        assert_eq!(
            (leaf.chain_id, leaf.signer, leaf.due),
            (CANONICAL, signer, true)
        );
        assert_eq!(leaf.hash(), shonin_rootkey::root_leaf(CANONICAL, signer));
        // The next token binds it: the tree's root is over it.
        assert_ne!(holder.attestation.binding().nonce(), before);
        let status = holder.attestation.status();
        let view = status.root.expect("the root's leaf is published");
        assert_eq!(view.leaf, leaf.hash().to_string());
        // Looked at again with still no record: the same root waits.
        assert_eq!(holder.look().await.unwrap(), Look::Waiting(signer));
        assert_eq!(holder.service.signer(), Some(signer));
    }

    #[tokio::test]
    async fn nothing_is_released_before_the_chain_records_the_signer() {
        let (mut holder, stand, _) = holder();
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        assert!(!holder.service.is_serving());
        assert!(!published_status(&holder.service).await);
        // The chain records it: served, and the leaf no longer wanted.
        stand.set(recorded(signer));
        assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));
        assert!(holder.service.is_serving());
        assert!(published_status(&holder.service).await);
        assert!(!holder.attestation.root_leaf().unwrap().due);
    }

    #[tokio::test]
    async fn with_its_own_signer_recorded_it_serves() {
        let (mut holder, stand, peers) = holder();
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        stand.set(recorded(signer));
        for _ in 0..3 {
            assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));
        }
        assert_eq!(holder.service.signer(), Some(signer));
        assert_eq!(*peers.asked.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_record_that_is_not_live_releases_nothing() {
        let (mut holder, stand, _) = holder();
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        stand.set(recorded(signer));
        holder.look().await.unwrap();
        assert!(published_status(&holder.service).await);
        // The image is revoked: nothing derived is served, and the root is
        // kept (only a clear retires it).
        stand.set(Record {
            live: false,
            image_allowed: false,
            ..recorded(signer)
        });
        assert_eq!(holder.look().await.unwrap(), Look::NotLive(signer));
        assert!(!holder.service.is_serving());
        assert!(!published_status(&holder.service).await);
        assert_eq!(holder.service.signer(), Some(signer));
        // A retired checker makes the leaf wanted again, so a fresh
        // attestation moves the record to an allowed checker.
        stand.set(Record {
            live: false,
            checker_allowed: false,
            ..recorded(signer)
        });
        assert_eq!(holder.look().await.unwrap(), Look::NotLive(signer));
        assert!(holder.attestation.root_leaf().unwrap().due);
        assert!(
            Record {
                live: false,
                checker_allowed: false,
                ..recorded(signer)
            }
            .why_not_live()
            .contains("checker")
        );
    }

    #[tokio::test]
    async fn with_another_signer_recorded_and_no_peer_nothing_derived_is_served_and_no_root_is_made()
     {
        let (mut holder, stand, peers) = holder();
        let other = Address::repeat_byte(0x0e);
        stand.set(recorded(other));
        for _ in 0..3 {
            assert_eq!(holder.look().await.unwrap(), Look::Elsewhere(other));
        }
        assert!(!holder.service.holds_root());
        assert!(!holder.service.is_serving());
        assert!(!published_status(&holder.service).await);
        assert!(holder.attestation.root_leaf().is_none());
        assert_eq!(holder.service.status().signer, None);
        assert_eq!(*peers.asked.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn a_root_held_is_let_go_once_another_signer_is_recorded() {
        let (mut holder, stand, _) = holder();
        stand.set(Record::default());
        let Look::Made(_) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        let other = Address::repeat_byte(0x0e);
        stand.set(recorded(other));
        assert_eq!(holder.look().await.unwrap(), Look::Elsewhere(other));
        assert!(!holder.service.holds_root());
        assert!(holder.attestation.root_leaf().is_none());
    }

    #[tokio::test]
    async fn after_a_clear_a_new_root_is_made() {
        let (mut holder, stand, _) = holder();
        // Held elsewhere, then cleared: a root is made here.
        let other = Address::repeat_byte(0x0e);
        stand.set(recorded(other));
        holder.look().await.unwrap();
        stand.set(Record::default());
        let Look::Made(first) = holder.look().await.unwrap() else {
            panic!("no root was made after the clear");
        };
        // Recorded, then cleared: that root is let go, and another made.
        stand.set(recorded(first));
        holder.look().await.unwrap();
        stand.set(Record {
            at: 1_700_000_500,
            ..Record::default()
        });
        let Look::Cleared { old, new } = holder.look().await.unwrap() else {
            panic!("the clear was not noticed");
        };
        // Attested only after the record was found empty.
        assert_eq!(holder.attestation.inner_dated_after(), 1_700_000_501);
        assert_eq!(old, first);
        assert_ne!(new, first);
        assert_eq!(holder.service.signer(), Some(new));
        assert!(!holder.service.is_serving());
        let leaf = holder.attestation.root_leaf().unwrap();
        assert_eq!((leaf.signer, leaf.due), (new, true));
    }

    #[tokio::test]
    async fn the_recorded_root_from_a_peer_is_held_and_served() {
        let (mut holder, stand, peers) = holder();
        let root = Root::generate().unwrap();
        let signer = root.signer_address();
        *peers.root.lock().unwrap() = Some(root);
        stand.set(recorded(signer));
        assert_eq!(holder.look().await.unwrap(), Look::Received(signer));
        assert!(holder.service.is_serving());
        assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));
    }

    #[tokio::test]
    async fn a_root_from_a_peer_that_is_not_the_recorded_one_is_refused() {
        let (mut holder, stand, peers) = holder();
        *peers.root.lock().unwrap() = Some(Root::generate().unwrap());
        let other = Address::repeat_byte(0x0e);
        stand.set(recorded(other));
        assert_eq!(holder.look().await.unwrap(), Look::Elsewhere(other));
        assert!(!holder.service.holds_root());
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_read_changes_nothing() {
        let (mut holder, stand, _) = holder();
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        stand.set(recorded(signer));
        holder.look().await.unwrap();
        stand.unreadable();
        assert!(holder.look().await.is_err());
        assert_eq!(holder.service.signer(), Some(signer));
        assert!(holder.service.is_serving());
    }

    /// Between two looks at the record, a clear or a revocation stops what
    /// is served from the very next request.
    #[tokio::test]
    async fn a_cleared_or_revoked_root_releases_nothing_from_the_next_request() {
        use tower::ServiceExt;
        let (mut holder, stand, _) = holder();
        let app = shonin_rootkey::routes(holder.service.clone());
        let get = || {
            http::Request::get("/rootkey/published/platform:dummy")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        stand.set(recorded(signer));
        holder.look().await.unwrap();
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 200);
        for record in [
            // Revoked: its image is no longer allowed.
            Record {
                live: false,
                image_allowed: false,
                ..recorded(signer)
            },
            // Cleared.
            Record::default(),
        ] {
            stand.set(record);
            // The holder has not looked again, and still says serve.
            assert!(holder.service.is_serving());
            assert!(!published_status(&holder.service).await);
            assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 503);
        }
    }

    #[tokio::test]
    async fn the_exchange_route_answers_unavailable_until_the_signer_is_recorded() {
        use tower::ServiceExt;
        let (mut holder, stand, _) = holder();
        let app = shonin_rootkey::routes(holder.service.clone());
        let get = || {
            http::Request::get("/rootkey/published/platform:dummy")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 503);
        stand.set(Record::default());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 503);
        stand.set(recorded(signer));
        holder.look().await.unwrap();
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 200);
    }
}
