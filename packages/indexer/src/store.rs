//! Reading what the build service and the reviewer left behind.
//!
//! The store is content-addressed: the chain holds a hash, and the bytes
//! behind it are here. Nothing in it is believed on its own — a reader with
//! the hash can check the bytes themselves.

use std::path::PathBuf;

use shared::Report;
use shared::manifest::{Canonical, hash_bytes};
use shared::model::bundle::{Source, SourceFile};

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Where a hash's files are kept, if it is a hash: anything else could
    /// name a path outside the store (an absolute path, `..`), so it names
    /// nothing.
    fn directory(&self, hash: &str) -> Option<PathBuf> {
        is_hash(hash).then(|| self.root.join(hash.replace(':', "-")))
    }

    /// A file in a hash's directory. Never a sealed one, however its name is
    /// spelt: on a case-insensitive filesystem `source.SEALED.tar` opens
    /// `source.sealed.tar`, and nothing that reads this store reads sealed
    /// material.
    fn file(&self, hash: &str, name: &str) -> Option<PathBuf> {
        (is_file_name(name) && !is_sealed(name))
            .then(|| self.directory(hash))
            .flatten()
            .map(|directory| directory.join(name))
    }

    pub fn bytes(&self, hash: &str, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.file(hash, name)?).ok()
    }

    /// The manifest the builder recorded: the author's file and what the
    /// builder added. It is filed under its own hash, since one bundle can
    /// be published under several manifests.
    pub fn manifest(&self, manifest_hash: &str) -> Option<Canonical> {
        let bytes = self.bytes(manifest_hash, "manifest.json")?;
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

    /// Whether the source was published, and where to read it.
    pub fn source(&self, source_hash: &str) -> Source {
        if self
            .file(source_hash, "source.tar")
            .is_some_and(|path| path.exists())
        {
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
            // Every file the project holds is shown, so nothing compiled or
            // reviewed is kept from whoever reads the code tab.
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let Ok(path) = entry.path() else { continue };
            let path = path.display().to_string();
            let mut entry = entry;
            let mut raw = Vec::new();
            use std::io::Read;
            if entry.read_to_end(&mut raw).is_err() {
                continue;
            }
            let bytes = raw.len() as u32;
            let body = String::from_utf8(raw).unwrap_or_else(|_| {
                format!("(not text: {bytes} bytes, kept in the source archive)")
            });
            files.push(SourceFile { path, bytes, body });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    /// Serve a file to whoever has its hash: only one of the names meant to
    /// be public, spelt exactly so. A sealed strategy's bundle (its component
    /// and the models it carries) is kept for the attested roles alone, as
    /// its source is, so the bundle's files are served only where its source
    /// was published.
    pub fn read_path(&self, hash: &str, name: &str) -> Option<(Vec<u8>, &'static str)> {
        if !PUBLIC.contains(&name) {
            return None;
        }
        if BUNDLE.contains(&name) && !self.bundle_published(hash) {
            return None;
        }
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

    /// Whether a bundle was built from source that was published, as its
    /// build recorded. A bundle whose build says nothing of its source is
    /// taken for a sealed one.
    fn bundle_published(&self, bundle_hash: &str) -> bool {
        self.hashes(bundle_hash)
            .and_then(|hashes| hashes["source"].as_str().map(str::to_string))
            .is_some_and(|source| matches!(self.source(&source), Source::Published { .. }))
    }
}

/// The files the store serves to anyone: what the builder and the reviewer
/// write for the public to check against the hashes the chain holds.
pub const PUBLIC: [&str; 6] = [
    "component.wasm",
    "bundle.tar",
    "hashes.json",
    "manifest.json",
    "report.json",
    "source.tar",
];

/// The files of a bundle, which are as sealed as the source they were built
/// from.
const BUNDLE: [&str; 2] = ["component.wasm", "bundle.tar"];

/// Whether a file name is sealed material's, however it is spelt.
pub fn is_sealed(name: &str) -> bool {
    name.to_ascii_lowercase().contains(".sealed")
}

/// A hash as the store files it: `sha256:` (or the directory's `sha256-`)
/// and 64 lowercase hex digits, and nothing else.
pub fn is_hash(hash: &str) -> bool {
    hash.strip_prefix("sha256:")
        .or_else(|| hash.strip_prefix("sha256-"))
        .is_some_and(|digits| {
            digits.len() == 64
                && digits
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// A name of one file in a hash's directory, as the builder and the reviewer
/// write them (`component.wasm`, `report.json`, `source.tar`): letters,
/// digits, `.`, `-` and `_`, not starting with a dot.
pub fn is_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
