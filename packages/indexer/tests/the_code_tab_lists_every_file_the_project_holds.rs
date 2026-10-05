//! The code tab shows the project that was compiled and reviewed. Every file
//! in the published archive is listed, whatever its name ends in: a build
//! script's data, a README, a JSON fixture the strategy reads. A file that
//! is not text is listed with its size and a note in place of its bytes.

use std::path::PathBuf;

use indexer::store::Store;

const DIGITS: &str = "4e7a1c9b2d3f50617a8b9c0d1e2f30415263748596a7b8c9d0e1f2a3b4c5d6e7";

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-code-tab-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
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

#[test]
fn every_file_the_project_holds_is_listed() {
    let root = scratch();
    let directory = root.join(format!("sha256-{DIGITS}"));
    std::fs::create_dir_all(&directory).unwrap();
    let binary: &[u8] = &[0xff, 0xfe, 0x00, 0x9f];
    std::fs::write(
        directory.join("source.tar"),
        archive(&[
            ("src/lib.rs", b"pub fn run() {}"),
            ("Cargo.toml", b"[package]"),
            ("README.md", b"# An agent"),
            ("data/pairs.json", b"[]"),
            ("data/weights.bin", binary),
        ]),
    )
    .unwrap();

    let files = Store::new(root).source_files(&format!("sha256:{DIGITS}"));

    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Cargo.toml",
            "README.md",
            "data/pairs.json",
            "data/weights.bin",
            "src/lib.rs"
        ]
    );
    let readme = files.iter().find(|file| file.path == "README.md").unwrap();
    assert_eq!((readme.body.as_str(), readme.bytes), ("# An agent", 10));
    let weights = files
        .iter()
        .find(|file| file.path == "data/weights.bin")
        .unwrap();
    assert_eq!(weights.bytes, 4);
    assert!(!weights.body.contains('\u{fffd}'));
    assert!(weights.body.contains("not text"));
}
