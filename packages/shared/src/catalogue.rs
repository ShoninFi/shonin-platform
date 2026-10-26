//! The published list of drivers and bridges, from `config/drivers.toml`.
//!
//! It is compiled in, so every machine image carries the list it was built
//! with, and which drivers an image accepts is part of what its attestation
//! measures.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::common::Chain;

const PUBLISHED: &str = include_str!("../../../config/drivers.toml");

/// Where a driver's station lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum DriverChain {
    /// A stand-in that runs on whichever chain the vault is on.
    Vault,
    On(Chain),
}

impl From<DriverChain> for String {
    fn from(chain: DriverChain) -> String {
        match chain {
            DriverChain::Vault => "vault".into(),
            DriverChain::On(chain) => chain.name().into(),
        }
    }
}

impl TryFrom<String> for DriverChain {
    type Error = String;
    fn try_from(name: String) -> Result<Self, String> {
        if name == "vault" {
            return Ok(Self::Vault);
        }
        name.parse().map(Self::On)
    }
}

/// Who stops a vault's keys sending money anywhere but its own accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Custody {
    /// The venue: the key it is given can trade, and only the station can
    /// move money out.
    Venue,
    /// The station's own account, whose rules allow only the venue's calls.
    Station,
}

/// Who closes positions when someone leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Exit {
    /// In the leaver's own transaction.
    Contract,
    /// By the contracts alone, but in later transactions: the venue acts on
    /// the station's orders after the block, so a close is placed, then
    /// proved and swept home.
    Delayed,
    Unwinder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Valuation {
    OnChain,
    AttestedRead,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverEntry {
    /// What a depositor sees: the venue, never the driver.
    pub label: String,
    pub chain: DriverChain,
    /// The networks it is offered on today.
    pub networks: Vec<String>,
    /// Who its venue's terms exclude: country codes, subdivision codes
    /// (`CA-ON`), or SANCTIONED.
    pub restricted: Vec<String>,
    pub custody: Custody,
    pub exit: Exit,
    pub valuation: Valuation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeEntry {
    pub label: String,
    pub networks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub drivers: BTreeMap<String, DriverEntry>,
    pub bridges: BTreeMap<String, BridgeEntry>,
}

impl Catalogue {
    /// The list as published in this build.
    pub fn published() -> Self {
        toml::from_str(PUBLISHED).expect("config/drivers.toml is a valid list")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_list_reads() {
        let catalogue = Catalogue::published();
        assert_eq!(catalogue.drivers["local"].chain, DriverChain::Vault);
        assert_eq!(
            catalogue.drivers["lighter"].chain,
            DriverChain::On(Chain::Robinhood)
        );
        assert!(
            catalogue.drivers["local"]
                .networks
                .contains(&"local".to_string())
        );
        assert!(
            !catalogue.drivers["local"]
                .networks
                .contains(&"mainnet".to_string())
        );
        // A driver not yet built is offered nowhere, mainnet least of all.
        assert!(catalogue.drivers["lighter"].networks.is_empty());
        assert!(catalogue.bridges.contains_key("local-bridge"));
    }

    /// SANCTIONED, an ISO 3166 country code (`GB`), or an ISO 3166-2
    /// subdivision code (`CA-ON`): what fits the contracts' 32-byte codes.
    fn is_restricted_code(code: &str) -> bool {
        let upper = |part: &str, least: usize, most: usize| {
            (least..=most).contains(&part.len())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        if code == "SANCTIONED" {
            return true;
        }
        match code.split_once('-') {
            None => code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase()),
            Some((country, subdivision)) => {
                country.len() == 2
                    && country.bytes().all(|b| b.is_ascii_uppercase())
                    && upper(subdivision, 1, 3)
            }
        }
    }

    #[test]
    fn a_restricted_code_is_a_country_a_subdivision_or_sanctioned() {
        for good in ["SANCTIONED", "US", "CA-ON", "GB-ENG", "FR-75"] {
            assert!(is_restricted_code(good), "{good}");
        }
        for bad in [
            "us",
            "USA",
            "CA-",
            "CA-ONTARIO",
            "-ON",
            "C-ON",
            "Sanctioned",
        ] {
            assert!(!is_restricted_code(bad), "{bad}");
        }
    }

    #[test]
    fn hyperliquid_is_offered_on_testnet_and_mainnet_with_the_driver_contract_s_list() {
        let catalogue = Catalogue::published();
        let hyperliquid = &catalogue.drivers["hyperliquid"];
        assert_eq!(hyperliquid.chain, DriverChain::On(Chain::Hyperliquid));
        assert_eq!(hyperliquid.networks, vec!["testnet", "mainnet"]);
        // The same codes, in the same order, as `HyperliquidDriver.restricted()`.
        assert_eq!(hyperliquid.restricted, vec!["CA-ON", "SANCTIONED", "US"]);
        assert_eq!(hyperliquid.exit, Exit::Delayed);
    }

    #[test]
    fn the_uniswap_driver_on_base_is_its_own_entry_with_the_robinhood_driver_s_terms() {
        let catalogue = Catalogue::published();
        let base = &catalogue.drivers["uniswap-base"];
        let robinhood = &catalogue.drivers["robinhood"];
        assert_eq!(base.chain, DriverChain::On(Chain::Base));
        assert_eq!(robinhood.chain, DriverChain::On(Chain::Robinhood));
        for driver in [base, robinhood] {
            assert_eq!(driver.networks, vec!["testnet", "mainnet"]);
            assert_eq!(driver.custody, Custody::Station);
            assert_eq!(driver.exit, Exit::Contract);
        }
        // One contract: what its `restricted()` answers, on both chains, in
        // the same order as `RobinhoodDriver.restricted()`.
        assert_eq!(base.restricted, robinhood.restricted);
        assert_eq!(
            robinhood.restricted,
            vec![
                "BY",
                "CA",
                "CH",
                "CU",
                "GB",
                "IR",
                "KP",
                "MM",
                "RU",
                "SANCTIONED",
                "SD",
                "SS",
                "SY",
                "UA",
                "US",
                "VE",
            ]
        );
        // A name the registry can hold: 32 bytes at most.
        assert!("uniswap-base".len() <= 32);
    }

    #[test]
    fn orca_s_list_is_its_driver_program_s() {
        // The same codes, in the same order, as the Orca driver's `restricted`.
        assert_eq!(
            Catalogue::published().drivers["orca"].restricted,
            vec![
                "AF",
                "BA",
                "BI",
                "BY",
                "CD",
                "CF",
                "CG",
                "CU",
                "GN",
                "GW",
                "HT",
                "IQ",
                "IR",
                "KP",
                "LB",
                "LY",
                "ML",
                "MM",
                "NI",
                "SANCTIONED",
                "SD",
                "SO",
                "SS",
                "SY",
                "UA-09",
                "UA-14",
                "UA-40",
                "UA-43",
                "VE",
                "YE",
                "ZW",
            ]
        );
    }

    #[test]
    fn every_restricted_list_is_sorted_codes() {
        for (name, driver) in Catalogue::published().drivers {
            let mut sorted = driver.restricted.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(
                sorted, driver.restricted,
                "{name}'s list is sorted and without repeats"
            );
            for code in driver.restricted {
                assert!(is_restricted_code(&code), "{name}: {code:?}");
            }
        }
    }
}
