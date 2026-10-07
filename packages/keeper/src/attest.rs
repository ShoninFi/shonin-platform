//! Blessings and gas for the executor's keys.
//!
//! The executor publishes, on its status port (`GET /attestation`), the
//! leaves it holds keys for and, once one is made, the seal that proves them
//! all. Anyone may send the registry's `bless` with them; the keeper does,
//! in its turn on each chain, for every leaf of that chain whose key is not
//! blessed for a while yet, or whose blessing the registry no longer counts
//! although it runs on (the checker it was recorded against retired: sent
//! again, the seal is recorded against the newest checker that accepts it).
//! A `bless` is simulated first and sent only if the simulation goes through,
//! and never again while one sent may still land.
//!
//! The executor also publishes, while it holds the root key, its signer's
//! leaf on the canonical chain. On that chain alone the keeper relays
//! `setRootKey` with the same seal: only while the registry records no root,
//! or records this same signer through a checker that a newer one, or its
//! own retirement, lets the record move from (the registry's own rule);
//! never while it records another signer; only once a simulation goes
//! through; and once per seal.
//!
//! Once the chain shows a key blessed as the executor of a vault that is
//! still live, and for the bundle that vault runs, the keeper keeps that key
//! stocked with gas: the key sends the vault's own transactions, and its
//! approval at the stations. A key the chain doesn't vouch for is never
//! given anything, whatever the executor says about it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::Provider;
use anyhow::{Context, Result};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::vault::{IVault, VaultState};
use chain::{Client, Deployments};
use serde::Deserialize;

/// What the executor publishes about its attestation, as far as the keeper
/// reads it.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Published {
    #[serde(default)]
    pub seal: Option<Bytes>,
    #[serde(default)]
    pub journal: Option<Bytes>,
    #[serde(default)]
    pub leaves: Vec<Leaf>,
    /// The root key's signer's leaf, while the executor holds the root.
    #[serde(default)]
    pub root: Option<RootLeaf>,
}

/// The root key's signer, as a leaf of the tree the seal proves, on the
/// canonical chain.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct RootLeaf {
    /// The canonical chain, whose registry records the root.
    pub chain_id: u64,
    pub signer: Address,
    #[serde(default)]
    pub proof: Vec<B256>,
}

/// One key the executor holds, the vault it is for, and its place in the
/// tree the seal proves.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct Leaf {
    /// The chain the blessing is sent on.
    pub chain_id: u64,
    /// The chain the vault lives on.
    pub vault_chain: u64,
    pub vault: Address,
    pub strategy_hash: B256,
    pub key: Address,
    #[serde(default)]
    pub proof: Vec<B256>,
}

impl Leaf {
    fn id(&self) -> (u64, Address, B256, Address) {
        (self.vault_chain, self.vault, self.strategy_hash, self.key)
    }
}

/// What became of a `bless` the keeper sent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sent {
    /// Mined, and went through.
    Done,
    /// No answer came after it may have gone out: it may still land.
    MayStillLand,
    /// It never went out, or was mined and reverted: it won't land.
    Failed,
}

/// The chain as the relay and the stocking see it: what they read, what
/// they send. The keeper's is [`OnChain`]; the tests' is a stand-in.
pub(crate) trait Registry {
    /// The chain's time, as of its latest block.
    async fn now(&self) -> Result<u64>;
    /// How long a blessing lasts from the attestation it came from.
    async fn ttl(&self) -> Result<u64>;
    /// When the leaf's key's blessing for its vault and bundle runs out;
    /// zero if it has none.
    async fn blessed_until(&self, leaf: &Leaf) -> Result<u64>;
    /// Whether the registry counts the leaf's key as blessed for its vault
    /// and bundle now (`isBlessedFor`): it may not although the blessing
    /// runs on, once the checker it was recorded against has been retired.
    async fn counts(&self, leaf: &Leaf) -> Result<bool>;
    /// Whether the chain vouches for the key as the executor of a live
    /// vault running the leaf's bundle.
    async fn vouches_for(&self, leaf: &Leaf) -> Result<bool>;
    /// Whether `bless` with this seal would go through now (a read: it costs
    /// nothing).
    async fn simulate_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Result<()>;
    async fn send_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Sent;
    /// The root key's record: its signer (zero for none) and the checker it
    /// was recorded through.
    async fn root_record(&self) -> Result<(Address, Address)>;
    /// Whether the registry would let a record made through `checker` move
    /// to another: it is no longer allowed, or a newer allowed one exists.
    async fn root_can_move(&self, checker: Address) -> Result<bool>;
    /// Whether `checker`, the one the root's record was made through, is
    /// still allowed: once it isn't, the record isn't live until a seal is
    /// recorded again through an allowed one.
    async fn root_checker_allowed(&self, checker: Address) -> Result<bool>;
    /// Whether `setRootKey` with this seal would go through now.
    async fn simulate_set_root(&self, seal: &Bytes, journal: &Bytes, root: &RootLeaf)
    -> Result<()>;
    async fn send_set_root(&self, seal: &Bytes, journal: &Bytes, root: &RootLeaf) -> Sent;
    /// Bring the key's gas back up to its stake if it has run below the
    /// top-up line. Says whether it holds its gas now.
    async fn stock(&self, key: Address, stakes: &mut Stakes) -> Result<bool>;
}

/// The stakes sent to each key and not yet seen to land, by their hash and
/// when they were sent.
pub(crate) type Stakes = HashMap<Address, (B256, Instant)>;

/// How often a chain's blessings and keys are looked at.
pub(crate) const EVERY: Duration = Duration::from_secs(15);

/// How long a `bless` that went through holds the next one for the same
/// leaf back: long enough for any node the keeper asks to have its block.
const DONE_HOLD: Duration = Duration::from_secs(90);

/// How long a `bless` that may still land holds the next one back: as long
/// as a send is watched for, and as long as a stake is waited for.
const LANDING_HOLD: Duration = Duration::from_secs(600);

/// What the keeper remembers of one chain's blessings and keys between
/// rounds.
#[derive(Default)]
pub(crate) struct Memory {
    /// Each leaf whose `bless` was sent, and until when no other is sent
    /// for it.
    held: HashMap<(u64, Address, B256, Address), Instant>,
    /// Each leaf whose blessing the registry stopped counting although it
    /// runs on, and the issue time of the seal sent again for it: that seal
    /// is not sent again until the registry has counted the key once more,
    /// so a send that changed nothing is not repeated round after round.
    resent: HashMap<(u64, Address, B256, Address), u64>,
    pub stakes: Stakes,
    /// When the chain was last looked at.
    last: Option<Instant>,
    /// The seal (with its journal) whose `setRootKey` went through: relayed
    /// again only once the checker the record was made through is no longer
    /// allowed.
    root_relayed: Option<B256>,
    /// The seal relayed again for that, while the checker stays retired: it
    /// is not relayed a third time until the record has been seen through an
    /// allowed checker again, so a send that changed nothing isn't repeated
    /// round after round.
    root_resent: Option<B256>,
    /// A `setRootKey` that may still land, for that seal, and until when no
    /// other is sent.
    root_held: Option<(B256, Instant)>,
}

impl Memory {
    /// Whether the chain is due a look.
    pub fn due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|last| now.duration_since(last) >= EVERY)
    }

    /// The chain was looked at, or would have been had the executor
    /// answered.
    pub fn looked(&mut self, at: Instant) {
        self.last = Some(at);
    }
}

/// What one round did, for the log and the tests.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Round {
    pub sent: Vec<Address>,
    pub not_simulated: Vec<Address>,
    pub stocked: Vec<Address>,
}

/// The issue time the journal states: its last word (the journal is the
/// registry's `abi.encode(imageDigest, projectId, signer, vaultsRoot,
/// issuedAt)`).
fn issued_at(journal: &[u8]) -> Option<u64> {
    if journal.len() != 160 {
        return None;
    }
    let word = U256::from_be_slice(&journal[128..160]);
    u64::try_from(word).ok()
}

/// One look at a chain: relay what needs blessing, stock what is blessed.
pub(crate) async fn round(
    registry: &impl Registry,
    chain_id: u64,
    published: &Published,
    memory: &mut Memory,
    at: Instant,
) -> Result<Round> {
    memory.looked(at);
    let mut done = Round::default();
    let leaves: Vec<&Leaf> = published
        .leaves
        .iter()
        .filter(|leaf| leaf.chain_id == chain_id)
        .collect();
    // What is no longer published is no longer held.
    let published_ids: HashSet<_> = leaves.iter().map(|leaf| leaf.id()).collect();
    memory
        .held
        .retain(|id, until| published_ids.contains(id) && at < *until);
    memory.resent.retain(|id, _| published_ids.contains(id));
    if leaves.is_empty() {
        return Ok(done);
    }

    let now = registry.now().await?;
    let ttl = registry.ttl().await?;
    for leaf in &leaves {
        let until = registry.blessed_until(leaf).await?;
        let Some(issued) = published.journal.as_ref().map(|journal| issued_at(journal)) else {
            continue;
        };
        // Down to a third of its blessing, and the seal adds something.
        let renews = until < now.saturating_add(ttl / 3)
            && issued.is_none_or(|issued| issued.saturating_add(ttl) > until);
        // Or the blessing runs on but the registry no longer counts it (the
        // checker it was recorded against retired): sending the seal again
        // has the newest checker that accepts it record it afresh. Once per
        // seal until the key counts again, so a send that changes nothing
        // isn't repeated.
        let mut recounts = false;
        if !renews && leaf.vault != Address::ZERO && until >= now {
            if registry.counts(leaf).await? {
                memory.resent.remove(&leaf.id());
            } else {
                recounts = memory.resent.get(&leaf.id()) != issued.as_ref();
            }
        }
        if !renews && !recounts {
            continue;
        }
        let (Some(seal), Some(journal)) = (&published.seal, &published.journal) else {
            continue;
        };
        if memory.held.contains_key(&leaf.id()) {
            continue;
        }
        if let Err(error) = registry.simulate_bless(seal, journal, leaf).await {
            tracing::debug!(vault = %leaf.vault, key = %leaf.key, error = format!("{error:#}"), "a blessing would not go through; not sent");
            done.not_simulated.push(leaf.key);
            continue;
        }
        let sent = registry.send_bless(seal, journal, leaf).await;
        let hold = match sent {
            Sent::Done => Some(DONE_HOLD),
            Sent::MayStillLand => Some(LANDING_HOLD),
            Sent::Failed => None,
        };
        if let Some(hold) = hold {
            memory.held.insert(leaf.id(), at + hold);
        }
        if sent == Sent::Done {
            if recounts && let Some(issued) = issued {
                memory.resent.insert(leaf.id(), issued);
            }
            tracing::info!(vault = %leaf.vault, key = %leaf.key, "a vault key's blessing was relayed");
        }
        done.sent.push(leaf.key);
    }

    // Each key is stocked once a round, whatever number of leaves name it,
    // and only once the chain vouches for it.
    let mut looked = HashSet::new();
    for leaf in &leaves {
        if leaf.vault == Address::ZERO || !looked.insert(leaf.key) {
            continue;
        }
        if !registry.vouches_for(leaf).await? {
            continue;
        }
        registry
            .stock(leaf.key, &mut memory.stakes)
            .await
            .with_context(|| format!("stocking {} with gas", leaf.key))?;
        done.stocked.push(leaf.key);
    }
    Ok(done)
}

/// One look at a chain: the vault keys' blessings and gas first, then the
/// root key's record. Each runs whatever became of the other, so a root that
/// can't be recorded (its simulation reverts, or its record can't be read)
/// never holds back a vault key's blessing from the same seal, and the other
/// way round. Either one's error is said; the blessings' first.
pub(crate) async fn look(
    registry: &impl Registry,
    chain_id: u64,
    canonical: bool,
    published: &Published,
    memory: &mut Memory,
    at: Instant,
) -> Result<()> {
    let blessed = round(registry, chain_id, published, memory, at).await;
    let recorded = root_round(registry, chain_id, canonical, published, memory, at).await;
    blessed.map(|_| ()).and(recorded.map(|_| ()))
}

/// What the root's relay did in one round, for the log and the tests.
#[derive(Debug, PartialEq)]
pub(crate) enum RootRelay {
    /// Nothing to relay: not the canonical chain, no root published, no
    /// seal, already relayed for this seal, or the record doesn't want it.
    Nothing,
    /// The registry records another signer: never relayed.
    AnotherSigner(Address),
    /// The simulation did not go through: not sent.
    NotSimulated,
    /// Sent, with what became of it.
    Sent(Sent),
}

/// The seal and journal, as one: what "once per seal" counts by.
fn seal_id(seal: &Bytes, journal: &Bytes) -> B256 {
    alloy::primitives::keccak256([seal.as_ref(), journal.as_ref()].concat())
}

/// One look at the root key's record on a chain: relay `setRootKey` from the
/// executor's seal if this is the canonical chain and the record wants it.
pub(crate) async fn root_round(
    registry: &impl Registry,
    chain_id: u64,
    canonical: bool,
    published: &Published,
    memory: &mut Memory,
    at: Instant,
) -> Result<RootRelay> {
    let Some(root) = published.root.as_ref() else {
        return Ok(RootRelay::Nothing);
    };
    // The root is recorded on the canonical chain alone, whatever chain the
    // executor names.
    if !canonical || root.chain_id != chain_id {
        return Ok(RootRelay::Nothing);
    }
    let (Some(seal), Some(journal)) = (&published.seal, &published.journal) else {
        return Ok(RootRelay::Nothing);
    };
    let id = seal_id(seal, journal);
    if memory
        .root_held
        .is_some_and(|(held, until)| held == id && at < until)
    {
        return Ok(RootRelay::Nothing);
    }
    let relayed = memory.root_relayed == Some(id);
    let (recorded, checker) = registry.root_record().await?;
    let mut resends = false;
    if recorded == Address::ZERO {
        // This seal recorded it, and the record is empty now: cleared, and
        // that signer refused for good. Never sent again.
        if relayed {
            return Ok(RootRelay::Nothing);
        }
    } else {
        if recorded != root.signer {
            return Ok(RootRelay::AnotherSigner(recorded));
        }
        if relayed {
            // This seal recorded it already. Once the checker it was
            // recorded through is retired, sending it again has an allowed
            // checker record it afresh, as a vault key's blessing is: once
            // per seal while that checker stays retired.
            if registry.root_checker_allowed(checker).await? {
                memory.root_resent = None;
                return Ok(RootRelay::Nothing);
            }
            if memory.root_resent == Some(id) {
                return Ok(RootRelay::Nothing);
            }
            resends = true;
        } else if !registry.root_can_move(checker).await? {
            return Ok(RootRelay::Nothing);
        }
    }
    if let Err(error) = registry.simulate_set_root(seal, journal, root).await {
        tracing::debug!(signer = %root.signer, error = format!("{error:#}"), "recording the root key would not go through; not sent");
        return Ok(RootRelay::NotSimulated);
    }
    let sent = registry.send_set_root(seal, journal, root).await;
    match sent {
        Sent::Done => {
            memory.root_relayed = Some(id);
            if resends {
                memory.root_resent = Some(id);
            }
            memory.root_held = None;
            tracing::info!(signer = %root.signer, "the root key's signer was relayed to the canonical chain's registry");
        }
        Sent::MayStillLand => memory.root_held = Some((id, at + LANDING_HOLD)),
        Sent::Failed => memory.root_held = None,
    }
    Ok(RootRelay::Sent(sent))
}

/// Ask the executor what it publishes about its attestation.
pub(crate) async fn published(http: &reqwest::Client, executor: &str) -> Result<Published> {
    let reply = http
        .get(format!("{}/attestation", executor.trim_end_matches('/')))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .context("asking the executor for its attestation")?;
    anyhow::ensure!(
        reply.status().is_success(),
        "the executor answered {} for its attestation",
        reply.status()
    );
    reply
        .json()
        .await
        .context("reading the executor's attestation")
}

// The chain itself --------------------------------------------------------

/// The registry and the keeper's account on one chain.
pub(crate) struct OnChain<'a> {
    pub client: &'a Client,
    pub deployments: &'a Deployments,
    pub keeper: Address,
}

impl Registry for OnChain<'_> {
    async fn now(&self) -> Result<u64> {
        self.client.block_timestamp().await
    }

    async fn ttl(&self) -> Result<u64> {
        self.client
            .read(
                self.deployments.attestation,
                IAttestationRegistry::blessingTtlCall {},
            )
            .await
    }

    async fn blessed_until(&self, leaf: &Leaf) -> Result<u64> {
        self.client
            .read(
                self.deployments.attestation,
                IAttestationRegistry::blessedUntilCall {
                    signer: leaf.key,
                    vaultChain: U256::from(leaf.vault_chain),
                    vault: leaf.vault,
                    strategyHash: leaf.strategy_hash,
                },
            )
            .await
    }

    async fn counts(&self, leaf: &Leaf) -> Result<bool> {
        self.client
            .read(
                self.deployments.attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer: leaf.key,
                    vaultChain: U256::from(leaf.vault_chain),
                    vault: leaf.vault,
                    strategyHash: leaf.strategy_hash,
                },
            )
            .await
    }

    async fn vouches_for(&self, leaf: &Leaf) -> Result<bool> {
        let attestation = self.deployments.attestation;
        if !self.counts(leaf).await? {
            return Ok(false);
        }
        let info = self
            .client
            .read(
                attestation,
                IAttestationRegistry::keyInfoCall { signer: leaf.key },
            )
            .await?;
        if !matches!(info.kind, ImageKind::Executor) {
            return Ok(false);
        }
        // A vault on another chain is read there; here the blessing, which
        // names it, is what vouches.
        if leaf.vault_chain != self.client.chain_id {
            return Ok(true);
        }
        let state = self.client.read(leaf.vault, IVault::stateCall {}).await?;
        if matches!(
            state,
            VaultState::None | VaultState::Closed | VaultState::Cancelled
        ) {
            return Ok(false);
        }
        let bundle = self
            .client
            .read(leaf.vault, IVault::bundleHashCall {})
            .await?;
        Ok(bundle == leaf.strategy_hash)
    }

    async fn simulate_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Result<()> {
        self.client
            .read(
                self.deployments.attestation,
                bless_call(seal, journal, leaf),
            )
            .await
            .map(|_| ())
    }

    async fn send_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Sent {
        match self
            .client
            .send(
                self.keeper,
                self.deployments.attestation,
                bless_call(seal, journal, leaf),
            )
            .await
        {
            Ok(_) => Sent::Done,
            Err(error) => {
                tracing::warn!(vault = %leaf.vault, key = %leaf.key, error = format!("{error:#}"), "a blessing the keeper sent did not go through");
                if chain::may_still_land(&error) {
                    Sent::MayStillLand
                } else {
                    Sent::Failed
                }
            }
        }
    }

    async fn stock(&self, key: Address, stakes: &mut Stakes) -> Result<bool> {
        fund_gas(self.client, self.keeper, stakes, key).await
    }

    async fn root_record(&self) -> Result<(Address, Address)> {
        let record = self
            .client
            .read(
                self.deployments.attestation,
                IAttestationRegistry::rootKeyCall {},
            )
            .await?;
        Ok((record.signer, record.verifier))
    }

    async fn root_can_move(&self, checker: Address) -> Result<bool> {
        let registry = self.deployments.attestation;
        let allowed = |verifier| async move {
            self.client
                .read(
                    registry,
                    IAttestationRegistry::isVerifierAllowedCall { verifier },
                )
                .await
        };
        if !allowed(checker).await? {
            return Ok(true);
        }
        // The checkers in the order they were proposed: one after the
        // record's that is allowed now is newer.
        let listed = self
            .client
            .read(registry, IAttestationRegistry::verifiersCall {})
            .await?;
        let Some(at) = listed.iter().position(|listed| *listed == checker) else {
            return Ok(false);
        };
        for newer in &listed[at + 1..] {
            if allowed(*newer).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn root_checker_allowed(&self, checker: Address) -> Result<bool> {
        self.client
            .read(
                self.deployments.attestation,
                IAttestationRegistry::isVerifierAllowedCall { verifier: checker },
            )
            .await
    }

    async fn simulate_set_root(
        &self,
        seal: &Bytes,
        journal: &Bytes,
        root: &RootLeaf,
    ) -> Result<()> {
        self.client
            .read(
                self.deployments.attestation,
                set_root_call(seal, journal, root),
            )
            .await
            .map(|_| ())
    }

    async fn send_set_root(&self, seal: &Bytes, journal: &Bytes, root: &RootLeaf) -> Sent {
        match self
            .client
            .send(
                self.keeper,
                self.deployments.attestation,
                set_root_call(seal, journal, root),
            )
            .await
        {
            Ok(_) => Sent::Done,
            Err(error) => {
                tracing::warn!(signer = %root.signer, error = format!("{error:#}"), "recording the root key did not go through");
                if chain::may_still_land(&error) {
                    Sent::MayStillLand
                } else {
                    Sent::Failed
                }
            }
        }
    }
}

fn set_root_call(
    seal: &Bytes,
    journal: &Bytes,
    root: &RootLeaf,
) -> IAttestationRegistry::setRootKeyCall {
    IAttestationRegistry::setRootKeyCall {
        seal: seal.clone(),
        journal: journal.clone(),
        rootSigner: root.signer,
        merkleProof: root.proof.clone(),
    }
}

fn bless_call(seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> IAttestationRegistry::blessCall {
    IAttestationRegistry::blessCall {
        seal: seal.clone(),
        journal: journal.clone(),
        vaultChain: U256::from(leaf.vault_chain),
        vault: leaf.vault,
        strategyHash: leaf.strategy_hash,
        key: leaf.key,
        merkleProof: leaf.proof.clone(),
    }
}

// Gas: the executor's rules, now the keeper's ------------------------------

/// A key's stake on a development chain, whose coin is free.
const GAS_STAKE_DEVELOPMENT: u128 = 1_000_000_000_000_000_000;
/// A key's stake on a public chain, before fees are counted: small, since
/// every vault's key holds one, and grown to three worst-case sends at the
/// chain's own fees whenever that is more.
const GAS_STAKE_PUBLIC: u128 = 500_000_000_000_000;

/// Roughly what one of the key's larger sends uses, such as an approval.
const ONE_SEND_GAS: u128 = 300_000;

/// How long a stake that was sent is waited for before it is taken for lost.
const STAKE_LANDING: Duration = Duration::from_secs(600);

fn gas_stake(chain: &Client) -> u128 {
    if chain.is_development() {
        GAS_STAKE_DEVELOPMENT
    } else {
        GAS_STAKE_PUBLIC
    }
}

/// The stake at today's fee: the chain's stake, or three of the key's
/// worst-case sends if that is more.
fn stake_at(base: u128, worst_send: u128) -> u128 {
    base.max(worst_send.saturating_mul(3))
}

/// Below what a key is topped up (back to a whole stake): half a stake, so a
/// key spends half its stake between top-ups rather than being sent a few
/// thousandths of it after every send it makes, each top-up a transfer of its
/// own. Never below two worst-case sends, so a key is topped up while it can
/// still pay for its next one, however the stake compares with the fee.
fn top_up_below(stake: u128, worst_send: Option<u128>) -> u128 {
    let half = stake / 2;
    match worst_send {
        Some(send) => half.max(send.saturating_mul(2)),
        None => half,
    }
}

/// What one of the key's larger sends can cost at most at today's fees.
async fn worst_send(chain: &Client) -> Result<u128> {
    let per_gas = match chain.provider().estimate_eip1559_fees().await {
        Ok(fees) => fees.max_fee_per_gas,
        Err(_) => chain
            .provider()
            .get_gas_price()
            .await
            .context("reading the gas price")?,
    };
    Ok(per_gas.saturating_mul(ONE_SEND_GAS))
}

/// Keep a key stocked with gas from the keeper's account: when it holds
/// less than half a stake (see [`top_up_below`]), send it what brings it back
/// up to a whole one. A stake sent and not yet seen to land is waited for, not sent
/// again; the key's pending balance counts too.
async fn fund_gas(
    chain: &Client,
    keeper: Address,
    stakes: &mut Stakes,
    key: Address,
) -> Result<bool> {
    let mut landing = false;
    if let Some((hash, sent)) = stakes.get(&key).copied()
        && sent.elapsed() < STAKE_LANDING
    {
        match chain.provider().get_transaction_receipt(hash).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => landing = true,
        }
    }
    if !landing {
        stakes.remove(&key);
    }

    let send = worst_send(chain).await.ok();
    let stake = stake_at(gas_stake(chain), send.unwrap_or(0));
    let below = U256::from(top_up_below(stake, send));
    let stake = U256::from(stake);
    let latest = chain
        .balance(key)
        .await
        .context("reading what gas the key holds")?;
    if latest >= below {
        return Ok(true);
    }
    if landing {
        return Ok(false);
    }
    let pending = chain
        .provider()
        .get_balance(key)
        .pending()
        .await
        .unwrap_or(latest);
    let holds = latest.max(pending);
    if holds >= below {
        return Ok(false);
    }
    let noted = std::sync::Arc::new(std::sync::Mutex::new(None));
    let noting = noted.clone();
    let sent = chain
        .transfer_noting(keeper, key, stake - holds, move |hash| {
            if let Ok(mut noted) = noting.lock() {
                *noted = Some(hash);
            }
        })
        .await;
    match sent {
        Ok(_) => {
            tracing::info!(%key, "a blessed vault key was stocked with gas");
            Ok(true)
        }
        Err(error) => {
            // A stake that went out and wasn't seen to land is waited for.
            if let Some(hash) = noted.lock().ok().and_then(|noted| *noted) {
                stakes.insert(key, (hash, Instant::now()));
            }
            Err(error).context("staking the key with gas")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A registry as the tests set it up, noting what was sent and stocked.
    #[derive(Default)]
    struct Stand {
        now: u64,
        ttl: u64,
        until: HashMap<Address, u64>,
        vouched: HashSet<Address>,
        /// Keys whose blessing the registry doesn't count although it runs
        /// on (the checker it was recorded against retired).
        uncounted: HashSet<Address>,
        simulation_fails: bool,
        sends_answer: Option<Sent>,
        sent: Mutex<Vec<Address>>,
        stocked: Mutex<Vec<Address>>,
        /// The root key's record: its signer and checker.
        root: (Address, Address),
        /// Whether that record's checker lets it move.
        root_moves: bool,
        /// Whether that record's checker is retired.
        root_checker_retired: bool,
        root_simulation_fails: bool,
        /// The root key's record can't be read (the node errs).
        root_unreadable: bool,
        roots_simulated: Mutex<usize>,
        roots_sent: Mutex<Vec<Address>>,
    }

    impl Registry for Stand {
        async fn now(&self) -> Result<u64> {
            Ok(self.now)
        }
        async fn ttl(&self) -> Result<u64> {
            Ok(self.ttl)
        }
        async fn blessed_until(&self, leaf: &Leaf) -> Result<u64> {
            Ok(self.until.get(&leaf.key).copied().unwrap_or(0))
        }
        async fn counts(&self, leaf: &Leaf) -> Result<bool> {
            Ok(!self.uncounted.contains(&leaf.key))
        }
        async fn vouches_for(&self, leaf: &Leaf) -> Result<bool> {
            Ok(self.vouched.contains(&leaf.key))
        }
        async fn simulate_bless(&self, _: &Bytes, _: &Bytes, _: &Leaf) -> Result<()> {
            if self.simulation_fails {
                anyhow::bail!("execution reverted: NotAmongTheVaults");
            }
            Ok(())
        }
        async fn send_bless(&self, _: &Bytes, _: &Bytes, leaf: &Leaf) -> Sent {
            self.sent.lock().unwrap().push(leaf.key);
            self.sends_answer.unwrap_or(Sent::Done)
        }
        async fn stock(&self, key: Address, _: &mut Stakes) -> Result<bool> {
            self.stocked.lock().unwrap().push(key);
            Ok(true)
        }
        async fn root_record(&self) -> Result<(Address, Address)> {
            if self.root_unreadable {
                anyhow::bail!("the node did not answer");
            }
            Ok(self.root)
        }
        async fn root_can_move(&self, _: Address) -> Result<bool> {
            Ok(self.root_moves)
        }
        async fn root_checker_allowed(&self, _: Address) -> Result<bool> {
            Ok(!self.root_checker_retired)
        }
        async fn simulate_set_root(&self, _: &Bytes, _: &Bytes, _: &RootLeaf) -> Result<()> {
            *self.roots_simulated.lock().unwrap() += 1;
            if self.root_simulation_fails {
                anyhow::bail!("execution reverted: RootKeyAlreadySet");
            }
            Ok(())
        }
        async fn send_set_root(&self, _: &Bytes, _: &Bytes, root: &RootLeaf) -> Sent {
            self.roots_sent.lock().unwrap().push(root.signer);
            self.sends_answer.unwrap_or(Sent::Done)
        }
    }

    const CHAIN: u64 = 46_630;
    const TTL: u64 = 72 * 3_600;
    const NOW: u64 = 1_800_000_000;

    fn leaf(key: u8) -> Leaf {
        Leaf {
            chain_id: CHAIN,
            vault_chain: CHAIN,
            vault: Address::repeat_byte(0x40 + key),
            strategy_hash: B256::repeat_byte(0x50),
            key: Address::repeat_byte(key),
            proof: vec![B256::repeat_byte(0x60)],
        }
    }

    /// A journal issued at `issued`.
    fn journal(issued: u64) -> Bytes {
        let mut bytes = vec![0u8; 160];
        bytes[128..160].copy_from_slice(&U256::from(issued).to_be_bytes::<32>());
        Bytes::from(bytes)
    }

    fn published(leaves: Vec<Leaf>) -> Published {
        Published {
            seal: Some(Bytes::from(vec![7u8; 324])),
            journal: Some(journal(NOW - 60)),
            leaves,
            root: None,
        }
    }

    fn stand() -> Stand {
        Stand {
            now: NOW,
            ttl: TTL,
            ..Stand::default()
        }
    }

    #[tokio::test]
    async fn a_key_the_chain_does_not_vouch_for_is_never_funded() {
        let mut stand = stand();
        // Key 1 is blessed and vouched for; key 2 is published by the
        // executor but the chain says nothing for it; key 3 is blessed for
        // no vault (a builder's leaf).
        stand.until.insert(Address::repeat_byte(1), NOW + TTL);
        stand.vouched.insert(Address::repeat_byte(1));
        let mut builder = leaf(3);
        builder.vault = Address::ZERO;
        stand.vouched.insert(Address::repeat_byte(3));
        let published = published(vec![leaf(1), leaf(2), builder]);
        let mut memory = Memory::default();
        round(&stand, CHAIN, &published, &mut memory, Instant::now())
            .await
            .unwrap();
        assert_eq!(*stand.stocked.lock().unwrap(), [Address::repeat_byte(1)]);

        // However many rounds, and with no seal at all.
        let mut bare = published.clone();
        bare.seal = None;
        bare.journal = None;
        for _ in 0..3 {
            round(&stand, CHAIN, &bare, &mut memory, Instant::now())
                .await
                .unwrap();
        }
        assert!(
            stand
                .stocked
                .lock()
                .unwrap()
                .iter()
                .all(|key| *key == Address::repeat_byte(1))
        );
    }

    #[tokio::test]
    async fn a_relay_is_not_sent_twice_while_one_may_still_land() {
        let mut stand = stand();
        stand.sends_answer = Some(Sent::MayStillLand);
        let published = published(vec![leaf(1)]);
        let mut memory = Memory::default();
        let start = Instant::now();
        round(&stand, CHAIN, &published, &mut memory, start)
            .await
            .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 1);
        // The chain still shows no blessing, round after round, while the
        // first send may still land.
        for seconds in [15, 60, 300, 590] {
            round(
                &stand,
                CHAIN,
                &published,
                &mut memory,
                start + Duration::from_secs(seconds),
            )
            .await
            .unwrap();
        }
        assert_eq!(stand.sent.lock().unwrap().len(), 1);
        // Once it no longer may, the blessing is sent again.
        round(
            &stand,
            CHAIN,
            &published,
            &mut memory,
            start + LANDING_HOLD + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_relay_that_went_through_is_not_sent_again_for_the_same_seal() {
        let mut stand = stand();
        let published = published(vec![leaf(1)]);
        let mut memory = Memory::default();
        round(&stand, CHAIN, &published, &mut memory, Instant::now())
            .await
            .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 1);
        // The chain now holds this seal's blessing, close to running out:
        // sending it again would change nothing, so a newer seal is waited
        // for.
        stand.until.insert(Address::repeat_byte(1), NOW - 60 + TTL);
        stand.now = NOW - 60 + TTL - 10;
        round(
            &stand,
            CHAIN,
            &published,
            &mut memory,
            Instant::now() + LANDING_HOLD * 2,
        )
        .await
        .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_seal_that_fails_simulation_is_not_sent() {
        let mut stand = stand();
        stand.simulation_fails = true;
        let published = published(vec![leaf(1), leaf(2)]);
        let mut memory = Memory::default();
        let done = round(&stand, CHAIN, &published, &mut memory, Instant::now())
            .await
            .unwrap();
        assert!(stand.sent.lock().unwrap().is_empty());
        assert_eq!(done.not_simulated.len(), 2);
    }

    #[tokio::test]
    async fn a_key_blessed_for_a_while_yet_or_on_another_chain_is_not_relayed() {
        let mut stand = stand();
        stand
            .until
            .insert(Address::repeat_byte(1), NOW + TTL / 3 + 1);
        let mut elsewhere = leaf(2);
        elsewhere.chain_id = 84_532;
        let published = published(vec![leaf(1), elsewhere]);
        let mut memory = Memory::default();
        round(&stand, CHAIN, &published, &mut memory, Instant::now())
            .await
            .unwrap();
        assert!(stand.sent.lock().unwrap().is_empty());
    }

    /// A key the registry no longer counts as blessed (the checker its
    /// blessing was recorded against retired), however far its blessing
    /// still runs, has the seal it came from sent again, so the newest
    /// checker that accepts it records it afresh.
    #[tokio::test]
    async fn a_key_the_registry_no_longer_counts_is_relayed_again_however_far_its_blessing_runs() {
        let mut stand = stand();
        // Attested an hour ago: the blessing runs almost a whole TTL yet.
        let issued = NOW - 3_600;
        stand.until.insert(Address::repeat_byte(1), issued + TTL);
        stand.uncounted.insert(Address::repeat_byte(1));
        let mut published = published(vec![leaf(1)]);
        published.journal = Some(journal(issued));
        let mut memory = Memory::default();
        round(&stand, CHAIN, &published, &mut memory, Instant::now())
            .await
            .unwrap();
        assert_eq!(*stand.sent.lock().unwrap(), [Address::repeat_byte(1)]);
    }

    /// A seal sent again for a key the registry still doesn't count after
    /// it went through changed nothing, and is not sent again round after
    /// round; once the key counts again and then stops, it is.
    #[tokio::test]
    async fn a_seal_sent_again_that_changed_nothing_is_not_repeated() {
        let mut stand = stand();
        let issued = NOW - 3_600;
        stand.until.insert(Address::repeat_byte(1), issued + TTL);
        stand.uncounted.insert(Address::repeat_byte(1));
        let mut published = published(vec![leaf(1)]);
        published.journal = Some(journal(issued));
        let mut memory = Memory::default();
        let start = Instant::now();
        for seconds in [0, 15, 120, 900, 3_600] {
            round(
                &stand,
                CHAIN,
                &published,
                &mut memory,
                start + Duration::from_secs(seconds),
            )
            .await
            .unwrap();
        }
        assert_eq!(stand.sent.lock().unwrap().len(), 1);

        // The key counts again, then stops (another rotation): the seal is
        // sent once more.
        stand.uncounted.clear();
        round(
            &stand,
            CHAIN,
            &published,
            &mut memory,
            start + Duration::from_secs(4_000),
        )
        .await
        .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 1);
        stand.uncounted.insert(Address::repeat_byte(1));
        round(
            &stand,
            CHAIN,
            &published,
            &mut memory,
            start + Duration::from_secs(4_100),
        )
        .await
        .unwrap();
        assert_eq!(stand.sent.lock().unwrap().len(), 2);
    }

    /// A seal the registry would refuse is never sent again for a key it
    /// doesn't count.
    #[tokio::test]
    async fn a_seal_sent_again_is_simulated_first() {
        let mut stand = stand();
        stand.simulation_fails = true;
        let issued = NOW - 3_600;
        stand.until.insert(Address::repeat_byte(1), issued + TTL);
        stand.uncounted.insert(Address::repeat_byte(1));
        let mut published = published(vec![leaf(1)]);
        published.journal = Some(journal(issued));
        let done = round(
            &stand,
            CHAIN,
            &published,
            &mut Memory::default(),
            Instant::now(),
        )
        .await
        .unwrap();
        assert!(stand.sent.lock().unwrap().is_empty());
        assert_eq!(done.not_simulated, [Address::repeat_byte(1)]);
    }

    #[test]
    fn a_key_is_topped_up_below_half_its_stake_and_never_below_two_sends() {
        assert_eq!(stake_at(500, 100), 500);
        assert_eq!(stake_at(500, 200), 600);
        // Half the stake, however many sends that leaves room for.
        assert_eq!(top_up_below(500, Some(10)), 250);
        assert_eq!(top_up_below(600, Some(100)), 300);
        // A stake of three sends would be topped up at a send and a half:
        // two sends it is, so the next one can still be paid for.
        assert_eq!(top_up_below(600, Some(200)), 400);
        assert_eq!(top_up_below(500, Some(400)), 800);
        assert_eq!(top_up_below(500, None), 250);
    }

    /// The public chains' stake (half a thousandth of the coin), at a fee
    /// whose worst-case send is three hundredths of a thousandth: the key is
    /// left to spend down to half its stake, not topped up after each send.
    #[test]
    fn a_key_is_topped_up_only_below_half_its_stake() {
        let send = 30_000_000_000_000;
        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        assert_eq!(stake, 500_000_000_000_000);
        let below = top_up_below(stake, Some(send));
        assert_eq!(below, 250_000_000_000_000);
        // A key one ordinary send short of its stake is not topped up.
        assert!(stake - 7_000_000_000_000 >= below);
        // Its top-up brings it from just under half a stake back to a whole
        // one, which it can still spend a worst-case send from twice over.
        assert!(below >= 2 * send);
    }

    #[test]
    fn the_executor_s_attestation_reads_as_published() {
        let json = r#"{
            "mode": "launcher",
            "binding": {"evm_signer": "0x0000000000000000000000000000000000000001"},
            "nonce": "00", "token": null, "token_issued_at": null,
            "seal": null, "journal": null, "seal_wanted": true,
            "leaves": [{"chain_id": 46630, "vault_chain": 46630,
                "vault": "0x1111111111111111111111111111111111111111",
                "strategy_hash": "0x2222222222222222222222222222222222222222222222222222222222222222",
                "key": "0x3333333333333333333333333333333333333333",
                "leaf": "0x4444444444444444444444444444444444444444444444444444444444444444",
                "proof": []}]
        }"#;
        let published: Published = serde_json::from_str(json).unwrap();
        assert!(published.seal.is_none());
        assert_eq!(published.leaves.len(), 1);
        assert_eq!(published.leaves[0].key, Address::repeat_byte(0x33));
        assert_eq!(issued_at(&journal(42)), Some(42));
        assert_eq!(issued_at(&[0u8; 12]), None);
    }
    // The root key ---------------------------------------------------------

    const SIGNER: Address = Address::repeat_byte(0x7e);

    fn with_root(chain_id: u64) -> Published {
        let mut published = published(vec![leaf(1)]);
        published.root = Some(RootLeaf {
            chain_id,
            signer: SIGNER,
            proof: vec![B256::repeat_byte(0x61)],
        });
        published
    }

    async fn relay(
        stand: &Stand,
        canonical: bool,
        published: &Published,
        memory: &mut Memory,
        at: Instant,
    ) -> RootRelay {
        root_round(stand, CHAIN, canonical, published, memory, at)
            .await
            .unwrap()
    }

    /// A root that can't be recorded (its `setRootKey` would revert, or its
    /// record can't be read) never keeps the same seal's vault key from
    /// being blessed and stocked in the same look.
    #[tokio::test]
    async fn a_root_that_cannot_be_recorded_never_holds_back_a_vault_key_blessing() {
        for unreadable in [false, true] {
            let mut stand = stand();
            stand.root_simulation_fails = !unreadable;
            stand.root_unreadable = unreadable;
            stand.vouched.insert(Address::repeat_byte(1));
            let mut memory = Memory::default();
            let looked = look(
                &stand,
                CHAIN,
                true,
                &with_root(CHAIN),
                &mut memory,
                Instant::now(),
            )
            .await;
            assert_eq!(looked.is_err(), unreadable);
            assert_eq!(*stand.sent.lock().unwrap(), [Address::repeat_byte(1)]);
            assert_eq!(*stand.stocked.lock().unwrap(), [Address::repeat_byte(1)]);
            assert!(stand.roots_sent.lock().unwrap().is_empty());
        }
    }

    /// The other way round: a vault key whose blessing would revert never
    /// keeps the root's record from being relayed from the same seal.
    #[tokio::test]
    async fn a_vault_key_that_cannot_be_blessed_never_holds_back_the_root() {
        let mut stand = stand();
        stand.simulation_fails = true;
        let mut memory = Memory::default();
        look(
            &stand,
            CHAIN,
            true,
            &with_root(CHAIN),
            &mut memory,
            Instant::now(),
        )
        .await
        .unwrap();
        assert!(stand.sent.lock().unwrap().is_empty());
        assert_eq!(*stand.roots_sent.lock().unwrap(), [SIGNER]);
    }

    #[tokio::test]
    async fn the_root_is_relayed_once_per_seal() {
        let stand = stand();
        let published = with_root(CHAIN);
        let mut memory = Memory::default();
        let start = Instant::now();
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Sent(Sent::Done)
        );
        // The chain hasn't caught up (the record still reads empty) round
        // after round, and hours later: the same seal is not sent again.
        for seconds in [15, 120, 900, 7_200] {
            assert_eq!(
                relay(
                    &stand,
                    true,
                    &published,
                    &mut memory,
                    start + Duration::from_secs(seconds)
                )
                .await,
                RootRelay::Nothing
            );
        }
        assert_eq!(*stand.roots_sent.lock().unwrap(), [SIGNER]);
        // A new seal is a new relay.
        let mut fresh = published.clone();
        fresh.journal = Some(journal(NOW));
        assert_eq!(
            relay(&stand, true, &fresh, &mut memory, start).await,
            RootRelay::Sent(Sent::Done)
        );
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_root_relay_that_may_still_land_is_not_sent_again_meanwhile() {
        let mut stand = stand();
        stand.sends_answer = Some(Sent::MayStillLand);
        let published = with_root(CHAIN);
        let mut memory = Memory::default();
        let start = Instant::now();
        relay(&stand, true, &published, &mut memory, start).await;
        relay(
            &stand,
            true,
            &published,
            &mut memory,
            start + Duration::from_secs(300),
        )
        .await;
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_root_is_relayed_only_on_the_canonical_chain() {
        let stand = stand();
        let mut memory = Memory::default();
        // Not the canonical chain, though the executor names this one.
        assert_eq!(
            relay(
                &stand,
                false,
                &with_root(CHAIN),
                &mut memory,
                Instant::now()
            )
            .await,
            RootRelay::Nothing
        );
        // The canonical chain, but the executor names another.
        assert_eq!(
            relay(
                &stand,
                true,
                &with_root(84_532),
                &mut memory,
                Instant::now()
            )
            .await,
            RootRelay::Nothing
        );
        assert!(stand.roots_sent.lock().unwrap().is_empty());
        assert_eq!(*stand.roots_simulated.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn the_root_is_relayed_only_after_a_passing_simulation() {
        let mut stand = stand();
        stand.root_simulation_fails = true;
        let mut memory = Memory::default();
        assert_eq!(
            relay(&stand, true, &with_root(CHAIN), &mut memory, Instant::now()).await,
            RootRelay::NotSimulated
        );
        assert!(stand.roots_sent.lock().unwrap().is_empty());
        // Without a seal there is nothing to simulate.
        let mut bare = with_root(CHAIN);
        bare.seal = None;
        stand.root_simulation_fails = false;
        assert_eq!(
            relay(&stand, true, &bare, &mut memory, Instant::now()).await,
            RootRelay::Nothing
        );
        assert!(stand.roots_sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_root_is_never_relayed_while_another_signer_is_recorded() {
        let mut stand = stand();
        let other = Address::repeat_byte(0x0e);
        stand.root = (other, Address::repeat_byte(0xcc));
        stand.root_moves = true;
        let mut memory = Memory::default();
        for _ in 0..3 {
            assert_eq!(
                relay(&stand, true, &with_root(CHAIN), &mut memory, Instant::now()).await,
                RootRelay::AnotherSigner(other)
            );
        }
        assert!(stand.roots_sent.lock().unwrap().is_empty());
        assert_eq!(*stand.roots_simulated.lock().unwrap(), 0);
    }

    /// The same signer recorded is relayed again only where the registry
    /// lets the record move: its checker retired, or a newer one allowed.
    #[tokio::test]
    async fn the_same_signer_is_relayed_again_only_when_its_record_can_move() {
        let mut stand = stand();
        stand.root = (SIGNER, Address::repeat_byte(0xcc));
        let mut memory = Memory::default();
        assert_eq!(
            relay(&stand, true, &with_root(CHAIN), &mut memory, Instant::now()).await,
            RootRelay::Nothing
        );
        assert_eq!(*stand.roots_simulated.lock().unwrap(), 0);
        stand.root_moves = true;
        assert_eq!(
            relay(&stand, true, &with_root(CHAIN), &mut memory, Instant::now()).await,
            RootRelay::Sent(Sent::Done)
        );
    }

    /// A root recorded from a seal whose checker is then retired is not live
    /// until it is recorded again through an allowed checker: the same seal
    /// is sent again, once while that checker stays retired, and once more
    /// only after the record has been seen through an allowed checker and
    /// that one is retired in turn.
    #[tokio::test]
    async fn the_root_s_seal_is_relayed_again_once_its_record_s_checker_is_retired() {
        let mut stand = stand();
        let published = with_root(CHAIN);
        let mut memory = Memory::default();
        let start = Instant::now();
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Sent(Sent::Done)
        );
        // Recorded through an allowed checker: nothing more to send, even
        // where a newer checker would let the record move.
        stand.root = (SIGNER, Address::repeat_byte(0xcc));
        stand.root_moves = true;
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Nothing
        );
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 1);

        // Its checker is retired: the seal is sent again, once.
        stand.root_checker_retired = true;
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Sent(Sent::Done)
        );
        for seconds in [15, 120, 900] {
            assert_eq!(
                relay(
                    &stand,
                    true,
                    &published,
                    &mut memory,
                    start + Duration::from_secs(seconds)
                )
                .await,
                RootRelay::Nothing
            );
        }
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 2);

        // Recorded again through an allowed checker, which is later retired
        // too: one more send.
        stand.root_checker_retired = false;
        relay(&stand, true, &published, &mut memory, start).await;
        stand.root_checker_retired = true;
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Sent(Sent::Done)
        );
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 3);

        // Cleared: the record is empty, and the seal is never sent again.
        stand.root = (Address::ZERO, Address::ZERO);
        assert_eq!(
            relay(&stand, true, &published, &mut memory, start).await,
            RootRelay::Nothing
        );
        assert_eq!(stand.roots_sent.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_executor_s_root_leaf_reads_as_published() {
        let json = r#"{
            "mode": "development", "nonce": "00", "seal": null, "journal": null,
            "leaves": [],
            "root": {"chain_id": 31337, "signer": "0x7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e",
                "due": true,
                "leaf": "0x4444444444444444444444444444444444444444444444444444444444444444",
                "proof": ["0x6161616161616161616161616161616161616161616161616161616161616161"]}
        }"#;
        let published: Published = serde_json::from_str(json).unwrap();
        let root = published.root.unwrap();
        assert_eq!((root.chain_id, root.signer), (31_337, SIGNER));
        assert_eq!(root.proof, [B256::repeat_byte(0x61)]);
    }
}
