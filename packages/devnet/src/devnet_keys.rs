//! The Solana programs' addresses on devnet. Each program that goes on
//! devnet is deployed at a key of the team's mnemonic
//! (`SHONIN_TESTNET_SOLANA_MNEMONIC`), along `m/44'/501'/<index>'/0'` at the
//! index this table gives it, so everyone deploying from the mnemonic deploys
//! the same ids, and each program's committed `devnet-id` is its key's
//! address.
//!
//! An index is never reused or renumbered: a program's devnet address is its
//! index's key, so moving it moves the program. A new program, or one moved
//! to a fresh address, takes the next index after the highest used so far
//! (the retired ones included), and a retired index is listed in
//! [`RETIRED`], so it is never handed out again. Below 100, index 0 is the
//! deployer and 1 the registry; the role keys are at 10 and up. Programs
//! that serve tests only have no index: they never leave a test.
//!
//! Two indices above 100 hold no program: 108, the stand-in dollar's mint
//! account, retired with the stand-in dollar, and [`MARKET`] (111), the
//! testnet market's account, which keeps Orca's pool at Pyth's price.
//!
//! The programs that fix the dollar for good in their state (publishing,
//! the stations, the vaults) and Orca's driver (final, and listing the
//! dollar's pool) moved to fresh indices when the devnet's dollar became
//! Circle's USDC: their first addresses keep the stand-in dollar's
//! deployment.

use std::path::Path;

use anyhow::Result;
use chain::solana::Signer;
use chain::solana::keys::{self, PROGRAM};

/// Every program deployed to devnet, by its library name, and its index.
pub const PROGRAM_KEYS: [(&str, u32); 11] = [
    ("shonin_registry", PROGRAM),
    ("shonin_prices", 101),
    ("shonin_profiles", 102),
    ("shonin_dev_verifier", 105),
    ("shonin_local_venue", 106),
    ("shonin_local_driver", 107),
    ("shonin_cs_verifier", 109),
    // 111 is the market's account.
    ("shonin_publishing", 112),
    ("shonin_station", 113),
    ("shonin_vault", 114),
    ("shonin_orca_driver", 115),
];

/// Indices no longer used, and never handed out again: the first addresses
/// of the programs that moved (publishing, the stations, the vaults, Orca's
/// driver) and the stand-in dollar's mint.
pub const RETIRED: [u32; 5] = [100, 103, 104, 108, 110];

/// The testnet market's own account on devnet: not a program and never a
/// role, so its key opens nothing of the platform's.
pub const MARKET: u32 = testnet_market::SOLANA_MARKET_INDEX;

/// Write each program's keypair, from `phrase`, into `dir` as
/// `<name>-keypair.json`, and return each name with its address.
pub fn write_program_keypairs(phrase: &str, dir: &Path) -> Result<Vec<(&'static str, String)>> {
    if let Some((name, index)) = PROGRAM_KEYS
        .iter()
        .find(|(_, index)| RETIRED.contains(index) || *index == MARKET)
    {
        anyhow::bail!("{name} is at {index}, an index no program may take");
    }
    let mut written = Vec::new();
    for (name, index) in PROGRAM_KEYS {
        let pair = keys::keypair_from_mnemonic(phrase, index)?;
        keys::write_keypair(&pair, &dir.join(format!("{name}-keypair.json")))?;
        written.push((name, pair.pubkey().to_string()));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use chain::solana::keys::DEPLOYER;

    /// The stand-in dollar's mint account's index, retired with it.
    const DOLLAR_MINT: u32 = 108;

    /// The first index a program other than the registry takes.
    const FIRST_PROGRAM_INDEX: u32 = 100;

    #[test]
    fn every_index_is_used_once_and_no_retired_one_again() {
        let mut indices: BTreeSet<u32> = PROGRAM_KEYS.iter().map(|(_, index)| *index).collect();
        assert_eq!(indices.len(), PROGRAM_KEYS.len());
        assert!(indices.insert(MARKET), "the market's index is a program's");
        for retired in RETIRED {
            assert!(indices.insert(retired), "{retired} is retired and in use");
        }
        assert!(RETIRED.contains(&DOLLAR_MINT));
        let names: BTreeSet<&str> = PROGRAM_KEYS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), PROGRAM_KEYS.len());
    }

    #[test]
    fn the_programs_holding_the_dollar_and_orca_s_driver_moved_past_the_market_s_index() {
        let index = |wanted: &str| {
            PROGRAM_KEYS
                .iter()
                .find(|(name, _)| *name == wanted)
                .map(|(_, index)| *index)
                .unwrap()
        };
        let moved = [
            "shonin_publishing",
            "shonin_station",
            "shonin_vault",
            "shonin_orca_driver",
        ];
        for (k, name) in moved.iter().enumerate() {
            assert_eq!(index(name), MARKET + 1 + k as u32, "{name}");
        }
        // Every index handed out before the move is at or below the last
        // retired one.
        let highest_before = *RETIRED.iter().max().unwrap();
        assert!(MARKET > highest_before);
        assert!(
            PROGRAM_KEYS
                .iter()
                .filter(|(name, _)| !moved.contains(name))
                .all(|(_, index)| *index < highest_before)
        );
    }

    #[test]
    fn the_deployer_is_at_0_and_the_registry_at_1() {
        assert_eq!(DEPLOYER, 0);
        assert!(PROGRAM_KEYS.iter().all(|(_, index)| *index != DEPLOYER));
        assert_eq!(
            PROGRAM_KEYS
                .iter()
                .find(|(name, _)| *name == "shonin_registry"),
            Some(&("shonin_registry", 1))
        );
    }

    #[test]
    fn every_other_program_is_at_100_or_above() {
        for (name, index) in PROGRAM_KEYS {
            if name != "shonin_registry" {
                assert!(index >= FIRST_PROGRAM_INDEX, "{name} is at {index}");
            }
        }
    }

    #[test]
    fn the_market_s_account_is_neither_a_role_nor_the_deployer_nor_the_registry() {
        assert_eq!(MARKET, 111);
        let last_role = keys::ROLE_BASE + chain::client::ROLE_COUNT as u32 - 1;
        for index in [MARKET, DOLLAR_MINT] {
            assert!(![DEPLOYER, PROGRAM].contains(&index));
            assert!(!(keys::ROLE_BASE..=last_role).contains(&index));
        }
    }

    #[test]
    fn the_confidential_space_checker_is_at_the_index_after_the_mint() {
        assert_eq!(
            PROGRAM_KEYS
                .iter()
                .find(|(name, _)| *name == "shonin_cs_verifier"),
            Some(&("shonin_cs_verifier", DOLLAR_MINT + 1))
        );
    }

    #[test]
    fn the_role_keys_stay_below_the_programs() {
        let last_role = keys::ROLE_BASE + chain::client::ROLE_COUNT as u32 - 1;
        assert!(last_role < FIRST_PROGRAM_INDEX);
        let roles = keys::ROLE_BASE..=last_role;
        assert!(PROGRAM_KEYS.iter().all(|(_, index)| !roles.contains(index)));
    }

    #[test]
    fn every_program_the_platform_deploys_has_an_index() {
        let deployed: BTreeSet<&str> = chain::solana::programs::compiled()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let indexed: BTreeSet<&str> = PROGRAM_KEYS.iter().map(|(name, _)| *name).collect();
        assert_eq!(deployed, indexed);
    }

    #[test]
    fn each_program_s_keypair_file_holds_its_derived_address() {
        let dir = std::env::temp_dir().join(format!("devnet-keys-{}", std::process::id()));
        let written = write_program_keypairs(keys::DEVELOPMENT_MNEMONIC, &dir).unwrap();
        for (name, address) in &written {
            let pair = keys::read_keypair(&dir.join(format!("{name}-keypair.json"))).unwrap();
            assert_eq!(pair.pubkey().to_string(), *address);
        }
        let addresses: BTreeSet<&String> = written.iter().map(|(_, address)| address).collect();
        assert_eq!(addresses.len(), PROGRAM_KEYS.len());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
