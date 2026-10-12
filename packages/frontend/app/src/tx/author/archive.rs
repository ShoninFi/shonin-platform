//! A packed project, read in the browser before anything is uploaded or
//! paid: how big it is unpacked, how many files it holds, and each
//! `shonin.toml` in it with the package beside it, so the page can show
//! what the Reviewer will check the code against and refuse what the build
//! service would refuse, before the author spends anything.

use std::io::{Cursor, Read};

use shared::Manifest;
use shared::archive::{MOST_ENTRIES, MOST_SOURCE_BYTES, MOST_UPLOAD_BYTES};

/// One agent a project holds: its `shonin.toml` and the package beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    /// Where its `shonin.toml` is in the archive.
    pub path: String,
    /// The package named by the `Cargo.toml` beside it, if there is one.
    pub package: Option<String>,
    /// What it declares, or why it can't be read.
    pub manifest: Result<Manifest, String>,
}

/// What a packed project holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// The archive's own size.
    pub bytes: usize,
    pub files: usize,
    /// Every file's size together: what the build service's limit counts.
    pub unpacked: u64,
    pub agents: Vec<Agent>,
}

/// Why a file can't be published as it is. Nothing has been uploaded or
/// charged when any of these is said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not a tar archive.
    NotAnArchive,
    /// Over the source limit, every file counted.
    TooLarge {
        unpacked: u64,
    },
    /// Too big to travel to the build service at all.
    UploadTooLarge {
        bytes: usize,
    },
    TooManyFiles,
    /// No `shonin.toml` within six directories of the top.
    NoManifest,
    /// The build service refuses a project carrying `.cargo`.
    CargoConfig,
}

impl Refusal {
    /// The words under the drop zone. `name` is the file's name.
    pub fn words(&self, name: &str) -> String {
        match self {
            Self::NotAnArchive => format!(
                "{name} isn't a tar archive. Package it with the command below and choose that file."
            ),
            Self::TooLarge { unpacked } => format!(
                "{name} is {} unpacked. The limit is {}, every file counted.",
                size(*unpacked),
                size(MOST_SOURCE_BYTES)
            ),
            Self::UploadTooLarge { bytes } => format!(
                "{name} is {}. The most an upload can be is {}.",
                size(*bytes as u64),
                size(MOST_UPLOAD_BYTES as u64)
            ),
            Self::TooManyFiles => {
                format!("{name} holds more than {MOST_ENTRIES} files, more than any project may.")
            }
            Self::NoManifest => format!(
                "{name} has no shonin.toml. Put one beside your agent's Cargo.toml and package it again."
            ),
            Self::CargoConfig => format!(
                "{name} carries a .cargo directory, which the build service refuses: a project's own Cargo settings could change how it's built."
            ),
        }
    }
}

/// Bytes the way the design writes a file's size: `412 KB`, `2.4 MB`.
pub fn size(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * 1_024;
    if bytes >= MB {
        // Tenths, rounded up, so a file just over the limit never reads as
        // the limit itself.
        let tenths = (bytes * 10).div_ceil(MB);
        if tenths.is_multiple_of(10) {
            format!("{} MB", tenths / 10)
        } else {
            format!("{}.{} MB", tenths / 10, tenths % 10)
        }
    } else if bytes >= KB {
        format!("{} KB", bytes.div_ceil(KB))
    } else {
        format!("{bytes} bytes")
    }
}

/// Read a packed project.
pub fn summarise(bytes: &[u8]) -> Result<Summary, Refusal> {
    // Base64 in a JSON envelope: four bytes for every three, and a little.
    let travels = bytes.len().div_ceil(3) * 4 + 1_024;
    if travels > MOST_UPLOAD_BYTES {
        return Err(Refusal::UploadTooLarge { bytes: bytes.len() });
    }
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive.entries().map_err(|_| Refusal::NotAnArchive)?;
    let mut files = 0usize;
    let mut unpacked = 0u64;
    let mut manifests: Vec<(String, String)> = Vec::new();
    let mut cargos: Vec<(String, String)> = Vec::new();
    let mut cargo_config = false;
    let mut any = false;
    for entry in entries {
        let mut entry = entry.map_err(|_| Refusal::NotAnArchive)?;
        any = true;
        let path = entry
            .path()
            .map_err(|_| Refusal::NotAnArchive)?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if parts.iter().any(|part| part.eq_ignore_ascii_case(".cargo")) {
            cargo_config = true;
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        files += 1;
        if files > MOST_ENTRIES {
            return Err(Refusal::TooManyFiles);
        }
        unpacked = unpacked.saturating_add(entry.size());
        let name = parts.last().copied().unwrap_or_default();
        // The build service looks six directories deep.
        if parts.len() <= 6 && (name == "shonin.toml" || name == "Cargo.toml") {
            let mut text = String::new();
            if entry.read_to_string(&mut text).is_ok() {
                let into = if name == "shonin.toml" {
                    &mut manifests
                } else {
                    &mut cargos
                };
                into.push((path.clone(), text));
            }
        }
    }
    if !any {
        return Err(Refusal::NotAnArchive);
    }
    if unpacked > MOST_SOURCE_BYTES {
        return Err(Refusal::TooLarge { unpacked });
    }
    if cargo_config {
        return Err(Refusal::CargoConfig);
    }
    if manifests.is_empty() {
        return Err(Refusal::NoManifest);
    }
    manifests.sort_by(|a, b| a.0.cmp(&b.0));
    let agents = manifests
        .into_iter()
        .map(|(path, text)| {
            let beside = match path.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/Cargo.toml"),
                None => "Cargo.toml".to_string(),
            };
            let package = cargos
                .iter()
                .find(|(cargo, _)| *cargo == beside)
                .and_then(|(_, text)| package_name(text));
            Agent {
                path,
                package,
                manifest: Manifest::from_toml(&text),
            }
        })
        .collect();
    Ok(Summary {
        bytes: bytes.len(),
        files,
        unpacked,
        agents,
    })
}

/// The package a `Cargo.toml` names in its `[package]` table, if any.
pub fn package_name(cargo: &str) -> Option<String> {
    let table: toml::Table = cargo.parse().ok()?;
    table
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        builder.into_inner().unwrap()
    }

    const MANIFEST: &str = r#"
[vault]
chain = "base"

[description]
title = "Funding basis"
text = "Holds the spot and shorts the perp."
claims = ["Never more than 1x."]

[drivers.local]
"#;

    #[test]
    fn a_workspace_with_several_agents_names_each_with_its_package() {
        let archive = tar(&[
            ("Cargo.toml", b"[workspace]\nmembers = []\n"),
            ("packages/a/Cargo.toml", b"[package]\nname = \"agent-a\"\n"),
            ("packages/a/shonin.toml", MANIFEST.as_bytes()),
            ("packages/a/src/lib.rs", b"// a\n"),
            ("packages/b/Cargo.toml", b"[package]\nname = 'agent-b'\n"),
            ("packages/b/shonin.toml", b"not toml ["),
        ]);
        let summary = summarise(&archive).unwrap();
        assert_eq!(summary.files, 6);
        assert_eq!(summary.agents.len(), 2);
        assert_eq!(summary.agents[0].package.as_deref(), Some("agent-a"));
        let manifest = summary.agents[0].manifest.as_ref().unwrap();
        assert_eq!(manifest.description.title, "Funding basis");
        assert_eq!(manifest.vault.chain, "base");
        assert_eq!(summary.agents[1].package.as_deref(), Some("agent-b"));
        assert!(summary.agents[1].manifest.is_err());
    }

    #[test]
    fn a_project_over_the_source_limit_is_refused_before_anything_is_uploaded() {
        let big = vec![b'a'; (MOST_SOURCE_BYTES + 1) as usize];
        let archive = tar(&[("shonin.toml", MANIFEST.as_bytes()), ("big.rs", &big)]);
        let refused = summarise(&archive).unwrap_err();
        assert!(matches!(refused, Refusal::TooLarge { .. }));
        assert_eq!(
            refused.words("agent.tar"),
            "agent.tar is 2.1 MB unpacked. The limit is 2 MB, every file counted."
        );
        // Exactly at the limit is taken.
        let at = vec![b'a'; MOST_SOURCE_BYTES as usize - MANIFEST.len()];
        let archive = tar(&[("shonin.toml", MANIFEST.as_bytes()), ("big.rs", &at)]);
        assert!(summarise(&archive).is_ok());
    }

    #[test]
    fn a_project_carrying_cargo_settings_or_no_manifest_is_refused() {
        let archive = tar(&[
            ("shonin.toml", MANIFEST.as_bytes()),
            (".cargo/config.toml", b"[patch]\n"),
        ]);
        assert_eq!(summarise(&archive), Err(Refusal::CargoConfig));
        let archive = tar(&[("src/lib.rs", b"//\n")]);
        assert_eq!(summarise(&archive), Err(Refusal::NoManifest));
        assert_eq!(summarise(b"PK\x03\x04 a zip"), Err(Refusal::NotAnArchive));
        assert_eq!(summarise(&[]), Err(Refusal::NotAnArchive));
    }

    #[test]
    fn a_manifest_deeper_than_the_build_service_looks_is_not_found() {
        let archive = tar(&[("a/b/c/d/e/f/shonin.toml", MANIFEST.as_bytes())]);
        assert_eq!(summarise(&archive), Err(Refusal::NoManifest));
        let archive = tar(&[("a/b/c/d/e/shonin.toml", MANIFEST.as_bytes())]);
        assert_eq!(summarise(&archive).unwrap().agents.len(), 1);
    }

    #[test]
    fn sizes_read_as_the_design_writes_them_and_never_round_down_to_a_limit() {
        assert_eq!(size(412 * 1_024), "412 KB");
        assert_eq!(size(2 * 1_024 * 1_024), "2 MB");
        assert_eq!(size(2 * 1_024 * 1_024 + 1), "2.1 MB");
        assert_eq!(size(900), "900 bytes");
    }

    #[test]
    fn a_package_name_is_read_from_the_package_table_only() {
        assert_eq!(
            package_name("[package]\nname = \"one\"\n").as_deref(),
            Some("one")
        );
        assert_eq!(package_name("[workspace]\nmembers = []\n"), None);
        assert_eq!(
            package_name("[dependencies]\nname = \"x\"\n[package]\nname = \"two\"\n").as_deref(),
            Some("two")
        );
    }
}
