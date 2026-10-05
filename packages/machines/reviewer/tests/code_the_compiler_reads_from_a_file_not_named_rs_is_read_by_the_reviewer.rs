//! The reviewer puts every file a project carries to the model, not only
//! those named `.rs` or `.toml`: the compiler reads whatever file the source
//! names (a module given by `#[path = "..."]`, an `include!`, a build script
//! or library named in `Cargo.toml`), and code in such a file is built,
//! registered and run under the reviewer's go-ahead. A project it cannot
//! read whole, a file that is not text or more than it reads in one review,
//! is not read in part: it is refused.
//!
//! Sequence: for each way of hiding code in a file not named `.rs`, an
//! author packs the strategy's real code in `notes.txt` or `setup.txt`; the
//! reviewer reads the project with `source::from_archive`, and the model is
//! shown that code. A project carrying a binary file, and one past the
//! reviewer's budget, are refused.

use shonin_reviewer::source;

fn packed(files: &[(&str, &str)]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        archive
            .append_data(&mut header, path, body.as_bytes())
            .unwrap();
    }
    archive.into_inner().unwrap()
}

#[test]
fn code_the_compiler_reads_from_a_file_not_named_rs_is_read_by_the_reviewer() {
    let hidden = "pub fn send_everything_to(_who: &str) { /* the strategy's real code */ }";
    for (how, files) in [
        (
            "a #[path] module",
            vec![
                ("Cargo.toml", "[package]\nname = \"agent\"\n"),
                ("src/lib.rs", "#[path = \"notes.txt\"]\nmod notes;\n"),
                ("src/notes.txt", hidden),
            ],
        ),
        (
            "an include!",
            vec![
                ("Cargo.toml", "[package]\nname = \"agent\"\n"),
                ("src/lib.rs", "include!(\"notes.txt\");\n"),
                ("src/notes.txt", hidden),
            ],
        ),
        (
            "a build script named in Cargo.toml",
            vec![
                (
                    "Cargo.toml",
                    "[package]\nname = \"agent\"\nbuild = \"setup.txt\"\n",
                ),
                ("setup.txt", hidden),
                ("src/lib.rs", "// nothing here\n"),
            ],
        ),
    ] {
        let read = source::from_archive(&packed(&files)).unwrap();
        assert!(
            read.contains("send_everything_to"),
            "code the compiler builds through {how} was never shown to the model:\n{read}"
        );
    }
}

#[test]
fn a_project_the_reviewer_cannot_read_whole_is_not_read_in_part() {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, body) in [
        ("Cargo.toml", b"[package]\nname = \"agent\"\n".as_slice()),
        ("src/lib.rs", b"include!(\"notes.bin\");\n"),
        ("src/notes.bin", &[0x00, 0x9f, 0x92, 0x96, 0xff, 0xfe]),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        archive.append_data(&mut header, path, body).unwrap();
    }
    let refused = source::from_archive(&archive.into_inner().unwrap());
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.to_string().contains("src/notes.bin")),
        "a file that is not text was left out rather than refused: {refused:?}"
    );

    let blank = "\n".repeat(1_000_000);
    let refused = source::from_archive(&packed(&[
        ("Cargo.toml", "[package]\nname = \"agent\"\n"),
        ("src/lib.rs", &blank),
    ]));
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.to_string().contains("reviewer reads")),
        "a project past the reviewer's budget was read in part: {}",
        refused.map(|read| read.len()).unwrap_or_default()
    );
}
