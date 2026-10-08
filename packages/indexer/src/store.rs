//! Reading what the build service and the reviewer left behind.
//!
//! The store is content-addressed: the chain holds a hash, and the bytes
//! behind it are here. Nothing in it is believed on its own — a reader with
//! the hash can check the bytes themselves.
//!
//! A store can read through to a public copy (`--store-url`): a file not on
//! disk is fetched from `<base>/sha256-<hash>/<name>`, checked against the
//! hash it is filed under, kept on disk, and from then on read as any file
//! on disk is. A fetched file that doesn't match its hash is never kept.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shared::Report;
use shared::manifest::{Canonical, hash_bytes};
use shared::model::bundle::{Source, SourceFile};

pub struct Store {
    root: PathBuf,
    remote: Option<Remote>,
}

/// The public copy a store reads through to, and what it recently failed to
/// find there.
struct Remote {
    base: String,
    /// Files asked for and not had (absent, unreachable or not matching
    /// their hash), and when: not asked again until [`MISS_RETRY`] has
    /// passed, so a sealed strategy's absent source isn't asked for on every
    /// read of the chain.
    missed: Mutex<HashMap<(String, String), Instant>>,
}

/// How long a file the public copy didn't give is left before it is asked
/// for again.
pub const MISS_RETRY: Duration = Duration::from_secs(300);

/// How many misses are held before those past [`MISS_RETRY`] are let go.
const MOST_MISSES_HELD: usize = 4096;

/// How long one fetch from the public copy may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// The most a fetched file may hold: well past any bundle the builder makes.
const MOST_FETCHED_BYTES: u64 = 256 * 1024 * 1024;

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root, remote: None }
    }

    /// Where the store is kept on disk.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// A store that fetches what it doesn't hold from a public copy at
    /// `base` (no trailing slash needed), checking each file against its
    /// hash before keeping it.
    pub fn with_remote(root: PathBuf, base: &str) -> Self {
        Self {
            root,
            remote: Some(Remote {
                base: base.trim_end_matches('/').to_string(),
                missed: Mutex::new(HashMap::new()),
            }),
        }
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
        let path = self.file(hash, name)?;
        match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(_) => self.fetch(hash, name, &path),
        }
    }

    /// Whether the store holds a file, on disk or (read through) in its
    /// public copy.
    fn holds(&self, hash: &str, name: &str) -> bool {
        self.file(hash, name).is_some_and(|path| path.exists()) || self.bytes(hash, name).is_some()
    }

    /// A file from the public copy, if it has it and it is what its hash
    /// says: kept at `path` and returned. Only the public names are ever
    /// asked for, so nothing sealed is.
    fn fetch(&self, hash: &str, name: &str, path: &std::path::Path) -> Option<Vec<u8>> {
        let remote = self.remote.as_ref()?;
        if !PUBLIC.contains(&name) {
            return None;
        }
        let directory = hash.replace(':', "-");
        let key = (directory.clone(), name.to_string());
        {
            let missed = remote.missed.lock().unwrap_or_else(|e| e.into_inner());
            if missed.get(&key).is_some_and(|at| at.elapsed() < MISS_RETRY) {
                return None;
            }
        }
        let url = format!("{}/{directory}/{name}", remote.base);
        let fetched = blocking(move || get(&url));
        let checked = fetched.filter(|bytes| {
            let good = self.matches(hash, name, bytes);
            if !good {
                tracing::warn!(
                    hash,
                    name,
                    "a file from the public store did not match its hash, and was not kept"
                );
            }
            good
        });
        let Some(bytes) = checked else {
            let mut missed = remote.missed.lock().unwrap_or_else(|e| e.into_inner());
            if missed.len() >= MOST_MISSES_HELD {
                missed.retain(|_, at| at.elapsed() < MISS_RETRY);
            }
            missed.insert(key, Instant::now());
            return None;
        };
        // Written aside and moved into place, so a reader never sees half a
        // file under its name.
        let kept = path.parent().is_some_and(|directory| {
            let aside = directory.join(format!(".{name}.{}.part", std::process::id()));
            std::fs::create_dir_all(directory).is_ok()
                && std::fs::write(&aside, &bytes).is_ok()
                && std::fs::rename(&aside, path).is_ok()
        });
        if !kept {
            tracing::warn!(
                hash,
                name,
                "a file from the public store could not be kept on disk"
            );
        }
        Some(bytes)
    }

    /// Whether fetched bytes are what the hash they are filed under says.
    /// A file the hash is of is checked against it: a source archive, a
    /// bundle and a report by their bytes, a manifest by its canonical
    /// hash. A bundle's component is checked against the component hash its
    /// build recorded, and that record against the bundle it is filed
    /// under; the record's other fields (the toolchain, the build time) are
    /// the builder's word, as they are on disk.
    fn matches(&self, hash: &str, name: &str, bytes: &[u8]) -> bool {
        let Some(digits) = hash
            .strip_prefix("sha256:")
            .or_else(|| hash.strip_prefix("sha256-"))
        else {
            return false;
        };
        let hash = format!("sha256:{digits}");
        match name {
            "source.tar" | "bundle.tar" | "report.json" => hash_bytes(bytes) == hash,
            "manifest.json" => serde_json::from_slice::<Canonical>(bytes)
                .is_ok_and(|canonical| canonical.hash() == hash),
            "hashes.json" => serde_json::from_slice::<serde_json::Value>(bytes)
                .is_ok_and(|hashes| hashes["bundle"].as_str() == Some(hash.as_str())),
            "component.wasm" => self
                .hashes(&hash)
                .and_then(|hashes| hashes["component"].as_str().map(str::to_string))
                .is_some_and(|component| component == hash_bytes(bytes)),
            _ => false,
        }
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
        if self.holds(source_hash, "source.tar") {
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
    ///
    /// Only what is on disk is served: a request for a hash the chain never
    /// named sends nothing to the public copy (whoever asks can ask it
    /// themselves), and what the record names was read through already. A
    /// bundle the record named (its build's record and its published source
    /// on disk) is read through for its component and its archive too, which
    /// the record itself never reads: the executor fetches them here.
    pub fn read_path(&self, hash: &str, name: &str) -> Option<(Vec<u8>, &'static str)> {
        if !PUBLIC.contains(&name) {
            return None;
        }
        let bundle = BUNDLE.contains(&name);
        if bundle && !self.bundle_published(hash) {
            return None;
        }
        let bytes = match self.on_disk(hash, name) {
            Some(bytes) => bytes,
            None if bundle => self.bytes(hash, name)?,
            None => return None,
        };
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
        self.on_disk(bundle_hash, "hashes.json")
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|hashes| hashes["source"].as_str().map(str::to_string))
            .and_then(|source| self.file(&source, "source.tar"))
            .is_some_and(|path| path.is_file())
    }

    /// A file as it is on disk, never read through.
    fn on_disk(&self, hash: &str, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.file(hash, name)?).ok()
    }

    /// Keep a file pushed to the store, only if it is what the hash it is
    /// filed under says (as a fetched file is checked, [`Store::matches`]).
    /// Only the public names are taken, never a sealed one. A component is
    /// checked against the hash its build recorded, so it is taken only once
    /// that record is on disk. A file held already is left as it is.
    pub fn put_checked(&self, hash: &str, name: &str, bytes: &[u8]) -> Put {
        if !is_hash(hash) || !PUBLIC.contains(&name) {
            return Put::Refused;
        }
        let Some(path) = self.file(hash, name) else {
            return Put::Refused;
        };
        if name == "component.wasm" && self.on_disk(hash, "hashes.json").is_none() {
            return Put::OutOfOrder;
        }
        if !self.matches(hash, name, bytes) {
            return Put::Mismatch;
        }
        if path.is_file() {
            return Put::Held;
        }
        let Some(directory) = path.parent() else {
            return Put::Failed;
        };
        // Written aside and moved into place, so a reader never sees half a
        // file under its name.
        let aside = directory.join(format!(".{name}.{}.push", std::process::id()));
        let kept = std::fs::create_dir_all(directory).is_ok()
            && std::fs::write(&aside, bytes).is_ok()
            && std::fs::rename(&aside, &path).is_ok();
        if kept {
            Put::Kept
        } else {
            let _ = std::fs::remove_file(&aside);
            Put::Failed
        }
    }
}

/// What became of a file pushed to the store ([`Store::put_checked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    /// Checked against its hash, and kept.
    Kept,
    /// Checked against its hash, and held already.
    Held,
    /// Not what its hash says: not kept.
    Mismatch,
    /// A component pushed before the record of its build.
    OutOfOrder,
    /// Not a hash, or not a name the store takes.
    Refused,
    /// Checked, and could not be written.
    Failed,
}

/// Run a blocking fetch off the async runtime's own threads: on a thread of
/// its own, with the runtime told this one is blocked where it can be.
fn blocking<T: Send + 'static>(fetch: impl FnOnce() -> T + Send + 'static) -> T {
    let run = move || {
        std::thread::spawn(fetch)
            .join()
            .expect("a fetch from the public store panicked")
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(run)
        }
        _ => run(),
    }
}

/// One file from the public copy: its bytes on a 200, nothing on anything
/// else, or past [`MOST_FETCHED_BYTES`].
fn get(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;

    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .ok()?;
    let reply = match client.get(url).send() {
        Ok(reply) => reply,
        Err(error) => {
            tracing::debug!(%error, "the public store could not be asked for a file");
            return None;
        }
    };
    if !reply.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    reply
        .take(MOST_FETCHED_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MOST_FETCHED_BYTES).then_some(bytes)
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
