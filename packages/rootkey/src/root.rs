//! The root, and every key derived from it.

use std::fmt;

use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use hkdf::Hkdf;
use hpke::{Deserializable, Kem as _, Serializable};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::Error;
use crate::chain_view::ImageKind;
use crate::published::PublishedKey;
use crate::purpose::Purpose;
use crate::sealed::{Kem, PrivateKey, PublicKey};

/// The salt every derivation is extracted under.
pub const SALT: &[u8] = b"shonin:rootkey:v1";
/// The label the root signer is expanded under, before its counter byte.
pub const SIGNER_LABEL: &[u8] = b"shonin:rootkey:v1:root-signer";
/// The label a purpose's key is expanded under, before the purpose's bytes.
pub const PURPOSE_LABEL: &[u8] = b"shonin:rootkey:v1:purpose";
/// The label a service's seed is expanded under, before its kind, image and
/// project.
pub const SERVICE_LABEL: &[u8] = b"shonin:rootkey:v1:service";

/// The root key: 32 bytes from the operating system's randomness.
///
/// It is wiped from memory when dropped. Its [`fmt::Debug`] says only the
/// root signer's address, and it is never serialised: the only way out of
/// the process is sealed to an attested enclave's key ([`crate::authorise`]).
pub struct Root(Zeroizing<[u8; 32]>);

impl Root {
    /// A fresh root, from the operating system's randomness.
    pub fn generate() -> Result<Root, Error> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut()).map_err(|_| Error::Randomness)?;
        Ok(Root(bytes))
    }

    /// The root an exchange released.
    pub(crate) fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Root {
        Root(bytes)
    }

    /// The root's bytes, for sealing it to another holder.
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    fn expand(&self, info: &[&[u8]], out: &mut [u8]) {
        let info: Vec<u8> = info.concat();
        Hkdf::<Sha256>::new(Some(SALT), self.0.as_slice())
            .expand(&info, out)
            .expect("a 32-byte output is within HKDF-SHA256's limit");
    }

    /// The root signer: the secp256k1 key whose address the canonical
    /// chain records, and which signs every published key.
    pub(crate) fn signer(&self) -> PrivateKeySigner {
        for counter in 0..=u8::MAX {
            let mut secret = Zeroizing::new([0u8; 32]);
            self.expand(&[SIGNER_LABEL, &[counter]], secret.as_mut());
            // Zero or past the curve's order is no key; the chance is about
            // 2^-128, and the next counter is the answer.
            if let Ok(signer) = PrivateKeySigner::from_bytes(&B256::from(*secret)) {
                return signer;
            }
        }
        unreachable!("256 draws in a row outside the curve's order")
    }

    /// The root signer's address, as `rootKey().signer` records it.
    pub fn signer_address(&self) -> Address {
        self.signer().address()
    }

    /// The key pair for `purpose`. The root and a service's seed have none.
    pub fn key_pair(&self, purpose: &Purpose) -> Result<KeyPair, Error> {
        if *purpose == Purpose::Root {
            return Err(Error::RootHasNoKeyPair);
        }
        if *purpose == Purpose::Service {
            return Err(Error::NoKeyPair(Purpose::Service));
        }
        if !purpose.is_well_formed() {
            return Err(Error::Malformed("purpose"));
        }
        let mut ikm = Zeroizing::new([0u8; 32]);
        self.expand(&[PURPOSE_LABEL, &purpose.to_bytes()], ikm.as_mut());
        let (private, public) = Kem::derive_keypair(ikm.as_slice());
        Ok(KeyPair::new(purpose.clone(), private, public))
    }

    /// The seed of a service of `kind` running the image `image_digest` in
    /// the operator project `project_id` (`keccak256` of its id, as the
    /// registry lists projects): expanded under `shonin:rootkey:v1:service`
    /// ‖ the kind's registry value (1 byte) ‖ image digest (32) ‖ project
    /// (32).
    ///
    /// It is the same for every boot of that image under this root, so a
    /// service's keys (and their blessings and gas) outlive a restart, and
    /// another image, project or kind never shares it.
    pub fn service_seed(
        &self,
        kind: ImageKind,
        image_digest: B256,
        project_id: B256,
    ) -> Zeroizing<[u8; 32]> {
        let mut seed = Zeroizing::new([0u8; 32]);
        self.expand(
            &[
                SERVICE_LABEL,
                &[kind.to_byte()],
                image_digest.as_slice(),
                project_id.as_slice(),
            ],
            seed.as_mut(),
        );
        seed
    }

    /// The public key for `purpose`, signed by the root signer for the
    /// canonical chain `chain_id`.
    pub fn publish(&self, purpose: &Purpose, chain_id: u64) -> Result<PublishedKey, Error> {
        let pair = self.key_pair(purpose)?;
        PublishedKey::sign(&self.signer(), chain_id, purpose, pair.public_key())
    }
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Root(signer {})", self.signer_address())
    }
}

/// One purpose's X25519 key pair. The private half is wiped when dropped,
/// and [`fmt::Debug`] says only the purpose and the public half.
pub struct KeyPair {
    purpose: Purpose,
    private: PrivateKey,
    public: PublicKey,
}

impl KeyPair {
    fn new(purpose: Purpose, private: PrivateKey, public: PublicKey) -> KeyPair {
        KeyPair {
            purpose,
            private,
            public,
        }
    }

    /// The pair whose private half an exchange released.
    pub(crate) fn from_private_bytes(purpose: Purpose, bytes: &[u8]) -> Result<KeyPair, Error> {
        let private =
            PrivateKey::from_bytes(bytes).map_err(|_| Error::Malformed("released key"))?;
        let public = Kem::sk_to_pk(&private);
        Ok(KeyPair::new(purpose, private, public))
    }

    pub fn purpose(&self) -> &Purpose {
        &self.purpose
    }

    pub fn public_key(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.public.to_bytes());
        out
    }

    pub(crate) fn private(&self) -> &PrivateKey {
        &self.private
    }

    /// The private half's bytes, for sealing it to an attested enclave.
    pub(crate) fn private_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut bytes = self.private.to_bytes();
        let out = Zeroizing::new(bytes.to_vec());
        bytes.as_mut_slice().zeroize();
        out
    }
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPair")
            .field("purpose", &self.purpose.to_string())
            .field(
                "public_key",
                &format!("0x{}", hex::encode(self.public_key())),
            )
            .finish()
    }
}

/// The registry's root leaf for `signer` on `chain_id`:
/// `keccak256(keccak256(abi.encode(0, address(0), ROOT_KEY_PURPOSE, chainid,
/// signer)))`, which the instance puts among its leaves so the attestation
/// that blesses its vault keys also carries the root (`setRootKey`). The
/// encoding is the chain crate's, the one the services' trees use.
pub fn root_leaf(chain_id: u64, signer: Address) -> B256 {
    chain::attest::root_leaf(chain_id, signer)
}

#[cfg(test)]
impl Root {
    /// A root from fixed bytes, for tests alone.
    pub(crate) fn for_tests(seed: u8) -> Root {
        Root(Zeroizing::new([seed; 32]))
    }
}
