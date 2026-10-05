//! An archive `shared::archive::unpack` refuses is refused in words that name
//! the path inside the archive and nothing about where it was being unpacked.
//!
//! The builder answers an upload it can't unpack with the refusal, and the
//! API relays it to whoever sent it, so a refusal naming the directory it was
//! unpacked into would hand anyone the build service's own layout.
//!
//! Sequence: an archive holding a file `a`, then a file `a/b`. Unpacking
//! writes `a`, then finds a file where `a/b` needs a directory, and refuses.

use std::path::PathBuf;

fn raw(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive.append_data(&mut header, name, *body).unwrap();
    }
    archive.into_inner().unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-{name}-{}-a-server-directory",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_refused_archive_names_only_the_path_inside_it() {
    let into = scratch("refused-archive").join("upload-0123456789abcdef");
    let archive = raw(&[("a", b"x"), ("a/b", b"y")]);
    let error = shared::archive::unpack(&archive, &into)
        .expect_err("a path through a file is refused")
        .to_string();
    let _ = std::fs::remove_dir_all(into.parent().unwrap());
    assert!(
        !error.contains(&*into.parent().unwrap().to_string_lossy()),
        "the refusal names the directory it was unpacked into: {error}"
    );
    assert!(
        error.ends_with(": a"),
        "the refusal does not name the path inside the archive: {error}"
    );
}
