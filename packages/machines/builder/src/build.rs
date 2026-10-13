//! Compiling a submission into a bundle.
//!
//! The recipe is pinned and the same for everyone: the toolchain the
//! repository names, the SDK the build service carries, `--locked` so the
//! dependency graph is the author's own, `--offline` against a vendored copy
//! of the platform's reviewed crates (see [`shared::dependencies`]) so it can
//! be nothing else, and the component target.
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

/// The compiler in the recipe, as rustup names it. It is chosen for every
/// build, ahead of any `rust-toolchain` file a project carries.
pub const RUST: &str = "1.98.1";

/// The world every component must implement.
pub const WORLD: &str = "shonin:agent@0.1.0";

/// The longest a build may run.
pub const TIME_LIMIT: Duration = Duration::from_secs(20 * 60);

/// The most a submission may carry, unpacked, every file counted.
pub const SIZE_LIMIT: u64 = shared::archive::MOST_SOURCE_BYTES;

/// The largest component a build may make. It is read whole into the build
/// service's memory, so one past this is refused before it is read; a real
/// agent's is a few MiB.
pub const COMPONENT_LIMIT: u64 = 64 * 1024 * 1024;

/// The longest loading a component may take. Wasmtime compiles it as it
/// loads, which a real agent's takes seconds to do.
pub const LOAD_LIMIT: Duration = Duration::from_secs(60);

/// The argument that has the build service's own program load a component
/// and say only whether it loads (see [`load_apart`]).
pub const LOAD: &str = "--load-component";

/// The argument that has the build service's own program run the command
/// after it and end that command's whole process group should the build
/// service go away (see [`watch_here`]): how the compiler is run.
pub const WATCH: &str = "--watch-group";

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
            .find(|path| package_of(path).as_deref() == Ok(package))
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

/// The manifest at `path` inside a packed project, read from the archive
/// itself: what was uploaded and reviewed, whatever has since been done to a
/// copy unpacked on disk.
pub fn manifest_in(archive: &[u8], path: &str) -> Result<Manifest, String> {
    use std::io::Read;

    let wanted = parts(Path::new(path));
    let mut entries = tar::Archive::new(std::io::Cursor::new(archive));
    for entry in entries
        .entries()
        .map_err(|_| "the archive could not be read".to_string())?
    {
        let mut entry = entry.map_err(|_| "the archive could not be read".to_string())?;
        let named = entry.header().entry_type() == tar::EntryType::Regular
            && entry.path().is_ok_and(|inside| parts(&inside) == wanted);
        if !named {
            continue;
        }
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|_| "shonin.toml could not be read".to_string())?;
        return Manifest::from_toml(&text);
    }
    Err(format!("no {path} in the archive"))
}

/// A path's named parts, so `./a/b` and `a/b` are the same path.
fn parts(path: &Path) -> Vec<&std::ffi::OsStr> {
    path.components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// The package a manifest belongs to: `[package].name` in the `Cargo.toml`
/// beside it, read as TOML, so a `name` in any other table (`[lib]`, `[[bin]]`)
/// is never taken for it. A `Cargo.toml` with no `[package]`, a workspace's
/// root, has no package to build: the manifest belongs beside the agent's own.
pub fn package_of(manifest: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(manifest.with_file_name("Cargo.toml"))
        .map_err(|_| "no Cargo.toml beside shonin.toml".to_string())?;
    let cargo: toml::Table = text
        .parse()
        .map_err(|_| "the Cargo.toml beside shonin.toml is not valid TOML".to_string())?;
    let Some(package) = cargo.get("package") else {
        return Err(
            "the Cargo.toml beside shonin.toml has no [package]: put shonin.toml \
                    beside the agent's own Cargo.toml, not a workspace's root"
                .into(),
        );
    };
    package
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the Cargo.toml beside shonin.toml names no package".to_string())
}

/// Whatever in a project would change how the build service builds it, named,
/// or `None`. Cargo reads `.cargo/config` ahead of the `.cargo/config.toml`
/// the build service writes to pin the SDK, and a Cargo config can name a
/// wrapper to run in place of the compiler, so a project may carry no
/// `.cargo` at all, however it is spelt: on a file system that ignores case,
/// `.CARGO` is the same directory. A `rust-toolchain` file is fine: [`RUST`]
/// is chosen ahead of it.
pub fn overrides(source: &Path) -> Option<String> {
    walk(source, 32).into_iter().find_map(|path| {
        let inside = path.strip_prefix(source).ok()?;
        inside
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case(".cargo"))
            .then(|| {
                format!(
                    "the project carries {}: the build service sets the Cargo config itself, \
                     so a project may carry no .cargo directory",
                    inside.display()
                )
            })
    })
}

/// How much a project carries, every file counted. No model is shipped in a
/// bundle yet, so a model file is source like any other: the reviewer reads
/// it, and it counts here.
pub fn source_size(source: &Path) -> u64 {
    walk(source, 32)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

/// Build a project that has already been unpacked, its manifest and its
/// dependencies checked. `source` is `<job>/source`, in a directory made
/// fresh for the job (see [`fresh`]), and the job's Cargo home and build
/// output go beside it. `vendor` is the vendored copy of the platform's
/// reviewed crates, the only crates.io crates the build can reach.
pub fn build(
    source: &Path,
    sdk: &Path,
    vendor: &Path,
    package: &str,
) -> Result<Built, (Failure, u32)> {
    let started = Instant::now();
    let seconds = || started.elapsed().as_secs() as u32;

    // The SDK is the build service's own copy, not whatever a submission
    // might point at, and every other crate is the vendored one.
    pin(source, sdk, vendor).map_err(|_| (Failure::Service, seconds()))?;
    if let Some(found) = above(source) {
        tracing::error!(
            found = %found.display(),
            "a Cargo.toml or a Cargo config lies above a build, where cargo would take it \
             for the build's own: no build runs until it is looked into and removed"
        );
        return Err((Failure::Service, seconds()));
    }
    let wasm = compile(source, package).map_err(|failure| (failure, seconds()))?;
    load_apart(&wasm).map_err(|failure| (failure, seconds()))?;
    // The build's own user owns everything in its job directory, so the
    // component is read only if it is in there, through no link out of it.
    let job = source.parent().unwrap_or(source);
    if !inside(&wasm, job) {
        return Err((Failure::Build, seconds()));
    }
    let component = read_component(&wasm).map_err(|failure| (failure, seconds()))?;
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

/// Whether `path`, every link in it followed, is inside `directory`.
fn inside(path: &Path, directory: &Path) -> bool {
    match (
        std::fs::canonicalize(path),
        std::fs::canonicalize(directory),
    ) {
        (Ok(path), Ok(directory)) => path.starts_with(directory),
        _ => false,
    }
}

/// A `Cargo.toml` or a `.cargo` in any directory above `source`, up to the
/// root, or `None`. Cargo reads a config from every directory above the one
/// it is run in, and walks every one of them for a `Cargo.toml` holding
/// `[workspace]`, which makes the project a member of that workspace, its
/// `[patch]`, `[profile]` and lock file included. A build runs as the build
/// service does, so a build script or procedural macro can write either
/// where the next job's build would read it. Each job is built in a directory
/// made fresh for it (see [`fresh`]), so nothing a build left can be inside
/// it, and everything above it is checked here. On a file system that
/// ignores case `.CARGO` and `CARGO.TOML` are the same names, and are found
/// too.
pub fn above(source: &Path) -> Option<PathBuf> {
    let source = std::path::absolute(source).unwrap_or_else(|_| source.to_path_buf());
    for directory in source.ancestors().skip(1) {
        for name in ["Cargo.toml", ".cargo"] {
            let found = directory.join(name);
            if std::fs::symlink_metadata(&found).is_ok() {
                return Some(found);
            }
        }
    }
    None
}

/// A directory made fresh for job `id` inside `jobs`, under a name nobody
/// could have guessed: nothing an earlier build left can be in it, since
/// making it fails if anything already has its name. What is above it is
/// checked before every build (see [`above`]).
pub fn fresh(jobs: &Path, id: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    use std::io::Read;

    let mut unguessable = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut unguessable)?;
    let directory = jobs.join(format!("{id}-{}", alloy::hex::encode(unguessable)));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    Ok(directory)
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
/// it asked for, and crates.io at the vendored copy of the reviewed crates.
/// An author declares a version; the build service decides what that version
/// is, and that every other crate is one the platform reviewed. A lock that
/// names the SDK from crates.io is made to name the path package patched in,
/// at the carried version, so the build is `--locked` to the lock as checked
/// (see [`shared::dependencies::with_carried_sdk`]).
fn pin(source: &Path, sdk: &Path, vendor: &Path) -> std::io::Result<()> {
    for lock in walk(source, 32)
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.lock"))
    {
        let text = std::fs::read_to_string(&lock)?;
        let rewritten = shared::dependencies::with_carried_sdk(&text, strategy_sdk::VERSION)
            .map_err(std::io::Error::other)?;
        if let Some(rewritten) = rewritten {
            std::fs::write(&lock, rewritten)?;
        }
    }
    let directory = source.join(".cargo");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("config.toml"), config(sdk, vendor))
}

/// The packages the SDK at `sdk` depends on, as its entry in a lock lists
/// them, if `sdk` is a copy of the SDK this program is built with, at its
/// version ([`strategy_sdk::VERSION`]): the version a lock must name the SDK
/// at, and the copy patched in for the build to be `--locked` to it. Only the
/// name and version are checked here; what the copy holds is pinned by the
/// image that carries it.
///
/// A dependency is named as its table names the package (`package = ...`,
/// or its key). An optional dependency, which a lock lists only as features
/// turn it on, makes the entry a lock must hold depend on features, so a
/// copy with one is refused rather than checked loosely.
pub fn carries(sdk: &Path) -> Result<Vec<String>, String> {
    let manifest = sdk.join("Cargo.toml");
    let cargo = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .unwrap_or_default();
    let field = |key: &str| {
        cargo
            .get("package")
            .and_then(|package| package.get(key))
            .and_then(toml::Value::as_str)
    };
    if field("name") != Some(shared::dependencies::SDK)
        || field("version") != Some(strategy_sdk::VERSION)
    {
        return Err(format!(
            "{} is not {} {}, the SDK this build service is built with: give --sdk a copy of \
             it",
            manifest.display(),
            shared::dependencies::SDK,
            strategy_sdk::VERSION,
        ));
    }
    // A lock lists a package's dependencies and build dependencies, on
    // every target; a dependency's own dev-dependencies never.
    let mut tables: Vec<&toml::Value> = ["dependencies", "build-dependencies"]
        .iter()
        .filter_map(|kind| cargo.get(*kind))
        .collect();
    if let Some(targets) = cargo.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for kind in ["dependencies", "build-dependencies"] {
                tables.extend(target.get(kind));
            }
        }
    }
    let mut named = Vec::new();
    for (key, dependency) in tables
        .into_iter()
        .filter_map(toml::Value::as_table)
        .flatten()
    {
        if dependency.get("optional").and_then(toml::Value::as_bool) == Some(true) {
            return Err(format!(
                "{} has an optional dependency, {key}: a lock lists it only as features turn \
                 it on, so the SDK's entry in a lock can't be checked exactly",
                manifest.display()
            ));
        }
        let package = dependency
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(key);
        named.push(package.to_string());
    }
    Ok(named)
}

/// The Cargo config every build is made with.
fn config(sdk: &Path, vendor: &Path) -> String {
    let quoted = |path: &Path| toml::Value::String(path.display().to_string()).to_string();
    format!(
        "# Written by the build service.\n\
         [patch.crates-io]\n{} = {{ path = {} }}\n\n\
         [source.crates-io]\nreplace-with = \"reviewed\"\n\n\
         [source.reviewed]\ndirectory = {}\n\n\
         [net]\noffline = true\n",
        shared::dependencies::SDK,
        quoted(sdk),
        quoted(vendor),
    )
}

/// The recipe's one command. The compiler's output is never kept: it could
/// quote sealed source. `home` is the job's own Cargo home, made empty for
/// it: its config, its downloaded crates and their unpacked source are the
/// job's alone, so nothing one build writes there is read by another's.
///
/// A build runs the author's code (build scripts, procedural macros, and
/// `env!` / `option_env!` read as it compiles), so it is given none of the
/// build service's environment, which holds its secrets (a testnet's
/// mnemonic, an RPC URL carrying a provider's key): only what compiling
/// needs (see [`environment`]).
fn cargo(source: &Path, target: &Path, home: &Path, package: &str) -> Command {
    let cargo = toolchain_cargo();
    // Run under this program given [`WATCH`], when it can be found, so a
    // compiler left running by a build service that died is ended with it.
    // The watcher says what it could not do in the build service's own log;
    // what it runs is given no output (see [`watch_here`]).
    let mut command = match std::env::current_exe() {
        Ok(program) => {
            let mut command = Command::new(program);
            command.arg(WATCH).arg(&cargo);
            command
        }
        Err(_) => {
            let mut command = Command::new(&cargo);
            command.stdout(Stdio::null()).stderr(Stdio::null());
            command
        }
    };
    let job = source.parent().unwrap_or(source);
    command
        .current_dir(source)
        .stdin(Stdio::null())
        .env_clear()
        .envs(environment(&cargo))
        .env("HOME", job)
        // rustup takes this ahead of any toolchain file in the project.
        .env("RUSTUP_TOOLCHAIN", RUST)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_HOME", home);
    crate::isolation::as_build_user(&mut command);
    command.args([
        "build",
        "--release",
        "--locked",
        "--offline",
        "--target",
        "wasm32-wasip2",
        "-p",
        package,
    ]);
    command
}

/// The `cargo` the build service's own `PATH` finds (the toolchain's, a
/// rustup proxy in the image), or the bare name when none is found.
fn toolchain_cargo() -> PathBuf {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("cargo"))
        .find(|cargo| cargo.is_file())
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// The whole environment a build is given, beside what each build sets for
/// itself (its `HOME`, Cargo home, target directory and toolchain): a `PATH`
/// to the toolchain and the system's own programs, and where rustup keeps
/// the toolchains, as the build service has it. Nothing else of the build
/// service's own environment, where its secrets are, reaches a build.
fn environment(cargo: &Path) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut path: Vec<PathBuf> = cargo
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    for system in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if !path.iter().any(|known| known == Path::new(system)) {
            path.push(PathBuf::from(system));
        }
    }
    let mut given = Vec::new();
    if let Ok(path) = std::env::join_paths(path) {
        given.push(("PATH".into(), path));
    }
    // rustup finds its toolchains here, by default under the home directory,
    // which a build is given a home of its own in place of.
    let rustup = std::env::var_os("RUSTUP_HOME").or_else(|| {
        std::env::var_os("HOME").map(|home| Path::new(&home).join(".rustup").into_os_string())
    });
    if let Some(rustup) = rustup {
        given.push(("RUSTUP_HOME".into(), rustup));
    }
    given
}

/// Compile the project, and say where the component it made is.
fn compile(source: &Path, package: &str) -> Result<PathBuf, Failure> {
    let target = source.join("../build");
    let home = source.join("../cargo");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|_| Failure::Service)?;
    // The job's directory is the build user's, and nothing else is.
    crate::isolation::hand_over(source.parent().unwrap_or(source)).map_err(|error| {
        tracing::error!(%error, "a job's directory could not be handed to the build user");
        Failure::Service
    })?;
    let mut child =
        Group::spawn(cargo(source, &target, &home, package)).map_err(|_| Failure::Service)?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            // Dropping the group ends it, the compiler and everything it ran.
            Ok(None) if started.elapsed() > TIME_LIMIT => return Err(Failure::TimedOut),
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => return Err(Failure::Service),
        }
    };
    if !status.success() {
        return Err(Failure::Build);
    }

    Ok(target
        .join("wasm32-wasip2/release")
        .join(format!("{}.wasm", package.replace('-', "_"))))
}

/// The component a build made, read only if it is within [`COMPONENT_LIMIT`].
fn read_component(wasm: &Path) -> Result<Vec<u8>, Failure> {
    let size = std::fs::metadata(wasm).map_err(|_| Failure::Build)?.len();
    if size > COMPONENT_LIMIT {
        return Err(Failure::Build);
    }
    std::fs::read(wasm).map_err(|_| Failure::Build)
}

/// Load the component in a process of its own (the build service's own
/// program, given [`LOAD`]), stopped after [`LOAD_LIMIT`], as the compiler
/// runs in one of its own: whatever loading it takes, time or memory, is
/// taken from that process, never from the build service.
fn load_apart(wasm: &Path) -> Result<(), Failure> {
    if std::fs::metadata(wasm).map_err(|_| Failure::Build)?.len() > COMPONENT_LIMIT {
        return Err(Failure::Build);
    }
    let program = std::env::current_exe().map_err(|_| Failure::Service)?;
    let mut command = Command::new(program);
    command
        .arg(LOAD)
        .arg(wasm)
        // Loading needs nothing of the build service's environment.
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::isolation::as_build_user(&mut command);
    let mut child = Group::spawn(command).map_err(|_| Failure::Service)?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(Failure::Build),
            Ok(None) if started.elapsed() > LOAD_LIMIT => return Err(Failure::TimedOut),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return Err(Failure::Service),
        }
    }
}

/// What the build service's program does given [`LOAD`]: load the component
/// at `wasm` and say whether it loads, by how it exits. It gives up should the
/// build service go away first.
pub fn load_here(wasm: &Path) -> bool {
    end_with_parent(None);
    read_component(wasm).is_ok_and(|component| instantiate(&component).is_ok())
}

/// A child in a process group of its own, which it leads, so that everything
/// it starts (cargo's compilers and build scripts) is in it too. The whole
/// group is killed when the step ends, however it ends: once the child is
/// seen to end, whether it succeeded or failed, and when this is dropped
/// before then, on a time limit, an error or a panic. Nothing a step started
/// in its group outlives the step; and where builds run as a user of their
/// own, nothing that user runs outlives it, in the group or not (see
/// [`crate::isolation::sweep`]).
struct Group {
    child: std::process::Child,
    reaped: bool,
}

impl Group {
    fn spawn(mut command: Command) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        Ok(Self {
            child: command.spawn()?,
            reaped: false,
        })
    }

    /// How the child ended, once it has. Its group is killed as soon as it
    /// is seen to have ended and before it is reaped: until then its id is
    /// still the group's and cannot have been given to anyone else.
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

        if self.reaped {
            return self.child.try_wait();
        }
        let leader = Pid::from_raw(self.child.id() as i32)
            .ok_or_else(|| std::io::Error::other("a child with no process id"))?;
        let ended = waitid(
            WaitId::Pid(leader),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?;
        if ended.is_none() {
            return Ok(None);
        }
        kill_group(self.child.id());
        let status = self.child.wait()?;
        self.reaped = true;
        // And whatever the step started outside its group.
        crate::isolation::sweep();
        Ok(Some(status))
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // The leader is not yet reaped, so its id is still the group's and
        // cannot have been given to anyone else.
        kill_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
        crate::isolation::sweep();
    }
}

/// Kill every process in the group `group` leads, signalled from this
/// process: no program needs to be there to do it. A group that is already
/// empty is no failure; any other is said, as an error, since it may leave a
/// build's processes running.
fn kill_group(group: u32) {
    use rustix::process::{Pid, Signal, kill_process_group};

    let Some(leader) = i32::try_from(group).ok().and_then(Pid::from_raw) else {
        tracing::error!(
            group,
            "a build's process group has no usable id: it was not killed"
        );
        return;
    };
    match kill_process_group(leader, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        // macOS answers so for a group whose only member is its leader,
        // ended and not yet reaped: there is nothing left to kill.
        #[cfg(target_os = "macos")]
        Err(rustix::io::Errno::PERM) => {}
        Err(error) => {
            tracing::error!(group, %error, "a build's process group could not be killed");
        }
    }
}

/// Watch, on a thread of its own, for the process that started this one to
/// go away (this one handed to another parent), and then end: the whole
/// process group this one leads, if `group` is given, else this process.
fn end_with_parent(group: Option<u32>) {
    let parent = std::os::unix::process::parent_id();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if std::os::unix::process::parent_id() != parent {
                if let Some(group) = group {
                    kill_group(group);
                }
                std::process::exit(1);
            }
        }
    });
}

/// What the build service's program does given [`WATCH`]: run `command` (a
/// program and its arguments) in this process's group, which the build
/// service started it leading, and exit as it exits. Should the build service
/// go away first, the whole group is killed: nothing it started outlives it.
pub fn watch_here(mut command: impl Iterator<Item = std::ffi::OsString>) -> i32 {
    let Some(program) = command.next() else {
        return 1;
    };
    end_with_parent(Some(std::process::id()));
    match Command::new(program)
        .args(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// Load the component the way the executor will, and check that it imports
/// nothing the executor doesn't give an agent, so a bundle that cannot run
/// never reaches a vault.
fn instantiate(component: &[u8]) -> Result<(), String> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model_async(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config).map_err(|error| error.to_string())?;
    let component = wasmtime::component::Component::new(&engine, component)
        .map_err(|error| error.to_string())?;
    for (name, _) in component.component_type().imports(&engine) {
        if !importable(name) {
            return Err(format!(
                "the agent imports {name}, which the executor doesn't give"
            ));
        }
    }
    Ok(())
}

/// Whether the executor gives an agent the import `name`: the
/// `shonin:agent` world's own interfaces, `wasi:keyvalue` for its state,
/// `wasi:http` for its requests, and the WASI 0.2 packages for clocks,
/// randomness, printing and the like. Never raw sockets: an agent reaches
/// the network only through the executor.
pub fn importable(name: &str) -> bool {
    let Some((interface, version)) = name.split_once('@') else {
        return false;
    };
    let Some((package, _)) = interface.split_once('/') else {
        return false;
    };
    let wasi_0_2 = version.starts_with("0.2.") && !version.contains('-');
    match package {
        "shonin:agent" => {
            version == "0.1.0"
                && matches!(
                    &interface["shonin:agent/".len()..],
                    "types"
                        | "drivers"
                        | "transfers"
                        | "vault"
                        | "data"
                        | "websocket"
                        | "inference"
                        | "secrets"
                )
        }
        "wasi:keyvalue" => {
            version == "0.2.0-draft"
                && matches!(
                    interface,
                    "wasi:keyvalue/store" | "wasi:keyvalue/atomics" | "wasi:keyvalue/batch"
                )
        }
        "wasi:http" => {
            wasi_0_2 && matches!(interface, "wasi:http/types" | "wasi:http/outgoing-handler")
        }
        "wasi:io" | "wasi:clocks" | "wasi:random" | "wasi:cli" | "wasi:filesystem" => wasi_0_2,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_may_import_only_what_the_executor_gives() {
        for given in [
            "shonin:agent/drivers@0.1.0",
            "shonin:agent/websocket@0.1.0",
            "wasi:keyvalue/store@0.2.0-draft",
            "wasi:keyvalue/atomics@0.2.0-draft",
            "wasi:keyvalue/batch@0.2.0-draft",
            "wasi:http/outgoing-handler@0.2.12",
            "wasi:http/types@0.2.9",
            "wasi:io/streams@0.2.12",
            "wasi:clocks/monotonic-clock@0.2.12",
            "wasi:cli/stdout@0.2.9",
            "wasi:random/random@0.2.0",
        ] {
            assert!(importable(given), "{given} was refused");
        }
        for refused in [
            "shonin:agent/drivers@0.2.0",
            "shonin:agent/state@0.1.0",
            "wasi:sockets/tcp@0.2.12",
            "wasi:http/incoming-handler@0.2.12",
            "wasi:keyvalue/watcher@0.2.0-draft",
            "wasi:http/types@0.3.0-rc-2025-09-16",
            "wasi:io/streams@0.2.0-rc-2023-11-10",
            "acme:secret/backdoor@1.0.0",
            "unversioned",
        ] {
            assert!(!importable(refused), "{refused} was let through");
        }
    }

    #[test]
    fn a_component_importing_what_the_executor_doesn_t_give_is_refused() {
        let refused =
            instantiate(br#"(component (import "acme:secret/backdoor@1.0.0" (instance)))"#)
                .unwrap_err();
        assert!(refused.contains("acme:secret/backdoor@1.0.0"), "{refused}");
        instantiate(br#"(component (import "wasi:cli/stdout@0.2.9" (instance)))"#).unwrap();
    }

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
            Ok("two")
        );
    }

    /// Cargo prefers a project's own `.cargo/config` to the
    /// `.cargo/config.toml` the build service writes, so a project carrying
    /// anything under `.cargo`, at any depth, is refused before it is paid
    /// for; a `rust-toolchain` file is no override, the recipe's compiler
    /// being chosen ahead of it.
    #[test]
    fn a_project_carrying_a_cargo_config_is_refused() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "packages/agent/.cargo/config",
            ".CARGO/config",
            "packages/agent/.Cargo/config.toml",
        ] {
            let root = scratch("cargo-config");
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join("Cargo.toml"), b"[workspace]").unwrap();
            std::fs::write(root.join(name), b"[build]\nrustc-wrapper = \"x\"\n").unwrap();
            let refused = overrides(&root);
            assert!(
                refused.as_deref().is_some_and(|why| why.contains(name)),
                "{name} was not refused: {refused:?}"
            );
        }
        let root = scratch("toolchain-file");
        std::fs::write(root.join("Cargo.toml"), b"[workspace]").unwrap();
        std::fs::write(root.join("rust-toolchain.toml"), b"[toolchain]\n").unwrap();
        assert_eq!(overrides(&root), None);
    }

    /// The recipe's compiler is chosen ahead of any `rust-toolchain` file a
    /// project carries, and it is the compiler the recipe names.
    #[test]
    fn every_build_runs_the_recipe_s_own_compiler() {
        let command = cargo(
            Path::new("source"),
            Path::new("build"),
            Path::new("cargo"),
            "agent",
        );
        let chosen = command
            .get_envs()
            .find(|(name, _)| *name == "RUSTUP_TOOLCHAIN")
            .and_then(|(_, value)| value);
        assert_eq!(chosen, Some(std::ffi::OsStr::new(RUST)));
        assert!(TOOLCHAIN.contains(RUST));
    }

    /// A build is given none of the build service's own environment, only
    /// what compiling needs.
    #[test]
    fn a_build_is_given_only_what_compiling_needs() {
        let command = cargo(
            Path::new("builds/job/source"),
            Path::new("builds/job/build"),
            Path::new("builds/job/cargo"),
            "agent",
        );
        let mut given: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_some())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        given.sort();
        given.retain(|name| name != "RUSTUP_HOME");
        assert_eq!(
            given,
            [
                "CARGO_HOME",
                "CARGO_TARGET_DIR",
                "HOME",
                "PATH",
                "RUSTUP_TOOLCHAIN"
            ]
        );
        let home = command
            .get_envs()
            .find(|(name, _)| *name == "HOME")
            .and_then(|(_, value)| value);
        assert_eq!(home, Some(std::ffi::OsStr::new("builds/job")));
    }

    /// Each build is given a Cargo home of its own, so no config, crate or
    /// unpacked source one build writes there is read by another's.
    #[test]
    fn every_build_has_a_cargo_home_of_its_own() {
        let command = cargo(
            Path::new("builds/job/source"),
            Path::new("builds/job/build"),
            Path::new("builds/job/cargo"),
            "agent",
        );
        let home = command
            .get_envs()
            .find(|(name, _)| *name == "CARGO_HOME")
            .and_then(|(_, value)| value);
        assert_eq!(home, Some(std::ffi::OsStr::new("builds/job/cargo")));
    }

    /// A `Cargo.toml` or a Cargo config in any directory above a build, as
    /// one build could leave for the next, stops the build before the
    /// compiler runs, however far above it is.
    #[test]
    fn anything_cargo_reads_left_above_a_build_stops_it() {
        let root = scratch("config-above");
        let jobs = root.join("jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        let job = fresh(&jobs, "8801").unwrap();
        let source = job.join("source");
        std::fs::create_dir_all(&source).unwrap();
        // Whatever lies above the scratch directory is the machine's own.
        let clear = above(&root.join("x"));
        assert_eq!(above(&source), clear);

        for (directory, name) in [
            (job.clone(), ".cargo"),
            (jobs.clone(), ".cargo"),
            (root.clone(), ".cargo"),
            (job.clone(), "Cargo.toml"),
            (jobs.clone(), "Cargo.toml"),
            (root.clone(), "Cargo.toml"),
        ] {
            let left = directory.join(name);
            if name == ".cargo" {
                std::fs::create_dir_all(&left).unwrap();
            } else {
                std::fs::write(&left, "[workspace]\n").unwrap();
            }
            assert_eq!(
                above(&source),
                Some(left.clone()),
                "{} was not seen",
                left.display()
            );
            assert_eq!(
                build(&source, Path::new("sdk"), Path::new("vendor"), "agent").err(),
                Some((Failure::Service, 0))
            );
            let _ = std::fs::remove_dir_all(&left);
            let _ = std::fs::remove_file(&left);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A job's directory is new: one already there under its name, as an
    /// earlier build could have made, is never built in.
    #[test]
    fn a_job_is_built_only_in_a_directory_made_for_it() {
        let root = scratch("fresh");
        let one = fresh(&root, "8801").unwrap();
        let two = fresh(&root, "8801").unwrap();
        assert_ne!(one, two);
        std::fs::create_dir_all(root.join("taken")).unwrap();
        assert!(
            std::fs::DirBuilder::new()
                .create(root.join("taken"))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every build is made `--locked` and `--offline`, with crates.io
    /// replaced by the vendored copy of the reviewed crates, so the only
    /// crates it can compile are the platform's reviewed ones.
    #[test]
    fn every_build_reaches_only_the_vendored_crates() {
        let command = cargo(
            Path::new("source"),
            Path::new("build"),
            Path::new("cargo"),
            "agent",
        );
        let args: Vec<_> = command.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--locked")));
        assert!(args.contains(&std::ffi::OsStr::new("--offline")));

        let written: toml::Table = config(Path::new("/opt/sdk"), Path::new("/opt/vendor \"x\""))
            .parse()
            .unwrap();
        assert_eq!(
            written["source"]["crates-io"]["replace-with"].as_str(),
            Some("reviewed")
        );
        assert_eq!(
            written["source"]["reviewed"]["directory"].as_str(),
            Some("/opt/vendor \"x\"")
        );
        assert_eq!(
            written["patch"]["crates-io"][shared::dependencies::SDK]["path"].as_str(),
            Some("/opt/sdk")
        );
        assert_eq!(written["net"]["offline"].as_bool(), Some(true));
    }

    #[test]
    fn a_component_past_the_limit_is_refused_unread() {
        let root = scratch("component-limit");
        let wasm = root.join("agent.wasm");
        let file = std::fs::File::create(&wasm).unwrap();
        file.set_len(COMPONENT_LIMIT + 1).unwrap();
        assert_eq!(read_component(&wasm).err(), Some(Failure::Build));
        assert_eq!(load_apart(&wasm).err(), Some(Failure::Build));
        file.set_len(4).unwrap();
        assert_eq!(read_component(&wasm).map(|bytes| bytes.len()), Ok(4));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_package_is_the_one_the_package_table_names() {
        let root = scratch("package-table");
        std::fs::write(
            root.join("Cargo.toml"),
            "[lib]\nname = \"helper\"\n\n[package]\nname = \"agent\"\n",
        )
        .unwrap();
        assert_eq!(
            package_of(&root.join("shonin.toml")).as_deref(),
            Ok("agent")
        );

        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"agent\"]\n",
        )
        .unwrap();
        let refused = package_of(&root.join("shonin.toml"));
        assert!(
            refused.as_ref().is_err_and(|why| why.contains("[package]")),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A group given up on is killed whole, what its leader started too.
    #[test]
    fn a_group_given_up_on_is_killed_whole() {
        let root = scratch("group");
        let started = root.join("started");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!("sleep 120 & echo $! > {}; wait", started.display()),
        ]);
        let group = Group::spawn(command).unwrap();
        let begun = Instant::now();
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&started)
                .ok()
                .filter(|pid| !pid.trim().is_empty())
            {
                break pid.trim().to_string();
            }
            assert!(begun.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(group);
        let alive = || {
            Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        let killed = Instant::now();
        while alive() && killed.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let outlived = alive();
        if outlived {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
        assert!(
            !outlived,
            "a process the group's leader started outlived it"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// No model is shipped in a bundle yet, so a model file counts towards
    /// the limit as any file does.
    #[test]
    fn a_model_file_counts_towards_the_size_limit() {
        let root = scratch("size");
        std::fs::write(root.join("lib.rs"), vec![b'x'; 100]).unwrap();
        std::fs::write(root.join("model.onnx"), vec![0u8; 5000]).unwrap();
        assert_eq!(source_size(&root), 5100);
    }
}
