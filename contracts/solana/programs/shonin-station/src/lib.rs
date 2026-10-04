//! A vault's station on Solana: the address that holds the vault's money at
//! its venues, and the guard over every call made from it.
//!
//! A station is a program-derived address of this program, one per vault
//! (seeds `["station", vault]`), so only this program can sign for it. It owns
//! its dollar account, where the vault's money arrives and from where it goes
//! home, and it never signs a venue call.
//!
//! At each of its venues the station has an authority of its own, another
//! program-derived address of this program (seeds `["venue", station, venue
//! program]`), which is the master of its account at that venue and owns the
//! token accounts it holds there. The station makes every venue call itself,
//! signed by that venue's authority alone, after checking it against the
//! allowlist the venue's pinned driver gave when the station was created: the
//! venue program, the instruction, and the accounts that must be the
//! authority's own. So a venue's code, whatever it becomes, holds only its own
//! authority's signature, which no other venue's accounts and not the
//! station's dollar account accept: one venue can never reach another's money
//! or the station's. The executor's key reaches the venue only through
//! [`execute`], and a driver only prepares instructions as data, which the
//! station checks the same way; no driver is ever handed a signature.
//!
//! Dollars move between the station's dollar account and a venue's only by
//! the station's own transfers, each measured: into the venue authority's
//! dollar account for an instruction that trades, as much as the executor
//! asks of what the station has counted, and back out of it after every
//! call, all that is there beyond what someone sent it unasked. Between calls
//! that account holds nothing that counts, so a station is worth its counted
//! dollars and what its drivers value at the venues, as before.
//!
//! It pins its drivers when it is created: the implementation the registry
//! approves then, checked final, and that implementation's allowlist. Nothing
//! anybody approves later reaches it, and nothing changes what it pinned.
//! Revoking a pinned driver stops trading through it and leaves valuing and
//! closing working.
//!
//! It counts only the dollars it has accounted for (what the vault sent it and
//! what came back from its venues), never its token account's balance, so a
//! stranger's transfer to it counts for nothing. And it keeps a running total
//! of every dollar it has sent home, whoever made it send, so the vault can
//! count those the same way.
//!
//! One driver per venue, and one executor at a time across all of them.
//!
//! This is the counterpart of the EVM `Station` and `StationFactory` for a
//! vault that trades on its own chain only. A station on another chain, its
//! bridges, the money it has in transit and the signed stop carried from the
//! vault's chain are deliberately absent: a Solana vault trades on Solana
//! alone in this release.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke, invoke_signed, set_return_data},
};
use anchor_spl::associated_token::{self, get_associated_token_address, AssociatedToken};
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};
use shonin_common::{account::create_pda, loader, time::now};
use shonin_driver::{
    discriminator, Allowlist, Class, Close, Plan, Valuation, BPS, MAX_MARKETS, MAX_RETURN_DATA, WAD,
};
use shonin_registry::read;
use shonin_registry::state::{ApprovalKind, ImageKind};

pub mod error;
pub mod events;
pub mod guard;
pub mod heap;
pub mod state;

#[cfg(all(
    target_os = "solana",
    feature = "custom-heap",
    not(feature = "no-entrypoint")
))]
#[global_allocator]
static HEAP: heap::Heap = heap::Heap;

use error::StationError;
use events::*;
use state::*;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

#[program]
pub mod shonin_station {
    use super::*;

    /// Sets a fresh deployment up, once, signed by the key that deployed it:
    /// which program's vaults may create stations, and the dollar. The
    /// cluster comes from the registry, which fixes it for good.
    pub fn initialize(ctx: Context<Initialize>, vault_program: Pubkey) -> Result<()> {
        require!(
            loader::is_upgrade_authority(
                &ctx.accounts.program,
                Some(&ctx.accounts.program_data),
                ctx.accounts.deployer.key,
            ),
            StationError::NotTheDeployer
        );
        let cluster = read::config(&ctx.accounts.registry_config)?.cluster;
        ctx.accounts.config.set_inner(Config {
            vault_program,
            mint: ctx.accounts.mint.key(),
            cluster,
            bump: ctx.bumps.config,
        });
        Ok(())
    }

    /// Creates the vault's station, signed by the vault: an address of the
    /// vault program, proved by `vault_seeds`, which only the vault program
    /// can sign for. So nobody else can create a vault's station, or create
    /// it first with other drivers.
    ///
    /// Each driver named is pinned for good: the implementation the registry
    /// approves for it now, which must be a program nobody can change, and
    /// the allowlist that implementation gives for its venue, with the
    /// station's authority at that venue. Its accounts come as remaining
    /// accounts, `driver_accounts[i]` of them for driver `i`: the program, its
    /// program-data account (any account for a program that has none), the
    /// registry's approval slot for the name, the revocation record of the
    /// implementation that slot approves now, the station's authority at the
    /// driver's venue and that authority's dollar account (created here), then
    /// anything the driver reads to give its allowlist.
    pub fn create<'info>(
        ctx: Context<'_, '_, 'info, 'info, Create<'info>>,
        args: CreateArgs,
    ) -> Result<()> {
        let config = &ctx.accounts.config;
        let vault = ctx.accounts.vault.key();
        let seeds: Vec<&[u8]> = args.vault_seeds.iter().map(Vec::as_slice).collect();
        let derived = Pubkey::create_program_address(&seeds, &config.vault_program)
            .map_err(|_| error!(StationError::NotAVault))?;
        require_keys_eq!(derived, vault, StationError::NotAVault);
        require!(
            args.exit_band_bps <= MAX_EXIT_BAND_BPS,
            StationError::BandTooWide
        );
        require!(
            args.drivers.len() <= MAX_DRIVERS,
            StationError::TooManyDrivers
        );
        // Each driver once: a name listed twice would count its venue twice
        // and close a fraction of it twice.
        for (i, name) in args.drivers.iter().enumerate() {
            require!(
                !args.drivers[i + 1..].contains(name),
                StationError::DuplicateDriver
            );
        }

        let now = now()?;
        let station_key = ctx.accounts.station.key();
        let groups = split(
            ctx.remaining_accounts,
            &args.driver_accounts,
            args.drivers.len(),
        )?;
        let mut drivers = Vec::with_capacity(args.drivers.len());
        let mut authorities = Vec::with_capacity(args.drivers.len());
        for (name, group) in args.drivers.iter().zip(&groups) {
            require!(group.len() >= 6, StationError::WrongDrivers);
            let (program, program_data, slot, revoked) =
                (&group[0], &group[1], &group[2], &group[3]);
            // Pin what the registry approves now, for good.
            let approved = read::trading(ApprovalKind::Driver, name, slot, revoked, now)?;
            require!(
                approved != Pubkey::default() && &approved == program.key,
                StationError::NotApproved
            );
            // The registry checked this when it was proposed; a station
            // checks again before trusting it for good.
            require!(
                loader::is_final(program, Some(program_data)),
                StationError::NotFinal
            );
            let mut asked = group[6..].to_vec();
            asked.push(program.clone());
            let allowlist: Allowlist =
                guard::ask(&asked, shonin_driver::data(discriminator::ALLOWLIST, &()))?;
            guard::validate(&allowlist, &config.vault_program)?;
            // One driver per venue: two drivers reading the same account
            // there would each count it, and value the station twice over.
            require!(
                !drivers.iter().any(|d: &Pinned| d.venue == allowlist.venue),
                StationError::SharedVenue
            );
            let (authority, authority_bump) = Pubkey::find_program_address(
                &[VENUE_SEED, station_key.as_ref(), allowlist.venue.as_ref()],
                &crate::ID,
            );
            let dollars = get_associated_token_address(&authority, &ctx.accounts.mint.key());
            require!(
                group[4].key == &authority && group[5].key == &dollars,
                StationError::WrongAuthority
            );
            authorities.push((&group[4], &group[5]));
            drivers.push(Pinned {
                name: *name,
                program: approved,
                venue: allowlist.venue,
                authority,
                authority_bump,
                dollars,
                key: Pubkey::default(),
                addresses: guard::station_addresses(&allowlist, &authority),
                rules: allowlist.rules,
                fixed: allowlist.fixed,
            });
        }
        // No driver's venue is one of the station's driver programs, its own
        // or another's: a driver only answers, and is never called with the
        // station's signature.
        require!(
            !drivers
                .iter()
                .any(|d| drivers.iter().any(|other| other.program == d.venue)),
            StationError::BadAllowlist
        );

        let mint = ctx.accounts.mint.key();
        let dollars = get_associated_token_address(&station_key, &mint);
        require_keys_eq!(
            ctx.accounts.station_dollars.key(),
            dollars,
            StationError::WrongDollars
        );
        let bump = ctx.bumps.station;
        let station = Station {
            vault,
            vault_program: config.vault_program,
            vault_dollars: ctx.accounts.vault_dollars.key(),
            dollars,
            mint,
            bundle_hash: args.bundle_hash,
            cluster: config.cluster,
            exit_band_bps: args.exit_band_bps,
            idle: 0,
            sent_home: 0,
            stopped: false,
            stopped_at: 0,
            payer: ctx.accounts.payer.key(),
            bump,
            drivers,
        };
        let mut bytes = Vec::new();
        station.try_serialize(&mut bytes)?;
        // Made at exactly its size, which never changes afterwards.
        create_pda(
            &ctx.accounts.payer,
            &ctx.accounts.station,
            &ctx.accounts.system_program,
            &[STATION_SEED, vault.as_ref(), &[bump]],
            bytes.len(),
            &crate::ID,
        )?;
        ctx.accounts
            .station
            .try_borrow_mut_data()?
            .copy_from_slice(&bytes);
        // Idempotent, so an account someone made for the station or one of
        // its authorities first, which can only be the right one, doesn't stop
        // it being created.
        let dollar_account = |owner: &AccountInfo<'info>, account: &AccountInfo<'info>| {
            associated_token::create_idempotent(CpiContext::new(
                ctx.accounts.associated_token_program.to_account_info(),
                associated_token::Create {
                    payer: ctx.accounts.payer.to_account_info(),
                    associated_token: account.clone(),
                    authority: owner.clone(),
                    mint: ctx.accounts.mint.to_account_info(),
                    system_program: ctx.accounts.system_program.to_account_info(),
                    token_program: ctx.accounts.token_program.to_account_info(),
                },
            ))
        };
        dollar_account(
            &ctx.accounts.station.to_account_info(),
            &ctx.accounts.station_dollars.to_account_info(),
        )?;
        for (authority, dollars) in authorities {
            dollar_account(authority, dollars)?;
        }
        emit!(StationCreated {
            vault,
            station: station_key,
            drivers: station.drivers.iter().map(|d| d.name).collect(),
            programs: station.drivers.iter().map(|d| d.program).collect(),
        });
        Ok(())
    }

    // Keys ------------------------------------------------------------------

    /// Lets `key` trade through `driver`. One executor at a time, across
    /// every driver: any other key approved through any of the station's
    /// drivers is withdrawn from all of them, so two executors never trade
    /// the same station at once. Anyone may send it; it obeys only the
    /// registry. While the key in place is still blessed, only the key being
    /// approved may take its place (it signs as `caller`), so nobody else can
    /// hand the station back and forth between two blessed instances during
    /// a handover; the key in place's records are then not needed. A revoked
    /// driver approves no key.
    pub fn approve_key(mut ctx: Context<ApproveKey>, driver: [u8; 32], key: Pubkey) -> Result<()> {
        let a = &mut ctx.accounts;
        let station = &mut a.station;
        require!(!station.stopped, StationError::AlreadyStopped);
        let i = station.driver(&driver).ok_or(StationError::NotDeclared)?;
        let now = now()?;
        let who = Identity::of(station);
        let records: [&AccountInfo; 5] = [
            &a.registry_config,
            &a.blessing,
            &a.key_info,
            &a.image,
            &a.project,
        ];
        require!(
            who.is_key(records, &key, ImageKind::Executor, now)?,
            StationError::NotBlessed
        );
        require!(
            !read::revoked(
                ApprovalKind::Driver,
                &driver,
                &station.drivers[i].program,
                &a.slot,
                &a.revoked,
                now,
            )?,
            StationError::NotApproved
        );
        // The key in place: the one approved through any driver, other than
        // `key` itself. Every approval withdraws all others, so there is never
        // more than one.
        let current = station
            .drivers
            .iter()
            .map(|pinned| pinned.key)
            .find(|held| *held != Pubkey::default() && *held != key);
        if let Some(current) = current {
            if a.caller.key() != key {
                let records: [Option<&AccountInfo>; 5] = [
                    Some(&a.registry_config),
                    a.current_blessing.as_deref(),
                    a.current_key_info.as_deref(),
                    a.current_image.as_deref(),
                    a.current_project.as_deref(),
                ];
                let [Some(c), Some(b), Some(k), Some(i), Some(p)] = records else {
                    return err!(StationError::MissingRecords);
                };
                require!(
                    !who.is_key([c, b, k, i, p], &current, ImageKind::Executor, now)?,
                    StationError::NotAllowed
                );
                // Only that key's records were read: any other still in place
                // could be blessed.
                require!(
                    station
                        .drivers
                        .iter()
                        .all(|pinned| { [Pubkey::default(), key, current].contains(&pinned.key) }),
                    StationError::NotAllowed
                );
            }
            for pinned in station.drivers.iter_mut() {
                if pinned.key == Pubkey::default() || pinned.key == key {
                    continue;
                }
                // On Solana a key exists only in the station's record, so
                // withdrawing it is a write that can't fail.
                emit!(KeyRevoked {
                    driver: pinned.name,
                    key: pinned.key,
                });
                pinned.key = Pubkey::default();
            }
        }
        let until = read::blessed_until(
            &a.blessing,
            &key,
            &who.cluster,
            &who.vault,
            &who.bundle_hash,
        )?;
        station.drivers[i].key = key;
        emit!(KeyApproved {
            driver,
            key,
            valid_until: until,
        });
        Ok(())
    }

    /// Withdraws the key approved for `driver` once its blessing has lapsed
    /// or been revoked, or its driver has been revoked, or the station has
    /// stopped. Anyone may call it: a passer-by can't cut off a key whose
    /// blessing stands, and can't keep one whose blessing has gone.
    pub fn revoke_key(mut ctx: Context<RevokeKey>, driver: [u8; 32], key: Pubkey) -> Result<()> {
        let a = &mut ctx.accounts;
        let station = &mut a.station;
        let i = station.driver(&driver).ok_or(StationError::NotDeclared)?;
        require!(
            key != Pubkey::default() && station.drivers[i].key == key,
            StationError::NotTheKey
        );
        let now = now()?;
        let who = Identity::of(station);
        if !station.stopped {
            let records: [&AccountInfo; 5] = [
                &a.registry_config,
                &a.blessing,
                &a.key_info,
                &a.image,
                &a.project,
            ];
            let blessed = who.is_key(records, &key, ImageKind::Executor, now)?;
            let revoked = read::revoked(
                ApprovalKind::Driver,
                &driver,
                &station.drivers[i].program,
                &a.slot,
                &a.revoked,
                now,
            )?;
            require!(!blessed || revoked, StationError::StillBlessed);
        }
        station.drivers[i].key = Pubkey::default();
        emit!(KeyRevoked { driver, key });
        Ok(())
    }

    // The guard -------------------------------------------------------------

    /// The approved key sends one venue instruction through the station,
    /// which checks it against `driver`'s pinned allowlist and signs it as its
    /// authority at that driver's venue, and as nothing else. Everything else
    /// is refused: another program, an instruction not on the list, a foreign
    /// account where the authority's own must stand, the authority's address
    /// anywhere a rule doesn't put it or writable anywhere, the station's own
    /// address or its own dollar account anywhere, another venue the station pins, any of its driver programs or a
    /// program no venue call needs named among the accounts, a key that isn't
    /// the approved one or is no longer blessed, a stopped station, and an
    /// instruction that would trade through a revoked driver. The remaining
    /// accounts are the venue program, then the instruction's accounts in
    /// order.
    ///
    /// Once its driver is revoked, an instruction that only brings money back
    /// (an exit) is any blessed executor's to send, approved or not: no key
    /// can be approved for a revoked driver, and anyone may withdraw the one
    /// that was, so its exits must never depend on an approval. Trading
    /// through it stays refused to every key.
    ///
    /// `fund` dollars of what the station has counted are moved to the
    /// authority's dollar account first, for an instruction that trades only:
    /// that is all of the station's money the venue's code is ever lent. After
    /// the call everything in that account beyond what was there before is
    /// moved back to the station's own and counted; the call may not take the
    /// account below what was there before, which nobody counted. And every
    /// token account of the authority's or the station's the instruction could
    /// write must come back still theirs alone: no delegate, no close
    /// authority, no other owner; and the authority itself still an account
    /// of the system program's with no data.
    pub fn execute<'info>(
        mut ctx: Context<'_, '_, 'info, 'info, Execute<'info>>,
        driver: u8,
        data: Vec<u8>,
        fund: u64,
    ) -> Result<()> {
        let a = &mut ctx.accounts;
        let station_info = a.station.to_account_info();
        let station_key = station_info.key();
        let main = Main::of(
            &a.station,
            station_info,
            a.station_dollars.to_account_info(),
            &a.mint,
            &a.token_program,
        );
        let station = &mut a.station;
        require!(!station.stopped, StationError::AlreadyStopped);
        let pinned = station
            .drivers
            .get(driver as usize)
            .ok_or(StationError::NotDeclared)?;
        require!(
            a.authority.key() == pinned.authority && a.authority_dollars.key() == pinned.dollars,
            StationError::WrongAuthority
        );
        let key = a.key.key();
        let now = now()?;

        let (program, accounts) = ctx
            .remaining_accounts
            .split_first()
            .ok_or(StationError::WrongProgram)?;
        let ix = Instruction {
            program_id: *program.key,
            accounts: accounts
                .iter()
                .map(|info| AccountMeta {
                    pubkey: *info.key,
                    is_signer: info.key == &pinned.authority || info.is_signer,
                    is_writable: info.is_writable,
                })
                .collect(),
            data,
        };
        let class = guard::check(
            &ix,
            pinned,
            &station.drivers,
            &station_key,
            &station.dollars,
            &station.vault_program,
            accounts,
        )?;
        let approved = pinned.key != Pubkey::default() && pinned.key == key;
        let revoked = read::revoked(
            ApprovalKind::Driver,
            &pinned.name,
            &pinned.program,
            &a.slot,
            &a.revoked,
            now,
        )?;
        match class {
            Class::Trade => {
                require!(approved, StationError::NotTheKey);
                require!(!revoked, StationError::NotApproved);
            }
            Class::Exit => {
                require!(approved || revoked, StationError::NotTheKey);
                require!(fund == 0, StationError::FundedExit);
            }
        }
        // Only counted dollars go to a venue, so a stranger's transfer to the
        // station never reaches one.
        require!(fund <= station.idle, StationError::Unaccounted);
        let name = pinned.name;
        let venue = Venue::of(pinned, &station_key);
        let who = Identity::of(station);
        let records: [&AccountInfo; 5] = [
            &a.registry_config,
            &a.blessing,
            &a.key_info,
            &a.image,
            &a.project,
        ];
        require!(
            who.is_key(records, &key, ImageKind::Executor, now)?,
            StationError::NotBlessed
        );
        let authority_dollars = a.authority_dollars.to_account_info();
        let mut infos = accounts.to_vec();
        infos.push(program.clone());
        let held = guard::TokenAccounts::before(&ix, accounts, &[&venue.authority, &station_key]);
        let unasked = guard::balance(&authority_dollars)?;
        if fund != 0 {
            main.send(fund, &authority_dollars)?;
        }
        invoke_signed(&ix, &infos, &[&venue.seeds()])?;
        held.still_held()?;
        guard::authority_untouched(&a.authority)?;
        let returned = venue.take_back(
            unasked,
            &a.authority.to_account_info(),
            &authority_dollars,
            &main,
        )?;
        let station = &mut a.station;
        station.idle = (station.idle - fund)
            .checked_add(returned)
            .ok_or(StationError::Overflow)?;
        emit!(Executed {
            driver: name,
            key,
            program: *program.key,
            funded: fund,
            returned,
        });
        Ok(())
    }

    // Moving money ----------------------------------------------------------

    /// The vault sends `amount` of its dollars to the station, signing for
    /// its own account. The station counts what arrived.
    pub fn pull(mut ctx: Context<Pull>, amount: u64) -> Result<()> {
        let a = &mut ctx.accounts;
        require!(!a.station.stopped, StationError::AlreadyStopped);
        let before = guard::balance(&a.station_dollars)?;
        token::transfer_checked(
            CpiContext::new(
                a.token_program.to_account_info(),
                TransferChecked {
                    from: a.vault_dollars.to_account_info(),
                    mint: a.mint.to_account_info(),
                    to: a.station_dollars.to_account_info(),
                    authority: a.vault.to_account_info(),
                },
            ),
            amount,
            a.mint.decimals,
        )?;
        let after = guard::balance(&a.station_dollars)?;
        count(&mut a.station.idle, before, after)?;
        emit!(Pulled {
            amount: after.saturating_sub(before),
        });
        Ok(())
    }

    /// The vault takes `amount` of the station's counted dollars home.
    pub fn push(mut ctx: Context<Push>, amount: u64) -> Result<()> {
        let a = &mut ctx.accounts;
        require!(!a.station.stopped, StationError::AlreadyStopped);
        require!(amount <= a.station.idle, StationError::NotEnoughIdle);
        send_home(
            amount,
            &mut a.station,
            &a.station_dollars,
            &a.vault_dollars,
            &a.mint,
            &a.token_program,
        )?;
        a.station.idle -= amount;
        emit!(Pushed { amount });
        Ok(())
    }

    // Stopping and unwinding --------------------------------------------------

    /// Stops the station for good, once the vault has halted and its grace
    /// has passed. Anyone may call it; the vault's own account says when it
    /// halted. Every key goes.
    pub fn stop(ctx: Context<Stop>) -> Result<()> {
        let station = &mut ctx.accounts.station;
        require!(!station.stopped, StationError::AlreadyStopped);
        let view = ask_vault(&ctx.accounts.vault_program, &ctx.accounts.vault)?;
        let now = now()?;
        require!(
            view.halted_at != 0 && now >= view.halted_at.saturating_add(view.halt_grace),
            StationError::NotHalted
        );
        station.stopped = true;
        station.stopped_at = now;
        for driver in station.drivers.iter_mut() {
            if driver.key == Pubkey::default() {
                continue;
            }
            // A key is only the station's record of it here: nothing at the
            // venue to withdraw, so nothing that could fail or run long and
            // keep the station from stopping.
            emit!(KeyRevoked {
                driver: driver.name,
                key: driver.key,
            });
            driver.key = Pubkey::default();
        }
        emit!(Stopped {
            halted_at: view.halted_at,
        });
        Ok(())
    }

    /// Closes `fraction` (`WAD` is all) of every position inside the exit
    /// band and sends that fraction's dollars, and that fraction of the
    /// station's counted dollars, to the vault. Answers with an [`Outcome`].
    ///
    /// The vault may always call it (signing as `caller`); an executor or
    /// unwinder key blessed for the vault may while the station runs (its
    /// registry records then come as the optional accounts); anyone may once
    /// the station has stopped, for all of it, which is the full unwind every
    /// holder shares.
    ///
    /// Every driver is asked about the band before anything is sold: a
    /// market outside it on one venue closes nothing anywhere, and the
    /// answer is `blocked`. A close that can't bring its worth home fails
    /// with `ShortSweep`, and the whole transaction with it, so trying a
    /// claim can never sell more than the leaver's share.
    ///
    /// The remaining accounts are one group per pinned driver, in the
    /// station's order, `groups[i]` accounts each: the driver program, the
    /// station's authority at the driver's venue and that authority's dollar
    /// account, then the accounts that driver reads.
    pub fn unwind<'info>(
        mut ctx: Context<'_, '_, 'info, 'info, Unwind<'info>>,
        fraction: u64,
        groups: Vec<u8>,
    ) -> Result<()> {
        require!(fraction != 0 && fraction <= WAD, StationError::NotAllowed);
        let a = &mut ctx.accounts;
        let now = now()?;
        let caller = a.caller.key();
        let is_vault = caller == a.station.vault;
        let stopped = a.station.stopped;
        let who = Identity::of(&a.station);
        let as_key = |kind: ImageKind| -> Result<bool> {
            let records = [
                a.registry_config.as_deref(),
                a.blessing.as_deref(),
                a.key_info.as_deref(),
                a.image.as_deref(),
                a.project.as_deref(),
            ];
            let [Some(c), Some(b), Some(k), Some(i), Some(p)] = records else {
                return Ok(false);
            };
            who.is_key([c, b, k, i, p], &caller, kind, now)
        };
        // Once stopped, a fraction is closed only for the vault's own claims
        // and serves, which wait for settlement themselves; the rest is a
        // full unwind, which waits for settlement too.
        let permitted = is_vault
            || (!stopped && (as_key(ImageKind::Executor)? || as_key(ImageKind::Unwinder)?))
            || (stopped && fraction == WAD);
        require!(permitted, StationError::NotAllowed);

        let main = Main::of(
            &a.station,
            a.station.to_account_info(),
            a.station_dollars.to_account_info(),
            &a.mint,
            &a.token_program,
        );
        let groups = split(ctx.remaining_accounts, &groups, a.station.drivers.len())?;
        let result = if stopped && fraction == WAD {
            // The vault itself can't be asked while it is the one calling (a
            // program can't be re-entered), so it settles before it unwinds;
            // anyone else's full unwind asks it.
            if !is_vault {
                waits_for_settlement(&a.station, a.vault.as_deref(), a.vault_program.as_deref())?;
            }
            let band = a.station.stopped_band(now);
            let named: Vec<(usize, &[AccountInfo<'info>])> =
                groups.iter().copied().enumerate().collect();
            let blocked = unwind_named(&mut a.station, &main, &named, band)?;
            let sent_home = a.station.idle;
            a.station.idle = 0;
            Outcome { sent_home, blocked }
        } else {
            unwind_fraction(&mut a.station, &main, &groups, fraction)?
        };
        if result.sent_home != 0 {
            send_home(
                result.sent_home,
                &mut a.station,
                &a.station_dollars,
                &a.vault_dollars,
                &a.mint,
                &a.token_program,
            )?;
        }
        emit!(Unwound {
            fraction,
            sent_home: result.sent_home,
            blocked: result.blocked,
        });
        answer(&result)
    }

    /// A stopped station's full unwind through the drivers named (by their
    /// index in the station), then every counted dollar home; with none
    /// named, only the counted dollars. Anyone may call it. A driver that
    /// fails fails the whole transaction (a call that fails can't be caught
    /// on Solana), so whoever unwinds leaves it out, and it can't keep any
    /// other venue's money, or the idle dollars, from coming home.
    ///
    /// The remaining accounts are one group per driver named, in the order
    /// named.
    pub fn unwind_stopped<'info>(
        mut ctx: Context<'_, '_, 'info, 'info, UnwindStopped<'info>>,
        drivers: Vec<u8>,
        groups: Vec<u8>,
    ) -> Result<()> {
        let a = &mut ctx.accounts;
        require!(a.station.stopped, StationError::NotHalted);
        // As in `unwind`: the vault settles before it calls; anyone else's
        // call asks it.
        if a.caller.key() != a.station.vault {
            waits_for_settlement(&a.station, Some(&a.vault), Some(&a.vault_program))?;
        }
        let count = a.station.drivers.len();
        for (i, index) in drivers.iter().enumerate() {
            require!(
                (*index as usize) < count && !drivers[i + 1..].contains(index),
                StationError::WrongDrivers
            );
        }
        let main = Main::of(
            &a.station,
            a.station.to_account_info(),
            a.station_dollars.to_account_info(),
            &a.mint,
            &a.token_program,
        );
        let groups = split(ctx.remaining_accounts, &groups, drivers.len())?;
        let named: Vec<(usize, &[AccountInfo<'info>])> = drivers
            .iter()
            .map(|index| *index as usize)
            .zip(groups.iter().copied())
            .collect();
        let band = a.station.stopped_band(now()?);
        let blocked = unwind_named(&mut a.station, &main, &named, band)?;
        let sent_home = a.station.idle;
        a.station.idle = 0;
        if sent_home != 0 {
            send_home(
                sent_home,
                &mut a.station,
                &a.station_dollars,
                &a.vault_dollars,
                &a.mint,
                &a.token_program,
            )?;
        }
        emit!(Unwound {
            fraction: WAD,
            sent_home,
            blocked,
        });
        answer(&Outcome { sent_home, blocked })
    }

    // What it holds -----------------------------------------------------------

    /// What the station holds, at the venues' marks and at what a sale would
    /// fetch, its counted dollars included; and whether any market a driver
    /// holds is outside the exit band, on either side of the venue's own
    /// price. Answers with a [`Valuation`]. Every pinned driver is asked, so
    /// the remaining accounts are one group per pinned driver, in the
    /// station's order, as for [`unwind`]. A venue authority's dollar account
    /// is valued by nobody: between the station's calls it holds only what
    /// someone sent it unasked.
    pub fn value<'info>(
        ctx: Context<'_, '_, 'info, 'info, Value<'info>>,
        groups: Vec<u8>,
    ) -> Result<()> {
        let station = &ctx.accounts.station;
        let groups = split(ctx.remaining_accounts, &groups, station.drivers.len())?;
        let mut total = Valuation {
            mark: station.idle,
            exit: station.idle,
            outside: false,
        };
        for (pinned, group) in station.drivers.iter().zip(&groups) {
            // Valued through the driver it can unwind through, so a revoked
            // driver's holdings still count.
            let accounts = driver_accounts(pinned, group)?;
            let v: Valuation = guard::ask(
                &accounts,
                shonin_driver::data(discriminator::VALUE, &station.exit_band_bps),
            )?;
            total.mark = total
                .mark
                .checked_add(v.mark)
                .ok_or(StationError::Overflow)?;
            total.exit = total
                .exit
                .checked_add(v.exit)
                .ok_or(StationError::Overflow)?;
            total.outside |= v.outside;
        }
        answer(&total)
    }
}

// Unwinding ------------------------------------------------------------------

/// One driver's part of a fractional close, prepared before anything is sold.
struct Leg<'info> {
    /// The driver's place in the station.
    index: usize,
    /// What the driver is handed, its program last.
    accounts: Vec<AccountInfo<'info>>,
    plan: Plan,
    /// The orders its close places: as many dollar-units as the venue's
    /// rounding may keep from what it brings home.
    orders: u64,
    mark: u64,
    exit: u64,
    freed: u64,
    skip: bool,
}

/// The orders a plan says it places, as the station credits them: never
/// more than a driver's account may hold markets.
fn orders(plan: &Plan) -> u64 {
    (plan.orders as usize).min(MAX_MARKETS) as u64
}

/// Closes `fraction` of every venue account inside the exit band, in the
/// order that keeps each rule from undoing another: every driver asked about
/// the band first; then a dust account left where it is; then a close that
/// would pay the leaver nothing refused; then each close made and swept,
/// what came home measured against the fraction's worth, and what was left
/// at the venue against the stayers' part.
///
/// Each order a close places is sold at the venue's price and its proceeds
/// rounded down there, so a close may come home a dollar-unit short per
/// order and still be whole: every check of what came home allows that much,
/// and no more. That rounding is the leaver's: it comes out of what they are
/// paid, never out of what the stayers keep.
fn unwind_fraction<'info>(
    station: &mut Station,
    main: &Main<'info>,
    groups: &[&[AccountInfo<'info>]],
    fraction: u64,
) -> Result<Outcome> {
    let band = station.exit_band_bps;

    // Every driver's close is prepared before any is made, and the band is
    // asked of every one first, so a market outside it on one venue closes
    // nothing anywhere, whatever any account reads as: a leaver is never
    // half-sold.
    let mut legs = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        let accounts = driver_accounts(&station.drivers[index], group)?;
        // The close comes with the account's valuation, read from the same
        // accounts the plan was prepared from: what the fraction is worth at
        // exit prices before the close, exactly what selling it realises and
        // all that comes home for it. One ask, rather than a separate value
        // before it, so every driver's close fits one transaction.
        let Close { value, plan } = guard::ask(
            &accounts,
            shonin_driver::data(discriminator::CLOSE, &(fraction, band)),
        )?;
        if plan.outside {
            return Ok(Outcome {
                sent_home: 0,
                blocked: true,
            });
        }
        legs.push(Leg {
            index,
            accounts,
            orders: orders(&plan),
            plan,
            mark: value.mark,
            exit: value.exit,
            freed: 0,
            skip: false,
        });
    }
    for leg in legs.iter_mut() {
        // An account worth no more than one dollar-unit more than the orders
        // its close would place is dust: a fraction of it is left where it
        // is rather than let its rounding hold every leaver's close. A full
        // unwind still sells it.
        let dust = leg.orders + 1;
        if fraction < WAD && leg.mark <= dust {
            leg.skip = true;
            continue;
        }
        leg.freed = shonin_driver::mul_div(leg.exit, fraction, WAD);
        // A fraction worth more than dust at the mark and no more at exit
        // prices than the venue's rounding may keep of it (an account under
        // water at the bid, or nearly) is never sold for the leaver: it could
        // bring them nothing and leave the loss with everyone else. Only a
        // full unwind, which everyone shares, may.
        if leg.freed <= leg.orders
            && shonin_driver::mul_div(leg.mark, fraction, WAD) > dust * dust
            && fraction < WAD
        {
            return err!(StationError::ShortSweep);
        }
    }

    let idle_before = station.idle;
    let mut brought: u64 = 0;
    for leg in legs.iter().filter(|leg| !leg.skip) {
        let held = guard::balance(&main.dollars)?;
        run(&leg.plan, leg.index, &leg.accounts, station, main)?;
        let sweep: Plan = guard::ask(
            &leg.accounts,
            shonin_driver::data(discriminator::SWEEP, &leg.freed),
        )?;
        run(&sweep, leg.index, &leg.accounts, station, main)?;
        let after = guard::balance(&main.dollars)?;
        require!(after >= held, StationError::Unaccounted);
        let swept = after - held;
        // What came home, with the most the venue's rounding may have kept.
        let whole = swept.saturating_add(leg.orders);
        // Selling the fraction is only a serve if its worth comes home:
        // proceeds left at the venue would still belong to everyone. And
        // nothing home for a fraction worth more than its rounding is never a
        // fill.
        if whole < shonin_driver::mul_div(leg.freed, BPS - band as u64, BPS)
            || (swept == 0 && leg.freed > leg.orders)
        {
            return err!(StationError::ShortSweep);
        }
        // The stayers' part of the account: what it was worth at exit prices
        // before the close, less the leaver's part.
        let share = leg.exit - leg.freed;
        let remains: Valuation = guard::ask(
            &leg.accounts,
            shonin_driver::data(discriminator::VALUE, &band),
        )?;
        // A shortfall past the rounding is the leaver's own fill, never the
        // stayers' gain: proceeds that went to repay the account's margin
        // debt, so the account is left worth more than the stayers' share,
        // still belong to the leaver, and the close is undone.
        if whole < leg.freed && remains.exit > share.saturating_add(leg.orders.max(1)) {
            return err!(StationError::ShortSweep);
        }
        // Never more than the fraction's worth: anything a venue sends beyond
        // it stays here, counted, everyone's.
        let mut kept = swept.min(leg.freed);
        // And the stayers never pay for what the close didn't fetch: what is
        // left of the account, with anything sent beyond the leaver's part,
        // must be worth their part. A shortfall of the venue's rounding is
        // taken from what the leaver is paid, as far as that goes; anything
        // more undoes the close.
        let left = remains.exit.saturating_add(swept - kept);
        let short = share.saturating_sub(left);
        if short > leg.orders {
            return err!(StationError::ShortSweep);
        }
        kept -= short.min(kept);
        brought = brought.checked_add(kept).ok_or(StationError::Overflow)?;
        station.idle = station
            .idle
            .checked_add(swept - kept)
            .ok_or(StationError::Overflow)?;
    }
    let idle_part = shonin_driver::mul_div(idle_before, fraction, WAD);
    station.idle -= idle_part;
    Ok(Outcome {
        sent_home: brought
            .checked_add(idle_part)
            .ok_or(StationError::Overflow)?,
        blocked: false,
    })
}

/// A stopped station's full unwind through the drivers named, each on its
/// own inside `band` as it has widened since the stop: a market still
/// outside the band leaves only its own venue's money behind. What comes
/// home is counted; the caller sends everything counted home. As for a
/// fraction, what comes home may be a dollar-unit short per order placed.
fn unwind_named<'info>(
    station: &mut Station,
    main: &Main<'info>,
    named: &[(usize, &[AccountInfo<'info>])],
    band: u16,
) -> Result<bool> {
    let mut blocked = false;
    for (index, group) in named {
        let accounts = driver_accounts(&station.drivers[*index], group)?;
        // The close comes with the account's valuation at the same band,
        // read from the same accounts: what its sale should bring home.
        let Close { value, plan } = guard::ask(
            &accounts,
            shonin_driver::data(discriminator::CLOSE, &(WAD, band)),
        )?;
        if plan.outside {
            blocked = true;
            continue;
        }
        let held = guard::balance(&main.dollars)?;
        run(&plan, *index, &accounts, station, main)?;
        let sweep: Plan = guard::ask(
            &accounts,
            shonin_driver::data(discriminator::SWEEP, &value.exit),
        )?;
        run(&sweep, *index, &accounts, station, main)?;
        let after = guard::balance(&main.dollars)?;
        require!(after >= held, StationError::Unaccounted);
        let swept = after - held;
        if swept.saturating_add(orders(&plan))
            < shonin_driver::mul_div(value.exit, BPS - band.min(BPS as u16) as u64, BPS)
        {
            return err!(StationError::ShortSweep);
        }
        station.idle = station
            .idle
            .checked_add(swept)
            .ok_or(StationError::Overflow)?;
    }
    Ok(blocked)
}

/// Makes each instruction the driver at `index` prepared, signed as the
/// station's authority at its venue, after checking it against that driver's
/// pinned allowlist exactly as an executor's own is checked, and refuses any
/// that leaves the authority's dollar account lower than it found it, or any
/// token account of the authority's or the station's it could write open to
/// someone else, or the authority itself other than a system account with no
/// data. Then everything the instructions brought to the authority's
/// dollar account is moved to the station's own, where the caller measures
/// it. `accounts` are what the driver was given, its program last; an
/// instruction may name only the ones before it.
fn run<'info>(
    plan: &Plan,
    index: usize,
    accounts: &[AccountInfo<'info>],
    station: &Station,
    main: &Main<'info>,
) -> Result<()> {
    let pinned = &station.drivers[index];
    let venue = Venue::of(pinned, main.station.key);
    let given = &accounts[..accounts.len() - 1];
    let (authority, dollars) = (&given[0], &given[1]);
    let unasked = guard::balance(dollars)?;
    for prepared in &plan.instructions {
        let ix = guard::rebuild(prepared, given, &venue.authority)?;
        guard::check(
            &ix,
            pinned,
            &station.drivers,
            main.station.key,
            main.dollars.key,
            &station.vault_program,
            given,
        )?;
        // A close only ever brings money to the station: no instruction a
        // driver prepares may take dollars out of the authority's account,
        // even to bring more back in a later one.
        let held = guard::TokenAccounts::before(&ix, given, &[&venue.authority, main.station.key]);
        let before = guard::balance(dollars)?;
        invoke_signed(&ix, accounts, &[&venue.seeds()])?;
        held.still_held()?;
        guard::authority_untouched(authority)?;
        require!(
            guard::balance(dollars)? >= before,
            StationError::Unaccounted
        );
    }
    venue.take_back(unasked, authority, dollars, main)?;
    Ok(())
}

/// What a driver is handed: the station's authority at its venue, that
/// authority's dollar account, then the group's own accounts; and last, for
/// the call, the driver program. The group is the driver program, which must
/// be the one pinned, the authority and its dollar account, which must be the
/// ones pinned with it, then the accounts the driver reads.
fn driver_accounts<'info>(
    pinned: &Pinned,
    group: &[AccountInfo<'info>],
) -> Result<Vec<AccountInfo<'info>>> {
    let [program, authority, dollars, rest @ ..] = group else {
        return err!(StationError::WrongDrivers);
    };
    require_keys_eq!(*program.key, pinned.program, StationError::WrongDrivers);
    require!(
        authority.key == &pinned.authority && dollars.key == &pinned.dollars,
        StationError::WrongAuthority
    );
    let mut accounts = Vec::with_capacity(rest.len() + 3);
    accounts.push(authority.clone());
    accounts.push(dollars.clone());
    accounts.extend(rest.iter().cloned());
    accounts.push(program.clone());
    Ok(accounts)
}

/// Splits `accounts` into `count` groups of the sizes given, which must use
/// every account.
fn split<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    sizes: &[u8],
    count: usize,
) -> Result<Vec<&'a [AccountInfo<'info>]>> {
    require!(sizes.len() == count, StationError::WrongDrivers);
    let mut groups = Vec::with_capacity(count);
    let mut at = 0usize;
    for size in sizes {
        let end = at + *size as usize;
        let group = accounts.get(at..end).ok_or(StationError::WrongDrivers)?;
        require!(!group.is_empty(), StationError::WrongDrivers);
        groups.push(group);
        at = end;
    }
    require!(at == accounts.len(), StationError::WrongDrivers);
    Ok(groups)
}

// Money and the vault ----------------------------------------------------------

/// Counts what a call did to the station's dollar account: what came in is
/// counted, and what went out may not exceed what was counted.
fn count(idle: &mut u64, before: u64, after: u64) -> Result<()> {
    if after >= before {
        *idle = idle
            .checked_add(after - before)
            .ok_or(StationError::Overflow)?;
    } else {
        let spent = before - after;
        require!(spent <= *idle, StationError::Unaccounted);
        *idle -= spent;
    }
    Ok(())
}

/// The station's own dollar account, and what a transfer out of it or into it
/// needs.
struct Main<'info> {
    station: AccountInfo<'info>,
    vault: Pubkey,
    bump: u8,
    dollars: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    decimals: u8,
}

impl<'info> Main<'info> {
    fn of(
        station: &Station,
        info: AccountInfo<'info>,
        dollars: AccountInfo<'info>,
        mint: &Account<'info, Mint>,
        token_program: &Program<'info, Token>,
    ) -> Self {
        Self {
            station: info,
            vault: station.vault,
            bump: station.bump,
            dollars,
            mint: mint.to_account_info(),
            token_program: token_program.to_account_info(),
            decimals: mint.decimals,
        }
    }

    /// Sends `amount` of the station's dollars to `to`, signed by the
    /// station.
    fn send(&self, amount: u64, to: &AccountInfo<'info>) -> Result<()> {
        let bump = [self.bump];
        let seeds: &[&[u8]] = &[STATION_SEED, self.vault.as_ref(), &bump];
        token::transfer_checked(
            CpiContext::new_with_signer(
                self.token_program.clone(),
                TransferChecked {
                    from: self.dollars.clone(),
                    mint: self.mint.clone(),
                    to: to.clone(),
                    authority: self.station.clone(),
                },
                &[seeds],
            ),
            amount,
            self.decimals,
        )
    }
}

/// The station's authority at one venue, and how it signs.
struct Venue {
    station: Pubkey,
    venue: Pubkey,
    authority: Pubkey,
    bump: [u8; 1],
}

impl Venue {
    fn of(pinned: &Pinned, station: &Pubkey) -> Self {
        Self {
            station: *station,
            venue: pinned.venue,
            authority: pinned.authority,
            bump: [pinned.authority_bump],
        }
    }

    fn seeds(&self) -> [&[u8]; 4] {
        [
            VENUE_SEED,
            self.station.as_ref(),
            self.venue.as_ref(),
            &self.bump,
        ]
    }

    /// Moves everything in the authority's dollar account beyond `unasked`
    /// (what was there before the station's call, which nobody counted) to
    /// the station's own, signed by the authority, and answers with how much.
    /// A call that left the account below `unasked` spent dollars nobody
    /// counted, and is refused.
    fn take_back<'info>(
        &self,
        unasked: u64,
        authority: &AccountInfo<'info>,
        dollars: &AccountInfo<'info>,
        main: &Main<'info>,
    ) -> Result<u64> {
        let now = guard::balance(dollars)?;
        require!(now >= unasked, StationError::Unaccounted);
        let back = now - unasked;
        if back != 0 {
            token::transfer_checked(
                CpiContext::new_with_signer(
                    main.token_program.clone(),
                    TransferChecked {
                        from: dollars.clone(),
                        mint: main.mint.clone(),
                        to: main.dollars.clone(),
                        authority: authority.clone(),
                    },
                    &[&self.seeds()],
                ),
                back,
                main.decimals,
            )?;
        }
        Ok(back)
    }
}

/// Sends `amount` from the station's dollar account to the vault's, signed
/// by the station, and adds it to the running total of everything sent home,
/// by which the vault counts what arrived.
fn send_home<'info>(
    amount: u64,
    station: &mut Account<'info, Station>,
    dollars: &AccountInfo<'info>,
    vault_dollars: &AccountInfo<'info>,
    mint: &Account<'info, Mint>,
    token_program: &Program<'info, Token>,
) -> Result<()> {
    let bump = [station.bump];
    let seeds: &[&[u8]] = &[STATION_SEED, station.vault.as_ref(), &bump];
    token::transfer_checked(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            TransferChecked {
                from: dollars.clone(),
                mint: mint.to_account_info(),
                to: vault_dollars.clone(),
                authority: station.to_account_info(),
            },
            &[seeds],
        ),
        amount,
        mint.decimals,
    )?;
    station.sent_home = station
        .sent_home
        .checked_add(amount as u128)
        .ok_or(StationError::Overflow)?;
    emit!(SentHome {
        amount,
        total: station.sent_home,
    });
    Ok(())
}

/// Asks the vault program, through the vault's own account, whether it has
/// halted and whether it has a priced valuation to settle. The station never
/// calls the vault otherwise, and never while the vault is calling it.
fn ask_vault<'info>(
    program: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
) -> Result<vault::VaultView> {
    let ix = Instruction {
        program_id: *program.key,
        accounts: vec![AccountMeta::new_readonly(*vault.key, false)],
        data: vault::STATION_VIEW.to_vec(),
    };
    set_return_data(&[]);
    invoke(&ix, &[vault.clone(), program.clone()])?;
    let (from, answer) = get_return_data().ok_or(StationError::DriverSilent)?;
    require_keys_eq!(from, *program.key, StationError::DriverSilent);
    vault::VaultView::try_from_slice(&answer).map_err(|_| error!(StationError::BadAnswer))
}

/// A stopped station's full unwind waits while the vault has a priced
/// valuation to settle: what the sale brings home must not pay withdrawals
/// priced before it.
fn waits_for_settlement<'info>(
    station: &Station,
    vault: Option<&AccountInfo<'info>>,
    program: Option<&AccountInfo<'info>>,
) -> Result<()> {
    let (Some(vault), Some(program)) = (vault, program) else {
        return err!(StationError::Unsettled);
    };
    require_keys_eq!(*vault.key, station.vault, StationError::NotAVault);
    require_keys_eq!(*program.key, station.vault_program, StationError::NotAVault);
    require!(
        !ask_vault(program, vault)?.unsettled,
        StationError::Unsettled
    );
    Ok(())
}

/// Leaves `value` as the instruction's answer.
fn answer(value: &impl AnchorSerialize) -> Result<()> {
    let bytes = value.try_to_vec()?;
    require!(bytes.len() <= MAX_RETURN_DATA, StationError::Overflow);
    set_return_data(&bytes);
    Ok(())
}

// The registry -----------------------------------------------------------------

/// Who a key must be blessed for: this vault, running this bundle, on this
/// cluster.
struct Identity {
    cluster: [u8; 32],
    vault: [u8; 32],
    bundle_hash: [u8; 32],
}

impl Identity {
    fn of(station: &Station) -> Self {
        Self {
            cluster: station.cluster,
            vault: station.vault.to_bytes(),
            bundle_hash: station.bundle_hash,
        }
    }

    /// Whether `key` is blessed now for this vault and bundle, from an image
    /// of `kind`. `records` are the registry's config, the key's blessing for
    /// this vault, its key record, and its image's and project's records,
    /// each checked to be the one it stands for.
    fn is_key(
        &self,
        records: [&AccountInfo; 5],
        key: &Pubkey,
        kind: ImageKind,
        now: u64,
    ) -> Result<bool> {
        let [config, blessing, key_info, image, project] = records;
        let blessed = read::blessed_for(
            config,
            blessing,
            key_info,
            image,
            project,
            key,
            &self.cluster,
            &self.vault,
            &self.bundle_hash,
            now,
        )?;
        Ok(blessed && read::key_info(key_info, key)?.is_some_and(|info| info.kind == kind))
    }
}

// Accounts -------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CreateArgs {
    /// The bundle the vault runs.
    pub bundle_hash: [u8; 32],
    pub exit_band_bps: u16,
    /// The drivers the strategy declared, by name.
    pub drivers: Vec<[u8; 32]>,
    /// How many remaining accounts each driver takes.
    pub driver_accounts: Vec<u8>,
    /// The seeds, bump last, that derive the vault's address from the vault
    /// program.
    pub vault_seeds: Vec<Vec<u8>>,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,
    #[account(
        init,
        payer = deployer,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump,
    )]
    pub config: Account<'info, Config>,
    pub mint: Account<'info, Mint>,
    /// CHECK: read by `read::config`, which checks it is the registry's.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: this program's own account, whose upgrade authority is read.
    #[account(address = crate::ID)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: checked against `program` by `loader::is_upgrade_authority`.
    pub program_data: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Create<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub vault: Signer<'info>,
    /// The vault's own dollar account, where the station sends everything.
    #[account(
        token::mint = config.mint,
        token::authority = vault,
        token::token_program = token_program,
    )]
    pub vault_dollars: Account<'info, TokenAccount>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// CHECK: created here, at the address its seeds derive; refused if
    /// anything is there already.
    #[account(mut, seeds = [STATION_SEED, vault.key().as_ref()], bump)]
    pub station: UncheckedAccount<'info>,
    /// CHECK: the station's associated dollar account, checked by address and
    /// created here.
    #[account(mut)]
    pub station_dollars: UncheckedAccount<'info>,
    #[account(address = config.mint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ApproveKey<'info> {
    pub caller: Signer<'info>,
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the registry's config, checked by `read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the key's blessing for this vault, checked by `read`.
    pub blessing: UncheckedAccount<'info>,
    /// CHECK: the key's record, checked by `read`.
    pub key_info: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `read`.
    pub image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `read`.
    pub project: UncheckedAccount<'info>,
    /// CHECK: the driver's approval slot, checked by `read`.
    pub slot: UncheckedAccount<'info>,
    /// CHECK: the pinned implementation's revocation record, checked by
    /// `read`.
    pub revoked: UncheckedAccount<'info>,
    /// CHECK: the same records for the key in place, when it is replaced by
    /// someone other than the new key; checked by `read`.
    pub current_blessing: Option<UncheckedAccount<'info>>,
    /// CHECK: see `current_blessing`.
    pub current_key_info: Option<UncheckedAccount<'info>>,
    /// CHECK: see `current_blessing`.
    pub current_image: Option<UncheckedAccount<'info>>,
    /// CHECK: see `current_blessing`.
    pub current_project: Option<UncheckedAccount<'info>>,
}

#[derive(Accounts)]
pub struct RevokeKey<'info> {
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the registry's config, checked by `read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the key's blessing for this vault, checked by `read`.
    pub blessing: UncheckedAccount<'info>,
    /// CHECK: the key's record, checked by `read`.
    pub key_info: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `read`.
    pub image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `read`.
    pub project: UncheckedAccount<'info>,
    /// CHECK: the driver's approval slot, checked by `read`.
    pub slot: UncheckedAccount<'info>,
    /// CHECK: the pinned implementation's revocation record, checked by
    /// `read`.
    pub revoked: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Execute<'info> {
    pub key: Signer<'info>,
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the station's dollar account, by address.
    #[account(mut, address = station.dollars)]
    pub station_dollars: UncheckedAccount<'info>,
    /// CHECK: the station's authority at the driver's venue, checked against
    /// the driver pinned.
    pub authority: UncheckedAccount<'info>,
    /// CHECK: that authority's dollar account, likewise.
    #[account(mut)]
    pub authority_dollars: UncheckedAccount<'info>,
    #[account(address = station.mint)]
    pub mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    /// CHECK: the registry's config, checked by `read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the key's blessing for this vault, checked by `read`.
    pub blessing: UncheckedAccount<'info>,
    /// CHECK: the key's record, checked by `read`.
    pub key_info: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `read`.
    pub image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `read`.
    pub project: UncheckedAccount<'info>,
    /// CHECK: the driver's approval slot, checked by `read`.
    pub slot: UncheckedAccount<'info>,
    /// CHECK: the pinned implementation's revocation record, checked by
    /// `read`.
    pub revoked: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Pull<'info> {
    pub vault: Signer<'info>,
    #[account(
        mut,
        seeds = [STATION_SEED, vault.key().as_ref()],
        bump = station.bump,
        has_one = vault,
    )]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the vault's dollar account, by address; the token program
    /// checks the vault signs for it.
    #[account(mut, address = station.vault_dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the station's dollar account, by address.
    #[account(mut, address = station.dollars)]
    pub station_dollars: UncheckedAccount<'info>,
    #[account(address = station.mint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Push<'info> {
    pub vault: Signer<'info>,
    #[account(
        mut,
        seeds = [STATION_SEED, vault.key().as_ref()],
        bump = station.bump,
        has_one = vault,
    )]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = station.vault_dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the station's dollar account, by address.
    #[account(mut, address = station.dollars)]
    pub station_dollars: UncheckedAccount<'info>,
    #[account(address = station.mint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Stop<'info> {
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the vault's account, by address; read by its own program.
    #[account(address = station.vault)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: the vault program, by address.
    #[account(address = station.vault_program)]
    pub vault_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Unwind<'info> {
    pub caller: Signer<'info>,
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the station's dollar account, by address.
    #[account(mut, address = station.dollars)]
    pub station_dollars: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = station.vault_dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    #[account(address = station.mint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    /// CHECK: the caller's registry records, when the caller is a key and
    /// not the vault; checked by `read`.
    pub registry_config: Option<UncheckedAccount<'info>>,
    /// CHECK: see `registry_config`.
    pub blessing: Option<UncheckedAccount<'info>>,
    /// CHECK: see `registry_config`.
    pub key_info: Option<UncheckedAccount<'info>>,
    /// CHECK: see `registry_config`.
    pub image: Option<UncheckedAccount<'info>>,
    /// CHECK: see `registry_config`.
    pub project: Option<UncheckedAccount<'info>>,
    /// CHECK: the vault's account, asked whether it has a valuation to
    /// settle, when someone other than the vault unwinds a stopped station;
    /// checked by address there.
    pub vault: Option<UncheckedAccount<'info>>,
    /// CHECK: the vault program, likewise.
    pub vault_program: Option<UncheckedAccount<'info>>,
}

#[derive(Accounts)]
pub struct UnwindStopped<'info> {
    pub caller: Signer<'info>,
    #[account(mut, seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
    /// CHECK: the station's dollar account, by address.
    #[account(mut, address = station.dollars)]
    pub station_dollars: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = station.vault_dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    #[account(address = station.mint)]
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    /// CHECK: the vault's account, by address.
    #[account(address = station.vault)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: the vault program, by address.
    #[account(address = station.vault_program)]
    pub vault_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Value<'info> {
    #[account(seeds = [STATION_SEED, station.vault.as_ref()], bump = station.bump)]
    pub station: Box<Account<'info, Station>>,
}

/// Where a vault's station is, or will be.
pub fn station_address(vault: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[STATION_SEED, vault.as_ref()], &crate::ID).0
}

/// The station's authority at `venue`, and its bump.
pub fn venue_authority(station: &Pubkey, venue: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VENUE_SEED, station.as_ref(), venue.as_ref()], &crate::ID)
}

pub fn config_address() -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED], &crate::ID).0
}
