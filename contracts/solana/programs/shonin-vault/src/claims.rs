//! Withdrawals the cash couldn't pay, as the EVM vault's `Claims`.
//!
//! A claim is served by closing the leaver's fraction of every position and
//! bringing it home, and it is paid in the same transaction: what the close
//! brought home and the leaver's part of the cash as it stood just before,
//! less their own profit fee and the transaction fee. Its shares are burned
//! there and then. A claim the cash already covers needs no close: it becomes
//! an ordinary request, priced at the next valuation.

use anchor_lang::prelude::*;
use shonin_common::time::now;

use crate::contexts::*;
use crate::error::VaultError;
use crate::events::*;
use crate::ledger::*;
use crate::maths::*;
use crate::state::*;
use crate::station;
use crate::valuations::{book_claim, exit_of, pro_rata, queue, sync_holding, Making};

/// Holds `assets` for a claim's owner on the claim.
pub fn hold_on_claim(vault: &mut Vault, claim: &mut Claim, assets: u64) -> Result<()> {
    claim.held = claim.held.checked_add(assets).ok_or(VaultError::Overflow)?;
    vault.held_total = vault
        .held_total
        .checked_add(assets)
        .ok_or(VaultError::Overflow)?;
    Ok(())
}

/// Anyone may try a claim. Trying it again once it is queued is refused, so
/// nobody can make the vault close more than the leaver's share. A close the
/// station refuses (one that can't bring its worth home, or would pay the
/// leaver nothing) fails the whole transaction: it sold nothing, so it was
/// never a try, and the claim waits as it was.
///
/// A try may serve only `part` of the claim's shares, where a venue can't
/// bring the whole of it home in one transaction: that part is closed and
/// paid exactly as a whole claim of it would be, its shares burned, and the
/// rest stays booked with its original turn and deadline. Partial payment
/// cannot postpone the protections for the unpaid remainder.
pub fn settle_claim<'info>(
    ctx: Context<'_, '_, 'info, 'info, SettleClaim<'info>>,
    claim_id: u64,
    groups: Vec<u8>,
    part: Option<u128>,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.pool.vault.key();
    let mut claim = load_claim(&a.claim, &vault_key, claim_id)?.ok_or(VaultError::NotAllowed)?;
    require!(
        claim.state == ClaimState::Pending || claim.state == ClaimState::Waiting,
        VaultError::NotAllowed
    );
    // Said through the program's own instruction data: the close calls every
    // venue first, and what they log counts against the logs' cap.
    let mut said = Said::kept();
    let authority = a.event_authority.to_account_info();
    let bump = ctx.bumps.event_authority;
    a.pool.vault.sync_arrivals(&a.pool.station, &mut said)?;
    let vault = &mut a.pool.vault;
    // A vault ready to close pays its claims from the closed vault.
    require!(
        vault.state != VaultState::Closed && !vault.ready_to_close(),
        VaultError::WrongState
    );
    let shares = match part {
        Some(part) => {
            require!(part > 0 && part < claim.shares, VaultError::NotAllowed);
            part
        }
        None => claim.shares,
    };
    let now = now()?;

    // Cash at home that already covers it: an ordinary request, priced at the
    // next valuation, with nothing to close. The claim makes it, at its own
    // address for the last request it was queued by, so nobody can take the
    // address it is queued at. Only a whole claim is queued.
    if vault.priced && part.is_none() {
        let latest = vault.latest;
        let (_, _, covered) = exit_of(vault, &latest, shares)?;
        if covered <= vault.cash {
            let request = a.request.as_ref().ok_or(VaultError::WrongAccount)?;
            claim.state = ClaimState::Queued;
            claim.last_attempt_at = now;
            vault.queued_claims += 1;
            claim.request = queue(
                vault,
                &vault_key,
                Making {
                    request,
                    maker: a.claim.key(),
                    nonce: claim.request,
                    payer: &a.caller,
                    system_program: &a.pool.system_program,
                },
                claim.owner,
                RequestKind::Claim,
                shares,
                claim_id,
                &mut said,
            )?;
            said.say(ClaimTried {
                claim_id,
                blocked: false,
            });
            store(&a.claim, &claim)?;
            return said.emit(&authority, bump);
        }
    }

    // Cash a priced withdrawal is to be paid from is not for a close's cash
    // share: those are settled first.
    require!(!vault.owed(), VaultError::NotYet);
    // The holding fee up to now falls on every share, the leaver's too.
    sync_holding(vault, &vault_key, now, &mut said)?;
    let supply = vault.total_supply + OFFSET;
    // The leaver's part of what the shares own of the station, and of the
    // cash as it stands before the close.
    let mut fraction = mul_div(shares, WAD - vault.live_units()?, supply)?;
    if fraction > WAD {
        fraction = WAD;
    }
    if fraction == 0 {
        fraction = 1;
    }
    let cash_share = dollars(mul_div(vault.cash as u128, shares, supply)?)?;

    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let vault_info = vault.to_account_info();
    let outcome = station::Station {
        program: &a.close.station_program,
        station: &a.pool.station,
        vault: &vault_info,
        seeds: &seeds,
    }
    .unwind(
        &a.close.station_dollars,
        &a.pool.vault_dollars,
        &a.pool.mint,
        &a.pool.token_program,
        ctx.remaining_accounts,
        groups.clone(),
        fraction as u64,
    )?;
    let vault = &mut a.pool.vault;
    // What the close sent home is the claim's, none of it the unit holders'.
    let home = vault.arrived(&a.pool.station)?;
    if outcome.blocked {
        // A market outside the band is a real try: the clock restarts.
        claim.state = ClaimState::Waiting;
        claim.last_attempt_at = now;
        said.say(ClaimTried {
            claim_id,
            blocked: true,
        });
        store(&a.claim, &claim)?;
        return said.emit(&authority, bump);
    }
    if home == 0 {
        // A claim worth fewer dollar-units at the station than its parts may
        // round away brings nothing home however often it is closed: it is
        // dust, paid its cash share and done, so it can't hold a vault open
        // or freeze it. Judged on what the station holds at the mark now and
        // as the last priced valuation counted it, whichever is more, so a
        // bid pushed for one block can't make a claim look like dust. A
        // close that went through and brought nothing home for a claim worth
        // more must not stand.
        let mark_now = station::value(
            &a.close.station_program,
            &a.pool.station,
            ctx.remaining_accounts,
            groups,
        )?
        .mark as u128;
        let mut stations = mark_now;
        if vault.priced {
            let counted = vault.latest.gross - 1 - vault.latest.cash as u128;
            stations = stations.max(counted);
        }
        require!(
            mul_div(stations, fraction, WAD)? <= vault.dust(),
            VaultError::NotYet
        );
    } else if vault.units != 0 && fraction < WAD {
        // The close came from the shares' part alone: the unit holders own the
        // same as before, now a larger fraction of what is left.
        vault.unit_scale = mul_div(vault.scale(), WAD, WAD - fraction)?;
    }
    vault.cash = vault.cash.checked_add(home).ok_or(VaultError::Overflow)?;
    said.say(FromStation {
        assets: home,
        to_units: 0,
    });
    let owned = cash_share.checked_add(home).ok_or(VaultError::Overflow)?;
    pay_claim(a, &mut claim, claim_id, shares, owned, &seeds, &mut said)?;
    if claim.shares != 0 {
        // Keep the original booking: even many tiny payments cannot
        // postpone the unpaid remainder's freeze or fallback.
        claim.state = ClaimState::Pending;
        claim.last_attempt_at = now;
    }
    said.say(ClaimTried {
        claim_id,
        blocked: false,
    });
    store(&a.claim, &claim)?;
    said.emit(&authority, bump)
}

/// Pays a claim what its shares own, less its own profit fee, paid in cash to
/// the author and the platform, and the transaction fee, which stays in the
/// pool. The profit fee is measured against the mark as it is now, which is
/// what the stayers' fee is measured against too.
fn pay_claim<'info>(
    a: &mut SettleClaim<'info>,
    claim: &mut Claim,
    claim_id: u64,
    shares: u128,
    owned: u64,
    seeds: &[&[u8]],
    said: &mut Said,
) -> Result<()> {
    let vault_key = a.pool.vault.key();
    let purse = Purse {
        vault: a.pool.vault.to_account_info(),
        dollars: a.pool.vault_dollars.to_account_info(),
        mint: a.pool.mint.to_account_info(),
        token_program: a.pool.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds,
    };
    let vault = &mut a.pool.vault;
    let owned = owned.min(vault.cash);
    let fee = dollars(fee_of(vault.mark, &vault.fees, owned as u128, shares)?)?;
    let gross = owned - fee;
    let kept = dollars(mul_div_up(
        gross as u128,
        vault.fees.transaction_bps as u128,
        BPS,
    )?)?;
    let payout = gross - kept;

    vault.pending_shares -= shares;
    vault.burn_held(shares, &vault_key, said)?;
    vault.cash -= owned - kept;
    claim.shares -= shares;
    if claim.shares == 0 {
        claim.state = ClaimState::Paid;
    }
    if fee != 0 {
        let total = vault.fees.author_bps as u128 + vault.fees.platform_bps as u128;
        let to_author = dollars(mul_div(fee as u128, vault.fees.author_bps as u128, total)?)?;
        let (author, platform) = (vault.author, vault.platform);
        if !purse.pay(&a.author_dollars, &author, to_author, said)? {
            vault.held_author += to_author;
            vault.held_total += to_author;
        }
        if !purse.pay(&a.platform_dollars, &platform, fee - to_author, said)? {
            vault.held_platform += fee - to_author;
            vault.held_total += fee - to_author;
        }
    }
    claim.paid += payout;
    if !purse.pay(&a.owner_dollars, &claim.owner, payout, said)? {
        hold_on_claim(vault, claim, payout)?;
    }
    said.say(Withdrawn {
        request_id: 0,
        owner: claim.owner,
        shares,
        assets: payout,
        fee_shares: 0,
    });
    said.say(ClaimPaid {
        claim_id,
        assets: payout,
    });
    Ok(())
}

/// A withdrawal nothing has priced for `fallback_after` (a market outside the
/// band voids every valuation, say) may become a claim instead of taking the
/// fallback: served by a close, and counted toward a freeze from when it was
/// due to be priced.
pub fn claim_instead<'info>(
    ctx: Context<'_, '_, '_, 'info, ClaimInstead<'info>>,
    request_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let mut r = load_request(&a.request, &vault_key, request_id)?.ok_or(VaultError::NotAllowed)?;
    require!(
        r.kind == RequestKind::Redeem && !r.done && a.owner.key() == r.owner,
        VaultError::NotAllowed
    );
    let vault = &mut a.vault;
    require!(
        vault.state != VaultState::Closed && !vault.ready_to_close(),
        VaultError::WrongState
    );
    require!(
        now()? >= r.earliest.saturating_add(vault.timers.fallback_after) && !vault.owed(),
        VaultError::NotYet
    );
    require!(
        !(vault.priced && vault.latest.time >= r.earliest),
        VaultError::NotAllowed
    );
    r.done = true;
    // The shares stay pending, held by the vault, as a claim's are.
    let booked_at = r.earliest;
    book_claim(
        vault,
        &vault_key,
        &a.request,
        &a.claim,
        &a.system_program,
        &r,
        booked_at,
        &mut Said::logs(),
    )?;
    Ok(())
}

/// A claim nothing could serve for `fallback_after` from its booking takes
/// the pro-rata fallback: its share of the cash now, and units for the rest.
pub fn take_pro_rata_claim<'info>(
    ctx: Context<'_, '_, '_, 'info, TakeProRataClaim<'info>>,
    claim_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.pool.vault.key();
    let now = now()?;
    let mut claim = load_claim(&a.claim, &vault_key, claim_id)?.ok_or(VaultError::NotAllowed)?;
    require_keys_eq!(a.owner.key(), claim.owner, VaultError::NotAllowed);
    a.pool
        .vault
        .sync_arrivals(&a.pool.station, &mut Said::logs())?;
    let vault = &mut a.pool.vault;
    // A vault that is closing or closed pays its claims from the closed
    // vault, fees stopped.
    require!(
        vault.state != VaultState::Closed && !vault.ready_to_close(),
        VaultError::WrongState
    );
    let state = claim.state;
    require!(
        matches!(
            state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ),
        VaultError::NotAllowed
    );
    // A day from its booking, whether or not valuations kept being priced.
    require!(
        now >= claim.booked_at.saturating_add(vault.timers.fallback_after),
        VaultError::NotYet
    );
    // Not while a priced valuation waits to settle, and not for a claim the
    // cash covers: that one can be served now, at a price.
    require!(!vault.owed(), VaultError::NotYet);
    if state != ClaimState::Queued && vault.priced {
        let latest = vault.latest;
        let (_, _, covered) = exit_of(vault, &latest, claim.shares)?;
        require!(covered > vault.cash, VaultError::NotAllowed);
    }
    if state == ClaimState::Queued {
        let request = a.request.as_ref().ok_or(VaultError::WrongAccount)?;
        let mut r =
            load_request(request, &vault_key, claim.request)?.ok_or(VaultError::WrongAccount)?;
        // Not while a priced valuation could settle it.
        require!(
            !(vault.priced && vault.latest.time >= r.earliest),
            VaultError::NotAllowed
        );
        // Queued, it is a request like any other, and falls back as one.
        require!(
            now >= r.earliest.saturating_add(vault.timers.fallback_after),
            VaultError::NotYet
        );
        vault.queued_claims -= 1;
        r.done = true;
        store(request, &r)?;
    }
    claim.state = ClaimState::Fallback;
    vault.pending_shares -= claim.shares;
    let owner = claim.owner;
    let author = &mut a.pool.author_holder;
    let platform = &mut a.pool.platform_holder;
    let subject = subject_of(&a.holder, &vault_key, &owner, author, platform)?;
    let mut holder = subject.copy(&vault_key, &owner, author, platform);
    let assets = pro_rata(
        vault,
        &vault_key,
        &mut holder,
        author,
        platform,
        claim.shares,
        now,
        &mut Said::logs(),
    )?;
    let payer = Some((a.owner.as_ref(), a.system_program.as_ref()));
    subject.put_back(&holder, &a.holder, author, platform, payer)?;
    let vault = &mut a.pool.vault;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let paid = Purse {
        vault: vault.to_account_info(),
        dollars: a.pool.vault_dollars.to_account_info(),
        mint: a.pool.mint.to_account_info(),
        token_program: a.pool.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    }
    .pay(&a.to_dollars, &claim.owner, assets, &mut Said::logs())?;
    if !paid {
        hold_on_claim(vault, &mut claim, assets)?;
    }
    store(&a.claim, &claim)
}
