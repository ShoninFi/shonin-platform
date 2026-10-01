//! Where the contracts are, and who holds which role.
//!
//! The deploy writes this file and every service reads it, so nothing has a
//! contract address compiled into it and a fresh chain is one file away from
//! being the chain everything talks to.

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

/// The addresses a deploy produced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deployments {
    /// The chain, as the chain config names it: `evm:31337`.
    pub chain_key: String,
    pub chain_id: u64,
    /// Where the chain answers, for anything that has only this file.
    pub rpc_url: String,
    /// The block the deploy landed in: where an indexer starts reading.
    pub deployed_block: u64,
    pub usd: Address,
    pub venue: Address,
    pub attestation_verifier: Address,
    pub attestation: Address,
    pub publishing: Address,
    pub prices: Address,
    pub strategies: Address,
    pub reviews: Address,
    pub profiles: Address,
    pub factory: Address,
    pub roles: Roles,
}

/// Who each of the platform's roles is on this chain. On a development chain
/// they are accounts it published itself, which is what makes it one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Roles {
    /// Governs the allowlist and kills vaults.
    pub admin: Address,
    /// Receives the platform's fees.
    pub platform: Address,
    /// Stands in for the hardware a real deployment would trust.
    pub attestor: Address,
    pub builder: Address,
    pub reviewer: Address,
    pub executor: Address,
    pub keeper: Address,
    /// Moves prices and funding on the venue.
    pub operator: Address,
    /// Publishes the reference agents.
    pub author: Address,
    /// The account the web app's development wallet uses.
    pub viewer: Address,
}

impl Deployments {
    /// Where a chain's deployment file is in a directory of them: one file
    /// per chain, named for its key (`evm-31337.json`).
    pub fn file_in(dir: &std::path::Path, key: &crate::config::ChainKey) -> std::path::PathBuf {
        dir.join(format!("{}.json", key.file_stem()))
    }

    /// Read the file a deploy wrote.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("addresses serialise")
    }
}
