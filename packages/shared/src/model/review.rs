//! The attested review: a pinned model reads an agent's source before it is
//! built and rates it in six areas. A report card, never an overall score,
//! never a gate except for malicious code, and never about profit.

use serde::{Deserialize, Serialize};
use time::Date;

/// The six areas a review rates, in the order the contract stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Area {
    Security,
    FinancialRisk,
    Reliability,
    Reproducibility,
    OutsideInfluence,
    AccurateDescription,
}

impl Area {
    pub const ALL: [Area; 6] = [
        Self::Security,
        Self::FinancialRisk,
        Self::Reliability,
        Self::Reproducibility,
        Self::OutsideInfluence,
        Self::AccurateDescription,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Security => "Security",
            Self::FinancialRisk => "Financial risk",
            Self::Reliability => "Reliability",
            Self::Reproducibility => "Reproducibility",
            Self::OutsideInfluence => "Outside influence",
            Self::AccurateDescription => "Accurate description",
        }
    }

    /// What the reviewer looks for in this area.
    pub fn description(self) -> &'static str {
        match self {
            Self::Security => {
                "Code written to do harm, logic that changes after a date, an address or a signal, \
                 text addressed to the reviewer, and little-used crates."
            }
            Self::FinancialRisk => {
                "Size or leverage with no ceiling, no response to a losing streak, sizing shapes that \
                 have failed before, a model sizing positions unchecked, and markets whose price can \
                 be moved cheaply or whose price feed someone else controls."
            }
            Self::Reliability => {
                "Rejected orders, venue errors, failed reads, and a restart with an empty cache."
            }
            Self::Reproducibility => {
                "A build script or procedural macro that reads the clock, the network, the \
                 environment or a random number."
            }
            Self::OutsideInfluence => {
                "Hosts, feeds and signals the agent follows, especially any its author controls."
            }
            Self::AccurateDescription => {
                "The author's description and claims, compared with what the code does."
            }
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|area| *area == self)
            .expect("every area is listed")
    }
}

/// How one area reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rating {
    LooksFine,
    SomeConcerns,
    SeriousConcerns,
    CouldntTell,
}

impl Rating {
    pub fn label(self) -> &'static str {
        match self {
            Self::LooksFine => "looks fine",
            Self::SomeConcerns => "some concerns",
            Self::SeriousConcerns => "serious concerns",
            Self::CouldntTell => "couldn't tell",
        }
    }

    /// The number the contract stores: `Rating` in `IReviewRegistry.sol`.
    pub fn code(self) -> u8 {
        match self {
            Self::LooksFine => 1,
            Self::SomeConcerns => 2,
            Self::SeriousConcerns => 3,
            Self::CouldntTell => 4,
        }
    }

    pub fn from_code(code: u8) -> Option<Rating> {
        match code {
            1 => Some(Self::LooksFine),
            2 => Some(Self::SomeConcerns),
            3 => Some(Self::SeriousConcerns),
            4 => Some(Self::CouldntTell),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Note,
    Concern,
    Serious,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Concern => "concern",
            Self::Serious => "serious",
        }
    }
}

/// One thing a careful reader would worry about. For sealed source it carries
/// no file, line or quote, so the report can't leak the code it read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub area: Area,
    pub severity: Severity,
    pub summary: String,
    pub detail: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// A signed review, as it is stored and as the page shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Review {
    /// One rating per area, in [`Area::ALL`]'s order.
    pub ratings: [Rating; 6],
    pub findings: Vec<Finding>,
    /// The reviewer's own summary of what the code does, beside the author's.
    pub summary: String,
    pub reviewed: Date,
    /// Whether the code was refused as malicious, and so never built.
    pub refused: bool,
    /// Whether the source is sealed, in which case findings name no file.
    pub sealed: bool,
    /// The image, model and prompt it came from. Changing any of them is an
    /// image change behind the allowlist timelock.
    pub reviewer_image: String,
    pub model_id_hash: String,
    pub prompt_hash: String,
    pub report_hash: String,
    /// What the review could not see, said plainly.
    pub limits: Vec<String>,
    /// The transaction that recorded the review on the chain, where the
    /// indexer knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_tx: Option<String>,
}

impl Review {
    pub fn rating(&self, area: Area) -> Rating {
        self.ratings[area.index()]
    }

    pub fn findings_in(&self, area: Area) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.area == area)
    }

    /// Whether any area reads as a serious concern, which a page shows loudly.
    pub fn is_alarming(&self) -> bool {
        self.refused || self.ratings.contains(&Rating::SeriousConcerns)
    }
}

/// The full report a reviewer writes to the store, under the hash its review
/// on the chain names. A page reads its ratings from the chain and the rest
/// from here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub ratings: [Rating; 6],
    pub refused: bool,
    pub sealed: bool,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub limits: Vec<String>,
    /// Which model, or models, answered.
    pub answered_by: String,
    /// The battery's answers the ratings were computed from, so a reader can
    /// see which answer moved which rating.
    pub answers: serde_json::Value,
}
