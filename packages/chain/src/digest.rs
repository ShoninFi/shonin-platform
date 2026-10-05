//! The digests a service signs, worked out here from the contracts'
//! definitions rather than read from a node.
//!
//! A key's signature over a digest is whatever the digest says it is: a
//! service that signed a digest a node handed it would sign anything the
//! node chose. So each digest a service signs is computed from the inputs
//! the service itself holds, the chain id it was configured with (checked
//! against the node at connection) and the contract's address from its
//! deployment, exactly as the contract writes it.

use alloy_primitives::{Address, B256, U256, keccak256};
use alloy_sol_types::SolValue;

use crate::bindings::strategies::ChainNeed;

/// `Upkeep.usageDigest(total, asOf)` of `vault`:
/// `keccak256(abi.encode(chainid, vault, "shonin-usage", total, asOf))`.
pub fn usage(chain_id: u64, vault: Address, total: U256, as_of: u64) -> B256 {
    keccak256(
        (
            U256::from(chain_id),
            vault,
            String::from("shonin-usage"),
            total,
            as_of,
        )
            .abi_encode_params(),
    )
}

/// `StrategyRegistry.clearanceDigest(strategyId, sourceHash)` of `registry`:
/// `keccak256(abi.encode(chainid, registry, "shonin-review-clearance",
/// strategyId, sourceHash))`.
pub fn clearance(chain_id: u64, registry: Address, strategy_id: U256, source_hash: B256) -> B256 {
    keccak256(
        (
            U256::from(chain_id),
            registry,
            String::from("shonin-review-clearance"),
            strategy_id,
            source_hash,
        )
            .abi_encode_params(),
    )
}

/// What a strategy is registered as, everything
/// `StrategyRegistry.registrationDigest` covers but the chain and the
/// registry.
pub struct Registration<'a> {
    pub strategy_id: U256,
    pub author: Address,
    pub source_hash: B256,
    pub bundle_hash: B256,
    pub manifest_hash: B256,
    pub chains: &'a [ChainNeed],
    pub bridges: &'a [B256],
    pub restricted: &'a [B256],
}

/// `StrategyRegistry.registrationDigest(...)` of `registry`:
/// `keccak256(abi.encode(chainid, registry, "shonin-strategy-registration",
/// strategyId, author, sourceHash, bundleHash, manifestHash,
/// keccak256(abi.encode(chains)), keccak256(abi.encode(bridges)),
/// keccak256(abi.encode(restricted))))`.
pub fn registration(chain_id: u64, registry: Address, of: &Registration<'_>) -> B256 {
    keccak256(
        (
            U256::from(chain_id),
            registry,
            String::from("shonin-strategy-registration"),
            of.strategy_id,
            of.author,
            of.source_hash,
            of.bundle_hash,
            of.manifest_hash,
            keccak256(of.chains.to_vec().abi_encode()),
            keccak256(of.bridges.to_vec().abi_encode()),
            keccak256(of.restricted.to_vec().abi_encode()),
        )
            .abi_encode_params(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};

    // The vectors were read from the contracts themselves: the compiled
    // `StrategyRegistry` and `Upkeep` deployed on a development chain (chain
    // 31337, the first two creations of its first account) and asked for
    // their digests of these inputs by `eth_call`.
    const CHAIN: u64 = 31_337;
    const REGISTRY: Address = address!("5fbdb2315678afecb367f032d93f642f64180aa3");
    const UPKEEP: Address = address!("e7f1725e7734ce288f8367e1bb143e90bb3f0512");

    #[test]
    fn the_usage_digest_is_the_vault_s() {
        assert_eq!(
            usage(CHAIN, UPKEEP, U256::from(123_456_789u64), 1_700_000_000),
            b256!("f2b427cd6f07c7a7733b2facb22a926eb21315a79158fe49bf923252b56d582d")
        );
    }

    #[test]
    fn the_clearance_digest_is_the_registry_s() {
        assert_eq!(
            clearance(CHAIN, REGISTRY, U256::from(7u64), B256::repeat_byte(0xab)),
            b256!("6ec7eb00135e49c5746438e3598546f88c7e836ce4fffaefc942111f01c31753")
        );
    }

    #[test]
    fn the_registration_digest_is_the_registry_s() {
        let chains = [ChainNeed {
            chain: B256::repeat_byte(0x01),
            drivers: vec![B256::repeat_byte(0x02), B256::repeat_byte(0x03)],
        }];
        let of = Registration {
            strategy_id: U256::from(7u64),
            author: Address::repeat_byte(0x33),
            source_hash: B256::repeat_byte(0xab),
            bundle_hash: B256::repeat_byte(0xcd),
            manifest_hash: B256::repeat_byte(0xef),
            chains: &chains,
            bridges: &[B256::repeat_byte(0x03)],
            restricted: &[B256::repeat_byte(0x04), B256::repeat_byte(0x01)],
        };
        assert_eq!(
            registration(CHAIN, REGISTRY, &of),
            b256!("24a002cf01a6f22f3a2968a2098b78e9314124aacccbbe38f9cf132c70f6d68f")
        );
    }
}
