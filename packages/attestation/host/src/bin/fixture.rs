//! Writes the EVM checker's Groth16 fixture: a real proof of the guest over
//! a test token, and everything a contract test needs to check it.
//!
//! The token is signed with the test key (Google publishes no signed sample
//! token), so the fixture's checker trusts the test key's hash, never
//! Google's. Proving needs Docker for the Groth16 step and takes minutes.
//!
//! `cargo run -p shonin-attestation --features fixture --bin attestation-fixture -- <dir>`
//! writes `<dir>/groth16.json` and `<dir>/Groth16Fixture.sol`.
//!
//! Given a genuine token instead (one a Confidential Space VM was issued,
//! with the binding its nonce commits to), it proves that token against
//! Google's published keys alone (an OIDC token) or Google's Confidential
//! Space Root CA alone (a PKI token), and writes `<dir>/<Name>.sol` (a
//! library `<Name>`, `RealTokenFixture` by default), for a checker that
//! trusts them:
//!
//! `attestation-fixture --token <token.jwt> --binding <binding.json> [--now <unix seconds>] [--name <Name>] <dir>`
//!
//! The binding file holds `evm_signer`, `solana_signer` and `vaults_root`
//! (0x hex), and either the one leaf's `vault_chain`, `vault` and
//! `strategy_hash`, or `leaves`: each with `vault_chain`, `vault`,
//! `strategy_hash`, `key` and `proof` (the five-word leaves that name their
//! key). `--now` is the prover's clock (default: a minute after the token's
//! `iat`); it must lie within the token's hour.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use shonin_attestation::{
    Binding, Input, Jwk, Proof, evm_seal, image_id, prove, solana_seal, verify,
};
use shonin_attestation_core::{GOOGLE_JWKS_2026_10_06, confidential_space_root_hash, fixture};

/// When the fixture's token was issued; a test warps the chain to after it.
const IAT: u64 = 1_790_000_000;

fn hex0x(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes))
}

const USAGE: &str = "usage: attestation-fixture [--token <token.jwt> --binding <binding.json> [--now <unix seconds>] [--name <Library>]] <dir>";

fn main() -> Result<()> {
    let mut token = None;
    let mut binding = None;
    let mut now = None;
    let mut name = None;
    let mut dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--token" => token = Some(args.next().context(USAGE)?),
            "--binding" => binding = Some(args.next().context(USAGE)?),
            "--now" => now = Some(args.next().context(USAGE)?.parse::<u64>().context(USAGE)?),
            "--name" => name = Some(args.next().context(USAGE)?),
            _ if dir.is_none() && !arg.starts_with("--") => dir = Some(arg),
            _ => bail!(USAGE),
        }
    }
    let dir = std::path::PathBuf::from(dir.context(USAGE)?);
    match (token, binding) {
        (None, None) if now.is_none() && name.is_none() => test_token(&dir),
        (Some(token), Some(binding)) => {
            let name = name.unwrap_or_else(|| "RealTokenFixture".into());
            anyhow::ensure!(
                name.chars().all(|c| c.is_ascii_alphanumeric()) && !name.is_empty(),
                "the library name must be alphanumeric"
            );
            real_token(&dir, &token, &binding, now, &name)
        }
        _ => bail!(USAGE),
    }
}

fn test_token(dir: &std::path::Path) -> Result<()> {
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

/// One leaf under a genuine token's root.
struct Leaf {
    vault_chain: u64,
    vault: [u8; 20],
    strategy_hash: [u8; 32],
    /// The key the leaf names; none for the old four-word leaf.
    key: Option<[u8; 20]>,
    proof: Vec<[u8; 32]>,
}

/// What a genuine token's nonce binds, and the leaves under its root.
struct RealBinding {
    binding: Binding,
    leaves: Vec<Leaf>,
}

fn hex_bytes<const N: usize>(text: &str, what: &str) -> Result<[u8; N]> {
    let bytes = hex::decode(text.trim_start_matches("0x"))
        .with_context(|| format!("the binding's `{what}` is not hex"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("the binding's `{what}` is not {N} bytes"))
}

fn hex_field<const N: usize>(doc: &Value, name: &str) -> Result<[u8; N]> {
    let text = doc[name]
        .as_str()
        .with_context(|| format!("the binding has no `{name}`"))?;
    hex_bytes(text, name)
}

/// A number written as a JSON number or as a decimal string.
fn number(doc: &Value, name: &str) -> Result<u64> {
    match &doc[name] {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .with_context(|| format!("the binding's `{name}` is not a number"))
}

fn read_leaf(doc: &Value) -> Result<Leaf> {
    let key = match doc.get("signer") {
        Some(_) => Some(hex_field(doc, "signer")?),
        None => None,
    };
    let proof = match doc.get("proof").and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .map(|item| hex_bytes(item.as_str().unwrap_or_default(), "proof"))
            .collect::<Result<_>>()?,
        None => Vec::new(),
    };
    Ok(Leaf {
        vault_chain: number(doc, "vault_chain")?,
        vault: hex_field(doc, "vault")?,
        strategy_hash: hex_field(doc, "strategy_hash")?,
        key,
        proof,
    })
}

fn read_binding(path: &str) -> Result<RealBinding> {
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
    )
    .with_context(|| format!("parsing {path}"))?;
    let binding = Binding {
        evm_signer: hex_field(&doc, "evm_signer")?,
        solana_signer: hex_field(&doc, "solana_signer")?,
        vaults_root: hex_field(&doc, "vaults_root")?,
    };
    let leaves = match doc.get("leaves").and_then(Value::as_array) {
        Some(items) => items.iter().map(read_leaf).collect::<Result<_>>()?,
        None => vec![read_leaf(&doc)?],
    };
    Ok(RealBinding { binding, leaves })
}

/// The token's `iat`, read without checking anything (the guest checks).
fn issued_at(token: &str) -> Result<u64> {
    let payload = token.split('.').nth(1).context("the token is not a JWT")?;
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .context("the token's claims are not base64")?,
    )?;
    claims["iat"].as_u64().context("the token has no `iat`")
}

/// The leaves as Solidity: the single old leaf as constants, or every
/// five-word leaf through `leaf(i)`.
fn solidity_leaves(leaves: &[Leaf]) -> String {
    let word = |value: &[u8]| format!("0x{}", hex::encode(value));
    if let [one] = leaves
        && one.key.is_none()
    {
        return format!(
            "    uint256 internal constant VAULT_CHAIN = {};\n    address internal constant VAULT = {};\n    bytes32 internal constant STRATEGY_HASH = {};\n",
            one.vault_chain,
            checksummed(&one.vault),
            word(&one.strategy_hash)
        );
    }
    let mut out = format!(
        "    uint256 internal constant LEAVES = {};\n\n    /// The `i`th leaf under the root: what `bless` takes besides the seal\n    /// and the journal.\n    function leaf(uint256 i)\n        internal\n        pure\n        returns (uint256 vaultChain, address vault, bytes32 strategyHash, address key, bytes32[] memory proof)\n    {{\n",
        leaves.len()
    );
    for (i, leaf) in leaves.iter().enumerate() {
        out += &format!("        if (i == {i}) {{\n");
        out += &format!(
            "            (vaultChain, vault, strategyHash, key) = ({}, {}, {}, {});\n",
            leaf.vault_chain,
            checksummed(&leaf.vault),
            word(&leaf.strategy_hash),
            checksummed(&leaf.key.unwrap_or_default())
        );
        out += &format!("            proof = new bytes32[]({});\n", leaf.proof.len());
        for (j, node) in leaf.proof.iter().enumerate() {
            out += &format!("            proof[{j}] = {};\n", word(node));
        }
        out += "            return (vaultChain, vault, strategyHash, key, proof);\n        }\n";
    }
    out += "        revert(\"no such leaf\");\n    }\n";
    out
}

fn real_token(
    dir: &std::path::Path,
    token: &str,
    binding: &str,
    now: Option<u64>,
    name: &str,
) -> Result<()> {
    let token = std::fs::read_to_string(token)
        .with_context(|| format!("reading {token}"))?
        .trim()
        .to_string();
    let real = read_binding(binding)?;
    let now = match now {
        Some(now) => now,
        None => issued_at(&token)? + 60,
    };
    let keys = Jwk::from_jwks(GOOGLE_JWKS_2026_10_06).map_err(|e| anyhow::anyhow!("{e}"))?;
    let google: Vec<[u8; 32]> = keys.iter().map(Jwk::key_hash).collect();
    let input = Input {
        token,
        keys,
        now,
        binding: real.binding,
    };
    let journal = shonin_attestation_core::verify(&input)
        .map_err(|e| anyhow::anyhow!("the token does not pass the native check: {e}"))?;
    let (_, cycles) = shonin_attestation::execute(&input)?;

    let started = std::time::Instant::now();
    let (receipt, proved) = prove(&input, Proof::Groth16)?;
    let seconds = started.elapsed().as_secs();
    anyhow::ensure!(proved == journal, "the guest committed another journal");
    // A PKI token is trusted by Google's root alone, an OIDC token by
    // Google's keys alone.
    let root = confidential_space_root_hash();
    let pki = journal.key_hash == root;
    let trusted = if pki { vec![root] } else { google.clone() };
    verify(&receipt, &trusted, false).context("the fresh receipt does not verify")?;
    let (signed_by, anchor) = if pki {
        (
            "a PKI token signed under Google's Confidential\n// Space Root CA, checked against that root alone",
            format!(
                "/// The hash of the root certificate the token's chain ends in:\n    /// Google's Confidential Space Root CA.\n    bytes32 internal constant ROOT_HASH = 0x{};",
                hex::encode(journal.key_hash)
            ),
        )
    } else {
        (
            "signed by Google, checked against Google's\n// published keys alone",
            format!(
                "/// The hash of Google's key that signed the token.\n    bytes32 internal constant JWK_HASH = 0x{};",
                hex::encode(journal.key_hash)
            ),
        )
    };

    let bytes = |value: &[u8]| format!("hex\"{}\"", hex::encode(value));
    let word = |value: &[u8]| format!("0x{}", hex::encode(value));
    let keys_sol: Vec<String> = google
        .iter()
        .enumerate()
        .map(|(i, key)| format!("        keys[{i}] = {};", word(key)))
        .collect();
    let sol = format!(
        r#"// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host
// (`--token`): a real RISC Zero Groth16 proof of the attestation guest over a
// genuine Confidential Space token, {signed_by}. The guest ran in {cycles}
// cycles; the Groth16 proof took {seconds} s. Regenerate it whenever the
// guest changes.
library {name} {{
    bytes32 internal constant IMAGE_ID = {image_id};
    {anchor}
    bytes internal constant SEAL = {seal};
    bytes internal constant JOURNAL = {journal};
    bytes32 internal constant IMAGE_DIGEST = {image_digest};
    bytes32 internal constant PROJECT_ID = {project_id};
    address internal constant SIGNER = {signer};
    bytes32 internal constant VAULTS_ROOT = {vaults_root};
    uint64 internal constant ISSUED_AT = {issued_at};
{leaves}
    /// Google's Confidential Space keys as published when the checker's key
    /// set was pinned, by hash.
    function googleKeys() internal pure returns (bytes32[] memory keys) {{
        keys = new bytes32[]({count});
{keys}
    }}
}}
"#,
        image_id = word(&image_id()),
        seal = bytes(&evm_seal(&receipt, &journal)?),
        journal = bytes(&journal.evm()),
        image_digest = word(&journal.image_digest),
        project_id = word(&journal.project_id),
        signer = checksummed(&journal.evm_signer),
        vaults_root = word(&journal.vaults_root),
        issued_at = journal.issued_at,
        leaves = solidity_leaves(&real.leaves),
        count = google.len(),
        keys = keys_sol.join("\n"),
    );
    let path = dir.join(format!("{name}.sol"));
    std::fs::write(&path, sol).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "wrote {}: guest {cycles} cycles, Groth16 proof in {seconds}s; image id {}",
        path.display(),
        hex0x(image_id())
    );
    Ok(())
}
