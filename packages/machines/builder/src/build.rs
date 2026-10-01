//! Compiling a submission into a bundle.
//!
//! The recipe is pinned and the same for everyone: the toolchain the
//! repository names, the SDK the build service carries, `--locked` so the
//! dependency graph is the author's own, and the component target.
//!
//! It is built once. What a depositor relies on is that an attested image ran
//! this recipe over that source and signed what came out, and the image
//! cannot misreport what it did. Reproducing a bundle elsewhere is a property
//! of a hermetic recipe, and a project that reaches for a clock or a random
//! number while it compiles is something the reviewer reads.
//!
//! A build keeps no log. A compiler's output can quote file names, paths and
//! source, which would leak sealed code, so a build ends with an outcome and
//! nothing else; an author sees why a build failed by running the same recipe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use shared::Failure;
use shared::manifest::{Manifest, hash_bytes};

/// The recipe. Every submission is built with exactly this, whatever it asked
/// for, so the same source builds the same way for everyone.
pub const TOOLCHAIN: &str = "rust 1.98.1 · wasm32-wasip2";

/// The world every component must implement.
pub const WORLD: &str = "shonin:agent@0.1.0";

/// The longest a build may run.
pub const TIME_LIMIT: Duration = Duration::from_secs(20 * 60);

/// The most source a submission may carry, unpacked, not counting its models.
pub const SIZE_LIMIT: u64 = 2 * 1024 * 1024;

/// What a finished build produced.
pub struct Built {
    pub component: Vec<u8>,
    pub bundle: Vec<u8>,
    pub component_hash: String,
    pub bundle_hash: String,
    pub seconds: u32,
}

/// Where the agent's manifest is in an unpacked project: the one `shonin.toml`
/// there is, or the one beside the named package's `Cargo.toml`.
pub fn find_manifest(source: &Path, package: Option<&str>) -> Result<PathBuf, String> {
    let found: Vec<PathBuf> = walk(source, 6)
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "shonin.toml"))
        .collect();
    match package {
        Some(package) => found
            .into_iter()
            .find(|path| package_of(path).as_deref() == Some(package))
            .ok_or_else(|| format!("no shonin.toml beside a package called {package}")),
        None => match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err("no shonin.toml in the project".into()),
            _ => Err("more than one shonin.toml in the project: name the package".into()),
        },
    }
}

pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text =
        std::fs::read_to_string(path).map_err(|_| "shonin.toml could not be read".to_string())?;
    Manifest::from_toml(&text)
}

/// The package a manifest belongs to: the name in the `Cargo.toml` beside it.
pub fn package_of(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest.with_file_name("Cargo.toml")).ok()?;
    text.lines().find_map(|line| {
        line.strip_prefix("name = ")
            .map(|name| name.trim().trim_matches('"').to_string())
    })
}

/// How much source a project carries, not counting the models it ships,
/// which have limits of their own.
pub fn source_size(source: &Path) -> u64 {
    walk(source, 32)
        .iter()
        .filter(|path| path.extension().is_none_or(|ext| ext != "onnx"))
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

/// Build a project that has already been unpacked, its manifest checked.
pub fn build(source: &Path, sdk: &Path, package: &str) -> Result<Built, (Failure, u32)> {
    let started = Instant::now();
    let seconds = || started.elapsed().as_secs() as u32;

    // The SDK is the build service's own copy, not whatever a submission
    // might point at.
    pin_sdk(source, sdk).map_err(|_| (Failure::Service, seconds()))?;
    let component = compile(source, package).map_err(|failure| (failure, seconds()))?;
    instantiate(&component).map_err(|_| (Failure::Build, seconds()))?;
    let bundle =
        shared::archive::bundle(&component, &[]).map_err(|_| (Failure::Service, seconds()))?;

    Ok(Built {
        component_hash: hash_bytes(&component),
        bundle_hash: hash_bytes(&bundle),
        component,
        bundle,
        seconds: seconds(),
    })
}

fn walk(directory: &Path, depth: usize) -> Vec<PathBuf> {
    if depth == 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return found;
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            found.extend(walk(&path, depth - 1));
        } else {
            found.push(path);
        }
    }
    found
}

/// Point the submission's SDK at the one this build service carries, whatever
/// it asked for. An author declares a version; the build service decides what
/// that version is.
fn pin_sdk(source: &Path, sdk: &Path) -> std::io::Result<()> {
    let directory = source.join(".cargo");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("config.toml"),
        format!(
            "# Written by the build service.\n[patch.crates-io]\nshonin-strategy-sdk = {{ path = \"{}\" }}\n",
            sdk.display()
        ),
    )
}

fn compile(source: &Path, package: &str) -> Result<Vec<u8>, Failure> {
    let target = source.join("../build");
    // The compiler's output is never kept: it could quote sealed source.
    let mut child = Command::new("cargo")
        .current_dir(source)
        .env("CARGO_TARGET_DIR", &target)
        .args([
            "build",
            "--release",
            "--locked",
            "--target",
            "wasm32-wasip2",
            "-p",
            package,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure::Service)?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TIME_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => return Err(Failure::Service),
        }
    };
    if !status.success() {
        return Err(Failure::Build);
    }

    let wasm = target
        .join("wasm32-wasip2/release")
        .join(format!("{}.wasm", package.replace('-', "_")));
    std::fs::read(&wasm).map_err(|_| Failure::Build)
}

/// Load the component the way the executor will, so a bundle that cannot run
/// never reaches a vault.
fn instantiate(component: &[u8]) -> Result<(), String> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model_async(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config).map_err(|error| error.to_string())?;
    wasmtime::component::Component::new(&engine, component).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("shonin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_manifest_is_found_deep_inside_a_workspace() {
        let root = scratch("deep");
        let nested = root.join("packages/agents/funding-basis");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").unwrap();
        std::fs::write(nested.join("shonin.toml"), b"").unwrap();
        assert_eq!(find_manifest(&root, None), Ok(nested.join("shonin.toml")));
    }

    #[test]
    fn a_workspace_of_several_agents_names_the_package() {
        let root = scratch("several");
        for name in ["one", "two"] {
            let dir = root.join("packages/agents").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("shonin.toml"), b"").unwrap();
            std::fs::write(
                dir.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            )
            .unwrap();
        }
        assert!(find_manifest(&root, None).is_err());
        assert_eq!(
            find_manifest(&root, Some("two")),
            Ok(root.join("packages/agents/two/shonin.toml"))
        );
        assert_eq!(
            package_of(&root.join("packages/agents/two/shonin.toml")).as_deref(),
            Some("two")
        );
    }

    #[test]
    fn models_do_not_count_towards_the_size_limit() {
        let root = scratch("size");
        std::fs::write(root.join("lib.rs"), vec![b'x'; 100]).unwrap();
        std::fs::write(root.join("model.onnx"), vec![0u8; 5000]).unwrap();
        assert_eq!(source_size(&root), 100);
    }
}
