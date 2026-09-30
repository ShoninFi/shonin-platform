//! Putting the contracts on the chain, once.
//!
//! A deploy is idempotent: if the file is there and the contracts it names are
//! still on the chain, nothing happens. Restarting a service never redeploys
//! anything, and a fresh chain is deployed to on the first service that asks.

use std::path::Path;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, anyhow};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::usd::ILocalUsd;
use chain::bindings::venue::ILocalVenue;
use chain::{ChainKey, Client, Deployments, Role, Roles};

/// How long an image waits between being proposed and becoming blessable.
const TIMELOCK: u64 = 2 * 24 * 60 * 60;
/// How long a blessing lasts before an executor has to attest again.
const BLESSING_TTL: u64 = 72 * 60 * 60;
/// What a review costs, in the asset's own units.
const REVIEW_FEE: u64 = 1_000_000;
/// The least an author may create a vault with: the build, the review and a
/// week at the allowances a modest strategy declares.
const MINIMUM_TANK: u64 = 40_000_000;
/// What the venue asks a perp position to be backed by.
const MARGIN_BPS: u64 = 1_000;
/// What every account starts with, so nobody has to ask for money first.
const STARTING_BALANCE: u64 = 1_000_000_000_000;

/// The markets the venue lists, and what they start at.
pub const SPOT: &str = "BTC";
pub const PERP: &str = "BTC-PERP";
pub const STARTING_PRICE: u64 = 60_000_000_000;

/// Deploy if nothing is there, and return what is.
pub async fn ensure(
    client: &Client,
    key: &ChainKey,
    artifacts: &Path,
    file: &Path,
) -> Result<Deployments> {
    if file.exists()
        && let Ok(existing) = client.deployments(file).await
        && client.has_code(existing.factory).await.unwrap_or(false)
    {
        tracing::info!(file = %file.display(), "the contracts are already on this chain");
        return Ok(existing);
    }

    let deployments = deploy(client, key, artifacts).await?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, deployments.to_json())
        .with_context(|| format!("writing {}", file.display()))?;
    tracing::info!(file = %file.display(), "the addresses are written");
    Ok(deployments)
}

async fn deploy(client: &Client, key: &ChainKey, artifacts: &Path) -> Result<Deployments> {
    let roles = Roles {
        admin: client.account(Role::Admin)?,
        platform: client.account(Role::Platform)?,
        attestor: client.account(Role::Attestor)?,
        builder: client.account(Role::Builder)?,
        reviewer: client.account(Role::Reviewer)?,
        executor: client.account(Role::Executor)?,
        keeper: client.account(Role::Keeper)?,
        operator: client.account(Role::Operator)?,
        author: client.account(Role::Author)?,
        viewer: client.account(Role::Viewer)?,
    };
    let admin = roles.admin;
    let from_block = client.block_number().await?;

    let usd = put(client, artifacts, admin, "LocalUsd", ().abi_encode_params()).await?;
    let venue = put(
        client,
        artifacts,
        admin,
        "LocalVenue",
        (usd, roles.operator, U256::from(MARGIN_BPS)).abi_encode_params(),
    )
    .await?;
    let verifier = put(
        client,
        artifacts,
        admin,
        "DevAttestationVerifier",
        (roles.attestor,).abi_encode_params(),
    )
    .await?;
    let attestation = put(
        client,
        artifacts,
        admin,
        "AttestationRegistry",
        (admin, verifier, TIMELOCK, BLESSING_TTL).abi_encode_params(),
    )
    .await?;
    let strategies = put(
        client,
        artifacts,
        admin,
        "StrategyRegistry",
        (attestation,).abi_encode_params(),
    )
    .await?;
    let reviews = put(
        client,
        artifacts,
        admin,
        "ReviewRegistry",
        (
            attestation,
            strategies,
            usd,
            roles.platform,
            U256::from(REVIEW_FEE),
        )
            .abi_encode_params(),
    )
    .await?;
    let profiles = put(
        client,
        artifacts,
        admin,
        "AuthorProfiles",
        ().abi_encode_params(),
    )
    .await?;
    let factory = put(
        client,
        artifacts,
        admin,
        "VaultFactory",
        (
            usd,
            venue,
            attestation,
            strategies,
            reviews,
            admin,
            roles.platform,
            // The terms every vault is created with, in the order
            // `VaultConfig` declares them.
            (
                100u16,              // streaming, a year
                1_000u16,            // of gains, to the author
                500u16,              // of the same gains, to the platform
                10u16,               // on a deposit or a withdrawal, kept in the pool
                100u16,              // the exit band
                10_000u16,           // how much of an author's fee refills the tank
                7 * 24 * 60 * 60u32, // an unserved claim freezes the vault after
                30u32,               // days of charges the tank aims to hold
                U256::from(MINIMUM_TANK),
            ),
        )
            .abi_encode_params(),
    )
    .await?;

    let deployments = Deployments {
        chain_key: key.to_string(),
        chain_id: client.chain_id,
        rpc_url: client.rpc_url.clone(),
        deployed_block: from_block,
        usd,
        venue,
        attestation_verifier: verifier,
        attestation,
        strategies,
        reviews,
        profiles,
        factory,
        roles,
    };

    list_markets(client, &deployments).await?;
    propose_images(client, &deployments).await?;
    hand_out_money(client, &deployments).await?;
    Ok(deployments)
}

/// Deploy one contract, from the bytecode the build tool left behind.
async fn put(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
) -> Result<Address> {
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
    let hex = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?;
    let mut bytecode = alloy::hex::decode(hex.trim_start_matches("0x"))
        .with_context(|| format!("{name}'s bytecode"))?;
    bytecode.extend_from_slice(&arguments);

    let address = client.deploy(from, bytecode).await?;
    tracing::info!(%address, contract = name, "deployed");
    Ok(address)
}

async fn list_markets(client: &Client, deployments: &Deployments) -> Result<()> {
    for (id, is_perp) in [(SPOT, false), (PERP, true)] {
        client
            .send(
                deployments.roles.operator,
                deployments.venue,
                ILocalVenue::listMarketCall {
                    id: market_id(id),
                    isPerp: is_perp,
                    price: U256::from(STARTING_PRICE),
                },
            )
            .await?;
    }
    tracing::info!("the venue has listed its markets");
    Ok(())
}

/// Propose the images the platform's own services run as. The timelock has to
/// pass before any of them can hold a key, which is the point of it.
async fn propose_images(client: &Client, deployments: &Deployments) -> Result<()> {
    for (digest, kind) in [
        (image_digest("executor"), ImageKind::Executor),
        (image_digest("builder"), ImageKind::Builder),
        (image_digest("reviewer"), ImageKind::Reviewer),
        (image_digest("unwinder"), ImageKind::Unwinder),
    ] {
        client
            .send(
                deployments.roles.admin,
                deployments.attestation,
                IAttestationRegistry::proposeImageCall {
                    imageDigest: digest,
                    kind,
                },
            )
            .await?;
    }
    client
        .send(
            deployments.roles.admin,
            deployments.attestation,
            IAttestationRegistry::proposeProjectCall {
                projectId: project_id(),
            },
        )
        .await?;
    tracing::info!("the images are proposed; the timelock is running");
    Ok(())
}

async fn hand_out_money(client: &Client, deployments: &Deployments) -> Result<()> {
    for who in [
        deployments.roles.author,
        deployments.roles.viewer,
        deployments.roles.keeper,
        deployments.roles.admin,
    ] {
        client
            .send(
                deployments.roles.admin,
                deployments.usd,
                ILocalUsd::mintCall {
                    to: who,
                    amount: U256::from(STARTING_BALANCE),
                },
            )
            .await?;
    }
    tracing::info!("the accounts are funded");
    Ok(())
}

/// A market's name, as the contract stores it.
pub fn market_id(name: &str) -> alloy::primitives::FixedBytes<32> {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    out[..bytes.len()].copy_from_slice(bytes);
    alloy::primitives::FixedBytes(out)
}

/// What a measured image would be, if there were one. On this chain there is
/// no enclave to measure, so each service's name stands for its image, and the
/// registry treats it exactly as it would a real digest.
pub fn image_digest(service: &str) -> alloy::primitives::FixedBytes<32> {
    alloy::primitives::keccak256(format!("shonin-dev-image:{service}"))
}

pub fn project_id() -> alloy::primitives::FixedBytes<32> {
    alloy::primitives::keccak256("shonin-dev-project")
}
