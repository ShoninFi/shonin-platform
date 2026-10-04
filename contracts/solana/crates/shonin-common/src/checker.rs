//! How the registry asks an attestation checker about a proof.
//!
//! A checker is a program with one instruction, `verify(seal, journal)`, laid
//! out as an Anchor instruction: the discriminator below, then the seal and
//! the journal as Borsh byte vectors. The registry hands it the accounts the
//! checker was proposed with (its state account first), every one read-only
//! and none a signer, so a checker can read but never act. It answers through
//! the program's return data: a one, then the 32-byte trust root it checked
//! against, to accept; anything else, or nothing, is a refusal. A checker
//! answers no to a proof it doesn't accept rather than failing, since a
//! failure here fails the whole blessing.

/// `sha256("global:verify")[..8]`, the Anchor discriminator of `verify`.
pub const VERIFY_DISCRIMINATOR: [u8; 8] = [133, 161, 141, 48, 120, 198, 88, 150];

/// The first byte of an answer that accepts.
pub const ACCEPTED: u8 = 1;

/// The answer that accepts a proof, checked against `trust_root`.
pub fn accepts(trust_root: &[u8; 32]) -> [u8; 33] {
    let mut answer = [0u8; 33];
    answer[0] = ACCEPTED;
    answer[1..].copy_from_slice(trust_root);
    answer
}

/// The trust root an answer accepted with, if it accepted.
pub fn accepted(answer: &[u8]) -> Option<[u8; 32]> {
    if answer.len() != 33 || answer[0] != ACCEPTED {
        return None;
    }
    answer[1..].try_into().ok()
}

/// The instruction data of `verify(seal, journal)`.
pub fn verify_data(seal: &[u8], journal: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + 4 + seal.len() + 4 + journal.len());
    data.extend_from_slice(&VERIFY_DISCRIMINATOR);
    data.extend_from_slice(&(seal.len() as u32).to_le_bytes());
    data.extend_from_slice(seal);
    data.extend_from_slice(&(journal.len() as u32).to_le_bytes());
    data.extend_from_slice(journal);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_one_and_a_root_accept() {
        let root = [5u8; 32];
        assert_eq!(accepted(&accepts(&root)), Some(root));
        assert_eq!(accepted(&[]), None);
        assert_eq!(accepted(&[1]), None);
        let mut no = accepts(&root);
        no[0] = 0;
        assert_eq!(accepted(&no), None);
        assert_eq!(accepted(&[1u8; 34]), None);
    }
}
