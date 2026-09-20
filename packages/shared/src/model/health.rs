//! Health reports: the operational picture of a running strategy, signed by
//! its executor. Evidence of how it is running, not a record of what it did.

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    Ok,
    Warning,
    Error,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "healthy",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One counter, measured over a rolling window, against the thresholds that
/// turn it into a warning or an error.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub name: String,
    pub value: f64,
    /// How to read the value: "an hour", "% of submissions", "days".
    pub unit: String,
    pub warn_at: f64,
    pub error_at: f64,
    /// True when a *lower* number is the problem, as with tank runway.
    pub lower_is_worse: bool,
    pub window: String,
    pub note: Option<String>,
}

impl Signal {
    pub fn level(&self) -> Level {
        let past = |threshold: f64| {
            if self.lower_is_worse {
                self.value <= threshold
            } else {
                self.value >= threshold
            }
        };
        if past(self.error_at) {
            Level::Error
        } else if past(self.warn_at) {
            Level::Warning
        } else {
            Level::Ok
        }
    }
}

/// Something that happened to the running strategy, in order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub at: PrimitiveDateTime,
    pub level: Level,
    pub message: String,
}

/// The whole report for one strategy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub signals: Vec<Signal>,
    pub entries: Vec<Entry>,
    pub updated: PrimitiveDateTime,
    /// The executor key that signed the report.
    pub signed_by: String,
}

impl Health {
    /// The worst level any signal is at: what the vault page shows.
    pub fn level(&self) -> Level {
        self.signals
            .iter()
            .map(Signal::level)
            .max()
            .unwrap_or(Level::Ok)
    }

    pub fn summary(&self) -> String {
        let warnings = self
            .signals
            .iter()
            .filter(|s| s.level() == Level::Warning)
            .count();
        let errors = self
            .signals
            .iter()
            .filter(|s| s.level() == Level::Error)
            .count();
        match (errors, warnings) {
            (0, 0) => "Every signal is inside its thresholds.".to_string(),
            (0, w) => format!("{w} signal{} past its warning threshold.", plural(w)),
            (e, 0) => format!("{e} signal{} past its error threshold.", plural(e)),
            (e, w) => format!(
                "{e} signal{} past its error threshold, {w} past a warning.",
                plural(e)
            ),
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
