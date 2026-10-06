//! Reading the chain on a strategy's behalf.
//!
//! One task per vault follows the logs as blocks arrive and turns the ones
//! that concern this vault into the payloads its streams carry. It is the only
//! thing in the executor that polls: the strategy itself never does.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Filter;
use anyhow::Result;
use chain::Client;
use chain::bindings::vault::{ClaimState, IVault, RequestKind};
use chain::bindings::venue::ILocalVenue;
use strategy_sdk::{AccountEvent, Side};
use tokio::sync::mpsc::UnboundedSender;

use crate::host::market_name;
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal};

/// What the vault's stream carries, in the shape the bindings generate.
pub use crate::host::shonin::agent::vault::{Claim, VaultEvent};

/// Follow one vault: what the venue does to its station's account, and what
/// the vault says about itself. It reads on from the block `next` holds and
/// keeps it at the block after the last it has passed on, so a watcher
/// started after this one carries on where it stopped.
pub async fn run(
    chain: Client,
    vault: Address,
    station: Address,
    venue: Address,
    next_block: Arc<AtomicU64>,
    account: UnboundedSender<Vec<u8>>,
    vault_events: UnboundedSender<VaultEvent>,
) {
    let mut next = next_block.load(Ordering::SeqCst);
    // The shortfall last reported, so an agent hears about a new one rather
    // than the same one every block.
    let mut reported = U256::ZERO;
    // Whether the last look failed, so a failure is said once at `warn` when
    // it starts rather than at every block.
    let mut chain_down = false;
    let mut cash_unread = false;
    // Blocks that can't be read are tried again every look, and said about
    // once a minute.
    let mut unread = crate::Throttle::default();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let head = match chain.block_number().await {
            Ok(head) => {
                chain_down = false;
                head
            }
            Err(error) => {
                if !chain_down {
                    tracing::warn!(%vault, error = format!("{error:#}"), "the chain did not answer; the agent hears nothing new until it does");
                }
                chain_down = true;
                continue;
            }
        };
        if head < next {
            continue;
        }

        let short = shortfall(&chain, vault).await;
        if let Err(error) = &short
            && !cash_unread
        {
            tracing::warn!(%vault, error = format!("{error:#}"), "could not read the vault's cash; the agent is not told of a shortfall until it can");
        }
        cash_unread = short.is_err();
        match short {
            Ok(short) if short > reported => {
                reported = short;
                let short = u64::try_from(short).unwrap_or(u64::MAX);
                if vault_events.send(VaultEvent::BufferLow(short)).is_err() {
                    return;
                }
            }
            Ok(short) if short.is_zero() => reported = U256::ZERO,
            Ok(_) | Err(_) => {}
        }

        // A node limits how many blocks one log query may span, so a gap
        // that has grown (a slow node, a long pause) is read a chunk at a
        // time, and each chunk read is never read again.
        while let Some((from, to)) = chunk(next, head) {
            match read(&chain, vault, station, venue, from, to).await {
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
                    next = to + 1;
                    next_block.store(next, Ordering::SeqCst);
                }
                Err(error) => {
                    if unread.due(std::time::Instant::now()) {
                        tracing::warn!(%vault, from, to, error = format!("{error:#}"), "could not read these blocks; trying again");
                    }
                    break;
                }
            }
        }
    }
}

/// The most blocks one log query spans: within what public nodes allow.
const MOST_BLOCKS: u64 = 2_000;

/// The next blocks to read, from `next` up to `head` but never more than
/// [`MOST_BLOCKS`] of them, or nothing once `next` has passed the head.
fn chunk(next: u64, head: u64) -> Option<(u64, u64)> {
    (next <= head).then(|| (next, head.min(next.saturating_add(MOST_BLOCKS - 1))))
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
            // which claim and what fraction, so it can rebalance what is
            // left.
            let supply = chain.read(vault, IVault::totalSupplyCall {}).await?;
            let booked = booked.inner.data;
            news.push(VaultEvent::ClaimPending(claim(
                booked.claimId,
                booked.shares,
                supply,
            )));
        } else if let Ok(halted) = log.log_decode::<IVault::Halted>() {
            news.push(halting(halted.inner.data.reason));
        }
    }

    Ok((account, news))
}

/// A claim's fraction of the whole vault is in parts of this.
pub const WHOLE: u64 = 1_000_000_000_000_000_000;

/// The claim an agent is told of: its id, and the fraction of the vault its
/// shares are, in parts per 10^18 (never more than the whole).
pub fn claim(id: U256, shares: U256, supply: U256) -> Claim {
    let whole = U256::from(WHOLE);
    let fraction = if supply.is_zero() {
        U256::ZERO
    } else {
        (shares.saturating_mul(whole) / supply).min(whole)
    };
    Claim {
        id: u64::try_from(id).unwrap_or(u64::MAX),
        fraction: u64::try_from(fraction).unwrap_or(WHOLE),
    }
}

/// What an agent is told when its vault halts, for the reason the vault gave.
fn halting(reason: alloy::primitives::B256) -> VaultEvent {
    let reason = chain::names::text(reason);
    VaultEvent::Halting(match reason.as_str() {
        "killed" => "the vault was stopped".to_string(),
        "frozen" => "a claim went unserved and the vault froze".to_string(),
        "retired" => "the author retired the vault and its wind-down is over".to_string(),
        "sealed" => "the vault is being wound down".to_string(),
        other => other.to_string(),
    })
}

/// The halting notice owed to an agent that starts on a vault that has
/// already halted, or nothing if it hasn't. A watcher reads on from where
/// the last one stopped, or from the head, so it never passes the halt
/// again: the agent would otherwise run out its grace without being told.
/// The reason comes from the vault's `Halted` log, looked for back through
/// as many blocks as the chain has made since the halt, newest first, a
/// query's span at a time (a fast chain makes thousands in a grace); a halt
/// whose log isn't found there is still a halt, said without its reason.
pub async fn halting_notice(chain: &Client, vault: Address) -> Result<Option<VaultEvent>> {
    let halted = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltedAtCall {},
        )
        .await?;
    if halted == 0 {
        return Ok(None);
    }
    let head = chain.block_number().await?;
    let back = match since_the_halt(chain, head, halted).await {
        Ok(back) => back,
        Err(_) => MOST_BLOCKS,
    };
    let oldest = head.saturating_sub(back.saturating_sub(1));
    let filter = Filter::new()
        .address(vault)
        .event_signature(<IVault::Halted as alloy::sol_types::SolEvent>::SIGNATURE_HASH);
    let mut to = head;
    let reason = loop {
        let from = to.saturating_sub(MOST_BLOCKS - 1).max(oldest);
        let logs = chain
            .logs(&filter.clone().from_block(from).to_block(to))
            .await
            .unwrap_or_default();
        let found = logs
            .iter()
            .rev()
            .find_map(|log| log.log_decode::<IVault::Halted>().ok())
            .map(|halted| halted.inner.data.reason);
        if found.is_some() || from <= oldest {
            break found;
        }
        to = from - 1;
    };
    Ok(Some(match reason {
        Some(reason) => halting(reason),
        None => VaultEvent::Halting("the vault has halted".to_string()),
    }))
}

/// The most blocks looked back through for a halt's log: a hundred queries.
const MOST_HALT_BLOCKS: u64 = 100 * MOST_BLOCKS;

/// The blocks a block interval is measured over.
const SAMPLED_BLOCKS: u64 = 100;

/// How many blocks back a halt at `halted` may be, from the chain's block
/// interval measured over its latest blocks.
async fn since_the_halt(chain: &Client, head: u64, halted: u64) -> Result<u64> {
    let sampled = head.min(SAMPLED_BLOCKS);
    let now = chain.block_timestamp_at(head).await?;
    let then = chain.block_timestamp_at(head - sampled).await?;
    Ok(blocks_back(
        now.saturating_sub(halted),
        sampled,
        now.saturating_sub(then),
    ))
}

/// How many blocks a chain that made `sampled_blocks` in `sampled_secs` has
/// made in `elapsed` seconds, with a query's span to spare, and never more
/// than [`MOST_HALT_BLOCKS`]. Blocks too fast to tell apart by their whole
/// seconds are taken as the most.
fn blocks_back(elapsed: u64, sampled_blocks: u64, sampled_secs: u64) -> u64 {
    if elapsed == 0 {
        return MOST_BLOCKS;
    }
    if sampled_secs == 0 {
        return MOST_HALT_BLOCKS;
    }
    elapsed
        .saturating_mul(sampled_blocks)
        .div_ceil(sampled_secs)
        .saturating_add(MOST_BLOCKS)
        .min(MOST_HALT_BLOCKS)
}

/// How far the vault's cash falls short of what leavers are waiting for
/// (withdrawals not yet priced and claims not yet paid) plus the idle floor:
/// what the agent should bring home.
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
    // And the claims not yet paid: each is paid from what comes home for it,
    // or from cash the next valuation finds. A claim whose close is staged
    // (closing) is paid from that close's own delivery and its part of the
    // cash already set aside, so it needs nothing brought home.
    let claims = chain.read(vault, IVault::claimCountCall {}).await?;
    let first = claims.saturating_sub(U256::from(50u64));
    let mut id = claims;
    while id > first {
        let claim = chain
            .read(vault, IVault::claimAtCall { claimId: id })
            .await?;
        if matches!(
            claim.state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ) {
            waiting += claim.shares * price / one;
        }
        id -= U256::from(1u64);
    }
    let needed = waiting + value * U256::from(floor.bps) / U256::from(10_000u64);
    Ok(needed.saturating_sub(cash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_carries_its_id_and_its_fraction_of_the_vault() {
        let pending = claim(U256::from(7), U256::from(25), U256::from(100));
        assert_eq!(pending.id, 7);
        assert_eq!(pending.fraction, WHOLE / 4);
        // Never more than the whole vault, and nothing of an empty one.
        assert_eq!(
            claim(U256::from(1), U256::from(300), U256::from(100)).fraction,
            WHOLE
        );
        assert_eq!(claim(U256::from(1), U256::from(3), U256::ZERO).fraction, 0);
        assert_eq!(claim(U256::MAX, U256::ZERO, U256::from(1)).id, u64::MAX);
    }

    use crate::stand_in;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_watcher_carries_on_from_the_block_after_the_last_one_read() {
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().head = 20;
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let next = Arc::new(AtomicU64::new(12));
        let (account, _account_rx) = tokio::sync::mpsc::unbounded_channel();
        let (vault_events, _vault_rx) = tokio::sync::mpsc::unbounded_channel();
        let address = Address::repeat_byte(0x11);
        let watching = tokio::spawn(run(
            client,
            address,
            address,
            address,
            next.clone(),
            account,
            vault_events,
        ));
        for _ in 0..100 {
            if next.load(Ordering::SeqCst) == 21 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        watching.abort();
        assert_eq!(next.load(Ordering::SeqCst), 21, "it read up to the head");
        let reads = state.lock().unwrap().log_reads.clone();
        assert_eq!(
            reads.first(),
            Some(&(12, 20)),
            "it started where the last watcher stopped"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_an_agent_never_read_reaches_the_agent_started_after_it() {
        // The vault halted at block 15, which the agent is owed well before
        // its positions are closed.
        let vault = Address::repeat_byte(0x11);
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().head = 20;
        state.lock().unwrap().halted = Some((vault, 15));
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let watched_to = Arc::new(AtomicU64::new(10));
        let account = crate::feed::Feed::<Vec<u8>>::default();
        let vault_events = crate::feed::Feed::<VaultEvent>::default();
        let watcher = || {
            tokio::spawn(run(
                client.clone(),
                vault,
                Address::repeat_byte(0x22),
                Address::repeat_byte(0x33),
                watched_to.clone(),
                account.sender(),
                vault_events.sender(),
            ))
        };

        // The first agent has its stream, and stops before reading it, once
        // its watcher has read past the halt.
        let first = watcher();
        let unread = vault_events.reader();
        for _ in 0..100 {
            if watched_to.load(Ordering::SeqCst) > 15 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            watched_to.load(Ordering::SeqCst) > 15,
            "the first watcher read past block 15"
        );
        drop(unread);
        first.abort();

        // The next agent and its watcher, which carries on after block 15.
        let second = watcher();
        let reader = vault_events.reader();
        let heard = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            std::future::poll_fn(|cx| reader.lock().unwrap().poll_recv(cx)),
        )
        .await;
        second.abort();
        assert!(
            matches!(heard, Ok(Some(VaultEvent::Halting(_)))),
            "the agent started next never hears that the vault is halting"
        );
    }

    #[test]
    fn a_halt_is_looked_for_as_far_back_as_the_chain_has_made_blocks_since() {
        // Ten minutes into the grace on a chain of quarter-second blocks:
        // 2,400 blocks, more than one query spans.
        assert_eq!(blocks_back(600, 100, 25), 2_400 + MOST_BLOCKS);
        // On a chain of twelve-second blocks, one query's span is plenty.
        assert_eq!(blocks_back(600, 100, 1_200), 50 + MOST_BLOCKS);
        // A long grace on a fast chain is capped.
        assert_eq!(blocks_back(30 * 86_400, 100, 25), MOST_HALT_BLOCKS);
        // Blocks too fast to tell apart: the most.
        assert_eq!(blocks_back(600, 100, 0), MOST_HALT_BLOCKS);
    }

    #[tokio::test]
    async fn a_halt_older_than_one_query_spans_is_still_found_with_its_reason() {
        let vault = Address::repeat_byte(0x11);
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().head = 10_000;
        state.lock().unwrap().halted = Some((vault, 3_000));
        // Blocks too fast to tell apart, and a halt a minute ago.
        state.lock().unwrap().halted_secs_ago = 60;
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;

        let notice = halting_notice(&client, vault).await.unwrap();
        assert!(
            matches!(&notice, Some(VaultEvent::Halting(why)) if why == "the vault was stopped"),
            "the halt's reason was not found: {notice:?}"
        );
        let reads = state.lock().unwrap().log_reads.clone();
        assert_eq!(
            reads.first(),
            Some(&(8_001, 10_000)),
            "the newest blocks are read first"
        );
    }

    #[test]
    fn a_wide_gap_is_read_in_chunks_of_at_most_two_thousand_blocks() {
        let mut next = 100;
        let head = 4_500;
        let mut chunks = Vec::new();
        while let Some((from, to)) = chunk(next, head) {
            chunks.push((from, to));
            next = to + 1;
        }
        assert_eq!(chunks, [(100, 2_099), (2_100, 4_099), (4_100, 4_500)]);
        assert_eq!(chunk(7, 7), Some((7, 7)), "one new block is one chunk");
        assert_eq!(chunk(8, 7), None, "nothing new, nothing to read");
    }
}
