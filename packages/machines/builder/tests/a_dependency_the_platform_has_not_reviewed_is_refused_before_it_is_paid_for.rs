//! Every dependency is source someone has read. The reviewer reads
//! the upload and nothing else, so a project may depend only on crates inside
//! its own archive, the SDK the build service carries, and the crates.io
//! crates the platform has reviewed itself, listed by name, version and
//! checksum in `config/dependency-allowlist.toml`. The build is then made
//! `--locked --offline` against a vendored copy of exactly that list.
//!
//! Sequence: author A uploads projects that would compile code nobody
//! reviewed: one taking a crate from git, one locking a crates.io crate the
//! platform never listed, one locking a listed crate under another checksum,
//! one with a path dependency outside the upload, and one carrying no
//! `Cargo.lock` at all. Expected: each is refused before it is paid for, and
//! no job is made. A project shaped as the agent template is (the SDK and
//! listed crates, a workspace member beside the agent) is taken.

mod black_box;

use alloy::primitives::Address;

const SHONIN_TOML: &str = "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\n\
                           text = \"x\"\n[drivers.local]\n";

/// A project holding `files`, each a path and its text.
fn project(files: &[(&str, String)]) -> Vec<u8> {
    let root = black_box::scratch().join("project");
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let packed = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    packed
}

/// The listed crate `name`, as the allowlist holds it.
fn listed(name: &str) -> shared::dependencies::Crate {
    shared::dependencies::Allowlist::platform()
        .crates()
        .iter()
        .find(|listed| listed.name == name)
        .unwrap()
        .clone()
}

/// An agent depending on `dependencies` (lines of `[dependencies]`), whose
/// lock file is `lock`, if it has one.
fn agent(dependencies: &str, lock: Option<String>) -> Vec<u8> {
    let mut files = vec![
        (
            "Cargo.toml",
            format!(
                "[package]\nname = \"agent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                 [dependencies]\n{dependencies}"
            ),
        ),
        ("shonin.toml", SHONIN_TOML.to_string()),
        ("src/lib.rs", "pub fn agent() {}\n".to_string()),
    ];
    if let Some(lock) = lock {
        files.push(("Cargo.lock", lock));
    }
    project(&files)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dependency_the_platform_has_not_reviewed_is_refused_before_it_is_paid_for() {
    let git = "0123456789abcdef0123456789abcdef01234567";
    let futures = listed("futures");
    let unlisted = "9".repeat(64);
    let refused = [
        (
            "a git dependency",
            agent(
                &format!(
                    "helper = {{ git = \"https://example.invalid/helper.git\", rev = \"{git}\" }}\n"
                ),
                Some(format!(
                    "{}\n[[package]]\nname = \"helper\"\nversion = \"0.1.0\"\n\
                     source = \"git+https://example.invalid/helper.git?rev={git}#{git}\"\n",
                    black_box::lock(&["agent"], &[])
                )),
            ),
            "git",
        ),
        (
            "an unlisted crates.io crate",
            agent(
                "helper = \"1.0.0\"\n",
                Some(black_box::lock(
                    &["agent"],
                    &[("helper", "1.0.0", &unlisted)],
                )),
            ),
            "helper 1.0.0",
        ),
        (
            "a listed crate under another checksum",
            agent(
                &format!("futures = \"{}\"\n", futures.version),
                Some(black_box::lock(
                    &["agent"],
                    &[("futures", &futures.version, &unlisted)],
                )),
            ),
            "checksum",
        ),
        (
            "a path dependency outside the upload",
            agent(
                "helper = { path = \"../helper\" }\n",
                Some(black_box::lock(&["agent", "helper"], &[])),
            ),
            "outside",
        ),
        (
            "no lock file",
            agent(&format!("futures = \"{}\"\n", futures.version), None),
            "Cargo.lock",
        ),
    ];

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for (what, archive, why) in refused {
        let (status, body) = builder.upload(Address::repeat_byte(0x71), &archive).await;
        assert_eq!(
            status, 400,
            "a project with {what} was taken for review and build, though the reviewer never \
             reads that source: {body}"
        );
        assert!(body.contains(why), "{what} was refused as: {body}");
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");

    // Shaped as the template and the reference agents are: the SDK, a listed
    // crate, and a helper crate of the author's own inside the upload.
    let taken = project(&[
        (
            "Cargo.toml",
            "[workspace]\nresolver = \"3\"\nmembers = [\"agent\", \"helper\"]\n\n\
             [workspace.dependencies]\nshonin-strategy-sdk = \"0.1.0\"\n"
                .to_string(),
        ),
        (
            "agent/Cargo.toml",
            format!(
                "[package]\nname = \"agent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                 [dependencies]\nshonin-strategy-sdk.workspace = true\n\
                 futures = \"{}\"\nhelper = {{ path = \"../helper\" }}\n",
                futures.version
            ),
        ),
        ("agent/shonin.toml", SHONIN_TOML.to_string()),
        ("agent/src/lib.rs", "pub fn agent() {}\n".to_string()),
        (
            "helper/Cargo.toml",
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n".to_string(),
        ),
        ("helper/src/lib.rs", "pub fn help() {}\n".to_string()),
        (
            "Cargo.lock",
            black_box::lock(
                &["agent", "helper", shared::dependencies::SDK],
                &[("futures", &futures.version, &futures.checksum)],
            ),
        ),
    ]);
    let (status, body) = builder.upload(Address::repeat_byte(0x72), &taken).await;
    assert_eq!(
        status, 200,
        "a project depending only on the SDK, a reviewed crate and its own was refused: {body}"
    );
}
