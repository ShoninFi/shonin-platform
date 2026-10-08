//! The admin's changes to a chain's attestation registry that a deployment
//! doesn't make by itself: a real image digest, an operator's project, the
//! development checker retired or allowed again, and the root key's record
//! read or cleared.
//!
//! Each is signed by the admin role and safe to run again: a change already
//! made is reported and nothing is sent. The same changes on a Solana
//! cluster's registry are in [`solana`].

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

/// The same changes on a Solana cluster's registry, signed by the admin
/// role, each safe to run again.
pub mod solana {
    use anyhow::{Context, Result, anyhow, bail};
    use chain::bindings::attestation::ImageKind as EvmKind;
    use chain::solana::Platform;
    use chain::solana::programs::{common, cs_verifier, dev_verifier, registry};
    use chain::solana::{Keypair, Pubkey, Signer, SolanaClient};
    use chain::{Role, SolanaDeployments};
    use registry::state::{Config, ImageKind, ImageRecord, ProjectRecord};

    /// The image kind on Solana: its registry has no root-key holder, whose
    /// record is on the network's canonical chain alone.
    pub fn kind_of(kind: EvmKind) -> Option<ImageKind> {
        match kind {
            EvmKind::Executor => Some(ImageKind::Executor),
            EvmKind::Builder => Some(ImageKind::Builder),
            EvmKind::Reviewer => Some(ImageKind::Reviewer),
            EvmKind::Unwinder => Some(ImageKind::Unwinder),
            _ => None,
        }
    }

    /// The platform and the admin's key, checked to be the deployment's
    /// admin: the registry would refuse any other, after the fee.
    fn admin<'a>(
        client: &'a SolanaClient,
        deployments: &SolanaDeployments,
    ) -> Result<(Platform, &'a Keypair)> {
        let platform = Platform::from_deployments(deployments)?;
        let admin = client.role(Role::Admin)?;
        if admin.pubkey() != platform.admin {
            bail!(
                "the admin key here is {}, and the deployment's admin is {}: is the mnemonic the team's?",
                admin.pubkey(),
                platform.admin
            );
        }
        Ok((platform, admin))
    }

    fn hex(bytes: &[u8; 32]) -> String {
        format!("0x{}", alloy::hex::encode(bytes))
    }

    /// Propose `digest` as an image of `kind`, unless it already is.
    pub async fn propose_image(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
        digest: [u8; 32],
        kind: EvmKind,
    ) -> Result<String> {
        let Some(kind) = kind_of(kind) else {
            return Ok(
                "skipped: the Solana registry has no holder kind (the root key's record is on the canonical chain)"
                    .to_string(),
            );
        };
        let shown = hex(&digest);
        let image: Option<ImageRecord> =
            client.load(&registry::read::image_address(&digest)).await?;
        if let Some(image) = image.filter(|image| image.proposed) {
            if image.kind != kind {
                bail!(
                    "the image {shown} is already proposed as another kind ({:?}); an image is proposed once",
                    image.kind
                );
            }
            if image.revoked {
                bail!("the image {shown} was revoked; a revoked image can't be proposed again");
            }
            return Ok(format!("the image {shown} is already proposed"));
        }
        let (platform, admin) = admin(client, deployments)?;
        client
            .send(vec![platform.ix_propose_image(digest, kind)], &[admin])
            .await
            .context("proposing the image")?;
        Ok(format!("the image {shown} is proposed"))
    }

    /// Revoke the image `digest`, unless it already is. One never proposed
    /// here has nothing to revoke, and a revocation of it wouldn't last
    /// (proposing it later allows it afresh), so nothing is sent.
    pub async fn revoke_image(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
        digest: [u8; 32],
    ) -> Result<String> {
        let shown = hex(&digest);
        let image: Option<ImageRecord> =
            client.load(&registry::read::image_address(&digest)).await?;
        match image {
            Some(image) if image.proposed && image.revoked => {
                return Ok(format!("the image {shown} is already revoked"));
            }
            Some(image) if image.proposed => {}
            _ => {
                return Ok(format!(
                    "the image {shown} was never proposed here: nothing to revoke"
                ));
            }
        }
        let (platform, admin) = admin(client, deployments)?;
        client
            .send(vec![platform.ix_revoke_image(digest)], &[admin])
            .await
            .context("revoking the image")?;
        Ok(format!("the image {shown} is revoked"))
    }

    /// Propose the operator project `project` (its hash), unless it already
    /// is.
    pub async fn propose_project(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
        project: &str,
    ) -> Result<String> {
        let id = super::project_id(project).0;
        let shown = hex(&id);
        let record: Option<ProjectRecord> =
            client.load(&registry::read::project_address(&id)).await?;
        if let Some(record) = record.filter(|record| record.proposed) {
            let now = client.now().await?;
            return Ok(if record.revoked {
                format!("the project {project} ({shown}) was revoked; it can't be proposed again")
            } else if record.is_allowed(now) {
                format!("the project {project} ({shown}) is already allowed")
            } else {
                format!(
                    "the project {project} ({shown}) is already proposed (waiting out the timelock)"
                )
            });
        }
        let (platform, admin) = admin(client, deployments)?;
        client
            .send(vec![platform.ix_propose_project(id)], &[admin])
            .await
            .context("proposing the project")?;
        Ok(format!("the project {project} ({shown}) is proposed"))
    }

    /// Every checker the registry lists (proposed and not retired), with
    /// whether it counts now and whether it is a development checker.
    async fn checkers(client: &SolanaClient) -> Result<Vec<(Pubkey, bool, bool)>> {
        let config: Config = client
            .load(&registry::read::config_address())
            .await?
            .ok_or_else(|| anyhow!("the registry is not set up on {}", client.key))?;
        let now = client.now().await?;
        Ok(config
            .verifiers
            .iter()
            .map(|entry| {
                (
                    entry.state,
                    now >= entry.ready_at,
                    entry.program == dev_verifier::ID,
                )
            })
            .collect())
    }

    /// The development checkers to retire, and whether that leaves a checker
    /// that counts: every listed development checker, those still waiting out
    /// the timelock too (they would count later), and only while another
    /// checker counts, since retiring the last would leave nothing able to
    /// bless.
    pub fn to_retire(listed: &[(Pubkey, bool, bool)]) -> Result<Vec<Pubkey>> {
        let dev: Vec<Pubkey> = listed
            .iter()
            .filter(|(_, _, dev)| *dev)
            .map(|(state, _, _)| *state)
            .collect();
        if !dev.is_empty() && !listed.iter().any(|(_, allowed, dev)| *allowed && !*dev) {
            bail!(
                "the development checker is the only checker allowed: propose the real one and let it count first"
            );
        }
        Ok(dev)
    }

    /// Retire every development checker, refused while no other checker
    /// counts.
    pub async fn retire_dev_checker(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
    ) -> Result<String> {
        let dev = to_retire(&checkers(client).await?)?;
        if dev.is_empty() {
            return Ok("no development checker is listed: nothing to retire".to_string());
        }
        let (platform, admin) = admin(client, deployments)?;
        let retire = dev
            .iter()
            .map(|state| platform.ix_retire_verifier(*state))
            .collect();
        client
            .send(retire, &[admin])
            .await
            .context("retiring the development checker")?;
        let named: Vec<String> = dev.iter().map(Pubkey::to_string).collect();
        Ok(format!(
            "retired the development checker {}",
            named.join(", ")
        ))
    }

    /// Create the Confidential Space checker for the attestation guest
    /// `image_id`, trusting the Google keys `keys`, and propose it beside the
    /// checkers already listed; it counts after the timelock. Its address is
    /// fixed by the guest and the keys, so running this again creates
    /// nothing and proposes nothing: a checker the registry ever listed is
    /// never proposed again (one retired stays retired).
    pub async fn propose_cs_checker(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
        image_id: [u8; 32],
        keys: &[[u8; 32]],
    ) -> Result<String> {
        let (platform, admin) = admin(client, deployments)?;
        let (create, state) = Platform::ix_create_cs_checker(&admin.pubkey(), image_id, keys);
        let record: Option<registry::state::VerifierRecord> = client
            .load(&registry::read::verifier_address(&state))
            .await?;
        if record.is_some_and(|record| record.proposed) {
            let listed = checkers(client).await?;
            return Ok(
                match listed.iter().find(|(listed, _, _)| *listed == state) {
                    Some((_, true, _)) => {
                        format!("the Confidential Space checker {state} is already allowed")
                    }
                    Some((_, false, _)) => format!(
                        "the Confidential Space checker {state} is proposed; it counts after the timelock"
                    ),
                    None => format!(
                        "the Confidential Space checker {state} was proposed once and retired: it is never proposed again (a new guest or key set is a new checker)"
                    ),
                },
            );
        }
        let mut ixs = Vec::new();
        if client.account(&state).await?.is_none() {
            ixs.push(create);
        }
        ixs.push(platform.ix_propose_verifier(cs_verifier::ID, state));
        client
            .send(ixs, &[admin])
            .await
            .context("creating and proposing the Confidential Space checker")?;
        let now = client.now().await?;
        let config: Config = client
            .load(&registry::read::config_address())
            .await?
            .ok_or_else(|| anyhow!("the registry is not set up"))?;
        Ok(format!(
            "the Confidential Space checker {state} is proposed; it counts after the timelock ({} s)",
            config.timelock(now)
        ))
    }

    /// Allow a development checker again, for a publish only it can bless: a
    /// retired checker can never be proposed again, so a fresh one is
    /// created, trusting the attestor role the deployment names, and
    /// proposed. One already listed is enough.
    pub async fn allow_dev_checker(
        client: &SolanaClient,
        deployments: &SolanaDeployments,
    ) -> Result<String> {
        let listed = checkers(client).await?;
        if let Some((state, allowed, _)) = listed.iter().find(|(_, _, dev)| *dev) {
            return Ok(if *allowed {
                format!("the development checker {state} is already allowed")
            } else {
                format!("the development checker {state} is proposed; it counts after the timelock")
            });
        }
        let (platform, admin) = admin(client, deployments)?;
        let attestor: Pubkey = deployments
            .roles
            .as_ref()
            .map(|roles| roles.attestor.as_str())
            .unwrap_or_default()
            .parse()
            .map_err(|_| anyhow!("the deployment names no attestor"))?;
        // A seed of its own, so the checker is one never proposed before.
        let now = client.now().await?;
        let seed = common::digest::keccak(&[b"shonin-dev-checker", &now.to_le_bytes()]);
        let (create, state) =
            Platform::ix_create_checker(&admin.pubkey(), attestor, seed, &client.cluster);
        let propose = platform.ix_propose_verifier(dev_verifier::ID, state);
        client
            .send(vec![create, propose], &[admin])
            .await
            .context("creating and proposing a development checker")?;
        let config: Config = client
            .load(&registry::read::config_address())
            .await?
            .ok_or_else(|| anyhow!("the registry is not set up"))?;
        Ok(format!(
            "the development checker {state} is proposed; it counts after the timelock ({} s). Retire it again after the publish: devnet retire-checker --dev",
            config.timelock(now)
        ))
    }
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
    fn the_solana_registry_takes_every_image_kind_but_the_root_key_s_holder() {
        use chain::solana::programs::registry::state::ImageKind as SolanaKind;
        for (name, kind) in [
            ("executor", SolanaKind::Executor),
            ("builder", SolanaKind::Builder),
            ("reviewer", SolanaKind::Reviewer),
            ("unwinder", SolanaKind::Unwinder),
        ] {
            assert_eq!(solana::kind_of(kind_named(name).unwrap()), Some(kind));
        }
        assert_eq!(solana::kind_of(kind_named("holder").unwrap()), None);
    }

    #[test]
    fn a_solana_development_checker_is_retired_only_while_another_counts() {
        use chain::solana::Pubkey;
        let dev = Pubkey::new_unique();
        let pending_dev = Pubkey::new_unique();
        let real = Pubkey::new_unique();
        // Alone, it stays: nothing else could bless.
        assert!(solana::to_retire(&[(dev, true, true)]).is_err());
        // A real checker still waiting out its timelock doesn't count yet.
        assert!(solana::to_retire(&[(dev, true, true), (real, false, false)]).is_err());
        // Beside a real one that counts, every development checker goes,
        // one still waiting out its timelock too.
        assert_eq!(
            solana::to_retire(&[
                (dev, true, true),
                (pending_dev, false, true),
                (real, true, false)
            ])
            .unwrap(),
            vec![dev, pending_dev]
        );
        // None listed: nothing to do.
        assert!(
            solana::to_retire(&[(real, true, false)])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn only_the_five_image_kinds_are_named() {
        assert!(kind_named("executor").is_ok());
        assert!(kind_named("unwinder").is_ok());
        assert!(kind_named("holder").is_ok());
        assert!(kind_named("none").is_err());
    }
}
