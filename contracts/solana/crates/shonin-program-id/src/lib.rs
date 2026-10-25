//! A program's address, decided at build time so that no local address is
//! ever committed. Every program's `build.rs` calls [`emit`].
//!
//! With the `mainnet` feature it is the committed id in `mainnet-id`, and
//! with the `devnet` feature the committed id in `devnet-id`, both next to
//! the program's `Cargo.toml`. A devnet id is the address of a keypair
//! derived from the team's mnemonic, which `task contracts:solana:deploy
//! NETWORK=testnet` writes. Otherwise it is the address of the keypair Anchor
//! deploys the program with, `target/deploy/<name>-keypair.json`, which never
//! leaves the machine. Without that keypair, as on a fresh checkout's `cargo
//! check`, a placeholder stands in.
//!
//! A mainnet id is chosen when the programs are first deployed, from keypairs
//! the owner holds. The mainnet deploy builds before any `mainnet-id` is
//! written, so a mainnet build may instead take its ids from the file
//! [`MAINNET_IDS_VAR`] names (one `<library name> <id>` a line): a rehearsal
//! builds that way and never writes the committed files. A committed id that
//! differs from the listed one is refused, as is a program the list leaves
//! out.

use std::path::{Path, PathBuf};

/// The system program's address: a valid key, and obviously not ours.
const PLACEHOLDER: &str = "11111111111111111111111111111111";

/// The variable naming a list of mainnet ids to build with, beside or
/// before the committed `mainnet-id` files.
pub const MAINNET_IDS_VAR: &str = "SHONIN_MAINNET_IDS";

/// Where a program's address comes from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// The id committed for a public network, in `file` next to the
    /// program's `Cargo.toml`.
    Committed {
        network: &'static str,
        file: &'static str,
    },
    /// The local deploy keypair's, or the placeholder without one.
    LocalKeypair,
}

/// Where the address comes from, for a build with or without the `mainnet`
/// and `devnet` features. A build is for one network, so both are refused.
pub fn source(mainnet: bool, devnet: bool) -> Result<Source, &'static str> {
    match (mainnet, devnet) {
        (true, true) => Err(
            "a program is built for one network: the mainnet and devnet features together are refused",
        ),
        (true, false) => Ok(Source::Committed {
            network: "mainnet",
            file: "mainnet-id",
        }),
        (false, true) => Ok(Source::Committed {
            network: "devnet",
            file: "devnet-id",
        }),
        (false, false) => Ok(Source::LocalKeypair),
    }
}

/// The id committed for `network` in the file at `path`, given its contents
/// (none when it is missing), or why there is none to build with.
pub fn committed_id(network: &str, path: &Path, text: Option<&str>) -> Result<String, String> {
    let id = text.unwrap_or_default().trim();
    if id.is_empty() {
        let mut why = format!(
            "no {network} id is committed yet: it goes in {}",
            path.display()
        );
        if network == "devnet" {
            why.push_str(", and `task contracts:solana:deploy NETWORK=testnet` derives it");
        }
        return Err(why);
    }
    match bs58::decode(id).into_vec() {
        Ok(bytes) if bytes.len() == 32 => Ok(id.to_string()),
        _ => Err(format!(
            "{} holds {id:?}, which is not a program id",
            path.display()
        )),
    }
}

/// The id a list of mainnet ids (`<library name> <id>` a line, `#` starting
/// a comment) gives `lib_name`, checked against the committed id when there
/// is one (`committed`, the committed file's contents). A list without the
/// program, or naming another id than the committed one, is refused.
pub fn listed_mainnet_id(
    lib_name: &str,
    list: &str,
    committed_path: &Path,
    committed: Option<&str>,
) -> Result<String, String> {
    let listed = list
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next() == Some(lib_name)).then(|| parts.next().unwrap_or_default())
        })
        .ok_or_else(|| format!("{MAINNET_IDS_VAR} lists no id for {lib_name}"))?;
    let id = committed_id("mainnet", Path::new(MAINNET_IDS_VAR), Some(listed))?;
    if let Some(text) = committed.filter(|text| !text.trim().is_empty()) {
        let held = committed_id("mainnet", committed_path, Some(text))?;
        if held != id {
            return Err(format!(
                "{} holds {held}, and {MAINNET_IDS_VAR} lists {id} for {lib_name}: a committed mainnet id is never replaced",
                committed_path.display()
            ));
        }
    }
    Ok(id)
}

/// Writes `program_id.rs` into `OUT_DIR` for the program whose library is
/// named `lib_name` (as in `[lib] name`), for `include!` in its `lib.rs`.
pub fn emit(lib_name: &str) {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let id = program_id(
        lib_name,
        &manifest,
        std::env::var_os("CARGO_FEATURE_MAINNET").is_some(),
        std::env::var_os("CARGO_FEATURE_DEVNET").is_some(),
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    std::fs::write(
        out.join("program_id.rs"),
        format!("anchor_lang::declare_id!(\"{id}\");\n"),
    )
    .expect("writing the program id");
}

/// The address, in base58, of the program whose library is named `lib_name`
/// and whose `Cargo.toml` is in `program_dir`, for a build for mainnet, devnet
/// or neither: what [`emit`] compiles into the program, for a build script
/// that compiles the same id into a crate that calls the program. Tells cargo
/// to run the build script again when the file it is read from changes, and
/// panics, failing the build, where [`emit`] would.
pub fn program_id(lib_name: &str, program_dir: &Path, mainnet: bool, devnet: bool) -> String {
    let source = source(mainnet, devnet).unwrap_or_else(|why| panic!("{why}"));
    match source {
        Source::Committed { network, file } => {
            let path = program_dir.join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            let text = std::fs::read_to_string(&path).ok();
            if network == "mainnet" {
                println!("cargo:rerun-if-env-changed={MAINNET_IDS_VAR}");
                if let Some(list) = std::env::var_os(MAINNET_IDS_VAR).filter(|v| !v.is_empty()) {
                    let list = PathBuf::from(list);
                    println!("cargo:rerun-if-changed={}", list.display());
                    let listed = std::fs::read_to_string(&list).unwrap_or_else(|error| {
                        panic!("reading {MAINNET_IDS_VAR} ({}): {error}", list.display())
                    });
                    return listed_mainnet_id(lib_name, &listed, &path, text.as_deref())
                        .unwrap_or_else(|why| panic!("{why}"));
                }
            }
            committed_id(network, &path, text.as_deref()).unwrap_or_else(|why| panic!("{why}"))
        }
        Source::LocalKeypair => {
            let keypair = program_dir.join(format!("../../target/deploy/{lib_name}-keypair.json"));
            println!("cargo:rerun-if-changed={}", keypair.display());
            match std::fs::read_to_string(&keypair) {
                Ok(text) => address_of(&text),
                Err(_) => PLACEHOLDER.to_string(),
            }
        }
    }
}

/// A keypair file is the secret then the public key, as 64 numbers.
fn address_of(text: &str) -> String {
    let bytes: Vec<u8> = text
        .trim()
        .trim_matches(['[', ']'])
        .split(',')
        .map(|n| n.trim().parse().expect("a keypair file holds numbers"))
        .collect();
    assert_eq!(bytes.len(), 64, "a keypair file holds 64 bytes");
    bs58::encode(&bytes[32..]).into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS";

    #[test]
    fn each_network_feature_reads_its_own_committed_file() {
        assert_eq!(
            source(true, false),
            Ok(Source::Committed {
                network: "mainnet",
                file: "mainnet-id"
            })
        );
        assert_eq!(
            source(false, true),
            Ok(Source::Committed {
                network: "devnet",
                file: "devnet-id"
            })
        );
    }

    #[test]
    fn no_network_feature_uses_the_local_keypair() {
        assert_eq!(source(false, false), Ok(Source::LocalKeypair));
    }

    #[test]
    fn mainnet_and_devnet_together_are_refused() {
        assert!(source(true, true).is_err());
    }

    #[test]
    fn a_missing_or_empty_committed_id_is_refused_naming_its_file() {
        let path = Path::new("programs/p/devnet-id");
        for text in [None, Some(""), Some("  \n")] {
            let why = committed_id("devnet", path, text).unwrap_err();
            assert!(why.contains("no devnet id is committed yet"), "{why}");
            assert!(why.contains("programs/p/devnet-id"), "{why}");
            assert!(
                why.contains("contracts:solana:deploy NETWORK=testnet"),
                "{why}"
            );
        }
        let why = committed_id("mainnet", Path::new("programs/p/mainnet-id"), None).unwrap_err();
        assert!(why.contains("no mainnet id is committed yet"), "{why}");
        assert!(why.contains("programs/p/mainnet-id"), "{why}");
        assert!(!why.contains("contracts:solana:deploy"), "{why}");
    }

    #[test]
    fn a_committed_id_is_read_trimmed() {
        let path = Path::new("devnet-id");
        assert_eq!(
            committed_id("devnet", path, Some(&format!("{ID}\n"))),
            Ok(ID.to_string())
        );
    }

    #[test]
    fn a_committed_id_that_is_not_an_address_is_refused() {
        let path = Path::new("devnet-id");
        assert!(committed_id("devnet", path, Some("not-an-id")).is_err());
        assert!(committed_id("devnet", path, Some("1111")).is_err());
    }

    const OTHER: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";

    #[test]
    fn a_listed_mainnet_id_is_built_with_when_nothing_is_committed() {
        let list = format!("# rehearsal\nshonin_vault {OTHER}\nshonin_registry {ID}\n");
        let path = Path::new("programs/shonin-registry/mainnet-id");
        assert_eq!(
            listed_mainnet_id("shonin_registry", &list, path, None),
            Ok(ID.to_string())
        );
        assert_eq!(
            listed_mainnet_id("shonin_registry", &list, path, Some(&format!("{ID}\n"))),
            Ok(ID.to_string())
        );
    }

    #[test]
    fn a_listed_mainnet_id_other_than_the_committed_one_is_refused() {
        let list = format!("shonin_registry {ID}\n");
        let path = Path::new("programs/shonin-registry/mainnet-id");
        let why = listed_mainnet_id("shonin_registry", &list, path, Some(OTHER)).unwrap_err();
        assert!(why.contains("never replaced"), "{why}");
    }

    #[test]
    fn a_mainnet_id_list_without_the_program_or_with_a_bad_id_is_refused() {
        let path = Path::new("mainnet-id");
        assert!(listed_mainnet_id(
            "shonin_station",
            &format!("shonin_registry {ID}"),
            path,
            None
        )
        .is_err());
        assert!(
            listed_mainnet_id("shonin_station", "shonin_station not-an-id", path, None).is_err()
        );
        assert!(listed_mainnet_id("shonin_station", "shonin_station", path, None).is_err());
    }

    #[test]
    fn a_keypair_file_s_address_is_its_public_half() {
        let mut bytes = vec![7u8; 32];
        bytes.extend(bs58::decode(ID).into_vec().unwrap());
        let text = format!(
            "[{}]",
            bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(address_of(&text), ID);
    }
}
