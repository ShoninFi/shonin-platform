//! Packing a project into one archive, the same way every time.
//!
//! A source hash only means something if two people packing the same tree get
//! the same bytes. So the archive is canonical: entries in name order,
//! timestamps zeroed, permissions fixed, no compression, and nothing that
//! isn't source in it at all.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What never goes into an archive: build output, local settings, anything a
/// machine made rather than a person wrote.
const SKIP: &[&str] = &["target", ".git", ".cargo", "node_modules", ".shonin", ".DS_Store"];

/// Pack a directory into a canonical archive.
pub fn pack(root: &Path) -> Result<Vec<u8>> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();

    let mut archive = tar::Builder::new(Vec::new());
    for relative in &files {
        let full = root.join(relative);
        let body = std::fs::read(&full).with_context(|| format!("reading {}", full.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        // Fixed, so the archive says nothing about the machine it was made on.
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        archive
            .append_data(&mut header, relative, Cursor::new(body))
            .with_context(|| format!("adding {}", relative.display()))?;
    }
    archive.into_inner().context("finishing the archive")
}

/// Unpack one back out, refusing anything that tries to write outside.
pub fn unpack(bytes: &[u8], into: &Path) -> Result<()> {
    std::fs::create_dir_all(into).with_context(|| format!("making {}", into.display()))?;
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    for entry in archive.entries().context("reading the archive")? {
        let mut entry = entry.context("reading an entry")?;
        let path = entry.path().context("an entry's name")?.into_owned();
        if path.is_absolute() || path.components().any(|part| part.as_os_str() == "..") {
            bail!("the archive tries to write outside itself: {}", path.display());
        }
        let target = into.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("writing {}", target.display()))?;
    }
    Ok(())
}

fn collect(root: &Path, directory: &Path, into: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, into)?;
        } else {
            into.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

/// The bundle: the component, and the models it calls, in one archive.
pub fn bundle(component: &[u8], models: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());
    add(&mut archive, "component.wasm", component)?;
    let mut sorted: Vec<&(String, Vec<u8>)> = models.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, body) in sorted {
        add(&mut archive, &format!("models/{name}.onnx"), body)?;
    }
    archive.into_inner().context("finishing the bundle")
}

fn add(archive: &mut tar::Builder<Vec<u8>>, name: &str, body: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    archive
        .append_data(header, name, Cursor::new(body))
        .with_context(|| format!("adding {name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"fn main() {}\n").unwrap();
        std::fs::write(root.join("target/debug/junk"), b"not source").unwrap();
    }

    #[test]
    fn the_same_tree_packs_to_the_same_bytes() {
        let one = tempdir("pack-one");
        let two = tempdir("pack-two");
        tree(&one);
        tree(&two);
        assert_eq!(pack(&one).unwrap(), pack(&two).unwrap());
    }

    #[test]
    fn build_output_is_left_behind() {
        let root = tempdir("pack-skip");
        tree(&root);
        let packed = pack(&root).unwrap();
        let text = String::from_utf8_lossy(&packed);
        assert!(text.contains("src/lib.rs"));
        assert!(!text.contains("junk"));
    }

    #[test]
    fn an_archive_unpacks_to_what_went_in() {
        let root = tempdir("pack-round");
        tree(&root);
        let packed = pack(&root).unwrap();
        let out = tempdir("pack-out");
        unpack(&packed, &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("src/lib.rs")).unwrap(),
            b"fn main() {}\n"
        );
    }

    #[test]
    fn a_bundle_orders_its_models() {
        let one = bundle(b"wasm", &[("b".into(), b"two".to_vec()), ("a".into(), b"one".to_vec())]).unwrap();
        let two = bundle(b"wasm", &[("a".into(), b"one".to_vec()), ("b".into(), b"two".to_vec())]).unwrap();
        assert_eq!(one, two);
    }

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("shonin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
