//! Blessings and gas for the executor's keys.
//!
//! The executor publishes, on its status port (`GET /attestation`), the
//! leaves it holds keys for and, once one is made, the seal that proves them
//! all. Anyone may send the registry's `bless` with them; the keeper does,
//! in its turn on each chain, for every leaf of that chain whose key is not
//! blessed for a while yet. A `bless` is simulated first and sent only if the
//! simulation goes through, and never again while one sent may still land.
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
    /// Whether the chain vouches for the key as the executor of a live
    /// vault running the leaf's bundle.
    async fn vouches_for(&self, leaf: &Leaf) -> Result<bool>;
    /// Whether `bless` with this seal would go through now (a read: it costs
    /// nothing).
    async fn simulate_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Result<()>;
    async fn send_bless(&self, seal: &Bytes, journal: &Bytes, leaf: &Leaf) -> Sent;
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
    pub stakes: Stakes,
    /// When the chain was last looked at.
    last: Option<Instant>,
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
    if leaves.is_empty() {
        return Ok(done);
    }

    let now = registry.now().await?;
    let ttl = registry.ttl().await?;
    for leaf in &leaves {
        let until = registry.blessed_until(leaf).await?;
        // Blessed for a while yet: nothing to send.
        if until >= now.saturating_add(ttl / 3) {
            continue;
        }
        let (Some(seal), Some(journal)) = (&published.seal, &published.journal) else {
            continue;
        };
        // This seal's blessing is already what the key holds: sending it
        // again changes nothing. A newer seal is waited for.
        if issued_at(journal).is_some_and(|issued| issued.saturating_add(ttl) <= until) {
            continue;
        }
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

    async fn vouches_for(&self, leaf: &Leaf) -> Result<bool> {
        let attestation = self.deployments.attestation;
        let blessed = self
            .client
            .read(
                attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer: leaf.key,
                    vaultChain: U256::from(leaf.vault_chain),
                    vault: leaf.vault,
                    strategyHash: leaf.strategy_hash,
                },
            )
            .await?;
        if !blessed {
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

/// Below what a key is topped up: the stake less one worst-case send, never
/// below two worst-case sends; half a stake when the fee couldn't be read.
fn top_up_below(stake: u128, worst_send: Option<u128>) -> u128 {
    match worst_send {
        Some(send) => stake.saturating_sub(send).max(send.saturating_mul(2)),
        None => stake / 2,
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
/// less than a stake less one worst-case send, send it what brings it back
/// up to one. A stake sent and not yet seen to land is waited for, not sent
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
        Ok(_) => Ok(true),
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
        simulation_fails: bool,
        sends_answer: Option<Sent>,
        sent: Mutex<Vec<Address>>,
        stocked: Mutex<Vec<Address>>,
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

    #[test]
    fn a_key_is_topped_up_below_its_stake_less_a_send_and_never_below_two_sends() {
        assert_eq!(stake_at(500, 100), 500);
        assert_eq!(stake_at(500, 200), 600);
        assert_eq!(top_up_below(600, Some(200)), 400);
        assert_eq!(top_up_below(500, Some(400)), 800);
        assert_eq!(top_up_below(500, None), 250);
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
}
