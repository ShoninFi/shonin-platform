//! The admin's changes to a chain's attestation registry that a deployment
//! doesn't make by itself: a real image digest, an operator's project, the
//! development checker retired or allowed again, and the root key's record
//! read or cleared.
//!
//! Each is signed by the admin role and safe to run again: a change already
//! made is reported and nothing is sent.

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, keccak256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::{SolCall, SolError, SolValue};
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::{Client, Deployments, Role};

alloy::sol! {
    /// What every attestation checker answers.
    interface IChecker {
        function trustRoot() external view returns (bytes32);
    }

    /// The registry's refusal of a second proposal of the same thing.
    error AlreadyProposed();
}

/// The trust root the development checker names (`DevAttestationVerifier`).
pub const DEV_TRUST_ROOT: &str = "dev-local";

/// The image kinds an operator may name on the command line.
pub fn kind_named(name: &str) -> Result<ImageKind> {
    Ok(match name {
        "executor" => ImageKind::Executor,
        "builder" => ImageKind::Builder,
        "reviewer" => ImageKind::Reviewer,
        "unwinder" => ImageKind::Unwinder,
        "holder" => ImageKind::Holder,
        other => bail!("no image kind {other:?}: executor, builder, reviewer, unwinder or holder"),
    })
}

/// An image digest as a container registry writes it, `sha256:<64 hex>`, as
/// the registry's 32 bytes.
pub fn image_digest(text: &str) -> Result<B256> {
    let hex = text
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("an image digest is sha256:<64 hex>, not {text:?}"))?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("an image digest is sha256:<64 hex>, not {text:?}");
    }
    hex.parse::<B256>()
        .map_err(|_| anyhow!("an image digest is sha256:<64 hex>, not {text:?}"))
}

/// An operator's project as the registry holds it: the hash of its id.
pub fn project_id(project: &str) -> B256 {
    keccak256(project.trim().as_bytes())
}

fn bytes32_text(text: &str) -> B256 {
    let mut word = [0u8; 32];
    word[..text.len()].copy_from_slice(text.as_bytes());
    B256::from(word)
}

/// Propose `digest` as an image of `kind`, unless it already is.
pub async fn propose_image(
    client: &Client,
    deployments: &Deployments,
    digest: B256,
    kind: ImageKind,
) -> Result<String> {
    let image = client
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: digest,
            },
        )
        .await?;
    if image.allowedAt != 0 {
        if image.kind as u8 != kind as u8 {
            bail!(
                "the image {digest} is already proposed as another kind ({:?}); an image is proposed once",
                image.kind as u8
            );
        }
        if image.revoked {
            bail!("the image {digest} was revoked; a revoked image can't be proposed again");
        }
        return Ok(format!("the image {digest} is already proposed"));
    }
    let admin = client.account(Role::Admin)?;
    client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::proposeImageCall {
                imageDigest: digest,
                kind,
            },
        )
        .await
        .context("proposing the image")?;
    Ok(format!("the image {digest} is proposed"))
}

/// Revoke the image `digest`, unless it already is. A revoked image can
/// never be allowed again, and what was blessed or recorded from it stops
/// counting.
pub async fn revoke_image(
    client: &Client,
    deployments: &Deployments,
    digest: B256,
) -> Result<String> {
    let image = client
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: digest,
            },
        )
        .await?;
    if image.revoked {
        return Ok(format!("the image {digest} is already revoked"));
    }
    // One never proposed here has nothing to revoke, and a revocation of it
    // wouldn't last: proposing it later allows it afresh. So nothing is
    // sent, and the digest is checked rather than a mistyped one "revoked".
    if image.allowedAt == 0 {
        return Ok(format!(
            "the image {digest} was never proposed here: nothing to revoke"
        ));
    }
    let admin = client.account(Role::Admin)?;
    client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::revokeImageCall {
                imageDigest: digest,
            },
        )
        .await
        .context("revoking the image")?;
    Ok(format!("the image {digest} is revoked"))
}

/// Propose the operator project `project` (its hash), unless it already is.
pub async fn propose_project(
    client: &Client,
    deployments: &Deployments,
    project: &str,
) -> Result<String> {
    let id = project_id(project);
    let allowed = client
        .read(
            deployments.attestation,
            IAttestationRegistry::isProjectAllowedCall { projectId: id },
        )
        .await?;
    if allowed {
        return Ok(format!("the project {project} ({id}) is already allowed"));
    }
    let admin = client.account(Role::Admin)?;
    let call = IAttestationRegistry::proposeProjectCall { projectId: id };
    // A project proposed before (waiting out its timelock, or revoked) is
    // refused again by the registry: asked first, so nothing is paid for.
    if already_proposed(client, admin, deployments.attestation, &call).await? {
        return Ok(format!(
            "the project {project} ({id}) is already proposed (waiting out the timelock, or revoked)"
        ));
    }
    client
        .send(admin, deployments.attestation, call)
        .await
        .context("proposing the project")?;
    Ok(format!("the project {project} ({id}) is proposed"))
}

/// Whether the registry would refuse `call` from `from` as already proposed.
async fn already_proposed<C: SolCall>(
    client: &Client,
    from: Address,
    to: Address,
    call: &C,
) -> Result<bool> {
    let request = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_input(call.abi_encode());
    match client.provider().call(request).await {
        Ok(_) => Ok(false),
        Err(error) => {
            let revert = error
                .as_error_resp()
                .and_then(|response| response.as_revert_data());
            match revert {
                Some(data) if data.starts_with(&AlreadyProposed::SELECTOR) => Ok(true),
                _ => Err(anyhow::Error::new(error).context(format!("simulating {}", C::SIGNATURE))),
            }
        }
    }
}

/// Every checker the registry lists, with whether it counts now and
/// whether it is a development checker.
async fn checkers(
    client: &Client,
    deployments: &Deployments,
) -> Result<Vec<(Address, bool, bool)>> {
    let listed = client
        .read(
            deployments.attestation,
            IAttestationRegistry::verifiersCall {},
        )
        .await?;
    let mut all = Vec::new();
    for checker in listed {
        let allowed = client
            .read(
                deployments.attestation,
                IAttestationRegistry::isVerifierAllowedCall { verifier: checker },
            )
            .await?;
        let dev = client
            .read(checker, IChecker::trustRootCall {})
            .await
            .is_ok_and(|root| root == bytes32_text(DEV_TRUST_ROOT));
        all.push((checker, allowed, dev));
    }
    Ok(all)
}

/// Retire every allowed development checker, refused while no other
/// checker counts: retiring the last would leave nothing able to bless.
pub async fn retire_dev_checker(client: &Client, deployments: &Deployments) -> Result<String> {
    let all = checkers(client, deployments).await?;
    let dev: Vec<Address> = all
        .iter()
        .filter(|(_, allowed, dev)| *allowed && *dev)
        .map(|(checker, _, _)| *checker)
        .collect();
    if dev.is_empty() {
        return Ok("no development checker is allowed: nothing to retire".to_string());
    }
    if !all.iter().any(|(_, allowed, dev)| *allowed && !*dev) {
        bail!(
            "the development checker is the only checker allowed: propose the real one (chains:checker) and let it count first"
        );
    }
    let admin = client.account(Role::Admin)?;
    for checker in &dev {
        client
            .send(
                admin,
                deployments.attestation,
                IAttestationRegistry::retireVerifierCall { verifier: *checker },
            )
            .await
            .with_context(|| format!("retiring the development checker {checker}"))?;
    }
    Ok(format!("retired the development checker {dev:?}"))
}

/// Allow a development checker again, for a publish only it can bless: a
/// retired checker can never be proposed again, so a fresh one is deployed,
/// trusting the attestor role, and proposed. One already allowed is enough.
pub async fn allow_dev_checker(
    client: &Client,
    deployments: &Deployments,
    artifacts: &std::path::Path,
) -> Result<String> {
    let all = checkers(client, deployments).await?;
    if let Some((checker, _, _)) = all.iter().find(|(_, allowed, dev)| *allowed && *dev) {
        return Ok(format!(
            "the development checker {checker} is already allowed"
        ));
    }
    let admin = client.account(Role::Admin)?;
    let mut bytecode = crate::deploy::creation_code(artifacts, "DevAttestationVerifier", &[])?;
    bytecode.extend_from_slice(&(deployments.roles.attestor,).abi_encode_params());
    let checker = client
        .deploy(admin, bytecode)
        .await
        .context("deploying a development checker")?;
    client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::proposeVerifierCall { verifier: checker },
        )
        .await
        .context("proposing the development checker")?;
    let timelock = client
        .read(
            deployments.attestation,
            IAttestationRegistry::timelockCall {},
        )
        .await?;
    Ok(format!(
        "the development checker {checker} is proposed; it counts after the timelock ({timelock} s). Retire it again after the publish: devnet retire-checker --dev"
    ))
}

/// The root key's record, said in one line: the signer, when and through
/// which checker it was recorded, its image and project, and whether it is
/// live.
pub async fn root_key(client: &Client, deployments: &Deployments) -> Result<String> {
    let record = client
        .read(
            deployments.attestation,
            IAttestationRegistry::rootKeyCall {},
        )
        .await?;
    if record.signer == Address::ZERO {
        return Ok("no root key is recorded".to_string());
    }
    let live = client
        .read(
            deployments.attestation,
            IAttestationRegistry::isRootKeyLiveCall {},
        )
        .await?;
    Ok(format!(
        "root key signer {} ({}), attested at {} and recorded at {} through checker {}, image {}, project {}",
        record.signer,
        if live { "live" } else { "NOT live" },
        record.issuedAt,
        record.setAt,
        record.verifier,
        record.imageDigest,
        record.projectId,
    ))
}

/// Clear the root key's record, as the admin, and say the signer cleared.
/// That signer can never be recorded again.
pub async fn clear_root_key(client: &Client, deployments: &Deployments) -> Result<String> {
    let record = client
        .read(
            deployments.attestation,
            IAttestationRegistry::rootKeyCall {},
        )
        .await?;
    if record.signer == Address::ZERO {
        return Ok("no root key is recorded: nothing to clear".to_string());
    }
    let admin = client.account(Role::Admin)?;
    client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::clearRootKeyCall {},
        )
        .await
        .with_context(|| format!("clearing the root key {}", record.signer))?;
    Ok(format!(
        "cleared the root key signer {}; it is refused for good, and the executor makes a new root",
        record.signer
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_digest_is_read_from_the_registry_s_spelling_and_nothing_else() {
        let hex = "b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342";
        assert_eq!(
            image_digest(&format!("sha256:{hex}")).unwrap(),
            hex.parse::<B256>().unwrap()
        );
        assert!(image_digest(hex).is_err());
        assert!(image_digest(&format!("sha256:{}", &hex[1..])).is_err());
        assert!(image_digest(&format!("sha256:0x{}", &hex[2..])).is_err());
    }

    #[test]
    fn a_project_is_the_hash_of_its_id() {
        assert_eq!(
            project_id("shonin-testnet"),
            "0xb585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f"
                .parse::<B256>()
                .unwrap()
        );
    }

    #[test]
    fn the_development_trust_root_is_the_checker_s_own() {
        let mut word = [0u8; 32];
        word[..9].copy_from_slice(b"dev-local");
        assert_eq!(bytes32_text(DEV_TRUST_ROOT), B256::from(word));
    }

    #[test]
    fn only_the_five_image_kinds_are_named() {
        assert!(kind_named("executor").is_ok());
        assert!(kind_named("unwinder").is_ok());
        assert!(kind_named("holder").is_ok());
        assert!(kind_named("none").is_err());
    }
}
