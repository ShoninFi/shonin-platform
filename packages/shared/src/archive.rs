//! Packing a project into one archive, the same way every time.
//!
//! A source hash only means something if two people packing the same tree get
//! the same bytes. So the archive is canonical: entries in name order,
//! timestamps zeroed, permissions fixed, no compression, and nothing that
//! isn't source in it at all.
//!
//! Both sides read this: whoever packs a submission and whoever unpacks it to
//! build it. They have to agree byte for byte, so there is one copy of it.

use std::io::{Cursor, Error, Result};
use std::path::{Path, PathBuf};

/// What never goes into an archive: build output, and anything a machine made
/// rather than a person wrote.
const SKIP: &[&str] = &["target", "node_modules"];

/// Whether a name is a person's project file rather than their machine's.
///
/// Nothing beginning with a dot is packed. A published archive is served to
/// anyone who asks for it, and a workspace's dotfiles are where the things
/// that must never be served live: `.env`, credentials, editor state, the
/// local git directory. A Rust project needs none of them to build —
/// `Cargo.toml`, `Cargo.lock`, `src/` and the manifest are not hidden files —
/// so the safe rule is also the simple one, and an author can predict it
/// without reading this list.
fn packable(name: &str) -> bool {
    !name.starts_with('.') && !SKIP.contains(&name)
}

/// Pack a directory into a canonical archive.
pub fn pack(root: &Path) -> Result<Vec<u8>> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();

    let mut archive = tar::Builder::new(Vec::new());
    for relative in &files {
        let full = root.join(relative);
        let body = std::fs::read(&full)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        // Fixed, so the archive says nothing about the machine it was made on.
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        archive.append_data(&mut header, relative, Cursor::new(body))?;
    }
    archive.into_inner()
}

/// Unpack one back out, refusing anything that tries to write outside.
///
/// What is unpacked here is anyone's upload, read before it is paid for, so
/// nothing in it is trusted: only regular files and directories are written,
/// at relative paths with no `..`, each made one component at a time under
/// `into`. A link of either kind, a device, a fifo or anything else refuses
/// the whole archive, and nothing is ever written through a link, whether
/// the archive or anything else put it there. Nothing is expanded either
/// (no compression, no sparse files), so no file's bytes are ever more than
/// the archive's own size.
///
/// Directories hold no bytes, though, and every one missing along a path is
/// made: one header naming a deep path makes a directory per component. So
/// before anything is made, the whole archive's paths are counted, and an
/// archive past [`MOST_ENTRIES`] entries, [`DEEPEST`] components in one path
/// or [`MOST_PATH_BYTES`] bytes of paths in all is refused. Each directory
/// made is a component of some path, so the paths' bytes bound how many.
pub fn unpack(bytes: &[u8], into: &Path) -> Result<()> {
    bounded(bytes)?;
    std::fs::create_dir_all(into)?;
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let parts = inside(&path)?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                directories(into, &parts)?;
            }
            tar::EntryType::Regular => {
                let Some((name, parents)) = parts.split_last() else {
                    return Err(refused("a file with no name", &path));
                };
                let parent = directories(into, parents)?;
                // A new file, never one already there: `create_new` refuses
                // a name that exists, a link included, rather than opening
                // what it points to.
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(parent.join(name))?;
                std::io::copy(&mut entry, &mut file)?;
            }
            other => {
                return Err(refused(&format!("a {other:?} entry"), &path));
            }
        }
    }
    Ok(())
}

/// The most entries an archive may hold. A project is a few dozen files; the
/// largest a person would write is far under this.
pub const MOST_ENTRIES: usize = 10_000;

/// The most a project may carry, unpacked, every file counted. What the
/// reviewer reads in one review ([`MOST_REVIEWED_BYTES`]) is the tighter
/// bound; this one is checked first, on the files as they are.
pub const MOST_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// The most bytes an upload's body may be, at the API and the build service
/// alike: room for a project at the [`MOST_SOURCE_BYTES`] source limit in
/// [`MOST_ENTRIES`] files. Each file costs the archive a 512-byte header and
/// up to 511 bytes of padding, a long name another header and its own
/// padded bytes, and the whole travels as base64 (4/3 its size) in a JSON
/// envelope: (2 MiB + 10,000 × 1 KiB + the long names') × 4/3 is about
/// 17.5 MB. Within this, the build service's own checks say what is wrong.
pub const MOST_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Why an upload past [`MOST_UPLOAD_BYTES`] is turned away, in the author's
/// terms: what a project may hold.
pub fn upload_too_large() -> String {
    format!(
        "the upload is over {} MiB, more than any project the build service takes: a project \
         may carry at most {} MiB of source, in at most {MOST_ENTRIES} files",
        MOST_UPLOAD_BYTES / (1024 * 1024),
        MOST_SOURCE_BYTES / (1024 * 1024),
    )
}

/// The most components one path may have.
pub const DEEPEST: usize = 32;

/// The most bytes all an archive's paths may come to together. Every
/// directory made is at least two of them (a name and a separator), so this
/// bounds what one archive makes on disk however its paths are arranged.
pub const MOST_PATH_BYTES: usize = 64 * 1024;

/// Refuse an archive whose paths would make more than a project does, before
/// anything of it is made.
fn bounded(bytes: &[u8]) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut entries = 0usize;
    let mut path_bytes = 0usize;
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        entries += 1;
        if entries > MOST_ENTRIES {
            return Err(Error::other(format!(
                "the archive holds more than {MOST_ENTRIES} entries, which is never unpacked"
            )));
        }
        if path.components().count() > DEEPEST {
            return Err(refused(
                &format!("a path deeper than {DEEPEST} components"),
                &path,
            ));
        }
        path_bytes += path.as_os_str().len();
        if path_bytes > MOST_PATH_BYTES {
            return Err(Error::other(format!(
                "the archive's paths come to more than {MOST_PATH_BYTES} bytes, which is never unpacked"
            )));
        }
    }
    Ok(())
}

/// A path's parts, if it names somewhere inside the directory it is unpacked
/// into. An archive that names a path outside is not a mistake, so it is
/// refused rather than corrected.
fn inside(path: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    use std::path::Component;

    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => parts.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::other(format!(
                    "the archive tries to write outside itself: {}",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(refused("an entry with no name", path));
    }
    Ok(parts)
}

/// Make each directory in `parts` under `into`, one at a time, and hand back
/// the last. Each must be a directory itself, never a link to one.
fn directories(into: &Path, parts: &[&std::ffi::OsStr]) -> Result<PathBuf> {
    let mut at = into.to_path_buf();
    // The same place, named as the archive names it: a refusal goes back to
    // whoever sent the archive, and says nothing about where it was unpacked.
    let mut named = PathBuf::new();
    for part in parts {
        at.push(part);
        named.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(found) if found.file_type().is_dir() => {}
            Ok(_) => {
                return Err(refused("a path through something not a directory", &named));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&at)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(at)
}

/// A refusal, naming the path as it is inside the archive.
fn refused(what: &str, path: &Path) -> Error {
    Error::other(format!(
        "the archive holds {what}, which is never unpacked: {}",
        path.display()
    ))
}

fn collect(root: &Path, directory: &Path, into: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !packable(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, into)?;
        } else {
            into.push(path.strip_prefix(root).map_err(Error::other)?.to_path_buf());
        }
    }
    Ok(())
}

/// The most bytes of source the reviewer's model reads in one review, as it
/// reads it: every file named, every line numbered (see [`reviewed`]). It is
/// set by the model's context: the review model reads a million tokens at
/// most, a token is never less than a byte, and the prompt, the battery, the
/// description and the report need room beside the source, so 768 KiB fits
/// whatever the text is. Real agents are far smaller. A project that does not
/// fit is refused before it is paid for, rather than reviewed in part.
pub const MOST_REVIEWED_BYTES: usize = 768 * 1024;

/// A packed project as the reviewer's model reads it: every file in it, each
/// named and each line numbered, so a finding can point at one.
///
/// Every file, not only those named `.rs` or `.toml`: the compiler reads
/// whatever file the source names (a module given by `#[path]`, an
/// `include!`, a build script or a library named in `Cargo.toml`), so code
/// may live in a file of any name, and a file the reviewer did not read is
/// code it did not review. So every file must be UTF-8 text, and the whole
/// within [`MOST_REVIEWED_BYTES`]; a project that is not is refused, by the
/// build service before it is paid for and by the reviewer all the same.
///
/// This is run on every upload, before anyone has paid, so it never holds
/// more than the budget: no file is read past it, and the numbered text is
/// counted as it is made and given up on the moment it passes it.
pub fn reviewed(bytes: &[u8]) -> Result<String> {
    use std::io::Read;

    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut out = Numbered::default();
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry.path()?.display().to_string();
        let mut body = Vec::new();
        entry
            .by_ref()
            .take(MOST_REVIEWED_BYTES as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > MOST_REVIEWED_BYTES {
            return Err(past_the_budget());
        }
        let body = String::from_utf8(body).map_err(|_| not_text(&path))?;
        out.file(&path, &body)?;
    }
    out.finish()
}

/// Files as the reviewer's model reads them: each named, each line numbered,
/// and the whole within [`MOST_REVIEWED_BYTES`].
pub fn numbered(files: &[(String, String)]) -> Result<String> {
    let mut out = Numbered::default();
    for (path, body) in files {
        out.file(path, body)?;
    }
    out.finish()
}

/// Numbered text being made, never let past [`MOST_REVIEWED_BYTES`].
#[derive(Default)]
struct Numbered(String);

impl Numbered {
    fn file(&mut self, path: &str, body: &str) -> Result<()> {
        use std::fmt::Write;

        self.room(path.len() + 14)?;
        let _ = write!(self.0, "\n----- {path} -----\n");
        for (number, line) in body.lines().enumerate() {
            let number = number + 1;
            let width = number.checked_ilog10().unwrap_or(0) as usize + 1;
            self.room(width.max(4) + line.len() + 2)?;
            let _ = writeln!(self.0, "{number:>4} {line}");
        }
        Ok(())
    }

    /// Refuse before `more` bytes would take the text past the budget.
    fn room(&self, more: usize) -> Result<()> {
        if self.0.len() + more > MOST_REVIEWED_BYTES {
            return Err(past_the_budget());
        }
        Ok(())
    }

    fn finish(self) -> Result<String> {
        if self.0.is_empty() {
            return Err(Error::other("there is no source to read"));
        }
        Ok(self.0)
    }
}

/// Why a project past [`MOST_REVIEWED_BYTES`] is refused.
fn past_the_budget() -> Error {
    Error::other(format!(
        "the source comes to more than the {} KiB the reviewer reads in one review, as it reads \
         it (each file named, each line numbered): a project is reviewed whole or not at all, \
         so carry less",
        MOST_REVIEWED_BYTES / 1024,
    ))
}

/// Why a file that is not UTF-8 text is refused.
pub fn not_text(path: &str) -> Error {
    Error::other(format!(
        "{path} is not UTF-8 text: the reviewer reads every file a project carries, \
         and cannot read this one, so a project may carry only text"
    ))
}

/// The manifest inside a packed project, without unpacking it. What an author
/// declared is what the build is about, so a page can read it back rather than
/// asking them to type it again. It is the one the builder takes from a project
/// uploaded without naming a package: the only `shonin.toml` within six levels
/// of the top, so none when there are several.
pub fn manifest_in(bytes: &[u8]) -> Option<crate::Manifest> {
    use std::io::Read;

    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut found = None;
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?.into_owned();
        let depth = path
            .components()
            .filter(|part| matches!(part, std::path::Component::Normal(_)))
            .count();
        let named = entry.header().entry_type() == tar::EntryType::Regular
            && path.file_name().is_some_and(|name| name == "shonin.toml")
            && depth <= 6;
        if !named {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let mut text = String::new();
        entry.read_to_string(&mut text).ok()?;
        found = Some(text);
    }
    crate::Manifest::from_toml(&found?).ok()
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
    archive.into_inner()
}

fn add(archive: &mut tar::Builder<Vec<u8>>, name: &str, body: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    archive.append_data(&mut header, name, Cursor::new(body))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// A published archive is served to anybody who asks. Whatever else packing
    /// gets wrong, it must never carry a secret out of an author's workspace.
    #[test]
    fn packing_leaves_every_hidden_file_behind() {
        let root = std::env::temp_dir().join(format!("shonin-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("a scratch project");
        std::fs::create_dir_all(root.join(".private")).expect("a hidden directory");
        std::fs::create_dir_all(root.join("target")).expect("build output");
        std::fs::write(root.join("Cargo.toml"), "[package]").expect("a manifest");
        std::fs::write(root.join("src/lib.rs"), "// code").expect("some code");
        std::fs::write(root.join(".env"), "SECRET=hunter2").expect("a secret");
        std::fs::write(root.join(".private/terms.txt"), "private").expect("a private file");
        std::fs::write(root.join("target/out.bin"), "built").expect("build output");

        let packed = super::pack(&root).expect("it packs");
        let mut archive = tar::Archive::new(std::io::Cursor::new(packed));
        let names: Vec<String> = archive
            .entries()
            .expect("entries")
            .flatten()
            .map(|entry| entry.path().expect("a path").display().to_string())
            .collect();
        let _ = std::fs::remove_dir_all(&root);

        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(names.contains(&"src/lib.rs".to_string()));
        for name in &names {
            assert!(
                !name.starts_with('.') && !name.starts_with("target"),
                "packed {name}, which should never leave the author's machine"
            );
        }
    }

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
        let one = bundle(
            b"wasm",
            &[("b".into(), b"two".to_vec()), ("a".into(), b"one".to_vec())],
        )
        .unwrap();
        let two = bundle(
            b"wasm",
            &[("a".into(), b"one".to_vec()), ("b".into(), b"two".to_vec())],
        )
        .unwrap();
        assert_eq!(one, two);
    }

    /// An archive of `entries`, each a name written into the header as it
    /// is (so a path a careful packer would refuse can still be made), an
    /// entry type, a link name and a body.
    fn raw(entries: &[(&str, tar::EntryType, &str, &[u8])]) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        for (name, kind, link, body) in entries {
            let mut header = tar::Header::new_old();
            let field = &mut header.as_old_mut().name;
            field[..name.len()].copy_from_slice(name.as_bytes());
            let target = &mut header.as_old_mut().linkname;
            target[..link.len()].copy_from_slice(link.as_bytes());
            header.set_entry_type(*kind);
            header.set_size(body.len() as u64);
            header.set_mode(if *kind == tar::EntryType::Directory {
                0o755
            } else {
                0o644
            });
            header.set_cksum();
            archive.append(&header, *body).unwrap();
        }
        archive.into_inner().unwrap()
    }

    /// Unpack `entries` into a fresh directory beside an `outside` one, and
    /// say whether it was refused and whether anything reached `outside`.
    fn unpacked(name: &str, entries: &[(&str, tar::EntryType, &str, &[u8])]) -> (bool, bool) {
        let root = tempdir(name);
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let into = root.join("into");
        let refused = unpack(&raw(entries), &into).is_err();
        let reached = std::fs::read_dir(&outside).unwrap().next().is_some();
        (refused, reached)
    }

    #[test]
    fn a_file_written_through_a_link_the_archive_made_is_refused() {
        let root = tempdir("unpack-symlink");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let entries = [
            (
                "evil",
                tar::EntryType::Symlink,
                outside.to_str().unwrap(),
                &b""[..],
            ),
            ("evil/pwned", tar::EntryType::Regular, "", &b"gotcha"[..]),
        ];
        let refused = unpack(&raw(&entries), &root.join("into")).is_err();
        assert!(refused, "an archive holding a link was unpacked");
        assert!(
            !outside.join("pwned").exists(),
            "a file was written through the link"
        );
        assert!(
            std::fs::symlink_metadata(root.join("into/evil")).is_err(),
            "the link was made"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_already_in_the_destination_is_not_followed() {
        let root = tempdir("unpack-standing-link");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let into = root.join("into");
        std::fs::create_dir_all(&into).unwrap();
        std::os::unix::fs::symlink(&outside, into.join("evil")).unwrap();
        std::os::unix::fs::symlink(outside.join("file"), into.join("file")).unwrap();
        for entries in [
            [("evil/pwned", tar::EntryType::Regular, "", &b"gotcha"[..])],
            [("file", tar::EntryType::Regular, "", &b"gotcha"[..])],
        ] {
            assert!(unpack(&raw(&entries), &into).is_err());
        }
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "a file was written through a link standing in the destination"
        );
    }

    #[test]
    fn a_hard_link_is_refused() {
        let (refused, _) = unpacked(
            "unpack-hardlink",
            &[("passwd", tar::EntryType::Link, "/etc/passwd", &b""[..])],
        );
        assert!(refused, "an archive holding a hard link was unpacked");
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let root = tempdir("unpack-absolute");
        let target = root.join("outside/pwned");
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let name = target.to_str().unwrap().to_string();
        let refused = unpack(
            &raw(&[(&name, tar::EntryType::Regular, "", &b"gotcha"[..])]),
            &root.join("into"),
        )
        .is_err();
        assert!(refused, "an archive naming an absolute path was unpacked");
        assert!(!target.exists());
    }

    #[test]
    fn a_path_climbing_out_is_refused() {
        let (refused, reached) = unpacked(
            "unpack-dotdot",
            &[(
                "../outside/pwned",
                tar::EntryType::Regular,
                "",
                &b"gotcha"[..],
            )],
        );
        assert!(
            refused && !reached,
            "refused {refused}, reached outside {reached}"
        );
    }

    #[test]
    fn a_device_a_fifo_or_a_sparse_file_is_refused() {
        for kind in [
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Fifo,
            tar::EntryType::GNUSparse,
        ] {
            let (refused, _) = unpacked("unpack-device", &[("dev", kind, "", &b""[..])]);
            assert!(refused, "an archive holding a {kind:?} entry was unpacked");
        }
    }

    #[test]
    fn plain_files_and_directories_still_unpack() {
        let root = tempdir("unpack-plain");
        let into = root.join("into");
        let entries = [
            ("src/", tar::EntryType::Directory, "", &b""[..]),
            (
                "src/lib.rs",
                tar::EntryType::Regular,
                "",
                &b"fn main() {}"[..],
            ),
            ("deep/er/file", tar::EntryType::Regular, "", &b"x"[..]),
        ];
        unpack(&raw(&entries), &into).unwrap();
        assert_eq!(
            std::fs::read(into.join("src/lib.rs")).unwrap(),
            b"fn main() {}"
        );
        assert_eq!(std::fs::read(into.join("deep/er/file")).unwrap(), b"x");
    }

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("shonin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
