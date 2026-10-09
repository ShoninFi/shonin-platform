//! Requests, valuations and settlement: how money joins and leaves a vault,
//! and what it pays to. The counterpart of the EVM vault's `Valuations`.
//!
//! Every request is priced at the first valuation at or after its `earliest`,
//! and everything one valuation settles is priced at that valuation's
//! snapshot. A slot settles its exits before its deposits, so every exit
//! meets the pool exactly as the snapshot found it, and its deposits pay one
//! price, the pool the exits left. A deposit buys at the net price; a
//! withdrawal is paid at the exit-side price, less a slice of its own shares
//! for the profit fee they owe and the transaction fee the pool keeps.

use anchor_lang::prelude::*;
use shonin_common::{account::create_pda, math::U256, time::now};

use crate::contexts::*;
use crate::error::VaultError;
use crate::events::*;
use crate::ledger::*;
use crate::maths::*;
use crate::state::*;
use crate::station;

// Requests --------------------------------------------------------------------------

/// Where a request is made, and by whom: its maker and nonce (see
/// `REQUEST_SEED`), and who pays its rent.
pub struct Making<'a, 'info> {
    pub request: &'a AccountInfo<'info>,
    pub maker: Pubkey,
    pub nonce: u64,
    pub payer: &'a AccountInfo<'info>,
    pub system_program: &'a AccountInfo<'info>,
}

/// Makes request `request_count + 1` at its maker's address. The id is given
/// here, as the request is made, so requests made at once never contend for
/// one.
#[allow(clippy::too_many_arguments)]
pub fn queue(
    vault: &mut Vault,
    vault_key: &Pubkey,
    making: Making,
    owner: Pubkey,
    kind: RequestKind,
    amount: u128,
    reference: u64,
    said: &mut Said,
) -> Result<u64> {
    let Making {
        request,
        maker,
        nonce,
        payer,
        system_program,
    } = making;
    let id = vault.request_count + 1;
    let (address, bump) = request_address(vault_key, &maker, nonce);
    require_keys_eq!(*request.key, address, VaultError::WrongAccount);
    let earliest = next_slot(&vault.timers, now()?)?;
    let space = 8 + Request::INIT_SPACE;
    create_pda(
        payer,
        request,
        system_program,
        &[
            REQUEST_SEED,
            vault_key.as_ref(),
            maker.as_ref(),
            &nonce.to_le_bytes(),
            &[bump],
        ],
        space,
        &crate::ID,
    )?;
    store(
        request,
        &Request {
            vault: *vault_key,
            id,
            owner,
            kind,
            earliest,
            done: false,
            amount,
            reference,
            payer: *payer.key,
            held: 0,
            held_for: Pubkey::default(),
            bump,
            reserved: [0; 16],
        },
    )?;
    vault.note_request(earliest);
    said.say(Requested {
        request_id: id,
        address,
        owner,
        kind,
        earliest,
        amount,
    });
    Ok(id)
}

/// Assets in escrow now, for shares at the next valuation. Only while the
/// vault is open, and only for a receiver who stated they are not a
/// restricted person for the list as it is: the payer states it by naming
/// the list's hash in this signed instruction when they are the receiver.
/// The request is made at the payer's address for `nonce`.
pub fn request_deposit<'info>(
    ctx: Context<'_, '_, '_, 'info, RequestDeposit<'info>>,
    assets: u64,
    receiver: Pubkey,
    statement: Option<[u8; 32]>,
    nonce: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let holder = &mut a.holder;
    if holder.owner == Pubkey::default() {
        holder.vault = vault_key;
        holder.owner = receiver;
        holder.payer = a.payer.key();
        holder.bump = ctx.bumps.holder;
    }
    if let Some(statement) = statement {
        require_keys_eq!(a.payer.key(), receiver, VaultError::NotAllowed);
        crate::upkeep::state(&a.vault, holder, statement)?;
    }
    let vault = &mut a.vault;
    require!(vault.state == VaultState::Open, VaultError::WrongState);
    // Nobody can sign for the default address, so its shares could never
    // leave the vault.
    require!(
        assets >= vault.minimum_deposit && receiver != Pubkey::default(),
        VaultError::TooSmall
    );
    // Every holder is someone who signed for the vault's list as it is.
    require!(
        holder.statement == vault.restricted_hash,
        VaultError::NoStatement
    );
    anchor_spl::token::transfer_checked(
        CpiContext::new(
            a.token_program.to_account_info(),
            anchor_spl::token::TransferChecked {
                from: a.payer_dollars.to_account_info(),
                mint: a.mint.to_account_info(),
                to: a.vault_dollars.to_account_info(),
                authority: a.payer.to_account_info(),
            },
        ),
        assets,
        DECIMALS,
    )?;
    vault.escrow = vault
        .escrow
        .checked_add(assets)
        .ok_or(VaultError::Overflow)?;
    holder.pending = holder.pending.checked_add(1).ok_or(VaultError::Overflow)?;
    queue(
        vault,
        &vault_key,
        Making {
            request: &a.request,
            maker: a.payer.key(),
            nonce,
            payer: &a.payer,
            system_program: &a.system_program,
        },
        receiver,
        RequestKind::Deposit,
        assets as u128,
        0,
        &mut Said::logs(),
    )?;
    Ok(())
}

/// Shares held by the vault now, for dollars at the next valuation, paid to
/// `receiver`. Allowed in every state that has shares, until the vault is
/// ready to close. The request is made at the owner's address for `nonce`.
pub fn request_redeem<'info>(
    ctx: Context<'_, '_, '_, 'info, RequestRedeem<'info>>,
    shares: u128,
    receiver: Pubkey,
    nonce: u64,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.vault.key();
    let vault = &mut a.vault;
    require!(
        vault.state != VaultState::Closed && vault.state != VaultState::None,
        VaultError::WrongState
    );
    // Nobody can sign for the default address, so its payout, or a claim's,
    // could never be collected.
    require!(
        shares != 0 && receiver != Pubkey::default(),
        VaultError::TooSmall
    );
    require!(!vault.ready_to_close(), VaultError::WrongState);
    let holder = &mut a.holder;
    holder.shares = holder
        .shares
        .checked_sub(shares)
        .ok_or(VaultError::TooSmall)?;
    vault.pending_shares = vault
        .pending_shares
        .checked_add(shares)
        .ok_or(VaultError::Overflow)?;
    emit!(Transfer {
        from: holder.owner,
        to: vault_key,
        value: shares,
    });
    queue(
        vault,
        &vault_key,
        Making {
            request: &a.request,
            maker: a.owner.key(),
            nonce,
            payer: &a.owner,
            system_program: &a.system_program,
        },
        receiver,
        RequestKind::Redeem,
        shares,
        0,
        &mut Said::logs(),
    )?;
    Ok(())
}

// Valuations ---------------------------------------------------------------------------

/// Records this slot's valuation: anyone, inside the slot's window, once a
/// slot, and only once the last priced valuation's requests are settled. A
/// valuation at which any market the station holds is outside the exit band
/// is void: it prices nothing, and its requests roll on. Everything it books
/// is said through the program's own instruction data, as a settlement's is.
pub fn record_valuation<'info>(
    ctx: Context<'_, '_, '_, 'info, RecordValuation<'info>>,
    groups: Vec<u8>,
) -> Result<()> {
    let p = &mut ctx.accounts.pool;
    let vault_key = p.vault.key();
    let vault = &mut p.vault;
    require!(
        vault.state != VaultState::Closed && vault.state != VaultState::None,
        VaultError::WrongState
    );
    let now = now()?;
    let interval = vault.timers.slot_interval;
    let time = now - now % interval;
    require!(
        now - time <= vault.timers.slot_window,
        VaultError::OutsideWindow
    );
    if vault.valuation_count != 0 {
        require!(vault.last_valued_at < time, VaultError::AlreadyValued);
    }
    require!(!vault.owed(), VaultError::Unsettled);

    // Whatever the station sent home since the vault last looked is cash now,
    // and no longer in the station's value.
    let mut said = Said::kept();
    vault.sync_arrivals(&p.station, &mut said)?;
    sync_holding(vault, &vault_key, now, &mut said)?;
    let valued = station::value(
        &ctx.accounts.station_program,
        &p.station,
        ctx.remaining_accounts,
        groups,
    )?;
    let (mut mark, mut exit) = (valued.mark as u128, valued.exit as u128);
    // An exit side above the mark is what the holdings could be sold for: a
    // deposit pays it on top, so it can't buy below the pool's realisable
    // worth; but nobody is paid it, and no withdrawal or fee is measured on
    // it.
    let mut extra = 0u128;
    if exit > mark {
        extra = exit - mark;
        exit = mark;
    }
    let units = vault.live_units()?;
    if units != 0 {
        // The station's value belongs partly to the fallback's unit holders,
        // who hold it outside the shares.
        mark = mul_div(mark, WAD - units, WAD)?;
        exit = mul_div(exit, WAD - units, WAD)?;
        extra = mul_div(extra, WAD - units, WAD)?;
    }
    let index = vault.valuation_count;
    vault.valuation_count = index + 1;
    vault.last_valued_at = time;
    let authority = ctx.accounts.event_authority.to_account_info();
    let bump = ctx.bumps.event_authority;
    if valued.outside {
        said.say(ValuationVoided { index, time });
        return said.emit(&authority, bump);
    }
    let cash = vault.cash as u128;
    let snapshot = Snapshot {
        index,
        time,
        gross: cash + mark + 1,
        gross_exit: cash + exit + 1,
        supply: 0,
        fee: 0,
        fee_exit: 0,
        cash: vault.cash,
        extra,
        sent: vault.sent_total,
    };
    price(
        vault,
        snapshot,
        &mut p.author_holder,
        &mut p.platform_holder,
        &vault_key,
        &mut said,
    )?;
    let latest = vault.latest;
    said.say(Valued {
        index,
        time,
        gross: latest.gross,
        gross_exit: latest.gross_exit,
        supply: latest.supply,
        fee: latest.fee,
    });
    said.emit(&authority, bump)
}

/// Fixes a valuation's prices: runs payday if it is due, then snapshots.
pub fn price(
    vault: &mut Vault,
    mut v: Snapshot,
    author: &mut Holder,
    platform: &mut Holder,
    vault_key: &Pubkey,
    said: &mut Said,
) -> Result<()> {
    // An exit is never worth more than the mark: a bid above it would pay a
    // leaver more than the pool holds for them.
    if v.gross_exit > v.gross {
        v.gross_exit = v.gross;
    }
    let mut supply = vault.total_supply + OFFSET;
    if vault.state != VaultState::Closed && v.time >= next_monday(vault.last_payment) {
        supply = payday(
            vault, v.gross, supply, v.time, author, platform, vault_key, said,
        )?;
    }
    v.supply = supply;
    v.fee = fee_of(vault.mark, &vault.fees, v.gross, supply)?;
    v.fee_exit = fee_of(vault.mark, &vault.fees, v.gross_exit, supply)?;
    vault.exit_paid = 0;
    vault.latest = v;
    vault.priced = true;
    // Every request made for this time or earlier is this valuation's to
    // settle; the next can't be recorded until they are.
    vault.priced_through = vault.priced_by(v.time);
    Ok(())
}

/// Pays the profit fee owed, in new shares, and hands the holding fee over.
/// The price is the same after as before.
#[allow(clippy::too_many_arguments)]
pub fn payday(
    vault: &mut Vault,
    gross: u128,
    supply: u128,
    time: u64,
    author: &mut Holder,
    platform: &mut Holder,
    vault_key: &Pubkey,
    said: &mut Said,
) -> Result<u128> {
    let fee = fee_of(vault.mark, &vault.fees, gross, supply)?;
    let minted = if fee == 0 {
        0
    } else {
        mul_div(fee, supply, gross - fee)?
    };
    let total = vault.fees.author_bps as u128 + vault.fees.platform_bps as u128;
    let to_author = if total == 0 {
        0
    } else {
        mul_div(minted, vault.fees.author_bps as u128, total)?
    };
    vault.mint_to(author, to_author, said)?;
    vault.mint_to(platform, minted - to_author, said)?;
    let holding = vault.unpaid_holding;
    if holding != 0 {
        platform.shares = platform
            .shares
            .checked_add(holding)
            .ok_or(VaultError::Overflow)?;
        vault.unpaid_holding = 0;
        said.say(Transfer {
            from: *vault_key,
            to: platform.owner,
            value: holding,
        });
    }
    let supply = supply + minted;
    let raised = mul_div_up(gross, PRICE, supply)?;
    if raised > vault.mark {
        vault.mark = raised;
    }
    vault.last_payment = time;
    said.say(FeesPaid {
        author_shares: to_author,
        platform_shares: minted - to_author,
        holding_shares: holding,
        mark: vault.mark,
    });
    Ok(supply)
}

/// Mints the holding fee for the time since the last sync to the vault.
/// Minting leaves the mark alone, so it lowers value per share against the
/// mark, as a holding fee should.
pub fn sync_holding(
    vault: &mut Vault,
    vault_key: &Pubkey,
    now: u64,
    said: &mut Said,
) -> Result<()> {
    let elapsed = now.saturating_sub(vault.last_holding);
    if elapsed == 0 {
        return Ok(());
    }
    vault.last_holding = now;
    let rate = mul_div(
        vault.fees.holding_bps as u128 * elapsed as u128,
        WAD,
        BPS * YEAR as u128,
    )?;
    if rate == 0 || rate >= WAD {
        return Ok(());
    }
    let minted = mul_div(vault.total_supply + OFFSET, rate, WAD - rate)?;
    if minted == 0 {
        return Ok(());
    }
    vault.total_supply = vault
        .total_supply
        .checked_add(minted)
        .ok_or(VaultError::Overflow)?;
    vault.unpaid_holding += minted;
    said.say(Transfer {
        from: Pubkey::default(),
        to: *vault_key,
        value: minted,
    });
    said.say(HoldingSynced { shares: minted });
    Ok(())
}

// Settlement ----------------------------------------------------------------------------

/// One request's accounts in a settlement: the request, then what settling
/// it touches. A request already done takes no more; a deposit its owner's
/// holding and its payer's associated dollar account (for sending it back);
/// a withdrawal the claim it would become (the claim with its own id) and its
/// receiver's associated dollar account; a queued claim's request its claim
/// and its owner's associated dollar account; a tank fill nothing more. A
/// request that became a claim before the cursor passed it is closed, and its
/// claim is passed in its place, alone.
struct Entry<'a, 'info> {
    request: &'a AccountInfo<'info>,
    /// Loaded at the start; `None` once it became a claim.
    loaded: Option<Request>,
    first: Option<&'a AccountInfo<'info>>,
    second: Option<&'a AccountInfo<'info>>,
}

fn entries<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    vault_key: &Pubkey,
    from: u64,
) -> Result<Vec<Entry<'a, 'info>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut id = from + 1;
    while at < accounts.len() {
        require!(out.len() < MAX_SETTLE, VaultError::TooMany);
        let request = &accounts[at];
        // Each request is known by the id it holds, so none can be skipped or
        // stood in for. One that is gone must have become a claim (nothing
        // else closes a request before the cursor has passed it), and only
        // that claim, at the address its id gives, stands in for it: never
        // an empty account, whose emptiness proves nothing.
        let loaded = if holds_claim(request)? {
            load_claim(request, vault_key, id)?.ok_or(VaultError::WrongAccount)?;
            None
        } else {
            Some(load_request(request, vault_key, id)?.ok_or(VaultError::WrongAccount)?)
        };
        at += 1;
        let extra = match &loaded {
            Some(r) if !r.done => match r.kind {
                RequestKind::TankFill => 0,
                _ => 2,
            },
            _ => 0,
        };
        let group = accounts
            .get(at..at + extra)
            .ok_or(VaultError::WrongCursor)?;
        at += extra;
        out.push(Entry {
            request,
            loaded,
            first: group.first(),
            second: group.get(1),
        });
        id += 1;
    }
    Ok(out)
}

/// Settles the last priced valuation's requests, in order: anyone may, a
/// batch of up to `MAX_SETTLE` at a time. The remaining accounts are the
/// requests from `from` on, each with what settling it touches (see
/// [`Entry`]); `from` is where the queue stands (the cursor), or where its
/// exits stand when they are further on. Each request's id is checked, so
/// none can be skipped or stood in for. Everything it settles is said
/// through the program's own instruction data. Answers with how many were
/// settled.
pub fn settle<'info>(ctx: Context<'_, '_, 'info, 'info, Settle<'info>>, from: u64) -> Result<()> {
    let p = &mut ctx.accounts.pool;
    let vault_key = p.vault.key();
    if !p.vault.priced {
        anchor_lang::solana_program::program::set_return_data(&0u64.to_le_bytes());
        return Ok(());
    }
    let mut said = Said::kept();
    p.vault.sync_arrivals(&p.station, &mut said)?;
    let seeds = p.vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = Purse {
        vault: p.vault.to_account_info(),
        dollars: p.vault_dollars.to_account_info(),
        mint: p.mint.to_account_info(),
        token_program: p.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    };
    let system_program = p.system_program.to_account_info();
    let vault = &mut p.vault;
    let fees = (&mut p.author_holder, &mut p.platform_holder);
    // Every request still owed belongs to the last priced valuation: a new
    // one can't be recorded while any is. Its exits settle first and its
    // deposits after.
    let index = vault.latest.index;
    let through = vault.priced_through;
    let start = vault.request_cursor;
    let exits_at = vault.exit_cursor.max(start);
    require!(from == start || from == exits_at, VaultError::WrongCursor);
    let mut list = entries(ctx.remaining_accounts, &vault_key, from)?;
    let end = from + list.len() as u64;
    let done = |list: &Vec<Entry>, k: u64| {
        list[(k - from) as usize]
            .loaded
            .as_ref()
            .is_none_or(|r| r.done)
    };

    let mut i = start;
    let mut j = exits_at;
    let mut settled: u64 = 0;
    if from == start {
        // Requests already done (taken through the fallback) are passed
        // whatever their slot, so no order of fallbacks can hold the cursor.
        while i < end && i < vault.request_count && done(&list, i) {
            i += 1;
        }
        if j < i {
            j = i;
        }
    }
    while j < through && j < end {
        let entry = &mut list[(j - from) as usize];
        if let Some(r) = entry.loaded.as_mut() {
            if !r.done && r.kind != RequestKind::Deposit {
                r.done = true;
                settle_one(
                    vault,
                    &vault_key,
                    entry,
                    j + 1,
                    &purse,
                    fees.0,
                    fees.1,
                    &system_program,
                    &mut said,
                )?;
                settled += 1;
            }
        }
        j += 1;
    }
    vault.exit_cursor = j;
    if j >= through {
        // The exits are settled: the slot's deposits pay the price of the
        // pool they left, the gap between what they were paid and the mark
        // included, never the snapshot's.
        if vault.deposits_for != index + 1 {
            let supply = vault.total_supply + OFFSET;
            let gross = vault
                .latest
                .gross
                .checked_sub(vault.exit_paid)
                .ok_or(VaultError::Overflow)?
                + vault.latest.extra;
            vault.deposit_net = gross - fee_of(vault.mark, &vault.fees, gross, supply)?;
            vault.deposit_supply = supply;
            vault.deposits_for = index + 1;
        }
        if from == start {
            while i < j && i < end {
                let entry = &mut list[(i - from) as usize];
                if let Some(r) = entry.loaded.as_mut() {
                    if !r.done {
                        r.done = true;
                        settle_one(
                            vault,
                            &vault_key,
                            entry,
                            i + 1,
                            &purse,
                            fees.0,
                            fees.1,
                            &system_program,
                            &mut said,
                        )?;
                        settled += 1;
                    }
                }
                i += 1;
            }
        }
    }
    vault.request_cursor = i;
    said.emit(
        &ctx.accounts.event_authority.to_account_info(),
        ctx.bumps.event_authority,
    )?;
    anchor_lang::solana_program::program::set_return_data(&settled.to_le_bytes());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn settle_one<'info>(
    vault: &mut Vault,
    vault_key: &Pubkey,
    entry: &mut Entry<'_, 'info>,
    id: u64,
    purse: &Purse<'_, 'info>,
    author: &mut Holder,
    platform: &mut Holder,
    system_program: &AccountInfo<'info>,
    said: &mut Said,
) -> Result<()> {
    let r = entry.loaded.as_mut().ok_or(VaultError::WrongAccount)?;
    match r.kind {
        RequestKind::Deposit => {
            let holder_info = entry.first.ok_or(VaultError::WrongCursor)?;
            let refund = entry.second.ok_or(VaultError::WrongCursor)?;
            // The receiver's holding: the pool's own copy when it is the
            // author's or the platform's.
            let subject = subject_of(holder_info, vault_key, &r.owner, author, platform)?;
            if let Subject::Other(None) = subject {
                return err!(VaultError::WrongAccount);
            }
            let mut holder = subject.copy(vault_key, &r.owner, author, platform);
            deposit(vault, r, id, &mut holder, refund, purse, said)?;
            match subject {
                Subject::Author => {
                    author.shares = holder.shares;
                    author.pending = holder.pending;
                }
                Subject::Platform => {
                    platform.shares = holder.shares;
                    platform.pending = holder.pending;
                }
                Subject::Other(_) => store(holder_info, &holder)?,
            }
        }
        RequestKind::Redeem => {
            let claim = entry.first.ok_or(VaultError::WrongCursor)?;
            let to = entry.second.ok_or(VaultError::WrongCursor)?;
            if redeem(vault, vault_key, r, id, to, purse, author, platform, said)? {
                // The cash couldn't pay it: a claim, in an account its
                // request's rent pays for.
                book_claim(
                    vault,
                    vault_key,
                    entry.request,
                    claim,
                    system_program,
                    r,
                    now()?,
                    said,
                )?;
                return Ok(());
            }
        }
        RequestKind::Claim => {
            let claim = entry.first.ok_or(VaultError::WrongCursor)?;
            let to = entry.second.ok_or(VaultError::WrongCursor)?;
            queued_claim(
                vault, vault_key, r, id, claim, to, purse, author, platform, said,
            )?;
        }
        RequestKind::TankFill => fill(vault, r, author, platform, said)?,
    }
    store(entry.request, &*r)
}

/// Sends a deposit back to whoever paid it, or holds it for them on its
/// request if their associated dollar account can't take it.
fn refund<'info>(
    vault: &mut Vault,
    r: &mut Request,
    id: u64,
    to: &AccountInfo<'info>,
    purse: &Purse<'_, 'info>,
    said: &mut Said,
) -> Result<()> {
    let assets = r.amount as u64;
    let payer = r.payer;
    if !purse.pay(to, &payer, assets, said)? {
        hold_on(vault, r, payer, assets)?;
    }
    said.say(Refunded {
        request_id: id,
        to: r.payer,
        assets,
    });
    Ok(())
}

/// Holds `assets` for `to` on the request.
pub fn hold_on(vault: &mut Vault, r: &mut Request, to: Pubkey, assets: u64) -> Result<()> {
    r.held = r.held.checked_add(assets).ok_or(VaultError::Overflow)?;
    r.held_for = to;
    vault.held_total = vault
        .held_total
        .checked_add(assets)
        .ok_or(VaultError::Overflow)?;
    Ok(())
}

fn deposit<'info>(
    vault: &mut Vault,
    r: &mut Request,
    id: u64,
    holder: &mut Holder,
    refund_to: &AccountInfo<'info>,
    purse: &Purse<'_, 'info>,
    said: &mut Said,
) -> Result<()> {
    let assets = r.amount as u64;
    vault.escrow = vault
        .escrow
        .checked_sub(assets)
        .ok_or(VaultError::Overflow)?;
    holder.pending = holder.pending.saturating_sub(1);
    // A vault that stopped taking deposits, or a list the depositor hasn't
    // signed for, sends the money back.
    if vault.state != VaultState::Open || holder.statement != vault.restricted_hash {
        return refund(vault, r, id, refund_to, purse, said);
    }
    let net = vault.deposit_net;
    let supply = vault.deposit_supply;
    let kept = mul_div_up(assets as u128, vault.fees.transaction_bps as u128, BPS)?;
    // A pool worth next to nothing a share would mint more shares than the
    // vault can count: the deposit goes back, as one buying none does,
    // rather than holding up every request behind it.
    let room = MAX_SUPPLY.saturating_sub(vault.total_supply + OFFSET);
    let shares = mul_div_within(assets as u128 - kept, supply, net, room)?.unwrap_or(0);
    if shares == 0 {
        return refund(vault, r, id, refund_to, purse, said);
    }
    // Money joining above the mark joins at its own price: the mark becomes
    // the average, so the fee owed doesn't jump and nobody's price moves.
    // Below the mark, the newcomer rides the recovery fee-free.
    let joined = mul_div_up(net, PRICE, supply)?;
    if joined > vault.mark {
        let live = vault.total_supply + OFFSET;
        vault.mark = weighted_up(vault.mark, live, joined, shares)?;
        said.say(MarkRaised { mark: vault.mark });
    }
    vault.cash = vault.cash.checked_add(assets).ok_or(VaultError::Overflow)?;
    vault.mint_to(holder, shares, said)?;
    said.say(Deposited {
        request_id: id,
        owner: r.owner,
        assets,
        shares,
    });
    Ok(())
}

/// What `shares` come to at this valuation's exit price: the slice of them
/// that pays the profit fee they owe, the rest burned, and the payout after
/// the transaction fee.
pub fn exit_of(vault: &Vault, v: &Snapshot, shares: u128) -> Result<(u128, u128, u64)> {
    let slice = if v.fee_exit == 0 {
        0
    } else {
        mul_div_up(shares, v.fee_exit, v.gross_exit)?
    };
    let burned = shares - slice.min(shares);
    let gross = mul_div(burned, v.gross_exit, v.supply)?;
    let kept = mul_div_up(gross, vault.fees.transaction_bps as u128, BPS)?;
    // Every caller pays only what the cash covers: a payout past what a
    // dollar amount holds is more than any cash, and counts as the most one
    // holds rather than failing the settlement it is part of.
    let payout = u64::try_from(gross - kept).unwrap_or(u64::MAX);
    Ok((slice.min(shares), burned, payout))
}

/// Pays a withdrawal from cash; answers true if the cash can't, for it to be
/// booked as a claim.
#[allow(clippy::too_many_arguments)]
fn redeem<'info>(
    vault: &mut Vault,
    vault_key: &Pubkey,
    r: &mut Request,
    id: u64,
    to: &AccountInfo<'info>,
    purse: &Purse<'_, 'info>,
    author: &mut Holder,
    platform: &mut Holder,
    said: &mut Said,
) -> Result<bool> {
    let shares = r.amount;
    let latest = vault.latest;
    let (slice, burned, payout) = exit_of(vault, &latest, shares)?;
    if payout > vault.cash {
        return Ok(true);
    }
    vault.pending_shares -= shares;
    vault.hand_slice(slice, author, platform, vault_key, said)?;
    vault.burn_held(burned, vault_key, said)?;
    vault.cash -= payout;
    vault.exit_paid += payout as u128;
    let owner = r.owner;
    if !purse.pay(to, &owner, payout, said)? {
        hold_on(vault, r, owner, payout)?;
    }
    said.say(Withdrawn {
        request_id: id,
        owner: r.owner,
        shares,
        assets: payout,
        fee_shares: slice,
    });
    Ok(false)
}

/// Books a withdrawal the cash couldn't pay as a claim, its shares still
/// pending and outstanding, served by a close inside the band. A claim's id is
/// the id of the withdrawal it was booked from, so its address is known before
/// it exists. Its account is funded by the request's rent: the request is
/// closed into it, and the claim refunds its payer when it is closed in turn.
#[allow(clippy::too_many_arguments)]
pub fn book_claim<'info>(
    vault: &mut Vault,
    vault_key: &Pubkey,
    request: &AccountInfo<'info>,
    claim: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    r: &Request,
    booked_at: u64,
    said: &mut Said,
) -> Result<u64> {
    let id = r.id;
    let (address, bump) = claim_address(vault_key, id);
    require_keys_eq!(*claim.key, address, VaultError::WrongAccount);
    // The claim's address holds nothing of this program's (it may hold
    // lamports someone sent it). It is given its room and this program as
    // its owner first, and only then is the request's rent moved there and
    // the request closed: lamports moved between an account a call names and
    // one it doesn't would unbalance that call.
    require!(
        claim.owner == &anchor_lang::system_program::ID && claim.data_is_empty(),
        VaultError::WrongAccount
    );
    let space = 8 + Claim::INIT_SPACE;
    let seeds: &[&[u8]] = &[CLAIM_SEED, vault_key.as_ref(), &id.to_le_bytes(), &[bump]];
    anchor_lang::solana_program::program::invoke_signed(
        &anchor_lang::solana_program::system_instruction::allocate(claim.key, space as u64),
        &[claim.clone(), system_program.clone()],
        &[seeds],
    )?;
    anchor_lang::solana_program::program::invoke_signed(
        &anchor_lang::solana_program::system_instruction::assign(claim.key, &crate::ID),
        &[claim.clone(), system_program.clone()],
        &[seeds],
    )?;
    close_account(request, claim)?;
    require!(claim.lamports() >= rent_for(space)?, VaultError::Overflow);
    store(
        claim,
        &Claim {
            id,
            owner: r.owner,
            shares: r.amount,
            booked_at,
            last_attempt_at: 0,
            state: ClaimState::Pending,
            paid: 0,
            request: 0,
            payer: r.payer,
            held: 0,
            bump,
        },
    )?;
    vault.claim_count += 1;
    said.say(ClaimBooked {
        claim_id: id,
        owner: r.owner,
        shares: r.amount,
    });
    Ok(id)
}

/// A claim the cash covered when it was tried, priced like any withdrawal.
/// If the cash no longer covers it, it waits to be tried again: nothing was
/// closed for it.
#[allow(clippy::too_many_arguments)]
fn queued_claim<'info>(
    vault: &mut Vault,
    vault_key: &Pubkey,
    r: &mut Request,
    id: u64,
    claim_info: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    purse: &Purse<'_, 'info>,
    author: &mut Holder,
    platform: &mut Holder,
    said: &mut Said,
) -> Result<()> {
    let mut claim =
        load_claim(claim_info, vault_key, r.reference)?.ok_or(VaultError::WrongAccount)?;
    if claim.state != ClaimState::Queued {
        return Ok(());
    }
    vault.queued_claims -= 1;
    let shares = claim.shares;
    let latest = vault.latest;
    let (slice, burned, payout) = exit_of(vault, &latest, shares)?;
    if payout > vault.cash {
        claim.state = ClaimState::Pending;
        said.say(ClaimTried {
            claim_id: r.reference,
            blocked: true,
        });
        return store(claim_info, &claim);
    }
    vault.pending_shares -= shares;
    vault.hand_slice(slice, author, platform, vault_key, said)?;
    vault.burn_held(burned, vault_key, said)?;
    vault.cash -= payout;
    vault.exit_paid += payout as u128;
    claim.state = ClaimState::Paid;
    claim.paid += payout;
    if !purse.pay(to, &claim.owner, payout, said)? {
        crate::claims::hold_on_claim(vault, &mut claim, payout)?;
    }
    said.say(Withdrawn {
        request_id: id,
        owner: claim.owner,
        shares,
        assets: payout,
        fee_shares: slice,
    });
    said.say(ClaimPaid {
        claim_id: r.reference,
        assets: payout,
    });
    store(claim_info, &claim)
}

/// The author's shares, redeemed into the tank like any withdrawal, from
/// cash only, and never as a claim: what the cash or the shares don't cover
/// is simply not filled.
fn fill(
    vault: &mut Vault,
    r: &mut Request,
    author: &mut Holder,
    platform: &mut Holder,
    said: &mut Said,
) -> Result<()> {
    let latest = vault.latest;
    let mut shares = r.amount;
    if r.reference == 1 {
        vault.tank.fill_pending = false;
        shares = shares_for(vault, &latest, r.amount, author.shares);
    }
    if shares > author.shares {
        shares = author.shares;
    }
    if shares == 0 {
        return Ok(());
    }
    let (mut slice, mut burned, mut payout) = exit_of(vault, &latest, shares)?;
    let available = vault.cash;
    if payout > available {
        shares = mul_div(shares, available as u128, payout as u128)?;
        (slice, burned, payout) = exit_of(vault, &latest, shares)?;
        // The rescaled figure rounds on its own and can still land a unit
        // over; the shares then buy a unit less, never more cash than is
        // there.
        if payout > available {
            payout = available;
        }
    }
    if payout == 0 {
        return Ok(());
    }
    // The author's slice stays theirs; the platform's part moves; the rest
    // is burned.
    let total = vault.fees.author_bps as u128 + vault.fees.platform_bps as u128;
    let to_author = if slice == 0 {
        0
    } else {
        mul_div(slice, vault.fees.author_bps as u128, total)?
    };
    let to_platform = slice - to_author;
    author.shares = author
        .shares
        .checked_sub(to_platform)
        .ok_or(VaultError::Overflow)?;
    platform.shares = platform
        .shares
        .checked_add(to_platform)
        .ok_or(VaultError::Overflow)?;
    if to_platform != 0 {
        said.say(Transfer {
            from: author.owner,
            to: platform.owner,
            value: to_platform,
        });
    }
    vault.burn_from(author, burned, said)?;
    vault.cash -= payout;
    vault.exit_paid += payout as u128;
    vault.tank.balance = vault
        .tank
        .balance
        .checked_add(payout)
        .ok_or(VaultError::Overflow)?;
    said.say(TankFilled {
        shares,
        assets: payout,
    });
    Ok(())
}

/// How many shares pay `assets` at this valuation's exit price, after the
/// fee slice and the transaction fee, up to `most`. Counted whole and capped
/// before it is narrowed, so a fill priced at a pool worth next to nothing
/// takes every share it may rather than failing its settlement.
fn shares_for(vault: &Vault, v: &Snapshot, assets: u128, most: u128) -> u128 {
    let per_share = v.gross_exit - v.fee_exit;
    let keep = BPS - vault.fees.transaction_bps as u128;
    if per_share == 0 || keep == 0 {
        return 0;
    }
    let product = U256::from(assets) * U256::from(v.supply) * U256::from(BPS);
    let per = U256::from(per_share) * U256::from(keep);
    let mut quotient = product / per;
    if !(product % per).is_zero() {
        quotient += U256::one();
    }
    if quotient > U256::from(most) {
        return most;
    }
    quotient.as_u128()
}

// The pro-rata fallback ----------------------------------------------------------------------

/// Whether the station could be valued now inside the band. A station that
/// can't be valued at all fails this call, and with it the transaction: on
/// Solana a failing call can't be caught.
fn priceable<'info>(
    program: &AccountInfo<'info>,
    station_info: &AccountInfo<'info>,
    groups: &[AccountInfo<'info>],
    sizes: Vec<u8>,
) -> Result<bool> {
    let valued = station::value(program, station_info, groups, sizes)?;
    Ok(!valued.outside)
}

/// A request nothing has priced for `fallback_after`. A redemption takes the
/// cash's share now and holds units for the rest of what the station holds;
/// a deposit, whose money never left, takes it back.
pub fn take_pro_rata<'info>(
    ctx: Context<'_, '_, '_, 'info, TakeProRata<'info>>,
    request_id: u64,
    groups: Vec<u8>,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.pool.vault.key();
    let now = now()?;
    let mut r = load_request(&a.request, &vault_key, request_id)?.ok_or(VaultError::NotAllowed)?;
    let deposit = r.kind == RequestKind::Deposit;
    require!(
        (deposit || r.kind == RequestKind::Redeem) && !r.done && a.owner.key() == r.owner,
        VaultError::NotAllowed
    );
    // Said through the program's own instruction data: a deposit's asks every
    // venue first, and what they log counts against the logs' cap.
    let mut said = Said::kept();
    let authority = a.event_authority.to_account_info();
    let bump = ctx.bumps.event_authority;
    a.pool.vault.sync_arrivals(&a.pool.station, &mut said)?;
    let vault = &mut a.pool.vault;
    // Nor while a priced valuation waits to settle: its withdrawals are paid
    // from the cash first.
    require!(
        now >= r.earliest.saturating_add(vault.timers.fallback_after) && !vault.owed(),
        VaultError::NotYet
    );
    // A closing vault pays its holders through its own door.
    require!(deposit || !vault.ready_to_close(), VaultError::WrongState);
    // Not while a valuation could price it: the fallback is for when nothing
    // can, not a way round the fee.
    require!(
        !(vault.priced && vault.latest.time >= r.earliest),
        VaultError::NotAllowed
    );
    // A deposit goes back only when a valuation now would price nothing
    // either: otherwise its owner could record one, see the price, and choose
    // between buying and taking the money back. A vault that no longer takes
    // deposits would send it back at settlement anyway; and a vault nobody
    // could value for as long as an unpaid claim takes to freeze one (a
    // driver that fails, which on Solana fails the asking transaction too)
    // gives it back without asking.
    if deposit
        && vault.state == VaultState::Open
        && now < r.earliest.saturating_add(vault.timers.freeze_after)
    {
        let can = priceable(
            &a.station_program,
            &a.pool.station,
            ctx.remaining_accounts,
            groups,
        )?;
        require!(!can, VaultError::NotAllowed);
    }
    let vault = &mut a.pool.vault;
    r.done = true;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let purse = Purse {
        vault: vault.to_account_info(),
        dollars: a.pool.vault_dollars.to_account_info(),
        mint: a.pool.mint.to_account_info(),
        token_program: a.pool.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    };
    let owner = r.owner;
    let author = &mut a.pool.author_holder;
    let platform = &mut a.pool.platform_holder;
    let subject = subject_of(&a.holder, &vault_key, &owner, author, platform)?;
    let mut holder = subject.copy(&vault_key, &owner, author, platform);
    let payer = Some((a.owner.as_ref(), a.system_program.as_ref()));
    if deposit {
        vault.escrow -= r.amount as u64;
        // The deposit's receiver is the owner, whose holding counts it.
        holder.pending = holder.pending.saturating_sub(1);
        subject.put_back(&holder, &a.holder, author, platform, payer)?;
        refund(vault, &mut r, request_id, &a.to_dollars, &purse, &mut said)?;
        store(&a.request, &r)?;
        return said.emit(&authority, bump);
    }
    vault.pending_shares -= r.amount;
    let assets = pro_rata(
        vault,
        &vault_key,
        &mut holder,
        author,
        platform,
        r.amount,
        now,
        &mut said,
    )?;
    subject.put_back(&holder, &a.holder, author, platform, payer)?;
    if !purse.pay(&a.to_dollars, &owner, assets, &mut said)? {
        hold_on(vault, &mut r, owner, assets)?;
    }
    store(&a.request, &r)?;
    said.emit(&authority, bump)
}

/// The fallback itself: no price is needed. The leaver's fee is their own
/// part of the fee owed at the last priced valuation, as a slice of their
/// shares; they are paid the rest's share of the cash, less the transaction
/// fee the pool keeps, and their units are a claim on their fraction of
/// whatever the station holds. Answers with what to pay them.
#[allow(clippy::too_many_arguments)]
pub fn pro_rata(
    vault: &mut Vault,
    vault_key: &Pubkey,
    owner: &mut Holder,
    author: &mut Holder,
    platform: &mut Holder,
    mut shares: u128,
    now: u64,
    said: &mut Said,
) -> Result<u64> {
    // The holding fee up to now falls on every share, the leaver's too.
    sync_holding(vault, vault_key, now, said)?;
    // And the profit fee the shares owed at the last priced valuation, as a
    // slice of them, as a withdrawal pays it: the fallback is a way out,
    // never a way round the fee.
    if vault.priced {
        let last = vault.latest;
        // The leaver's own part of the fee owed: with `p` the net price per
        // share, `m` the mark as the slot's deposits left it and `r` the fee
        // rate, the part of a share owed as fee is r (p - m) / (p - r m).
        let rate = vault.fees.author_bps as u128 + vault.fees.platform_bps as u128;
        let p = mul_div(last.gross_exit - last.fee_exit, PRICE, last.supply)?;
        let m = vault.mark;
        if p > m {
            let numerator = rate * (p - m);
            let denominator = BPS * p - rate * m;
            let mut slice = mul_div_up(shares, numerator, denominator)?;
            if slice > shares {
                slice = shares;
            }
            vault.hand_slice(slice, author, platform, vault_key, said)?;
            shares -= slice;
        }
    }
    let supply = vault.total_supply + OFFSET;
    let assets = mul_div(vault.cash as u128, shares, supply)?;
    // The shares own what the units don't; these shares' part of that is the
    // leaver's for good, whatever fees are minted later.
    let fraction = mul_div(shares, WAD - vault.live_units()?, supply)?;
    let raw = mul_div(fraction, WAD, vault.scale())?;
    // The transaction fee stays in the pool, as for every other exit.
    let kept = mul_div_up(assets, vault.fees.transaction_bps as u128, BPS)?;
    vault.burn_held(shares, vault_key, said)?;
    let paid = dollars(assets - kept)?;
    vault.cash -= paid;
    if raw != 0 {
        accrue(vault, owner)?;
        if owner.units == 0 {
            owner.units_since = now;
        }
        owner.units += raw;
        vault.units += raw;
        reset_debt(vault, owner)?;
    }
    said.say(ProRataTaken {
        owner: owner.owner,
        shares,
        assets: paid,
        units: fraction,
    });
    Ok(paid)
}

/// The holder's units' share of what came home, to any account of theirs.
pub fn collect_units<'info>(ctx: Context<'_, '_, '_, 'info, CollectUnits<'info>>) -> Result<()> {
    let a = ctx.accounts;
    let vault = &mut a.vault;
    accrue(vault, &mut a.holder)?;
    let assets = a.holder.unit_credit;
    require!(assets != 0, VaultError::Nothing);
    own_account(&a.to, &a.owner.key(), &vault.mint)?;
    a.holder.unit_credit = 0;
    vault.unit_reserve -= assets;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    Purse {
        vault: vault.to_account_info(),
        dollars: a.vault_dollars.to_account_info(),
        mint: a.mint.to_account_info(),
        token_program: a.token_program.to_account_info(),
        decimals: DECIMALS,
        seeds: &seeds,
    }
    .send(assets, &a.to)?;
    emit!(UnitsPaid {
        owner: a.owner.key(),
        assets,
    });
    Ok(())
}

/// Closes exactly one holder's fraction of every position and gives them all
/// of what it brings home. What remains is owned by everyone else in the
/// same proportions as before. Anyone may: the vault trades on its own chain
/// only.
pub fn serve_pro_rata<'info>(
    ctx: Context<'_, '_, 'info, 'info, ServeProRata<'info>>,
    owner: Pubkey,
    groups: Vec<u8>,
) -> Result<()> {
    let a = ctx.accounts;
    let vault_key = a.pool.vault.key();
    let subject = subject_of(
        &a.holder,
        &vault_key,
        &owner,
        &a.pool.author_holder,
        &a.pool.platform_holder,
    )?;
    let mut holder = subject.copy(
        &vault_key,
        &owner,
        &a.pool.author_holder,
        &a.pool.platform_holder,
    );
    let raw = holder.units;
    require!(raw != 0, VaultError::Nothing);
    // Not between a valuation's pricing and its settlement, as claims.
    require!(!a.pool.vault.owed(), VaultError::NotYet);
    // Said through the program's own instruction data: the close calls every
    // venue first, and what they log counts against the logs' cap.
    let mut said = Said::kept();
    a.pool.vault.sync_arrivals(&a.pool.station, &mut said)?;
    let vault = &mut a.pool.vault;
    let mut fraction = mul_div(raw, vault.scale(), WAD)?;
    if fraction > WAD {
        fraction = WAD;
    }
    if fraction == 0 {
        fraction = 1;
    }
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let vault_info = vault.to_account_info();
    let call = station::Station {
        program: &a.close.station_program,
        station: &a.pool.station,
        vault: &vault_info,
        seeds: &seeds,
    };
    let outcome = call.unwind(
        &a.close.station_dollars,
        &a.pool.vault_dollars,
        &a.pool.mint,
        &a.pool.token_program,
        ctx.remaining_accounts,
        groups.clone(),
        fraction as u64,
    )?;
    let vault = &mut a.pool.vault;
    // Everything the close sent home is the holder's.
    let home = vault.arrived(&a.pool.station)?;
    // Units are only cleared for what came home for them. A serve the station
    // refused (one that would pay the holder nothing at exit prices, say)
    // failed the whole transaction and left them waiting. One that went
    // through and brought nothing home clears them only if they are dust:
    // judged at the mark, on what the station holds now and as the last
    // valuation counted it, whichever is more. Once the vault has closed the
    // station was found empty, and only what it holds now counts.
    if outcome.blocked {
        return err!(VaultError::NotYet);
    }
    if home == 0 {
        let now_mark = station::value(
            &a.close.station_program,
            &a.pool.station,
            ctx.remaining_accounts,
            groups,
        )?
        .mark as u128;
        let mut stations = now_mark;
        if vault.priced && vault.state != VaultState::Closed {
            let counted = vault.latest.gross - 1 - vault.latest.cash as u128;
            stations = stations.max(counted);
        }
        let worth_something = mul_div(stations, fraction, WAD)? > vault.dust();
        require!(!worth_something, VaultError::NotYet);
    }
    accrue(vault, &mut holder)?;
    holder.unit_credit = holder
        .unit_credit
        .checked_add(home)
        .ok_or(VaultError::Overflow)?;
    vault.unit_reserve = vault
        .unit_reserve
        .checked_add(home)
        .ok_or(VaultError::Overflow)?;
    said.say(FromStation {
        assets: home,
        to_units: home,
    });
    vault.units -= raw;
    holder.units = 0;
    holder.units_since = 0;
    // What is left at the station is everyone else's, in the same
    // proportions as before: every remaining fraction grows to match.
    if vault.units == 0 {
        vault.unit_scale = 0;
    } else if fraction < WAD {
        vault.unit_scale = mul_div(vault.scale(), WAD, WAD - fraction)?;
    }
    holder.unit_debt = 0;
    subject.put_back(
        &holder,
        &a.holder,
        &mut a.pool.author_holder,
        &mut a.pool.platform_holder,
        None,
    )?;
    said.emit(
        &a.event_authority.to_account_info(),
        ctx.bumps.event_authority,
    )
}

// The station -----------------------------------------------------------------------------------

/// Checks `key` is the executor key the station has approved, and still
/// blessed as this vault's executor.
fn approved_key(a: &Move) -> Result<()> {
    let station = {
        let data = a.station.try_borrow_data()?;
        shonin_station::state::Station::try_deserialize(&mut &data[..])?
    };
    require_keys_eq!(
        *a.station.owner,
        shonin_station::ID,
        VaultError::WrongAccount
    );
    let key = a.key.key();
    require!(
        key != Pubkey::default() && station.drivers.iter().any(|d| d.key == key),
        VaultError::NotBlessed
    );
    crate::upkeep::require_executor(
        &a.vault,
        &key,
        [
            &a.registry_config,
            &a.blessing,
            &a.key_info,
            &a.image,
            &a.project,
        ],
    )
}

/// The approved executor sends cash to the station, never below the idle
/// floor. Nothing goes out while the vault is stopping, while unit holders
/// are owed part of what is out, while a queued claim's cash waits to be
/// priced, or between a valuation and its settlement.
pub fn to_station<'info>(ctx: Context<'_, '_, '_, 'info, Move<'info>>, amount: u64) -> Result<()> {
    approved_key(ctx.accounts)?;
    let a = ctx.accounts;
    a.vault.sync_arrivals(&a.station, &mut Said::logs())?;
    let vault = &mut a.vault;
    require!(
        vault.live()
            && !vault.halted()
            && vault.units == 0
            && vault.queued_claims == 0
            && !vault.owed(),
        VaultError::WrongState
    );
    // The station as last valued, the cash as it is now, and what was sent
    // out since: money that joined or left counts, and a run of pulls can't
    // shrink the value the floor is measured on.
    let mut value = vault.cash as u128 + vault.sent_total + 1;
    if vault.priced {
        value = value + (vault.latest.gross - 1 - vault.latest.cash as u128) - vault.latest.sent;
    }
    let floor = mul_div_up(value, vault.floor_now(now()?) as u128, BPS)?;
    require!(
        amount <= vault.cash && (vault.cash - amount) as u128 >= floor,
        VaultError::BelowFloor
    );
    vault.cash -= amount;
    vault.sent_total += amount as u128;
    let seeds = vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let vault_info = vault.to_account_info();
    station::Station {
        program: &a.station_program,
        station: &a.station,
        vault: &vault_info,
        seeds: &seeds,
    }
    .pull(
        &a.vault_dollars,
        &a.station_dollars,
        &a.mint,
        &a.token_program,
        amount,
    )?;
    emit!(ToStation { assets: amount });
    Ok(())
}

/// The approved executor has the station send counted dollars home.
pub fn from_station<'info>(
    ctx: Context<'_, '_, '_, 'info, Move<'info>>,
    amount: u64,
) -> Result<()> {
    approved_key(ctx.accounts)?;
    let a = ctx.accounts;
    a.vault.sync_arrivals(&a.station, &mut Said::logs())?;
    let seeds = a.vault.seeds();
    let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let vault_info = a.vault.to_account_info();
    station::Station {
        program: &a.station_program,
        station: &a.station,
        vault: &vault_info,
        seeds: &seeds,
    }
    .push(
        &a.vault_dollars,
        &a.station_dollars,
        &a.mint,
        &a.token_program,
        amount,
    )?;
    a.vault.sync_arrivals(&a.station, &mut Said::logs())
}
