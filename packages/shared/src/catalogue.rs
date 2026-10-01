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
    Contract,
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
    /// Who its venue's terms exclude: country codes, or SANCTIONED.
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
        assert!(catalogue.bridges.contains_key("local-bridge"));
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
                assert!(
                    code == "SANCTIONED"
                        || (code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase()))
                );
            }
        }
    }
}
