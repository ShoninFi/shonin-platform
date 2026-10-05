//! Where the contracts are, and who holds which role.
//!
//! The deploy writes this file and every service reads it, so nothing has a
//! contract address compiled into it and a fresh chain is one file away from
//! being the chain everything talks to.
//!
//! An EVM chain's file is [`Deployments`]; a Solana cluster's is
//! [`SolanaDeployments`], whose addresses are 32-byte keys written in base58.
//! [`ChainDeployments`] reads either, by the kind of chain its key names, so
//! a reader that serves every chain needs one call.

use std::collections::BTreeMap;

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use crate::config::{ChainKey, ChainKind};

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
    /// Creates every vault's stations, at the same address on every chain.
    pub station_factory: Address,
    /// The stand-in venue's driver and the stand-in bridge, approved through
    /// the registry under `local` and `local-bridge`.
    pub driver: Address,
    pub bridge: Address,
    /// Stops, empties, values and closes a halted vault in one transaction.
    /// Holds nothing and answers to nobody, so it is at the same address on
    /// every chain. A deployment from before it existed has none.
    #[serde(default)]
    pub closer: Address,
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

/// What a Solana deploy produced: each program's id, and on a development
/// cluster the platform's own accounts.
///
/// The least a deploy writes is `{"programs": {...}}`, each program by its
/// library name (`shonin_registry`); everything else is absent until the
/// platform is set up on the cluster, so a deploy that only put programs
/// there still reads. Every address is a base58 string, as Solana writes one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SolanaDeployments {
    /// The chain, as the chain config names it: `solana:localnet`.
    #[serde(default)]
    pub chain_key: String,
    /// The cluster's name: `localnet`, `devnet`, `mainnet-beta`. Every
    /// digest a machine signs on it is bound to this.
    #[serde(default)]
    pub cluster: String,
    /// Where the cluster answers, for anything that has only this file.
    #[serde(default)]
    pub rpc_url: String,
    /// The cluster's genesis hash when it was deployed to: a local cluster
    /// started again is a new cluster, and this tells the file is stale.
    #[serde(default)]
    pub genesis_hash: String,
    /// The slot the deploy began in: where an indexer starts reading.
    #[serde(default)]
    pub deployed_slot: u64,
    /// Each program's id, by its library name.
    pub programs: BTreeMap<String, String>,
    /// The dollar's mint, and the token program that owns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_program: Option<String>,
    /// The development checker's state account the registry trusts, where
    /// it trusts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
    /// Who each role is on this cluster, where the deploy set the platform
    /// up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<SolanaRoles>,
}

/// The platform's roles on a Solana cluster, each a base58 key. The same
/// parts as [`Roles`], plus the account that deployed the programs and set
/// each one up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SolanaRoles {
    pub deployer: String,
    pub admin: String,
    pub platform: String,
    pub attestor: String,
    pub builder: String,
    pub reviewer: String,
    pub executor: String,
    pub keeper: String,
    pub operator: String,
    pub author: String,
    pub viewer: String,
}

impl SolanaDeployments {
    /// Read the file a deploy wrote.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("addresses serialise")
    }

    /// A program's id by its library name, if it was deployed.
    pub fn program(&self, name: &str) -> Option<&str> {
        self.programs.get(name).map(String::as_str)
    }
}

/// One chain's deployment, whichever kind of chain it is.
#[derive(Clone, Debug)]
pub enum ChainDeployments {
    Evm(Box<Deployments>),
    Solana(Box<SolanaDeployments>),
}

impl ChainDeployments {
    /// Read a deployment file for the chain `key` names, as the shape that
    /// kind of chain's deploy writes.
    pub fn from_json(key: &ChainKey, json: &str) -> Result<Self, serde_json::Error> {
        Ok(match key.kind {
            ChainKind::Evm => Self::Evm(Box::new(Deployments::from_json(json)?)),
            ChainKind::Solana => Self::Solana(Box::new(SolanaDeployments::from_json(json)?)),
        })
    }

    /// The chain key the file records, which a file written before the
    /// platform was set up on a Solana cluster leaves empty.
    pub fn chain_key(&self) -> &str {
        match self {
            Self::Evm(deployments) => &deployments.chain_key,
            Self::Solana(deployments) => &deployments.chain_key,
        }
    }

    pub fn evm(&self) -> Option<&Deployments> {
        match self {
            Self::Evm(deployments) => Some(deployments),
            Self::Solana(_) => None,
        }
    }

    pub fn solana(&self) -> Option<&SolanaDeployments> {
        match self {
            Self::Solana(deployments) => Some(deployments),
            Self::Evm(_) => None,
        }
    }

    pub fn to_json(&self) -> String {
        match self {
            Self::Evm(deployments) => deployments.to_json(),
            Self::Solana(deployments) => deployments.to_json(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solana_file_with_only_its_programs_reads() {
        let json = r#"{"programs": {"shonin_registry": "11111111111111111111111111111111"}}"#;
        let key = ChainKey::new(ChainKind::Solana, "devnet");
        let read = ChainDeployments::from_json(&key, json).unwrap();
        let solana = read.solana().expect("a Solana deployment");
        assert_eq!(
            solana.program("shonin_registry"),
            Some("11111111111111111111111111111111")
        );
        assert!(solana.roles.is_none());
        assert_eq!(read.chain_key(), "");
    }

    #[test]
    fn a_solana_file_reads_back_what_was_written() {
        let mut programs = BTreeMap::new();
        programs.insert("shonin_vault".to_string(), "Vau1t".to_string());
        let written = SolanaDeployments {
            chain_key: "solana:localnet".into(),
            cluster: "localnet".into(),
            genesis_hash: "G".into(),
            programs,
            mint: Some("M".into()),
            roles: Some(SolanaRoles::default()),
            ..SolanaDeployments::default()
        };
        assert_eq!(
            SolanaDeployments::from_json(&written.to_json()).unwrap(),
            written
        );
    }
}
