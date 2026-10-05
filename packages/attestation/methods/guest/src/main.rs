//! The attestation guest: checks a Confidential Space token and commits what
//! it attests to.
//!
//! It reads an `Input` (the token, the signing keys, the prover's clock and
//! the keys and root the instance bound into the token's nonce), runs every
//! check, and commits the 188-byte canonical journal. A token that fails any
//! check stops the guest, so no proof of it can exist.

use risc0_zkvm::guest::env;
use shonin_attestation_core::{Input, verify};

fn main() {
    let input: Input = env::read();
    let journal = match verify(&input) {
        Ok(journal) => journal,
        Err(error) => panic!("attestation refused: {error}"),
    };
    env::commit_slice(&journal.encode());
}
