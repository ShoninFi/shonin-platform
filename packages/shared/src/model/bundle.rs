//! What an author commits: the source, the bundle built from it, and the
//! allowances it declares.

use serde::{Deserialize, Serialize};

/// Whether anyone but the builder and the reviewer can read the source.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Anyone can read it, rebuild it and compare hashes.
    Published { repo: String, commit: String },
    /// Stored encrypted; the attested builder and the reviewer still read it.
    Sealed,
}

impl Source {
    pub fn is_published(&self) -> bool {
        matches!(self, Self::Published { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Published { .. } => "source published",
            Self::Sealed => "source sealed",
        }
    }
}

/// A trained model shipped inside a bundle. Weights are data, not source: the
/// review judges how the code uses it, never what it learned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelFile {
    /// The name the component passes to `infer`.
    pub name: String,
    pub opset: u32,
    pub hash: String,
    pub size_mib: f64,
    /// What the author says it is for.
    pub purpose: String,
}

/// One file of the submitted project, for the source browser.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub bytes: u32,
    pub body: String,
}

/// A line of a build log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub text: String,
    pub level: LogLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Plain,
    Warn,
    Error,
}

/// The compute a strategy may use, declared in its manifest, enforced as caps
/// and metered to its vault's tank.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allowances {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    /// A region near a named venue, or anywhere.
    pub placement: String,
}

/// The bundle: one component, the models it calls, and the build that made it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_hash: String,
    pub component_hash: String,
    pub source_hash: String,
    pub manifest_hash: String,
    pub world: String,
    pub toolchain: String,
    pub builder_image: String,
    pub models: Vec<ModelFile>,
    pub allowances: Allowances,
    /// Names of the sealed secrets the strategy reads. Values are never shown.
    pub secrets: Vec<String>,
    /// The config passed to `run`, part of what was hashed.
    pub config: Vec<(String, String)>,
    pub source: Source,
    pub source_files: Vec<SourceFile>,
    pub build_log: Vec<LogLine>,
    pub built_in_seconds: u32,
}
