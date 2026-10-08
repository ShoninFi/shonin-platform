//! Keys made inside the enclave.
//!
//! Each vault taken on gets a key of its own, and the instance one more (the
//! key its attestation names). Every one is made from the operating system's
//! randomness at the moment it is needed, held in memory only, and never
//! derived from anything public: an image digest and a vault address are
//! known to everyone, so a key made from them is anyone's key. A key lost
//! with the process is replaced, not recovered: a restarted executor takes
//! its vaults over again (see the handover in `main.rs`).
//!
//! Nothing here is ever logged or served: a key's [`std::fmt::Debug`] says its
//! address alone.

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use zeroize::Zeroizing;

/// A secp256k1 key made from the operating system's randomness. The secret
/// scalar is wiped from memory when the signer is dropped (the signer's own
/// key type does so), and the bytes it was made from are wiped at once.
pub fn fresh() -> Result<PrivateKeySigner> {
    loop {
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(secret.as_mut()).context("reading the operating system's randomness")?;
        // A value of zero or past the curve's order is no key; the chance is
        // negligible, and another draw is the answer.
        if let Ok(signer) = PrivateKeySigner::from_bytes(&(*secret).into()) {
            return Ok(signer);
        }
    }
}

/// An ed25519 key for Solana made from the operating system's randomness:
/// a vault's key on a Solana cluster, or the instance's Solana key. The seed
/// it was made from is wiped at once; the keypair is held in memory only.
pub fn fresh_solana() -> Result<chain::solana::Keypair> {
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret.as_mut()).context("reading the operating system's randomness")?;
    Ok(chain::solana::Keypair::new_from_array(*secret))
}

/// A key and nothing to print of it but its address.
pub struct Key(PrivateKeySigner);

impl Key {
    pub fn fresh() -> Result<Key> {
        fresh().map(Key)
    }

    pub fn address(&self) -> Address {
        self.0.address()
    }

    /// The signer, for the one client that sends with it.
    pub fn signer(&self) -> PrivateKeySigner {
        self.0.clone()
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Key({})", self.0.address())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_hex(key: &PrivateKeySigner) -> String {
        alloy::hex::encode(key.to_bytes())
    }

    #[test]
    fn every_key_is_a_new_one() {
        let keys: std::collections::HashSet<Address> =
            (0..64).map(|_| fresh().unwrap().address()).collect();
        assert_eq!(keys.len(), 64);
    }

    #[test]
    fn a_key_prints_its_address_and_never_its_secret() {
        let key = Key::fresh().unwrap();
        let secret = secret_hex(&key.0);
        for printed in [
            format!("{key:?}"),
            format!("{key:#?}"),
            format!("{:?}", key.signer()),
            format!("{:#?}", key.signer()),
        ] {
            assert!(!printed.contains(&secret), "a key printed its secret");
            assert!(!printed.to_lowercase().contains(&secret[..16]));
        }
        assert!(format!("{key:?}").contains(&key.address().to_string()));
    }

    #[test]
    fn every_solana_key_is_a_new_one_and_none_is_the_one_a_public_seed_would_give() {
        use chain::solana::Signer;
        let keys: std::collections::HashSet<_> =
            (0..64).map(|_| fresh_solana().unwrap().pubkey()).collect();
        assert_eq!(keys.len(), 64);
        // What the executor once derived from the image and the vault.
        let vault = chain::solana::Pubkey::new_from_array([0x11; 32]);
        let seed = chain::solana::programs::common::digest::keccak(&[
            &chain::solana::platform::image_digest("executor"),
            vault.as_ref(),
        ]);
        let public = chain::solana::Keypair::new_from_array(seed);
        assert!(!keys.contains(&public.pubkey()));
    }

    #[test]
    fn a_solana_key_never_prints_its_secret() {
        use chain::solana::Signer;
        let key = fresh_solana().unwrap();
        let secret = alloy::hex::encode(&key.to_bytes()[..32]);
        let printed = format!("{key:?}");
        assert!(!printed.contains(&secret));
        assert!(!printed.contains(&key.to_base58_string()));
    }

    #[test]
    fn a_key_is_not_the_one_a_public_seed_would_give() {
        // What the executor once derived from the image and the vault: anyone
        // could work it out.
        let vault = Address::repeat_byte(0x11);
        let mut seed = chain::attest::image_digest("executor").to_vec();
        seed.extend_from_slice(vault.as_slice());
        let public = PrivateKeySigner::from_bytes(&alloy::primitives::keccak256(seed)).unwrap();
        assert_ne!(fresh().unwrap().address(), public.address());
    }
}
