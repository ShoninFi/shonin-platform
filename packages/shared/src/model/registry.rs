//! The rest of what the app lists: authors, the build queue and the public
//! feed.

use serde::{Deserialize, Serialize};
use time::{Date, PrimitiveDateTime};

use super::bundle::LogLine;

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

/// How far along a build is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    Queued { position: u32 },
    Building { step: String, percent: u32 },
    Succeeded,
    Failed { reason: String },
}

impl JobState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::Building { .. } => "building",
            Self::Succeeded => "succeeded",
            Self::Failed { .. } => "failed",
        }
    }
}

/// One job in the public, first-come-first-served build queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// The strategy being built.
    pub subject: String,
    pub author: String,
    pub state: JobState,
    pub submitted: PrimitiveDateTime,
    pub duration_seconds: u32,
    pub source_hash: String,
    /// Present once the build has produced one.
    pub bundle_hash: Option<String>,
    pub log: Vec<LogLine>,
    /// Where the strategy's page is, once it has one.
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
