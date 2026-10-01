//! Reading the chain on a strategy's behalf.
//!
//! One task per vault follows the logs as blocks arrive and turns the ones
//! that concern this vault into the payloads its streams carry. It is the only
//! thing in the executor that polls: the strategy itself never does.

use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use anyhow::Result;
use chain::Client;
use chain::bindings::vault::IVault;
use chain::bindings::venue::ILocalVenue;
use strategy_sdk::{AccountEvent, ClaimPending, Side};
use tokio::sync::mpsc::UnboundedSender;

use crate::host::market_name;
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal};

/// What the vault's stream carries, in the shape the bindings generate.
pub use crate::host::shonin::agent::vault::VaultEvent;

/// Follow one vault: what the venue does to its account, and what the vault
/// says about itself.
pub async fn run(
    chain: Client,
    vault: Address,
    venue: Address,
    from_block: u64,
    account: UnboundedSender<Vec<u8>>,
    vault_events: UnboundedSender<VaultEvent>,
) {
    let mut next = from_block;
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

        match read(&chain, vault, venue, next, head).await {
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
            if event.master != vault {
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
            // A depositor is leaving; the contract is closing their fraction
            // of every position. The strategy is told what fraction, so it can
            // rebalance what is left rather than be surprised by it.
            let fraction = to_decimal(booked.inner.data.fractionClosed, SIZE_DECIMALS)?;
            news.push(VaultEvent::ClaimPending(strategy_sdk::to_payload(
                &ClaimPending { fraction },
            )));
        } else if log.log_decode::<IVault::Killed>().is_ok() {
            news.push(VaultEvent::Halting("the vault was stopped".to_string()));
        } else if log.log_decode::<IVault::Frozen>().is_ok() {
            news.push(VaultEvent::Halting(
                "a claim went unserved and the vault froze".to_string(),
            ));
        }
    }

    Ok((account, news))
}
