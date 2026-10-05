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
//! running it twice deploys nothing new.

use std::path::Path;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Role};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::GOOGLE_JWKS_2026_10_06;
use shonin_attestation_deploy::{GUEST_IMAGE_ID, deploy_and_propose, propose, trusted_keys};

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
    for (key, config) in chains {
        let client = chain::connect_to(network, &key, &config).await?;
        let file = Deployments::file_in(deployments_dir, &key);
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
                    &keys,
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
        println!(
            "{} ({key}): the Confidential Space checker {:?} (RISC Zero's verifier {:?}) is proposed to the registry {:?}",
            config.label, written.checker, written.risc_zero, deployments.attestation
        );
    }
    Ok(())
}
