//! A real RISC Zero Groth16 proof of the attestation guest over a genuine
//! Confidential Space token a VM in the testnet's project was issued, signed
//! by Google and checked against Google's published keys alone: the EVM
//! checker's fixture, laid out as the Solana registry and checker read it.
//! The instance it came from served one EVM vault, so its Solana key is
//! zero and its root is that vault's leaf.

use crate::{hash, Journal};
use anchor_lang::prelude::Pubkey;

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn word(hex: &str) -> [u8; 32] {
    bytes(hex).try_into().unwrap()
}

/// The attestation guest's image id.
pub fn image_id() -> [u8; 32] {
    word("173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0")
}

/// The container image the token names.
pub fn image_digest() -> [u8; 32] {
    word("b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342")
}

/// The project the VM ran in, `keccak256("shonin-testnet")`.
pub fn project_id() -> [u8; 32] {
    word("b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f")
}

pub fn vaults_root() -> [u8; 32] {
    word("f159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164")
}

pub const ISSUED_AT: u64 = 1_791_320_211;

/// The hash of the Google key that signed the token.
pub fn key_hash() -> [u8; 32] {
    word("8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb")
}

/// Google's Confidential Space keys as published when the checker's key set
/// was pinned, by hash.
pub fn google_keys() -> Vec<[u8; 32]> {
    vec![
        key_hash(),
        word("8bcab72feb3cd8f7e7981f51c45e6bd383c3ad446e5e2895683ef5662ca022d5"),
    ]
}

/// The registry's journal: the instance's Solana key is zero.
pub fn journal() -> Journal {
    Journal {
        image_digest: image_digest(),
        project_id: project_id(),
        signer: Pubkey::default(),
        vaults_root: vaults_root(),
        issued_at: ISSUED_AT,
    }
}

/// The Groth16 seal, then the instance's EVM key and the Google key's hash.
pub fn seal() -> Vec<u8> {
    let mut out = bytes(
        "73c457ba1ad1484236b08983c05de4ffedc010f0c0bd1854ab07f496dd58ab53c63adc233030edd66ab46e7ccac34c21cd20fbe468097b721a9d2dac51d855d9e124bf99090bd58d9ca4e9ae506a3a66f623b288ca239576d364d32fd9d3bb2b9720ad38260865854709fe84ce2cc5dbc08af75d66d4bd5262cb7d5895e16a30c5e482da1c91056b98b554fed559961d61a288862558acb5472c889e353942b5656115032dc4ee98c4d3e31d26fa644f65f3501b3132599c67deceda571eab17274c44a01b061a8abb7ed304ff683a30d60950b7288d55dd3556295d82b71d7fa54c12610d2fb12741f1f0cf397400563f5822b2f3fee9a3593aee08372e13722503ab07",
    );
    out.extend(bytes("e890cd248bed3547dc084cfb22c62da62f410539"));
    out.extend(key_hash());
    out
}

/// A project that isn't the token's.
pub fn other_project() -> [u8; 32] {
    hash("another project")
}
