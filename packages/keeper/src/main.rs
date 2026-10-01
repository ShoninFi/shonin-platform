//! The keeper: the clockwork nobody has to trust.
//!
//! Once a chain-day it collects each vault's bill from its tank, with the
//! running total the executor signed. Every slot it values each vault and
//! settles what the valuation prices. It tries every claim that is waiting,
//! freezes a vault whose claim has gone untried too long, evicts a retired
//! vault whose wind-down is over and cancels one that never opened. Once a
//! vault has halted and its grace is over, it stops the station, unwinds it,
//! and closes the vault when a valuation shows the station empty.
//!
//! Every one of those calls is public: anyone may send them, and a depositor
//! who does not want to wait can send their own. The keeper exists so that
//! nobody has to.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Context, Result};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{ClaimState, IVault, VaultState};
use chain::{Client, Deployments, Role};
use clap::Parser;
use serde::Deserialize;
use shared::Chain;

#[derive(Parser)]
#[command(about = "Keeps each vault's clockwork turning")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    api_url: String,
    /// Where the executor serves each vault's signed running total.
    #[arg(long)]
    executor_url: String,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

/// A running total as the executor serves it.
#[derive(Deserialize)]
struct Signed {
    total: String,
    as_of: u64,
    signature: Bytes,
}

const DAY: u64 = 86_400;
const WAD: u64 = 1_000_000_000_000_000_000;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let served = args.chain_args.connect_all().await?;

    if args.port != 0 {
        tokio::spawn(serve_status(SocketAddr::from(([127, 0, 0, 1], args.port))));
    }

    // Every vault chain turns on its own, so a slow chain holds up no other.
    let mut chains = tokio::task::JoinSet::new();
    for served in served {
        let keeper = served.client.account(Role::Keeper)?;
        let executor = args.executor_url.clone();
        tracing::info!(chain = %served.key, %keeper, "the keeper is up");
        chains.spawn(async move {
            loop {
                if let Err(error) = tick(
                    served.vault_chain,
                    &served.client,
                    &served.deployments,
                    keeper,
                    &executor,
                )
                .await
                {
                    tracing::warn!(chain = %served.key, %error, "the round did not finish");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
    while chains.join_next().await.is_some() {}
    Ok(())
}

async fn tick(
    chain: Chain,
    client: &Client,
    deployments: &Deployments,
    keeper: Address,
    executor: &str,
) -> Result<()> {
    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    for vault in vaults {
        if let Err(error) = keep_one(client, keeper, executor, vault).await {
            tracing::debug!(%chain, %vault, error = format!("{error:#}"), "a step did not go through");
        }
    }
    Ok(())
}

/// Everything one vault might need this round. Each step is tried on its own:
/// one that can't go through yet doesn't hold up the others.
async fn keep_one(client: &Client, keeper: Address, executor: &str, vault: Address) -> Result<()> {
    let state = client.read(vault, IVault::stateCall {}).await?;
    let now = client.block_timestamp().await?;
    let timers = client.read(vault, IVault::timersCall {}).await?;

    match state {
        VaultState::Closed | VaultState::Cancelled | VaultState::None => return Ok(()),
        VaultState::Activating => {
            let created = client.read(vault, IVault::createdAtCall {}).await?;
            if now >= created + timers.activationWindow {
                send(client, keeper, vault, IVault::cancelCall {}, "cancel").await;
            }
            return Ok(());
        }
        _ => {}
    }

    // The bill, once a chain-day, with the total the executor signed. Without
    // one the call still tops the tank up from the author's shares.
    let tank = client.read(vault, IVault::tankCall {}).await?;
    if now.saturating_sub(tank.lastCollection) >= DAY {
        let signed = fetch_total(client.chain_id, executor, vault).await;
        let call = match signed {
            Some(signed) => IVault::keepCall {
                total: signed.total.parse().unwrap_or_default(),
                asOf: signed.as_of,
                signature: signed.signature,
            },
            None => IVault::keepCall {
                total: U256::ZERO,
                asOf: 0,
                signature: Bytes::new(),
            },
        };
        send(client, keeper, vault, call, "keep").await;
    }

    price(
        client,
        keeper,
        vault,
        now,
        timers.slotInterval,
        timers.slotWindow,
    )
    .await;
    claims(client, keeper, vault, now, timers.freezeAfter).await?;

    if matches!(state, VaultState::WindingDown) {
        let halted = client.read(vault, IStationVault::haltedAtCall {}).await?;
        let retired = retired_at(client, vault).await;
        if halted == 0 && retired.is_some_and(|at| now >= at + timers.windDown) {
            send(client, keeper, vault, IVault::evictCall {}, "evict").await;
        }
    }

    stop_and_close(client, keeper, vault, now).await
}

/// Value the vault at the slot, and settle what has been priced.
async fn price(
    client: &Client,
    keeper: Address,
    vault: Address,
    now: u64,
    interval: u64,
    window: u64,
) {
    let slot = now - now % interval;
    let count = client
        .read(vault, IVault::valuationCountCall {})
        .await
        .unwrap_or_default();
    let last = if count.is_zero() {
        0
    } else {
        client
            .read(
                vault,
                IVault::valuationAtCall {
                    index: count - U256::from(1u64),
                },
            )
            .await
            .map(|valuation| valuation.time)
            .unwrap_or(0)
    };
    let settle = IVault::settleCall {
        max: U256::from(100u64),
    };
    if now - slot <= window && last < slot {
        send(client, keeper, vault, settle.clone(), "settle").await;
        send(
            client,
            keeper,
            vault,
            IVault::recordValuationCall {},
            "value",
        )
        .await;
    }
    send(client, keeper, vault, settle, "settle").await;
}

/// Try every claim that is waiting, and freeze a vault whose claim nobody has
/// tried for its whole freeze period.
async fn claims(
    client: &Client,
    keeper: Address,
    vault: Address,
    now: u64,
    freeze_after: u64,
) -> Result<()> {
    let count: u64 = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .try_into()
        .unwrap_or(0);
    for id in 1..=count {
        let claim_id = U256::from(id);
        let claim = client
            .read(vault, IVault::claimAtCall { claimId: claim_id })
            .await?;
        match claim.state {
            ClaimState::Pending | ClaimState::Waiting => {
                send(
                    client,
                    keeper,
                    vault,
                    IVault::settleClaimCall { claimId: claim_id },
                    "serve a claim",
                )
                .await;
                let since = claim.lastAttemptAt.max(claim.bookedAt);
                if now >= since + freeze_after {
                    send(
                        client,
                        keeper,
                        vault,
                        IVault::freezeCall { claimId: claim_id },
                        "freeze",
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Once the vault has halted and its grace is over: stop the station, bring
/// everything home, and close the vault when a valuation shows nothing left.
async fn stop_and_close(client: &Client, keeper: Address, vault: Address, now: u64) -> Result<()> {
    let halted = client.read(vault, IStationVault::haltedAtCall {}).await?;
    if halted == 0 {
        return Ok(());
    }
    let grace = client.read(vault, IStationVault::haltGraceCall {}).await?;
    if now < halted + grace {
        return Ok(());
    }
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    if !client.read(station, IStation::stoppedCall {}).await? {
        send(
            client,
            keeper,
            station,
            IStation::stop_0Call {},
            "stop the station",
        )
        .await;
    }
    let value = client.read(station, IStation::valueCall {}).await?;
    if !value.mark.is_zero() {
        send(
            client,
            keeper,
            station,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
            "unwind the station",
        )
        .await;
    }
    send(client, keeper, vault, IVault::closeCall {}, "close").await;
    Ok(())
}

/// When the author retired the vault, from its `StateChanged` log.
async fn retired_at(client: &Client, vault: Address) -> Option<u64> {
    let filter = alloy::rpc::types::Filter::new()
        .address(vault)
        .event_signature(<IVault::StateChanged as alloy::sol_types::SolEvent>::SIGNATURE_HASH)
        .from_block(0);
    let logs = client.logs(&filter).await.ok()?;
    for log in logs.iter().rev() {
        let decoded = log.log_decode::<IVault::StateChanged>().ok()?;
        if matches!(decoded.inner.data.state, VaultState::WindingDown) {
            let block = log.block_number?;
            return client.block_timestamp_at(block).await.ok();
        }
    }
    None
}

async fn fetch_total(chain_id: u64, executor: &str, vault: Address) -> Option<Signed> {
    let reply = reqwest::Client::new()
        .get(format!("{executor}/usage/{chain_id}/{vault:?}"))
        .send()
        .await
        .ok()?;
    if !reply.status().is_success() {
        return None;
    }
    reply.json().await.ok()
}

/// Send one call as the keeper. A call that can't go through yet is ordinary:
/// somebody else got there first, or it isn't time.
async fn send<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
    what: &str,
) {
    if let Err(error) = client
        .send(keeper, to, call)
        .await
        .context(what.to_string())
    {
        tracing::debug!(%to, error = format!("{error:#}"), "not yet");
    } else {
        tracing::debug!(%to, what, "done");
    }
}

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "keeper" }));
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}
