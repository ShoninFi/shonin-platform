//! A real RISC Zero Groth16 proof of the attestation guest over a genuine
//! PKI token from the same VM as [`crate::real_token`]'s (a second later):
//! its header carries its signing key's certificate chain up to Google's
//! Confidential Space Root CA, and its proof names that root's hash where an
//! OIDC token's names its Google key's. The EVM checker's fixture
//! (`RealPkiTokenFixture`), laid out as the Solana registry and checker read
//! it.

use crate::real_token::{image_digest, project_id, vaults_root};
use crate::Journal;
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

/// The attestation guest's image id: the guest that checks certificate
/// chains.
pub fn image_id() -> [u8; 32] {
    word("8380b1a222ddbf918b25839f79b6e4aa9b7bab73e307cf21f8938ea1dacd6cef")
}

pub const ISSUED_AT: u64 = 1_791_320_212;

/// `sha256` of Google's Confidential Space Root CA certificate (DER).
pub fn root_hash() -> [u8; 32] {
    word("148b293821bb0c6a317f413c8ba475814091cb22d49b9e3c94198db8e8f86c39")
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

/// The Groth16 seal, then the instance's EVM key and the root's hash.
pub fn seal() -> Vec<u8> {
    let mut out = bytes(
        "73c457ba21f39c4854ebe10ca504d027826ec2394b9905d1ac9005375c0db18e2aca8da2056392cd1602e51cf9ad6ae80c2acb1a2dbf019ea3fd9c6551bfb5d0d3cf9a821cb3552c13ac5949b25264135deb05749b0ce58c2cf19b99a6c12a24d6e6fb022a85cdc1731e5e170151c2362f67e0ce1d2569b148f4179039a085f990dcfda72e1085133cad31fa5d4b10439fa7e84dd129abec1012902a9f07ba4daaa1cad2124f2b74ef97392e0574fd98c4da4e404960231696ef8ac83297b22a5a4a1bcf2d7df01052a572b77573935e2570347f0379918f6390dc3b37f7eddc3c93901a006cb117db5a6216d916e0f8746c1132c864c8758613786804557b103b49a1e7",
    );
    out.extend(bytes("e890cd248bed3547dc084cfb22c62da62f410539"));
    out.extend(root_hash());
    out
}
