//! Compiling a submission into a bundle, twice.
//!
//! The recipe is pinned and the same for everyone: the toolchain the
//! repository names, the SDK the build service carries, `--locked` so the
//! dependency graph is the author's own, and the component target. Then the
//! whole thing is built a second time and the bytes are compared. A project
//! that does not produce the same component twice is refused with the
//! difference shown, because a bundle nobody else can reproduce is a bundle
//! nobody else can check.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use shared::manifest::{Manifest, hash_bytes};
use shared::model::bundle::{LogLevel, LogLine};

/// The recipe. Every submission is built with exactly this, whatever it
/// asked for, so two builds of the same source are comparable.
pub const TOOLCHAIN: &str = "rust 1.98.1 · wasm32-wasip2";

/// What a finished build produced.
pub struct Built {
    pub manifest: Manifest,
    pub component: Vec<u8>,
    pub bundle: Vec<u8>,
    pub component_hash: String,
    pub bundle_hash: String,
    pub manifest_hash: String,
    pub log: Vec<LogLine>,
    pub seconds: u32,
    /// The recipe this was built with, which anyone repeating it needs.
    pub toolchain: String,
}

/// What went wrong, and the log that shows it.
pub struct Failed {
    pub reason: String,
    pub log: Vec<LogLine>,
    /// How long it ran before it failed. A build that ran is charged, so a
    /// failure has to say how much of the machine it used.
    pub seconds: u32,
}

pub fn plain(text: impl Into<String>) -> LogLine {
    LogLine {
        text: text.into(),
        level: LogLevel::Plain,
    }
}

fn warn(text: impl Into<String>) -> LogLine {
    LogLine {
        text: text.into(),
        level: LogLevel::Warn,
    }
}

fn error(text: impl Into<String>) -> LogLine {
    LogLine {
        text: text.into(),
        level: LogLevel::Error,
    }
}

/// Build a project that has already been unpacked. `claimed` is the strategy
/// and version the vault was created for: the manifest inside has to say the
/// same, or the bundle and the vault would be about different things.
pub fn build(
    source: &Path,
    sdk: &Path,
    image: &str,
    claimed: (&str, u32),
) -> Result<Built, Failed> {
    let started = std::time::Instant::now();
    let mut log = vec![
        plain(format!("builder {image} (allowlisted, timelock cleared)")),
        plain("reading the manifest"),
    ];

    let manifest = match read_manifest(source) {
        Ok(manifest) => manifest,
        Err(error_text) => {
            log.push(error(error_text.to_string()));
            return Err(Failed {
                reason: "the manifest could not be read".to_string(),
                log,
                seconds: started.elapsed().as_secs() as u32,
            });
        }
    };
    if manifest.strategy.id != claimed.0 || manifest.strategy.version != claimed.1 {
        log.push(error(format!(
            "the vault was created for {} v{}, and the manifest says {} v{}",
            claimed.0, claimed.1, manifest.strategy.id, manifest.strategy.version
        )));
        return Err(Failed {
            reason: "the manifest names a different strategy than the vault".to_string(),
            log,
            seconds: started.elapsed().as_secs() as u32,
        });
    }

    let package = package_name(source, &manifest);
    log.push(plain(format!(
        "{} v{}, package {package}",
        manifest.strategy.id, manifest.strategy.version
    )));

    // The SDK is the build service's own copy, not whatever a submission
    // might point at.
    if let Err(problem) = pin_sdk(source, sdk) {
        log.push(error(problem.to_string()));
        return Err(Failed {
            reason: "the SDK could not be pinned".to_string(),
            log,
            seconds: started.elapsed().as_secs() as u32,
        });
    }
    log.push(plain("the SDK is the build service's own copy"));

    let mut passes = Vec::new();
    for pass in 1..=2 {
        log.push(plain(format!(
            "cargo build --release --locked --target wasm32-wasip2 -p {package}   (pass {pass})"
        )));
        match compile(source, &package, pass) {
            Ok((bytes, output)) => {
                for line in output.lines().take(40) {
                    log.push(plain(line.to_string()));
                }
                log.push(plain(format!("pass {pass} complete")));
                passes.push(bytes);
            }
            Err(output) => {
                for line in output.lines() {
                    log.push(error(line.to_string()));
                }
                return Err(Failed {
                    reason: "the strategy did not compile".to_string(),
                    log,
                    seconds: started.elapsed().as_secs() as u32,
                });
            }
        }
    }

    if passes[0] != passes[1] {
        log.push(error(format!(
            "comparing outputs: the two passes differ ({} bytes against {})",
            passes[0].len(),
            passes[1].len()
        )));
        return Err(Failed {
            reason: "the two passes produced different bytes".to_string(),
            log,
            seconds: started.elapsed().as_secs() as u32,
        });
    }
    log.push(plain("comparing outputs: byte-identical"));

    let component = passes.remove(0);
    if let Err(problem) = instantiate(&component) {
        log.push(error(problem.to_string()));
        return Err(Failed {
            reason: "the component would not load".to_string(),
            log,
            seconds: started.elapsed().as_secs() as u32,
        });
    }
    log.push(plain(
        "instantiating the component in the executor's runtime",
    ));
    log.push(plain(format!(
        "world {} satisfied",
        "shonin:strategy@0.1.0"
    )));

    let bundle = match shared::archive::bundle(&component, &[]) {
        Ok(bundle) => bundle,
        Err(problem) => {
            log.push(error(problem.to_string()));
            return Err(Failed {
                reason: "the bundle could not be made".to_string(),
                log,
                seconds: started.elapsed().as_secs() as u32,
            });
        }
    };
    if manifest.runtime.capabilities.is_empty() {
        log.push(plain("no extra capabilities declared"));
    } else {
        log.push(warn(format!(
            "capabilities declared: {}",
            manifest.runtime.capabilities.join(", ")
        )));
    }

    let seconds = started.elapsed().as_secs() as u32;
    log.push(plain(format!("build finished in {seconds}s")));

    Ok(Built {
        component_hash: hash_bytes(&component),
        bundle_hash: hash_bytes(&bundle),
        manifest_hash: manifest.hash(),
        manifest,
        component,
        bundle,
        log,
        seconds,
        toolchain: TOOLCHAIN.to_string(),
    })
}

fn read_manifest(source: &Path) -> Result<Manifest> {
    let path = find_manifest(source).context("no shonin.toml in the submission")?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Manifest::from_toml(&text).context("the manifest is not valid")
}

/// A submission may be one package or a workspace; the manifest is wherever
/// the strategy is.
fn find_manifest(source: &Path) -> Option<PathBuf> {
    let direct = source.join("shonin.toml");
    if direct.exists() {
        return Some(direct);
    }
    // A submission is the whole workspace a strategy belongs to, so its
    // manifest usually sits a few directories in.
    walk(source, 6)
        .into_iter()
        .find(|path| path.file_name().is_some_and(|name| name == "shonin.toml"))
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
        if path.is_dir() {
            found.extend(walk(&path, depth - 1));
        } else {
            found.push(path);
        }
    }
    found
}

/// Which cargo package to build. The manifest's strategy id is the package
/// name by convention; a submission that disagrees says so in its own
/// `Cargo.toml`.
fn package_name(source: &Path, manifest: &Manifest) -> String {
    let Some(path) = find_manifest(source) else {
        return manifest.strategy.id.clone();
    };
    let cargo = path.with_file_name("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&cargo) else {
        return manifest.strategy.id.clone();
    };
    text.lines()
        .find_map(|line| {
            line.strip_prefix("name = ")
                .map(|name| name.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| manifest.strategy.id.clone())
}

/// Point the submission's SDK at the one this build service carries, whatever
/// it asked for. An author declares a version; the build service decides what
/// that version is.
fn pin_sdk(source: &Path, sdk: &Path) -> Result<()> {
    let directory = source.join(".cargo");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("config.toml"),
        format!(
            "# Written by the build service.\n[patch.crates-io]\nshonin-strategy-sdk = {{ path = \"{}\" }}\n",
            sdk.display()
        ),
    )?;
    Ok(())
}

/// One pass. Each writes to its own directory, so neither can see what the
/// other left behind.
fn compile(source: &Path, package: &str, pass: u32) -> Result<(Vec<u8>, String), String> {
    let target = source.join(format!("../build-pass-{pass}"));
    let output = Command::new("cargo")
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
        .output()
        .map_err(|error| format!("cargo would not start: {error}"))?;

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(text);
    }

    let wasm = target
        .join("wasm32-wasip2/release")
        .join(format!("{}.wasm", package.replace('-', "_")));
    let bytes = std::fs::read(&wasm).map_err(|error| {
        format!(
            "the build produced no component at {}: {error}",
            wasm.display()
        )
    })?;
    Ok((bytes, text))
}

/// Load the component the way the executor will, so a bundle that cannot run
/// never reaches a vault.
fn instantiate(component: &[u8]) -> Result<()> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model_async(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config)
        .map_err(|error| anyhow::anyhow!("starting the runtime: {error}"))?;
    wasmtime::component::Component::new(&engine, component)
        .map_err(|error| anyhow::anyhow!("the component is not loadable: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_is_found_deep_inside_a_workspace() {
        let root = std::env::temp_dir().join(format!("shonin-deep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("packages/bots/funding-basis");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").unwrap();
        std::fs::write(nested.join("shonin.toml"), b"").unwrap();
        assert_eq!(find_manifest(&root), Some(nested.join("shonin.toml")));
    }

    #[test]
    fn a_manifest_is_found_wherever_the_strategy_is() {
        let root = std::env::temp_dir().join(format!("shonin-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("packages/bots/one");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("shonin.toml"), b"").unwrap();
        assert_eq!(find_manifest(&root), Some(nested.join("shonin.toml")));
    }

    #[test]
    fn the_package_comes_from_the_cargo_manifest_beside_it() {
        let root = std::env::temp_dir().join(format!("shonin-package-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("shonin.toml"), b"").unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname = \"a-different-name\"\n",
        )
        .unwrap();

        let manifest = Manifest::from_toml(
            r#"
[strategy]
id = "the-id"
version = 1
[description]
title = "t"
text = "t"
[runtime.allowances]
fuel_per_sec = 1
memory_mib = 1
requests_per_min = 1
bytes_per_day = 1
streams = 1
inference_ms_per_min = 0
"#,
        )
        .unwrap();
        assert_eq!(package_name(&root, &manifest), "a-different-name");
    }
}
