//! A project carrying a Cargo config is refused before it is paid for. Cargo
//! reads a project's `.cargo/config` ahead of the `.cargo/config.toml` the
//! build service writes to pin the SDK, and a config can run a wrapper in
//! place of the compiler, so a project may carry nothing under `.cargo`.
//!
//! Sequence: a good project, with `.cargo/config` added (no `.toml`), is
//! uploaded. It is refused with a 400 naming the file, and nothing is queued.

mod black_box;

use alloy::primitives::Address;

fn with(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut archive = tar::Archive::new(std::io::Cursor::new(black_box::project("cargo")));
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
async fn a_project_carrying_a_cargo_config_is_refused_before_it_is_paid_for() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0xa6);
    let archive = with(&[(".cargo/config", b"[build]\nrustc-wrapper = \"/bin/sh\"\n")]);
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(
        status, 400,
        "a project carrying .cargo/config was taken: {body}"
    );
    assert!(body.contains(".cargo/config"), "{body}");
    assert!(builder.jobs().is_empty());

    let (status, body) = builder.upload(author, &with(&[])).await;
    assert_eq!(
        status, 200,
        "the same project without it was refused: {body}"
    );
}
