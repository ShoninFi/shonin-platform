//! Proving a Confidential Space attestation, and checking the proof.
//!
//! Anyone may prove: the input is the instance's token, Google's signing
//! keys, a clock, and the keys and root the instance bound into the token's
//! nonce. The guest checks the token and commits the canonical journal
//! ([`Journal`]); a Groth16 receipt of that run is what a chain's checker
//! verifies, given the registry's own journal and what follows the seal
//! ([`evm_seal`], [`solana_seal`]).
//!
//! [`verify`] is the same check off-chain: the receipt against the guest's
//! image id, and the Google key it names against the keys the caller trusts.

use anyhow::{Context, Result, bail, ensure};
use risc0_zkvm::{
    Digest, ExecutorEnv, ProverOpts, Receipt, VerifierContext, default_executor, default_prover,
};
pub use shonin_attestation_core::{Binding, Input, Journal, Jwk};
use shonin_attestation_methods::{SHONIN_ATTESTATION_GUEST_ELF, SHONIN_ATTESTATION_GUEST_ID};

/// The guest's image id as a chain pins it: the 32 bytes RISC Zero's
/// verifier contract takes as `imageId`.
pub fn image_id() -> [u8; 32] {
    Digest::from(SHONIN_ATTESTATION_GUEST_ID).into()
}

/// The length of a Groth16 seal as RISC Zero's verifier contract takes it:
/// a 4-byte selector naming the verifier parameters, then the proof.
pub const GROTH16_SEAL_LEN: usize = 4 + 256;

/// How hard to prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proof {
    /// A receipt that proves nothing, for tests: it verifies only where dev
    /// mode is allowed, and never on a chain.
    Dev,
    /// A Groth16 receipt, which a chain verifies. Needs Docker.
    Groth16,
}

/// Run the guest without proving: what the journal would be, and how many
/// cycles it takes. A token the guest refuses is an error here.
pub fn execute(input: &Input) -> Result<(Journal, u64)> {
    let env = ExecutorEnv::builder()
        .write(input)
        .context("writing the guest's input")?
        .build()?;
    let session = default_executor()
        .execute(env, SHONIN_ATTESTATION_GUEST_ELF)
        .context("the guest refused the token")?;
    let journal = Journal::decode(&session.journal.bytes)
        .context("the guest committed a journal of the wrong length")?;
    Ok((journal, session.cycles()))
}

/// Prove the guest's run over `input`.
pub fn prove(input: &Input, proof: Proof) -> Result<(Receipt, Journal)> {
    let env = ExecutorEnv::builder()
        .write(input)
        .context("writing the guest's input")?
        .build()?;
    let dev = proof == Proof::Dev;
    let opts = match proof {
        Proof::Dev => ProverOpts::default(),
        Proof::Groth16 => ProverOpts::groth16(),
    }
    .with_dev_mode(dev);
    let context = VerifierContext::default().with_dev_mode(dev);
    let receipt = default_prover()
        .prove_with_ctx(env, &context, SHONIN_ATTESTATION_GUEST_ELF, &opts)
        .context("the guest refused the token, or proving failed")?
        .receipt;
    let journal = Journal::decode(&receipt.journal.bytes)
        .context("the guest committed a journal of the wrong length")?;
    Ok((receipt, journal))
}

/// Check a receipt as a chain's checker does: that it proves this guest's
/// run, and that the Google key it names is one of `trusted_keys` (by
/// [`Jwk::key_hash`]). A dev-mode receipt passes only with `allow_dev`.
pub fn verify(receipt: &Receipt, trusted_keys: &[[u8; 32]], allow_dev: bool) -> Result<Journal> {
    let context = VerifierContext::default().with_dev_mode(allow_dev);
    receipt
        .verify_with_context(&context, SHONIN_ATTESTATION_GUEST_ID)
        .context("the receipt does not prove the attestation guest")?;
    let journal = Journal::decode(&receipt.journal.bytes)
        .context("the receipt's journal is the wrong length")?;
    ensure!(
        trusted_keys.contains(&journal.key_hash),
        "the token was signed by a key that is not trusted"
    );
    Ok(journal)
}

/// The Groth16 seal as RISC Zero's verifier contract takes it.
pub fn groth16_seal(receipt: &Receipt) -> Result<Vec<u8>> {
    let Ok(groth16) = receipt.inner.groth16() else {
        bail!("not a Groth16 receipt");
    };
    let mut seal = Vec::with_capacity(GROTH16_SEAL_LEN);
    seal.extend_from_slice(&groth16.verifier_parameters.as_bytes()[..4]);
    seal.extend_from_slice(&groth16.seal);
    ensure!(
        seal.len() == GROTH16_SEAL_LEN,
        "unexpected Groth16 seal length"
    );
    Ok(seal)
}

/// The `seal` an EVM registry's `bless` passes to the checker: the Groth16
/// seal, then the instance's Solana key and the Google key's hash (324
/// bytes). The registry's `journal` is [`Journal::evm`].
pub fn evm_seal(receipt: &Receipt, journal: &Journal) -> Result<Vec<u8>> {
    let mut seal = groth16_seal(receipt)?;
    seal.extend_from_slice(&journal.evm_seal_tail());
    Ok(seal)
}

/// The `seal` the Solana registry's `bless` passes to the checker: the
/// Groth16 seal, then the instance's EVM key and the Google key's hash (312
/// bytes). The registry's `journal` is [`Journal::solana`].
pub fn solana_seal(receipt: &Receipt, journal: &Journal) -> Result<Vec<u8>> {
    let mut seal = groth16_seal(receipt)?;
    seal.extend_from_slice(&journal.solana_seal_tail());
    Ok(seal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shonin_attestation_core::fixture;

    const IAT: u64 = 1_790_000_000;

    fn trusted() -> Vec<[u8; 32]> {
        vec![fixture::test_jwk().key_hash()]
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn the_guest_commits_the_journal_the_native_check_computes() {
        let input = fixture::input(&fixture::binding(), IAT, IAT + 60);
        let native = shonin_attestation_core::verify(&input).unwrap();
        let (journal, cycles) = execute(&input).unwrap();
        assert_eq!(journal, native);
        println!("the guest ran in {cycles} cycles");
        // An RS256 check on the zkVM's RSA precompile stays small.
        assert!(cycles < 20_000_000, "{cycles} cycles");
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn the_guest_refuses_a_token_the_native_check_refuses() {
        let mut input = fixture::input(&fixture::binding(), IAT, IAT + 60);
        input.binding.vaults_root = [9; 32];
        assert!(execute(&input).is_err());
        input = fixture::input(&fixture::binding(), IAT, IAT + 3600);
        assert!(execute(&input).is_err());
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn a_dev_receipt_verifies_only_where_dev_mode_is_allowed_and_only_for_trusted_keys() {
        let input = fixture::input(&fixture::binding(), IAT, IAT + 60);
        let (receipt, journal) = prove(&input, Proof::Dev).unwrap();
        assert_eq!(verify(&receipt, &trusted(), true).unwrap(), journal);
        assert!(verify(&receipt, &trusted(), false).is_err());
        assert!(verify(&receipt, &[[0; 32]], true).is_err());
        // A dev receipt has no seal a chain could check.
        assert!(evm_seal(&receipt, &journal).is_err());
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn a_receipt_whose_journal_was_swapped_does_not_verify() {
        let input = fixture::input(&fixture::binding(), IAT, IAT + 60);
        let (mut receipt, journal) = prove(&input, Proof::Dev).unwrap();
        let forged = Journal {
            image_digest: [0xEE; 32],
            ..journal
        };
        receipt.journal.bytes = forged.encode().to_vec();
        assert!(verify(&receipt, &trusted(), true).is_err());
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn the_guest_accepts_the_real_token_and_commits_what_the_native_check_computes() {
        let input = fixture::real_input(fixture::REAL_IAT + 60);
        let native = shonin_attestation_core::verify(&input).unwrap();
        let (journal, cycles) = execute(&input).unwrap();
        assert_eq!(journal, native);
        assert_eq!(hex::encode(journal.key_hash), fixture::REAL_KEY_HASH);
        println!("the guest checked the real token in {cycles} cycles");
        assert!(cycles < 20_000_000, "{cycles} cycles");
    }

    #[test]
    #[ignore = "runs the guest in r0vm: needs RISC Zero's toolchain (`rzup`)"]
    fn the_guest_refuses_the_real_token_after_its_hour_or_for_another_binding() {
        assert!(execute(&fixture::real_input(fixture::REAL_EXP)).is_err());
        let mut input = fixture::real_input(fixture::REAL_IAT + 60);
        input.binding.vaults_root = [9; 32];
        assert!(execute(&input).is_err());
    }

    #[test]
    fn the_image_id_is_the_guest_s() {
        assert_eq!(
            image_id(),
            Digest::from(SHONIN_ATTESTATION_GUEST_ID).as_bytes()
        );
    }

    #[test]
    #[ignore = "needs the guest built (Docker); without SHONIN_ATTESTATION_BUILD_GUEST the crate has no image id"]
    fn the_image_id_the_deploy_pins_is_the_guest_s() {
        assert_eq!(
            image_id(),
            shonin_attestation_deploy::GUEST_IMAGE_ID.0,
            "the guest changed: pin its new image id in the deploy crate and regenerate the contracts' fixture"
        );
    }
}
