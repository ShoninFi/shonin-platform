//! The root key, held in the executor.
//!
//! The canonical chain's registry records one root key's signer
//! (`rootKey()`), set once from an attested proof and cleared only by the
//! admin. The executor reads that record and acts on it:
//!
//! | the record | the executor |
//! | --- | --- |
//! | empty, nothing held | once a later block shows it empty too, makes a root, and puts its signer's leaf in the instance's tree, so the next attestation carries it; the keeper relays `setRootKey` |
//! | empty, a root held that was never seen recorded | waits for the keeper to relay it, unless the chain logged that signer's clear (recorded and cleared between two looks): then lets it go |
//! | empty, a root held that was recorded | the admin cleared it: lets it go, and makes a new one as above |
//! | this instance's signer, live | serves the exchange and the published keys |
//! | this instance's signer, not live | serves nothing derived, and says why |
//! | another signer | lets go of any root it holds, asks its peers for the recorded one, and without one serves nothing, looking again every few minutes |
//!
//! A root is never made while a record names another signer, so two roots
//! are never in use at once; and nothing derived from a root is served until
//! the chain records its signer, so a root nobody can check is never used.
//!
//! The record is read whole as of one block, the head when it is read, and
//! the holder remembers the newest block it acted on. An RPC name can front
//! several nodes, some blocks behind the others: an answer from a block below
//! the newest one acted on is from such a node, and is ignored. So the only
//! copy of a recorded root is let go only when the record reads empty at a
//! block at or past the one that showed it recorded (or when the chain
//! logged its clear), and a root is made only when two reads, the second at
//! a later block, both find the record empty.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use alloy::primitives::Address;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::{Client, Deployments};
use shonin_rootkey::{ChainView, Purpose, Received, Release, Root, Service};

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
    /// The block it was read at: every field above is as of this one block.
    pub head: u64,
    /// That block's time: whatever cleared it came no later.
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
    /// The record, every field read as of the head block when it is read.
    fn record(&self) -> impl Future<Output = Result<Record>> + Send;
    /// Whether the chain logged the clear of `signer` (`RootKeyCleared`) in
    /// blocks `from` to `to`, both included.
    fn cleared(
        &self,
        signer: Address,
        from: u64,
        to: u64,
    ) -> impl Future<Output = Result<bool>> + Send;
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
        // The head and its time from one read, and every read after it
        // pinned to that block: a node behind the others answers as of its
        // own block, which the holder then sees is older than one it acted
        // on, never with a mix of blocks.
        let (head, at) = read.head_and_time().await?;
        let block = alloy::eips::BlockId::number(head);
        let record = read
            .read_at(registry, IAttestationRegistry::rootKeyCall {}, block)
            .await?;
        if record.signer == Address::ZERO {
            return Ok(Record {
                head,
                at,
                ..Record::default()
            });
        }
        let live = read
            .read_at(registry, IAttestationRegistry::isRootKeyLiveCall {}, block)
            .await?;
        let image_allowed = read
            .read_at(
                registry,
                IAttestationRegistry::isImageAllowedCall {
                    imageDigest: record.imageDigest,
                },
                block,
            )
            .await?;
        let project_allowed = read
            .read_at(
                registry,
                IAttestationRegistry::isProjectAllowedCall {
                    projectId: record.projectId,
                },
                block,
            )
            .await?;
        let checker_allowed = read
            .read_at(
                registry,
                IAttestationRegistry::isVerifierAllowedCall {
                    verifier: record.verifier,
                },
                block,
            )
            .await?;
        let ttl = read
            .read_at(registry, IAttestationRegistry::blessingTtlCall {}, block)
            .await?;
        Ok(Record {
            signer: record.signer,
            live,
            image_allowed,
            project_allowed,
            checker_allowed,
            until: record.issuedAt.saturating_add(ttl),
            head,
            at,
        })
    }

    async fn cleared(&self, signer: Address, from: u64, to: u64) -> Result<bool> {
        if from > to {
            return Ok(false);
        }
        let filter = alloy::rpc::types::Filter::new()
            .address(self.deployments.attestation)
            .event_signature(IAttestationRegistry::RootKeyCleared::SIGNATURE_HASH)
            .topic1(signer.into_word())
            .from_block(from)
            .to_block(to);
        let logs = self
            .client
            .logs(&filter)
            .await
            .context("looking for the root key's clear")?;
        Ok(!logs.is_empty())
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
        let body = shonin_rootkey::body::read_at_most(answer, MOST_RELEASE_BYTES)
            .await
            .context("reading the peer's release")?;
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
    /// No record and nothing held, first seen at this block: a root is made
    /// once a later block shows the record empty too.
    Empty(u64),
    /// No record, confirmed at a later block than first seen, and nothing
    /// held: a root was made.
    Made(Address),
    /// No record: the root held waits for the chain to record it.
    Waiting(Address),
    /// The held root was cleared by the admin: its record, once seen, reads
    /// empty at a block at or past the one that showed it, or the chain
    /// logged its clear. It is let go, and a new root is made as from an
    /// empty record.
    Cleared(Address),
    /// The record names the held root, and is live: served.
    Serving(Address),
    /// The record names the held root, and is not live: nothing served.
    NotLive(Address),
    /// The record names another signer, which a peer released.
    Received(Address),
    /// The record names another signer, and no peer released it.
    Elsewhere(Address),
    /// The record was read at a block below the newest one acted on (a node
    /// behind the others answered): ignored, and nothing changed.
    Behind { head: u64, newest: u64 },
}

impl Look {
    /// How long until the next look.
    fn wait(self) -> Duration {
        match self {
            Look::Empty(_)
            | Look::Made(_)
            | Look::Waiting(_)
            | Look::Cleared(_)
            | Look::Behind { .. } => WAITING,
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
    /// found empty after that (at a block no older, as every record acted on
    /// is) means the root was cleared.
    recorded: Option<Address>,
    /// The record as last acted on.
    last: Record,
    /// The newest block a record was acted on at. A record read at an older
    /// one came from a node behind the others, and is ignored.
    newest: u64,
    /// While nothing is held, the block the record was first seen empty at
    /// since it last named a signer: a root is made only once a later block
    /// shows it empty too.
    empty_since: Option<u64>,
    /// While the root held waits to be recorded, the first block not yet
    /// searched for its clear.
    unsearched: u64,
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
            newest: 0,
            empty_since: None,
            unsearched: 0,
        }
    }

    /// Make a root, hold it unserved, and put its signer in the tree, to be
    /// attested after the record was read empty (and so after any clear).
    fn make(&mut self) -> Result<Address> {
        let root = Root::generate()?;
        let signer = root.signer_address();
        self.service.hold(root);
        self.recorded = None;
        self.empty_since = None;
        // The record was empty at this block, so anything that records this
        // root, or clears it, comes later.
        self.unsearched = self.last.head.saturating_add(1);
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

    /// Read the record once, and act on it. A record that can't be read, or
    /// that was read at a block older than one already acted on, changes
    /// nothing.
    pub async fn look(&mut self) -> Result<Look> {
        let record = self.records.record().await?;
        if record.head < self.newest {
            return Ok(Look::Behind {
                head: record.head,
                newest: self.newest,
            });
        }
        let held = self.service.signer();
        if record.signer == Address::ZERO {
            // The clear's log is searched before anything is acted on, so a
            // search that fails leaves everything as it was.
            let logged_clear = match held {
                Some(signer) if self.recorded != Some(signer) => {
                    self.records
                        .cleared(signer, self.unsearched, record.head)
                        .await?
                }
                _ => false,
            };
            self.newest = record.head;
            self.last = record;
            self.service.set_serving(false);
            return Ok(match held {
                // Recorded once and now gone, at a block no older than the
                // one that showed it: cleared by the admin. Or never seen
                // recorded, but its clear logged: recorded and cleared
                // between two looks. That signer is refused for good, so it
                // is let go, and a new root is made once a later block
                // confirms the record empty.
                Some(old) if self.recorded == Some(old) || logged_clear => {
                    self.let_go();
                    self.empty_since = Some(record.head);
                    Look::Cleared(old)
                }
                Some(signer) => {
                    self.unsearched = self.unsearched.max(record.head.saturating_add(1));
                    self.attestation.note_root(signer, true, 0);
                    Look::Waiting(signer)
                }
                None => match self.empty_since {
                    Some(first) if record.head > first => Look::Made(self.make()?),
                    Some(first) => Look::Empty(first),
                    None => {
                        self.empty_since = Some(record.head);
                        Look::Empty(record.head)
                    }
                },
            });
        }
        self.newest = record.head;
        self.last = record;
        self.empty_since = None;
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
                Ok(look @ Look::Behind { .. }) => {
                    // Said, but not taken for what the record is.
                    say(look, &self.last);
                    look.wait()
                }
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
        Look::Empty(head) => tracing::info!(
            block = head,
            "the root key's record is empty: a root is made once a later block shows it empty too"
        ),
        Look::Made(signer) => tracing::info!(
            %signer,
            "made a root key: its signer is in the instance's tree, for the canonical chain to record; nothing derived from it is served until then"
        ),
        Look::Waiting(signer) => tracing::info!(
            %signer,
            "the root key waits for the canonical chain to record its signer"
        ),
        Look::Cleared(old) => tracing::warn!(
            %old,
            "the root key's record was cleared: its root is let go, and a new one is made once a later block shows the record still empty"
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
        Look::Behind { head, newest } => tracing::info!(
            block = head,
            newest,
            "the root key's record was read at a block older than one already read (a node behind the others answered): ignored"
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
    use alloy::primitives::B256;
    use shonin_rootkey::ImageKind;
    use std::sync::{Arc, Mutex};

    /// The canonical chain as a test sets it: the record, the blocks it is
    /// read at, and the clears logged.
    #[derive(Default)]
    struct Chain {
        /// `None` while it can't be read.
        record: Option<Record>,
        /// The block the next read is at, if the test names one; otherwise
        /// one past the highest yet.
        at: Option<u64>,
        top: u64,
        /// Each clear logged: its signer and block.
        clears: Vec<(Address, u64)>,
        /// Each search for a clear: its signer and blocks.
        searched: Vec<(Address, u64, u64)>,
        clears_unreadable: bool,
    }

    #[derive(Clone, Default)]
    struct Stand(Arc<Mutex<Chain>>);

    impl Stand {
        /// The record from the next read on, each read at a block past the
        /// highest yet.
        fn set(&self, record: Record) {
            let mut chain = self.0.lock().unwrap();
            chain.record = Some(record);
            chain.at = None;
        }
        /// The record from the next read on, read at block `head`, as a node
        /// at that block answers.
        fn set_at(&self, head: u64, record: Record) {
            let mut chain = self.0.lock().unwrap();
            chain.record = Some(record);
            chain.at = Some(head);
        }
        fn unreadable(&self) {
            self.0.lock().unwrap().record = None;
        }
        /// The chain logged `signer`'s clear at `block`.
        fn log_clear(&self, signer: Address, block: u64) {
            self.0.lock().unwrap().clears.push((signer, block));
        }
        fn top(&self) -> u64 {
            self.0.lock().unwrap().top
        }
        /// The record as it stands, without a read.
        fn peek(&self) -> Option<Record> {
            self.0.lock().unwrap().record
        }
    }

    impl Records for Stand {
        async fn record(&self) -> Result<Record> {
            let mut chain = self.0.lock().unwrap();
            let record = chain.record.context("the stand-in chain does not answer")?;
            let head = chain.at.unwrap_or(chain.top + 1);
            chain.top = chain.top.max(head);
            Ok(Record { head, ..record })
        }

        async fn cleared(&self, signer: Address, from: u64, to: u64) -> Result<bool> {
            let mut chain = self.0.lock().unwrap();
            anyhow::ensure!(!chain.clears_unreadable, "the logs can't be read");
            chain.searched.push((signer, from, to));
            Ok(chain
                .clears
                .iter()
                .any(|(cleared, at)| *cleared == signer && (from..=to).contains(at)))
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
        fn reads_chain(&self, chain: u64) -> bool {
            chain == CANONICAL
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
        async fn key_image(&self, _: u64, _: Address) -> Result<Option<B256>> {
            Ok(None)
        }
        async fn live_root_signer(&self) -> Result<Option<Address>> {
            let record = self
                .0
                .peek()
                .context("the stand-in chain does not answer")?;
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
            head: 0,
            at: 1_700_000_000,
        }
    }

    /// With the record empty, look until a root is made: the first look
    /// only notes the block it was empty at, the next (at a later block)
    /// makes it.
    async fn make(holder: &mut Holder<StandView, Stand, Arc<Asked>>, stand: &Stand) -> Address {
        stand.set(Record::default());
        let first = holder.look().await.unwrap();
        assert!(matches!(first, Look::Empty(_)), "{first:?}");
        assert!(!holder.service.holds_root());
        let Look::Made(signer) = holder.look().await.unwrap() else {
            panic!("no root was made");
        };
        signer
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
        let before = holder.attestation.binding().nonce();
        let signer = make(&mut holder, &stand).await;
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

    /// An empty record read at one block may be a node behind the others:
    /// a root is made only once a later block shows it empty too, so an
    /// empty answer at start-up, or another at the same block, makes
    /// nothing while the chain records a root.
    #[tokio::test]
    async fn a_root_is_made_only_once_a_later_block_confirms_the_record_empty() {
        let (mut holder, stand, peers) = holder();
        // Empty at block 10, twice (a node stuck there): nothing made.
        stand.set_at(10, Record::default());
        assert_eq!(holder.look().await.unwrap(), Look::Empty(10));
        assert_eq!(holder.look().await.unwrap(), Look::Empty(10));
        assert!(!holder.service.holds_root());
        assert!(holder.attestation.root_leaf().is_none());
        // The chain in fact records another root: still none made.
        let other = Address::repeat_byte(0x0e);
        stand.set_at(12, recorded(other));
        assert_eq!(holder.look().await.unwrap(), Look::Elsewhere(other));
        // An older empty answer after that is ignored, and makes nothing.
        stand.set_at(11, Record::default());
        assert_eq!(
            holder.look().await.unwrap(),
            Look::Behind {
                head: 11,
                newest: 12
            }
        );
        // Empty at block 13 starts the count again: nothing made until a
        // block after it is empty too.
        stand.set_at(13, Record::default());
        assert_eq!(holder.look().await.unwrap(), Look::Empty(13));
        assert!(!holder.service.holds_root());
        stand.set_at(14, Record::default());
        assert!(matches!(holder.look().await.unwrap(), Look::Made(_)));
        assert_eq!(*peers.asked.lock().unwrap(), 1);
    }

    /// A node behind the others answers a look with an older block, at which
    /// the record was still empty. The executor holds the only copy of the
    /// recorded root: that answer is ignored, the root kept and served, and
    /// the next truthful answer finds it recorded as before. An older answer
    /// naming another signer is ignored the same way.
    #[tokio::test]
    async fn a_record_read_at_an_older_block_never_loses_the_recorded_root() {
        let (mut holder, stand, _) = holder();
        let signer = make(&mut holder, &stand).await;
        stand.set_at(40, recorded(signer));
        assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));

        for stale in [Record::default(), recorded(Address::repeat_byte(0x0e))] {
            stand.set_at(16, stale);
            assert_eq!(
                holder.look().await.unwrap(),
                Look::Behind {
                    head: 16,
                    newest: 40
                }
            );
            assert_eq!(holder.service.signer(), Some(signer));
            assert!(holder.service.holds_root());
            assert!(holder.service.is_serving());
            assert_eq!(holder.attestation.root_leaf().unwrap().signer, signer);
        }

        // The truth again, at the same block or a later one.
        stand.set_at(40, recorded(signer));
        assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));
        stand.set_at(41, recorded(signer));
        assert_eq!(holder.look().await.unwrap(), Look::Serving(signer));
        assert_eq!(holder.service.signer(), Some(signer));
        assert!(holder.service.is_serving());
    }

    #[tokio::test]
    async fn nothing_is_released_before_the_chain_records_the_signer() {
        let (mut holder, stand, _) = holder();
        let signer = make(&mut holder, &stand).await;
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
        let signer = make(&mut holder, &stand).await;
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
        let signer = make(&mut holder, &stand).await;
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
        make(&mut holder, &stand).await;
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
        let first = make(&mut holder, &stand).await;
        // Recorded, then cleared: that root is let go, and another made once
        // a later block shows the record still empty.
        stand.set(recorded(first));
        holder.look().await.unwrap();
        stand.set(Record {
            at: 1_700_000_500,
            ..Record::default()
        });
        assert_eq!(holder.look().await.unwrap(), Look::Cleared(first));
        assert!(!holder.service.holds_root());
        assert!(holder.attestation.root_leaf().is_none());
        stand.set(Record {
            at: 1_700_000_502,
            ..Record::default()
        });
        let Look::Made(new) = holder.look().await.unwrap() else {
            panic!("no root was made after the clear");
        };
        // Attested only after the record was found empty.
        assert_eq!(holder.attestation.inner_dated_after(), 1_700_000_503);
        assert_ne!(new, first);
        assert_eq!(holder.service.signer(), Some(new));
        assert!(!holder.service.is_serving());
        let leaf = holder.attestation.root_leaf().unwrap();
        assert_eq!((leaf.signer, leaf.due), (new, true));
    }

    /// A root recorded and cleared between two looks was never seen
    /// recorded, and its signer is retired on the chain for good: the
    /// chain's log of its clear lets it go, and a new root is made, rather
    /// than waiting for a record that can never come. Each block is searched
    /// once, from the block the root was made after.
    #[tokio::test]
    async fn a_root_recorded_and_cleared_before_it_was_seen_recorded_is_let_go() {
        let (mut holder, stand, _) = holder();
        let signer = make(&mut holder, &stand).await;
        let made_at = stand.top();
        stand.set(Record::default());
        assert_eq!(holder.look().await.unwrap(), Look::Waiting(signer));
        let waited_at = stand.top();
        // A search that fails changes nothing.
        stand.0.lock().unwrap().clears_unreadable = true;
        assert!(holder.look().await.is_err());
        assert_eq!(holder.service.signer(), Some(signer));
        stand.0.lock().unwrap().clears_unreadable = false;

        // Recorded at one block and cleared at the next, both between looks:
        // the clear is at the block the next look reads.
        stand.log_clear(signer, stand.top() + 1);
        stand.set(Record::default());
        assert_eq!(holder.look().await.unwrap(), Look::Cleared(signer));
        assert!(!holder.service.holds_root());
        assert!(holder.attestation.root_leaf().is_none());
        let searched = stand.0.lock().unwrap().searched.clone();
        assert_eq!(searched[0], (signer, made_at + 1, waited_at));
        assert_eq!(searched.last().unwrap().1, waited_at + 1);

        stand.set(Record::default());
        let Look::Made(new) = holder.look().await.unwrap() else {
            panic!("no root was made after the clear");
        };
        assert_ne!(new, signer);
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
        let signer = make(&mut holder, &stand).await;
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
        let signer = make(&mut holder, &stand).await;
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
        let signer = make(&mut holder, &stand).await;
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 503);
        stand.set(recorded(signer));
        holder.look().await.unwrap();
        assert_eq!(app.clone().oneshot(get()).await.unwrap().status(), 200);
    }
}
