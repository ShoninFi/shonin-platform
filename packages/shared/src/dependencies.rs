//! What a project may depend on.
//!
//! The reviewer reads the upload and nothing else, so a crate the build
//! fetched from elsewhere would be code nobody reviewed. A project may depend
//! only on crates inside its own archive, the strategy SDK the build service
//! carries, and the crates.io crates the platform has reviewed itself and
//! listed in `config/dependency-allowlist.toml`, each by name, version and
//! checksum: the pinned dependency snapshot. The build service refuses any
//! other before an upload is paid for, and builds offline against a vendored
//! copy of exactly that list, so nothing else could be compiled anyway.
//!
//! A lock names the SDK from crates.io, as Cargo writes it for an author who
//! depends on it as on any crate, at the version the build service carries
//! or an earlier one compatible with it. The lock only names it: the build
//! service decides what the SDK is, and builds against its own copy, the
//! lock's entry made the path package that copy is (see
//! [`with_carried_sdk`]), whatever checksum the entry held. A lock naming the
//! SDK as a path package, patched to a copy of it, is refused.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

/// The platform's list, as committed. It is compiled in, so the build service
/// that checks against it is the image that carries it.
pub const ALLOWLIST: &str = include_str!("../../../config/dependency-allowlist.toml");

/// How `Cargo.lock` names crates.io.
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The strategy SDK, which a lock names from crates.io. The build service
/// patches in its own copy, and makes the lock's entry the path package of
/// that copy before it builds.
pub const SDK: &str = "shonin-strategy-sdk";

/// One crate on the list: crates.io's package of `name` at `version`, whose
/// SHA-256 is `checksum`, as `Cargo.lock` records it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

impl Crate {
    /// The directory a vendored copy of it is unpacked into.
    pub fn directory(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// The crates a project may depend on from crates.io.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    crates: Vec<Crate>,
    /// The version of the SDK the build service carries, where the SDK may
    /// be named from crates.io (see [`Allowlist::carrying`]).
    sdk: Option<semver::Version>,
}

impl Allowlist {
    /// The platform's own list, [`ALLOWLIST`].
    pub fn platform() -> Self {
        Self::from_toml(ALLOWLIST).expect("the committed dependency allowlist reads")
    }

    /// This list, taking the SDK from crates.io too: at `carried`, the
    /// version of the SDK the build service carries and builds every agent
    /// against, or at an earlier version compatible with it. A list not told
    /// what the build service carries takes no SDK at all.
    pub fn carrying(self, carried: &str) -> Result<Self, String> {
        let sdk = semver::Version::parse(carried)
            .map_err(|error| format!("the SDK's version {carried}: {error}"))?;
        Ok(Self {
            sdk: Some(sdk),
            ..self
        })
    }

    /// The version of the SDK the build service carries, if this list was
    /// told it.
    pub fn carried(&self) -> Option<&semver::Version> {
        self.sdk.as_ref()
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct Listed {
            #[serde(rename = "crate", default)]
            crates: Vec<Crate>,
        }
        let listed: Listed =
            toml::from_str(text).map_err(|error| format!("the allowlist: {error}"))?;
        let mut seen = std::collections::BTreeSet::new();
        for listed in &listed.crates {
            if !seen.insert((&listed.name, &listed.version)) {
                return Err(format!(
                    "the allowlist names {} {} twice",
                    listed.name, listed.version
                ));
            }
            let hex = listed.checksum.len() == 64
                && listed.checksum.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !hex {
                return Err(format!(
                    "the allowlist's checksum for {} {} is not 64 hex digits",
                    listed.name, listed.version
                ));
            }
        }
        Ok(Self {
            crates: listed.crates,
            sdk: None,
        })
    }

    pub fn crates(&self) -> &[Crate] {
        &self.crates
    }

    /// The listed crate `name` at `version`, if it is listed.
    pub fn get(&self, name: &str, version: &str) -> Option<&Crate> {
        self.crates
            .iter()
            .find(|listed| listed.name == name && listed.version == version)
    }
}

/// Every `Cargo.toml` and `Cargo.lock` in a packed project, by path.
fn manifests(archive: &[u8]) -> Result<BTreeMap<PathBuf, String>, String> {
    let unreadable = |_| "the archive could not be read".to_string();
    let mut found = BTreeMap::new();
    let mut entries = tar::Archive::new(Cursor::new(archive));
    for entry in entries.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = normal(&entry.path().map_err(unreadable)?);
        let named = path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml" || name == "Cargo.lock");
        if !named {
            continue;
        }
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
        found.insert(path, text);
    }
    Ok(found)
}

/// A path's named parts only, so `./a/b` and `a/b` are the same path.
fn normal(path: &Path) -> PathBuf {
    path.components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .collect()
}

/// `relative`, read from the directory `from` inside the archive, if it stays
/// inside: lexically, `..` by `..`, as nothing in an archive is a link.
fn within(from: &Path, relative: &str) -> Option<PathBuf> {
    let mut at = normal(from);
    for part in Path::new(relative).components() {
        match part {
            Component::Normal(name) => at.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !at.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(at)
}

/// The crates.io crates a packed project depends on, every one of them on
/// `allowed`, or why it may not be built. `manifest_path` is the agent's
/// `shonin.toml` inside the archive.
///
/// Refused: a project with no `Cargo.lock` beside its package or above it;
/// a lock file naming a git dependency, another registry, or a crates.io
/// crate not on the list or under another checksum; a lock naming the SDK
/// as a path package, or from crates.io at a version the build service does
/// not take (see
/// [`sdk_taken`]), or in a way it cannot rewrite (see [`with_carried_sdk`]);
/// a path package no `Cargo.toml` in the archive declares; and a
/// `Cargo.toml` naming a git or registry source for a dependency or a patch,
/// or naming any path outside the archive: a dependency's, a patch's, its
/// workspace's, a target's (`[lib]`, `[[bin]]` and the rest) or its build
/// script's, under whatever spelling Cargo reads.
pub fn checked(
    archive: &[u8],
    manifest_path: &str,
    allowed: &Allowlist,
) -> Result<Vec<Crate>, String> {
    let files = manifests(archive)?;
    let manifest = normal(Path::new(manifest_path));
    let beside = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let has_lock = beside
        .ancestors()
        .any(|directory| files.contains_key(&directory.join("Cargo.lock")));
    if !has_lock {
        return Err(
            "the project carries no Cargo.lock: the build is made --locked from the one the \
             project carries, so commit it beside the agent's Cargo.toml or its workspace's"
                .into(),
        );
    }

    let mut packages = Vec::new();
    for (path, text) in files
        .iter()
        .filter(|(path, _)| path.ends_with("Cargo.toml"))
    {
        let Ok(cargo) = text.parse::<toml::Table>() else {
            return Err(format!("{} is not valid TOML", path.display()));
        };
        let directory = path.parent().unwrap_or(Path::new(""));
        sources(&cargo, directory, path)?;
        if let Some(name) = cargo
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
        {
            packages.push(name.to_string());
        }
    }

    let mut depended = BTreeMap::new();
    for (path, text) in files
        .iter()
        .filter(|(path, _)| path.ends_with("Cargo.lock"))
    {
        #[derive(serde::Deserialize)]
        struct Lock {
            #[serde(rename = "package", default)]
            packages: Vec<Locked>,
        }
        #[derive(serde::Deserialize)]
        struct Locked {
            name: String,
            version: String,
            source: Option<String>,
            checksum: Option<String>,
        }
        let lock: Lock = toml::from_str(text)
            .map_err(|_| format!("{} is not a lock file Cargo reads", path.display()))?;
        for locked in lock.packages {
            let named = format!("{} {}", locked.name, locked.version);
            match locked.source.as_deref() {
                None if locked.name == SDK => {
                    return Err(format!(
                        "{} locks {named} as a path package: take {SDK} from crates.io, as \
                         any crate, and drop any [patch] for it; the build service builds \
                         against its own copy",
                        path.display()
                    ));
                }
                None if packages.contains(&locked.name) => {}
                None => {
                    return Err(format!(
                        "{} locks {named} from a path outside the project: a project may \
                         depend only on crates inside it, the SDK and the platform's \
                         reviewed crates.io list",
                        path.display()
                    ));
                }
                Some(CRATES_IO) if locked.name == SDK => {
                    sdk_taken(&locked.version, allowed.carried()).map_err(|why| {
                        format!("{} locks {named} from crates.io: {why}", path.display())
                    })?;
                }
                Some(CRATES_IO) => {
                    let listed = allowed.get(&locked.name, &locked.version);
                    match (listed, locked.checksum.as_deref()) {
                        (Some(listed), Some(checksum)) if listed.checksum == checksum => {
                            depended.insert(listed.directory(), listed.clone());
                        }
                        (Some(_), _) => {
                            return Err(format!(
                                "{} locks {named} under a checksum other than the one the \
                                 platform reviewed",
                                path.display()
                            ));
                        }
                        (None, _) => {
                            return Err(format!(
                                "{} locks {named}, which is not on the platform's reviewed \
                                 list of crates.io crates: a project may depend only on \
                                 crates inside it, the SDK and that list (ask for a crate \
                                 to be reviewed and added)",
                                path.display()
                            ));
                        }
                    }
                }
                Some(source) if source.starts_with("git+") => {
                    return Err(format!(
                        "{} locks {named} from git: a project may depend only on crates \
                         inside it, the SDK and the platform's reviewed crates.io list",
                        path.display()
                    ));
                }
                Some(_) => {
                    return Err(format!(
                        "{} locks {named} from a registry other than crates.io",
                        path.display()
                    ));
                }
            }
        }
        // What is checked here is what the build service builds: the lock
        // with the SDK from crates.io made the copy it carries.
        if let Some(carried) = allowed.carried() {
            with_carried_sdk(text, &carried.to_string())
                .map_err(|why| format!("{} {why}", path.display()))?;
        }
    }
    Ok(depended.into_values().collect())
}

/// Whether a lock may name the SDK from crates.io at `version`, the build
/// service carrying the SDK at `carried`: at `carried` itself, or at an
/// earlier version it is compatible with, as Cargo reads compatible (`^`).
/// The build service builds against its own copy, so a lock naming a newer
/// version, or one its copy is not compatible with, names an SDK the agent
/// would not be built against.
pub fn sdk_taken(version: &str, carried: Option<&semver::Version>) -> Result<(), String> {
    let Some(carried) = carried else {
        return Err(format!(
            "this check is not told which {SDK} the build service carries, so it takes no SDK"
        ));
    };
    let Ok(named) = semver::Version::parse(version) else {
        return Err(format!("{version} is not a version Cargo reads"));
    };
    let compatible = semver::Comparator {
        op: semver::Op::Caret,
        major: named.major,
        minor: Some(named.minor),
        patch: Some(named.patch),
        pre: named.pre.clone(),
    };
    if compatible.matches(carried) {
        return Ok(());
    }
    let what = if named > *carried {
        "newer than"
    } else {
        "not compatible with"
    };
    Err(format!(
        "{version} is {what} the {SDK} the build service carries and builds every agent \
         against, {carried}: lock {carried} or an earlier version compatible with it \
         (cargo update -p {SDK} --precise {carried})"
    ))
}

/// `lock`, the text of a `Cargo.lock`, with its entry for the SDK from
/// crates.io made the path package the build service patches in: its
/// version `carried`, the version of the SDK the build service carries, and
/// no source or checksum, as Cargo writes that package. Every other entry,
/// and every other byte of the lock, is as it was. `None` when the lock
/// names no SDK from crates.io: nothing is rewritten.
///
/// Refused, as a lock the build would not build as it was checked: one
/// naming the SDK more than once, or writing its entry, or a reference to
/// it, otherwise than Cargo does (the rewritten lock is read back, and must
/// differ from the lock only in that entry). Cargo names a package by its
/// version in a reference only where two packages share its name, so for a
/// lock naming the SDK once it never does.
pub fn with_carried_sdk(lock: &str, carried: &str) -> Result<Option<String>, String> {
    let unwritten =
        || format!("names {SDK} otherwise than Cargo writes it: commit the lock Cargo writes");
    let read: toml::Table = lock
        .parse()
        .map_err(|_| "is not a lock file Cargo reads".to_string())?;
    let packages = read
        .get("package")
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let named = |package: &toml::Value, key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let is_sdk = |package: &toml::Value| named(package, "name").as_deref() == Some(SDK);
    let Some(at) = packages.iter().position(|package| {
        is_sdk(package) && named(package, "source").as_deref() == Some(CRATES_IO)
    }) else {
        return Ok(None);
    };
    if packages.iter().filter(|package| is_sdk(package)).count() > 1 {
        return Err(format!(
            "locks {SDK} more than once: the build service builds every agent against its one \
             copy"
        ));
    }
    // Where one package has a name, Cargo refers to it by the name alone; a
    // reference naming the version would name one no package has once the
    // version is the carried one.
    let qualified = packages
        .iter()
        .filter_map(|package| package.get("dependencies").and_then(toml::Value::as_array))
        .flatten()
        .filter_map(toml::Value::as_str)
        .any(|reference| reference != SDK && reference.split(' ').next() == Some(SDK));
    if qualified {
        return Err(unwritten());
    }

    // The lock, a block at a time: each runs from a line opening a table to
    // the next. Only the SDK's block from crates.io is touched.
    let name = format!("name = \"{SDK}\"");
    let source = format!("source = \"{CRATES_IO}\"");
    let mut rewritten = String::with_capacity(lock.len());
    let mut block: Vec<&str> = Vec::new();
    let flush = |block: &mut Vec<&str>, rewritten: &mut String| {
        let bare = |line: &str| line.trim_end_matches(['\n', '\r']).to_string();
        let sdk = block.iter().any(|line| bare(line) == name)
            && block.iter().any(|line| bare(line) == source);
        for line in block.drain(..) {
            let text = bare(line);
            if !sdk {
                rewritten.push_str(line);
            } else if text.starts_with("version = ") {
                rewritten.push_str(&format!("version = \"{carried}\""));
                rewritten.push_str(&line[text.len()..]);
            } else if !text.starts_with("source = ") && !text.starts_with("checksum = ") {
                rewritten.push_str(line);
            }
        }
    };
    for line in lock.split_inclusive('\n') {
        if line.starts_with('[') {
            flush(&mut block, &mut rewritten);
        }
        block.push(line);
    }
    flush(&mut block, &mut rewritten);

    // Read back, it must be the lock with that one entry changed.
    let mut expected = read.clone();
    if let Some(entry) = expected
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
        .and_then(|packages| packages.get_mut(at))
        .and_then(toml::Value::as_table_mut)
    {
        entry.remove("source");
        entry.remove("checksum");
        entry.insert("version".into(), toml::Value::String(carried.into()));
    }
    let reread: toml::Table = rewritten.parse().map_err(|_| unwritten())?;
    if reread != expected {
        return Err(unwritten());
    }
    Ok(Some(rewritten))
}

/// Refuse a `Cargo.toml` that takes a dependency, a patch or its workspace
/// from anywhere but crates.io or a path inside the archive, or that names
/// any other path outside the archive (see [`paths_outside`]).
fn sources(cargo: &toml::Table, directory: &Path, path: &Path) -> Result<(), String> {
    // Cargo still reads the underscore spellings, with a warning, on the
    // editions before 2024.
    const KINDS: [&str; 5] = [
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "dev_dependencies",
        "build_dependencies",
    ];

    let mut tables: Vec<(&toml::Table, String)> = Vec::new();
    fn add<'a>(
        tables: &mut Vec<(&'a toml::Table, String)>,
        table: Option<&'a toml::Value>,
        what: String,
    ) {
        if let Some(table) = table.and_then(toml::Value::as_table) {
            tables.push((table, what));
        }
    }
    for kind in KINDS {
        add(&mut tables, cargo.get(kind), kind.to_string());
    }
    if let Some(targets) = cargo.get("target").and_then(toml::Value::as_table) {
        for (target, table) in targets {
            for kind in KINDS {
                add(
                    &mut tables,
                    table.get(kind),
                    format!("target.{target}.{kind}"),
                );
            }
        }
    }
    add(
        &mut tables,
        cargo
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
        "workspace.dependencies".into(),
    );
    if let Some(patches) = cargo.get("patch").and_then(toml::Value::as_table) {
        for (source, table) in patches {
            add(&mut tables, Some(table), format!("patch.{source}"));
        }
    }
    add(&mut tables, cargo.get("replace"), "replace".into());

    for (table, what) in tables {
        for (name, dependency) in table {
            let Some(dependency) = dependency.as_table() else {
                continue;
            };
            for key in ["git", "registry", "registry-index"] {
                if dependency.contains_key(key) {
                    return Err(format!(
                        "{} takes {name} in [{what}] from {}: a project may depend only on \
                         crates inside it, the SDK and the platform's reviewed crates.io list",
                        path.display(),
                        if key == "git" {
                            "git"
                        } else {
                            "another registry"
                        },
                    ));
                }
            }
            if let Some(relative) = dependency.get("path").and_then(toml::Value::as_str)
                && within(directory, relative).is_none()
            {
                return Err(format!(
                    "{} takes {name} in [{what}] from {relative}, outside the project: a path \
                     dependency must be inside the upload, where the reviewer reads it",
                    path.display()
                ));
            }
        }
    }
    if let Some(relative) = cargo
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str)
        && within(directory, relative).is_none()
    {
        return Err(format!(
            "{} names a workspace outside the project: {relative}",
            path.display()
        ));
    }
    if let Some((what, relative)) = paths_outside(cargo, directory).into_iter().next() {
        return Err(format!(
            "{} names {relative} in {what}, outside the project: everything a build compiles \
             must be inside the upload, where the reviewer reads it",
            path.display()
        ));
    }
    Ok(())
}

/// Every path a `Cargo.toml` names that is outside the archive, read from
/// `directory`, its own, each with where it is named. Every field Cargo reads
/// a path from is looked at, wherever it is: any `path` at any depth (a
/// dependency's or a patch's under any table spelling, `[lib]`, `[[bin]]`,
/// `[[example]]`, `[[test]]`, `[[bench]]`), the build script
/// (`package.build`), the workspace a package names (`package.workspace`)
/// and a workspace's members. Only `metadata`, which Cargo never reads, is
/// passed over.
fn paths_outside(cargo: &toml::Table, directory: &Path) -> Vec<(String, String)> {
    fn walk(value: &toml::Value, at: &str, directory: &Path, found: &mut Vec<(String, String)>) {
        match value {
            toml::Value::Table(table) => {
                for (key, inner) in table {
                    let here = if at.is_empty() {
                        key.clone()
                    } else {
                        format!("{at}.{key}")
                    };
                    // Free-form, for tools other than Cargo, which reads
                    // nothing there.
                    if here == "package.metadata" || here == "workspace.metadata" {
                        continue;
                    }
                    if key == "path" {
                        named(inner, &here, directory, found);
                    }
                    walk(inner, &here, directory, found);
                }
            }
            toml::Value::Array(items) => {
                for item in items {
                    walk(item, at, directory, found);
                }
            }
            _ => {}
        }
    }
    /// A path, or a list of them, as a field holds it.
    fn named(value: &toml::Value, at: &str, directory: &Path, found: &mut Vec<(String, String)>) {
        match value {
            toml::Value::String(relative) => {
                if within(directory, relative).is_none() {
                    found.push((at.to_string(), relative.clone()));
                }
            }
            toml::Value::Array(items) => {
                for item in items {
                    named(item, at, directory, found);
                }
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    walk(
        &toml::Value::Table(cargo.clone()),
        "",
        directory,
        &mut found,
    );
    for (table, field) in [
        ("package", "build"),
        ("package", "workspace"),
        ("workspace", "members"),
        ("workspace", "default-members"),
    ] {
        if let Some(value) = cargo.get(table).and_then(|inner| inner.get(field)) {
            named(value, &format!("{table}.{field}"), directory, &mut found);
        }
    }
    found
}

/// The crates.io crates a packed project's lock files name, by name and
/// version, for the reviewer: source it is told of and does not read, the
/// platform having reviewed it. Unreadable lock files name none (a project
/// with one is refused before it is paid for).
pub fn listed(archive: &[u8]) -> Vec<(String, String)> {
    #[derive(serde::Deserialize)]
    struct Lock {
        #[serde(rename = "package", default)]
        packages: Vec<Locked>,
    }
    #[derive(serde::Deserialize)]
    struct Locked {
        name: String,
        version: String,
        source: Option<String>,
    }
    let mut named = std::collections::BTreeSet::new();
    for (path, text) in manifests(archive).unwrap_or_default() {
        if !path.ends_with("Cargo.lock") {
            continue;
        }
        let Ok(lock) = toml::from_str::<Lock>(&text) else {
            continue;
        };
        // The SDK is the platform's own, and the build service builds
        // against its own copy, whichever version the lock names.
        for locked in lock.packages {
            if locked.source.as_deref() == Some(CRATES_IO) && locked.name != SDK {
                named.insert((locked.name, locked.version));
            }
        }
    }
    named.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_allowlist_reads_and_names_the_sdk_s_own_dependencies() {
        let allowed = Allowlist::platform();
        for name in ["futures", "serde", "ciborium", "fastnum", "wit-bindgen"] {
            assert!(
                allowed.crates().iter().any(|listed| listed.name == name),
                "{name} is not on the allowlist"
            );
        }
    }

    /// A project in memory: an agent depending on the SDK, locked by `lock`.
    fn agent_locking(lock: &str) -> Vec<u8> {
        let files = [
            (
                "Cargo.toml",
                format!(
                    "[package]\nname = \"agent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                     [dependencies]\n{SDK} = \"0.1\"\n"
                ),
            ),
            ("Cargo.lock", lock.to_string()),
            ("src/lib.rs", "pub fn agent() {}\n".to_string()),
        ];
        let mut archive = tar::Builder::new(Vec::new());
        for (path, text) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, path, text.as_bytes())
                .unwrap();
        }
        archive.into_inner().unwrap()
    }

    /// A lock holding the agent and the SDK's entry `sdk`.
    fn lock_with(sdk: &str) -> String {
        format!(
            "# This file is automatically @generated by Cargo.\nversion = 4\n\n\
             [[package]]\nname = \"agent\"\nversion = \"0.1.0\"\n\
             dependencies = [\n \"{SDK}\",\n]\n\n{sdk}"
        )
    }

    fn from_crates_io(version: &str) -> String {
        format!(
            "[[package]]\nname = \"{SDK}\"\nversion = \"{version}\"\nsource = \"{CRATES_IO}\"\n\
             checksum = \"{}\"\n",
            "ab".repeat(32)
        )
    }

    /// The SDK's entry as the build service rewrites it: the path package
    /// of its copy at `version`.
    fn as_carried(version: &str) -> String {
        format!("[[package]]\nname = \"{SDK}\"\nversion = \"{version}\"\n")
    }

    fn carrying(version: &str) -> Allowlist {
        Allowlist::platform().carrying(version).unwrap()
    }

    #[test]
    fn the_sdk_is_taken_at_the_carried_version_or_an_earlier_compatible_one() {
        let carried = semver::Version::parse("0.1.1").unwrap();
        for taken in ["0.1.0", "0.1.1"] {
            assert_eq!(sdk_taken(taken, Some(&carried)), Ok(()), "{taken}");
        }
        for (refused, why) in [
            ("0.1.2", "newer than"),
            ("0.2.0", "newer than"),
            ("1.0.0", "newer than"),
            ("0.0.9", "not compatible with"),
        ] {
            let said = sdk_taken(refused, Some(&carried)).unwrap_err();
            assert!(
                said.contains(why)
                    && said.contains("carries and builds every agent against, 0.1.1"),
                "{refused}: {said}"
            );
        }

        let carried = semver::Version::parse("1.2.3").unwrap();
        assert_eq!(sdk_taken("1.0.0", Some(&carried)), Ok(()));
        assert!(sdk_taken("1.2.4", Some(&carried)).is_err());
        assert!(sdk_taken("2.0.0", Some(&carried)).is_err());
        assert!(sdk_taken("0.9.0", Some(&carried)).is_err());

        // Before 0.1, every version is compatible only with itself.
        let carried = semver::Version::parse("0.0.3").unwrap();
        assert_eq!(sdk_taken("0.0.3", Some(&carried)), Ok(()));
        assert!(sdk_taken("0.0.2", Some(&carried)).is_err());

        assert!(sdk_taken("one", Some(&carried)).is_err());
        assert!(sdk_taken("0.0.3", None).is_err());
    }

    #[test]
    fn a_lock_naming_the_sdk_from_crates_io_passes_under_any_checksum() {
        let allowed = carrying("0.1.1");
        for version in ["0.1.0", "0.1.1"] {
            let archive = agent_locking(&lock_with(&from_crates_io(version)));
            assert_eq!(checked(&archive, "shonin.toml", &allowed), Ok(vec![]));
            let other =
                lock_with(&from_crates_io(version)).replace(&"ab".repeat(32), &"0".repeat(64));
            assert_eq!(
                checked(&agent_locking(&other), "shonin.toml", &allowed),
                Ok(vec![])
            );
        }
    }

    #[test]
    fn a_lock_naming_the_sdk_by_path_is_refused() {
        for allowed in [carrying("0.1.1"), Allowlist::platform()] {
            for version in ["0.1.0", "0.1.1"] {
                let archive = agent_locking(&lock_with(&as_carried(version)));
                let said = checked(&archive, "shonin.toml", &allowed).unwrap_err();
                assert!(
                    said.starts_with(&format!(
                        "Cargo.lock locks {SDK} {version} as a path package: take {SDK} from \
                         crates.io"
                    )) && said.contains("drop any [patch] for it"),
                    "{said}"
                );
            }
        }
    }

    #[test]
    fn a_lock_naming_a_newer_sdk_is_refused_naming_the_carried_version() {
        let allowed = carrying("0.1.1");
        for version in ["0.1.2", "0.2.0"] {
            let archive = agent_locking(&lock_with(&from_crates_io(version)));
            let said = checked(&archive, "shonin.toml", &allowed).unwrap_err();
            assert!(
                said.starts_with(&format!("Cargo.lock locks {SDK} {version} from crates.io"))
                    && said.contains("0.1.1"),
                "{said}"
            );
        }
    }

    #[test]
    fn the_sdk_s_name_is_taken_from_nowhere_else() {
        let allowed = carrying("0.1.1");
        for (source, why) in [
            ("git+https://example.invalid/sdk.git#0123", "from git"),
            (
                "registry+https://example.invalid/index",
                "from a registry other than crates.io",
            ),
            (
                "sparse+https://example.invalid/index/",
                "from a registry other than crates.io",
            ),
        ] {
            let lock = lock_with(&from_crates_io("0.1.1")).replace(CRATES_IO, source);
            let said = checked(&agent_locking(&lock), "shonin.toml", &allowed).unwrap_err();
            assert!(said.contains(why), "{source}: {said}");
        }
    }

    #[test]
    fn a_list_not_told_the_carried_sdk_takes_none() {
        let archive = agent_locking(&lock_with(&from_crates_io("0.1.1")));
        let said = checked(&archive, "shonin.toml", &Allowlist::platform()).unwrap_err();
        assert!(said.contains("takes no SDK"), "{said}");
    }

    #[test]
    fn the_sdk_from_crates_io_is_rewritten_as_the_path_package_and_nothing_else() {
        let lock = lock_with(&from_crates_io("0.1.0"));
        let rewritten = with_carried_sdk(&lock, "0.1.1").unwrap().unwrap();
        assert_eq!(rewritten, lock_with(&as_carried("0.1.1")));

        // Every other byte as it was, line endings and all.
        let crlf = lock.replace('\n', "\r\n") + "\r\n# the end\r\n";
        let rewritten = with_carried_sdk(&crlf, "0.1.1").unwrap().unwrap();
        assert_eq!(
            rewritten,
            lock_with(&as_carried("0.1.1")).replace('\n', "\r\n") + "\r\n# the end\r\n"
        );

        // Nothing to rewrite.
        assert_eq!(
            with_carried_sdk(&lock_with(&as_carried("0.1.1")), "0.1.1"),
            Ok(None)
        );
        assert_eq!(with_carried_sdk("version = 4\n", "0.1.1"), Ok(None));
    }

    #[test]
    fn a_lock_the_rewrite_would_not_build_as_checked_is_refused() {
        let allowed = carrying("0.1.1");
        let refused = [
            // The SDK twice.
            lock_with(&format!(
                "{}\n{}",
                from_crates_io("0.1.0"),
                from_crates_io("0.1.1")
            )),
            // Its entry written otherwise than Cargo writes it.
            lock_with(&from_crates_io("0.1.0").replace(&format!("\"{SDK}\""), &format!("'{SDK}'"))),
            // A reference to it naming its version.
            lock_with(&from_crates_io("0.1.0"))
                .replace(&format!(" \"{SDK}\",\n"), &format!(" \"{SDK} 0.1.0\",\n")),
        ];
        for lock in refused {
            assert!(with_carried_sdk(&lock, "0.1.1").is_err(), "{lock}");
            let said = checked(&agent_locking(&lock), "shonin.toml", &allowed).unwrap_err();
            assert!(said.starts_with("Cargo.lock "), "{said}");
        }
    }

    #[test]
    fn the_reviewer_is_not_told_the_sdk_is_a_reviewed_crate() {
        let futures = Allowlist::platform()
            .crates()
            .iter()
            .find(|listed| listed.name == "futures")
            .unwrap()
            .clone();
        let lock = format!(
            "{}\n[[package]]\nname = \"futures\"\nversion = \"{}\"\nsource = \"{CRATES_IO}\"\n\
             checksum = \"{}\"\n",
            lock_with(&from_crates_io("0.1.1")),
            futures.version,
            futures.checksum
        );
        assert_eq!(
            listed(&agent_locking(&lock)),
            vec![("futures".to_string(), futures.version)]
        );
    }

    #[test]
    fn a_path_is_inside_only_while_it_stays_inside() {
        assert_eq!(
            within(Path::new("agents/one"), "../two"),
            Some(PathBuf::from("agents/two"))
        );
        assert_eq!(within(Path::new("agents/one"), "../../.."), None);
        assert_eq!(within(Path::new(""), "/etc"), None);
        assert_eq!(within(Path::new(""), "./helper"), Some("helper".into()));
    }
}
