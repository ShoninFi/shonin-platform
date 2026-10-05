//! Writes the EVM checker's Groth16 fixture: a real proof of the guest over
//! a test token, and everything a contract test needs to check it.
//!
//! The token is signed with the test key (Google publishes no signed sample
//! token), so the fixture's checker trusts the test key's hash, never
//! Google's. Proving needs Docker for the Groth16 step and takes minutes.
//!
//! `cargo run -p shonin-attestation --features fixture --bin attestation-fixture -- <dir>`
//! writes `<dir>/groth16.json` and `<dir>/Groth16Fixture.sol`.

use anyhow::{Context, Result};
use serde_json::json;
use shonin_attestation::{Proof, evm_seal, image_id, prove, solana_seal, verify};
use shonin_attestation_core::fixture;

/// When the fixture's token was issued; a test warps the chain to after it.
const IAT: u64 = 1_790_000_000;

fn hex0x(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn main() -> Result<()> {
    let dir = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: attestation-fixture <dir>")?,
    );
    let binding = fixture::binding();
    let input = fixture::input(&binding, IAT, IAT + 60);

    let started = std::time::Instant::now();
    let (receipt, journal) = prove(&input, Proof::Groth16)?;
    let seconds = started.elapsed().as_secs();
    let key_hash = fixture::test_jwk().key_hash();
    verify(&receipt, &[key_hash], false).context("the fresh receipt does not verify")?;

    let fixture = json!({
        "imageId": hex0x(image_id()),
        "jwkHash": hex0x(key_hash),
        "seal": hex0x(evm_seal(&receipt, &journal)?),
        "journal": hex0x(journal.evm()),
        "canonicalJournal": hex0x(journal.encode()),
        "journalDigest": hex0x(journal.digest()),
        "solanaSeal": hex0x(solana_seal(&receipt, &journal)?),
        "solanaJournal": hex0x(journal.solana()),
        "imageDigest": hex0x(journal.image_digest),
        "projectId": hex0x(journal.project_id),
        "signer": hex0x(journal.evm_signer),
        "solanaSigner": hex0x(journal.solana_signer),
        "vaultsRoot": hex0x(journal.vaults_root),
        "issuedAt": journal.issued_at,
        "provingSeconds": seconds,
    });
    let json = dir.join("groth16.json");
    std::fs::write(&json, serde_json::to_string_pretty(&fixture)? + "\n")
        .with_context(|| format!("writing {}", json.display()))?;
    let sol = dir.join("Groth16Fixture.sol");
    std::fs::write(&sol, solidity(&receipt, &journal, key_hash)?)
        .with_context(|| format!("writing {}", sol.display()))?;
    println!(
        "wrote {} and {} in {seconds}s; image id {}",
        json.display(),
        sol.display(),
        hex0x(image_id())
    );
    Ok(())
}

/// The same fixture as a Solidity library, so a test needs no file access.
fn solidity(
    receipt: &risc0_zkvm::Receipt,
    journal: &shonin_attestation_core::Journal,
    key_hash: [u8; 32],
) -> Result<String> {
    let bytes = |value: &[u8]| format!("hex\"{}\"", hex::encode(value));
    let word = |value: &[u8]| format!("0x{}", hex::encode(value));
    Ok(format!(
        r#"// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host:
// a real RISC Zero Groth16 proof of the attestation guest over a test token
// (signed by the test key, not Google's). Regenerate it whenever the guest
// changes, since its image id changes with it.
library Groth16Fixture {{
    bytes32 internal constant IMAGE_ID = {image_id};
    bytes32 internal constant JWK_HASH = {key_hash};
    bytes internal constant SEAL = {seal};
    bytes internal constant JOURNAL = {journal};
    bytes32 internal constant IMAGE_DIGEST = {image_digest};
    bytes32 internal constant PROJECT_ID = {project_id};
    address internal constant SIGNER = {signer};
    bytes32 internal constant VAULTS_ROOT = {vaults_root};
    uint64 internal constant ISSUED_AT = {issued_at};
    uint256 internal constant VAULT_CHAIN = {chain};
    address internal constant VAULT = {vault};
    bytes32 internal constant STRATEGY_HASH = {strategy};
}}
"#,
        image_id = word(&image_id()),
        key_hash = word(&key_hash),
        seal = bytes(&evm_seal(receipt, journal)?),
        journal = bytes(&journal.evm()),
        image_digest = word(&journal.image_digest),
        project_id = word(&journal.project_id),
        signer = checksummed(&journal.evm_signer),
        vaults_root = word(&journal.vaults_root),
        issued_at = journal.issued_at,
        chain = fixture::CHAIN_ID,
        vault = checksummed(&fixture::VAULT),
        strategy = word(&fixture::STRATEGY_HASH),
    ))
}

/// An address as Solidity requires a literal: EIP-55 checksummed.
fn checksummed(address: &[u8; 20]) -> String {
    use sha3::{Digest, Keccak256};
    let lower = hex::encode(address);
    let hash = Keccak256::digest(lower.as_bytes());
    let mixed: String = lower
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let nibble = (hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0xf;
            if c.is_ascii_alphabetic() && nibble >= 8 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    format!("0x{mixed}")
}
