//! The keeper: the clockwork nobody has to trust.
//!
//! It crystallises each vault's fees once a chain-day, settles the claims
//! depositors have booked, and freezes a vault whose claim has gone unserved
//! for too long. Every one of those calls is public: anyone may send them, and
//! a depositor who does not want to wait can send their own. The keeper
//! exists so that nobody has to.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use anyhow::Result;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::{ClaimState, IVault, VaultState};
use chain::{Client, Deployments, Role};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Keeps each vault's clockwork turning")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    api_url: String,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let target = args.chain_args.resolve()?;
    let chain = target.connect().await?;
    let deployments = chain
        .wait_for_deployments(&target.deployments_file())
        .await?;
    let keeper = chain.account(Role::Keeper)?;
    tracing::info!(%keeper, "the keeper is up");

    if args.port != 0 {
        tokio::spawn(serve_status(SocketAddr::from(([127, 0, 0, 1], args.port))));
    }

    loop {
        if let Err(error) = tick(&chain, &deployments, keeper).await {
            tracing::warn!(%error, "the round did not finish");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn tick(chain: &Client, deployments: &Deployments, keeper: Address) -> Result<()> {
    let now = chain.block_timestamp().await?;
    let vaults = chain
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;

    for vault in vaults {
        let state = chain.read(vault, IVault::stateCall {}).await?;
        if matches!(state, VaultState::Building | VaultState::Closed) {
            continue;
        }

        // Fees are proportional to elapsed time, so a late call loses nothing
        // and an early one gains nothing. Once a chain-day is plenty.
        let last = chain.read(vault, IVault::lastCrystallisedCall {}).await? as u64;
        if now.saturating_sub(last) >= 86_400
            && let Err(error) = chain.send(keeper, vault, IVault::crystalliseCall {}).await
        {
            tracing::debug!(%vault, %error, "could not crystallise");
        }

        settle_claims(chain, keeper, vault, now).await?;
    }
    Ok(())
}

/// Serve every claim that is waiting, and freeze a vault whose claim has gone
/// unserved for its whole freeze period.
async fn settle_claims(chain: &Client, keeper: Address, vault: Address, now: u64) -> Result<()> {
    let count = chain.read(vault, IVault::claimCountCall {}).await?;
    let count: u64 = count.try_into().unwrap_or(0);
    let freeze_after = chain.read(vault, IVault::freezeAfterCall {}).await? as u64;

    for id in 1..=count {
        let claim = chain
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(id),
                },
            )
            .await?;
        if matches!(claim.state, ClaimState::Paid | ClaimState::None) {
            continue;
        }

        if let Err(error) = chain
            .send(
                keeper,
                vault,
                IVault::settleCall {
                    claimId: U256::from(id),
                },
            )
            .await
        {
            tracing::debug!(%vault, id, %error, "could not settle");
        }

        // A claim nobody could serve freezes the vault: no new deposits, and
        // the trading approvals go, so only unwinding is left.
        if now.saturating_sub(claim.bookedAt as u64) >= freeze_after
            && let Err(error) = chain.send(keeper, vault, IVault::freezeCall {}).await
        {
            tracing::debug!(%vault, %error, "could not freeze");
        }
    }
    Ok(())
}

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "keeper" }));
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}
