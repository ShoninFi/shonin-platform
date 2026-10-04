//! What a development chain trusts instead of an enclave.
//!
//! A real deployment verifies the platform's attestation token and believes
//! what the hardware says. Here one key stands for the hardware, and a journal
//! it signed is accepted. Everything the registry does with the result is the
//! same either way, so the seam is the only thing that changes when a real
//! checker takes over: the trust root says which of the two is in use, and
//! every vault page shows it.
//!
//! Each checker is an account of this program holding the key that stands for
//! the hardware, so one deployment serves any number of checkers, as the EVM
//! contract is deployed once per key. Anyone may create one; only the
//! registry's admin, behind the timelock, can make one count.
//!
//! The key signs, with ed25519, `keccak256(purpose ‖ cluster ‖
//! keccak256(journal))`, where `purpose` is `keccak256("shonin-dev-attestation")`
//! and `cluster` the checker's cluster value. The signature is checked by the
//! Ed25519 program in the same transaction, and found here; that instruction
//! is the whole proof, so the seal argument is not read and is sent empty,
//! which keeps a blessing's transaction small. Like the attestation it stands
//! for, the signature names no checker, so a checker re-deployed with the
//! same key accepts the same proofs, and a rotation between the two keeps
//! every key that attested during the overlap.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::set_return_data, sysvar::instructions};
use shonin_common::{checker, digest, ed25519, MAINNET_CLUSTER};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// One checker, by the key it trusts and a seed of its creator's choosing.
pub const CHECKER_SEED: &[u8] = b"checker";

/// Names what a proof was checked against, for the vault's page: the bytes of
/// "dev-local", zero-padded, as the EVM checker names it.
pub const TRUST_ROOT: [u8; 32] = *b"dev-local\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// What the key signs for a journal.
pub fn attestation_digest(cluster: &[u8; 32], journal: &[u8]) -> [u8; 32] {
    let journal_hash = digest::keccak(&[journal]);
    digest::keccak(&[
        &digest::purpose("shonin-dev-attestation"),
        cluster,
        &journal_hash,
    ])
}

#[program]
pub mod shonin_dev_verifier {
    use super::*;

    /// A checker that trusts `attestation_key`, for one cluster. Never for
    /// mainnet.
    pub fn create_checker(
        ctx: Context<CreateChecker>,
        attestation_key: Pubkey,
        seed: [u8; 32],
        cluster_name: String,
    ) -> Result<()> {
        require!(cluster_name != MAINNET_CLUSTER, DevVerifierError::Mainnet);
        ctx.accounts.checker.set_inner(Checker {
            attestation_key,
            cluster: digest::cluster_value(&cluster_name),
            seed,
            bump: ctx.bumps.checker,
        });
        Ok(())
    }

    /// Answers whether the checker key's signature over `journal` was
    /// verified by the Ed25519 program in this transaction. It answers, and
    /// never fails, whatever the journal holds.
    #[allow(unused_variables)]
    pub fn verify(ctx: Context<Verify>, seal: Vec<u8>, journal: Vec<u8>) -> Result<()> {
        let checker = &ctx.accounts.checker;
        let message = attestation_digest(&checker.cluster, &journal);
        let signed = ed25519::signed_in_transaction(
            &ctx.accounts.instructions,
            &checker.attestation_key,
            &message,
            None,
        )?;
        if signed {
            set_return_data(&checker::accepts(&TRUST_ROOT));
        } else {
            set_return_data(&[0]);
        }
        Ok(())
    }
}

#[account]
#[derive(InitSpace)]
pub struct Checker {
    /// The key that stands in for the hardware.
    pub attestation_key: Pubkey,
    /// `keccak256("solana:" + cluster name)`.
    pub cluster: [u8; 32],
    pub seed: [u8; 32],
    pub bump: u8,
}

#[derive(Accounts)]
#[instruction(attestation_key: Pubkey, seed: [u8; 32])]
pub struct CreateChecker<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + Checker::INIT_SPACE,
        seeds = [CHECKER_SEED, attestation_key.as_ref(), seed.as_ref()],
        bump,
    )]
    pub checker: Account<'info, Checker>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Verify<'info> {
    pub checker: Account<'info, Checker>,
    /// CHECK: the instructions sysvar, by address.
    #[account(address = instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

#[error_code]
pub enum DevVerifierError {
    #[msg("The development checker is never used on mainnet")]
    Mainnet,
}
