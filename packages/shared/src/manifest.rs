//! What an author commits, and the hashes that pin it.
//!
//! The manifest is the `shonin.toml` in an agent's project. It only declares,
//! for the platform: the chain its vault lives on, the author's description
//! and claims, the drivers it trades through, the bridges it moves money over,
//! and who else may not deposit. The agent's own settings are constants in its
//! code, and its allowances and idle floor are set on the vault, so none of
//! those are here.
//!
//! The builder adds what the author doesn't write (the strategy's id, the
//! hashes, the toolchain) and hashes the two together, canonically, so the
//! same manifest hashes to the same bytes on every machine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalogue::{Catalogue, DriverChain};
use crate::model::common::Chain;

/// The version of the canonical format. It is hashed along with the manifest,
/// so a later format can never collide with this one.
pub const SPEC_VERSION: &str = "shonin-agent/1";

/// Who every vault refuses, whatever its drivers: US persons and sanctioned
/// persons.
pub const BASELINE_RESTRICTED: [&str; 2] = ["SANCTIONED", "US"];

/// What an author declares, as `shonin.toml` says it.
///
/// Unknown keys are refused, so a manifest written for an older format fails
/// with a message rather than being half read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub vault: VaultSection,
    pub description: Description,
    /// One table per driver, from the published list, with that driver's
    /// settings. Maps are ordered, so the hash doesn't depend on the order
    /// the author wrote them in.
    #[serde(default)]
    pub drivers: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub bridges: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Depositors::is_empty")]
    pub depositors: Depositors,
    /// Extras the agent asks for beyond the world's own imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSection {
    /// `base`, `hyperliquid`, `robinhood` or `solana`.
    pub chain: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// The name people see.
    pub title: String,
    /// The author's own words, shown as the author's words.
    pub text: String,
    /// What the agent promises about itself, one plain sentence each. The
    /// review checks the code against them.
    #[serde(default)]
    pub claims: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Depositors {
    /// Places whose residents may not deposit, as ISO 3166 country codes, on
    /// top of what the drivers and the platform already exclude. An author
    /// can only add to the list.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Depositors {
    pub fn is_empty(&self) -> bool {
        self.exclude.is_empty()
    }
}

/// One chain the agent trades on, with the drivers it uses there: what the
/// strategy registry records and the vault's stations are created from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainNeed {
    pub chain: Chain,
    pub drivers: Vec<String>,
}

impl Manifest {
    /// Read a manifest from the `shonin.toml` in a project.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|error| format!("shonin.toml: {}", error.message()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("a manifest serialises")
    }

    /// The vault chain, if it names one there is.
    pub fn vault_chain(&self) -> Result<Chain, String> {
        self.vault.chain.parse()
    }

    /// Check the manifest against the published list, as it is offered on
    /// `network` (`local`, `testnet` or `mainnet`). The builder refuses a
    /// manifest this rejects, before anything is reviewed or built.
    pub fn check(&self, catalogue: &Catalogue, network: &str) -> Result<(), String> {
        let vault_chain = self.vault_chain()?;
        if self.description.title.trim().is_empty() {
            return Err("[description] needs a title".into());
        }
        if self.drivers.is_empty() {
            return Err("name at least one driver, as a [drivers.<name>] table".into());
        }
        for name in self.drivers.keys() {
            let driver = catalogue.drivers.get(name).ok_or_else(|| {
                format!("there is no driver called {name:?} on the published list")
            })?;
            if !driver.networks.iter().any(|offered| offered == network) {
                return Err(format!("the {name} driver isn't offered on {network} yet"));
            }
            if !self.drivers[name].is_empty() {
                return Err(format!("the {name} driver takes no settings"));
            }
        }
        for name in self.bridges.keys() {
            let bridge = catalogue.bridges.get(name).ok_or_else(|| {
                format!("there is no bridge called {name:?} on the published list")
            })?;
            if !bridge.networks.iter().any(|offered| offered == network) {
                return Err(format!("the {name} bridge isn't offered on {network} yet"));
            }
            if !self.bridges[name].is_empty() {
                return Err(format!("the {name} bridge takes no settings"));
            }
        }
        let needs = self.chains(catalogue)?;
        if needs.iter().any(|need| need.chain != vault_chain) && self.bridges.is_empty() {
            return Err(
                "a driver on another chain than the vault's needs a bridge, as a [bridges.<name>] table"
                    .into(),
            );
        }
        for code in &self.depositors.exclude {
            let ok = code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase());
            if !ok {
                return Err(format!(
                    "[depositors] exclude takes two-letter country codes, like \"GB\", not {code:?}"
                ));
            }
        }
        Ok(())
    }

    /// Every chain the agent trades on, with its drivers there, the vault's
    /// own chain first.
    pub fn chains(&self, catalogue: &Catalogue) -> Result<Vec<ChainNeed>, String> {
        let vault_chain = self.vault_chain()?;
        let mut by_chain: BTreeMap<Chain, Vec<String>> = BTreeMap::new();
        for name in self.drivers.keys() {
            let driver = catalogue.drivers.get(name).ok_or_else(|| {
                format!("there is no driver called {name:?} on the published list")
            })?;
            let chain = match driver.chain {
                DriverChain::Vault => vault_chain,
                DriverChain::On(chain) => chain,
            };
            by_chain.entry(chain).or_default().push(name.clone());
        }
        let mut needs: Vec<ChainNeed> = Vec::new();
        if let Some(drivers) = by_chain.remove(&vault_chain) {
            needs.push(ChainNeed {
                chain: vault_chain,
                drivers,
            });
        }
        needs.extend(
            by_chain
                .into_iter()
                .map(|(chain, drivers)| ChainNeed { chain, drivers }),
        );
        Ok(needs)
    }

    /// Who the vault refuses: the platform's baseline, every driver's list,
    /// and the author's additions, sorted and without repeats.
    pub fn restricted(&self, catalogue: &Catalogue) -> Vec<String> {
        let mut all: BTreeSet<String> = BASELINE_RESTRICTED
            .iter()
            .map(|code| code.to_string())
            .collect();
        for name in self.drivers.keys() {
            if let Some(driver) = catalogue.drivers.get(name) {
                all.extend(driver.restricted.iter().cloned());
            }
        }
        all.extend(self.depositors.exclude.iter().cloned());
        all.into_iter().collect()
    }

    pub fn bridge_names(&self) -> Vec<String> {
        self.bridges.keys().cloned().collect()
    }
}

/// The manifest as the builder hashes it: the author's file, and what the
/// builder worked out inside the attested machine. Nothing here is taken from
/// the author but the file itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Canonical {
    pub strategy_id: u64,
    pub vault_chain: Chain,
    /// The wallet that paid to publish.
    pub author: String,
    pub bundle_hash: String,
    pub component_hash: String,
    pub source_hash: String,
    pub sealed: bool,
    pub world: String,
    pub toolchain: String,
    pub builder_image: String,
    pub manifest: Manifest,
}

impl Canonical {
    /// The hash the registry stores and the builder signs over.
    ///
    /// The format's name is hashed first, so the same bytes read under a later
    /// format can never produce the same hash.
    pub fn hash(&self) -> String {
        let mut bytes = Vec::new();
        ciborium::into_writer(self, &mut bytes).expect("a manifest encodes");
        let mut hasher = Sha256::new();
        hasher.update(SPEC_VERSION.as_bytes());
        hasher.update(&bytes);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

/// The compute an agent may use, set by its author on the live vault and
/// charged to its tank at the published prices. Not part of the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Allowances {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    /// Gas, in dollars a day.
    pub gas_per_day: f64,
}

impl Allowances {
    /// Enough for an agent that watches one venue and trades on a signal
    /// rather than on every tick.
    pub fn modest() -> Self {
        Self {
            fuel_per_sec: 2_000_000,
            memory_mib: 256,
            requests_per_min: 120,
            bytes_per_day: 50_000_000,
            streams: 4,
            inference_ms_per_min: 0,
            gas_per_day: 1.0,
        }
    }

    /// An agent that carries a model: the same shape as a modest one, with
    /// room to think and the bandwidth to fetch what it thinks about.
    pub fn thinking() -> Self {
        Self {
            fuel_per_sec: 4_000_000,
            memory_mib: 512,
            bytes_per_day: 500_000_000,
            inference_ms_per_min: 2_000,
            ..Self::modest()
        }
    }

    /// An agent that quotes: it answers every tick, so it needs the fuel to
    /// price one and the request rate to place what it priced.
    pub fn quoting() -> Self {
        Self {
            fuel_per_sec: 16_000_000,
            memory_mib: 1_024,
            requests_per_min: 6_000,
            bytes_per_day: 5_000_000_000,
            streams: 64,
            inference_ms_per_min: 0,
            gas_per_day: 5.0,
        }
    }

    /// The shapes an author picks between when activating, named. They are
    /// starting points rather than a menu.
    pub const SHAPES: [&'static str; 3] = ["modest", "thinking", "quoting"];

    /// One of [`Allowances::SHAPES`] by name. An unknown name is the modest
    /// shape, which is the one that asks for least.
    pub fn shape(name: &str) -> Self {
        match name {
            "quoting" => Self::quoting(),
            "thinking" => Self::thinking(),
            _ => Self::modest(),
        }
    }
}

/// The published price of each unit of an allowance, which is how a day's
/// allowances become the cap the vault's contract enforces. The contract
/// keeps the same list; this copy is for showing estimates.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Prices {
    /// A full core, per hour.
    pub vcpu_hour: f64,
    pub gib_hour: f64,
    pub gb_egress: f64,
    /// What a second of inference costs, at the processor rate.
    pub inference_second: f64,
}

impl Prices {
    /// The prices as published, per unit. They are the same for every vault,
    /// and a change to them is announced before it takes effect.
    pub fn published() -> Self {
        Self {
            vcpu_hour: 0.15,
            gib_hour: 0.01,
            gb_egress: 0.15,
            inference_second: 0.15 / 3_600.0,
        }
    }

    /// What a day at these allowances costs, which is the most a day's usage
    /// may charge a tank.
    pub fn daily_cap(&self, allowances: &Allowances) -> f64 {
        // Fuel stands for processor time: the executor's measured mapping is
        // what turns one into the other, and until it is measured a million
        // units of fuel a second is taken as one core.
        let cores = allowances.fuel_per_sec as f64 / 1_000_000.0;
        let memory_gib = allowances.memory_mib as f64 / 1_024.0;
        let egress_gb = allowances.bytes_per_day as f64 / 1_000_000_000.0;
        let inference_seconds = allowances.inference_ms_per_min as f64 / 1_000.0 * 60.0 * 24.0;

        cores * self.vcpu_hour * 24.0
            + memory_gib * self.gib_hour * 24.0
            + egress_gb * self.gb_egress
            + inference_seconds * self.inference_second
            + allowances.gas_per_day
    }
}

/// The same hash, over any bytes: source archives, bundles, reports.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// The 32 bytes behind a `sha256:...` string, for the calls that take a
/// `bytes32`.
pub fn hash_to_bytes(hash: &str) -> Option<[u8; 32]> {
    let hex_part = hash.strip_prefix("sha256:")?;
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_part, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> &'static str {
        r#"
[vault]
chain = "base"

[description]
title = "Funding basis"
text = "Holds spot, shorts the perp, collects funding."
claims = ["gross leverage at most 2x"]

[drivers.local]

[depositors]
exclude = ["GB"]
"#
    }

    fn catalogue() -> Catalogue {
        Catalogue::published()
    }

    #[test]
    fn a_manifest_reads_and_writes_the_same_thing() {
        let manifest = Manifest::from_toml(example()).expect("it parses");
        assert_eq!(manifest.vault_chain(), Ok(Chain::Base));
        assert_eq!(
            manifest.description.claims,
            vec!["gross leverage at most 2x"]
        );
        assert!(manifest.drivers.contains_key("local"));
        let round_tripped = Manifest::from_toml(&manifest.to_toml()).expect("it parses again");
        assert_eq!(round_tripped, manifest);
    }

    #[test]
    fn an_older_manifest_is_refused_with_a_message() {
        let old = "[strategy]\nid = \"x\"\nversion = 1\n";
        let error = Manifest::from_toml(old).expect_err("it is refused");
        assert!(error.contains("strategy"), "{error}");
    }

    #[test]
    fn a_manifest_is_checked_against_the_published_list() {
        let manifest = Manifest::from_toml(example()).expect("it parses");
        assert_eq!(manifest.check(&catalogue(), "local"), Ok(()));
        // The stand-in venue is never offered on mainnet.
        assert!(manifest.check(&catalogue(), "mainnet").is_err());

        let unknown = example().replace("[drivers.local]", "[drivers.nowhere]");
        let unknown = Manifest::from_toml(&unknown).expect("it parses");
        assert!(
            unknown
                .check(&catalogue(), "local")
                .unwrap_err()
                .contains("nowhere")
        );

        let not_yet = example().replace("[drivers.local]", "[drivers.hyperliquid]");
        let not_yet = Manifest::from_toml(&not_yet).expect("it parses");
        assert!(
            not_yet
                .check(&catalogue(), "local")
                .unwrap_err()
                .contains("isn't offered")
        );

        let bad_code = example().replace("[\"GB\"]", "[\"Britain\"]");
        let bad_code = Manifest::from_toml(&bad_code).expect("it parses");
        assert!(bad_code.check(&catalogue(), "local").is_err());
    }

    #[test]
    fn the_stand_in_driver_trades_on_the_vault_chain() {
        let manifest =
            Manifest::from_toml(&example().replace("\"base\"", "\"robinhood\"")).unwrap();
        let chains = manifest.chains(&catalogue()).unwrap();
        assert_eq!(
            chains,
            vec![ChainNeed {
                chain: Chain::Robinhood,
                drivers: vec!["local".into()]
            }]
        );
    }

    #[test]
    fn the_restricted_list_adds_the_baseline_the_drivers_and_the_author() {
        let manifest = Manifest::from_toml(example()).expect("it parses");
        assert_eq!(
            manifest.restricted(&catalogue()),
            vec!["GB", "SANCTIONED", "US"]
        );
    }

    fn canonical(manifest: Manifest) -> Canonical {
        Canonical {
            strategy_id: 7,
            vault_chain: Chain::Base,
            author: "0xa11ce".into(),
            bundle_hash: hash_bytes(b"bundle"),
            component_hash: hash_bytes(b"component"),
            source_hash: hash_bytes(b"source"),
            sealed: false,
            world: "shonin:agent@0.1.0".into(),
            toolchain: "rust".into(),
            builder_image: "sha256:builder".into(),
            manifest,
        }
    }

    #[test]
    fn the_hash_does_not_depend_on_how_it_was_written() {
        let two_drivers = "[vault]\nchain = \"base\"\n[description]\ntitle = \"t\"\ntext = \"x\"\n";
        let one = Manifest::from_toml(&format!(
            "{two_drivers}[drivers.local]\n[bridges.local-bridge]\n"
        ))
        .unwrap();
        let two = Manifest::from_toml(&format!(
            "{two_drivers}[bridges.local-bridge]\n[drivers.local]\n"
        ))
        .unwrap();
        assert_eq!(canonical(one).hash(), canonical(two).hash());
    }

    #[test]
    fn a_different_manifest_hashes_differently() {
        let one = canonical(Manifest::from_toml(example()).unwrap());
        let mut two = one.clone();
        two.strategy_id = 8;
        assert_ne!(one.hash(), two.hash());
    }

    #[test]
    fn a_hash_converts_to_the_bytes_a_call_carries() {
        let hash = hash_bytes(b"some source");
        let bytes = hash_to_bytes(&hash).expect("it converts");
        assert_eq!(hash, format!("sha256:{}", hex::encode(bytes)));
        assert!(hash_to_bytes("not a hash").is_none());
    }

    /// The contract's `Prices.dailyCost` gives 7,267,500 for the same shape
    /// without gas; this is the same figure in dollars.
    #[test]
    fn a_days_allowances_cost_what_the_contract_says() {
        let mut modest = Allowances::modest();
        modest.gas_per_day = 0.0;
        let cap = Prices::published().daily_cap(&modest);
        assert!((cap - 7.2675).abs() < 1e-9, "a day cost {cap}");
        modest.gas_per_day = 2.0;
        assert!((Prices::published().daily_cap(&modest) - 9.2675).abs() < 1e-9);
    }
}
