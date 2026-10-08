//! Blessings and lamports for the executor's keys on a Solana cluster.
//!
//! The executor publishes on its status port (`GET /attestation`), beside
//! its EVM leaves, each Solana cluster's leaves, with the journal and seal
//! the Solana registry takes: the prover's proof read for the Solana checker,
//! or, on a development cluster, the development checker's signature. The
//! keeper relays them as it does on an EVM chain: `bless` for every leaf
//! whose key is not blessed for a while yet, or whose blessing the registry
//! no longer counts although it runs on; simulated first, sent only if the
//! simulation goes through, and never again while one sent may still land.
//!
//! Once the registry counts a key as the executor of a vault that is still
//! live, for the bundle that vault runs, the keeper keeps the key in
//! lamports, up to a fixed stake: the key pays for its approval at the
//! station and for the agent's own sends. A key the cluster doesn't vouch
//! for is never given anything, whatever the executor says about it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chain::solana::attest::{KeyLeaf, SolanaSeal};
use chain::solana::executor::{self as on_chain, Blessed};
use chain::solana::programs::registry::state::Journal;
use chain::solana::programs::vault::state::VaultState;
use chain::solana::{Keypair, MayStillLand, Platform, Pubkey, Signer, SolanaClient};
use serde::Deserialize;

use crate::attest::Sent;

/// What the executor publishes about its Solana keys, as far as the keeper
/// reads it.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Published {
    #[serde(default)]
    pub solana: Vec<Cluster>,
}

/// One cluster's keys, and the journal and seal its registry takes.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Cluster {
    /// The cluster's chain key (`solana:devnet`).
    pub cluster: String,
    #[serde(default)]
    pub journal: Option<String>,
    #[serde(default)]
    pub seal: Option<SolanaSeal>,
    #[serde(default)]
    pub leaves: Vec<Leaf>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct Leaf {
    pub vault: String,
    pub strategy_hash: String,
    pub key: String,
    #[serde(default)]
    pub proof: Vec<String>,
}

/// A leaf as the registry takes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Read {
    pub vault: Pubkey,
    pub bundle: [u8; 32],
    pub key: Pubkey,
    pub proof: Vec<[u8; 32]>,
}

fn word(text: &str) -> Result<[u8; 32]> {
    alloy::hex::decode(text)
        .context("not hex")?
        .try_into()
        .map_err(|_| anyhow!("not 32 bytes"))
}

impl Leaf {
    fn read(&self) -> Result<Read> {
        Ok(Read {
            vault: self
                .vault
                .parse()
                .map_err(|_| anyhow!("the vault is not a key"))?,
            bundle: word(&self.strategy_hash).context("the bundle")?,
            key: self
                .key
                .parse()
                .map_err(|_| anyhow!("the key is not a key"))?,
            proof: self
                .proof
                .iter()
                .map(|hash| word(hash))
                .collect::<Result<_>>()
                .context("the proof")?,
        })
    }
}

/// The issue time a Solana journal states: its last eight bytes,
/// little-endian.
fn issued_at(journal: &Journal) -> u64 {
    journal.issued_at
}

fn journal_of(hex: &str) -> Result<Journal> {
    let bytes = alloy::hex::decode(hex).context("the journal is not hex")?;
    anyhow::ensure!(bytes.len() == 136, "the journal is not the registry's");
    <Journal as chain::solana::programs::anchor_lang::AnchorDeserialize>::deserialize(
        &mut bytes.as_slice(),
    )
    .context("the journal is not the registry's")
}

/// The cluster as the relay and the stocking see it. The keeper's is
/// [`OnCluster`]; the tests' is a stand-in.
pub(crate) trait Registry {
    /// The cluster's time.
    async fn now(&self) -> Result<u64>;
    /// How long a blessing lasts from the attestation it came from.
    async fn ttl(&self) -> Result<u64>;
    /// Where the leaf's key's blessing for its vault and bundle stands.
    async fn blessed(&self, leaf: &Read) -> Result<Blessed>;
    /// Whether the cluster vouches for the key as the executor of a live
    /// vault running the leaf's bundle.
    async fn vouches_for(&self, leaf: &Read) -> Result<bool>;
    async fn simulate_bless(&self, journal: &Journal, seal: &SolanaSeal, leaf: &Read)
    -> Result<()>;
    async fn send_bless(&self, journal: &Journal, seal: &SolanaSeal, leaf: &Read) -> Sent;
    /// Bring the key's lamports back up to its stake if it has run below
    /// half of it. Says whether it holds them now.
    async fn stock(&self, key: Pubkey, stakes: &mut Stakes) -> Result<bool>;
}

/// The stakes sent to each key and not yet seen to land, by when they were
/// sent.
pub(crate) type Stakes = HashMap<Pubkey, Instant>;

/// How long a `bless` that went through holds the next one for the same
/// leaf back: long enough for any node the keeper asks to have its slot.
const DONE_HOLD: Duration = Duration::from_secs(90);

/// How long a `bless`, or a stake, that may still land holds the next one
/// back: as long as a send is watched for.
const LANDING_HOLD: Duration = Duration::from_secs(600);

/// What a vault key is given, on a development cluster and elsewhere: the
/// agent's sends for days, and its approval's rent. Topped back up once it
/// holds less than half.
const STAKE_DEVELOPMENT: u64 = 2_000_000_000;
const STAKE_PUBLIC: u64 = 50_000_000;

/// The stake on a cluster.
pub(crate) fn stake_on(cluster: &str) -> u64 {
    if cluster == "localnet" {
        STAKE_DEVELOPMENT
    } else {
        STAKE_PUBLIC
    }
}

/// What a key holding `holds` is given to bring it to `stake`: nothing
/// while it holds half or more.
pub(crate) fn top_up(holds: u64, stake: u64) -> u64 {
    if holds >= stake / 2 { 0 } else { stake - holds }
}

/// What the keeper remembers of one cluster's blessings and keys between
/// rounds.
#[derive(Default)]
pub(crate) struct Memory {
    held: HashMap<Read, Instant>,
    /// Each leaf whose blessing the registry stopped counting although it
    /// runs on, and the issue time of the seal sent again for it.
    resent: HashMap<Read, u64>,
    pub stakes: Stakes,
    last: Option<Instant>,
}

impl Memory {
    pub fn due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|last| now.duration_since(last) >= crate::attest::EVERY)
    }

    pub fn looked(&mut self, at: Instant) {
        self.last = Some(at);
    }
}

/// What one round did, for the log and the tests.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Round {
    pub sent: Vec<Pubkey>,
    pub not_simulated: Vec<Pubkey>,
    pub stocked: Vec<Pubkey>,
}

/// One look at a cluster: relay what needs blessing, stock what is blessed.
pub(crate) async fn round(
    registry: &impl Registry,
    cluster: &str,
    published: &Published,
    memory: &mut Memory,
    at: Instant,
) -> Result<Round> {
    memory.looked(at);
    let mut done = Round::default();
    let Some(this) = published.solana.iter().find(|each| each.cluster == cluster) else {
        memory.held.clear();
        memory.resent.clear();
        return Ok(done);
    };
    let leaves: Vec<Read> = this
        .leaves
        .iter()
        .filter_map(|leaf| {
            leaf.read()
                .inspect_err(|error| {
                    tracing::debug!(
                        error = format!("{error:#}"),
                        "a published leaf that can't be read"
                    )
                })
                .ok()
        })
        .collect();
    let published_leaves: HashSet<&Read> = leaves.iter().collect();
    memory
        .held
        .retain(|leaf, until| published_leaves.contains(leaf) && at < *until);
    memory
        .resent
        .retain(|leaf, _| published_leaves.contains(leaf));
    if leaves.is_empty() {
        return Ok(done);
    }
    let sealed = match (&this.journal, &this.seal) {
        (Some(journal), Some(seal)) => Some((journal_of(journal)?, seal.clone())),
        _ => None,
    };

    let now = registry.now().await?;
    let ttl = registry.ttl().await?;
    for leaf in &leaves {
        let Some((journal, seal)) = &sealed else {
            break;
        };
        let issued = issued_at(journal);
        let blessed = registry.blessed(leaf).await?;
        // Down to a third of its blessing, and the seal adds something.
        let renews = blessed.until < now.saturating_add(ttl / 3)
            && issued.saturating_add(ttl) > blessed.until;
        // Or the blessing runs on but the registry no longer counts it (the
        // checker it was recorded against retired): sending the seal again
        // has the newest checker that accepts it record it afresh. Once per
        // seal until the key counts again.
        let mut recounts = false;
        if !renews && blessed.until >= now {
            if blessed.executor && blessed.counts {
                memory.resent.remove(leaf);
            } else {
                recounts = memory.resent.get(leaf) != Some(&issued);
            }
        }
        if !renews && !recounts {
            continue;
        }
        if memory.held.contains_key(leaf) {
            continue;
        }
        if let Err(error) = registry.simulate_bless(journal, seal, leaf).await {
            tracing::debug!(vault = %leaf.vault, key = %leaf.key, error = format!("{error:#}"), "a blessing would not go through; not sent");
            done.not_simulated.push(leaf.key);
            continue;
        }
        let sent = registry.send_bless(journal, seal, leaf).await;
        let hold = match sent {
            Sent::Done => Some(DONE_HOLD),
            Sent::MayStillLand => Some(LANDING_HOLD),
            Sent::Failed => None,
        };
        if let Some(hold) = hold {
            memory.held.insert(leaf.clone(), at + hold);
        }
        if sent == Sent::Done {
            if recounts {
                memory.resent.insert(leaf.clone(), issued);
            }
            tracing::info!(cluster, vault = %leaf.vault, key = %leaf.key, "a vault key's blessing was relayed");
        }
        done.sent.push(leaf.key);
    }

    // Each key once a round, and only once the cluster vouches for it.
    let mut looked = HashSet::new();
    for leaf in &leaves {
        if !looked.insert(leaf.key) || !registry.vouches_for(leaf).await? {
            continue;
        }
        registry
            .stock(leaf.key, &mut memory.stakes)
            .await
            .with_context(|| format!("stocking {} with lamports", leaf.key))?;
        done.stocked.push(leaf.key);
    }
    Ok(done)
}

/// Ask the executor what it publishes about its Solana keys.
pub(crate) async fn published(http: &reqwest::Client, source: &str) -> Result<Published> {
    let reply = http
        .get(format!("{}/attestation", source.trim_end_matches('/')))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| format!("asking {source} for its attestation"))?;
    anyhow::ensure!(
        reply.status().is_success(),
        "{source} answered {} for its attestation",
        reply.status()
    );
    reply
        .json()
        .await
        .with_context(|| format!("reading {source}'s attestation"))
}

// The cluster itself ---------------------------------------------------------

pub(crate) struct OnCluster<'a> {
    pub client: &'a SolanaClient,
    pub platform: &'a Platform,
    pub keeper: &'a Keypair,
}

impl OnCluster<'_> {
    fn key_leaf(&self, leaf: &Read) -> KeyLeaf {
        KeyLeaf {
            vault_chain: self.platform.cluster,
            vault: leaf.vault.to_bytes(),
            strategy_hash: leaf.bundle,
            key: leaf.key,
            proof: leaf.proof.clone(),
        }
    }
}

impl Registry for OnCluster<'_> {
    async fn now(&self) -> Result<u64> {
        self.client.now().await
    }

    async fn ttl(&self) -> Result<u64> {
        Ok(self
            .platform
            .registry_config(self.client)
            .await?
            .blessing_ttl)
    }

    async fn blessed(&self, leaf: &Read) -> Result<Blessed> {
        Blessed::read(
            self.client,
            self.platform,
            &leaf.key,
            &leaf.vault,
            &leaf.bundle,
        )
        .await
    }

    async fn vouches_for(&self, leaf: &Read) -> Result<bool> {
        let vault = match self.platform.vault(self.client, &leaf.vault).await {
            Ok(vault) => vault,
            Err(_) => return Ok(false),
        };
        let live = !matches!(vault.state, VaultState::Closed | VaultState::None);
        if !live || vault.bundle_hash != leaf.bundle {
            return Ok(false);
        }
        let blessed = self.blessed(leaf).await?;
        Ok(blessed.holds(self.client.now().await?))
    }

    async fn simulate_bless(
        &self,
        journal: &Journal,
        seal: &SolanaSeal,
        leaf: &Read,
    ) -> Result<()> {
        self.platform
            .simulate_bless(
                self.client,
                &self.keeper.pubkey(),
                journal,
                seal,
                &self.key_leaf(leaf),
            )
            .await
    }

    async fn send_bless(&self, journal: &Journal, seal: &SolanaSeal, leaf: &Read) -> Sent {
        let sent = async {
            let config = self.platform.registry_config(self.client).await?;
            let now = self.client.now().await?;
            let ixs = self.platform.bless_with(
                &config,
                now,
                &self.keeper.pubkey(),
                journal,
                seal,
                &self.key_leaf(leaf),
            )?;
            self.client.send(ixs, &[self.keeper]).await
        }
        .await;
        landed(sent)
    }

    async fn stock(&self, key: Pubkey, stakes: &mut Stakes) -> Result<bool> {
        let now = Instant::now();
        stakes.retain(|_, sent| now.duration_since(*sent) < LANDING_HOLD);
        let stake = stake_on(&self.client.cluster);
        let holds = self.client.lamports(&key).await?;
        let give = top_up(holds, stake);
        if give == 0 {
            stakes.remove(&key);
            return Ok(true);
        }
        if stakes.contains_key(&key) {
            return Ok(false);
        }
        let sent = self
            .client
            .send(
                vec![on_chain::ix_fund(&self.keeper.pubkey(), &key, give)],
                &[self.keeper],
            )
            .await;
        match landed(sent) {
            Sent::Done => {
                tracing::info!(%key, lamports = give, "a vault key was given its lamports");
                Ok(true)
            }
            Sent::MayStillLand => {
                stakes.insert(key, now);
                Ok(false)
            }
            Sent::Failed => Err(anyhow!("giving {key} its lamports failed")),
        }
    }
}

fn landed(sent: Result<chain::solana::Landed>) -> Sent {
    match sent {
        Ok(_) => Sent::Done,
        Err(error) if error.downcast_ref::<MayStillLand>().is_some() => Sent::MayStillLand,
        Err(_) => Sent::Failed,
    }
}

/// Relay the executor's blessings on one cluster and keep its blessed keys
/// in lamports, for as long as the keeper runs.
pub(crate) async fn keep_keys(
    client: SolanaClient,
    platform: Platform,
    cluster: String,
    keeper: Keypair,
    executor: String,
) {
    let http = reqwest::Client::new();
    let mut memory = Memory::default();
    let mut said = crate::Said::default();
    loop {
        let at = Instant::now();
        if memory.due(at) {
            let looked = match published(&http, &executor).await {
                Ok(published) => {
                    let registry = OnCluster {
                        client: &client,
                        platform: &platform,
                        keeper: &keeper,
                    };
                    round(&registry, &cluster, &published, &mut memory, at)
                        .await
                        .map(|_| ())
                }
                Err(error) => {
                    memory.looked(at);
                    Err(error)
                }
            };
            match looked {
                Ok(()) => {
                    if said.over() {
                        tracing::info!(%cluster, "the executor's Solana keys are looked after again");
                    }
                }
                Err(error) => {
                    if let Some(times) = said.due(Instant::now()) {
                        tracing::warn!(%cluster, times, error = format!("{error:#}"), "could not look after the executor's Solana keys");
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const NOW: u64 = 1_800_000_000;
    const TTL: u64 = 72 * 3600;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    fn leaf(n: u8) -> Leaf {
        Leaf {
            vault: key(100 + n).to_string(),
            strategy_hash: alloy::hex::encode_prefixed([7u8; 32]),
            key: key(n).to_string(),
            proof: vec![],
        }
    }

    fn journal(issued_at: u64) -> String {
        let journal = Journal {
            image_digest: [1; 32],
            project_id: [2; 32],
            signer: key(200),
            vaults_root: [3; 32],
            issued_at,
        };
        alloy::hex::encode_prefixed(
            chain::solana::programs::anchor_lang::AnchorSerialize::try_to_vec(&journal).unwrap(),
        )
    }

    fn published(leaves: Vec<Leaf>, issued_at: Option<u64>) -> Published {
        Published {
            solana: vec![Cluster {
                cluster: "solana:devnet".into(),
                journal: issued_at.map(journal),
                seal: issued_at.map(|_| SolanaSeal::Proof {
                    seal: "0x00".into(),
                }),
                leaves,
            }],
        }
    }

    #[derive(Default)]
    struct Stand {
        blessings: Mutex<HashMap<Pubkey, Blessed>>,
        vouched: Mutex<HashSet<Pubkey>>,
        simulates: Mutex<bool>,
        sent: Mutex<Vec<Pubkey>>,
        outcome: Mutex<Option<Sent>>,
        given: Mutex<Vec<Pubkey>>,
    }

    impl Stand {
        fn new() -> Stand {
            let stand = Stand::default();
            *stand.simulates.lock().unwrap() = true;
            stand
        }
    }

    impl Registry for Stand {
        async fn now(&self) -> Result<u64> {
            Ok(NOW)
        }
        async fn ttl(&self) -> Result<u64> {
            Ok(TTL)
        }
        async fn blessed(&self, leaf: &Read) -> Result<Blessed> {
            Ok(self
                .blessings
                .lock()
                .unwrap()
                .get(&leaf.key)
                .copied()
                .unwrap_or(Blessed {
                    ttl: TTL,
                    ..Default::default()
                }))
        }
        async fn vouches_for(&self, leaf: &Read) -> Result<bool> {
            Ok(self.vouched.lock().unwrap().contains(&leaf.key))
        }
        async fn simulate_bless(&self, _: &Journal, _: &SolanaSeal, _: &Read) -> Result<()> {
            if *self.simulates.lock().unwrap() {
                Ok(())
            } else {
                Err(anyhow!("the registry refuses it"))
            }
        }
        async fn send_bless(&self, _: &Journal, _: &SolanaSeal, leaf: &Read) -> Sent {
            self.sent.lock().unwrap().push(leaf.key);
            self.outcome.lock().unwrap().unwrap_or(Sent::Done)
        }
        async fn stock(&self, key: Pubkey, _: &mut Stakes) -> Result<bool> {
            self.given.lock().unwrap().push(key);
            Ok(true)
        }
    }

    async fn one_round(stand: &Stand, published: &Published, memory: &mut Memory) -> Round {
        round(stand, "solana:devnet", published, memory, Instant::now())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_key_the_cluster_does_not_vouch_for_is_never_given_lamports() {
        let stand = Stand::new();
        let mut memory = Memory::default();
        let done = one_round(
            &stand,
            &published(vec![leaf(1), leaf(2)], Some(NOW)),
            &mut memory,
        )
        .await;
        assert_eq!(done.sent, vec![key(1), key(2)]);
        assert!(done.stocked.is_empty());
        assert!(stand.given.lock().unwrap().is_empty());
        stand.vouched.lock().unwrap().insert(key(2));
        let done = one_round(
            &stand,
            &published(vec![leaf(1), leaf(2)], Some(NOW)),
            &mut memory,
        )
        .await;
        assert_eq!(done.stocked, vec![key(2)]);
        assert_eq!(*stand.given.lock().unwrap(), vec![key(2)]);
    }

    #[tokio::test]
    async fn a_blessing_that_fails_simulation_is_not_sent() {
        let stand = Stand::new();
        *stand.simulates.lock().unwrap() = false;
        let mut memory = Memory::default();
        let done = one_round(&stand, &published(vec![leaf(1)], Some(NOW)), &mut memory).await;
        assert!(done.sent.is_empty());
        assert_eq!(done.not_simulated, vec![key(1)]);
        assert!(stand.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_blessing_is_not_sent_twice_while_one_may_still_land() {
        let stand = Stand::new();
        *stand.outcome.lock().unwrap() = Some(Sent::MayStillLand);
        let mut memory = Memory::default();
        let published = published(vec![leaf(1)], Some(NOW));
        one_round(&stand, &published, &mut memory).await;
        one_round(&stand, &published, &mut memory).await;
        assert_eq!(*stand.sent.lock().unwrap(), vec![key(1)]);
    }

    #[tokio::test]
    async fn a_key_blessed_for_a_while_yet_or_with_no_seal_is_not_relayed() {
        let stand = Stand::new();
        stand.blessings.lock().unwrap().insert(
            key(1),
            Blessed {
                until: NOW + TTL / 2,
                executor: true,
                counts: true,
                ttl: TTL,
            },
        );
        let mut memory = Memory::default();
        one_round(&stand, &published(vec![leaf(1)], Some(NOW)), &mut memory).await;
        one_round(&stand, &published(vec![leaf(2)], None), &mut memory).await;
        assert!(stand.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_key_the_registry_no_longer_counts_is_relayed_again_once_per_seal() {
        let stand = Stand::new();
        stand.blessings.lock().unwrap().insert(
            key(1),
            Blessed {
                until: NOW + TTL / 2,
                executor: true,
                counts: false,
                ttl: TTL,
            },
        );
        let mut memory = Memory::default();
        let published = published(vec![leaf(1)], Some(NOW - 10));
        one_round(&stand, &published, &mut memory).await;
        memory.held.clear();
        one_round(&stand, &published, &mut memory).await;
        assert_eq!(*stand.sent.lock().unwrap(), vec![key(1)]);
    }

    #[test]
    fn a_key_is_topped_up_to_its_stake_only_below_half() {
        let stake = stake_on("devnet");
        assert_eq!(stake, STAKE_PUBLIC);
        assert_eq!(top_up(stake, stake), 0);
        assert_eq!(top_up(stake / 2, stake), 0);
        assert_eq!(top_up(stake / 2 - 1, stake), stake / 2 + 1);
        assert_eq!(top_up(0, stake), stake);
        assert_eq!(stake_on("localnet"), STAKE_DEVELOPMENT);
    }
}
