//! Finding a machine's signature in the transaction that carries it.
//!
//! A message a machine signs and someone else sends (the attestor's seal, a
//! builder's signature on a registration, a reviewer's go-ahead) is checked by
//! the Ed25519 program, which fails the whole transaction if any signature it
//! is given is false. A program then only has to find, among the
//! transaction's instructions, an Ed25519 instruction that verified exactly
//! this key over exactly this message. Three things make that sound, and each
//! is checked: the instruction really is the Ed25519 program's; the key, the
//! message and the signature it verified are all read from that same
//! instruction (an offset may name another instruction, whose bytes the
//! verifier and this reader could then see differently); and the bytes are
//! compared whole, the message by its exact length.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions;
use solana_sdk_ids::ed25519_program;

/// The size of one signature's offsets in an Ed25519 instruction.
const OFFSETS_SIZE: usize = 14;
/// Where the offsets start: after the count and a padding byte.
const OFFSETS_START: usize = 2;
/// An offset's instruction index meaning "this instruction".
const THIS_INSTRUCTION: u16 = u16::MAX;

/// Whether this transaction carries an Ed25519 instruction that verified
/// `signer`'s signature over `message` (and, if given, that it is exactly
/// `signature`).
///
/// `instructions_sysvar` must be the instructions sysvar; anything else is an
/// error, not a "no".
pub fn signed_in_transaction(
    instructions_sysvar: &AccountInfo,
    signer: &Pubkey,
    message: &[u8],
    signature: Option<&[u8]>,
) -> Result<bool> {
    if instructions_sysvar.key != &instructions::ID {
        return Err(ProgramError::UnsupportedSysvar.into());
    }
    let count = {
        let data = instructions_sysvar.try_borrow_data()?;
        if data.len() < 2 {
            return Err(ProgramError::InvalidAccountData.into());
        }
        u16::from_le_bytes([data[0], data[1]]) as usize
    };
    for index in 0..count {
        let instruction = instructions::load_instruction_at_checked(index, instructions_sysvar)?;
        if instruction.program_id != ed25519_program::ID {
            continue;
        }
        if verifies(&instruction.data, signer.as_ref(), message, signature) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one Ed25519 instruction's data verifies `signer` over `message`,
/// with every part read from the instruction itself.
pub fn verifies(data: &[u8], signer: &[u8], message: &[u8], signature: Option<&[u8]>) -> bool {
    let Some(&count) = data.first() else {
        return false;
    };
    for k in 0..count as usize {
        let start = OFFSETS_START + k * OFFSETS_SIZE;
        let Some(offsets) = data.get(start..start + OFFSETS_SIZE) else {
            return false;
        };
        let word = |at: usize| u16::from_le_bytes([offsets[at], offsets[at + 1]]);
        let (signature_at, signature_in) = (word(0) as usize, word(2));
        let (key_at, key_in) = (word(4) as usize, word(6));
        let (message_at, message_len, message_in) = (word(8) as usize, word(10) as usize, word(12));
        if signature_in != THIS_INSTRUCTION
            || key_in != THIS_INSTRUCTION
            || message_in != THIS_INSTRUCTION
        {
            continue;
        }
        let key = data.get(key_at..key_at + 32);
        let signed = data.get(message_at..message_at + message_len);
        let sig = data.get(signature_at..signature_at + 64);
        if key == Some(signer)
            && signed == Some(message)
            && sig.is_some()
            && signature.is_none_or(|expected| sig == Some(expected))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One signature's instruction data, laid out as the Ed25519 program's
    /// own builder lays it out.
    fn one(key: &[u8; 32], sig: &[u8; 64], message: &[u8], index: u16) -> Vec<u8> {
        let key_at = 16u16;
        let sig_at = key_at + 32;
        let message_at = sig_at + 64;
        let mut data = vec![1u8, 0];
        for word in [
            sig_at,
            index,
            key_at,
            index,
            message_at,
            message.len() as u16,
            index,
        ] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.extend_from_slice(key);
        data.extend_from_slice(sig);
        data.extend_from_slice(message);
        data
    }

    #[test]
    fn the_exact_key_and_message_verify() {
        let data = one(&[1; 32], &[2; 64], b"message", THIS_INSTRUCTION);
        assert!(verifies(&data, &[1; 32], b"message", None));
        assert!(verifies(&data, &[1; 32], b"message", Some(&[2; 64])));
        assert!(!verifies(&data, &[1; 32], b"message", Some(&[3; 64])));
        assert!(!verifies(&data, &[9; 32], b"message", None));
        assert!(!verifies(&data, &[1; 32], b"messag", None));
        assert!(!verifies(&data, &[1; 32], b"message!", None));
    }

    #[test]
    fn offsets_into_another_instruction_never_verify() {
        let data = one(&[1; 32], &[2; 64], b"message", 0);
        assert!(!verifies(&data, &[1; 32], b"message", None));
    }

    #[test]
    fn truncated_data_never_verifies() {
        let data = one(&[1; 32], &[2; 64], b"message", THIS_INSTRUCTION);
        for len in 0..data.len() {
            assert!(!verifies(&data[..len], &[1; 32], b"message", None));
        }
    }
}
