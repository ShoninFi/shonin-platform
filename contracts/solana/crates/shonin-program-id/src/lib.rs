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

use std::path::{Path, PathBuf};

/// The system program's address: a valid key, and obviously not ours.
const PLACEHOLDER: &str = "11111111111111111111111111111111";

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

/// Writes `program_id.rs` into `OUT_DIR` for the program whose library is
/// named `lib_name` (as in `[lib] name`), for `include!` in its `lib.rs`.
pub fn emit(lib_name: &str) {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let source = source(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_some(),
        std::env::var_os("CARGO_FEATURE_DEVNET").is_some(),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    let id = match source {
        Source::Committed { network, file } => {
            let path = manifest.join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            let text = std::fs::read_to_string(&path).ok();
            committed_id(network, &path, text.as_deref()).unwrap_or_else(|why| panic!("{why}"))
        }
        Source::LocalKeypair => {
            let keypair = manifest.join(format!("../../target/deploy/{lib_name}-keypair.json"));
            println!("cargo:rerun-if-changed={}", keypair.display());
            match std::fs::read_to_string(&keypair) {
                Ok(text) => address_of(&text),
                Err(_) => PLACEHOLDER.to_string(),
            }
        }
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    std::fs::write(
        out.join("program_id.rs"),
        format!("anchor_lang::declare_id!(\"{id}\");\n"),
    )
    .expect("writing the program id");
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
