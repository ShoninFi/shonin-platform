//! Blessing an attested instance's keys on a Solana cluster, as the
//! executor publishes them and anyone (the keeper) relays them.
//!
//! An instance attests once for every chain: its token's nonce binds its EVM
//! key, its Solana key and the root of a tree with one leaf per key it holds
//! ([`Platform::vault_leaf`], [`Platform::service_leaf`]). One proof of that
//! token serves every registry. The prover makes it for the EVM checker; the
//! Solana checker reads the same proof with the journal laid out as the
//! Solana registry reads it, and the EVM key and the Google key's hash after
//! the seal ([`SolanaSeal::from_evm`]). On a development cluster the
//! development checker's seal is the attestor role's signature over the
//! journal, carried by an Ed25519 instruction in the blessing's transaction.

use anchor_lang::AnchorSerialize;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use shonin_registry::error::RegistryError;
use shonin_registry::state::{Config, Journal};

use super::client::{SolanaClient, TransactionFailed};
use super::platform::Platform;
use super::{Instruction, Pubkey, tables};

/// The Groth16 seal's length, as the EVM and Solana checkers take it.
pub const GROTH16_SEAL_LENGTH: usize = 260;

/// What proves a journal to the Solana registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SolanaSeal {
    /// A proof the Confidential Space checker verifies: the Groth16 seal,
    /// the instance's EVM key and the Google key's hash, hex.
    Proof { seal: String },
    /// The development checker's: the attestor's ed25519 signature over the
    /// journal's attestation digest, carried in the transaction.
    Development { attestor: String, signature: String },
}

impl SolanaSeal {
    /// The Solana registry's journal and seal for a proof made for an EVM
    /// registry: its 160-byte journal and its seal (the Groth16 seal, then
    /// the instance's Solana key and the Google key's hash). `None` if
    /// either isn't one.
    pub fn from_evm(journal: &[u8], seal: &[u8]) -> Option<(Journal, SolanaSeal)> {
        if seal.len() != GROTH16_SEAL_LENGTH + 64 {
            return None;
        }
        let canonical =
            shonin_attestation_core::Journal::from_evm(journal, &seal[GROTH16_SEAL_LENGTH..])?;
        let mut solana_seal = seal[..GROTH16_SEAL_LENGTH].to_vec();
        solana_seal.extend_from_slice(&canonical.solana_seal_tail());
        let journal = Journal {
            image_digest: canonical.image_digest,
            project_id: canonical.project_id,
            signer: Pubkey::new_from_array(canonical.solana_signer),
            vaults_root: canonical.vaults_root,
            issued_at: canonical.issued_at,
        };
        Some((
            journal,
            SolanaSeal::Proof {
                seal: format!("0x{}", alloy::hex::encode(solana_seal)),
            },
        ))
    }

    /// The instructions that bless with this seal: the Ed25519 instruction
    /// first for a development seal, then `bless`.
    pub fn instructions(
        &self,
        journal: &Journal,
        cluster: &[u8; 32],
        bless: impl FnOnce(Vec<u8>) -> Instruction,
    ) -> Result<Vec<Instruction>> {
        match self {
            SolanaSeal::Proof { seal } => {
                let seal = alloy::hex::decode(seal).context("the seal is not hex")?;
                Ok(vec![bless(seal)])
            }
            SolanaSeal::Development {
                attestor,
                signature,
            } => {
                let attestor: Pubkey = attestor
                    .parse()
                    .map_err(|_| anyhow!("the attestor is not a key"))?;
                let signature: [u8; 64] = alloy::hex::decode(signature)
                    .context("the signature is not hex")?
                    .try_into()
                    .map_err(|_| anyhow!("the signature is not 64 bytes"))?;
                let message =
                    shonin_dev_verifier::attestation_digest(cluster, &journal.try_to_vec()?);
                let ed25519 = solana_ed25519_program::new_ed25519_instruction_with_signature(
                    &message,
                    &signature,
                    &attestor.to_bytes(),
                );
                Ok(vec![ed25519, bless(Vec::new())])
            }
        }
    }
}

/// One key's leaf, as the registry's `bless` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyLeaf {
    pub vault_chain: [u8; 32],
    pub vault: [u8; 32],
    pub strategy_hash: [u8; 32],
    pub key: Pubkey,
    pub proof: Vec<[u8; 32]>,
}

impl Platform {
    /// The instructions blessing `leaf`'s key with `seal`, `payer` paying.
    pub fn bless_with(
        &self,
        config: &Config,
        now: u64,
        payer: &Pubkey,
        journal: &Journal,
        seal: &SolanaSeal,
        leaf: &KeyLeaf,
    ) -> Result<Vec<Instruction>> {
        seal.instructions(journal, &self.cluster, |seal| {
            Platform::bless_instruction(
                config,
                now,
                payer,
                journal,
                seal,
                leaf.vault_chain,
                leaf.vault,
                leaf.strategy_hash,
                &leaf.key,
                leaf.proof.clone(),
            )
        })
    }

    /// Whether blessing `leaf`'s key with `seal` would go through now, with
    /// `payer` paying. While two checkers' accounts make the transaction
    /// too large to name by address, and `payer` has no lookup table holding
    /// them yet, it is simulated without the leaf's proof: the registry asks
    /// its checkers before anything else and checks the leaf last of all,
    /// after the image, the project, the kind, the journal's age and the
    /// instance, so the leaf being all it refuses says the rest would go
    /// through.
    pub async fn simulate_bless(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        journal: &Journal,
        seal: &SolanaSeal,
        leaf: &KeyLeaf,
    ) -> Result<()> {
        let config = self.registry_config(client).await?;
        let now = client.now().await?;
        let ixs = self.bless_with(&config, now, payer, journal, seal, leaf)?;
        let fits = tables::fits(&tables::by_address(
            &ixs,
            payer,
            solana_hash::Hash::default(),
        ));
        let whole = client.simulate(ixs, payer).await;
        if fits || leaf.proof.is_empty() {
            return whole.map(|_| ());
        }
        match whole {
            Ok(_) => Ok(()),
            Err(error) if error.downcast_ref::<TransactionFailed>().is_some() => Err(error),
            Err(_) => {
                let bare = KeyLeaf {
                    proof: Vec::new(),
                    ..leaf.clone()
                };
                let ixs = self.bless_with(&config, now, payer, journal, seal, &bare)?;
                match client.simulate(ixs, payer).await {
                    Ok(_) => Ok(()),
                    Err(error)
                        if error
                            .downcast_ref::<TransactionFailed>()
                            .is_some_and(|failed| {
                                failed.custom == Some(RegistryError::NotAmongTheVaults.into())
                            }) =>
                    {
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evm_journal(signer: [u8; 20], issued_at: u64) -> shonin_attestation_core::Journal {
        shonin_attestation_core::Journal {
            image_digest: [1; 32],
            project_id: [2; 32],
            vaults_root: [3; 32],
            issued_at,
            evm_signer: signer,
            solana_signer: [4; 32],
            key_hash: [5; 32],
        }
    }

    #[test]
    fn the_solana_journal_and_seal_are_the_same_canonical_journal_as_the_evm_s() {
        let canonical = evm_journal([9; 20], 1_791_320_211);
        let mut seal = vec![7u8; GROTH16_SEAL_LENGTH];
        seal.extend_from_slice(&canonical.evm_seal_tail());
        let (journal, solana_seal) = SolanaSeal::from_evm(&canonical.evm(), &seal).unwrap();
        assert_eq!(journal.try_to_vec().unwrap(), canonical.solana().to_vec());
        let SolanaSeal::Proof { seal: hex } = solana_seal else {
            panic!("a proof");
        };
        let bytes = alloy::hex::decode(hex).unwrap();
        assert_eq!(&bytes[..GROTH16_SEAL_LENGTH], &seal[..GROTH16_SEAL_LENGTH]);
        let rebuilt = shonin_attestation_core::Journal::from_solana(
            &journal.try_to_vec().unwrap(),
            &bytes[GROTH16_SEAL_LENGTH..],
        )
        .unwrap();
        assert_eq!(rebuilt, canonical);
        assert_eq!(rebuilt.digest(), canonical.digest());
    }

    #[test]
    fn a_seal_or_journal_that_is_not_the_evm_s_gives_nothing() {
        let canonical = evm_journal([9; 20], 5);
        let mut seal = vec![7u8; GROTH16_SEAL_LENGTH];
        seal.extend_from_slice(&canonical.evm_seal_tail());
        assert!(SolanaSeal::from_evm(&canonical.evm(), &seal[1..]).is_none());
        assert!(SolanaSeal::from_evm(&canonical.evm()[1..], &seal).is_none());
        let mut padded = canonical.evm();
        padded[70] = 1;
        assert!(SolanaSeal::from_evm(&padded, &seal).is_none());
    }
}
