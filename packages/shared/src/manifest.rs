//! What an author commits, and the hashes that pin it.
//!
//! The manifest is the `shonin.toml` in a strategy's project: what it is, what
//! it may use, and which accounts it trades. The build service hashes it, the
//! registry stores the hash, and the vault's page shows what it said, so an
//! author's declaration and the thing that runs are the same object.
//!
//! Hashing is canonical on purpose. The same manifest hashes to the same
//! bytes on every machine, so two people who build the same source can compare
//! one number and know whether they got the same strategy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The version of this format. It is hashed along with the manifest, so a
/// later format can never collide with this one.
pub const SPEC_VERSION: &str = "shonin-strategy/1";

/// What an author declares about a strategy.
///
/// Maps are ordered, so the canonical encoding is the same whatever order the
/// author wrote them in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub strategy: StrategySection,
    pub description: Description,
    /// Passed to `run`. Decimals are strings, so no float ever rounds
    /// differently on two machines.
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// The venues it trades, and which of the vault's accounts it uses on
    /// each. The vault registers exactly these and no others.
    #[serde(default)]
    pub venues: BTreeMap<String, VenueSection>,
    pub runtime: Runtime,
    /// The names of the sealed secrets it reads. Never the values.
    #[serde(default)]
    pub secrets: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategySection {
    /// Stable across versions.
    pub id: String,
    pub version: u32,
    /// The version this one claims to replace, as `id@version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub title: String,
    /// The author's own words, shown as the author's words.
    pub text: String,
    /// The limits the strategy sets in its own code, said plainly, so a reader
    /// can check the page against the source.
    #[serde(default)]
    pub limits: Vec<String>,
    /// The hosts and feeds it depends on.
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VenueSection {
    pub accounts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Runtime {
    pub allowances: Allowances,
    /// Extras the strategy asks for beyond the world's own imports.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// The compute a strategy may use. Declared here, enforced as caps by the
/// executor, and charged to the vault's tank at the published prices.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Allowances {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
}

impl Allowances {
    /// Enough for a strategy that watches one venue and trades on a signal
    /// rather than on every tick.
    pub fn modest() -> Self {
        Self {
            fuel_per_sec: 2_000_000,
            memory_mib: 256,
            requests_per_min: 120,
            bytes_per_day: 50_000_000,
            streams: 4,
            inference_ms_per_min: 0,
        }
    }

    /// A strategy that carries a model: the same shape as a modest one, with
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

    /// A strategy that quotes: it answers every tick, so it needs the fuel to
    /// price one and the request rate to place what it priced.
    pub fn quoting() -> Self {
        Self {
            fuel_per_sec: 16_000_000,
            memory_mib: 1_024,
            requests_per_min: 6_000,
            bytes_per_day: 5_000_000_000,
            streams: 64,
            inference_ms_per_min: 0,
        }
    }

    /// The shapes an author picks between, named. They are starting points
    /// rather than a menu: a manifest may declare any allowances it likes, and
    /// what it declares is what its vault is capped at.
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
/// declared allowances become the cap the vault's contract enforces.
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
    /// statement may charge a tank.
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
    }
}

impl Manifest {
    /// Read a manifest from the `shonin.toml` in a project.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("a manifest serialises")
    }

    /// `id@version`: what a vault, a URL and a supersession claim all name.
    pub fn slug(&self) -> String {
        format!("{}-v{}", self.strategy.id, self.strategy.version)
    }

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
[strategy]
id = "funding-basis"
version = 1

[description]
title = "Funding basis"
text = "Holds spot, shorts the perp, collects funding."
limits = ["gross leverage at most 2x"]
dependencies = ["the venue's own price stream"]

[config]
entry_apr = "0.12"
exit_apr = "0.04"

[venues.local]
accounts = ["venue"]

[runtime.allowances]
fuel_per_sec = 2000000
memory_mib = 256
requests_per_min = 120
bytes_per_day = 50000000
streams = 4
inference_ms_per_min = 0
"#
    }

    #[test]
    fn a_manifest_reads_and_writes_the_same_thing() {
        let manifest = Manifest::from_toml(example()).expect("it parses");
        assert_eq!(manifest.strategy.id, "funding-basis");
        assert_eq!(manifest.slug(), "funding-basis-v1");
        assert_eq!(manifest.config["entry_apr"], "0.12");
        assert_eq!(manifest.venues["local"].accounts, vec!["venue"]);
        assert_eq!(manifest.runtime.allowances.memory_mib, 256);

        let round_tripped = Manifest::from_toml(&manifest.to_toml()).expect("it parses again");
        assert_eq!(round_tripped, manifest);
    }

    #[test]
    fn the_hash_does_not_depend_on_how_it_was_written() {
        let one = Manifest::from_toml(example()).expect("it parses");
        let reordered = example().replace(
            "entry_apr = \"0.12\"\nexit_apr = \"0.04\"",
            "exit_apr = \"0.04\"\nentry_apr = \"0.12\"",
        );
        let two = Manifest::from_toml(&reordered).expect("it parses");
        assert_eq!(one.hash(), two.hash());
    }

    #[test]
    fn a_different_manifest_hashes_differently() {
        let one = Manifest::from_toml(example()).expect("it parses");
        let mut two = one.clone();
        two.strategy.version = 2;
        assert_ne!(one.hash(), two.hash());
    }

    #[test]
    fn a_hash_converts_to_the_bytes_a_call_carries() {
        let hash = hash_bytes(b"some source");
        let bytes = hash_to_bytes(&hash).expect("it converts");
        assert_eq!(hash, format!("sha256:{}", hex::encode(bytes)));
        assert!(hash_to_bytes("not a hash").is_none());
    }

    #[test]
    fn a_days_allowances_cost_what_the_published_prices_say() {
        let cap = Prices::published().daily_cap(&Allowances::modest());
        // Two million units of fuel a second is two cores, so most of the
        // cost is the processor, and the whole day is a few dollars.
        assert!(cap > 5.0 && cap < 20.0, "a day cost {cap}");
    }
}
