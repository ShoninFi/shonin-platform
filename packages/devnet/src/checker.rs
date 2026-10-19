//! The Confidential Space checker, proposed beside the development checker.
//!
//! It deploys RISC Zero's Groth16 verifier and the checker that accepts a
//! proof of a Confidential Space attestation, trusting Google's Confidential
//! Space Root CA (PKI tokens) and Google's signing keys as pinned (OIDC
//! tokens), and proposes the checker to the chain's attestation registry.
//! The development checker stays: nothing here retires it, so both count
//! once the timelock passes.
//!
//! The addresses are written beside the chain's deployment, and a checker
//! already there is only proposed again if the registry doesn't list it, so
//! running it twice deploys nothing new. A fresh deployment (a new
//! generation) proposes the checkers already on the chain when their files
//! (`attestation-<chain>.json`) are copied into its deployments directory:
//! a checker named there that has code, pins the guest this build pins and
//! trusts what this build trusts is proposed as it is, never deployed again.
//! One that pins another guest or trusts otherwise (a guest change, or a
//! newly pinned key set) is a rotation: a new checker is deployed beside it,
//! on the same RISC Zero verifier, and proposed, and the earlier one stays
//! allowed until it is retired deliberately, once the machines are blessed
//! through the new one.

use std::path::Path;

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, bail};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Role};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::PINNED_GOOGLE_JWKS;
use shonin_attestation_deploy::{GUEST_IMAGE_ID, deploy_and_propose, propose, trusted_hashes};

alloy::sol! {
    interface IConfidentialSpaceChecker {
        function imageId() external view returns (bytes32);
        function trustedKeys() external view returns (bytes32[] memory);
    }
}

#[derive(Serialize, Deserialize)]
struct Written {
    risc_zero: Address,
    checker: Address,
    /// The checkers this one was deployed beside, newest first: still
    /// proposed or allowed until each is retired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    earlier: Vec<Address>,
}

/// Whether a checker pinning `image` and trusting `trusted` is the one this
/// build deploys: the same guest, and the same hashes in any order.
fn is_current(image: B256, trusted: &[B256], wanted: &[B256]) -> bool {
    let sorted = |hashes: &[B256]| {
        let mut hashes = hashes.to_vec();
        hashes.sort();
        hashes.dedup();
        hashes
    };
    image == GUEST_IMAGE_ID && sorted(trusted) == sorted(wanted)
}

pub async fn run(
    network: NetworkName,
    chains: Vec<(ChainKey, ChainConfig)>,
    deployments_dir: &Path,
    artifacts: &Path,
) -> Result<()> {
    let keys = trusted_hashes(PINNED_GOOGLE_JWKS)?;
    // Every chain at once: each is its own registry and its own admin
    // nonce. Each chain's outcome is printed; the command fails if any did.
    let each = chains.into_iter().map(|(key, config)| {
        let keys = &keys;
        async move {
            let outcome = on_chain(network, &key, &config, deployments_dir, artifacts, keys).await;
            (key, config, outcome)
        }
    });
    let mut failed = Vec::new();
    for (key, config, outcome) in futures::future::join_all(each).await {
        match outcome {
            Ok(said) => println!("{} ({key}): {said}", config.label),
            Err(error) => {
                println!("{} ({key}): failed: {error:#}", config.label);
                failed.push(format!("{} ({key})", config.label));
            }
        }
    }
    if !failed.is_empty() {
        bail!("not done on {}", failed.join(", "));
    }
    Ok(())
}

/// One chain's checker: the one its file names, if that is on the chain and
/// is this build's (its guest and its trusted hashes), else a new one beside
/// it; proposed to its registry.
async fn on_chain(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
    deployments_dir: &Path,
    artifacts: &Path,
    keys: &[B256],
) -> Result<String> {
    let client = chain::connect_to(network, key, config).await?;
    let file = Deployments::file_in(deployments_dir, key);
    let deployments = client
        .deployments(&file)
        .await
        .with_context(|| format!("no deployment on {key}: run chains:deploy first"))?;
    let admin = client.account(Role::Admin)?;
    let written_file = deployments_dir.join(format!(
        "attestation-{}.json",
        key.to_string().replace(':', "-")
    ));
    let existing = std::fs::read_to_string(&written_file)
        .ok()
        .and_then(|text| serde_json::from_str::<Written>(&text).ok());
    let written = match existing {
        Some(written) if client.has_code(written.checker).await.unwrap_or(false) => {
            let image = client
                .read(written.checker, IConfidentialSpaceChecker::imageIdCall {})
                .await
                .with_context(|| format!("reading the guest {:?} pins", written.checker))?;
            let trusted = client
                .read(
                    written.checker,
                    IConfidentialSpaceChecker::trustedKeysCall {},
                )
                .await
                .with_context(|| format!("reading what {:?} trusts", written.checker))?;
            if is_current(image, &trusted, keys) {
                propose(&client, admin, deployments.attestation, written.checker).await?;
                written
            } else {
                // A rotation: a new checker beside the earlier one, on the
                // same RISC Zero verifier.
                let proposed = deploy_and_propose(
                    &client,
                    admin,
                    artifacts,
                    deployments.attestation,
                    Some(written.risc_zero),
                    GUEST_IMAGE_ID,
                    keys,
                )
                .await?;
                let mut earlier = vec![written.checker];
                earlier.extend(written.earlier);
                let rotated = Written {
                    risc_zero: proposed.risc_zero,
                    checker: proposed.checker,
                    earlier,
                };
                std::fs::write(&written_file, serde_json::to_string_pretty(&rotated)?)
                    .with_context(|| format!("writing {}", written_file.display()))?;
                tracing::info!(
                    earlier = ?rotated.earlier[0],
                    checker = ?rotated.checker,
                    "a new Confidential Space checker is proposed beside the earlier one, which stays until it is retired"
                );
                rotated
            }
        }
        _ => {
            let proposed = deploy_and_propose(
                &client,
                admin,
                artifacts,
                deployments.attestation,
                None,
                GUEST_IMAGE_ID,
                keys,
            )
            .await?;
            let written = Written {
                risc_zero: proposed.risc_zero,
                checker: proposed.checker,
                earlier: Vec::new(),
            };
            std::fs::write(&written_file, serde_json::to_string_pretty(&written)?)
                .with_context(|| format!("writing {}", written_file.display()))?;
            written
        }
    };
    Ok(format!(
        "the Confidential Space checker {:?} (RISC Zero's verifier {:?}) is proposed to the registry {:?}",
        written.checker, written.risc_zero, deployments.attestation
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checker_of_this_guest_trusting_the_same_hashes_in_any_order_is_current() {
        let wanted = trusted_hashes(PINNED_GOOGLE_JWKS).unwrap();
        let mut reversed = wanted.clone();
        reversed.reverse();
        assert!(is_current(GUEST_IMAGE_ID, &reversed, &wanted));
    }

    #[test]
    fn a_checker_of_another_guest_or_trusting_otherwise_is_rotated() {
        let wanted = trusted_hashes(PINNED_GOOGLE_JWKS).unwrap();
        assert!(!is_current(B256::repeat_byte(1), &wanted, &wanted));
        // A checker trusting the keys alone, as before the root was pinned.
        assert!(!is_current(GUEST_IMAGE_ID, &wanted[1..], &wanted));
        let mut more = wanted.clone();
        more.push(B256::repeat_byte(2));
        assert!(!is_current(GUEST_IMAGE_ID, &more, &wanted));
    }

    #[test]
    fn a_checker_file_written_before_rotations_reads_with_none_earlier() {
        let old = r#"{"risc_zero": "0x0000000000000000000000000000000000000001", "checker": "0x0000000000000000000000000000000000000002"}"#;
        let written: Written = serde_json::from_str(old).unwrap();
        assert!(written.earlier.is_empty());
        assert!(!serde_json::to_string(&written).unwrap().contains("earlier"));
    }
}
