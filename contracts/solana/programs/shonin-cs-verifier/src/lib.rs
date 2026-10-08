//! Checks a zero-knowledge proof that an instance holds a genuine Google
//! Confidential Space attestation token: the Solana counterpart of the EVM's
//! `ConfidentialSpaceVerifier`, accepting the same proofs.
//!
//! The proof is a RISC Zero Groth16 seal of one guest program, pinned by its
//! image id in each checker. The guest checks the token's RS256 signature
//! against a Google signing key, and its claims: issued by Confidential Space
//! for Shonin's audience, valid when proved, a production (non-debug)
//! launcher on Intel TDX with secure boot, a launcher version Google marks
//! stable, no operator override of the image's arguments or environment, and
//! a nonce that binds the instance's keys and the root of its leaves. It
//! commits one canonical journal that serves every kind of chain:
//!
//! ```text
//! imageDigest (32) ‖ projectId (32) ‖ vaultsRoot (32) ‖ issuedAt (8, big-endian)
//! ‖ evmSigner (20) ‖ solanaSigner (32) ‖ keyHash (32)            = 188 bytes
//! ```
//!
//! The registry hands this checker its own journal, the Borsh bytes of the
//! image digest, the project id, the instance's Solana key, the vaults root
//! and `issued_at` (u64, little-endian), 136 bytes; and a seal of the Groth16
//! seal (260 bytes) followed by the instance's EVM key (20 bytes) and the
//! Google key's hash (32). The checker rebuilds the canonical journal from
//! the two, checks the key hash against the keys it trusts, and verifies the
//! seal against the journal's `sha256`. So one proof blesses an instance's
//! keys on every chain, and a seal accepted here is accepted by the EVM
//! checker for the same instance, and no other.
//!
//! Each checker is an account of this program holding the guest's image id
//! and the Google keys it trusts, both fixed when it is created and at an
//! address derived from them, so nothing about a checker can ever change:
//! a new guest, or Google adding or dropping a key, is a new checker,
//! proposed through the registry's timelock beside this one. Anyone may
//! create one; only the registry's admin, behind the timelock, can make one
//! count. The program itself is deployed with no upgrade authority, as every
//! checker must be.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;
use shonin_common::checker;
use solana_sha256_hasher::hashv;

pub mod groth16;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// One checker, by the guest's image id and the hash of the keys it trusts.
pub const CHECKER_SEED: &[u8] = b"checker";

/// The most Google keys one checker trusts. Google publishes two or three at
/// a time.
pub const MAX_KEYS: usize = 8;

/// The Groth16 seal, the instance's EVM key and the Google key's hash.
pub const SEAL_LENGTH: usize = groth16::SEAL_LENGTH + 20 + 32;

/// The Borsh bytes of the registry's journal.
pub const JOURNAL_LENGTH: usize = 136;

/// The canonical journal's length.
pub const CANONICAL_LENGTH: usize = 188;

/// Names what a proof was checked against, for the vault's page: the bytes
/// of "gcp-cs-tdx", zero-padded, as the EVM checker names it.
pub const TRUST_ROOT: [u8; 32] = *b"gcp-cs-tdx\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// What a checker's address commits to for its keys: `sha256` of the keys,
/// in the strictly ascending order they are given in.
pub fn keys_digest(keys: &[[u8; 32]]) -> [u8; 32] {
    let parts: Vec<&[u8]> = keys.iter().map(|key| key.as_slice()).collect();
    hashv(&parts).to_bytes()
}

/// The address of the checker for this guest and these keys.
pub fn checker_address(image_id: &[u8; 32], keys: &[[u8; 32]]) -> Pubkey {
    Pubkey::find_program_address(&[CHECKER_SEED, image_id, &keys_digest(keys)], &crate::ID).0
}

/// The canonical journal rebuilt from the registry's journal and what
/// follows the Groth16 seal; `None` if either is the wrong length.
pub fn canonical_journal(journal: &[u8], tail: &[u8]) -> Option<[u8; CANONICAL_LENGTH]> {
    if journal.len() != JOURNAL_LENGTH || tail.len() != 52 {
        return None;
    }
    let mut issued_at = [0u8; 8];
    issued_at.copy_from_slice(&journal[128..136]);
    issued_at.reverse();
    let mut out = [0u8; CANONICAL_LENGTH];
    out[0..64].copy_from_slice(&journal[0..64]); // image digest, project id
    out[64..96].copy_from_slice(&journal[96..128]); // vaults root
    out[96..104].copy_from_slice(&issued_at); // big-endian
    out[104..124].copy_from_slice(&tail[..20]); // the EVM key
    out[124..156].copy_from_slice(&journal[64..96]); // the Solana key
    out[156..188].copy_from_slice(&tail[20..]); // the Google key's hash
    Some(out)
}

/// Whether `seal` proves `journal` for a checker pinning `image_id` and
/// trusting `keys`. It answers, and never fails, whatever it is given.
pub fn accepts(image_id: &[u8; 32], keys: &[[u8; 32]], seal: &[u8], journal: &[u8]) -> bool {
    if seal.len() != SEAL_LENGTH {
        return false;
    }
    let (proof, tail) = seal.split_at(groth16::SEAL_LENGTH);
    let key_hash = &tail[20..];
    if !keys.iter().any(|key| key.as_slice() == key_hash) {
        return false;
    }
    let Some(canonical) = canonical_journal(journal, tail) else {
        return false;
    };
    let digest = hashv(&[&canonical]).to_bytes();
    groth16::verify(proof, image_id, &digest)
}

#[program]
pub mod shonin_cs_verifier {
    use super::*;

    /// A checker for the guest `image_id`, trusting the Google keys whose
    /// hashes are `trusted_keys`, given in strictly ascending order so one
    /// set of keys has one address; `keys` is their [`keys_digest`], which
    /// the address is derived from.
    pub fn create_checker(
        ctx: Context<CreateChecker>,
        image_id: [u8; 32],
        keys: [u8; 32],
        trusted_keys: Vec<[u8; 32]>,
    ) -> Result<()> {
        require!(image_id != [0; 32], CsVerifierError::NoImage);
        require!(
            keys == keys_digest(&trusted_keys),
            CsVerifierError::KeysNotInOrder
        );
        require!(
            !trusted_keys.is_empty() && trusted_keys.len() <= MAX_KEYS,
            CsVerifierError::KeyCount
        );
        require!(
            trusted_keys.iter().all(|key| key != &[0; 32])
                && trusted_keys.windows(2).all(|pair| pair[0] < pair[1]),
            CsVerifierError::KeysNotInOrder
        );
        ctx.accounts.checker.set_inner(Checker {
            image_id,
            trusted_keys,
            bump: ctx.bumps.checker,
        });
        Ok(())
    }

    /// Answers whether `seal` proves `journal`. It answers, and never fails,
    /// whatever the seal and the journal hold.
    pub fn verify(ctx: Context<Verify>, seal: Vec<u8>, journal: Vec<u8>) -> Result<()> {
        let checker = &ctx.accounts.checker;
        if accepts(&checker.image_id, &checker.trusted_keys, &seal, &journal) {
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
    /// The attestation guest's RISC Zero image id.
    pub image_id: [u8; 32],
    /// The Google signing keys trusted, by the guest's key hash
    /// (`sha256(len(n) ‖ n ‖ len(e) ‖ e)`), ascending.
    #[max_len(MAX_KEYS)]
    pub trusted_keys: Vec<[u8; 32]>,
    pub bump: u8,
}

#[derive(Accounts)]
#[instruction(image_id: [u8; 32], keys: [u8; 32])]
pub struct CreateChecker<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + Checker::INIT_SPACE,
        seeds = [CHECKER_SEED, image_id.as_ref(), keys.as_ref()],
        bump,
    )]
    pub checker: Account<'info, Checker>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Verify<'info> {
    pub checker: Account<'info, Checker>,
}

#[error_code]
pub enum CsVerifierError {
    #[msg("A checker pins a guest image id")]
    NoImage,
    #[msg("A checker trusts between one and eight Google keys")]
    KeyCount,
    #[msg("The trusted keys are given in strictly ascending order, none zero, with their digest")]
    KeysNotInOrder,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real RISC Zero Groth16 proof of the attestation guest over a genuine
    /// Confidential Space token, signed by Google: the EVM checker's fixture,
    /// its journal and seal laid out as the Solana registry and this checker
    /// read them. The instance it came from served one EVM vault, so its
    /// Solana key is zero.
    pub(crate) mod real {
        pub const IMAGE_ID: &str =
            "173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0";
        pub const PROOF: &str = "73c457ba1ad1484236b08983c05de4ffedc010f0c0bd1854ab07f496dd58ab53c63adc233030edd66ab46e7ccac34c21cd20fbe468097b721a9d2dac51d855d9e124bf99090bd58d9ca4e9ae506a3a66f623b288ca239576d364d32fd9d3bb2b9720ad38260865854709fe84ce2cc5dbc08af75d66d4bd5262cb7d5895e16a30c5e482da1c91056b98b554fed559961d61a288862558acb5472c889e353942b5656115032dc4ee98c4d3e31d26fa644f65f3501b3132599c67deceda571eab17274c44a01b061a8abb7ed304ff683a30d60950b7288d55dd3556295d82b71d7fa54c12610d2fb12741f1f0cf397400563f5822b2f3fee9a3593aee08372e13722503ab07";
        pub const IMAGE_DIGEST: &str =
            "b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342";
        pub const PROJECT_ID: &str =
            "b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f";
        pub const EVM_SIGNER: &str = "e890cd248bed3547dc084cfb22c62da62f410539";
        pub const VAULTS_ROOT: &str =
            "f159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164";
        pub const ISSUED_AT: u64 = 1_791_320_211;
        pub const KEY_HASH: &str =
            "8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
        pub const OTHER_GOOGLE_KEY: &str =
            "8bcab72feb3cd8f7e7981f51c45e6bd383c3ad446e5e2895683ef5662ca022d5";
    }

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn word(hex: &str) -> [u8; 32] {
        bytes(hex).try_into().unwrap()
    }

    fn journal() -> Vec<u8> {
        let mut out = bytes(real::IMAGE_DIGEST);
        out.extend(bytes(real::PROJECT_ID));
        out.extend([0u8; 32]);
        out.extend(bytes(real::VAULTS_ROOT));
        out.extend(real::ISSUED_AT.to_le_bytes());
        out
    }

    fn seal() -> Vec<u8> {
        let mut out = bytes(real::PROOF);
        out.extend(bytes(real::EVM_SIGNER));
        out.extend(bytes(real::KEY_HASH));
        out
    }

    fn google() -> Vec<[u8; 32]> {
        let mut keys = vec![word(real::KEY_HASH), word(real::OTHER_GOOGLE_KEY)];
        keys.sort();
        keys
    }

    #[test]
    fn a_real_token_s_proof_is_accepted_for_its_guest_and_google_s_keys() {
        assert!(accepts(
            &word(real::IMAGE_ID),
            &google(),
            &seal(),
            &journal()
        ));
    }

    #[test]
    fn the_canonical_journal_is_the_one_the_evm_checker_rebuilds() {
        let canonical = canonical_journal(&journal(), &seal()[groth16::SEAL_LENGTH..]).unwrap();
        assert_eq!(&canonical[..32], &bytes(real::IMAGE_DIGEST)[..]);
        assert_eq!(&canonical[64..96], &bytes(real::VAULTS_ROOT)[..]);
        assert_eq!(&canonical[96..104], &real::ISSUED_AT.to_be_bytes());
        assert_eq!(&canonical[104..124], &bytes(real::EVM_SIGNER)[..]);
        assert_eq!(&canonical[124..156], &[0u8; 32]);
        assert_eq!(&canonical[156..], &bytes(real::KEY_HASH)[..]);
    }

    #[test]
    fn a_changed_byte_anywhere_is_refused() {
        let (image, keys) = (word(real::IMAGE_ID), google());
        let journal = journal();
        let seal = seal();
        // A byte of every journal field: image, project, Solana key, root,
        // time.
        for at in [0, 63, 64, 127, 135] {
            let mut changed = journal.clone();
            changed[at] ^= 1;
            assert!(
                !accepts(&image, &keys, &seal, &changed),
                "journal byte {at}"
            );
        }
        // The selector, the proof, the EVM key and the key hash.
        for at in [0, 100, 259, 260, 279, 300] {
            let mut changed = seal.clone();
            changed[at] ^= 1;
            assert!(
                !accepts(&image, &keys, &changed, &journal),
                "seal byte {at}"
            );
        }
    }

    #[test]
    fn a_checker_for_another_guest_or_other_keys_refuses_it() {
        assert!(!accepts(
            &word(real::VAULTS_ROOT),
            &google(),
            &seal(),
            &journal()
        ));
        assert!(!accepts(
            &word(real::IMAGE_ID),
            &[word(real::OTHER_GOOGLE_KEY)],
            &seal(),
            &journal()
        ));
    }

    #[test]
    fn a_seal_or_journal_of_the_wrong_length_is_refused() {
        let (image, keys) = (word(real::IMAGE_ID), google());
        let mut long = seal();
        long.push(0);
        assert!(!accepts(&image, &keys, &long, &journal()));
        assert!(!accepts(
            &image,
            &keys,
            &seal()[..SEAL_LENGTH - 1],
            &journal()
        ));
        let mut journal = journal();
        journal.push(0);
        assert!(!accepts(&image, &keys, &seal(), &journal));
        assert!(!accepts(&image, &keys, &[], &[]));
    }
}
