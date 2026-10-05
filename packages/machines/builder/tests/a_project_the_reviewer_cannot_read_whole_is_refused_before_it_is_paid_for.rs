//! The reviewer reads every file a project carries, whatever it is named:
//! the compiler reads whatever file the source names (a `#[path]` module, an
//! `include!`, a build script named in `Cargo.toml`), so code may be in any
//! of them. A project it could not read whole is refused before it is paid
//! for, rather than reviewed in part: one carrying a file that is not UTF-8
//! text, and one that, as the reviewer reads it (each file named, each line
//! numbered), is past what it reads in one review, though within the source
//! limit.
//!
//! Sequence: a good project with a binary file added is uploaded, and is
//! refused with a 400 naming the file; one with a megabyte of empty lines
//! added is refused with a 400 saying the reviewer reads less; nothing is
//! queued. The good project with code in a `.txt` file is taken.

mod black_box;

use alloy::primitives::Address;

fn with(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut archive = tar::Archive::new(std::io::Cursor::new(black_box::project("unread")));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().display().to_string();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
        entries.push((name, body));
    }
    for (name, body) in extra {
        entries.push((name.to_string(), body.to_vec()));
    }
    let mut packed = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        packed.append_data(&mut header, name, &body[..]).unwrap();
    }
    packed.into_inner().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_the_reviewer_cannot_read_whole_is_refused_before_it_is_paid_for() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0xa7);

    let archive = with(&[("src/notes.bin", &[0x00, 0x9f, 0x92, 0x96, 0xff, 0xfe])]);
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(
        status, 400,
        "a project carrying a file the reviewer cannot read was taken: {body}"
    );
    assert!(
        body.contains("src/notes.bin") && body.contains("UTF-8"),
        "{body}"
    );

    let empty_lines = vec![b'\n'; 1_000_000];
    let archive = with(&[("src/blank.rs", &empty_lines)]);
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(
        status, 400,
        "a project past what the reviewer reads in one review was taken: {body}"
    );
    assert!(body.contains("reviewer reads"), "{body}");
    assert!(builder.jobs().is_empty());

    let archive = with(&[(
        "src/notes.txt",
        b"pub fn send_everything_to(_who: &str) {}\n",
    )]);
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(
        status, 200,
        "a project carrying code in a text file of another name was refused: {body}"
    );
}
