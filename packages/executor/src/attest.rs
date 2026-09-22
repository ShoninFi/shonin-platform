//! Proving what this is, and getting a key blessed for one vault.
//!
//! On a real deployment the enclave produces a proof no service could forge,
//! and the registry checks it against the platform's own trust root. Here the
//! development chain plays that part: a verifier contract trusts one account's
//! signature over the same journal, and everything downstream -- the
//! allowlist, the timelock, what a key may do and for how long -- works
//! exactly as it will with real hardware.

use alloy::primitives::{Address, B256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result};
use chain::bindings::attestation::{IAttestationRegistry, KeyInfo};
use chain::{Client, Deployments};

/// What the instance attests to. It matches `AttestationRegistry.Journal`.
type Journal = (B256, B256, Address, B256);

/// Bless `key` for one vault running one bundle, and return when the registry
/// has recorded it.
pub async fn bless(
    chain: &Client,
    deployments: &Deployments,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<KeyInfo> {
    // The one pair this instance holds a key for. A larger instance would
    // commit to a root over many, and prove one of them here.
    let leaf = keccak256(keccak256((vault, bundle).abi_encode()));
    let journal: Journal = (image_digest(), project_id(), key, leaf);
    let encoded = journal.abi_encode_params();

    let seal = chain
        .sign(deployments.roles.attestor, keccak256(&encoded))
        .await
        .context("the attestation was not sealed")?;

    chain
        .send(
            deployments.roles.executor,
            deployments.attestation,
            IAttestationRegistry::blessCall {
                seal,
                journal: encoded.into(),
                vault,
                strategyHash: bundle,
                merkleProof: Vec::new(),
            },
        )
        .await
        .context("the registry would not bless the key")?;

    let info = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::keyInfoCall { signer: key },
        )
        .await?;
    Ok(info)
}

/// What this executor's image is. There is no enclave here to measure, so the
/// service's name stands for its digest; the registry treats it exactly as it
/// would a real one, and the vault's page says which of the two it is.
pub fn image_digest() -> B256 {
    keccak256("shonin-dev-image:executor")
}

pub fn project_id() -> B256 {
    keccak256("shonin-dev-project")
}
