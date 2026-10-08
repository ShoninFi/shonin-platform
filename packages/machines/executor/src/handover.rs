//! Taking a vault over from an executor that has gone quiet.
//!
//! Keys live in memory only, so an executor that restarts, or a new one,
//! finds the vault's station approving a key it doesn't hold, still blessed.
//! It watches the API for a health report signed by that key and dated
//! after the watch began. While one keeps coming, the vault is that
//! executor's and is left alone; once the API has answered with none for
//! the whole silence, the vault is taken over: a key of this executor's own
//! is blessed and approved in the old one's place (the station lets a
//! blessed key replace another).
//!
//! An executor that finds itself replaced lets the vault go and never takes
//! it back while the other key is blessed: that is the sweep's.

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, Signature};
use anyhow::{Context, Result};
use shared::HealthPost;

/// How long a vault's approved key must be silent before it is taken over.
pub const HANDOVER_SILENCE: u64 = 600;

/// How far the two executors' clocks, and the API's, may disagree: a report
/// dated this much before the watch began still counts as heard.
pub const CLOCK_SKEW: u64 = 30;

/// How far in the future a report may be dated and still be believed.
const FUTURE: u64 = 60;

/// How often the API is asked during a watch.
const LOOK_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(30)
};

/// One vault's watch on another executor's key: an EVM address, or a
/// Solana key.
#[derive(Debug)]
pub struct Watch<K = Address> {
    /// The key the station approves.
    pub by: K,
    /// When the current stretch of silence began, in unix seconds: the watch
    /// began then, or the key's latest report was heard then.
    began: u64,
    last_look: Option<Instant>,
}

/// What a look at the API found.
#[derive(Debug, PartialEq)]
pub enum Heard {
    /// A report from the key, dated within the watch.
    Report,
    /// None from the key since the watch began.
    Nothing,
}

impl<K: PartialEq + Copy> Watch<K> {
    pub fn new(by: K, now: u64) -> Watch<K> {
        Watch {
            by,
            began: now,
            last_look: None,
        }
    }

    /// Whether the API is to be asked now.
    pub fn look_due(&mut self, now: Instant) -> bool {
        let due = self
            .last_look
            .is_none_or(|last| now.duration_since(last) >= LOOK_EVERY);
        if due {
            self.last_look = Some(now);
        }
        due
    }

    /// Take note of the latest report the API holds for the vault: its
    /// signer and when it was signed. One from the watched key, dated after
    /// the watch began (give or take [`CLOCK_SKEW`]) and not in the future,
    /// starts the silence again.
    pub fn heard(&mut self, latest: Option<(K, u64)>, now: u64) -> Heard {
        match latest {
            Some((signer, at))
                if signer == self.by
                    && at != 0
                    && at.saturating_add(CLOCK_SKEW) >= self.began
                    && at <= now.saturating_add(FUTURE) =>
            {
                self.began = now;
                Heard::Report
            }
            _ => Heard::Nothing,
        }
    }

    /// Whether the key has been silent for `silence` seconds by `now`. Asked
    /// only after a look the API answered, so an API out of reach never
    /// counts as a silent executor.
    pub fn silent_for(&self, silence: u64, now: u64) -> bool {
        now >= self.began.saturating_add(silence)
    }
}

/// The latest report the API took for a vault: who signed it, and when it
/// says it was signed. `None` if the API has none.
pub async fn latest_report(
    http: &reqwest::Client,
    api: &str,
    chain: shared::Chain,
    vault: Address,
) -> Result<Option<(Address, u64)>> {
    let answer = http
        .get(format!("{api}/api/{chain}/health/{vault:?}/signed"))
        .send()
        .await
        .context("asking the API for the vault's latest report")?;
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let answer = answer
        .error_for_status()
        .context("the API would not say the vault's latest report")?;
    let post: HealthPost = answer.json().await.context("the API's report")?;
    Ok(Some((signer(chain, &post)?, post.report.at)))
}

/// The latest report the API took for a Solana vault: the key that signed it
/// and when it says it was signed, once its signature is checked here, as
/// that key's ed25519 signature over the report's digest on this cluster.
/// `None` if the API has none; a report whose signature doesn't check is no
/// report.
pub async fn latest_solana_report(
    http: &reqwest::Client,
    api: &str,
    chain: shared::Chain,
    cluster: &[u8; 32],
    vault: &chain::solana::Pubkey,
) -> Result<Option<(chain::solana::Pubkey, u64)>> {
    let answer = http
        .get(format!("{api}/api/{chain}/health/{vault}/signed"))
        .send()
        .await
        .context("asking the API for the vault's latest report")?;
    if answer.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let answer = answer
        .error_for_status()
        .context("the API would not say the vault's latest report")?;
    let post: HealthPost = answer.json().await.context("the API's report")?;
    Ok(solana_signer(cluster, vault, &post).map(|signer| (signer, post.report.at)))
}

/// Who signed a Solana report: the key it names, if the signature is that
/// key's over the report's digest for this vault on this cluster.
pub fn solana_signer(
    cluster: &[u8; 32],
    vault: &chain::solana::Pubkey,
    post: &HealthPost,
) -> Option<chain::solana::Pubkey> {
    use chain::solana::Signer;
    fn parsed_like<T: std::str::FromStr>(_: &T, text: &str) -> Option<T> {
        text.parse().ok()
    }
    if post.vault != vault.to_string() {
        return None;
    }
    let signer: chain::solana::Pubkey = post.signer.as_deref()?.parse().ok()?;
    let probe = chain::solana::Keypair::new_from_array([1; 32]).sign_message(&[]);
    let signature: [u8; 64] = parsed_like(&probe, &post.signature)?.into();
    let digest = HealthPost::solana_digest(cluster, &post.vault, &post.report);
    chain::solana::executor::verify_ed25519(&signer, &digest, signature).then_some(signer)
}

/// Who signed a report, recovered from its signature over the report's
/// digest: the API is not trusted to say.
pub fn signer(chain: shared::Chain, post: &HealthPost) -> Result<Address> {
    let bytes = alloy::hex::decode(&post.signature).context("the signature is not hex")?;
    let signature = Signature::try_from(bytes.as_slice())
        .map_err(|error| anyhow::anyhow!("the signature is malformed: {error}"))?;
    let digest = B256::from(HealthPost::digest(chain, &post.vault, &post.report));
    Ok(signature.recover_address_from_msg(digest.as_slice())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn a_key_silent_for_the_whole_watch_is_taken_over() {
        let by = Address::repeat_byte(0xee);
        let mut watch = Watch::new(by, NOW);
        assert_eq!(watch.heard(None, NOW + 30), Heard::Nothing);
        assert!(!watch.silent_for(HANDOVER_SILENCE, NOW + HANDOVER_SILENCE - 1));
        assert_eq!(watch.heard(None, NOW + HANDOVER_SILENCE), Heard::Nothing);
        assert!(watch.silent_for(HANDOVER_SILENCE, NOW + HANDOVER_SILENCE));
    }

    #[test]
    fn a_report_from_the_key_dated_after_the_watch_began_leaves_the_vault_alone() {
        let by = Address::repeat_byte(0xee);
        let mut watch = Watch::new(by, NOW);
        let heard_at = NOW + HANDOVER_SILENCE - 5;
        assert_eq!(watch.heard(Some((by, NOW + 300)), heard_at), Heard::Report);
        // The silence starts again from when the report was heard.
        assert!(!watch.silent_for(HANDOVER_SILENCE, NOW + HANDOVER_SILENCE));
        assert!(watch.silent_for(HANDOVER_SILENCE, heard_at + HANDOVER_SILENCE));
    }

    #[test]
    fn only_a_fresh_report_signed_by_the_approved_key_counts() {
        let by = Address::repeat_byte(0xee);
        let mut watch = Watch::new(by, NOW);
        // Another key's report.
        assert_eq!(
            watch.heard(Some((Address::repeat_byte(0xaa), NOW + 10)), NOW + 20),
            Heard::Nothing
        );
        // One dated well before the watch began: an old report replayed.
        assert_eq!(
            watch.heard(Some((by, NOW - CLOCK_SKEW - 1)), NOW + 20),
            Heard::Nothing
        );
        // An undated one.
        assert_eq!(watch.heard(Some((by, 0)), NOW + 20), Heard::Nothing);
        // One dated far ahead.
        assert_eq!(
            watch.heard(Some((by, NOW + 20 + FUTURE + 1)), NOW + 20),
            Heard::Nothing
        );
        assert!(watch.silent_for(HANDOVER_SILENCE, NOW + HANDOVER_SILENCE));
        // Within the clocks' disagreement it counts.
        assert_eq!(
            watch.heard(Some((by, NOW - CLOCK_SKEW)), NOW + 20),
            Heard::Report
        );
    }

    #[test]
    fn a_solana_report_counts_only_with_the_named_key_s_signature_for_this_vault_and_cluster() {
        use chain::solana::Signer;
        let key = crate::keys::fresh_solana().unwrap();
        let vault = chain::solana::Pubkey::new_from_array([0x11; 32]);
        let cluster = [7u8; 32];
        let report = shared::model::health::Health {
            signals: vec![],
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 280).unwrap(),
                time::Time::MIDNIGHT,
            ),
            signed_by: key.pubkey().to_string(),
            at: NOW,
        };
        let digest = HealthPost::solana_digest(&cluster, &vault.to_string(), &report);
        let mut post = HealthPost {
            vault: vault.to_string(),
            report,
            signature: key.sign_message(&digest).to_string(),
            signer: Some(key.pubkey().to_string()),
        };
        assert_eq!(solana_signer(&cluster, &vault, &post), Some(key.pubkey()));
        // Another cluster, another vault, a changed report, another named
        // key: none is the key's report.
        assert_eq!(solana_signer(&[8u8; 32], &vault, &post), None);
        let other = chain::solana::Pubkey::new_from_array([0x22; 32]);
        assert_eq!(solana_signer(&cluster, &other, &post), None);
        let stranger = crate::keys::fresh_solana().unwrap();
        post.signer = Some(stranger.pubkey().to_string());
        assert_eq!(solana_signer(&cluster, &vault, &post), None);
        post.signer = Some(key.pubkey().to_string());
        post.report.at += 1;
        assert_eq!(solana_signer(&cluster, &vault, &post), None);

        // And the watch takes a Solana key like an address.
        let mut watch = Watch::new(key.pubkey(), NOW);
        assert_eq!(
            watch.heard(Some((key.pubkey(), NOW + 5)), NOW + 10),
            Heard::Report
        );
        assert_eq!(
            watch.heard(Some((stranger.pubkey(), NOW + 15)), NOW + 20),
            Heard::Nothing
        );
    }

    #[test]
    fn the_signer_of_a_report_is_recovered_from_its_signature() {
        let key = crate::keys::fresh().unwrap();
        let report = shared::model::health::Health {
            signals: vec![],
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 280).unwrap(),
                time::Time::MIDNIGHT,
            ),
            signed_by: key.address().to_string(),
            at: NOW,
        };
        let vault = format!("{:?}", Address::repeat_byte(0x11));
        let digest = HealthPost::digest(shared::Chain::Base, &vault, &report);
        use alloy::signers::SignerSync;
        let signature = key.sign_message_sync(&digest).unwrap();
        let mut post = HealthPost {
            vault,
            report,
            signature: alloy::hex::encode_prefixed(signature.as_bytes()),
            signer: None,
        };
        assert_eq!(signer(shared::Chain::Base, &post).unwrap(), key.address());
        // A report changed after signing names someone else.
        post.report.at += 1;
        assert_ne!(signer(shared::Chain::Base, &post).unwrap(), key.address());
    }
}
