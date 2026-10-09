//! What the API says, in the shapes the web app reads.
//!
//! One snapshot carries everything a page renders, so the app fetches once and
//! every figure on the screen comes from the same moment. The types are plain
//! data with serde derives, and they compile for the browser and for the
//! services alike, so neither side can describe the platform differently from
//! the other.

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

use crate::model::common::Chain;
use crate::model::health::Health;
use crate::model::registry::{Author, FeedEvent, Job};
use crate::model::strategy::Strategy;
use crate::model::vault::ClaimState;

/// Everything the app knows, as of one moment on the chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Rises whenever anything below changes, so the app can ask "is there
    /// anything new?" without reading the rest.
    pub version: u64,
    /// The chains' own clock, not the reader's: the latest of them (each
    /// chain's own is on its [`ChainInfo`]). Everything
    /// dated is relative to its own chain, so a page reads correctly however
    /// fast a chain is running.
    pub as_of: PrimitiveDateTime,
    /// Every vault chain the snapshot was read from. Each strategy says which
    /// one its vault is on.
    pub chains: Vec<ChainInfo>,
    pub strategies: Vec<Strategy>,
    pub authors: Vec<Author>,
    pub jobs: Vec<Job>,
    /// When the build service last gave its queue, on the reader's clock.
    /// None until it first has.
    #[serde(default)]
    pub jobs_as_of: Option<PrimitiveDateTime>,
    /// Whether the build service failed to answer the latest ask: the queue
    /// is then the one it last gave, as of `jobs_as_of`.
    #[serde(default)]
    pub jobs_stale: bool,
    pub feed: Vec<FeedEvent>,
    pub totals: Totals,
}

/// Which chain the platform is reading, for the pages that say so.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainInfo {
    pub chain: Chain,
    /// The chain as the chain config names it, whatever kind it is:
    /// `evm:31337`, `solana:localnet`.
    #[serde(default)]
    pub key: String,
    /// An EVM chain's id, what a wallet is asked to switch to and what typed
    /// data names. None on a chain that isn't an EVM one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,
    /// The block the snapshot was built from: on Solana, the slot.
    pub block: u64,
    /// That block's time, on the chain's own clock: each chain's part of the
    /// snapshot is read on its own, so each says how fresh it is. None until
    /// the chain is first read.
    #[serde(default)]
    pub as_of: Option<PrimitiveDateTime>,
}

impl Snapshot {
    /// What the snapshot read from one vault chain.
    pub fn chain(&self, chain: Chain) -> Option<&ChainInfo> {
        self.chains.iter().find(|info| info.chain == chain)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Totals {
    pub vault_capital: f64,
    pub trades: u32,
}

/// What the app needs to reach one vault chain itself, from
/// `/api/{chain}/config`. No secrets: the addresses are public and the
/// development wallet is an account the chain published.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub chain: ChainInfo,
    pub rpc_url: String,
    pub addresses: Addresses,
    /// The asset deposits and withdrawals are paid in.
    pub asset_symbol: String,
    pub asset_decimals: u8,
    /// The terms the platform is running under on this chain, read from it.
    pub terms: Terms,
    /// Set on a development chain, where accounts are unlocked and the app can
    /// offer one as a wallet. Never set anywhere else.
    pub dev: Option<DevMode>,
}

/// The terms a new vault is created with and the charges an author meets,
/// every one of them read from the contract that enforces it. A page that
/// states one of these as a fact quotes this rather than a number written into
/// the app, so the app cannot go stale against the chain it is talking to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Terms {
    /// The least a vault may be activated with, so an agent that can't run is
    /// never activated. The factory refuses anything under it.
    pub minimum_tank: f64,
    /// The flat fee to publish, covering the review and the build.
    pub publish_fee: f64,
    /// The fee schedule a vault created today carries for its whole life.
    pub fees: crate::Fees,
    /// How far either side of the venue's price a claim may close.
    pub exit_band_bps: u32,
    /// How long a claim may go unserved before anyone may freeze the vault.
    pub freeze_after_days: f64,
    /// How long a proposed runtime image waits before it may be blessed.
    pub image_timelock_days: f64,
    /// The least a deposit may be.
    pub minimum_deposit: f64,
    /// The hash of these terms, which an author's activation names so it
    /// can't land on terms they never read.
    pub terms_hash: String,
}

/// Where the platform lives on one vault chain, in the shape that kind of
/// chain has. Untagged, so an EVM chain's reads exactly as it always has.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Addresses {
    Evm(EvmAddresses),
    Solana(SolanaAddresses),
}

impl Addresses {
    pub fn evm(&self) -> Option<&EvmAddresses> {
        match self {
            Self::Evm(addresses) => Some(addresses),
            Self::Solana(_) => None,
        }
    }

    pub fn solana(&self) -> Option<&SolanaAddresses> {
        match self {
            Self::Solana(addresses) => Some(addresses),
            Self::Evm(_) => None,
        }
    }
}

/// Where each contract lives, as hex strings, because that is what a
/// transaction carries and what a browser hands to a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmAddresses {
    pub usd: String,
    pub publishing: String,
    pub prices: String,
    pub venue: String,
    pub attestation: String,
    pub strategies: String,
    pub reviews: String,
    pub profiles: String,
    pub factory: String,
    pub station_factory: String,
}

/// Where the platform lives on a Solana cluster, each key in base58: every
/// program by its library name (`shonin_vault`), the dollar's mint and the
/// token program that owns it, and the accounts a client starts from.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SolanaAddresses {
    /// The cluster's name: `localnet`, `devnet`, `mainnet-beta`.
    pub cluster: String,
    pub programs: std::collections::BTreeMap<String, String>,
    pub mint: String,
    pub token_program: String,
    /// The vault program's factory account: the terms, and the vault count.
    pub vault_factory: String,
    /// The publishing program's config account: the fee, and the count of
    /// strategies submitted.
    pub publishing_config: String,
}

/// The development chain's offer: an account it already holds the key for, so
/// the app can act as someone without anyone installing anything.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DevMode {
    pub address: String,
    /// What the account is called on the page.
    pub label: String,
}

/// What one address holds, read from the chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
/// What one address holds on one vault chain, from
/// `/api/{chain}/portfolio/{address}`.
pub struct Portfolio {
    pub chain: Chain,
    pub address: String,
    /// The asset, outside any vault.
    pub balance: f64,
    pub positions: Vec<Holding>,
    pub claims: Vec<HeldClaim>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Holding {
    /// The strategy whose vault holds it.
    pub slug: String,
    pub shares: f64,
    /// What those shares are worth at mark.
    pub value: f64,
    /// What they would fetch on the way out, after the spread and the fee.
    pub exit_value: f64,
    /// What was paid for them, read from the vault's own deposits and
    /// withdrawals, so a profit or loss is the chain's arithmetic rather than
    /// something the browser remembered.
    pub cost: f64,
}

/// A claim this address is waiting on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeldClaim {
    pub slug: String,
    pub id: u64,
    pub shares: f64,
    pub fraction_closed: f64,
    pub state: ClaimState,
    pub booked: PrimitiveDateTime,
    pub settled_for: Option<f64>,
}

/// An agent's source, uploaded to `/api/{chain}/uploads` before its author
/// pays to publish it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    /// The wallet that will pay. Only a payment from it starts the job.
    pub author: String,
    /// The project as a canonical tar ([`crate::archive::pack`]), as base64.
    pub archive_base64: String,
    /// Sealed source is still reviewed and built; only its publishing stops.
    pub sealed: bool,
    /// Which package of a workspace is the agent, when the project holds more
    /// than one `shonin.toml`.
    #[serde(default)]
    pub package: Option<String>,
}

/// What the build service says when it takes an upload: the reference to pay
/// against, and the job that will follow it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadAccepted {
    /// `0x`-prefixed 32 bytes: what `Publishing.submit` takes.
    pub upload: String,
    pub job_id: String,
}

/// What the build service hands the reviewer, before anything is built:
/// `POST /{chain}/reviews`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub strategy_id: u64,
    /// The source, exactly as uploaded. The reviewer hashes it itself.
    pub archive_base64: String,
    pub sealed: bool,
    /// Where the agent's `shonin.toml` is inside the archive.
    pub manifest_path: String,
}

/// The reviewer's answer: its go-ahead for the build, or a refusal it has
/// already recorded on the chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewAnswer {
    /// The source hash the reviewer computed and signed for.
    pub source_hash: String,
    pub report_hash: String,
    /// The signed go-ahead `StrategyRegistry.register` needs, as `0x` hex:
    /// on an EVM chain the reviewer key's 65-byte signature, on Solana its
    /// 64-byte ed25519 signature over the cluster's clearance digest. None
    /// when the code was refused.
    pub clearance: Option<String>,
    /// On Solana, the reviewer key that signed the go-ahead (base58), which
    /// the registration names. None on an EVM chain, where the signature
    /// names its signer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
}

/// A health report, as the executor signs and posts it.
///
/// Posted to `/api/{chain}/health`: the same address can be a different vault
/// on another chain.
///
/// - on an EVM chain, `signature` is the 65-byte secp256k1 signature of
///   [`HealthPost::digest`] as an Ethereum signed message (`0x` hex), and the
///   signer is recovered from it; `signer` is absent.
/// - on Solana, `signature` is the 64-byte ed25519 signature of the 32-byte
///   [`HealthPost::solana_digest`] itself, in base58, which binds the
///   cluster, and `signer` is the key that made it, in base58: ed25519 has
///   no recovery, so the post names its key and the reader checks the
///   signature against it.
///
/// Either way the reader then takes the report only if that key is blessed as
/// the vault's executor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthPost {
    /// The vault it is about.
    pub vault: String,
    pub report: Health,
    /// A signature over [`HealthPost::digest`] (on Solana,
    /// [`HealthPost::solana_digest`]) by a key blessed for that vault, so a
    /// report can only come from whoever holds the vault's keys.
    pub signature: String,
    /// The signing key, where it can't be recovered from the signature: an
    /// ed25519 key, in base58.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
}

impl HealthPost {
    /// What a reporter signs and a reader checks: the vault chain, the vault
    /// and the report, canonically encoded, so two encodings of one report
    /// cannot differ. The chain is in it because one key can be blessed for
    /// the same address on two chains, and a report about one must not pass
    /// for a report about the other. An EVM address is taken in lower case,
    /// whatever case it came in; a Solana key is taken as written, since
    /// base58 is case-sensitive.
    pub fn digest(chain: Chain, vault: &str, report: &Health) -> [u8; 32] {
        let vault = if vault.starts_with("0x") || vault.starts_with("0X") {
            vault.to_ascii_lowercase()
        } else {
            vault.to_string()
        };
        Self::digest_bound(b"shonin-health/2", chain.name().as_bytes(), &vault, report)
    }

    /// What a Solana executor signs and a reader checks: as
    /// [`HealthPost::digest`], but bound to the cluster rather than the
    /// chain's name, by the 32-byte value the vault program binds its own
    /// digests to on that cluster. A vault has the same address on every
    /// cluster, so a report about it on one cluster must not pass for a
    /// report on another. The vault is taken as written.
    pub fn solana_digest(cluster: &[u8; 32], vault: &str, report: &Health) -> [u8; 32] {
        Self::digest_bound(b"shonin-health-solana/1", cluster, vault, report)
    }

    fn digest_bound(tag: &[u8], bound: &[u8], vault: &str, report: &Health) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut body = Vec::new();
        // Canonical CBOR, the same encoding the manifest and bundle hashes
        // use, so there is one way to write a report down.
        ciborium::into_writer(report, &mut body).expect("a report encodes");

        let mut hasher = Sha256::new();
        hasher.update(tag);
        hasher.update(bound);
        hasher.update(b"/");
        hasher.update(vault.as_bytes());
        hasher.update(&body);
        hasher.finalize().into()
    }
}

/// What an error looks like, whichever endpoint returned it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::health::{Health, Signal};

    /// A reader recovers the signer from the report as it arrived, so any
    /// value that reads back even slightly different names someone else.
    #[test]
    fn a_health_report_digests_the_same_after_travelling_as_json() {
        let signals = (1..2000)
            .map(|i| Signal {
                name: "runway".into(),
                value: i as f64 / 7.3 + 1e-9 * i as f64,
                unit: "days".into(),
                warn_at: 1.0 / i as f64,
                error_at: (i as f64).sqrt(),
                lower_is_worse: true,
                window: "now".into(),
                note: None,
            })
            .collect();
        let report = Health {
            signals,
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 274).unwrap(),
                time::Time::from_hms_nano(5, 38, 52, 757_106_000).unwrap(),
            ),
            signed_by: "0x0".into(),
            at: 0,
        };
        let arrived: Health =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(
            HealthPost::digest(Chain::Base, "0xA", &report),
            HealthPost::digest(Chain::Base, "0xA", &arrived)
        );
    }

    #[test]
    fn a_report_about_one_chain_is_not_a_report_about_another() {
        let report = Health {
            signals: vec![],
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 274).unwrap(),
                time::Time::MIDNIGHT,
            ),
            signed_by: "0x0".into(),
            at: 0,
        };
        assert_ne!(
            HealthPost::digest(Chain::Base, "0xA", &report),
            HealthPost::digest(Chain::Hyperliquid, "0xA", &report)
        );
    }

    fn a_report() -> Health {
        Health {
            signals: vec![],
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 274).unwrap(),
                time::Time::MIDNIGHT,
            ),
            signed_by: "0x0".into(),
            at: 0,
        }
    }

    #[test]
    fn an_evm_vault_is_digested_in_any_case_and_a_solana_vault_only_as_written() {
        let report = a_report();
        assert_eq!(
            HealthPost::digest(Chain::Base, "0xABCDEF", &report),
            HealthPost::digest(Chain::Base, "0xabcdef", &report)
        );
        let key = "So11111111111111111111111111111111111111112";
        assert_ne!(
            HealthPost::solana_digest(&[1; 32], key, &report),
            HealthPost::solana_digest(&[1; 32], &key.to_lowercase(), &report)
        );
    }

    #[test]
    fn a_solana_report_about_one_cluster_is_not_a_report_about_another() {
        let report = a_report();
        let key = "So11111111111111111111111111111111111111112";
        assert_ne!(
            HealthPost::solana_digest(&[1; 32], key, &report),
            HealthPost::solana_digest(&[2; 32], key, &report)
        );
    }

    /// The EVM digest is what EVM executors already sign: its layout stays.
    #[test]
    fn an_evm_report_digests_as_it_always_has() {
        use sha2::{Digest, Sha256};
        let report = a_report();
        let mut body = Vec::new();
        ciborium::into_writer(&report, &mut body).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"shonin-health/2");
        hasher.update(Chain::Base.name().as_bytes());
        hasher.update(b"/");
        hasher.update(b"0xabc");
        hasher.update(&body);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(HealthPost::digest(Chain::Base, "0xABC", &report), expected);
    }

    /// The time a report was signed is part of what is signed, so a report
    /// can't be re-dated without its signature failing.
    #[test]
    fn a_report_s_signing_time_is_inside_its_digest() {
        let undated = a_report();
        let dated = Health {
            at: 1_790_000_000,
            ..a_report()
        };
        let later = Health {
            at: 1_790_000_001,
            ..a_report()
        };
        assert_ne!(
            HealthPost::digest(Chain::Base, "0xA", &undated),
            HealthPost::digest(Chain::Base, "0xA", &dated)
        );
        assert_ne!(
            HealthPost::digest(Chain::Base, "0xA", &dated),
            HealthPost::digest(Chain::Base, "0xA", &later)
        );
        let arrived: Health =
            serde_json::from_str(&serde_json::to_string(&dated).unwrap()).unwrap();
        assert_eq!(arrived.at, 1_790_000_000);
        assert_eq!(
            HealthPost::digest(Chain::Base, "0xA", &dated),
            HealthPost::digest(Chain::Base, "0xA", &arrived)
        );
    }

    /// A report written before reports were dated still reads, as undated.
    #[test]
    fn a_report_written_before_it_had_a_time_reads_as_undated() {
        let mut old = serde_json::to_value(a_report()).unwrap();
        // What an older writer produced: the same fields, and no time.
        old.as_object_mut().unwrap().remove("at");
        let read: Health = serde_json::from_value(old).unwrap();
        assert_eq!(read.at, 0);
        assert_eq!(
            HealthPost::digest(Chain::Base, "0xA", &read),
            HealthPost::digest(Chain::Base, "0xA", &a_report())
        );
    }

    #[test]
    fn an_evm_chain_s_addresses_and_chain_read_as_they_always_have() {
        let json = r#"{"usd":"0x1","publishing":"0x2","prices":"0x3","venue":"0x4",
            "attestation":"0x5","strategies":"0x6","reviews":"0x7","profiles":"0x8",
            "factory":"0x9","station_factory":"0xa"}"#;
        let addresses: Addresses = serde_json::from_str(json).unwrap();
        assert_eq!(addresses.evm().unwrap().factory, "0x9");
        let back = serde_json::to_value(&addresses).unwrap();
        assert_eq!(back["usd"], "0x1");

        let solana = Addresses::Solana(SolanaAddresses {
            cluster: "localnet".into(),
            programs: [("shonin_vault".to_string(), "V".to_string())].into(),
            mint: "M".into(),
            token_program: "T".into(),
            vault_factory: "F".into(),
            publishing_config: "P".into(),
        });
        let read: Addresses =
            serde_json::from_str(&serde_json::to_string(&solana).unwrap()).unwrap();
        assert_eq!(read.solana().unwrap().mint, "M");

        // A chain written before it had a key still reads, and an EVM
        // chain's id is where it always was.
        let info: ChainInfo =
            serde_json::from_str(r#"{"chain":"base","chain_id":31337,"block":5}"#).unwrap();
        assert_eq!(info.chain_id, Some(31337));
        let solana: ChainInfo =
            serde_json::from_str(r#"{"chain":"solana","key":"solana:localnet","block":5}"#)
                .unwrap();
        assert_eq!(solana.chain_id, None);
        assert!(!serde_json::to_string(&solana).unwrap().contains("chain_id"));
    }
}
