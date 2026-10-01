//! What an author commits: the source, and the bundle built from it.

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
    pub source: Source,
    pub source_files: Vec<SourceFile>,
    pub built_in_seconds: u32,
}
