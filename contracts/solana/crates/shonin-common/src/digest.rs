//! Hashes that have to agree with every other chain's contracts.
//!
//! Leaves and roots use keccak256, as the EVM contracts do, so one attested
//! root serves an instance's vaults on every chain.

use solana_keccak_hasher::hashv;

/// keccak256 of the parts, concatenated.
pub fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    hashv(parts).to_bytes()
}

/// What stands for this cluster where an EVM contract uses its chain id:
/// `keccak256("solana:" + name)`, read as a 256-bit number. An EVM chain id
/// is a small number, so the two can never collide.
pub fn cluster_value(name: &str) -> [u8; 32] {
    keccak(&[b"solana:", name.as_bytes()])
}

/// A purpose string as it is bound into a digest: its keccak256, so every
/// field of a digest has a fixed width and no two layouts can read alike.
pub fn purpose(name: &str) -> [u8; 32] {
    keccak(&[name.as_bytes()])
}

/// The leaf an instance commits to for one vault and one bundle, served on
/// one chain: `keccak256(keccak256(vaultChain ‖ vault ‖ strategyHash ‖
/// thisChain))`, each field 32 bytes, laid out as the EVM's `abi.encode` lays
/// out four 32-byte words. A Solana vault's address fills its word; an EVM
/// vault's address is left-padded. A builder or a reviewer commits to the leaf
/// with a zero vault chain, vault and bundle.
pub fn blessing_leaf(
    vault_chain: &[u8; 32],
    vault: &[u8; 32],
    strategy_hash: &[u8; 32],
    this_chain: &[u8; 32],
) -> [u8; 32] {
    let inner = keccak(&[vault_chain, vault, strategy_hash, this_chain]);
    keccak(&[&inner])
}

/// Whether `leaf` is under `root`, by a proof of sibling hashes, each pair
/// hashed in sorted order: the scheme of OpenZeppelin's `MerkleProof`, so a
/// root built for the EVM contracts proves the same leaves here.
pub fn merkle_verify(proof: &[[u8; 32]], root: &[u8; 32], leaf: [u8; 32]) -> bool {
    let mut computed = leaf;
    for sibling in proof {
        computed = if computed <= *sibling {
            keccak(&[&computed, sibling])
        } else {
            keccak(&[sibling, &computed])
        };
    }
    &computed == root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_leaf_is_its_own_root() {
        let leaf = [7u8; 32];
        assert!(merkle_verify(&[], &leaf, leaf));
        assert!(!merkle_verify(&[], &[8u8; 32], leaf));
    }

    #[test]
    fn a_pair_hashes_in_sorted_order_either_way() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let root = keccak(&[&a, &b]);
        assert!(merkle_verify(&[b], &root, a));
        assert!(merkle_verify(&[a], &root, b));
    }

    #[test]
    fn the_cluster_value_is_the_hash_of_its_name() {
        assert_eq!(
            cluster_value("localnet"),
            keccak(&[b"solana:localnet".as_slice()])
        );
        assert_ne!(cluster_value("devnet"), cluster_value("mainnet-beta"));
    }
}
