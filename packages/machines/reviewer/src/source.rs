//! What the reviewer reads.
//!
//! One definition, used by the reviewer and by the calibration harness, so a
//! comparison between two models is a comparison over exactly the bytes a real
//! review would see. A harness that fed them something tidier would be
//! measuring the harness.

use std::path::Path;

use anyhow::{Result, bail};

/// A file the reviewer reads. Anything else in the project is carried, hashed
/// and published, but not put to a model.
fn worth_reading(path: &str) -> bool {
    path.ends_with(".rs") || path.ends_with(".toml")
}

/// The source as a model sees it: each file named, each line numbered, so a
/// finding can point at one.
pub fn numbered(files: &[(String, String)]) -> Result<String> {
    let mut out = String::new();
    for (path, body) in files {
        out.push_str(&format!("\n----- {path} -----\n"));
        for (number, line) in body.lines().enumerate() {
            out.push_str(&format!("{:>4} {line}\n", number + 1));
        }
    }
    if out.is_empty() {
        bail!("there is no source to read");
    }
    Ok(out)
}

/// The source inside a packed project.
pub fn from_archive(archive: &[u8]) -> Result<String> {
    let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
    let mut files = Vec::new();
    for entry in reader.entries()?.flatten() {
        let path = entry.path()?.display().to_string();
        if !worth_reading(&path) {
            continue;
        }
        let mut entry = entry;
        let mut body = String::new();
        use std::io::Read;
        if entry.read_to_string(&mut body).is_err() {
            continue;
        }
        files.push((path, body));
    }
    numbered(&files)
}

/// The source of a project on disk, for a comparison that has not been
/// published. The same files, in the same order a packed project holds them,
/// so this and a real review read the same thing.
pub fn from_dir(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    numbered(&files)
}

fn collect(root: &Path, at: &Path, files: &mut Vec<(String, String)>) -> Result<()> {
    for entry in std::fs::read_dir(at)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // What a pack would leave out anyway.
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            if worth_reading(&relative)
                && let Ok(body) = std::fs::read_to_string(&path)
            {
                files.push((relative, body));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_is_numbered_from_one() {
        let text = numbered(&[("a.rs".to_string(), "fn main() {}\nlet x = 1;".to_string())])
            .expect("some source");
        assert!(text.contains("----- a.rs -----"));
        assert!(text.contains("   1 fn main() {}"));
        assert!(text.contains("   2 let x = 1;"));
    }

    #[test]
    fn nothing_to_read_is_an_error_rather_than_an_empty_review() {
        assert!(numbered(&[]).is_err());
    }
}
