//! Putting the Confidential Space checker on an EVM chain, and proposing it.
//!
//! The checker is proposed to the attestation registry by its admin, like an
//! image: it counts only once the registry's timelock has passed, while the
//! checkers already allowed (the development checker, on a development
//! chain or a testnet) keep working throughout. Retiring the development
//! checker afterwards is a separate, deliberate step.
//!
//! RISC Zero's Groth16 verifier is deployed alongside it unless the chain
//! already has one the caller names: on chains where RISC Zero has deployed
//! its own, that one can be reused.

use std::path::Path;

use alloy::primitives::{Address, B256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, anyhow, bail};
use chain::Client;
use chain::bindings::attestation::IAttestationRegistry;
use shonin_attestation_core::Jwk;

/// The attestation guest's image id, as built reproducibly in RISC Zero's
/// guest-builder image (`packages/attestation/methods`). A host test checks
/// it against the guest whenever the guest is built; a guest change is a new
/// image id, and so a new checker proposed through the timelock.
pub const GUEST_IMAGE_ID: B256 =
    alloy::primitives::b256!("173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0");

/// RISC Zero's Groth16 verifier parameters for the vendored release
/// (`risc0-ethereum` v3.0.1, `ControlID.sol`), which the verifier is
/// constructed with.
pub const CONTROL_ROOT: B256 =
    alloy::primitives::b256!("a54dc85ac99f851c92d7c96d7318af41dbe7c0194edfcc37eb4d422a998c1f56");
pub const BN254_CONTROL_ID: B256 =
    alloy::primitives::b256!("04446e66d300eb7fb45c9726bb53c793dda407a62e9601618bb43c5c14657ac0");

/// What was deployed and proposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proposed {
    /// RISC Zero's Groth16 verifier the checker calls.
    pub risc_zero: Address,
    /// The checker, as proposed to the registry.
    pub checker: Address,
    /// The registry's timelock when it was proposed: it counts that long
    /// after the proposal.
    pub timelock: u64,
}

/// The key hashes a checker trusts, from a JWKS document: by default
/// Google's Confidential Space keys as published when it is deployed.
pub fn trusted_keys(jwks: &str) -> Result<Vec<B256>> {
    let keys = Jwk::from_jwks(jwks).map_err(|error| anyhow!("{error}"))?;
    if keys.is_empty() {
        bail!("the key set has no RSA keys");
    }
    Ok(keys.iter().map(|key| B256::from(key.key_hash())).collect())
}

/// Deploy the checker (and RISC Zero's verifier, unless `risc_zero` names
/// one already on the chain) from the contracts' build in `artifacts`
/// (`contracts/evm/out`), and propose it to `registry` as `admin`.
///
/// Already proposed is done: a checker the registry already lists is not
/// proposed again, so a deploy that stopped halfway can be run again.
pub async fn deploy_and_propose(
    client: &Client,
    admin: Address,
    artifacts: &Path,
    registry: Address,
    risc_zero: Option<Address>,
    image_id: B256,
    trusted_keys: &[B256],
) -> Result<Proposed> {
    if image_id == B256::ZERO {
        bail!("no guest image id: build the guest and pin its id");
    }
    if trusted_keys.is_empty() {
        bail!("a checker that trusts no key accepts nothing");
    }
    let risc_zero = match risc_zero {
        Some(existing) => {
            if !client.has_code(existing).await? {
                bail!("no contract at the RISC Zero verifier {existing}");
            }
            existing
        }
        None => {
            put(
                client,
                artifacts,
                admin,
                "RiscZeroGroth16Verifier",
                (CONTROL_ROOT, BN254_CONTROL_ID).abi_encode_params(),
            )
            .await?
        }
    };
    let checker = put(
        client,
        artifacts,
        admin,
        "ConfidentialSpaceVerifier",
        (risc_zero, image_id, trusted_keys.to_vec()).abi_encode_params(),
    )
    .await?;
    propose(client, admin, registry, checker).await?;
    let timelock = client
        .read(registry, IAttestationRegistry::timelockCall {})
        .await?;
    Ok(Proposed {
        risc_zero,
        checker,
        timelock,
    })
}

/// Propose an already-deployed checker, unless the registry lists it.
pub async fn propose(
    client: &Client,
    admin: Address,
    registry: Address,
    checker: Address,
) -> Result<()> {
    let listed = client
        .read(registry, IAttestationRegistry::verifiersCall {})
        .await?;
    if listed.contains(&checker) {
        tracing::info!(%checker, "the checker is already proposed");
        return Ok(());
    }
    client
        .send(
            admin,
            registry,
            IAttestationRegistry::proposeVerifierCall { verifier: checker },
        )
        .await
        .context("proposing the checker")?;
    tracing::info!(%checker, "the Confidential Space checker is proposed; the timelock is running");
    Ok(())
}

async fn put(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
) -> Result<Address> {
    let mut bytecode = creation_code(artifacts, name)?;
    bytecode.extend_from_slice(&arguments);
    let address = client.deploy(from, bytecode).await?;
    tracing::info!(%address, contract = name, "deployed");
    Ok(address)
}

/// A contract's creation code from Foundry's build output. Neither contract
/// links a library.
fn creation_code(artifacts: &Path, name: &str) -> Result<Vec<u8>> {
    let path = artifacts
        .join(format!("{name}.sol"))
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}. Run the contracts' build first: `task contracts:build`",
            path.display()
        )
    })?;
    let artifact: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if artifact["bytecode"]["linkReferences"]
        .as_object()
        .is_some_and(|links| !links.is_empty())
    {
        bail!("{name} links a library, which this deploy does not fill in");
    }
    let hex = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?;
    alloy::hex::decode(hex.trim_start_matches("0x")).with_context(|| format!("{name}'s bytecode"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_s_published_keys_become_two_distinct_trusted_hashes() {
        let keys = trusted_keys(shonin_attestation_core::GOOGLE_JWKS_2026_10_06).unwrap();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn a_key_set_with_no_rsa_keys_is_refused() {
        assert!(trusted_keys(r#"{"keys": []}"#).is_err());
        assert!(trusted_keys(r#"{"keys": [{"kty": "EC"}]}"#).is_err());
    }
}
