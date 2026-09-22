//! Reading what the build service and the reviewer left behind.
//!
//! The store is content-addressed: the chain holds a hash, and the bytes
//! behind it are here. Nothing in it is believed on its own — a reader with
//! the hash can check the bytes themselves.

use std::path::PathBuf;

use shared::manifest::{Manifest, hash_bytes};
use shared::model::bundle::{LogLine, Source, SourceFile};

use crate::read::Report;

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn directory(&self, hash: &str) -> PathBuf {
        self.root.join(hash.replace(':', "-"))
    }

    pub fn bytes(&self, hash: &str, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.directory(hash).join(name)).ok()
    }

    pub fn manifest(&self, bundle_hash: &str) -> Option<Manifest> {
        let bytes = self.bytes(bundle_hash, "manifest.json")?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn report(&self, report_hash: &str) -> Option<Report> {
        let bytes = self.bytes(report_hash, "report.json")?;
        serde_json::from_slice(&bytes).ok()
    }

    /// What the build recorded, or, if it did not, what the bytes say.
    pub fn component_hash(&self, bundle_hash: &str) -> Option<String> {
        self.hashes(bundle_hash)
            .and_then(|hashes| hashes["component"].as_str().map(str::to_string))
            .or_else(|| {
                self.bytes(bundle_hash, "component.wasm")
                    .map(|bytes| hash_bytes(&bytes))
            })
    }

    fn hashes(&self, bundle_hash: &str) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.bytes(bundle_hash, "hashes.json")?).ok()
    }

    /// The recipe a build used, for the page that shows it.
    pub fn toolchain(&self, bundle_hash: &str) -> Option<String> {
        self.hashes(bundle_hash)?["toolchain"]
            .as_str()
            .map(str::to_string)
    }

    /// How long the build took.
    pub fn built_in_seconds(&self, bundle_hash: &str) -> u32 {
        self.hashes(bundle_hash)
            .and_then(|hashes| hashes["seconds"].as_u64())
            .unwrap_or(0) as u32
    }

    pub fn job_log(&self, _slug: &str) -> Vec<LogLine> {
        Vec::new()
    }

    /// Whether the source was published, and where to read it.
    pub fn source(&self, source_hash: &str) -> Source {
        if self.directory(source_hash).join("source.tar").exists() {
            Source::Published {
                repo: format!("the platform's store, at {source_hash}"),
                commit: source_hash.to_string(),
            }
        } else {
            Source::Sealed
        }
    }

    /// The files of a published submission, for the code tab.
    pub fn source_files(&self, source_hash: &str) -> Vec<SourceFile> {
        let Some(archive) = self.bytes(source_hash, "source.tar") else {
            return Vec::new();
        };
        let mut files = Vec::new();
        let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
        let Ok(entries) = reader.entries() else {
            return files;
        };
        for entry in entries.flatten() {
            let Ok(path) = entry.path() else { continue };
            let path = path.display().to_string();
            // The code tab is for reading, so it shows what a person wrote.
            if !(path.ends_with(".rs") || path.ends_with(".toml") || path.ends_with(".lock")) {
                continue;
            }
            let mut entry = entry;
            let mut body = String::new();
            use std::io::Read;
            if entry.read_to_string(&mut body).is_err() {
                continue;
            }
            files.push(SourceFile {
                bytes: body.len() as u32,
                path,
                body,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    /// Serve a file to whoever has its hash.
    pub fn read_path(&self, hash: &str, name: &str) -> Option<(Vec<u8>, &'static str)> {
        let bytes = self.bytes(hash, name)?;
        let kind = if name.ends_with(".json") {
            "application/json"
        } else if name.ends_with(".wasm") {
            "application/wasm"
        } else {
            "application/octet-stream"
        };
        Some((bytes, kind))
    }
}
