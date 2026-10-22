//! The recovery-phrase wallet on Solana: an ed25519 key derived from a
//! BIP-39 phrase along `m/44'/501'/<account>'/0'` (SLIP-10, every step
//! hardened, as Phantom and Solflare derive an account), held in this page's
//! memory only and signing here. Nothing of it is stored, logged or sent,
//! and only what it signs leaves the page. The key's bytes, the seed and
//! the app's own copies of the phrase are wiped; the BIP-39 library's own
//! short-lived copies are not (see `wallet::phrase`).

use alloy_signer_local::coins_bip39::{English, Mnemonic};
use chain::solana_ix::Pubkey;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use sha2::{Digest, Sha512};
use zeroize::Zeroizing;

use crate::wallet::phrase::BadPhrase;

/// A Solana key held in memory.
#[derive(Clone)]
pub struct SolKey {
    signing: SigningKey,
}

impl std::fmt::Debug for SolKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key: only whose it is.
        write!(f, "SolKey({})", self.pubkey())
    }
}

impl SolKey {
    /// Account `account` of a phrase (`m/44'/501'/<account>'/0'`; a
    /// wallet's first account is 0). Spaces and case don't matter; the
    /// words and their checksum do.
    pub fn from_phrase(phrase: &str, account: u32) -> Result<Self, BadPhrase> {
        let words = crate::wallet::phrase::normalized(phrase)?;
        let mnemonic = Mnemonic::<English>::new_from_phrase(&words)
            .map_err(crate::wallet::phrase::wipe_refusal)?;
        let seed = Zeroizing::new(mnemonic.to_seed(None).map_err(|_| BadPhrase)?);
        let secret = Zeroizing::new(derive(&*seed, &[44, 501, account, 0]));
        Ok(Self {
            signing: SigningKey::from_bytes(&secret),
        })
    }

    pub fn pubkey(&self) -> Pubkey {
        Pubkey::new_from_array(self.signing.verifying_key().to_bytes())
    }

    /// Sign a transaction's message.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }
}

/// Whether `signature` is `signer`'s over `message`.
pub fn verify(signer: &Pubkey, message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&signer.to_bytes()) else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(signature))
        .is_ok()
}

/// SLIP-0010 for ed25519: every step hardened.
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
    fn derivation_matches_the_published_slip10_vectors() {
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
        assert_eq!(
            hex(&hmac_sha512(b"Jefe", b"what do ya want for nothing?")),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
             9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
    }

    /// The published development phrase's viewer account (role 6, at
    /// `m/44'/501'/16'/0'`) is the account the local cluster's API names as
    /// its development wallet.
    #[test]
    fn the_development_phrase_gives_the_local_clusters_development_account() {
        let key = SolKey::from_phrase(
            "test test test test test test test test test test test junk",
            16,
        )
        .unwrap();
        assert_eq!(
            key.pubkey().to_string(),
            "4fNNXcemx1jQ1crbXfADJ8tdkB5sTccjRp6bH6HJFnV5"
        );
        // Case and spacing don't matter.
        let again = SolKey::from_phrase(
            "  TEST test test test test test test test test test test   junk ",
            16,
        )
        .unwrap();
        assert_eq!(again.pubkey(), key.pubkey());
    }

    #[test]
    fn a_signature_verifies_for_its_signer_and_message_only() {
        let key = SolKey::from_phrase(
            "test test test test test test test test test test test junk",
            0,
        )
        .unwrap();
        let other = SolKey::from_phrase(
            "test test test test test test test test test test test junk",
            1,
        )
        .unwrap();
        let signature = key.sign(b"message");
        assert!(verify(&key.pubkey(), b"message", &signature));
        assert!(!verify(&key.pubkey(), b"messagf", &signature));
        assert!(!verify(&other.pubkey(), b"message", &signature));
    }

    #[test]
    fn a_bad_phrase_is_refused_without_repeating_it() {
        assert!(SolKey::from_phrase("not a phrase", 0).is_err());
        assert!(
            SolKey::from_phrase(
                "test test test test test test test test test test test test",
                0
            )
            .is_err()
        );
        let key = SolKey::from_phrase(
            "test test test test test test test test test test test junk",
            0,
        )
        .unwrap();
        assert!(!format!("{key:?}").contains("test"));
    }
}
