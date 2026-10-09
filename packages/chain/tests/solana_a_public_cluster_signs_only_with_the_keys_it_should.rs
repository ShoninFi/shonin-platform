//! What a public Solana cluster's keys lean on in the chain crate:
//! - the published development phrase is refused on a testnet however it is
//!   spaced (the check compares derived keys, not strings);
//! - mainnet only reads, whatever the environment holds;
//! - a keeper's own key is refused unless it is the deployment's keeper,
//!   its text never in the error, and it never signs as another role;
//! - a cluster key is answered only by its own genesis;
//! - no committed devnet program id is a key of the development phrase
//!   (anyone could deploy or upgrade at those).

#![cfg(feature = "solana")]

use chain::Role;
use chain::config::NetworkName;
use chain::solana::Signer;
use chain::solana::client::check_cluster;
use chain::solana::keys::{self, DEVELOPMENT_MNEMONIC, RoleKeys, Signing};

const TEAM: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn the_development_phrase_is_refused_on_a_testnet_however_spaced() {
    let spaced = DEVELOPMENT_MNEMONIC.replace(' ', "  \t ");
    for phrase in [
        DEVELOPMENT_MNEMONIC.to_string(),
        spaced,
        format!("\n{DEVELOPMENT_MNEMONIC}\n"),
    ] {
        if Signing::from(NetworkName::Testnet, Some(&phrase), None).is_ok() {
            // Accepted only if it derives other keys than the development
            // phrase does.
            let keys = RoleKeys::from_mnemonic(&phrase).unwrap();
            let dev = RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap();
            assert_ne!(keys.deployer.pubkey(), dev.deployer.pubkey(), "{phrase:?}");
        }
    }
}

#[test]
fn mainnet_signs_nothing_whatever_the_environment_holds() {
    let keeper = RoleKeys::from_mnemonic(TEAM)
        .unwrap()
        .get(Role::Keeper)
        .insecure_clone();
    let text = bs58::encode(keeper.to_bytes()).into_string();
    let signing = Signing::from(NetworkName::Mainnet, Some(TEAM), Some(&text)).unwrap();
    assert!(signing.get(Role::Keeper).is_none());
    assert!(signing.get(Role::Admin).is_none());
    assert!(signing.find(&keeper.pubkey()).is_none());
}

#[test]
fn a_keeper_key_for_another_deployment_is_refused_without_quoting_it() {
    let keeper = RoleKeys::from_mnemonic(TEAM)
        .unwrap()
        .get(Role::Keeper)
        .insecure_clone();
    let text = bs58::encode(keeper.to_bytes()).into_string();
    let signing = Signing::from(NetworkName::Testnet, None, Some(&text)).unwrap();
    let other = RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC)
        .unwrap()
        .address(Role::Keeper);
    let error = format!(
        "{:#}",
        signing.check_keeper(&other.to_string()).unwrap_err()
    );
    assert!(!error.contains(&text));
    // The keeper key never signs as another role.
    for role in [Role::Admin, Role::Operator, Role::Executor, Role::Attestor] {
        assert!(signing.get(role).is_none());
    }
}

#[test]
fn a_cluster_key_is_answered_only_by_its_own_genesis() {
    const MAINNET: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
    const DEVNET: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
    assert!(check_cluster("devnet", DEVNET, "d").is_ok());
    assert!(check_cluster("devnet", MAINNET, "d").is_err());
    assert!(check_cluster("localnet", DEVNET, "l").is_err());
    assert!(check_cluster("localnet", MAINNET, "l").is_err());
    assert!(check_cluster("mainnet-beta", DEVNET, "m").is_err());
}

#[test]
fn no_committed_devnet_id_is_a_key_of_the_development_phrase() {
    // The indices of packages/devnet/src/devnet_keys.rs.
    let table = [
        ("shonin-registry", 1u32),
        ("shonin-publishing", 100),
        ("shonin-prices", 101),
        ("shonin-profiles", 102),
        ("shonin-station", 103),
        ("shonin-vault", 104),
        ("shonin-dev-verifier", 105),
        ("shonin-local-venue", 106),
        ("shonin-local-driver", 107),
        ("shonin-cs-verifier", 109),
    ];
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/solana/programs");
    for (program, index) in table {
        let id = std::fs::read_to_string(root.join(program).join("devnet-id")).unwrap();
        let id = id.trim();
        for i in [index, 0, 108] {
            let dev = keys::keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, i)
                .unwrap()
                .pubkey()
                .to_string();
            assert_ne!(
                id, dev,
                "{program}'s devnet id is the development phrase's key {i}"
            );
        }
    }
}
