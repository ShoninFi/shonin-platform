//! What the API says, in the shapes the web app reads.
//!
//! One snapshot carries everything a page renders, so the app fetches once and
//! every figure on the screen comes from the same moment. The types are plain
//! data with serde derives, and they compile for the browser and for the
//! services alike, so neither side can describe the platform differently from
//! the other.

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

use crate::model::common::{Chain, Point};
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
    /// The chains' own clock, not the reader's: the latest of them. Everything
    /// dated is relative to its own chain, so a page reads correctly however
    /// fast a chain is running.
    pub as_of: PrimitiveDateTime,
    /// Every vault chain the snapshot was read from. Each strategy says which
    /// one its vault is on.
    pub chains: Vec<ChainInfo>,
    pub strategies: Vec<Strategy>,
    pub authors: Vec<Author>,
    pub jobs: Vec<Job>,
    pub feed: Vec<FeedEvent>,
    /// Simple things to compare a record against.
    pub benchmarks: Vec<Benchmark>,
    pub totals: Totals,
}

/// Which chain the platform is reading, for the pages that say so.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainInfo {
    pub chain: Chain,
    pub chain_id: u64,
    /// The block the snapshot was built from.
    pub block: u64,
}

impl Snapshot {
    /// What the snapshot read from one vault chain.
    pub fn chain(&self, chain: Chain) -> Option<&ChainInfo> {
        self.chains.iter().find(|info| info.chain == chain)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Benchmark {
    pub label: String,
    /// What it is, said plainly, because a benchmark nobody can place is
    /// worse than none.
    pub note: String,
    pub series: Vec<Point>,
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Terms {
    /// The least a vault may be created with: the build, the review and a
    /// minimum run. The factory refuses anything under it.
    pub minimum_tank: f64,
    /// A flat fee for one review, charged whoever asks for it.
    pub review_fee: f64,
    /// The fee schedule a vault created today carries for its whole life.
    pub fees: crate::Fees,
    /// How far either side of the venue's price a claim may close.
    pub exit_band_bps: u32,
    /// How long a claim may go unserved before anyone may freeze the vault.
    pub freeze_after_days: f64,
    /// How long a proposed runtime image waits before it may be blessed.
    pub image_timelock_days: f64,
    /// Days of charges auto-fill aims to keep in a tank.
    pub tank_target_days: u32,
}

/// Where each contract lives, as hex strings, because that is what a
/// transaction carries and what a browser hands to a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Addresses {
    pub usd: String,
    pub venue: String,
    pub attestation: String,
    pub strategies: String,
    pub reviews: String,
    pub profiles: String,
    pub factory: String,
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

/// A strategy offered to the build service.
#[derive(Clone, Debug, Serialize, Deserialize)]
/// A strategy offered to be built, posted to `/api/{chain}/submissions`: the
/// vault chain its vault was created on, where the build is registered.
pub struct Submission {
    pub strategy_id: String,
    pub version: u32,
    /// The address that will own it, and whose vault pays for the build.
    pub author: String,
    pub source: SubmissionSource,
    /// Which package of the submitted project is the strategy.
    pub package: String,
    /// Sealed source is still built and still reviewed; it is only the
    /// publishing of it that stops.
    pub sealed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum SubmissionSource {
    /// A canonical archive of the project, as the publisher packs it.
    Archive { base64: String },
    /// A public repository and a commit, which the build service reads itself.
    Repository { url: String, commit: String },
}

/// What the build service says when it takes a submission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmissionAccepted {
    pub job_id: String,
    pub source_hash: String,
}

/// A health report, as the executor signs and posts it.
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Posted to `/api/{chain}/health`: the same address can be a different vault
/// on another chain.
pub struct HealthPost {
    /// The vault it is about.
    pub vault: String,
    pub report: Health,
    /// A signature over [`HealthPost::digest`] by a key blessed for that
    /// vault, so a report can only come from whoever holds the vault's keys.
    pub signature: String,
}

impl HealthPost {
    /// What a reporter signs and a reader checks: the vault and the report,
    /// canonically encoded, so two encodings of one report cannot differ.
    pub fn digest(vault: &str, report: &Health) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut body = Vec::new();
        // Canonical CBOR, the same encoding the manifest and bundle hashes
        // use, so there is one way to write a report down.
        ciborium::into_writer(report, &mut body).expect("a report encodes");

        let mut hasher = Sha256::new();
        hasher.update(b"shonin-health/1");
        hasher.update(vault.to_ascii_lowercase().as_bytes());
        hasher.update(&body);
        hasher.finalize().into()
    }
}

/// What an error looks like, whichever endpoint returned it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
}
