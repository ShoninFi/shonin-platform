//! Reading the chain on a strategy's behalf.
//!
//! One task per vault follows the logs as blocks arrive and turns the ones
//! that concern this vault into the payloads its streams carry. It is the only
//! thing in the executor that polls: the strategy itself never does.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Filter;
use anyhow::Result;
use chain::Client;
use chain::bindings::vault::{IVault, RequestKind};
use chain::bindings::venue::ILocalVenue;
use strategy_sdk::{AccountEvent, ClaimPending, Side};
use tokio::sync::mpsc::UnboundedSender;

use crate::host::market_name;
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal};

/// What the vault's stream carries, in the shape the bindings generate.
pub use crate::host::shonin::agent::vault::VaultEvent;

/// Follow one vault: what the venue does to its station's account, and what
/// the vault says about itself.
pub async fn run(
    chain: Client,
    vault: Address,
    station: Address,
    venue: Address,
    from_block: u64,
    account: UnboundedSender<Vec<u8>>,
    vault_events: UnboundedSender<VaultEvent>,
) {
    let mut next = from_block;
    // The shortfall last reported, so an agent hears about a new one rather
    // than the same one every block.
    let mut reported = U256::ZERO;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let head = match chain.block_number().await {
            Ok(head) => head,
            Err(error) => {
                tracing::debug!(%error, "the chain did not answer");
                continue;
            }
        };
        if head < next {
            continue;
        }

        match shortfall(&chain, vault).await {
            Ok(short) if short > reported => {
                reported = short;
                let short = u64::try_from(short).unwrap_or(u64::MAX);
                if vault_events.send(VaultEvent::BufferLow(short)).is_err() {
                    return;
                }
            }
            Ok(short) if short.is_zero() => reported = U256::ZERO,
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "could not read the vault's cash"),
        }

        match read(&chain, vault, station, venue, next, head).await {
            Ok((account_events, vault_news)) => {
                for event in account_events {
                    if account.send(strategy_sdk::to_payload(&event)).is_err() {
                        return;
                    }
                }
                for event in vault_news {
                    if vault_events.send(event).is_err() {
                        return;
                    }
                }
                next = head + 1;
            }
            Err(error) => tracing::debug!(%error, "could not read a block"),
        }
    }
}

async fn read(
    chain: &Client,
    vault: Address,
    station: Address,
    venue: Address,
    from: u64,
    to: u64,
) -> Result<(Vec<AccountEvent>, Vec<VaultEvent>)> {
    let mut account = Vec::new();
    let mut news = Vec::new();

    let venue_logs = chain
        .logs(&Filter::new().address(venue).from_block(from).to_block(to))
        .await?;
    for log in venue_logs {
        if let Ok(filled) = log.log_decode::<ILocalVenue::Filled>() {
            let event = filled.inner.data;
            // Someone else's fill on the same venue is none of this
            // strategy's business.
            if event.master != station {
                continue;
            }
            account.push(AccountEvent::Filled {
                market: market_name(event.market),
                side: if event.isBuy { Side::Buy } else { Side::Sell },
                size: to_decimal(event.size, SIZE_DECIMALS)?,
                price: to_decimal(event.price, ASSET_DECIMALS)?,
                notional: to_decimal(event.notional, ASSET_DECIMALS)?,
                reduce_only: event.reduceOnly,
            });
        } else if let Ok(funding) = log.log_decode::<ILocalVenue::FundingAccrued>() {
            let event = funding.inner.data;
            account.push(AccountEvent::Funding {
                market: market_name(event.market),
                paid_per_unit: signed_to_decimal(event.paidPerUnit, ASSET_DECIMALS)?,
            });
        }
    }

    let vault_logs = chain
        .logs(&Filter::new().address(vault).from_block(from).to_block(to))
        .await?;
    for log in vault_logs {
        if let Ok(booked) = log.log_decode::<IVault::ClaimBooked>() {
            // A depositor is leaving and the cash couldn't pay them: their
            // fraction of every position will be closed. The agent is told
            // what fraction, so it can rebalance what is left.
            let supply = chain.read(vault, IVault::totalSupplyCall {}).await?;
            let fraction = if supply.is_zero() {
                U256::ZERO
            } else {
                booked.inner.data.shares * U256::from(10u64.pow(18)) / supply
            };
            news.push(VaultEvent::ClaimPending(strategy_sdk::to_payload(
                &ClaimPending {
                    fraction: to_decimal(fraction, SIZE_DECIMALS)?,
                },
            )));
        } else if let Ok(halted) = log.log_decode::<IVault::Halted>() {
            let reason = chain::names::text(halted.inner.data.reason);
            news.push(VaultEvent::Halting(match reason.as_str() {
                "killed" => "the vault was stopped".to_string(),
                "frozen" => "a claim went unserved and the vault froze".to_string(),
                "retired" => "the author retired the vault and its wind-down is over".to_string(),
                "sealed" => "the vault is being wound down".to_string(),
                other => other.to_string(),
            }));
        }
    }

    Ok((account, news))
}

/// How far the vault's cash falls short of what leavers are waiting for plus
/// the idle floor: what the agent should bring home.
async fn shortfall(chain: &Client, vault: Address) -> Result<U256> {
    let cash = chain.read(vault, IVault::cashCall {}).await?;
    let price = chain.read(vault, IVault::sharePriceCall {}).await?;
    let value = chain.read(vault, IVault::totalValueCall {}).await?;
    let floor = chain.read(vault, IVault::idleFloorCall {}).await?;
    let count = chain.read(vault, IVault::requestCountCall {}).await?;
    let one = U256::from(10u64.pow(18));

    // The newest requests are the ones still waiting; a few dozen is plenty
    // to see a run coming.
    let mut waiting = U256::ZERO;
    let first = count.saturating_sub(U256::from(50u64));
    let mut id = count;
    while id > first {
        let request = chain
            .read(vault, IVault::requestAtCall { requestId: id })
            .await?;
        if !request.done && matches!(request.kind, RequestKind::Redeem) {
            waiting += request.amount * price / one;
        }
        id -= U256::from(1u64);
    }
    let needed = waiting + value * U256::from(floor.bps) / U256::from(10_000u64);
    Ok(needed.saturating_sub(cash))
}
