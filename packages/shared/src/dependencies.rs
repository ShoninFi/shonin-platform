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

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

/// The platform's list, as committed. It is compiled in, so the build service
/// that checks against it is the image that carries it.
pub const ALLOWLIST: &str = include_str!("../../../config/dependency-allowlist.toml");

/// How `Cargo.lock` names crates.io.
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The strategy SDK. The build service patches in its own copy, so a lock
/// file holds it as a path package, and that is the one path package that
/// need not be in the archive.
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
}

impl Allowlist {
    /// The platform's own list, [`ALLOWLIST`].
    pub fn platform() -> Self {
        Self::from_toml(ALLOWLIST).expect("the committed dependency allowlist reads")
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
/// crate not on the list or under another checksum; a path package no
/// `Cargo.toml` in the archive declares (other than the SDK); and a
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
                None if locked.name == SDK || packages.contains(&locked.name) => {}
                None => {
                    return Err(format!(
                        "{} locks {named} from a path outside the project: a project may \
                         depend only on crates inside it, the SDK and the platform's \
                         reviewed crates.io list",
                        path.display()
                    ));
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
    }
    Ok(depended.into_values().collect())
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
        for locked in lock.packages {
            if locked.source.as_deref() == Some(CRATES_IO) {
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
