//! A purpose's public key, signed by the root signer.
//!
//! Anyone sealing to a purpose fetches its published key from wherever it is
//! served, and believes it only once the signature recovers to the signer
//! the canonical chain records (`rootKey().signer`, while `isRootKeyLive()`),
//! for the purpose they mean and that chain.

use alloy::primitives::{Address, B256, Bytes, FixedBytes, Signature, U256, keccak256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::purpose::Purpose;

/// What a published key's digest begins with.
pub const PUBLISHED_KEY_TAG: &[u8] = b"shonin:published-key:v1";

/// A purpose's public key, as the root signer published it.
///
/// ```json
/// {
///   "chain_id": 84532,
///   "purpose": "platform:dummy",
///   "public_key": "0x<32 bytes>",
///   "signature": "0x<65 bytes: r ‖ s ‖ v>"
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublishedKey {
    /// The canonical chain whose registry records the signer.
    pub chain_id: u64,
    pub purpose: Purpose,
    pub public_key: FixedBytes<32>,
    pub signature: Bytes,
}

/// What the root signer signs for a published key:
/// `keccak256("shonin:published-key:v1" ‖ chainId ‖ purpose ‖ publicKey)`,
/// the chain id a 32-byte big-endian word.
pub fn digest(chain_id: u64, purpose: &Purpose, public_key: &[u8; 32]) -> B256 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(PUBLISHED_KEY_TAG);
    bytes.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
    bytes.extend_from_slice(&purpose.to_bytes());
    bytes.extend_from_slice(public_key);
    keccak256(bytes)
}

impl PublishedKey {
    /// Sign `public_key` for `purpose` on `chain_id`, as an Ethereum signed
    /// message over the digest.
    pub(crate) fn sign(
        signer: &PrivateKeySigner,
        chain_id: u64,
        purpose: &Purpose,
        public_key: [u8; 32],
    ) -> Result<PublishedKey, Error> {
        let signature = signer
            .sign_message_sync(digest(chain_id, purpose, &public_key).as_slice())
            .map_err(|_| Error::Seal)?;
        Ok(PublishedKey {
            chain_id,
            purpose: purpose.clone(),
            public_key: FixedBytes(public_key),
            signature: Bytes::from(signature.as_bytes().to_vec()),
        })
    }

    /// Who signed it, recovered from its signature.
    pub fn signer(&self) -> Result<Address, Error> {
        let signature =
            Signature::try_from(self.signature.as_ref()).map_err(|_| Error::BadPublishedKey)?;
        signature
            .recover_address_from_msg(
                digest(self.chain_id, &self.purpose, &self.public_key.0).as_slice(),
            )
            .map_err(|_| Error::BadPublishedKey)
    }

    /// Whether this is the key for `purpose`, published for the canonical
    /// chain `chain_id`, by `signer` (the chain's `rootKey().signer`).
    pub fn verify(&self, signer: Address, chain_id: u64, purpose: &Purpose) -> Result<(), Error> {
        if signer == Address::ZERO
            || self.chain_id != chain_id
            || self.purpose != *purpose
            || self.signer()? != signer
        {
            return Err(Error::BadPublishedKey);
        }
        Ok(())
    }
}
