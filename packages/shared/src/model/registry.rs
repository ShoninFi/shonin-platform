//! The rest of what the app lists: authors, the build queue and the public
//! feed.

use serde::{Deserialize, Serialize};
use time::{Date, PrimitiveDateTime};

use super::review::Review;

/// Whoever publishes strategies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Author {
    pub handle: String,
    pub joined: Date,
    /// The author's own words about what they build.
    pub blurb: String,
    /// Where else they can be found, as label and URL.
    pub links: Vec<(String, String)>,
}

/// Where a publishing job is. A job shows how it ended, never a log, which
/// could leak sealed source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum JobState {
    /// Uploaded, waiting for the $1 from the same wallet.
    AwaitingPayment,
    /// Paid; waiting its turn.
    Queued {
        position: u32,
    },
    /// The reviewer is reading the source. Nothing is built before it has.
    Reviewing,
    Building,
    /// Reviewed, built and registered: its author can activate it.
    Ready,
    /// Its author activated it, and it has a vault.
    Activated,
    /// The reviewer found the code malicious; nothing was built.
    Refused,
    /// The build failed, or ran past a limit, or failed on our side.
    Failed {
        outcome: Failure,
    },
}

impl JobState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::AwaitingPayment => "awaiting payment",
            Self::Queued { .. } => "queued",
            Self::Reviewing => "reviewing",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Activated => "activated",
            Self::Refused => "refused",
            Self::Failed { .. } => "failed",
        }
    }

    /// Whether the job has ended, one way or another.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Activated | Self::Refused | Self::Failed { .. }
        )
    }
}

/// How a failed job failed, coarsely, so it says enough to act on and nothing
/// about the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failure {
    /// The manifest isn't one the platform accepts.
    Manifest,
    /// The source is over the size limit.
    TooLarge,
    /// It didn't compile, or the component didn't load.
    Build,
    /// The build ran past the time limit.
    TimedOut,
    /// Something failed on our side. The fee is spent all the same, since a
    /// free retry is something anyone could trigger.
    Service,
}

impl Failure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manifest => "the manifest was refused",
            Self::TooLarge => "the source is over the size limit",
            Self::Build => "the build failed",
            Self::TimedOut => "the build ran past the time limit",
            Self::Service => "the job failed on our side",
        }
    }
}

/// One job in the public, first-come-first-served queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// The vault chain it was paid for on.
    pub chain: crate::model::common::Chain,
    /// Assigned when the $1 is paid.
    pub strategy_id: Option<u64>,
    /// The title from its `shonin.toml`, once read.
    pub title: String,
    /// The paying wallet, or the author's handle.
    pub author: String,
    pub state: JobState,
    pub submitted: PrimitiveDateTime,
    pub duration_seconds: u32,
    pub source_hash: String,
    pub sealed: bool,
    /// Present once the build has produced one.
    pub bundle_hash: Option<String>,
    /// The review, once posted: a refusal's included.
    pub review: Option<Review>,
    /// Where the agent's page is, once it is activated.
    pub strategy_slug: Option<String>,
}

/// Something worth announcing: a kill, an allowlist change, a venue going
/// wrong, a vault freezing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedEvent {
    pub at: PrimitiveDateTime,
    pub kind: FeedKind,
    pub title: String,
    pub detail: String,
    /// Where to read more, in the app.
    pub link: Option<String>,
    pub reference: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeedKind {
    Kill,
    ImageProposed,
    ImageRevoked,
    VenueSuspended,
    VenueRestored,
    VaultFrozen,
    StrategyRegistered,
    ReviewPublished,
    ClaimNote,
}

impl FeedKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::ImageProposed => "image proposed",
            Self::ImageRevoked => "image revoked",
            Self::VenueSuspended => "venue suspended",
            Self::VenueRestored => "venue restored",
            Self::VaultFrozen => "vault frozen",
            Self::StrategyRegistered => "agent registered",
            Self::ReviewPublished => "review published",
            Self::ClaimNote => "claim waiting",
        }
    }

    /// Whether it should read as a problem.
    pub fn is_alarming(self) -> bool {
        matches!(
            self,
            Self::Kill
                | Self::ImageRevoked
                | Self::VenueSuspended
                | Self::VaultFrozen
                | Self::ClaimNote
        )
    }
}
