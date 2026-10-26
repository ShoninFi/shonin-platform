//! The working-tree verifiable build (`solana:verifiable-build` in
//! taskfiles/contracts.yml, which a dry run or a fork with uncommitted
//! changes runs) copies into what the build container sees only, by rule:
//! the tracked regular files under the build's inputs, each submodule under
//! them as the commit HEAD records it, and new files under them named .rs,
//! .toml or .lock. Any of those named like a key or holding a Solana keypair
//! is refused before anything is copied.
//!
//! The test runs the task's own export lines, read from the Taskfile, in a
//! scratch git repository, so it tests what the task runs. Nothing here
//! touches the repository, Docker or any chain.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shonin-export-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sh(dir: &Path, script: &str) -> Output {
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap()
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(dir: &Path, script: &str) {
    let out = sh(dir, script);
    assert!(out.status.success(), "{script}\n{}", said(&out));
}

/// The task's working-tree export lines, from `source="{{.WORKING_TREE}}"`
/// through the line that marks the export's end.
fn task_export_lines() -> String {
    let yml = std::fs::read_to_string(repo().join("taskfiles/contracts.yml")).unwrap();
    let start = yml
        .find("source=\"{{.WORKING_TREE}}\"")
        .expect("the working-tree export in solana:verifiable-build");
    let end_marker = "# (the working-tree export ends here)";
    let end = start + yml[start..].find(end_marker).expect("the export's end") + end_marker.len();
    yml[start..end].replace("{{.WORKING_TREE}}", "working-tree")
}

/// Runs the export from `top` into a fresh `work/src`, and gives back what it
/// said and every file it copied (relative to `src`).
fn export(root: &Path, top: &Path) -> (Output, Vec<String>) {
    let work = root.join("work");
    let _ = std::fs::remove_dir_all(&work);
    let src = work.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let script = format!(
        "set -eu\nwork='{}'\nsrc='{}'\n{}\n",
        work.display(),
        src.display(),
        task_export_lines()
    );
    let out = sh(top, &script);
    let mut copied = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                copied.push(path.strip_prefix(&src).unwrap().display().to_string());
            }
        }
    }
    copied.sort();
    (out, copied)
}

const KEY: &str = "[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64]";

#[test]
fn the_working_tree_build_copies_no_key_file_into_what_its_container_sees() {
    let root = scratch("key-filter");
    let lib = root.join("lib-repo");
    let top = root.join("repo");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::create_dir_all(&top).unwrap();
    ok(
        &lib,
        "git init -q . && mkdir src && echo 'contract A {}' > src/A.sol && git add . && git commit -qm lib",
    );
    ok(
        &top,
        &format!(
            "git init -q . && mkdir -p contracts/solana/programs/x packages/a .github && \
             echo '[package]' > contracts/solana/programs/x/Cargo.toml && \
             echo 'fn main(){{}}' > packages/a/main.rs && echo 'data' > packages/a/data.txt && \
             echo '[workspace]' > Cargo.toml && echo '' > Cargo.lock && \
             echo 'echo' > .github/fresh-keypairs.sh && \
             printf '.env\\n.shonin/\\ntarget/\\n' > .gitignore && \
             git add . && git commit -qm top && \
             git -c protocol.file.allow=always submodule add -q {lib} contracts/evm/lib/sub && \
             git -c protocol.file.allow=always submodule add -q {lib} packages/sub && \
             git commit -qm subs",
            lib = lib.display()
        ),
    );
    // The owner's keys, not committed, where the build must never see them:
    // inside a submodule's working directory (outside the inputs and under
    // them), and a new key file under the inputs whose name says nothing of
    // keys. And a submodule edit that isn't committed.
    let never = [
        "contracts/evm/lib/sub/owner-keys/shonin_registry-keypair.json",
        "packages/sub/owner/deployer.json",
        "packages/sub/id.json",
        "packages/keys/mainnet-deployer.json",
    ];
    for file in never {
        let file = top.join(file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, KEY).unwrap();
    }
    std::fs::write(top.join("packages/sub/src/A.sol"), "uncommitted").unwrap();
    std::fs::write(top.join("packages/a/new.rs"), "fn new() {}").unwrap();
    std::fs::write(top.join("packages/a/notes.md"), "notes").unwrap();

    let (out, copied) = export(&root, &top);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(
        copied,
        [
            "Cargo.lock",
            "Cargo.toml",
            "contracts/solana/programs/x/Cargo.toml",
            "packages/a/data.txt",
            "packages/a/main.rs",
            "packages/a/new.rs",
            "packages/sub/src/A.sol",
        ],
        "{}",
        said(&out)
    );
    // The submodule is the commit HEAD records, not its working directory.
    let a = std::fs::read_to_string(root.join("work/src/packages/sub/src/A.sol")).unwrap();
    assert_eq!(a.trim(), "contract A {}");
    assert!(said(&out).contains("packages/a/notes.md"), "{}", said(&out));

    // Each of these is refused before anything is copied.
    let pretty_key = KEY
        .replace(',', ",\n  ")
        .replace('[', "[\n  ")
        .replace(']', "\n]\n");
    let refusals: [(&str, &str); 4] = [
        // A key's name in another letter case.
        ("contracts/solana/Shonin_Registry-KEYPAIR.rs", "fn x() {}"),
        ("packages/a/ID.JSON.toml", "[x]"),
        // A keypair in a new file whose name says nothing of keys.
        ("packages/keys/deployer.toml", &pretty_key),
        // A tracked file whose content is now a keypair.
        ("packages/a/data.txt", KEY),
    ];
    for (file, content) in refusals {
        let path = top.join(file);
        let before = std::fs::read(&path).ok();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        let (out, copied) = export(&root, &top);
        assert!(!out.status.success(), "{file}: {}", said(&out));
        assert!(said(&out).contains(file), "{file}: {}", said(&out));
        assert!(copied.is_empty(), "{file}: copied {copied:?}");
        match before {
            Some(before) => std::fs::write(&path, before).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
    }
    // A link in a tracked file's place is refused, never followed.
    let main = top.join("packages/a/main.rs");
    std::fs::remove_file(&main).unwrap();
    std::os::unix::fs::symlink(top.join("packages/keys/mainnet-deployer.json"), &main).unwrap();
    let (out, copied) = export(&root, &top);
    assert!(!out.status.success(), "{}", said(&out));
    assert!(copied.is_empty(), "copied {copied:?}");
    std::fs::remove_file(&main).unwrap();
    std::fs::write(&main, "fn main(){}").unwrap();
    let (out, _) = export(&root, &top);
    assert!(out.status.success(), "{}", said(&out));
    let _ = std::fs::remove_dir_all(&root);
}
