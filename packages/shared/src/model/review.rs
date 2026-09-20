//! The attested review: a pinned model reads a strategy's source and scores
//! whether it is reasonable. A score, never a gate, and never about profit.

use serde::{Deserialize, Serialize};
use time::Date;

/// The fixed categories a finding can fall into. The reviewer image pins
/// them, so a score means the same thing across strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingCategory {
    UnboundedRisk,
    MissingFailureHandling,
    ModelDependence,
    ExternalDependence,
    KnownFailurePattern,
    HiddenBehaviour,
    UnauditedDependencies,
}

impl FindingCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::UnboundedRisk => "Unbounded risk",
            Self::MissingFailureHandling => "Missing failure handling",
            Self::ModelDependence => "Model dependence",
            Self::ExternalDependence => "External dependence",
            Self::KnownFailurePattern => "Known failure pattern",
            Self::HiddenBehaviour => "Hidden behaviour",
            Self::UnauditedDependencies => "Unaudited dependencies",
        }
    }

    /// What the reviewer is looking for in this category.
    pub fn description(self) -> &'static str {
        match self {
            Self::UnboundedRisk => {
                "Position sizing that can grow without a ceiling, leverage the code never \
                 bounds, or what happens to positions after a losing streak."
            }
            Self::MissingFailureHandling => {
                "Rejected orders, venue errors, a host that is down, or a restart with an \
                 empty state cache."
            }
            Self::ModelDependence => {
                "A model's output sizing a position directly, with no bound and no fallback \
                 when inference fails or runs over budget."
            }
            Self::ExternalDependence => {
                "Hosts the strategy depends on, especially any the author controls, and data \
                 feeds whose owner depositors would be trusting."
            }
            Self::KnownFailurePattern => {
                "Shapes that have failed before: martingale or short-volatility sizing, hedges \
                 resting on one oracle, strategies that only work with unbounded capital."
            }
            Self::HiddenBehaviour => {
                "Code paths that behave very differently from the rest, such as logic that \
                 trades differently after a date or a signal."
            }
            Self::UnauditedDependencies => {
                "Components or crates with little use elsewhere, composed into code that holds \
                 depositors' money."
            }
        }
    }

    pub fn all() -> [FindingCategory; 7] {
        [
            Self::UnboundedRisk,
            Self::MissingFailureHandling,
            Self::ModelDependence,
            Self::ExternalDependence,
            Self::KnownFailurePattern,
            Self::HiddenBehaviour,
            Self::UnauditedDependencies,
        ]
    }
}

/// How much a finding weighs on the score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// One thing a careful reader would worry about, tied to a file and line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub category: FindingCategory,
    pub severity: Severity,
    pub summary: String,
    pub detail: String,
    pub file: String,
    pub line: u32,
}

/// A signed review, as it is stored and as the page shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    /// 0 to 100; higher is more reasonable, and it never predicts profit.
    pub score: u8,
    pub findings: Vec<Finding>,
    /// The reviewer's own summary of what the code does, beside the author's.
    pub summary: String,
    pub reviewed: Date,
    /// The image, model and prompt the score came from. Changing any of them
    /// is an image change behind the allowlist timelock.
    pub reviewer_image: String,
    pub model_id_hash: String,
    pub prompt_hash: String,
    pub report_hash: String,
    /// What the review could not see, said plainly.
    pub limits: Vec<String>,
    /// Who paid for it: the vault's tank at creation, or whoever bought it.
    pub bought_by: String,
}

impl Review {
    pub fn band(&self) -> ScoreBand {
        match self.score {
            0..=59 => ScoreBand::Low,
            60..=79 => ScoreBand::Middling,
            _ => ScoreBand::High,
        }
    }

    pub fn count(&self, category: FindingCategory) -> usize {
        self.findings
            .iter()
            .filter(|f| f.category == category)
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreBand {
    Low,
    Middling,
    High,
}
