//! The build service on a Solana cluster.
//!
//! Its key there is its Solana key (see [`crate::on_solana`]), blessed and
//! stocked before anything on the cluster is taken. Payments are the
//! publishing program's submissions: each takes the next strategy id and
//! leaves a record of its own at an address made from that id, naming the
//! upload and the wallet that paid. So payments are read by id, from the one
//! after the last read to the program's count, at the cluster's finalized
//! commitment: a payment is acted on only once it can no longer be rolled
//! back, and none can be read past, since the ids leave no gaps. Where they
//! were read to moves on only once what they admitted is written down, as on
//! an EVM chain.
//!
//! A registration is staged in a draft of the key's own, then registered in
//! one transaction carrying the key's signature over it and the reviewer's
//! go-ahead, each in an Ed25519 instruction.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::platform::Registration;
use chain::solana::programs::publishing::state::{ChainNeed, Lists};
use chain::solana::{Pubkey, Signer as _};
use shared::manifest::{Manifest, hash_to_bytes};
use shared::{Chain, ReviewAnswer};

use crate::on_solana::{self, OnSolana};
use crate::{Entry, Payment, Service, Wallet, build};

/// Where payments on the cluster were read to is kept under this chain's
/// name, as the next strategy id to read.
pub fn first_unread(read: &BTreeMap<String, u64>) -> u64 {
    read.get(&Chain::Solana.to_string())
        .copied()
        .unwrap_or(1)
        .max(1)
}

/// Take the cluster on once it is connected: the key blessed and stocked
/// there, the jobs a restart caught mid-way picked up, and only then its
/// uploads taken and its jobs built, and its payments read on from where the
/// last run left off.
pub async fn take_on(service: Arc<Service>, served: chain::SolanaServed) {
    let on = loop {
        match on_solana::take_on(
            shonin_rootkey::ImageKind::Builder,
            service.attestation.clone(),
            service.solana_key.clone(),
            served.clone(),
            crate::BLESSING_LOOKED_AT_EVERY,
        )
        .await
        {
            Ok(on) => break Arc::new(on),
            Err(error) => {
                tracing::warn!(cluster = %served.key, error = format!("{error:#}"), "the Solana cluster could not be taken on; trying again");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    crate::until_done(
        Chain::Solana,
        "the jobs a restart caught mid-way could not be picked up",
        || pick_up(&service, &on),
    )
    .await;
    if let Ok(mut solana) = service.solana.write() {
        *solana = Some(on.clone());
    }
    tracing::info!(cluster = on.cluster(), key = %on.address(), "taking uploads and building jobs on this Solana cluster");

    let mut from = first_unread(&*service.payments_read.lock().await);
    loop {
        let Some(turn) = service.turns.turn().await else {
            return;
        };
        if let Err(error) = admit(&service, &on, &mut from).await {
            tracing::warn!(
                cluster = on.cluster(),
                error = format!("{error:#}"),
                "could not read payments"
            );
        }
        drop(turn);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Pick the jobs on the cluster a restart caught mid-way back up (see
/// [`crate::picked_up`]).
async fn pick_up(service: &Service, on: &OnSolana) -> Result<()> {
    use shared::model::registry::JobState;
    let jobs = service.queue.lock().await.jobs.clone();
    let mut registered = BTreeMap::new();
    for entry in &jobs {
        if entry.job.chain != Chain::Solana
            || !matches!(entry.job.state, JobState::Reviewing | JobState::Building)
        {
            continue;
        }
        let Some(strategy_id) = entry.job.strategy_id else {
            continue;
        };
        if let Some(bundle_hash) = registered_bundle(on, strategy_id).await? {
            registered.insert(entry.job.id.clone(), bundle_hash);
        }
    }
    let mut queue = service.queue.lock().await;
    crate::picked_up(&mut queue.jobs, Chain::Solana, &registered, |bundle_hash| {
        service.holds(bundle_hash)
    });
    service.sweep(&queue.jobs);
    crate::write_queue(service, &mut queue).await
}

/// The bundle the cluster holds for a strategy, if it is registered.
pub async fn registered_bundle(on: &OnSolana, strategy_id: u64) -> Result<Option<String>> {
    Ok(on
        .served
        .platform
        .strategy(&on.served.client, strategy_id)
        .await?
        .map(|strategy| format!("sha256:{}", alloy::hex::encode(strategy.bundle_hash))))
}

/// Read the payments from strategy id `from` to the program's count, at the
/// finalized commitment, and admit the jobs they pay for; `from` moves on,
/// and is kept, once what they admitted is written down. A submission that
/// names no upload waiting from that wallet admits nothing, as on an EVM
/// chain.
async fn admit(service: &Arc<Service>, on: &OnSolana, from: &mut u64) -> Result<()> {
    let platform = &on.served.platform;
    let client = &on.served.client;
    let (count, at) = platform.finalized_count(client).await?;
    let mut payments = Vec::new();
    for id in *from..=count {
        let submission = platform
            .finalized_submission(client, id)
            .await?
            .ok_or_else(|| anyhow!("strategy {id} is counted and its submission isn't there"))?;
        payments.push(Payment {
            strategy_id: id,
            upload: alloy::primitives::B256::from(submission.upload),
            author: Wallet::Solana(submission.author.to_bytes()),
        });
    }
    if !payments.is_empty() {
        crate::admit_payments(service, Chain::Solana, payments).await?;
    }
    let next = count + 1;
    if next > *from {
        *from = next;
        let mut read = service.payments_read.lock().await;
        read.insert(Chain::Solana.to_string(), next);
        match serde_json::to_vec(&*read) {
            Ok(bytes) => {
                if let Err(error) = service
                    .keep(&service.payments_file, crate::PAYMENTS, &bytes)
                    .await
                {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "where payments were read to could not be written down"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "where payments were read to could not be written down")
            }
        }
    }
    // Every payment up to the cluster's finalized time has been read and
    // acted on: only then may an upload waiting here be forgotten.
    let at = time::OffsetDateTime::from_unix_timestamp(at.try_into()?)?;
    service.payments_read_to.lock().await.insert(
        Chain::Solana,
        time::PrimitiveDateTime::new(at.date(), at.time()),
    );
    service
        .payments_seen
        .lock()
        .await
        .insert(seen_key(on), count);
    Ok(())
}

/// What the wake plan names the cluster's payments under, as the API counts
/// them: the cluster's chain key.
pub fn seen_key(on: &OnSolana) -> String {
    on.cluster()
}

/// Whether the cluster has taken a payment past those read.
pub async fn unread(service: &Service, on: &OnSolana) -> bool {
    let Some(seen) = service
        .payments_seen
        .lock()
        .await
        .get(&seen_key(on))
        .copied()
    else {
        return true;
    };
    match on
        .served
        .platform
        .publishing_config(&on.served.client)
        .await
    {
        Ok(config) => config.count > seen,
        Err(error) => {
            tracing::debug!(
                error = format!("{error:#}"),
                "the payments taken could not be counted"
            );
            false
        }
    }
}

/// The lists a registration on the cluster names, from the manifest the
/// reviewer read: each chain with its drivers, the bridges, and the
/// restricted list sorted without repeats, as the program takes it.
pub fn lists(service: &Service, manifest: &Manifest) -> Result<Lists> {
    let chains = manifest
        .chains(&service.catalogue)
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .map(|need| ChainNeed {
            chain: chain::solana::name(need.chain.name()),
            drivers: need
                .drivers
                .iter()
                .map(|driver| chain::solana::name(driver))
                .collect(),
        })
        .collect();
    let bridges = manifest
        .bridge_names()
        .iter()
        .map(|bridge| chain::solana::name(bridge))
        .collect();
    let mut restricted: Vec<[u8; 32]> = manifest
        .restricted(&service.catalogue)
        .iter()
        .map(|code| chain::solana::name(code))
        .collect();
    restricted.sort_unstable();
    restricted.dedup();
    Ok(Lists {
        chains,
        bridges,
        restricted,
    })
}

/// The reviewer's go-ahead on Solana: its key and its signature over the
/// cluster's clearance digest for `strategy_id` and `source_hash`, checked
/// here before anything is sent, so a go-ahead that would only make the
/// registration fail is never paid for.
pub fn clearance(digest: &[u8; 32], answer: &ReviewAnswer) -> Result<(Pubkey, [u8; 64])> {
    let signer: Pubkey = answer
        .signer
        .as_deref()
        .context("the reviewer's go-ahead names no Solana key")?
        .parse()
        .map_err(|_| anyhow!("the reviewer's Solana key is not one"))?;
    let signature: [u8; 64] = alloy::hex::decode(
        answer
            .clearance
            .as_deref()
            .context("the reviewer gave no go-ahead")?,
    )
    .context("the reviewer's go-ahead is not hex")?
    .try_into()
    .map_err(|_| anyhow!("the reviewer's go-ahead is not an ed25519 signature"))?;
    if !chain::solana::executor::verify_ed25519(&signer, digest, signature) {
        bail!("the reviewer's go-ahead is not its signature over this strategy's source");
    }
    Ok((signer, signature))
}

/// Register a build on the cluster, as [`crate::register`] does on an EVM
/// chain: written into the store first, then staged and registered with the
/// key's signature and the reviewer's go-ahead. Returns the bundle the
/// cluster holds for the strategy, which the store holds too.
#[allow(clippy::too_many_arguments)]
pub async fn register(
    service: &Arc<Service>,
    entry: &Entry,
    strategy_id: u64,
    archive: &[u8],
    manifest: Manifest,
    built: build::Built,
    answer: &ReviewAnswer,
) -> Result<String> {
    let on = service.on_solana()?;
    let author = entry.author.solana()?;
    let (canonical, manifest_hash) =
        crate::canonical_of(service, entry, strategy_id, &manifest, &built);
    let lists = lists(service, &manifest)?;
    let bytes = |hash: &str| -> Result<[u8; 32]> { hash_to_bytes(hash).context("a hash") };
    let registration = Registration {
        id: strategy_id,
        author,
        source_hash: bytes(&entry.job.source_hash)?,
        bundle_hash: bytes(&built.bundle_hash)?,
        manifest_hash: bytes(&manifest_hash)?,
        lists,
    };
    let digest = on
        .served
        .platform
        .clearance_digest(strategy_id, &registration.source_hash);
    let (reviewer, clearance) = clearance(&digest, answer)?;
    crate::keep_built(service, entry, archive, &built, &canonical, &manifest_hash).await?;

    let sent = on
        .served
        .platform
        .register_signed(
            &on.served.client,
            &on.key,
            &registration,
            &reviewer,
            &clearance,
        )
        .await;
    // A registration whose landing was not seen may land all the same, and
    // one that landed is refused if sent again: either way, the cluster says
    // whether it holds it, and which bundle it holds.
    let bundle_hash = match sent {
        Ok(_) => built.bundle_hash,
        Err(error) => match registered_bundle(&on, strategy_id).await {
            Ok(Some(held)) => {
                tracing::info!(
                    id = entry.job.id,
                    %error,
                    "the registration was not seen to go through, and the cluster holds it"
                );
                held
            }
            _ => {
                return Err(error)
                    .context("the publishing program would not take the registration");
            }
        },
    };
    anyhow::ensure!(
        service.holds(&bundle_hash),
        "the cluster holds bundle {bundle_hash} for strategy {strategy_id}, and the store \
         does not hold its files, so the job has failed rather than read ready with it"
    );
    tracing::info!(
        id = entry.job.id,
        strategy_id,
        key = %on.key.pubkey(),
        "built, signed and registered on Solana"
    );
    Ok(bundle_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::Keypair;

    #[test]
    fn payments_on_solana_are_read_from_the_first_id_until_some_are() {
        let mut read = BTreeMap::new();
        assert_eq!(first_unread(&read), 1);
        read.insert("solana".to_string(), 7);
        assert_eq!(first_unread(&read), 7);
        read.insert("solana".to_string(), 0);
        assert_eq!(first_unread(&read), 1);
    }

    fn answer(signer: Option<String>, clearance: Option<String>) -> ReviewAnswer {
        ReviewAnswer {
            source_hash: String::new(),
            report_hash: String::new(),
            clearance,
            signer,
        }
    }

    #[test]
    fn only_the_named_reviewer_s_signature_over_this_digest_is_a_go_ahead() {
        let reviewer = Keypair::new_from_array([4; 32]);
        let digest = [9u8; 32];
        let signature = reviewer.sign_message(&digest);
        let hex = alloy::hex::encode_prefixed(signature.as_ref());
        let (signer, taken) = clearance(
            &digest,
            &answer(Some(reviewer.pubkey().to_string()), Some(hex.clone())),
        )
        .unwrap();
        assert_eq!(signer, reviewer.pubkey());
        assert_eq!(taken.as_slice(), signature.as_ref());
        // Over another digest, by another key, or naming none: no go-ahead.
        assert!(
            clearance(
                &[8; 32],
                &answer(Some(reviewer.pubkey().to_string()), Some(hex.clone()))
            )
            .is_err()
        );
        let other = Keypair::new_from_array([5; 32]);
        assert!(
            clearance(
                &digest,
                &answer(Some(other.pubkey().to_string()), Some(hex.clone()))
            )
            .is_err()
        );
        assert!(clearance(&digest, &answer(None, Some(hex))).is_err());
        assert!(clearance(&digest, &answer(Some(reviewer.pubkey().to_string()), None)).is_err());
        // An EVM go-ahead is no ed25519 signature.
        assert!(
            clearance(
                &digest,
                &answer(
                    Some(reviewer.pubkey().to_string()),
                    Some(format!("0x{}", "11".repeat(65)))
                )
            )
            .is_err()
        );
    }
}
