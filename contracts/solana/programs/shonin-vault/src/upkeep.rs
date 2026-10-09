//! Everything about a vault that isn't pricing: who may deposit, the tank,
//! freezes, its lifecycle, the closed vault's doors, held payouts, and its
//! idle floor. The counterpart of the EVM vault's `Upkeep`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions as sysvar_instructions;
use shonin_common::{account::fit, digest, ed25519, time::now};
use shonin_registry::read;
use shonin_registry::state::ImageKind;

use crate::codes;
use crate::contexts::*;
use crate::error::VaultError;
use crate::events::*;
use crate::ledger::*;
use crate::maths::*;
use crate::state::*;
use crate::valuations::{hold_on, payday, queue, sync_holding, Making};

// Who may deposit ---------------------------------------------------------------------

/// Records that `holder`'s owner states they are not a restricted person for
/// the list whose hash they named, which must be the vault's list as it is.
pub fn state(vault: &Vault, holder: &mut Holder, statement: [u8; 32]) -> Result<()> {
    require!(statement == vault.restricted_hash, VaultError::NoStatement);
    holder.statement = statement;
    emit!(StatementRecorded {
        depositor: holder.owner,
        restricted_hash: statement,
    });
    Ok(())
}

/// A depositor's statement, made by signing this instruction, which names the
/// hash of the vault's restricted list.
pub fn record_statement(ctx: Context<RecordStatement>, statement: [u8; 32]) -> Result<()> {
    let a = ctx.accounts;
    let holder = &mut a.holder;
    if holder.owner == Pubkey::default() {
        holder.vault = a.vault.key();
        holder.owner = a.owner.key();
        holder.payer = a.owner.key();
        holder.bump = ctx.bumps.holder;
    }
    state(&a.vault, holder, statement)
}

/// Reads the restricted lists of the drivers named (by their place in the
/// vault) again, from the implementation the registry approves now: a
/// venue's terms can exclude more people after a vault was created, and its
/// pinned driver can't learn that. The list only ever grows, and a depositor
/// states again for the longer one. Only a list is read, never anything
/// signed. A driver that fails fails the call, which can't be caught on
/// Solana, so whoever refreshes leaves it out; one that answers with more
/// codes than a refresh takes is skipped. The remaining accounts are, per
/// driver named, its approval slot, the revocation record of the
/// implementation that slot approves now, and that implementation's program.
pub fn refresh_restricted<'info>(
    ctx: Context<'_, '_, '_, 'info, RefreshRestricted<'info>>,
    drivers: Vec<u8>,
) -> Result<()> {
    let a = ctx.accounts;
    let vault = &mut a.vault;
    require_keys_eq!(*a.restricted.owner, crate::ID, VaultError::WrongAccount);
    let mut list = {
        let data = a.restricted.try_borrow_data()?;
        Restricted::try_deserialize(&mut &data[..])?
    };
    require!(
        ctx.remaining_accounts.len() == drivers.len() * 3,
        VaultError::WrongAccount
    );
    let now = now()?;
    let mut added = false;
    for (n, index) in drivers.iter().enumerate() {
        require!(
            (*index as usize) < vault.drivers.len() && !drivers[n + 1..].contains(index),
            VaultError::WrongAccount
        );
        let name = vault.drivers[*index as usize];
        let group = &ctx.remaining_accounts[n * 3..n * 3 + 3];
        let implementation = read::trading(
            shonin_registry::state::ApprovalKind::Driver,
            &name,
            &group[0],
            &group[1],
            now,
        )?;
        if implementation == Pubkey::default() {
            continue;
        }
        require_keys_eq!(*group[2].key, implementation, VaultError::WrongAccount);
        let codes = crate::station::restricted_of(&group[2])?;
        if codes.len() > MAX_REFRESHED {
            continue;
        }
        for code in codes {
            if codes::insert(&mut list.codes, code) {
                added = true;
                emit!(RestrictedAdded { code });
            }
        }
    }
    if added {
        require!(list.codes.len() <= MAX_LIST, VaultError::ListFull);
        // Whoever brings the list up to date pays for the room it needs.
        fit(
            &a.restricted,
            &a.payer,
            &a.system_program,
            Restricted::space(list.codes.len()),
        )?;
        store(&a.restricted, &list)?;
        vault.restricted_hash = codes::hash(&list.codes);
    }
    Ok(())
}

// Keys --------------------------------------------------------------------------------

/// Requires `key` to be blessed now as this vault's executor. `records` are
/// the registry's config, the key's blessing for this vault, its key record,
/// and its image's and project's records.
pub fn require_executor(
    vault: &Account<Vault>,
    key: &Pubkey,
    records: [&AccountInfo; 5],
) -> Result<()> {
    let [config, blessing, key_info, image, project] = records;
    let blessed = read::blessed_for(
        config,
        blessing,
        key_info,
        image,
        project,
        key,
        &vault.cluster,
        &vault.key().to_bytes(),
        &vault.bundle_hash,
        now()?,
    )?;
    require!(
        blessed
            && read::key_info(key_info, key)?.is_some_and(|info| info.kind == ImageKind::Executor),
        VaultError::NotBlessed
    );
    Ok(())
}

// The tank ----------------------------------------------------------------------------

/// A running usage total the vault's executor signed, carried in the
/// transaction by an Ed25519 instruction over [`usage_digest`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub total: u64,
    pub as_of: u64,
    pub signer: Pubkey,
}

/// What the executor signs: keccak256 of the Borsh encoding of the purpose
/// (`keccak256("shonin-usage")`), the cluster value, this program, the vault,
/// the total and when it was signed. Bound to the cluster, the program, the
/// vault and the purpose, as the EVM digest binds chain, contract and purpose.
pub fn usage_digest(cluster: &[u8; 32], vault: &Pubkey, total: u64, as_of: u64) -> [u8; 32] {
    #[derive(AnchorSerialize)]
    struct UsageMessage {
        purpose: [u8; 32],
        cluster: [u8; 32],
        program: Pubkey,
        vault: Pubkey,
        total: u64,
        as_of: u64,
    }
    let message = UsageMessage {
        purpose: digest::purpose("shonin-usage"),
        cluster: *cluster,
        program: crate::ID,
        vault: *vault,
        total,
        as_of,
    };
    digest::keccak(&[&message.try_to_vec().unwrap_or_default()])
}

pub fn fund_tank(ctx: Context<FundTank>, assets: u64) -> Result<()> {
    let a = ctx.accounts;
    anchor_spl::token::transfer_checked(
        CpiContext::new(
            a.token_program.to_account_info(),
            anchor_spl::token::TransferChecked {
                from: a.from.to_account_info(),
                mint: a.mint.to_account_info(),
                to: a.vault_dollars.to_account_info(),
                authority: a.funder.to_account_info(),
            },
        ),
        assets,
        DECIMALS,
    )?;
    a.vault.tank.balance = a
        .vault
        .tank
        .balance
        .checked_add(assets)
        .ok_or(VaultError::Overflow)?;
    emit!(TankFunded {
        from: a.funder.key(),
        assets,
    });
    Ok(())
}

/// Collects the bill (when a signed total is given), then, if the tank is
/// below what the author asked it kept at, queues a fill from their shares.
/// Anyone can send it, every day; whoever queues a fill pays its request's
/// rent and is refunded it when the request is closed. The fill is made at
/// the vault's own address for the last fill it queued, which only this can
/// move on, so no other request can take its place.
pub fn keep<'info>(
    ctx: Context<'_, '_, '_, 'info, Keep<'info>>,
    usage: Option<Usage>,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    if let Some(usage) = usage {
        let seeds = a.vault.seeds();
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        let purse = Purse {
            vault: a.vault.to_account_info(),
            dollars: a.vault_dollars.to_account_info(),
            mint: a.mint.to_account_info(),
            token_program: a.token_program.to_account_info(),
            decimals: DECIMALS,
            seeds: &seeds,
        };
        collect(&mut a.vault, &a.billing, &purse, usage)?;
    }
    let vault = &mut a.vault;
    let state = vault.state;
    if vault.tank.keep_at > vault.tank.balance
        && !vault.tank.fill_pending
        && state != VaultState::Closed
        && !vault.ready_to_close()
    {
        let request = a.request.as_ref().ok_or(VaultError::WrongAccount)?;
        vault.tank.fill_pending = true;
        let gap = vault.tank.keep_at - vault.tank.balance;
        let author = vault.author;
        let nonce = vault.tank.fill_request;
        vault.tank.fill_request = queue(
            vault,
            &vault_key,
            Making {
                request,
                maker: vault_key,
                nonce,
                payer: &a.caller,
                system_program: &a.system_program,
            },
            author,
            RequestKind::TankFill,
            gap as u128,
            1,
            &mut Said::logs(),
        )?;
    }
    Ok(())
}

/// Charges the running total signed by the vault's executor, believing it
/// only up to the author's allowances at published prices for the time it
/// covers. What isn't charged is written off: the tank never owes. What a
/// collection leaves of the cap is kept for the next, up to a day's cost at
/// the allowances in force: a burst inside a day, or gas landing after a
/// halt, is charged whenever anyone collects, while allowance left unused for
/// longer can't be saved up, so a wrong total takes at most about a day
/// beyond what the time since the last collection allowed.
fn collect<'info>(
    vault: &mut Account<'info, Vault>,
    billing: &Billing<'info>,
    purse: &Purse<'_, 'info>,
    usage: Usage,
) -> Result<()> {
    require_keys_eq!(
        *billing.instructions.key,
        sysvar_instructions::ID,
        VaultError::WrongAccount
    );
    let digest = usage_digest(&vault.cluster, &vault.key(), usage.total, usage.as_of);
    require!(
        ed25519::signed_in_transaction(&billing.instructions, &usage.signer, &digest, None)?,
        VaultError::NoSignature
    );
    require_executor(
        vault,
        &usage.signer,
        [
            &billing.registry_config,
            &billing.blessing,
            &billing.key_info,
            &billing.image,
            &billing.project,
        ],
    )?;
    let now = now()?;
    // Each total is signed later than the last, so an old statement can't be
    // replayed to reset the cap that is carried forward.
    require!(
        usage.total >= vault.tank.usage_settled
            && usage.as_of > vault.tank.usage_as_of
            && usage.as_of <= now,
        VaultError::Stale
    );
    let start = vault.tank.last_collection;
    let end = cap_end(vault, usage.as_of).max(start);
    let cap = vault
        .tank
        .cap_carried
        .checked_add(cost(vault, &billing.prices, start, end)?)
        .ok_or(VaultError::Overflow)?;
    let billed = usage.total - vault.tank.usage_settled;
    let charge = (billed as u128).min(cap).min(vault.tank.balance as u128) as u64;
    vault.tank.usage_settled = usage.total;
    vault.tank.usage_as_of = usage.as_of;
    vault.tank.last_collection = end;
    let day = daily_cost(vault, &billing.prices, now)?;
    vault.tank.cap_carried = (cap - charge as u128).min(day);
    vault.tank.balance -= charge;
    if charge != 0 {
        let platform = vault.platform;
        require!(
            purse.takes(&billing.platform_dollars, &platform)?,
            VaultError::WrongAccount
        );
        purse.send(charge, &billing.platform_dollars)?;
    }
    emit!(TankCharged {
        billed,
        charged: charge,
        total: usage.total,
    });
    Ok(())
}

/// The cap stops when the strategy does: at the halt, plus the grace it is
/// given to wind down.
fn cap_end(vault: &Vault, at: u64) -> u64 {
    if vault.halted_at == 0 {
        return at;
    }
    let stop = vault.halted_at.saturating_add(vault.timers.halt_grace);
    at.min(stop)
}

/// What the allowances in force cost from `start`, or from when they were
/// set if later, to `end`.
fn cost(vault: &Vault, prices: &AccountInfo, start: u64, end: u64) -> Result<u128> {
    let from = vault.allowances_since.max(start);
    if end <= from {
        return Ok(0);
    }
    let data = prices.try_borrow_data()?;
    let history = shonin_prices::read::History::new(prices.key, prices.owner, &data)?;
    let cost = history.cost_of(&vault.allowances.into(), from, end)?;
    if cost > shonin_common::math::U256::from(u128::MAX) {
        return err!(VaultError::Overflow);
    }
    Ok(cost.as_u128())
}

/// A day at the allowances in force, at the prices in force at `now`.
fn daily_cost(vault: &Vault, prices: &AccountInfo, now: u64) -> Result<u128> {
    let data = prices.try_borrow_data()?;
    let history = shonin_prices::read::History::new(prices.key, prices.owner, &data)?;
    let day = history.daily_cost(&vault.allowances.into(), now)?;
    // Beyond any cap that could be carried, so the bound changes nothing.
    Ok(day
        .min(shonin_common::math::U256::from(u128::MAX))
        .as_u128())
}

/// The author withdraws from the tank, the bill settled first against a total
/// recent enough to cover what the agent has used. A bill somebody has
/// already collected needn't be collected again.
pub fn withdraw_tank<'info>(
    ctx: Context<'_, '_, '_, 'info, WithdrawTank<'info>>,
    assets: u64,
    usage: Usage,
) -> Result<()> {
    let a = ctx.accounts;
    require!(
        usage.as_of.saturating_add(a.vault.timers.usage_freshness) >= now()?,
        VaultError::Stale
    );
    let seeds = a.vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = Purse {
        vault: a.vault.to_account_info(),
        dollars: a.vault_dollars.to_account_info(),
        mint: a.mint.to_account_info(),
        token_program: a.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    };
    if usage.total != a.vault.tank.usage_settled || usage.as_of != a.vault.tank.usage_as_of {
        collect(&mut a.vault, &a.billing, &purse, usage)?;
    }
    pay_tank(&mut a.vault, &purse, &a.to, &a.author.key(), assets)
}

fn pay_tank<'info>(
    vault: &mut Vault,
    purse: &Purse<'_, 'info>,
    to: &AccountInfo<'info>,
    author: &Pubkey,
    assets: u64,
) -> Result<()> {
    own_account(to, author, &vault.mint)?;
    let assets = assets.min(vault.tank.balance);
    vault.tank.balance -= assets;
    purse.send(assets, to)?;
    emit!(TankWithdrawn { assets });
    Ok(())
}

/// With no executor running nobody can sign a total. After a delay long
/// enough for anyone to collect a final bill, the author can withdraw
/// without one, within the same length of time again: a request lapses, so
/// it can't be armed early and used once an executor runs again.
pub fn request_tank_withdrawal(ctx: Context<Author>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    let now = now()?;
    vault.tank.escape_requested_at = now;
    emit!(TankWithdrawalRequested {
        ready_at: now.saturating_add(vault.timers.tank_escape_delay),
    });
    Ok(())
}

pub fn withdraw_tank_unsigned<'info>(
    ctx: Context<'_, '_, '_, 'info, WithdrawTankUnsigned<'info>>,
    assets: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault = &mut a.vault;
    if vault.state != VaultState::Closed {
        let asked = vault.tank.escape_requested_at;
        let delay = vault.timers.tank_escape_delay;
        let now = now()?;
        require!(
            asked != 0 && now >= asked.saturating_add(delay),
            VaultError::NotYet
        );
        require!(
            now <= asked.saturating_add(delay.saturating_mul(2)),
            VaultError::NotAllowed
        );
        vault.tank.escape_requested_at = 0;
    }
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = Purse {
        vault: vault.to_account_info(),
        dollars: a.vault_dollars.to_account_info(),
        mint: a.mint.to_account_info(),
        token_program: a.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    };
    pay_tank(vault, &purse, &a.to, &a.author.key(), assets)
}

/// The author redeems some of their shares into the tank: queued, priced like
/// any withdrawal, from cash only, never a claim. The request is made at the
/// author's address for `nonce`.
pub fn top_up_from_shares<'info>(
    ctx: Context<'_, '_, '_, 'info, TopUpFromShares<'info>>,
    shares: u128,
    nonce: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let vault = &mut a.vault;
    require!(
        vault.state != VaultState::Closed && !vault.ready_to_close(),
        VaultError::WrongState
    );
    require!(shares != 0, VaultError::TooSmall);
    let author = vault.author;
    queue(
        vault,
        &vault_key,
        Making {
            request: &a.request,
            maker: a.author.key(),
            nonce,
            payer: &a.author,
            system_program: &a.system_program,
        },
        author,
        RequestKind::TankFill,
        shares,
        0,
        &mut Said::logs(),
    )?;
    Ok(())
}

/// What the keeper keeps the tank at, from the author's shares; 0 is off.
pub fn set_tank_keep(ctx: Context<Author>, assets: u64) -> Result<()> {
    require!(assets <= MAX_TANK_KEEP, VaultError::OutOfRange);
    ctx.accounts.vault.tank.keep_at = assets;
    emit!(TankKeepSet { assets });
    Ok(())
}

/// Closes the cap's interval at the old allowances, then runs on the new.
pub fn set_allowances(
    ctx: Context<SetAllowances>,
    allowances: shonin_prices::Allowances,
) -> Result<()> {
    let a = ctx.accounts;
    let vault = &mut a.vault;
    let now = now()?;
    let end = cap_end(vault, now);
    let earned = cost(vault, &a.prices, vault.tank.last_collection, end)?;
    vault.tank.cap_carried = vault
        .tank
        .cap_carried
        .checked_add(earned)
        .ok_or(VaultError::Overflow)?;
    vault.allowances = allowances.into();
    vault.allowances_since = now;
    emit!(AllowancesSet { allowances });
    Ok(())
}

// Freezes -------------------------------------------------------------------------------

/// A claim unpaid for `freeze_after` from its booking freezes the vault,
/// whatever was tried since: a leaver left unpaid this long is reason enough
/// to stop, whether the market or anyone else kept them waiting.
pub fn freeze(ctx: Context<Freeze>, claim_id: u64) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let claim = load_claim(&a.claim, &vault_key, claim_id)?.ok_or(VaultError::NotAllowed)?;
    require!(
        matches!(
            claim.state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ),
        VaultError::NotAllowed
    );
    let vault = &mut a.vault;
    require!(
        now()? >= claim.booked_at.saturating_add(vault.timers.freeze_after),
        VaultError::NotYet
    );
    // A vault already halted (an evicted retirement) is stopping anyway.
    require!(vault.live() && !vault.halted(), VaultError::WrongState);
    halt(vault, HaltReason::Frozen)?;
    set_state(vault, VaultState::Frozen);
    Ok(())
}

/// Units nobody could serve for as long as an unpaid claim waits freeze the
/// vault the same way: anyone may serve them on a vault that trades on its
/// own chain only, so a close that can't come home must not keep a unit
/// holder out for good either.
pub fn freeze_units(ctx: Context<FreezeUnits>) -> Result<()> {
    let a = ctx.accounts;
    require!(a.holder.units != 0, VaultError::NotAllowed);
    let vault = &mut a.vault;
    require!(
        now()?
            >= a.holder
                .units_since
                .saturating_add(vault.timers.freeze_after),
        VaultError::NotYet
    );
    require!(vault.live() && !vault.halted(), VaultError::WrongState);
    halt(vault, HaltReason::Frozen)?;
    set_state(vault, VaultState::Frozen);
    Ok(())
}

// The lifecycle --------------------------------------------------------------------------

pub fn retire(ctx: Context<Author>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    require!(vault.state == VaultState::Open, VaultError::WrongState);
    vault.retired_at = now()?;
    set_state(vault, VaultState::WindingDown);
    Ok(())
}

/// Anyone, once a retirement's wind-down has run its course.
pub fn evict(ctx: Context<Anyone>) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    require!(
        vault.state == VaultState::WindingDown && !vault.halted(),
        VaultError::WrongState
    );
    require!(
        now()? >= vault.retired_at.saturating_add(vault.timers.wind_down),
        VaultError::NotYet
    );
    halt(vault, HaltReason::Retired)
}

/// The admin key stops a vault: a kill, or the wind-down of a vault whose
/// root key was lost, recorded as sealed. It moves no money: the station
/// unwinds and every holder is paid from the closed vault.
pub fn kill(ctx: Context<Admin>, reason: HaltReason) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    require!(
        matches!(
            vault.state,
            VaultState::Open | VaultState::WindingDown | VaultState::Frozen
        ),
        VaultError::WrongState
    );
    halt(vault, reason)?;
    set_state(vault, VaultState::Killed);
    Ok(())
}

/// Once a valuation after the halt shows the station holds nothing: pay the
/// fees owed, then stop them. Value is cash from here on.
pub fn close(ctx: Context<CloseVault>) -> Result<()> {
    let p = &mut ctx.accounts.pool;
    let vault_key = p.vault.key();
    p.vault.sync_arrivals(&p.station, &mut Said::logs())?;
    let vault = &mut p.vault;
    require!(
        vault.halted() && vault.state != VaultState::Closed,
        VaultError::WrongState
    );
    require!(vault.priced, VaultError::NotYet);
    // Strictly after the halt: nothing can be sent out once it has halted, so
    // a station found empty then stays empty.
    require!(
        vault.latest.time > vault.halted_at && vault.empty(),
        VaultError::NotYet
    );
    // What the last valuation priced is settled at its price; anything asked
    // for after it is settled at the closed vault's.
    require!(!vault.owed(), VaultError::Unsettled);
    let now = now()?;
    sync_holding(vault, &vault_key, now, &mut Said::logs())?;
    let cash = vault.cash as u128;
    let supply = vault.total_supply + OFFSET;
    payday(
        vault,
        cash + 1,
        supply,
        now,
        &mut p.author_holder,
        &mut p.platform_holder,
        &vault_key,
        &mut Said::logs(),
    )?;
    // The dead shares' part, which no holder can ever redeem, goes to the
    // author who paid for them, into the tank they can take at once.
    let dead = vault.dead_shares;
    if dead != 0 {
        let theirs = dollars(mul_div(vault.cash as u128, dead, vault.total_supply)?)?;
        vault.dead_shares = 0;
        vault.total_supply -= dead;
        emit!(Transfer {
            from: Pubkey::default(),
            to: Pubkey::default(),
            value: dead,
        });
        vault.cash -= theirs;
        vault.tank.balance = vault
            .tank
            .balance
            .checked_add(theirs)
            .ok_or(VaultError::Overflow)?;
    }
    set_state(vault, VaultState::Closed);
    Ok(())
}

// The closed vault's doors -----------------------------------------------------------------
//
// Every door books what the station has sent home since the vault last looked
// before it pays, as every live path does: what arrives after the close is
// the pool's, so the order holders leave in doesn't change what each is paid.

fn closed_purse<'a, 'info>(
    vault: &Account<'info, Vault>,
    dollars_account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    seeds: &'a [&'a [u8]],
) -> Purse<'a, 'info> {
    Purse {
        vault: vault.to_account_info(),
        dollars: dollars_account.clone(),
        mint: mint.clone(),
        token_program: token_program.clone(),
        decimals: DECIMALS,
        seeds,
    }
}

/// A holder's fraction of the closed vault's cash, with no request, no
/// valuation and no transaction fee, paid to the associated dollar account
/// of whoever they name. Its caller chooses the receiver, so one that can't
/// take it fails the call instead of holding it.
pub fn redeem_closed<'info>(
    ctx: Context<'_, '_, '_, 'info, RedeemClosed<'info>>,
    shares: u128,
    receiver: Pubkey,
) -> Result<()> {
    let a = ctx.accounts;
    require!(a.vault.state == VaultState::Closed, VaultError::WrongState);
    // Nobody can sign for the default address, so nothing paid there could
    // ever be spent.
    require!(receiver != Pubkey::default(), VaultError::TooSmall);
    a.vault.sync_arrivals(&a.station, &mut Said::logs())?;
    let vault_key = a.vault.key();
    let vault = &mut a.vault;
    let assets = dollars(mul_div(vault.cash as u128, shares, vault.total_supply)?)?;
    let said = &mut Said::logs();
    vault.burn_from(&mut a.holder, shares, said)?;
    vault.cash -= assets;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = closed_purse(vault, &a.vault_dollars, &a.mint, &a.token_program, &seeds);
    require!(
        purse.pay(&a.to, &receiver, assets, said)?,
        VaultError::WrongAccount
    );
    let _ = vault_key;
    emit!(Withdrawn {
        request_id: 0,
        owner: receiver,
        shares,
        assets,
        fee_shares: 0,
    });
    Ok(())
}

/// A request nothing priced before the vault closed: a deposit goes back to
/// whoever paid it, a withdrawal is paid as `redeem_closed` pays, a tank fill
/// lapses, and a claim's request is settled through `claim_closed`. `other`
/// is a deposit's holding; `to` the receiver's associated dollar account.
pub fn settle_closed<'info>(
    ctx: Context<'_, '_, '_, 'info, ClosedDoor<'info>>,
    request_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    require!(a.vault.state == VaultState::Closed, VaultError::WrongState);
    a.vault.sync_arrivals(&a.station, &mut Said::logs())?;
    let vault_key = a.vault.key();
    let mut r = load_request(&a.record, &vault_key, request_id)?.ok_or(VaultError::NotAllowed)?;
    require!(
        !r.done && r.kind != RequestKind::Claim,
        VaultError::NotAllowed
    );
    r.done = true;
    let seeds = a.vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = closed_purse(
        &a.vault,
        &a.vault_dollars,
        &a.mint,
        &a.token_program,
        &seeds,
    );
    let vault = &mut a.vault;
    let said = &mut Said::logs();
    match r.kind {
        RequestKind::Deposit => {
            let holder_info = a.other.as_ref().ok_or(VaultError::WrongAccount)?;
            let mut holder = load::<Holder>(holder_info, &holder_address(&vault_key, &r.owner).0)?
                .ok_or(VaultError::WrongAccount)?;
            holder.pending = holder.pending.saturating_sub(1);
            store(holder_info, &holder)?;
            let assets = r.amount as u64;
            vault.escrow -= assets;
            let payer = r.payer;
            if !purse.pay(&a.to, &payer, assets, said)? {
                hold_on(vault, &mut r, payer, assets)?;
            }
            emit!(Refunded {
                request_id,
                to: payer,
                assets,
            });
        }
        RequestKind::Redeem => {
            let shares = r.amount;
            let assets = dollars(mul_div(vault.cash as u128, shares, vault.total_supply)?)?;
            vault.pending_shares -= shares;
            vault.burn_held(shares, &vault_key, said)?;
            vault.cash -= assets;
            let owner = r.owner;
            if !purse.pay(&a.to, &owner, assets, said)? {
                hold_on(vault, &mut r, owner, assets)?;
            }
            emit!(Withdrawn {
                request_id,
                owner,
                shares,
                assets,
                fee_shares: 0,
            });
        }
        _ => {
            if r.reference == 1 {
                vault.tank.fill_pending = false;
            }
        }
    }
    store(&a.record, &r)
}

/// A claim the closed vault pays: its fraction of the cash, fee-free. `other`
/// is a queued claim's request.
pub fn claim_closed<'info>(
    ctx: Context<'_, '_, '_, 'info, ClosedDoor<'info>>,
    claim_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    require!(a.vault.state == VaultState::Closed, VaultError::WrongState);
    a.vault.sync_arrivals(&a.station, &mut Said::logs())?;
    let vault_key = a.vault.key();
    let mut claim = load_claim(&a.record, &vault_key, claim_id)?.ok_or(VaultError::NotAllowed)?;
    require!(
        matches!(
            claim.state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ),
        VaultError::NotAllowed
    );
    let seeds = a.vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = closed_purse(
        &a.vault,
        &a.vault_dollars,
        &a.mint,
        &a.token_program,
        &seeds,
    );
    let vault = &mut a.vault;
    if claim.state == ClaimState::Queued {
        let request = a.other.as_ref().ok_or(VaultError::WrongAccount)?;
        let mut r =
            load_request(request, &vault_key, claim.request)?.ok_or(VaultError::WrongAccount)?;
        r.done = true;
        store(request, &r)?;
        vault.queued_claims -= 1;
    }
    let assets = dollars(mul_div(
        vault.cash as u128,
        claim.shares,
        vault.total_supply,
    )?)?;
    let said = &mut Said::logs();
    vault.pending_shares -= claim.shares;
    vault.burn_held(claim.shares, &vault_key, said)?;
    vault.cash -= assets;
    claim.state = ClaimState::Paid;
    claim.paid += assets;
    let owner = claim.owner;
    if !purse.pay(&a.to, &owner, assets, said)? {
        crate::claims::hold_on_claim(vault, &mut claim, assets)?;
    }
    emit!(ClaimPaid { claim_id, assets });
    store(&a.record, &claim)
}

/// Where a held payout is recorded.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldIn {
    Request(u64),
    Claim(u64),
    /// A claim's profit fee in cash, held for the author or the platform.
    Fees,
}

/// A payout its receiver's account couldn't take, collected by the receiver
/// to any dollar account of theirs.
pub fn collect_held<'info>(
    ctx: Context<'_, '_, '_, 'info, CollectHeld<'info>>,
    held: HeldIn,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let owner = a.owner.key();
    let assets = match held {
        HeldIn::Request(id) => {
            let record = a.record.as_ref().ok_or(VaultError::WrongAccount)?;
            let mut r = load_request(record, &vault_key, id)?.ok_or(VaultError::Nothing)?;
            require!(r.held != 0 && r.held_for == owner, VaultError::Nothing);
            let assets = r.held;
            r.held = 0;
            store(record, &r)?;
            assets
        }
        HeldIn::Claim(id) => {
            let record = a.record.as_ref().ok_or(VaultError::WrongAccount)?;
            let mut claim = load_claim(record, &vault_key, id)?.ok_or(VaultError::Nothing)?;
            require!(claim.held != 0 && claim.owner == owner, VaultError::Nothing);
            let assets = claim.held;
            claim.held = 0;
            store(record, &claim)?;
            assets
        }
        HeldIn::Fees => {
            let vault = &mut a.vault;
            let mut assets = 0;
            if owner == vault.author {
                assets += vault.held_author;
                vault.held_author = 0;
            }
            if owner == vault.platform {
                assets += vault.held_platform;
                vault.held_platform = 0;
            }
            require!(assets != 0, VaultError::Nothing);
            assets
        }
    };
    let vault = &mut a.vault;
    vault.held_total -= assets;
    own_account(&a.to, &owner, &vault.mint)?;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    closed_purse(vault, &a.vault_dollars, &a.mint, &a.token_program, &seeds).send(assets, &a.to)?;
    emit!(HeldCollected { to: owner, assets });
    Ok(())
}

// Rent back -------------------------------------------------------------------------------

/// Closes a request that is done and holds nothing, refunding its rent to
/// whoever paid it. Anyone may send it; the refund goes where the request
/// says. Not before the cursor has passed it unless the vault has closed:
/// settlement knows a request by its account, and only a request that became
/// a claim may be missing from the queue ahead of the cursor.
pub fn close_request<'info>(
    ctx: Context<'_, '_, '_, 'info, CloseRecord<'info>>,
    request_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let r = load_request(&a.record, &vault_key, request_id)?.ok_or(VaultError::Nothing)?;
    require!(r.done && r.held == 0, VaultError::NotAllowed);
    require!(passed(&a.vault, request_id), VaultError::NotYet);
    require_keys_eq!(a.payer.key(), r.payer, VaultError::WrongAccount);
    close_account(&a.record, &a.payer)
}

/// Closes a claim that is paid or taken through the fallback and holds
/// nothing, refunding its rent to whoever paid it.
pub fn close_claim<'info>(
    ctx: Context<'_, '_, '_, 'info, CloseRecord<'info>>,
    claim_id: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let claim = load_claim(&a.record, &vault_key, claim_id)?.ok_or(VaultError::Nothing)?;
    require!(
        matches!(claim.state, ClaimState::Paid | ClaimState::Fallback) && claim.held == 0,
        VaultError::NotAllowed
    );
    // A claim stands in for its request in settlement until the cursor has
    // passed it.
    require!(passed(&a.vault, claim_id), VaultError::NotYet);
    require_keys_eq!(a.payer.key(), claim.payer, VaultError::WrongAccount);
    close_account(&a.record, &a.payer)
}

/// Whether settlement has no more need of request `id`'s account, or of the
/// claim it became: the cursor has passed it, or the vault has closed and
/// settles what is left through its own doors.
fn passed(vault: &Vault, id: u64) -> bool {
    id <= vault.request_cursor || vault.state == VaultState::Closed
}

/// A holder closes an empty holding: nothing in it, nothing waiting on it.
/// The author's and the platform's stay, since fees are paid into them.
pub fn close_holder(ctx: Context<CloseHolder>) -> Result<()> {
    let a = ctx.accounts;
    let h = &a.holder;
    require!(
        h.owner != a.vault.author && h.owner != a.vault.platform,
        VaultError::NotAllowed
    );
    require!(
        h.shares == 0 && h.units == 0 && h.unit_credit == 0 && h.pending == 0,
        VaultError::NotAllowed
    );
    Ok(())
}

// The idle floor --------------------------------------------------------------------------

/// Raising the floor applies at once; lowering it waits `floor_delay`, with
/// the old floor in force meanwhile.
pub fn set_idle_floor(ctx: Context<Author>, bps: u16) -> Result<()> {
    let vault = &mut ctx.accounts.vault;
    require!(bps as u128 <= BPS, VaultError::NotAllowed);
    let now = now()?;
    let current = vault.floor_now(now);
    vault.floor_bps = current;
    if bps >= current {
        vault.floor_bps = bps;
        vault.pending_floor_bps = 0;
        vault.pending_floor_at = 0;
        emit!(IdleFloorSet {
            bps,
            effective_at: now,
        });
    } else {
        vault.pending_floor_bps = bps;
        vault.pending_floor_at = now.saturating_add(vault.timers.floor_delay);
        emit!(IdleFloorSet {
            bps,
            effective_at: vault.pending_floor_at,
        });
    }
    Ok(())
}

// Internals --------------------------------------------------------------------------------

/// The agent is told, and keeps its keys for the grace period; after it
/// anyone may stop the station and unwind.
fn halt(vault: &mut Vault, reason: HaltReason) -> Result<()> {
    if vault.halted_at == 0 {
        vault.halted_at = now()?;
        vault.halt_reason = reason;
        emit!(Halted {
            reason,
            halted_at: vault.halted_at,
        });
    }
    Ok(())
}

fn set_state(vault: &mut Vault, state: VaultState) {
    vault.state = state;
    emit!(StateChanged { state });
}
