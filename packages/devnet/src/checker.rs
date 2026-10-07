//! The Confidential Space checker, proposed beside the development checker.
//!
//! It deploys RISC Zero's Groth16 verifier and the checker that accepts a
//! proof of a Confidential Space attestation, trusting Google's signing keys
//! as published when it is deployed, and proposes the checker to the chain's
//! attestation registry. The development checker stays: nothing here retires
//! it, so both count once the timelock passes.
//!
//! The addresses are written beside the chain's deployment, and a checker
//! already there is only proposed again if the registry doesn't list it, so
//! running it twice deploys nothing new. A fresh deployment (a new
//! generation) proposes the checkers already on the chain when their files
//! (`attestation-<chain>.json`) are copied into its deployments directory:
//! a checker named there that has code and trusts the guest this build
//! pins is proposed as it is, never deployed again.

use std::path::Path;

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, bail};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Role};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::GOOGLE_JWKS_2026_10_06;
use shonin_attestation_deploy::{GUEST_IMAGE_ID, deploy_and_propose, propose, trusted_keys};

alloy::sol! {
    interface IConfidentialSpaceChecker {
        function imageId() external view returns (bytes32);
    }
}

#[derive(Serialize, Deserialize)]
struct Written {
    risc_zero: Address,
    checker: Address,
}

pub async fn run(
    network: NetworkName,
    chains: Vec<(ChainKey, ChainConfig)>,
    deployments_dir: &Path,
    artifacts: &Path,
) -> Result<()> {
    let keys = trusted_keys(GOOGLE_JWKS_2026_10_06)?;
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
/// trusts this build's guest, else a new one; proposed to its registry.
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
                .with_context(|| format!("reading the guest {:?} trusts", written.checker))?;
            if image != GUEST_IMAGE_ID {
                bail!(
                    "the checker {:?} named in {} trusts the guest {image}, not this build's {GUEST_IMAGE_ID}: move the file aside to deploy a new one",
                    written.checker,
                    written_file.display()
                );
            }
            propose(&client, admin, deployments.attestation, written.checker).await?;
            written
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
