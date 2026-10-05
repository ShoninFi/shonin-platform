//! An archive whose paths would make more than a project holds is refused
//! before anything of it is made.
//!
//! The builder unpacks every upload, unpaid, before checking anything else.
//! Directories hold no bytes, so a size check on files never sees them, and
//! every directory missing along a path is made: one 512-byte header naming
//! a 125-component path makes 125 directories.
//!
//! Sequence: archives of directory entries that go past one bound each: the
//! 512 KB archive of a thousand 125-component paths, one path too deep, too
//! many entries, and too many bytes of paths in all. Each is refused, and
//! nothing is made where it would have been unpacked. An archive of the
//! deepest paths, as many as fit, still unpacks.

use shared::archive::{DEEPEST, MOST_ENTRIES, MOST_PATH_BYTES};
use std::path::{Path, PathBuf};

fn directories(paths: impl IntoIterator<Item = String>) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for path in paths {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, &path, std::io::empty())
            .unwrap();
    }
    builder.into_inner().unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shonin-bounded-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Unpack and say whether it was refused, and whether anything was made.
fn unpacked(name: &str, archive: &[u8]) -> (bool, bool) {
    let into = scratch(name);
    let refused = shared::archive::unpack(archive, &into).is_err();
    let made = into.exists() && std::fs::read_dir(&into).unwrap().next().is_some();
    let _ = std::fs::remove_dir_all(&into);
    (refused, made)
}

fn deep(depth: usize, top: usize) -> String {
    let mut path = format!("{top:05}");
    for _ in 1..depth {
        path.push_str("/a");
    }
    path
}

fn assert_refused(name: &str, archive: &[u8]) {
    let (refused, made) = unpacked(name, archive);
    assert!(refused, "{name}: unpacked");
    assert!(!made, "{name}: something was made before it was refused");
}

#[test]
fn an_archive_makes_no_more_than_a_project_holds() {
    // The shape that made 125,000 directories from 512 KB.
    assert_refused(
        "deep-many",
        &directories((0..1_000).map(|i| format!("{i:05}/{}", "a/".repeat(124)))),
    );
    // A path one component too deep, after a harmless one.
    assert_refused(
        "too-deep",
        &directories(["src".to_string(), deep(DEEPEST + 1, 0)]),
    );
    // Too many entries, each a short name.
    assert_refused(
        "too-many",
        &directories((0..=MOST_ENTRIES).map(|i| format!("{i}"))),
    );
    // Within depth and count, but too many bytes of paths in all.
    let each = deep(DEEPEST, 0).len();
    let over = MOST_PATH_BYTES / each + 1;
    assert!(over <= MOST_ENTRIES);
    assert_refused(
        "too-long",
        &directories((0..over).map(|i| deep(DEEPEST, i))),
    );

    // The deepest paths, as many as fit, still unpack.
    let fits = MOST_PATH_BYTES / each;
    let into = scratch("fits");
    shared::archive::unpack(&directories((0..fits).map(|i| deep(DEEPEST, i))), &into)
        .expect("an archive within every bound unpacks");
    assert!(Path::new(&into).join(deep(DEEPEST, fits - 1)).is_dir());
    let _ = std::fs::remove_dir_all(&into);
}
