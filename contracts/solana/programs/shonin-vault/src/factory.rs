//! One vault per strategy, created when its author activates it, as the EVM
//! `VaultFactory`.
//!
//! Publishing and activating are separate on purpose: the author reads the
//! review before putting any money in. A strategy that was refused or failed
//! to build has no record, so it can never have a vault. Each vault sits at
//! an address fixed by its strategy id, so its author can name it before it
//! exists. Activation creates the vault, its station and every account the
//! vault keeps for life in one transaction, so no vault is ever left half
//! made: a Solana vault trades on its own chain only, and opens at once.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_lang::{Discriminator, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::{self, get_associated_token_address_with_program_id};
use shonin_common::{account::create_pda, digest::keccak, loader, time::now};
use shonin_prices::Allowances;
use shonin_registry::read;

use crate::codes;
use crate::contexts::*;
use crate::error::VaultError;
use crate::events::*;
use crate::ledger::store;
use crate::state::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializeArgs {
    pub admin: Pubkey,
    pub platform: Pubkey,
    /// This chain's name as `shonin.toml` writes it ("solana").
    pub chain_name: [u8; 32],
    pub terms: Terms,
    /// The vault program this one replaces; the default key for none.
    pub previous: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ActivateArgs {
    pub strategy_id: u64,
    pub allowances: Allowances,
    pub idle_floor_bps: u16,
    pub tank_funding: u64,
    pub tank_keep: u64,
    /// The hash of the vault's restricted list the author states they are not
    /// on: they hold fee shares, so they state it like every other holder.
    pub statement: [u8; 32],
    /// The hash of the factory's terms the author read.
    pub terms: [u8; 32],
    /// How many of the remaining accounts each driver takes in the station's
    /// `create` (after the factories this one replaces).
    pub driver_accounts: Vec<u8>,
    /// How many factories this one replaces are named, two accounts each:
    /// that program's factory settings and its vault address for this
    /// strategy.
    pub previous: u8,
}

/// The hash an author passes to `activate`: keccak256 of the terms' Borsh
/// encoding.
pub fn terms_hash(terms: &Terms) -> [u8; 32] {
    keccak(&[&terms.try_to_vec().unwrap_or_default()])
}

/// Every term within its floor and ceiling, as the EVM factory checks them,
/// on every change. A Solana vault has no money in transit, no post deadline
/// and no activating state, so those timers are absent; and its slot lead must
/// be shorter than a slot (see [`Vault::priced_by`]), which a lead of a slot
/// or more would only delay every request for, without strengthening forward
/// pricing.
pub fn checked(terms: &Terms) -> Result<()> {
    let t = &terms.timers;
    let f = &terms.fees;
    let day = shonin_common::DAY;
    let too_long = [
        t.slot_interval,
        t.fallback_after,
        t.usage_freshness,
        t.tank_escape_delay,
        t.floor_delay,
        t.halt_grace,
        t.freeze_after,
        t.wind_down,
    ]
    .iter()
    .any(|timer| *timer > MAX_TIMER);
    let bad = f.holding_bps > MAX_HOLDING_BPS
        || f.transaction_bps > MAX_TRANSACTION_BPS
        || terms.exit_band_bps > MAX_EXIT_BAND_BPS
        || f.author_bps as u32 + f.platform_bps as u32 > MAX_PROFIT_BPS as u32
        || terms.dead_assets == 0
        || terms.minimum_tank <= terms.dead_assets
        || terms.minimum_deposit == 0
        || terms.minimum_deposit > MAX_MINIMUM_DEPOSIT
        || t.slot_interval == 0
        || t.slot_window >= t.slot_interval
        || t.slot_lead <= t.slot_window
        || t.slot_lead >= t.slot_interval
        || t.freeze_after < day
        || t.fallback_after < day
        || t.usage_freshness == 0
        || t.usage_freshness > day
        || t.tank_escape_delay < day
        || t.floor_delay < day
        || t.halt_grace < 10 * 60
        || t.wind_down < 7 * day
        || t.freeze_after <= t.fallback_after
        || too_long;
    require!(!bad, VaultError::OutOfRange);
    Ok(())
}

/// Sets a fresh deployment up, once, signed by the key that deployed it.
pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
    require!(
        loader::is_upgrade_authority(
            &ctx.accounts.program,
            Some(&ctx.accounts.program_data),
            ctx.accounts.deployer.key,
        ),
        VaultError::NotTheDeployer
    );
    require!(
        ctx.accounts.mint.decimals == DECIMALS
            && ctx.accounts.mint.to_account_info().owner == &anchor_spl::token::ID,
        VaultError::WrongMint
    );
    checked(&args.terms)?;
    let cluster = read::config(&ctx.accounts.registry_config)?.cluster;
    ctx.accounts.factory.set_inner(Factory {
        admin: args.admin,
        platform: args.platform,
        mint: ctx.accounts.mint.key(),
        token_program: anchor_spl::token::ID,
        cluster,
        chain_name: args.chain_name,
        registry: shonin_registry::ID,
        publishing: shonin_publishing::ID,
        prices: shonin_prices::ID,
        station_program: shonin_station::ID,
        previous: args.previous,
        terms: args.terms,
        vault_count: 0,
        bump: ctx.bumps.factory,
    });
    emit!(TermsSet { terms: args.terms });
    Ok(())
}

/// The admin changes the terms new vaults are created with; no vault that
/// exists is touched.
pub fn set_terms(ctx: Context<SetTerms>, terms: Terms) -> Result<()> {
    checked(&terms)?;
    ctx.accounts.factory.terms = terms;
    emit!(TermsSet { terms });
    Ok(())
}

/// Whether strategy `id` already has a vault from a factory this one
/// replaces, or from any before it: `accounts` are, per factory, its program's
/// settings and its vault address for the strategy, newest first, until a
/// factory that replaces none.
fn has_earlier_vault(previous: Pubkey, id: u64, accounts: &[AccountInfo]) -> Result<bool> {
    let mut program = previous;
    let mut pairs = accounts.chunks(2);
    for _ in 0..MAX_PREVIOUS {
        if program == Pubkey::default() {
            return Ok(false);
        }
        let [config, vault] = pairs.next().ok_or(VaultError::WrongAccount)? else {
            return err!(VaultError::WrongAccount);
        };
        let (config_address, _) = Pubkey::find_program_address(&[FACTORY_SEED], &program);
        require_keys_eq!(*config.key, config_address, VaultError::WrongAccount);
        require_keys_eq!(
            *vault.key,
            vault_address_of(&program, id).0,
            VaultError::WrongAccount
        );
        if vault.owner == &program && !vault.data_is_empty() {
            return Ok(true);
        }
        require_keys_eq!(*config.owner, program, VaultError::WrongAccount);
        let data = config.try_borrow_data()?;
        require!(
            data.len() >= 8 && data[..8] == *Factory::DISCRIMINATOR,
            VaultError::WrongAccount
        );
        program = Factory::try_deserialize(&mut &data[..])?.previous;
    }
    require!(program == Pubkey::default(), VaultError::WrongAccount);
    Ok(false)
}

/// The strategy's author only, once it is registered and reviewed: creates its
/// vault and its station, on the terms the author read. The remaining
/// accounts are the factories this one replaces (see `has_earlier_vault`),
/// then each driver's accounts for the station's `create`.
pub fn activate<'info>(
    ctx: Context<'_, '_, 'info, 'info, Activate<'info>>,
    args: ActivateArgs,
) -> Result<()> {
    let a = &ctx.accounts;
    let factory = &a.factory;
    let terms = factory.terms;
    // The author is held to what they read: a change of terms between reading
    // and activating stops the activation.
    require!(args.terms == terms_hash(&terms), VaultError::TermsChanged);

    // The record and its review, read from publishing.
    let strategy_address = shonin_publishing::state::strategy_address(args.strategy_id);
    require_keys_eq!(a.strategy.key(), strategy_address, VaultError::NotReady);
    require!(
        a.strategy.owner == &shonin_publishing::ID && !a.strategy.data_is_empty(),
        VaultError::NotReady
    );
    let record = {
        let data = a.strategy.try_borrow_data()?;
        shonin_publishing::state::Strategy::try_deserialize(&mut &data[..])?
    };
    let review_address = Pubkey::find_program_address(
        &[
            shonin_publishing::state::REVIEW_SEED,
            &args.strategy_id.to_le_bytes(),
        ],
        &shonin_publishing::ID,
    )
    .0;
    require_keys_eq!(a.review.key(), review_address, VaultError::NotReady);
    let reviewed = a.review.owner == &shonin_publishing::ID && !a.review.data_is_empty() && {
        let data = a.review.try_borrow_data()?;
        shonin_publishing::state::Review::try_deserialize(&mut &data[..])?.is_review()
    };
    require!(record.registered_at != 0 && reviewed, VaultError::NotReady);
    require_keys_eq!(a.author.key(), record.author, VaultError::NotTheAuthor);
    // The author's and the platform's holdings are separate accounts.
    require_keys_neq!(a.author.key(), factory.platform, VaultError::NotAllowed);
    require!(
        args.tank_funding >= terms.minimum_tank,
        VaultError::TankTooSmall
    );
    require!(args.idle_floor_bps as u128 <= BPS, VaultError::OutOfRange);
    require!(args.tank_keep <= MAX_TANK_KEEP, VaultError::OutOfRange);

    // This chain's drivers; a record naming another chain can't be activated.
    let mut drivers: Vec<[u8; 32]> = Vec::new();
    for (i, need) in record.lists.chains.iter().enumerate() {
        require!(
            !record.lists.chains[..i]
                .iter()
                .any(|other| other.chain == need.chain),
            VaultError::ChainTwice
        );
        require!(
            need.chain == factory.chain_name,
            VaultError::OtherChainsNotYet
        );
        drivers = need.drivers.clone();
    }

    let previous = args.previous as usize * 2;
    let (earlier, groups) = ctx
        .remaining_accounts
        .split_at_checked(previous)
        .ok_or(VaultError::WrongAccount)?;
    require!(
        !has_earlier_vault(factory.previous, args.strategy_id, earlier)?,
        VaultError::VaultExists
    );
    require!(
        a.vault.data_is_empty() && a.vault.owner == &anchor_lang::system_program::ID,
        VaultError::VaultExists
    );

    let vault_key = a.vault.key();
    let vault_bump = ctx.bumps.vault;
    let id = args.strategy_id.to_le_bytes();
    let vault_seeds: &[&[u8]] = &[VAULT_SEED, &id, &[vault_bump]];
    let author = a.author.to_account_info();
    let system = a.system_program.to_account_info();

    // The vault's account first: the station's creation needs its signature.
    create_pda(
        &author,
        &a.vault,
        &system,
        vault_seeds,
        8 + Vault::INIT_SPACE,
        &crate::ID,
    )?;
    let dollars = get_associated_token_address_with_program_id(
        &vault_key,
        &factory.mint,
        &anchor_spl::token::ID,
    );
    require_keys_eq!(a.vault_dollars.key(), dollars, VaultError::WrongAccount);
    associated_token::create_idempotent(CpiContext::new(
        a.associated_token_program.to_account_info(),
        associated_token::Create {
            payer: author.clone(),
            associated_token: a.vault_dollars.to_account_info(),
            authority: a.vault.to_account_info(),
            mint: a.mint.to_account_info(),
            system_program: system.clone(),
            token_program: a.token_program.to_account_info(),
        },
    ))?;

    // The station, pinning this chain's drivers for good.
    let station_key = shonin_station::station_address(&vault_key);
    require_keys_eq!(a.station.key(), station_key, VaultError::WrongAccount);
    create_station(
        a,
        groups,
        &drivers,
        record.bundle_hash,
        terms.exit_band_bps,
        args.driver_accounts.clone(),
        vault_seeds,
    )?;

    // Who the vault refuses: US and sanctioned persons, the record's list
    // (every driver's list on every chain with the author's additions), and
    // the drivers the station pinned in this same transaction, as they stand.
    let pinned = pinned_programs(&a.station)?;
    let mut lists = Vec::with_capacity(pinned.len());
    for program in &pinned {
        let info = groups
            .iter()
            .find(|info| info.key == program)
            .ok_or(VaultError::WrongAccount)?;
        lists.push(crate::station::restricted_of(info)?);
    }
    let list = codes::merged(&record.lists.restricted, lists.iter().map(Vec::as_slice));
    require!(list.len() <= MAX_LIST, VaultError::ListFull);
    let restricted_hash = codes::hash(&list);
    let restricted_bump = ctx.bumps.restricted;
    create_pda(
        &author,
        &a.restricted,
        &system,
        &[RESTRICTED_SEED, vault_key.as_ref(), &[restricted_bump]],
        Restricted::space(list.len()),
        &crate::ID,
    )?;
    store(
        &a.restricted,
        &Restricted {
            vault: vault_key,
            bump: restricted_bump,
            codes: list,
        },
    )?;

    // The author's and the platform's holdings, which fees are paid into. The
    // author is the first holder, and states like every other.
    require!(args.statement == restricted_hash, VaultError::NoStatement);
    let author_key = a.author.key();
    for (holder, owner, bump, statement) in [
        (
            &a.author_holder,
            author_key,
            ctx.bumps.author_holder,
            restricted_hash,
        ),
        (
            &a.platform_holder,
            factory.platform,
            ctx.bumps.platform_holder,
            [0u8; 32],
        ),
    ] {
        create_pda(
            &author,
            holder,
            &system,
            &[HOLDER_SEED, vault_key.as_ref(), owner.as_ref(), &[bump]],
            8 + Holder::INIT_SPACE,
            &crate::ID,
        )?;
        store(
            holder,
            &Holder {
                vault: vault_key,
                owner,
                shares: 0,
                statement,
                units: 0,
                unit_debt: 0,
                unit_credit: 0,
                units_since: 0,
                pending: 0,
                payer: author_key,
                bump,
            },
        )?;
    }
    emit!(StatementRecorded {
        depositor: author_key,
        restricted_hash,
    });

    // The tank. The dead shares come out of it: a cent nobody can ever
    // redeem, so the first depositor can't be rounded down by a donation.
    anchor_spl::token::transfer_checked(
        CpiContext::new(
            a.token_program.to_account_info(),
            anchor_spl::token::TransferChecked {
                from: a.author_dollars.to_account_info(),
                mint: a.mint.to_account_info(),
                to: a.vault_dollars.to_account_info(),
                authority: author.clone(),
            },
        ),
        args.tank_funding,
        DECIMALS,
    )?;
    let now = now()?;
    let dead = terms.dead_assets;
    let dead_shares = dead as u128 * OFFSET;
    let vault = Vault {
        strategy_id: args.strategy_id,
        bundle_hash: record.bundle_hash,
        author: author_key,
        admin: factory.admin,
        platform: factory.platform,
        mint: factory.mint,
        dollars,
        station: station_key,
        cluster: factory.cluster,
        created_at: now,
        exit_band_bps: terms.exit_band_bps,
        fees: terms.fees,
        timers: terms.timers,
        minimum_deposit: terms.minimum_deposit,
        drivers: drivers.clone(),
        bump: vault_bump,
        state: VaultState::Open,
        halted_at: 0,
        retired_at: 0,
        halt_reason: HaltReason::None,
        total_supply: dead_shares,
        pending_shares: 0,
        unpaid_holding: 0,
        dead_shares,
        cash: dead,
        escrow: 0,
        mark: WAD,
        last_holding: now,
        last_payment: now,
        tank: Tank {
            balance: args.tank_funding - dead,
            keep_at: args.tank_keep,
            last_collection: now,
            ..Tank::default()
        },
        allowances: args.allowances.into(),
        allowances_since: now,
        floor_bps: args.idle_floor_bps,
        pending_floor_bps: 0,
        pending_floor_at: 0,
        request_count: 0,
        request_cursor: 0,
        exit_cursor: 0,
        priced_through: 0,
        tail_earliest: 0,
        tail_start: 0,
        previous_earliest: 0,
        previous_start: 0,
        valuation_count: 0,
        last_valued_at: 0,
        priced: false,
        latest: Snapshot::default(),
        exit_paid: 0,
        deposit_net: 0,
        deposit_supply: 0,
        deposits_for: 0,
        claim_count: 0,
        queued_claims: 0,
        units: 0,
        unit_scale: 0,
        unit_reserve: 0,
        per_unit: [0; 32],
        sent_total: 0,
        station_booked: 0,
        restricted_hash,
        held_total: 0,
        held_author: 0,
        held_platform: 0,
    };
    store(&a.vault, &vault)?;
    emit!(Transfer {
        from: Pubkey::default(),
        to: DEAD,
        value: dead_shares,
    });
    emit!(TankFunded {
        from: author_key,
        assets: args.tank_funding - dead,
    });
    emit!(StateChanged {
        state: VaultState::Open,
    });
    emit!(Activated {
        strategy_id: args.strategy_id,
        author: author_key,
        vault: vault_key,
        station: station_key,
        tank_funding: args.tank_funding,
    });
    let factory = &mut ctx.accounts.factory;
    factory.vault_count = factory
        .vault_count
        .checked_add(1)
        .ok_or(VaultError::Overflow)?;
    Ok(())
}

/// Where the dead shares are shown to go: an address nobody holds.
pub const DEAD: Pubkey = Pubkey::new_from_array([0xde; 32]);

/// The station program's `create`, signed by the vault.
fn create_station<'info>(
    a: &Activate<'info>,
    groups: &[AccountInfo<'info>],
    drivers: &[[u8; 32]],
    bundle_hash: [u8; 32],
    exit_band_bps: u16,
    sizes: Vec<u8>,
    vault_seeds: &[&[u8]],
) -> Result<()> {
    let mut accounts = shonin_station::accounts::Create {
        payer: a.author.key(),
        vault: a.vault.key(),
        vault_dollars: a.vault_dollars.key(),
        config: a.station_config.key(),
        station: a.station.key(),
        station_dollars: a.station_dollars.key(),
        mint: a.mint.key(),
        token_program: a.token_program.key(),
        associated_token_program: a.associated_token_program.key(),
        system_program: a.system_program.key(),
    }
    .to_account_metas(None);
    for meta in accounts.iter_mut() {
        meta.is_signer = meta.pubkey == a.vault.key() || meta.pubkey == a.author.key();
    }
    accounts.extend(groups.iter().map(|info| AccountMeta {
        pubkey: *info.key,
        is_signer: false,
        is_writable: info.is_writable,
    }));
    let ix = Instruction {
        program_id: shonin_station::ID,
        accounts,
        data: shonin_station::instruction::Create {
            args: shonin_station::CreateArgs {
                bundle_hash,
                exit_band_bps,
                drivers: drivers.to_vec(),
                driver_accounts: sizes,
                vault_seeds: vault_seeds.iter().map(|seed| seed.to_vec()).collect(),
            },
        }
        .data(),
    };
    let mut infos = vec![
        a.author.to_account_info(),
        a.vault.to_account_info(),
        a.vault_dollars.to_account_info(),
        a.station_config.to_account_info(),
        a.station.to_account_info(),
        a.station_dollars.to_account_info(),
        a.mint.to_account_info(),
        a.token_program.to_account_info(),
        a.associated_token_program.to_account_info(),
        a.system_program.to_account_info(),
    ];
    infos.extend(groups.iter().cloned());
    infos.push(a.station_program.to_account_info());
    invoke_signed(&ix, &infos, &[vault_seeds])?;
    Ok(())
}

/// The driver programs a station pinned, read from its record.
fn pinned_programs(station: &AccountInfo) -> Result<Vec<Pubkey>> {
    require_keys_eq!(*station.owner, shonin_station::ID, VaultError::WrongAccount);
    let data = station.try_borrow_data()?;
    let station = shonin_station::state::Station::try_deserialize(&mut &data[..])?;
    Ok(station.drivers.iter().map(|d| d.program).collect())
}
