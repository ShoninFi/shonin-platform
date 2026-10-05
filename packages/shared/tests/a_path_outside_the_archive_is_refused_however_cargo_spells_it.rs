//! A path outside the archive is refused before payment, so that
//! every line the build compiles is source the reviewer read or the platform
//! reviewed. Cargo compiles code from outside a package through more than a
//! dependency's, a patch's or a workspace's path:
//!
//! - a target path: `[lib] path`, `[[bin]] path` or `package.build` naming a
//!   file outside the archive;
//! - the deprecated `build_dependencies` / `dev_dependencies` table names,
//!   which Cargo still reads on edition 2021 (with a warning), with a `path`
//!   outside the archive, at the top or under `[target.*]`. The lock entry is
//!   then a path package with no source, which the lock check passes whenever
//!   any `Cargo.toml` in the archive declares a package of that name (a dummy
//!   one will do).
//!
//! Each project below is refused, the error naming the path outside; the same
//! project with its paths inside the archive is taken.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Each project's own scratch directory, so tests running at once never
/// share one.
static SCRATCH: AtomicU64 = AtomicU64::new(0);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-outside-{name}-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn packed(name: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let archive = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    archive
}

const SHONIN_TOML: &str = "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\n\
                           text = \"x\"\n[drivers.local]\n";

fn lock(packages: &[&str]) -> String {
    let mut lock = "version = 4\n".to_string();
    for package in packages {
        lock.push_str(&format!(
            "\n[[package]]\nname = \"{package}\"\nversion = \"0.1.0\"\n"
        ));
    }
    lock
}

fn agent(cargo_toml_tail: &str, extra: &[(&str, &str)], locked: &[&str]) -> Vec<u8> {
    let cargo = format!(
        "[package]\nname = \"agent\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{cargo_toml_tail}"
    );
    let lock = lock(locked);
    let mut files = vec![
        ("Cargo.toml", cargo.as_str()),
        ("Cargo.lock", lock.as_str()),
        ("shonin.toml", SHONIN_TOML),
        ("src/lib.rs", "pub fn agent() {}\n"),
    ];
    files.extend_from_slice(extra);
    packed("outside", &files)
}

#[test]
fn a_path_outside_the_archive_is_refused_however_cargo_spells_it() {
    let allowed = shared::dependencies::Allowlist::platform();
    let dummy = "[package]\nname = \"evil\"\nversion = \"0.1.0\"\n";
    let cases = [
        (
            "[lib] path outside the archive",
            agent(
                "\n[lib]\npath = \"../../outside/lib.rs\"\n",
                &[],
                &["agent"],
            ),
        ),
        (
            "package.build outside the archive",
            agent("build = \"../../outside/build.rs\"\n", &[], &["agent"]),
        ),
        (
            "[[bin]] path outside the archive",
            agent(
                "\n[[bin]]\nname = \"x\"\npath = \"/tmp/outside/main.rs\"\n",
                &[],
                &["agent"],
            ),
        ),
        (
            "[build_dependencies] path outside the archive",
            agent(
                "\n[build_dependencies]\nevil = { path = \"../../outside\" }\n",
                &[("unused/Cargo.toml", dummy), ("unused/src/lib.rs", "")],
                &["agent", "evil"],
            ),
        ),
        (
            "[target.*.build_dependencies] path outside the archive",
            agent(
                "\n[target.'cfg(all())'.build_dependencies]\nevil = { path = \"/opt/outside\" }\n",
                &[("unused/Cargo.toml", dummy), ("unused/src/lib.rs", "")],
                &["agent", "evil"],
            ),
        ),
    ];
    let mut accepted = Vec::new();
    for (what, archive) in &cases {
        match shared::dependencies::checked(archive, "shonin.toml", &allowed) {
            Ok(_) => accepted.push(*what),
            Err(why) => assert!(why.contains("outside"), "{what}: {why}"),
        }
    }
    assert!(
        accepted.is_empty(),
        "projects compiling code from outside the archive were accepted: {accepted:?}"
    );
}

#[test]
fn the_same_paths_inside_the_archive_are_taken() {
    let allowed = shared::dependencies::Allowlist::platform();
    let helper = "[package]\nname = \"helper\"\nversion = \"0.1.0\"\n";
    let archive = agent(
        "build = \"build.rs\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"x\"\n\
         path = \"./src/main.rs\"\n\n[build_dependencies]\nhelper = { path = \"helper\" }\n\
         \n[target.'cfg(all())'.build_dependencies]\nhelper = { path = \"helper\" }\n\
         \n[package.metadata.anything]\npath = \"/not/read/by/cargo\"\n",
        &[
            ("build.rs", "fn main() {}\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("helper/Cargo.toml", helper),
            ("helper/src/lib.rs", ""),
        ],
        &["agent", "helper"],
    );
    let taken = shared::dependencies::checked(&archive, "shonin.toml", &allowed);
    assert!(taken.is_ok(), "{taken:?}");
}
