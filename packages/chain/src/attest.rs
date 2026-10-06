//! Proving what a service is, and getting its key blessed.
//!
//! On a real deployment the enclave produces a proof no service could forge,
//! and the registry checks it against the platform's own trust root. On a
//! development chain a verifier contract trusts one account's signature over
//! the same journal instead. Everything downstream is the same either way:
//! which images may hold a key, how long an addition waits, what a key may do
//! and for how long. The trust root says which of the two is in use, and every
//! vault's page shows it.

use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, bail};

use crate::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use crate::client::{Client, KeyClient, NeverSent, Unanswered};
use crate::deployments::Deployments;

/// How far before the head's time an attestation is dated.
const ATTESTED_BEFORE_THE_HEAD: u64 = 60;

/// Who puts a blessing on the chain. Anyone may — the proof is what the
/// registry checks — but a key that holds its own account sends with it, so
/// two services never reach for the same account's next nonce at once.
#[derive(Clone)]
pub enum Sender {
    /// An account the development chain itself holds the key for.
    Unlocked(Address),
    /// A key made inside the service, which signs its own transactions.
    Own(Box<KeyClient>),
}

impl Sender {
    /// The same sender, a key's own noting each blessing it broadcasts in
    /// `unanswered` (see [`KeyClient::noting`]).
    fn noting(self, unanswered: &Unanswered) -> Sender {
        match self {
            Sender::Own(key) => Sender::Own(Box::new(key.noting(unanswered))),
            unlocked => unlocked,
        }
    }
}

/// What an instance attests to. It matches `AttestationRegistry.Journal`:
/// image, project, key, the root of its leaves, and when it attested.
type Journal = (B256, B256, Address, B256, u64);

/// What a service's image is. There is no enclave here to measure, so the
/// service's name stands for its digest and the registry treats it exactly as
/// it would a real one.
pub fn image_digest(service: &str) -> B256 {
    keccak256(format!("shonin-dev-image:{service}"))
}

pub fn project_id() -> B256 {
    keccak256("shonin-dev-project")
}

/// Bless a key that serves every strategy and no particular vault: a build
/// service, or a reviewer.
///
/// The key sends its own blessing. Anyone may send one — the proof is what
/// the registry checks — but having each key send its own keeps two services
/// from reaching for the same account's next nonce at the same moment.
pub async fn bless_service(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
) -> Result<KeyInfo> {
    bless(
        chain,
        deployments,
        service,
        key,
        Sender::Unlocked(key),
        Address::ZERO,
        B256::ZERO,
    )
    .await
}

/// Bless a key for one vault running one bundle, sent by `sender`.
pub async fn bless_for_vault(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
) -> Result<KeyInfo> {
    bless(chain, deployments, service, key, sender, vault, bundle).await
}

#[allow(clippy::too_many_arguments)]
async fn bless(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
) -> Result<KeyInfo> {
    let image = image_digest(service);
    // The vault is on this chain: a vault on another chain is served by its
    // station here, which is blessed against the vault chain's id.
    let vault_chain = vault_chain(deployments, vault);
    // The one leaf this instance holds a key for: a vault and its bundle, or
    // for a builder or a reviewer, no vault, on this chain, naming the key.
    // Here the instance's own key is the one blessed; an instance holding
    // several keys commits to a root over all their leaves (see
    // [`merkle_root`]) and proves each with [`merkle_proof`].
    let this_chain = U256::from(deployments.chain_id);
    let root = key_leaf(vault_chain, vault, bundle, this_chain, key);
    // A blessing runs a TTL from when the instance attested, by the chain's
    // own clock, dated a little back: a public RPC is often several nodes
    // behind one name, and the registry refuses an attestation from after
    // the block it runs in, which a node a block or two behind the one that
    // gave the time would be. The blessing is that much shorter, never
    // longer.
    // Nothing is sent until the registry is asked to bless: a failure before
    // that can't land.
    let issued_at = chain
        .block_timestamp()
        .await
        .context("reading the chain's clock")
        .map_err(NeverSent::wrap)?
        .saturating_sub(ATTESTED_BEFORE_THE_HEAD);
    let journal: Journal = (image, project_id(), key, root, issued_at);
    let encoded = journal.abi_encode_params();
    let seal = chain
        .sign(deployments.roles.attestor, keccak256(&encoded))
        .await
        .context("the attestation was not sealed")
        .map_err(NeverSent::wrap)?;

    // An image waits out its timelock before it can hold a key. A service
    // that starts the moment the chain does is simply early, so it waits
    // rather than gives up. The wait is measured by the clock: each look
    // can itself take a request's timeout.
    let started = tokio::time::Instant::now();
    let mut said_so = false;
    loop {
        let allowed = chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isImageAllowedCall { imageDigest: image },
            )
            .await
            .unwrap_or(false);

        if allowed {
            let call = IAttestationRegistry::blessCall {
                seal: seal.clone(),
                journal: encoded.clone().into(),
                vaultChain: vault_chain,
                vault,
                strategyHash: bundle,
                key,
                merkleProof: Vec::new(),
            };
            match &sender {
                Sender::Unlocked(from) => {
                    chain
                        .send(*from, deployments.attestation, call)
                        .await
                        .context("the registry would not bless the key")?;
                }
                // Looked at again once the key's turn to send has come: a
                // blessing of this key's sent before (by a serve since
                // stopped, say) may have landed while this one waited, and
                // a second would be paid for nothing.
                Sender::Own(own) => {
                    own.send_if(
                        async {
                            blessed_for_a_while(chain, deployments, service, key, vault, bundle)
                                .await
                                .map(|blessed| !blessed)
                        },
                        deployments.attestation,
                        call,
                    )
                    .await
                    .context("the registry would not bless the key")?;
                }
            }
            break;
        }

        if started.elapsed() > Duration::from_secs(300) {
            bail!("the {service} image is still not allowlisted");
        }
        if !said_so {
            tracing::info!(service, "waiting out the image's timelock");
            said_so = true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    chain
        .read(
            deployments.attestation,
            IAttestationRegistry::keyInfoCall { signer: key },
        )
        .await
        .context("reading back what was blessed")
}

/// The leaf an instance commits to for one key: the vault and bundle the key
/// acts for (zero for a builder's or a reviewer's), the chain the blessing is
/// sent on, and the key. It matches `AttestationRegistry.bless`:
/// `keccak256(bytes.concat(keccak256(abi.encode(vaultChain, vault,
/// strategyHash, block.chainid, key))))`.
pub fn key_leaf(
    vault_chain: U256,
    vault: Address,
    strategy_hash: B256,
    this_chain: U256,
    key: Address,
) -> B256 {
    keccak256(keccak256(
        (vault_chain, vault, strategy_hash, this_chain, key).abi_encode(),
    ))
}

/// One level up the tree: the pair hashed in sorted order, as OpenZeppelin's
/// `MerkleProof` does.
fn hash_pair(a: B256, b: B256) -> B256 {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let mut both = [0u8; 64];
    both[..32].copy_from_slice(low.as_slice());
    both[32..].copy_from_slice(high.as_slice());
    keccak256(both)
}

/// Each level of the tree over `leaves`, the leaves first and the root last.
/// Leaves are paired in the order given; a node left without a partner is
/// carried up as it is.
fn levels(leaves: &[B256]) -> Vec<Vec<B256>> {
    let mut levels = vec![leaves.to_vec()];
    while levels.last().is_some_and(|level| level.len() > 1) {
        let below = levels.last().expect("a level");
        let above = below
            .chunks(2)
            .map(|pair| match pair {
                [a, b] => hash_pair(*a, *b),
                [lone] => *lone,
                _ => unreachable!("chunks of two"),
            })
            .collect();
        levels.push(above);
    }
    levels
}

/// The root over `leaves`, as the registry checks it: zero for none, the leaf
/// itself for one.
pub fn merkle_root(leaves: &[B256]) -> B256 {
    levels(leaves)
        .last()
        .and_then(|top| top.first().copied())
        .unwrap_or(B256::ZERO)
}

/// The proof that `leaves[index]` is under [`merkle_root`]`(leaves)`, as
/// OpenZeppelin's `MerkleProof.verify` takes it: the sibling at each level,
/// bottom up, a level where the node has no sibling contributing nothing.
/// Empty for one leaf.
///
/// # Panics
///
/// If `index` is not a leaf's.
pub fn merkle_proof(leaves: &[B256], index: usize) -> Vec<B256> {
    assert!(
        index < leaves.len(),
        "no leaf {index} among {}",
        leaves.len()
    );
    let mut proof = Vec::new();
    let mut at = index;
    for level in levels(leaves).iter().take_while(|level| level.len() > 1) {
        if let Some(sibling) = level.get(at ^ 1) {
            proof.push(*sibling);
        }
        at /= 2;
    }
    proof
}

/// Which chain the vault a key is blessed for is on: none for a key that
/// serves no particular vault.
fn vault_chain(deployments: &Deployments, vault: Address) -> U256 {
    if vault == Address::ZERO {
        U256::ZERO
    } else {
        U256::from(deployments.chain_id)
    }
}

/// Whether the registry counts `key` as blessed for this vault and bundle
/// (or, with no vault, as this service's kind) right now. The key's record
/// alone can't say: a blessing also ends when the checker that gave it is
/// retired, until an allowed checker vouches for the key again.
async fn blessed_now(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<bool> {
    if vault == Address::ZERO {
        chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isBlessedAsCall {
                    signer: key,
                    kind: kind_of(service),
                },
            )
            .await
    } else {
        chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer: key,
                    vaultChain: vault_chain(deployments, vault),
                    vault,
                    strategyHash: bundle,
                },
            )
            .await
    }
}

/// Whether `key` is blessed for this vault and bundle with more than half
/// its blessing left by the chain's clock, so blessing it again now would
/// pay for nothing.
async fn blessed_for_a_while(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<bool> {
    let info = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::keyInfoCall { signer: key },
        )
        .await?;
    if info.vaultChain != vault_chain(deployments, vault)
        || info.vault != vault
        || info.strategyHash != bundle
    {
        return Ok(false);
    }
    if !blessed_now(chain, deployments, service, key, vault, bundle).await? {
        return Ok(false);
    }
    let ttl = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessingTtlCall {},
        )
        .await?;
    let now = chain.block_timestamp().await?;
    Ok(info.blessedUntil.saturating_sub(now).saturating_mul(2) > ttl)
}

/// Keep a key blessed for as long as this service runs.
///
/// A blessing expires, and an expired key is exactly as useless as a revoked
/// one — which is the point: an instance that stops attesting stops being
/// able to act. So an instance that is still running attests again well
/// before its blessing runs out.
///
/// `on_renewal` runs whenever the key's blessing is seen to have moved, not
/// only when a renewal reports success: a renewal can land on the chain and
/// still fail afterwards (its receipt or the read-back lost), and whatever
/// follows the blessing has to follow it all the same.
///
/// A renewal that has been broadcast may land, for as long as the chain
/// client watches for its receipt. While it may, no other is sent: the
/// blessing's end is read again at every look, and a renewal that lands is
/// seen there. Renewals start with a third of the blessing left, which is far
/// longer than that wait.
#[allow(clippy::too_many_arguments)]
pub fn keep_blessed(
    chain: Client,
    deployments: Deployments,
    service: &'static str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
    on_renewal: impl FnMut() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    keep_blessed_sharing(
        chain,
        deployments,
        service,
        key,
        sender,
        vault,
        bundle,
        Unanswered::default(),
        on_renewal,
    )
}

/// [`keep_blessed`], holding its renewals back while any blessing of the
/// key's noted in `unanswered` may still land, and noting there each of its
/// own: one sent by an earlier keeper of the same key (a serve started
/// again) holds this one back as its own would. A key's own renewal is noted
/// by its send the moment it is broadcast, so a keeper stopped halfway
/// through the send still leaves it noted; a renewal sent by an unlocked
/// account is noted when it is seen to go unanswered.
#[allow(clippy::too_many_arguments)]
pub fn keep_blessed_sharing(
    chain: Client,
    deployments: Deployments,
    service: &'static str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
    unanswered: Unanswered,
    mut on_renewal: impl FnMut() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let sender = sender.noting(&unanswered);
    tokio::spawn(async move {
        let mut unread = false;
        // The blessing's end as last seen, so a move is noticed however it
        // came about.
        let mut seen_until: Option<u64> = None;
        loop {
            // A read that failed says nothing about the blessing, so it is
            // looked at again shortly rather than renewed on a guess: a
            // renewal on every failed read would pay for blessings that
            // still have days to run.
            let due = match renewal_due(&chain, &deployments, service, key, vault, bundle).await {
                Ok((due, until)) => {
                    unread = false;
                    if seen_until.is_some_and(|seen| seen != until) {
                        unanswered.settled();
                        on_renewal();
                    }
                    seen_until = Some(until);
                    due
                }
                Err(error) => {
                    if !unread {
                        tracing::warn!(
                            service,
                            error = format!("{error:#}"),
                            "could not tell how long a blessing has left; looking again shortly"
                        );
                    }
                    unread = true;
                    false
                }
            };

            if due && !unanswered.may_still_land(key) {
                match bless(
                    &chain,
                    &deployments,
                    service,
                    key,
                    sender.clone(),
                    vault,
                    bundle,
                )
                .await
                {
                    Ok(info) => {
                        unanswered.settled();
                        on_renewal();
                        seen_until = Some(info.blessedUntil);
                    }
                    // A key that stops being blessed stops being able to act,
                    // so this is never a quiet failure. A renewal that landed
                    // regardless is noticed at the next look.
                    Err(error) => {
                        unanswered.failed(key, &error);
                        tracing::warn!(service, %error, "could not renew a blessing");
                    }
                }
            }

            // The chain's clock runs at its own speed, so wait in its terms.
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

/// Whether a key's blessing is down to a third of its length, so it is
/// renewed with time to spare for a slow chain or a busy moment, and when it
/// ends. A key the registry no longer counts as blessed (its checker
/// retired, say) is due at once, however long its record says is left.
async fn renewal_due(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<(bool, u64)> {
    let ttl = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessingTtlCall {},
        )
        .await?;
    let info = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::keyInfoCall { signer: key },
        )
        .await?;
    let blessed = blessed_now(chain, deployments, service, key, vault, bundle).await?;
    let now = chain.block_timestamp().await?;
    let due = !blessed || info.blessedUntil.saturating_sub(now).saturating_mul(3) <= ttl;
    Ok((due, info.blessedUntil))
}

/// Which kind of image a service runs as, for the pages that say so.
pub fn kind_of(service: &str) -> ImageKind {
    match service {
        "builder" => ImageKind::Builder,
        "reviewer" => ImageKind::Reviewer,
        "unwinder" => ImageKind::Unwinder,
        _ => ImageKind::Executor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::NeverLanded;

    /// OpenZeppelin's `MerkleProof.processProof` and `verify`, word for word.
    fn oz_verify(proof: &[B256], root: B256, leaf: B256) -> bool {
        proof
            .iter()
            .fold(leaf, |computed, sibling| hash_pair(computed, *sibling))
            == root
    }

    fn leaves(count: usize) -> Vec<B256> {
        (0..count)
            .map(|i| {
                key_leaf(
                    U256::from(46_630),
                    Address::repeat_byte(i as u8 + 1),
                    B256::repeat_byte(0x5b),
                    U256::from(46_630),
                    Address::repeat_byte(0xa0 + i as u8),
                )
            })
            .collect()
    }

    #[test]
    fn every_leaf_s_proof_verifies_under_the_root_by_openzeppelin_s_rule() {
        for count in 1..=9 {
            let leaves = leaves(count);
            let root = merkle_root(&leaves);
            for (index, leaf) in leaves.iter().enumerate() {
                let proof = merkle_proof(&leaves, index);
                assert!(oz_verify(&proof, root, *leaf), "leaf {index} of {count}");
                // And no other leaf passes with this leaf's proof.
                for (other, wrong) in leaves.iter().enumerate() {
                    if other != index && count > 1 {
                        assert!(
                            !oz_verify(&proof, root, *wrong),
                            "leaf {other} with {index}'s proof"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn one_leaf_is_its_own_root_and_none_is_zero() {
        let one = leaves(1);
        assert_eq!(merkle_root(&one), one[0]);
        assert!(merkle_proof(&one, 0).is_empty());
        assert_eq!(merkle_root(&[]), B256::ZERO);
    }

    /// The vector `KeysInTheLeaves.t.sol` checks on the contract side: the
    /// registry's own leaf and OpenZeppelin's `verify`, over three leaves.
    #[test]
    fn the_leaf_and_the_proof_match_the_contract_s_vector() {
        let key = |byte| Address::repeat_byte(byte);
        let leaves = [
            key_leaf(
                U256::from(31_337),
                key(0x0a),
                B256::repeat_byte(0x5b),
                U256::from(31_337),
                key(0xe1),
            ),
            key_leaf(
                U256::from(31_337),
                key(0x0b),
                B256::repeat_byte(0x5b),
                U256::from(31_337),
                key(0xe2),
            ),
            key_leaf(
                U256::ZERO,
                Address::ZERO,
                B256::ZERO,
                U256::from(31_337),
                key(0xe3),
            ),
        ];
        assert_eq!(
            leaves[0],
            "0x13bff5568b694d0a5bcff54736167b4950f3e40d1e91348b917fe483c6af5fb3"
                .parse::<B256>()
                .unwrap()
        );
        let root = merkle_root(&leaves);
        assert_eq!(
            root,
            "0x84ab51bbb5eef0d7b04749b37284b518840833bdee69bad44be90f861b3c40cd"
                .parse::<B256>()
                .unwrap()
        );
        assert_eq!(
            merkle_proof(&leaves, 2),
            vec![hash_pair(leaves[0], leaves[1])]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_renewal_is_not_sent_again_while_an_unanswered_one_may_still_land() {
        let key = Address::repeat_byte(1);
        let unanswered = Unanswered::default();
        // Nothing went unanswered: renewed when due.
        assert!(!unanswered.may_still_land(key));
        // One that was never sent can't land.
        unanswered.failed(
            key,
            &NeverSent::wrap(anyhow::Error::new(NeverLanded)).context("bless"),
        );
        assert!(!unanswered.may_still_land(key));
        // One went unanswered a minute ago: it may still land, for this key.
        unanswered.failed(key, &anyhow::Error::new(NeverLanded).context("bless"));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(unanswered.may_still_land(key));
        assert!(!unanswered.may_still_land(Address::repeat_byte(2)));
        // Once the client has stopped watching for it, it won't.
        tokio::time::advance(crate::client::LANDING_TIMEOUT).await;
        assert!(!unanswered.may_still_land(key));
    }
}
