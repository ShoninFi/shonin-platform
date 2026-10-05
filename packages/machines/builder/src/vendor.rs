//! The vendored copy of the platform's reviewed crates, which every build is
//! compiled against offline.
//!
//! It is made once, when the builder image is made (`builder --vendor-into
//! <directory>`), from crates.io's own packages, each checked against the
//! checksum the allowlist holds for it; and checked again, against the same
//! list, every time the build service starts. It holds exactly the listed
//! crates, one directory each, as Cargo reads a directory source: the
//! package's files and a `.cargo-checksum.json` naming the package's
//! checksum (which `Cargo.lock` must match) and every file's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use sha2::{Digest, Sha256};
use shared::dependencies::{Allowlist, Crate};

/// The argument that has the build service's own program vendor the
/// allowlisted crates into the directory after it, and stop.
pub const INTO: &str = "--vendor-into";

/// Where crates.io serves a crate's package.
fn package_url(listed: &Crate) -> String {
    format!(
        "https://static.crates.io/crates/{name}/{name}-{version}.crate",
        name = listed.name,
        version = listed.version
    )
}

fn sha256(bytes: &[u8]) -> String {
    alloy::hex::encode(Sha256::digest(bytes))
}

/// Fetch every crate on `allowed` into `into`, which must not exist yet,
/// each checked against its checksum before it is unpacked.
pub async fn fetch(allowed: &Allowlist, into: &Path) -> Result<()> {
    ensure!(
        !into.exists(),
        "{} exists already: vendor into a new directory",
        into.display()
    );
    let making = into.with_extension("making");
    let _ = std::fs::remove_dir_all(&making);
    std::fs::create_dir_all(&making)?;
    let http = reqwest::Client::new();
    for listed in allowed.crates() {
        let package = http
            .get(package_url(listed))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("fetching {} {}", listed.name, listed.version))?
            .bytes()
            .await?;
        ensure!(
            sha256(&package) == listed.checksum,
            "{} {} from crates.io does not match the checksum the allowlist holds",
            listed.name,
            listed.version
        );
        unpack(&package, listed, &making)?;
    }
    std::fs::rename(&making, into)?;
    check(allowed, into)
}

/// Unpack one checked package into `<into>/<name>-<version>`, with the
/// checksums Cargo reads beside it.
fn unpack(package: &[u8], listed: &Crate, into: &Path) -> Result<()> {
    let file = into.join(format!("{}.crate", listed.directory()));
    std::fs::write(&file, package)?;
    // A crate's package is a gzipped tar of `<name>-<version>/...`, already
    // checked against the reviewed checksum.
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&file)
        .arg("-C")
        .arg(into)
        .status()
        .context("running tar")?;
    std::fs::remove_file(&file)?;
    ensure!(
        status.success(),
        "{} {} did not unpack",
        listed.name,
        listed.version
    );
    let directory = into.join(listed.directory());
    let mut files = BTreeMap::new();
    for path in walk(&directory)? {
        let relative = path
            .strip_prefix(&directory)?
            .to_str()
            .ok_or_else(|| anyhow!("a file name in {} is not UTF-8", listed.directory()))?
            .to_string();
        files.insert(relative, sha256(&std::fs::read(&path)?));
    }
    let checksums = serde_json::json!({"files": files, "package": listed.checksum});
    std::fs::write(
        directory.join(".cargo-checksum.json"),
        serde_json::to_vec(&checksums)?,
    )?;
    Ok(())
}

fn walk(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            found.extend(walk(&entry.path())?);
        } else if kind.is_file() {
            found.push(entry.path());
        } else {
            bail!(
                "{} is neither a file nor a directory",
                entry.path().display()
            );
        }
    }
    Ok(found)
}

/// Whether `vendor` holds exactly the crates on `allowed`, each under the
/// checksum the list holds for it: nothing missing, which would fail every
/// build needing it, and nothing more, which a build could reach unreviewed.
pub fn check(allowed: &Allowlist, vendor: &Path) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Checksums {
        package: Option<String>,
    }
    let mut wanted: BTreeMap<String, &Crate> = allowed
        .crates()
        .iter()
        .map(|listed| (listed.directory(), listed))
        .collect();
    for entry in std::fs::read_dir(vendor)
        .with_context(|| format!("reading the vendored crates in {}", vendor.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(listed) = wanted.remove(&name) else {
            bail!(
                "{} holds {name}, which is not on the allowlist",
                vendor.display()
            );
        };
        let checksums: Checksums = serde_json::from_slice(
            &std::fs::read(entry.path().join(".cargo-checksum.json"))
                .with_context(|| format!("{name} has no .cargo-checksum.json"))?,
        )
        .with_context(|| format!("{name}'s .cargo-checksum.json"))?;
        ensure!(
            checksums.package.as_deref() == Some(listed.checksum.as_str()),
            "{name} is vendored under a checksum other than the allowlist's"
        );
    }
    if let Some(missing) = wanted.keys().next() {
        bail!(
            "{} is missing {missing}, and {} other crates on the allowlist: vendor them again \
             into a new directory with --vendor-into",
            vendor.display(),
            wanted.len() - 1
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, checksum: &str) -> String {
        format!("[[crate]]\nname = \"{name}\"\nversion = \"1.0.0\"\nchecksum = \"{checksum}\"\n")
    }

    /// The build service starts only on a vendored copy of exactly the list:
    /// a crate missing, one more, or one under another checksum stops it.
    #[test]
    fn only_a_vendored_copy_of_exactly_the_list_is_built_against() {
        let root = std::env::temp_dir().join(format!("shonin-vendor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let one = "1".repeat(64);
        let two = "2".repeat(64);
        let allowed =
            Allowlist::from_toml(&format!("{}{}", listed("a", &one), listed("b", &two))).unwrap();
        let put = |name: &str, checksum: &str| {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join(".cargo-checksum.json"),
                format!("{{\"files\":{{}},\"package\":\"{checksum}\"}}"),
            )
            .unwrap();
        };
        put("a-1.0.0", &one);
        assert!(check(&allowed, &root).is_err(), "a crate missing was taken");
        put("b-1.0.0", &two);
        check(&allowed, &root).unwrap();
        put("c-1.0.0", &two);
        assert!(
            check(&allowed, &root).is_err(),
            "a crate unlisted was taken"
        );
        std::fs::remove_dir_all(root.join("c-1.0.0")).unwrap();
        put("b-1.0.0", &one);
        assert!(
            check(&allowed, &root).is_err(),
            "a crate under another checksum was taken"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
