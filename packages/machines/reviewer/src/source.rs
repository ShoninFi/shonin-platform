//! What the reviewer reads.
//!
//! One definition, used by the reviewer and by the calibration harness, so a
//! comparison between two models is a comparison over exactly the bytes a real
//! review would see. A harness that fed them something tidier would be
//! measuring the harness.

use std::path::Path;

use anyhow::{Result, anyhow};

/// The source as a model sees it: every file named, each line numbered, so a
/// finding can point at one (see [`shared::archive::numbered`]).
pub fn numbered(files: &[(String, String)]) -> Result<String> {
    shared::archive::numbered(files).map_err(|error| anyhow!(error))
}

/// The source inside a packed project: every file in it, since code can live
/// in a file of any name, and none that is not text. A project that can't be
/// read whole is not read at all (see [`shared::archive::reviewed`]).
pub fn from_archive(archive: &[u8]) -> Result<String> {
    shared::archive::reviewed(archive).map_err(|error| anyhow!(error))
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
            let body = String::from_utf8(std::fs::read(&path)?)
                .map_err(|_| anyhow!(shared::archive::not_text(&relative)))?;
            files.push((relative, body));
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
