//! The guard: what the station checks before it signs anything, and how it
//! asks a driver.
//!
//! The station signs a venue call only as its authority at that venue, a
//! program-derived address of its own kept for that venue alone
//! ([`crate::state::VENUE_SEED`]). That authority is the master of the
//! station's account at the venue and owns the token accounts the venue's
//! rules name as the station's; the station's own address, which owns its
//! dollar account, never signs a venue call. So whatever a venue's code does
//! with the signature it is lent, it holds that venue's authority and nothing
//! else: every other venue's accounts and the station's dollar account refuse
//! it. This is what contains a venue, including one whose upgrade authority
//! replaces its code, and it holds however a program reaches another (a
//! program can call any program the transaction names anywhere, not only
//! those among its own accounts).
//!
//! Every call made with a venue's authority goes through [`check`], whoever
//! prepared it: the executor's key sending a venue instruction, or a driver
//! preparing a close. The instruction must be for the driver's venue program,
//! must start with a discriminator on the driver's pinned allowlist, and must
//! have the authority's own accounts at the positions that rule names. The
//! authority may stand only where a rule puts it, since it is the one account
//! the call is signed for: anywhere else its signature could reach something
//! the allowlist never meant. And it stands there read-only: a venue handed
//! its authority as a writable signer could have the system program give the
//! authority data and a new owner (a token multisig of its own keys, say),
//! after which the token program would let those keys move every token
//! account the authority owns, outside any station call. The station's own
//! address and its own dollar account stand nowhere: the dollar account is
//! measured only around the station's own transfers, so whatever a venue call
//! sent there would never be counted.
//!
//! Nor may a call name, anywhere among its accounts, the program of another
//! venue the station pins, any driver it pins, or a program no allowlist may
//! name as its venue other than the token programs, the associated token
//! program and the system program (which venues are routinely handed). That
//! is a check on what the call
//! names, kept beside the authority: a venue handed none of these can still
//! call one the transaction names elsewhere, and only the authority it signs
//! with stops that call reaching anyone else's money.
//!
//! What a call did is checked after it too ([`TokenAccounts`]): a venue lent
//! its authority's signature could approve a delegate over one of that
//! authority's token accounts, give it a close authority or hand it to another
//! owner while moving nothing. Every token account of the authority's, or of
//! the station's own, that a call could write must come back still theirs
//! alone. And the authority itself must come back as it always is, an account
//! of the system program's with no data ([`authority_untouched`]), which a call
//! that can't write it can't change; this is checked anyway, so that nothing
//! rests on the writable check alone.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke, set_return_data},
};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use shonin_driver::{
    Allowlist, Class, Own, Prepared, Rule, MAX_DISCRIMINATOR, MAX_FIXED, MAX_POSITIONS, MAX_RULES,
    MAX_SEED, MAX_STATION_ADDRESSES,
};
use solana_sdk_ids::{
    bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4, native_loader,
    system_program,
};

use crate::error::StationError;
use crate::state::{Pinned, StationAddress};

/// Programs no allowlist may name, whatever a driver says: the station
/// itself, the vault's program, Shonin's other programs, and the programs
/// that hold or create accounts and money directly. A driver reaches its
/// venue and nothing else. (A station also refuses a venue that is one of its
/// own driver programs, which only it knows.)
pub fn forbidden(venue: &Pubkey, vault_program: &Pubkey) -> bool {
    never_named(venue, vault_program)
        || [
            system_program::ID,
            anchor_spl::token::ID,
            anchor_spl::token_2022::ID,
            anchor_spl::associated_token::ID,
        ]
        .contains(venue)
}

/// The programs no venue call may name anywhere among its accounts: every
/// program [`forbidden`] as a venue except the token programs, the associated
/// token program and the system program, which a venue's own instructions
/// take to move and create the accounts it holds. Shonin's programs and the
/// loaders have no place in a venue's instruction.
pub fn never_named(program: &Pubkey, vault_program: &Pubkey) -> bool {
    // The development checker is never deployed on mainnet.
    #[cfg(not(feature = "mainnet"))]
    if program == &shonin_dev_verifier::ID {
        return true;
    }
    [
        crate::ID,
        *vault_program,
        shonin_registry::ID,
        shonin_publishing::ID,
        shonin_prices::ID,
        shonin_profiles::ID,
        bpf_loader::ID,
        bpf_loader_deprecated::ID,
        bpf_loader_upgradeable::ID,
        loader_v4::ID,
        native_loader::ID,
    ]
    .contains(program)
}

/// Checks an allowlist a driver gave at creation: a venue the station may
/// call, a bounded number of rules, no discriminator another one starts
/// with (so every instruction matches one rule at most), each position
/// named once, a bounded number of the venue's addresses for the station,
/// and a bounded number of pinned accounts, in sets none of which is empty
/// and every one a rule names there.
pub fn validate(allowlist: &Allowlist, vault_program: &Pubkey) -> Result<()> {
    require!(
        !forbidden(&allowlist.venue, vault_program),
        StationError::BadAllowlist
    );
    let sets = &allowlist.fixed;
    require!(
        sets.iter().all(|set| !set.is_empty())
            && sets.iter().map(Vec::len).sum::<usize>() <= MAX_FIXED,
        StationError::BadAllowlist
    );
    let rules = &allowlist.rules;
    require!(
        !rules.is_empty() && rules.len() <= MAX_RULES,
        StationError::BadAllowlist
    );
    for (i, rule) in rules.iter().enumerate() {
        let length = rule.discriminator.len();
        require!(
            (1..=MAX_DISCRIMINATOR).contains(&length) && rule.positions.len() <= MAX_POSITIONS,
            StationError::BadAllowlist
        );
        for other in &rules[i + 1..] {
            require!(
                !rule.discriminator.starts_with(&other.discriminator)
                    && !other.discriminator.starts_with(&rule.discriminator),
                StationError::BadAllowlist
            );
        }
        for (j, position) in rule.positions.iter().enumerate() {
            require!(
                rule.positions[j + 1..]
                    .iter()
                    .all(|other| other.index != position.index),
                StationError::BadAllowlist
            );
            let fits = match &position.own {
                Own::StationAddress(prefix) => prefix.len() <= MAX_SEED,
                Own::Fixed(set) | Own::StationToken(Some(set)) => (*set as usize) < sets.len(),
                Own::Station | Own::StationDollars | Own::StationToken(None) => true,
            };
            require!(fits, StationError::BadAllowlist);
        }
    }
    require!(
        prefixes(allowlist).len() <= MAX_STATION_ADDRESSES,
        StationError::BadAllowlist
    );
    Ok(())
}

/// Each seed prefix the allowlist's rules name for a venue address of the
/// station's authority, once.
fn prefixes(allowlist: &Allowlist) -> Vec<&Vec<u8>> {
    let mut prefixes: Vec<&Vec<u8>> = Vec::new();
    for position in allowlist.rules.iter().flat_map(|rule| &rule.positions) {
        if let Own::StationAddress(prefix) = &position.own {
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// The venue's address for the station's `authority` there under each prefix
/// the allowlist names, worked out once, for the station to pin.
pub fn station_addresses(allowlist: &Allowlist, authority: &Pubkey) -> Vec<StationAddress> {
    prefixes(allowlist)
        .into_iter()
        .map(|prefix| StationAddress {
            prefix: prefix.clone(),
            address: Pubkey::find_program_address(&[prefix, authority.as_ref()], &allowlist.venue)
                .0,
        })
        .collect()
}

/// The rule `data` falls under, if any.
fn rule_for<'a>(rules: &'a [Rule], data: &[u8]) -> Option<&'a Rule> {
    rules
        .iter()
        .find(|rule| data.starts_with(&rule.discriminator))
}

/// Whether `ix` may be made with the station's authority at `pinned`'s venue,
/// `pinned` being one of the station's `drivers`, and the class of the rule
/// it falls under. `station` is the station's own address and
/// `station_dollars` its own dollar account, neither of which stands
/// anywhere in a venue call. `accounts` are the accounts the instruction will
/// be given, to read a token account's mint and owner.
pub fn check(
    ix: &Instruction,
    pinned: &Pinned,
    drivers: &[Pinned],
    station: &Pubkey,
    station_dollars: &Pubkey,
    vault_program: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<Class> {
    require_keys_eq!(ix.program_id, pinned.venue, StationError::WrongProgram);
    let rule = rule_for(&pinned.rules, &ix.data).ok_or(StationError::NotOnTheAllowlist)?;
    for position in &rule.positions {
        require!(
            (position.index as usize) < ix.accounts.len(),
            StationError::ForeignAccount
        );
    }
    let authority = &pinned.authority;
    for (i, meta) in ix.accounts.iter().enumerate() {
        // No other venue the station pins, no driver program, and no program
        // a venue never needs, anywhere in the call. This only refuses what
        // the call names: the venue can still call a program named elsewhere
        // in the transaction, and what keeps it from reaching anyone else's
        // money then is that it signs only as this venue's authority.
        require!(
            !never_named(&meta.pubkey, vault_program)
                && !drivers.iter().any(|other| {
                    meta.pubkey == other.program
                        || (meta.pubkey == other.venue && other.venue != pinned.venue)
                }),
            StationError::ReachesPinned
        );
        // The station's own address never signs a venue call and has no
        // place in one. Nor has its own dollar account: the station counts
        // what reaches it only around its own transfers, so dollars a venue
        // call sent there would sit uncounted for good.
        require!(
            &meta.pubkey != station && &meta.pubkey != station_dollars,
            StationError::StationOutOfPlace
        );
        let own = rule
            .positions
            .iter()
            .find(|position| position.index as usize == i)
            .map(|position| &position.own);
        if &meta.pubkey == authority {
            require!(own == Some(&Own::Station), StationError::StationOutOfPlace);
            // Signing is all a venue needs of its authority. Writable, the
            // signature would also let it give the authority data and a new
            // owner through the system program, and so hand every token
            // account the authority owns to whoever that owner obeys.
            require!(!meta.is_writable, StationError::AuthorityWritable);
        }
        let Some(own) = own else {
            continue;
        };
        let pinned_set = |set: &u8| pinned.fixed.get(*set as usize);
        let ours = match own {
            Own::Station => &meta.pubkey == authority,
            Own::StationDollars => meta.pubkey == pinned.dollars,
            Own::StationToken(set) => station_token_mint(&meta.pubkey, authority, accounts)
                .is_some_and(|mint| {
                    set.as_ref()
                        .is_none_or(|set| pinned_set(set).is_some_and(|set| set.contains(&mint)))
                }),
            Own::StationAddress(prefix) => pinned.address(prefix) == Some(&meta.pubkey),
            Own::Fixed(set) => pinned_set(set).is_some_and(|set| set.contains(&meta.pubkey)),
        };
        require!(ours, StationError::ForeignAccount);
    }
    Ok(rule.class)
}

/// The mint of `key` if it is `authority`'s associated token account for the
/// mint the account holds, under the token program that owns it: never a
/// look-alike someone created in the authority's name.
fn station_token_mint(
    key: &Pubkey,
    authority: &Pubkey,
    accounts: &[AccountInfo],
) -> Option<Pubkey> {
    let info = accounts.iter().find(|info| info.key == key)?;
    let program = *info.owner;
    if !is_token_program(&program) {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    let mint = Pubkey::try_from(data.get(0..32)?).ok()?;
    let owner = Pubkey::try_from(data.get(32..64)?).ok()?;
    (owner == *authority
        && *key == get_associated_token_address_with_program_id(authority, &mint, &program))
    .then_some(mint)
}

fn is_token_program(program: &Pubkey) -> bool {
    program == &anchor_spl::token::ID || program == &anchor_spl::token_2022::ID
}

/// Where a token account keeps its owner, its delegate and its close
/// authority, under either token program: the owner as a key, the others
/// each as a four-byte tag (zero for none) and a key.
const OWNER: core::ops::Range<usize> = 32..64;
const DELEGATE_TAG: core::ops::Range<usize> = 72..76;
const CLOSE_AUTHORITY_TAG: core::ops::Range<usize> = 129..133;
const TOKEN_ACCOUNT_LEN: usize = 165;

/// The token accounts a call could change that belong to one of `owners`
/// (the venue's authority and the station itself): those it is given
/// writable that a token program owns with one of them as their owner, read
/// before the call. A call can't change an account it is given read-only,
/// nor reach one it isn't given at all.
pub struct TokenAccounts<'a, 'info> {
    held: Vec<(&'a AccountInfo<'info>, Pubkey)>,
}

impl<'a, 'info> TokenAccounts<'a, 'info> {
    /// `accounts` holds every account `ix` names.
    pub fn before(
        ix: &Instruction,
        accounts: &'a [AccountInfo<'info>],
        owners: &[&Pubkey],
    ) -> Self {
        let mut held = Vec::new();
        for meta in ix.accounts.iter().filter(|meta| meta.is_writable) {
            let Some(info) = accounts.iter().find(|info| info.key == &meta.pubkey) else {
                continue;
            };
            if !is_token_program(info.owner) {
                continue;
            }
            let owner = info.try_borrow_data().ok().and_then(|data| {
                (data.len() >= TOKEN_ACCOUNT_LEN)
                    .then(|| data.get(OWNER).and_then(|o| Pubkey::try_from(o).ok()))
                    .flatten()
            });
            if let Some(owner) = owner.filter(|owner| owners.contains(&owner)) {
                held.push((info, owner));
            }
        }
        Self { held }
    }

    /// After the call: each is still a token account of the same owner's,
    /// with no delegate and no close authority, so nobody but the station
    /// can move or close it.
    pub fn still_held(&self) -> Result<()> {
        for (info, owner) in &self.held {
            let data = info.try_borrow_data()?;
            let held = is_token_program(info.owner)
                && data.len() >= TOKEN_ACCOUNT_LEN
                && data.get(OWNER) == Some(owner.as_ref())
                && data.get(DELEGATE_TAG) == Some(&[0; 4][..])
                && data.get(CLOSE_AUTHORITY_TAG) == Some(&[0; 4][..]);
            require!(held, StationError::LeftOpen);
        }
        Ok(())
    }
}

/// After a venue call: the station's authority at the venue is still what it
/// always is, an account of the system program's with no data. Only then do
/// the token accounts it owns answer to the station's signature alone; an
/// authority given data and another owner (a token multisig, say) would let
/// that owner's keys move them. A call that can't write the authority can't
/// change it, and [`check`] never lets one write it; this holds the line
/// even so.
pub fn authority_untouched(authority: &AccountInfo) -> Result<()> {
    require!(
        authority.owner == &system_program::ID && authority.data_is_empty(),
        StationError::AuthorityChanged
    );
    Ok(())
}

/// The instruction a driver prepared, rebuilt from the accounts it was given.
/// Only the station's authority at the venue signs; nothing else a driver
/// names can.
pub fn rebuild(
    prepared: &Prepared,
    given: &[AccountInfo],
    authority: &Pubkey,
) -> Result<Instruction> {
    let program = given
        .get(prepared.program as usize)
        .ok_or(StationError::BadCall)?;
    // The data first and the accounts after, so that dropping the instruction
    // (accounts, then data) gives both back to the station's heap, which
    // reclaims only its most recent allocation.
    let data = prepared.data.clone();
    let mut accounts = Vec::with_capacity(prepared.accounts.len());
    for meta in &prepared.accounts {
        let info = given
            .get(meta.index as usize)
            .ok_or(StationError::BadCall)?;
        accounts.push(AccountMeta {
            pubkey: *info.key,
            is_signer: info.key == authority,
            is_writable: meta.writable,
        });
    }
    Ok(Instruction {
        program_id: *program.key,
        accounts,
        data,
    })
}

/// The balance of a dollar account the caller has checked is the station's
/// own or its authority's at a venue.
pub fn balance(dollars: &AccountInfo) -> Result<u64> {
    require_keys_eq!(
        *dollars.owner,
        anchor_spl::token::ID,
        StationError::WrongDollars
    );
    let data = dollars.try_borrow_data()?;
    let amount = data.get(64..72).ok_or(StationError::WrongDollars)?;
    Ok(u64::from_le_bytes(amount.try_into().unwrap()))
}

/// Asks a driver: `data` is the instruction, and `accounts` the accounts it
/// is handed followed by the driver program, every one read-only and none a
/// signer, so a driver can read but never act. Its answer is whatever it
/// left in the return data, which must decode whole.
pub fn ask<T: AnchorDeserialize>(accounts: &[AccountInfo], data: Vec<u8>) -> Result<T> {
    let (program, given) = accounts.split_last().ok_or(StationError::WrongDrivers)?;
    let ix = Instruction {
        program_id: *program.key,
        accounts: given
            .iter()
            .map(|info| AccountMeta::new_readonly(*info.key, false))
            .collect(),
        data,
    };
    // Whatever an earlier call left behind is not this driver's answer.
    set_return_data(&[]);
    invoke(&ix, accounts)?;
    // The station's heap reclaims only its most recent allocation. Dropped
    // here, the instruction's accounts and then its data are each the most
    // recent, so both are given back. The answer's bytes and what they
    // decode into are not: each ask leaves them on the heap for the rest of
    // the call, and the station's limits are measured with that.
    drop(ix);
    let (from, answer) = get_return_data().ok_or(StationError::DriverSilent)?;
    require_keys_eq!(from, *program.key, StationError::DriverSilent);
    T::try_from_slice(&answer).map_err(|_| error!(StationError::BadAnswer))
}
