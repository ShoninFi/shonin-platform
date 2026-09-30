//! Solana keys from a mnemonic, the way `solana-keygen` and wallets derive
//! them: the BIP-39 seed, then SLIP-0010 for ed25519 along
//! `m/44'/501'/<index>'/0'`.
//!
//! Anchor wants a keypair file, so this writes one: the 32-byte secret then
//! the 32-byte public key, as a JSON array of numbers.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha512};

/// The mnemonic the testnet Solana accounts are derived from.
pub const TESTNET_SOLANA_MNEMONIC: &str = "SHONIN_TESTNET_SOLANA_MNEMONIC";

/// Which account plays which part. The deployer pays; the program's own
/// keypair fixes its address, so everyone deploying from the same mnemonic
/// deploys the same program id.
pub const DEPLOYER: u32 = 0;
pub const PROGRAM: u32 = 1;

/// A keypair: the secret and the public key.
pub struct Keypair {
    pub secret: [u8; 32],
    pub public: [u8; 32],
}

impl Keypair {
    pub fn address(&self) -> String {
        bs58::encode(self.public).into_string()
    }

    /// Write the keypair file Anchor and the Solana CLI read, readable only by
    /// its owner.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes: Vec<String> = self
            .secret
            .iter()
            .chain(self.public.iter())
            .map(u8::to_string)
            .collect();
        std::fs::write(path, format!("[{}]", bytes.join(",")))
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

/// The keypair at `m/44'/501'/<index>'/0'` of a mnemonic.
pub fn keypair_from_mnemonic(phrase: &str, index: u32) -> Result<Keypair> {
    use alloy::signers::local::coins_bip39::{English, Mnemonic};
    let mnemonic = Mnemonic::<English>::new_from_phrase(phrase.trim())
        .map_err(|error| anyhow!("the Solana mnemonic is not valid: {error}"))?;
    let seed = mnemonic
        .to_seed(None)
        .map_err(|error| anyhow!("the Solana mnemonic's seed: {error}"))?;
    let secret = derive(&seed, &[44, 501, index, 0]);
    Ok(keypair(secret))
}

/// The testnet mnemonic, from the environment.
pub fn testnet_mnemonic() -> Result<String> {
    std::env::var(TESTNET_SOLANA_MNEMONIC)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "the testnet Solana accounts come from {TESTNET_SOLANA_MNEMONIC}; set it in .env"
            )
        })
}

fn keypair(secret: [u8; 32]) -> Keypair {
    let signing = ed25519_dalek::SigningKey::from_bytes(&secret);
    Keypair {
        secret,
        public: signing.verifying_key().to_bytes(),
    }
}

/// SLIP-0010 for ed25519: every step is hardened.
fn derive(seed: &[u8], path: &[u32]) -> [u8; 32] {
    let master = hmac_sha512(b"ed25519 seed", seed);
    let (mut key, mut chain) = split(&master);
    for index in path {
        let mut data = Vec::with_capacity(37);
        data.push(0);
        data.extend_from_slice(&key);
        data.extend_from_slice(&(index | 0x8000_0000).to_be_bytes());
        let child = hmac_sha512(&chain, &data);
        (key, chain) = split(&child);
    }
    key
}

fn split(bytes: &[u8; 64]) -> ([u8; 32], [u8; 32]) {
    let mut left = [0u8; 32];
    let mut right = [0u8; 32];
    left.copy_from_slice(&bytes[..32]);
    right.copy_from_slice(&bytes[32..]);
    (left, right)
}

/// HMAC (RFC 2104) over SHA-512, whose block is 128 bytes.
fn hmac_sha512(key: &[u8], message: &[u8]) -> [u8; 64] {
    let mut block = [0u8; 128];
    if key.len() > block.len() {
        block[..64].copy_from_slice(&Sha512::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);

    let mut inner = Sha512::new();
    inner.update(pad(0x36));
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha512::new();
    outer.update(pad(0x5c));
    outer.update(inner);
    let mut out = [0u8; 64];
    out.copy_from_slice(&outer.finalize());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// SLIP-0010, ed25519 test vector 1.
    #[test]
    fn derivation_matches_the_published_vectors() {
        let seed: Vec<u8> = (0u8..16).collect();
        assert_eq!(
            hex(&derive(&seed, &[])),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            hex(&derive(&seed, &[0])),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
        assert_eq!(
            hex(&derive(&seed, &[0, 1])),
            "b1d0bad404bf35da785a64ca1ac54b2617211d2777696fbffaf208f746ae84f2"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha512(b"Jefe", b"what do ya want for nothing?")),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
             9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
    }

    #[test]
    fn a_keypair_file_is_secret_then_public() {
        let pair = keypair_from_mnemonic(
            "test test test test test test test test test test test junk",
            0,
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("shonin-solana-{}", std::process::id()));
        let path = dir.join("key.json");
        pair.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let numbers: Vec<u8> = text
            .trim_matches(['[', ']'])
            .split(',')
            .map(|n| n.parse().unwrap())
            .collect();
        assert_eq!(numbers.len(), 64);
        assert_eq!(&numbers[32..], &pair.public);
        assert!(!pair.address().is_empty());
    }
}
