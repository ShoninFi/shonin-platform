//! The guard: the station signs a venue instruction, as its authority at
//! that venue, only if it is for the pinned driver's venue program, starts
//! with a discriminator on that driver's allowlist, has the authority's own
//! accounts, or the venue's accounts the allowlist pinned, at every position
//! the rule names, the authority nowhere else and never writable, the
//! station's own address and its own dollar account nowhere at all, and comes
//! from the one key approved for that driver, still
//! blessed, while the station runs; and a revoked driver's instructions that
//! trade are refused while those that only bring money back still pass.
//! Everything outside that is refused, and each test here walks every case of
//! its kind, not a sample: every program the fixture knows, every byte of
//! every discriminator, every constrained position of every rule with every
//! substitute, every position for the station's address and for the
//! authority's, every kind of key.
//!
//! And whatever a venue's code does with the signature it is lent, no call
//! leaves a token account of the authority's open to anyone else: no
//! delegate, no close authority, no other owner, whether the executor's call
//! or a driver's close made it; and none goes through once the authority is
//! anything but a system account with no data.

use anchor_lang::prelude::{AccountInfo, AccountMeta, Pubkey};
use anchor_lang::InstructionData;
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token;
use shonin_driver::{Class, Own, Position, Rule};
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::guard;
use shonin_station::state::Pinned;
use shonin_test_driver::mode;
use shonin_test_hostile_venue::{attack, how};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// A station trading at the venue, with spare counted dollars at the
/// station, and a second vault's station beside it to borrow accounts from.
fn world() -> (Env, Home, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    let other = env.open_vault("other");
    expect_ok(env.create_station(&other, EXIT_BAND, &[Declared::local()]));
    let other = env.home_of("other", &[shonin_local_driver::ID]);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, other.vault.to_bytes(), other.bundle);
    expect_ok(env.pull(&other, FUNDS));
    env.approve_executor(&other);
    env.fund_venue(&other, FUNDS / 2);
    (env, home, other)
}

/// Every instruction on the stand-in driver's allowlist, as the executor
/// would send it for `home`'s station: each names the station's authority at
/// the venue as the master, and its dollar account.
fn allowed(env: &Env, home: &Home) -> Vec<(&'static str, Instruction)> {
    let executor = env.executor.pubkey();
    let (authority, dollars) = (home.authority(0), home.authority_dollars(0));
    vec![
        ("open", env.ix_venue_open(&executor, &authority)),
        ("close", env.ix_venue_close(&authority, &executor)),
        ("deposit", env.ix_venue_deposit(&authority, &dollars, 1)),
        ("withdraw", env.ix_venue_withdraw(&authority, &dollars, 1)),
        (
            "place",
            env.ix_venue_place(&authority, vec![order(spot(), true, UNIT / 100, PRICE)]),
        ),
    ]
}

/// What a deposit in [`allowed`] needs funded: the one dollar-unit it moves.
fn funding(what: &str) -> u64 {
    u64::from(what == "deposit")
}

fn pinned(env: &Env, home: &Home) -> Pinned {
    env.station(home).drivers[0].clone()
}

#[test]
fn every_instruction_on_the_allowlist_goes_through_from_the_approved_key() {
    let (mut env, home, _) = world();
    // Withdraw and deposit, place, then open and close an account of another
    // master: each allowed instruction, signed by the station's authority.
    for (what, ix) in allowed(&env, &home) {
        if what == "open" || what == "close" {
            continue;
        }
        let result = env.execute_funded(&home, ix, funding(what));
        assert!(
            result.is_ok(),
            "{what} was refused: {:?}",
            result.err().map(|f| f.meta.logs)
        );
    }
    // Open and close: on a vault whose station has no venue account yet.
    let fresh = env.open_vault("fresh");
    expect_ok(env.create_station(&fresh, EXIT_BAND, &[Declared::local()]));
    let fresh = env.home_of("fresh", &[shonin_local_driver::ID]);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, fresh.vault.to_bytes(), fresh.bundle);
    env.approve_executor(&fresh);
    let open = env.ix_venue_open(&executor, &fresh.authority(0));
    expect_ok(env.execute(&fresh, open));
    assert!(env.margin_account(&fresh.authority(0)).is_some());
    let close = env.ix_venue_close(&fresh.authority(0), &executor);
    expect_ok(env.execute(&fresh, close));
    assert!(env.margin_account(&fresh.authority(0)).is_none());
}

#[test]
fn every_other_program_is_refused() {
    let (mut env, home, _) = world();
    let other_final = env.final_program();
    let programs = [
        anchor_spl::token::ID,
        anchor_spl::token_2022::ID,
        anchor_spl::associated_token::ID,
        solana_sdk_ids::system_program::ID,
        solana_sdk_ids::bpf_loader::ID,
        solana_sdk_ids::bpf_loader_upgradeable::ID,
        shonin_station::ID,
        shonin_test_vault::ID,
        shonin_registry::ID,
        shonin_publishing::ID,
        shonin_prices::ID,
        shonin_profiles::ID,
        shonin_dev_verifier::ID,
        shonin_local_driver::ID,
        shonin_test_driver::ID,
        other_final,
        Pubkey::new_unique(),
    ];
    for (what, allowed) in allowed(&env, &home) {
        for program in programs {
            let mut ix = allowed.clone();
            ix.program_id = program;
            let result = env.execute(&home, ix);
            assert_eq!(
                failed_code(&result),
                Some(station_error(StationError::WrongProgram)),
                "{what} sent to {program} was not refused as another program"
            );
        }
    }
}

/// The guard's own check, over every byte of every discriminator: the data
/// passes only when it starts with a rule's discriminator exactly.
#[test]
fn every_discriminator_off_the_allowlist_is_refused() {
    let (env, home, _) = world();
    let pinned = pinned(&env, &home);
    for (what, ix) in allowed(&env, &home) {
        let rule_length = 8;
        for position in 0..rule_length {
            for value in 0..=255u8 {
                let mut changed = ix.clone();
                if changed.data[position] == value {
                    continue;
                }
                changed.data[position] = value;
                let result = guard::check(
                    &changed,
                    &pinned,
                    std::slice::from_ref(&pinned),
                    &home.station,
                    &home.dollars,
                    &shonin_test_vault::ID,
                    &[],
                );
                // Changing a byte may land on another rule's discriminator;
                // then it is that instruction, checked as that one.
                let other_rule = pinned
                    .rules
                    .iter()
                    .any(|rule| changed.data.starts_with(&rule.discriminator));
                if !other_rule {
                    assert!(
                        result.is_err(),
                        "{what} with byte {position} set to {value} passed"
                    );
                }
            }
        }
        // Shorter than its discriminator, or empty: refused.
        for length in 0..8 {
            let mut short = ix.clone();
            short.data.truncate(length);
            let result = guard::check(
                &short,
                &pinned,
                std::slice::from_ref(&pinned),
                &home.station,
                &home.dollars,
                &shonin_test_vault::ID,
                &[],
            );
            assert!(result.is_err(), "{what} cut to {length} bytes passed");
        }
    }
}

/// And on the chain: a discriminator the venue has but the allowlist
/// doesn't name, and one nobody has, are refused before anything runs.
#[test]
fn an_instruction_the_allowlist_doesnt_name_is_refused_on_chain() {
    let (mut env, home, _) = world();
    let operator_only = env.ix_set_prices(spot(), PRICE, PRICE, PRICE, PRICE);
    for data in [
        operator_only.data.clone(),
        vec![0; 8],
        vec![0xff; 16],
        vec![],
        vec![1],
    ] {
        let mut ix = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
        ix.data = data.clone();
        let result = env.execute(&home, ix);
        assert_eq!(
            failed_code(&result),
            Some(station_error(StationError::NotOnTheAllowlist)),
            "data {data:?} was not refused"
        );
    }
}

/// Every position a rule constrains, with every substitute: a stranger's
/// key, the other station's account of the same kind, the vault's dollar
/// account, and each of this authority's other accounts. (The station's own
/// dollar account is refused at every position, constrained or not:
/// [`the_stations_own_dollar_account_is_never_in_a_venue_call`].)
#[test]
fn a_foreign_account_where_the_authoritys_own_must_stand_is_refused() {
    let (mut env, home, other) = world();
    let pinned = pinned(&env, &home);
    let own_account = shonin_local_venue::account_address(&home.authority(0));
    let other_account = shonin_local_venue::account_address(&other.authority(0));
    let stranger = Keypair::new().pubkey();
    let mut tried = 0;
    for (what, ix) in allowed(&env, &home) {
        let rule = pinned
            .rules
            .iter()
            .find(|rule| ix.data.starts_with(&rule.discriminator))
            .unwrap();
        for position in &rule.positions {
            let at = position.index as usize;
            let substitutes: Vec<Pubkey> = match &position.own {
                // The authority is the signer: its substitutes are covered
                // by the station never signing as another.
                Own::Station => vec![
                    other.authority(0),
                    stranger,
                    home.vault,
                    home.authority_dollars(0),
                ],
                Own::StationDollars => vec![
                    other.authority_dollars(0),
                    home.vault_dollars,
                    stranger,
                    own_account,
                ],
                Own::StationAddress(_) => {
                    vec![other_account, stranger, home.authority_dollars(0)]
                }
                Own::StationToken(_) => {
                    vec![other.authority_dollars(0), home.vault_dollars, stranger]
                }
                Own::Fixed(_) => vec![other_account, stranger, home.authority_dollars(0)],
            };
            for substitute in substitutes {
                let mut changed = ix.clone();
                changed.accounts[at] = AccountMeta {
                    pubkey: substitute,
                    is_signer: false,
                    is_writable: changed.accounts[at].is_writable,
                };
                let result = env.execute(&home, changed);
                tried += 1;
                assert!(
                    failed_code(&result) == Some(station_error(StationError::ForeignAccount)),
                    "{what} with {substitute} at {at} was not refused: {:?}",
                    result.as_ref().err().map(|f| &f.err)
                );
            }
        }
    }
    assert!(tried >= 20, "only {tried} substitutions were tried");
}

/// The authority's address at every position no rule gives it, and the
/// station's own address at every position at all, in every allowed
/// instruction: refused. The authority is the one account the station signs
/// a venue call for, and the station's own address never signs one.
#[test]
fn the_authority_where_the_allowlist_doesnt_put_it_or_the_station_anywhere_is_refused() {
    let (mut env, home, _) = world();
    let pinned = pinned(&env, &home);
    let authority = home.authority(0);
    let mut tried = 0;
    for (what, ix) in allowed(&env, &home) {
        let rule = pinned
            .rules
            .iter()
            .find(|rule| ix.data.starts_with(&rule.discriminator))
            .unwrap();
        for at in 0..ix.accounts.len() {
            let signs_here = rule
                .positions
                .iter()
                .any(|position| position.index as usize == at && position.own == Own::Station);
            let mut placed = vec![home.station];
            if !signs_here {
                placed.push(authority);
            }
            for key in placed {
                let mut changed = ix.clone();
                changed.accounts[at] = AccountMeta::new_readonly(key, false);
                let result = env.execute(&home, changed);
                tried += 1;
                assert_eq!(
                    failed_code(&result),
                    Some(station_error(StationError::StationOutOfPlace)),
                    "{what} with {key} at {at} was not refused: {:?}",
                    result.as_ref().err().map(|f| &f.err)
                );
            }
        }
        // And either added past the end, where nothing names it.
        for key in [home.station, authority] {
            let mut longer = ix.clone();
            longer.accounts.push(AccountMeta::new_readonly(key, false));
            assert_eq!(
                failed_code(&env.execute(&home, longer)),
                Some(station_error(StationError::StationOutOfPlace)),
                "{what} with {key} appended was not refused"
            );
            tried += 1;
        }
    }
    assert!(tried >= 50, "only {tried} placements were tried");
}

/// An instruction cut short, so a position its rule constrains is missing.
#[test]
fn an_instruction_missing_a_constrained_account_is_refused() {
    let (mut env, home, _) = world();
    for (what, ix) in allowed(&env, &home) {
        let mut short = ix.clone();
        short.accounts.truncate(1);
        let result = env.execute(&home, short);
        assert!(
            failed_code(&result) == Some(station_error(StationError::ForeignAccount)),
            "{what} cut to one account was not refused: {:?}",
            result.as_ref().err().map(|f| &f.err)
        );
    }
}

#[test]
fn only_the_approved_key_still_blessed_sends_anything() {
    let (mut env, home, other) = world();
    let ix = |env: &Env| env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);

    // A stranger, an unwinder blessed for this vault, an executor blessed for
    // another vault: none is the key approved here.
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let unwinder = Keypair::new();
    env.svm.airdrop(&unwinder.pubkey(), 1_000_000_000).unwrap();
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let journal = env.journal_for(
        unwinder_image(),
        unwinder.pubkey(),
        env.leaf_for(home.vault.to_bytes(), home.bundle, unwinder.pubkey()),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(
        &journal,
        cluster,
        home.vault.to_bytes(),
        home.bundle,
        vec![],
    ));
    let elsewhere = Keypair::new();
    env.svm.airdrop(&elsewhere.pubkey(), 1_000_000_000).unwrap();
    env.bless_executor_key(elsewhere.pubkey(), other.vault.to_bytes(), other.bundle);
    for key in [&stranger, &unwinder, &elsewhere] {
        let tx = ix(&env);
        let result = env.execute_as(&home, key, 0, tx);
        assert_eq!(
            failed_code(&result),
            Some(station_error(StationError::NotTheKey)),
            "{} was let through",
            key.pubkey()
        );
    }
    // A driver the station doesn't have.
    let executor = env.executor.insecure_clone();
    let tx = ix(&env);
    expect_err(
        env.execute_as(&home, &executor, 1, tx),
        station_error(StationError::NotDeclared),
    );

    // The approved key once its blessing has lapsed.
    env.skip(BLESSING_TTL + 1);
    let tx = ix(&env);
    expect_err(
        env.execute(&home, tx),
        station_error(StationError::NotBlessed),
    );
}

#[test]
fn a_replaced_key_sends_nothing() {
    let (mut env, home, _) = world();
    let successor = Keypair::new();
    env.svm.airdrop(&successor.pubkey(), 1_000_000_000).unwrap();
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    expect_ok(env.approve_key_by(&home, &successor, &successor.pubkey()));
    let ix = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
    expect_err(
        env.execute_funded(&home, ix.clone(), 1),
        station_error(StationError::NotTheKey),
    );
    expect_ok(env.execute_as_funded(&home, &successor, 0, ix, 1));
}

#[test]
fn a_key_whose_image_or_project_is_revoked_sends_nothing() {
    for revoke_image in [true, false] {
        let (mut env, home, _) = world();
        if revoke_image {
            expect_ok(env.revoke_image(executor_image()));
        } else {
            expect_ok(env.revoke_project(project()));
        }
        let ix = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
        expect_err(
            env.execute_funded(&home, ix, 1),
            station_error(StationError::NotBlessed),
        );
    }
}

#[test]
fn a_stopped_station_sends_nothing() {
    let (mut env, home, _) = world();
    env.halt_and_stop(&home);
    for (what, ix) in allowed(&env, &home) {
        let result = env.execute(&home, ix);
        assert_eq!(
            failed_code(&result),
            Some(station_error(StationError::AlreadyStopped)),
            "{what} went through a stopped station"
        );
    }
}

#[test]
fn a_revoked_driver_trades_nothing_and_still_brings_money_back() {
    let (mut env, home, _) = world();
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    let pinned = pinned(&env, &home);
    for (what, ix) in allowed(&env, &home) {
        let class = pinned
            .rules
            .iter()
            .find(|rule| ix.data.starts_with(&rule.discriminator))
            .unwrap()
            .class;
        let result = env.execute(&home, ix);
        match class {
            Class::Trade => assert_eq!(
                failed_code(&result),
                Some(station_error(StationError::NotApproved)),
                "{what} traded through a revoked driver"
            ),
            Class::Exit if what == "close" => {
                // The account still holds money, so the venue refuses to
                // close it; the station let it through.
                assert_eq!(
                    failed_code(&result),
                    Some(venue_error(shonin_local_venue::VenueError::NotEmpty))
                );
            }
            Class::Exit => assert!(
                result.is_ok(),
                "{what} was refused through a revoked driver"
            ),
        }
    }
    // Everything comes back out through it.
    let cash = env.venue_cash(&home.authority(0)) as u64;
    let orders = vec![reduce(spot(), false, UNIT / 2, 0)];
    let place = env.ix_venue_place(&home.authority(0), orders);
    expect_err(
        env.execute(&home, place),
        station_error(StationError::NotApproved),
    );
    let idle = env.idle(&home);
    let withdraw = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), cash);
    expect_ok(env.execute(&home, withdraw));
    assert_eq!(env.idle(&home), idle + cash);
    assert_eq!(env.usd_balance(&home.authority(0)), 0);
}

/// Only counted dollars reach a venue. A stranger's transfer to the station's
/// dollar account can't be sent to the venue as funding, and one to the
/// authority's dollar account can't be spent by a call either: a call may
/// not leave that account below what was there before it. Both stay where
/// they were sent, uncounted.
#[test]
fn a_call_can_never_spend_dollars_the_station_has_not_counted() {
    let (mut env, home, _) = world();
    let (authority, dollars) = (home.authority(0), home.authority_dollars(0));
    let counted = env.idle(&home);
    env.give(&home.station, 1_000_000);
    env.give(&authority, 2_000_000);
    // Funding past what the station counted.
    let ix = env.ix_venue_deposit(&authority, &dollars, counted + 1);
    expect_err(
        env.execute_funded(&home, ix, counted + 1),
        station_error(StationError::Unaccounted),
    );
    // A deposit of more than it was funded with, out of the gift.
    let ix = env.ix_venue_deposit(&authority, &dollars, counted + 1);
    expect_err(
        env.execute_funded(&home, ix, counted),
        station_error(StationError::Unaccounted),
    );
    let ix = env.ix_venue_deposit(&authority, &dollars, 1);
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::Unaccounted),
    );
    // Everything it counted goes.
    let ix = env.ix_venue_deposit(&authority, &dollars, counted);
    expect_ok(env.execute_funded(&home, ix, counted));
    assert_eq!(env.idle(&home), 0);
    assert_eq!(env.usd_balance(&home.station), 1_000_000);
    assert_eq!(env.usd_balance(&authority), 2_000_000);
}

/// What the guard check sees off the chain is what it sees on it: the
/// check is the station's own function, given no account it doesn't need.
#[test]
fn the_guard_needs_no_account_but_the_instructions_own() {
    let (env, home, _) = world();
    let pinned = pinned(&env, &home);
    for (_, ix) in allowed(&env, &home) {
        let no_accounts: &[AccountInfo] = &[];
        assert!(guard::check(
            &ix,
            &pinned,
            std::slice::from_ref(&pinned),
            &home.station,
            &home.dollars,
            &shonin_test_vault::ID,
            no_accounts
        )
        .is_ok());
    }
}

// Accounts the allowlist pins ------------------------------------------------

/// A position the allowlist pins to one of the venue's own accounts takes
/// that account and no other: not the venue's other pinned account, not the
/// same account of a copy of the venue, not anyone's.
#[test]
fn a_position_pinned_to_a_fixed_account_refuses_any_other() {
    let (mut env, home, _) = world();
    let pinned = pinned(&env, &home);
    let copy = shonin_local_venue_2::ID;
    let candidates = [
        shonin_local_venue::venue_address(),
        shonin_local_venue::vault_address(),
        shonin_local_venue::venue_address_of(&copy),
        shonin_local_venue::vault_address_of(&copy),
        Keypair::new().pubkey(),
    ];
    let mut tried = 0;
    for (what, ix) in allowed(&env, &home) {
        let rule = pinned
            .rules
            .iter()
            .find(|rule| ix.data.starts_with(&rule.discriminator))
            .unwrap();
        for position in &rule.positions {
            let Own::Fixed(set) = position.own else {
                continue;
            };
            let at = position.index as usize;
            assert_eq!(pinned.fixed[set as usize], vec![ix.accounts[at].pubkey]);
            for substitute in candidates {
                if substitute == ix.accounts[at].pubkey {
                    continue;
                }
                let mut changed = ix.clone();
                changed.accounts[at].pubkey = substitute;
                tried += 1;
                assert_eq!(
                    failed_code(&env.execute(&home, changed)),
                    Some(station_error(StationError::ForeignAccount)),
                    "{what} with {substitute} at {at} was not refused"
                );
            }
        }
    }
    assert_eq!(
        tried,
        5 * 4,
        "deposit, withdraw and place pin five positions"
    );
}

/// The authority of the made-up drivers below.
const AUTHORITY: Pubkey = Pubkey::new_from_array([9; 32]);

/// A rule over one instruction, `[7]`, whose positions are `owns` in order,
/// pinned with `fixed`, for a station whose authority at the venue is
/// [`AUTHORITY`].
fn pinned_with(owns: Vec<Own>, fixed: Vec<Vec<Pubkey>>) -> Pinned {
    Pinned {
        name: [0; 32],
        program: Pubkey::new_unique(),
        venue: shonin_local_venue::ID,
        authority: AUTHORITY,
        authority_bump: 0,
        dollars: get_associated_token_address(&AUTHORITY, &Pubkey::new_from_array([3; 32])),
        key: Pubkey::default(),
        rules: vec![Rule {
            discriminator: vec![7],
            class: Class::Trade,
            positions: owns
                .into_iter()
                .enumerate()
                .map(|(index, own)| Position {
                    index: index as u8,
                    own,
                })
                .collect(),
        }],
        addresses: vec![],
        fixed,
    }
}

fn call(accounts: &[Pubkey]) -> Instruction {
    Instruction {
        program_id: shonin_local_venue::ID,
        accounts: accounts
            .iter()
            .map(|key| AccountMeta::new(*key, false))
            .collect(),
        data: vec![7],
    }
}

/// A position pinned to a set of accounts (one of the named pools, say)
/// takes any of them and nothing else.
#[test]
fn a_position_pinned_to_a_set_takes_any_of_it_and_nothing_else() {
    let station = Pubkey::new_unique();
    let (a, b, c) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );
    let pinned = pinned_with(vec![Own::Fixed(1)], vec![vec![c], vec![a, b]]);
    let check = |key: Pubkey| {
        guard::check(
            &call(&[key]),
            &pinned,
            std::slice::from_ref(&pinned),
            &station,
            &Pubkey::new_unique(),
            &shonin_test_vault::ID,
            &[],
        )
    };
    assert!(check(a).is_ok());
    assert!(check(b).is_ok());
    for other in [c, pinned.dollars, Pubkey::new_unique()] {
        assert!(check(other).is_err(), "{other} was taken");
    }
}

/// A token account of the authority's pinned to a mint is the authority's
/// own associated account for that mint, and for no other.
#[test]
fn an_authority_token_pinned_to_a_mint_takes_no_other_mint() {
    let station = Pubkey::new_unique();
    let (mint, other_mint) = (Pubkey::new_unique(), Pubkey::new_unique());
    let owner = spl_token::ID;
    // The authority's associated token account for `mint`, as a token
    // program keeps it: the mint, then the owner.
    let token_account = |mint: &Pubkey| {
        let mut data = vec![0u8; 165];
        data[..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(AUTHORITY.as_ref());
        (get_associated_token_address(&AUTHORITY, mint), data)
    };
    let (ours, mut ours_data) = token_account(&mint);
    let (theirs, mut theirs_data) = token_account(&other_mint);
    let (mut l1, mut l2) = (0u64, 0u64);
    let infos = [
        AccountInfo::new(
            &ours,
            false,
            true,
            &mut l1,
            &mut ours_data,
            &owner,
            false,
            0,
        ),
        AccountInfo::new(
            &theirs,
            false,
            true,
            &mut l2,
            &mut theirs_data,
            &owner,
            false,
            0,
        ),
    ];
    let check = |own: Own, key: Pubkey| {
        let pinned = pinned_with(vec![own], vec![vec![mint]]);
        guard::check(
            &call(&[key]),
            &pinned,
            std::slice::from_ref(&pinned),
            &station,
            &Pubkey::new_unique(),
            &shonin_test_vault::ID,
            &infos,
        )
        .is_ok()
    };
    assert!(check(Own::StationToken(Some(0)), ours));
    assert!(!check(Own::StationToken(Some(0)), theirs));
    // Without a mint pinned, any of the authority's own token accounts.
    assert!(check(Own::StationToken(None), ours));
    assert!(check(Own::StationToken(None), theirs));
}

// What a call leaves behind --------------------------------------------------

/// A station with counted dollars at its account and at the venue, after the
/// venue's upgrade authority has replaced its code with the hostile venue's,
/// and a stranger's gift in the authority's dollar account, so that account
/// holds something the venue's code could reach.
fn hostile_venue() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.give(&home.authority(0), 1_000_000);
    env.turn_venue_hostile();
    (env, home)
}

/// The approved key sends the pinned `deposit`, with every position its rule
/// constrains holding what it must, and the venue's new code does `how` to
/// the authority's dollar account in favour of the venue's settings, moving
/// nothing: refused, and the account, and the station's, are as they were.
fn a_deposit_that_moves_nothing_and_does(how: u64) {
    let (mut env, home) = hostile_venue();
    let dollars = home.authority_dollars(0);
    let before = env.svm.get_account(&dollars).unwrap();
    let main = env.svm.get_account(&home.dollars).unwrap();
    let ix = env.ix_venue_deposit(&home.authority(0), &dollars, how);
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::LeftOpen),
    );
    assert_eq!(env.svm.get_account(&dollars).unwrap(), before);
    assert_eq!(env.svm.get_account(&home.dollars).unwrap(), main);
    assert_eq!(env.idle(&home), FUNDS / 2);
}

#[test]
fn a_venue_call_that_moves_nothing_cannot_leave_a_delegate_over_the_authoritys_dollars() {
    a_deposit_that_moves_nothing_and_does(how::DELEGATE);
}

#[test]
fn a_venue_call_cannot_give_the_authoritys_dollars_a_close_authority() {
    a_deposit_that_moves_nothing_and_does(how::CLOSE_AUTHORITY);
}

#[test]
fn a_venue_call_cannot_hand_the_authoritys_dollars_to_another_owner() {
    a_deposit_that_moves_nothing_and_does(how::OWNER);
}

/// The same through a driver's close: the sweep's withdrawal, made with the
/// authority's signature, makes the venue's settings a delegate over the
/// authority's dollar account. The close is undone.
#[test]
fn a_drivers_close_cannot_leave_a_delegate_over_the_authoritys_dollars() {
    let (mut env, home) = hostile_venue();
    let dollars = home.authority_dollars(0);
    let before = env.svm.get_account(&dollars).unwrap();
    let main = env.svm.get_account(&home.dollars).unwrap();
    expect_err(
        env.unwind_as_vault(&home, WAD / 4),
        station_error(StationError::LeftOpen),
    );
    assert_eq!(env.svm.get_account(&dollars).unwrap(), before);
    assert_eq!(env.svm.get_account(&home.dollars).unwrap(), main);
    assert_eq!(env.idle(&home), FUNDS / 2);
}

// Programs a call may not be handed ------------------------------------------

/// A station with a driver at every venue, the executor approved for the
/// first and money in its account there.
fn every_venue() -> (Env, Home) {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("every", &Declared::one_per_venue());
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    expect_ok(env.approve_key_at(&home, &executor, 0, &executor.pubkey()));
    env.fund_venue_at(&home, 0, FUNDS / 2);
    (env, home)
}

/// A call to one venue that passes another venue the station pins, or any
/// driver program it pins, anywhere among its accounts is refused: no venue
/// call has a use for them. The same call without it, or passing its own
/// venue's program, goes through.
#[test]
fn a_call_to_one_venue_that_passes_another_pinned_program_is_refused() {
    let (mut env, home) = every_venue();
    let authority = home.authority(0);
    let place =
        |env: &Env| env.ix_venue_place(&authority, vec![order(spot(), true, UNIT / 100, PRICE)]);
    let pinned_programs = [
        shonin_local_venue_2::ID,
        shonin_local_venue_3::ID,
        shonin_local_driver::ID,
        shonin_local_driver_2::ID,
        shonin_local_driver_3::ID,
    ];
    let executor = env.executor.insecure_clone();
    for program in pinned_programs {
        // At the end, and in the middle, where the venue reads nothing.
        for at_end in [true, false] {
            let mut ix = place(&env);
            let meta = AccountMeta::new_readonly(program, false);
            if at_end {
                ix.accounts.push(meta);
            } else {
                ix.accounts.insert(3, meta);
            }
            expect_err(
                env.execute_as(&home, &executor, 0, ix),
                station_error(StationError::ReachesPinned),
            );
        }
    }
    assert_eq!(env.position(&authority, spot()), 0);
    // The call as the venue takes it.
    let ix = place(&env);
    expect_ok(env.execute_as(&home, &executor, 0, ix));
    // Passing the venue's own program is no reach past it.
    let mut ix = place(&env);
    ix.accounts
        .push(AccountMeta::new_readonly(shonin_local_venue::ID, false));
    expect_ok(env.execute_as(&home, &executor, 0, ix));
    assert_eq!(env.position(&authority, spot()), 2 * (UNIT / 100) as i128);
}

/// The same for a driver's close: a close whose `place` passes another
/// venue the station pins is undone, and the same close passing an account
/// that isn't a pinned program goes through.
#[test]
fn a_drivers_close_that_passes_another_pinned_program_is_refused() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("passes", &[Declared::test(), Declared::local_2()]);
    env.start_trading(&home);
    env.set_mode(mode::PASS_ON_LAST);
    let with_last = |env: &Env, last: Pubkey| {
        let (mut accounts, mut sizes) = env.groups(&home, &home.drivers);
        accounts.insert(sizes[0] as usize, AccountMeta::new_readonly(last, false));
        sizes[0] += 1;
        env.ix_unwind_with(&home, &home.vault, WAD / 4, false, (accounts, sizes))
    };
    for program in [shonin_local_venue_2::ID, shonin_local_driver_2::ID] {
        let ix = with_last(&env, program);
        expect_err(
            env.as_vault(&home, ix),
            station_error(StationError::ReachesPinned),
        );
    }
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    let ix = with_last(&env, Pubkey::new_unique());
    expect_ok(env.as_vault(&home, ix));
    assert_eq!(
        env.position(&home.authority(0), spot()),
        (UNIT - UNIT / 4) as i128
    );
}

// The authority's own account, and the station's dollar account ------------

/// Every allowed instruction with the authority, where its rule puts it,
/// marked writable: refused, and nothing moves. Signing is all a venue needs
/// of its authority; writable, it could also have the system program give
/// the authority data and a new owner.
#[test]
fn a_venue_call_is_never_handed_its_authority_writable() {
    let (mut env, home, _) = world();
    let authority = home.authority(0);
    let before = (
        env.idle(&home),
        env.token_balance(&home.dollars),
        env.position(&authority, spot()),
    );
    for (what, ix) in allowed(&env, &home) {
        let mut changed = ix.clone();
        let mut marked = 0;
        for meta in changed.accounts.iter_mut() {
            if meta.pubkey == authority {
                meta.is_writable = true;
                marked += 1;
            }
        }
        assert_eq!(marked, 1, "{what} names the authority once");
        let result = env.execute_funded(&home, changed, funding(what));
        assert_eq!(
            failed_code(&result),
            Some(station_error(StationError::AuthorityWritable)),
            "{what} went through with its authority writable: {:?}",
            result.as_ref().err().map(|f| &f.err)
        );
        // The same instruction as the allowlist means it still goes through.
        if what == "place" || what == "withdraw" {
            expect_ok(env.execute_funded(&home, ix, funding(what)));
        }
    }
    let placed = env.position(&authority, spot()) - before.2;
    assert_eq!(placed, (UNIT / 100) as i128);
    assert_eq!(env.idle(&home), before.0 + 1);
    assert!(env.svm.get_account(&authority).is_none_or(|account| {
        account.owner == solana_sdk_ids::system_program::ID && account.data.is_empty()
    }));
}

/// The same for a driver's close: a close whose `place` marks the authority
/// writable is undone, and nothing moves.
#[test]
fn a_drivers_close_is_never_handed_its_authority_writable() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("writable", &[Declared::test()]);
    env.start_trading(&home);
    env.set_mode(mode::AUTHORITY_WRITABLE);
    let vault = env.usd_balance(&home.vault);
    let venue = env.venue_vault_balance();
    expect_err(
        env.unwind_as_vault(&home, WAD / 4),
        station_error(StationError::AuthorityWritable),
    );
    assert_eq!(env.usd_balance(&home.vault), vault);
    assert_eq!(env.venue_vault_balance(), venue);
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
}

/// The size of a token multisig: how many signers it requires and names,
/// whether it is set up, and room for eleven signers' keys.
const MULTISIG_LEN: usize = 3 + 11 * 32;

/// A station with counted dollars at the venue and a stranger's gift in the
/// authority's dollar account, whose authority has been made what a venue
/// handed it as a writable signer could make it: through the system
/// program, given the size of a token multisig and the token program as its
/// owner, then set up by the token program as a one-of-one multisig over
/// `signer`. Written here directly, as the account the venue would leave.
fn authority_made_a_multisig(signer: &Keypair) -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.give(&home.authority(0), 1_000_000);
    let mut data = vec![0u8; MULTISIG_LEN];
    data[0] = 1; // signers required
    data[1] = 1; // signers named
    data[2] = 1; // set up
    data[3..35].copy_from_slice(signer.pubkey().as_ref());
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            home.authority(0),
            solana_account::Account {
                lamports,
                data,
                owner: spl_token::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    (env, home)
}

/// Why the authority is never handed writable: once it is a token multisig,
/// the token program lets that multisig's signers move every token account
/// the authority owns, with no station call at all.
#[test]
fn an_authority_made_a_token_multisig_answers_to_its_signers_and_not_the_station() {
    let signer = Keypair::new();
    let (mut env, home) = authority_made_a_multisig(&signer);
    env.svm.airdrop(&signer.pubkey(), 1_000_000_000).unwrap();
    let sink = env.token_account(&Keypair::new().pubkey());
    let ix = spl_token::instruction::transfer(
        &spl_token::ID,
        &home.authority_dollars(0),
        &sink,
        &home.authority(0),
        &[&signer.pubkey()],
        1_000_000,
    )
    .unwrap();
    expect_ok(env.send(vec![ix], &[&signer]));
    assert_eq!(env.token_balance(&sink), 1_000_000);
    assert_eq!(env.token_balance(&home.authority_dollars(0)), 0);
}

/// And whatever lets an authority become something other than a system
/// account with no data, the station makes no venue call with it afterwards:
/// neither the executor's nor a driver's close goes through, and nothing
/// moves.
#[test]
fn no_venue_call_goes_through_with_an_authority_that_is_no_longer_a_plain_account() {
    let (mut env, home) = authority_made_a_multisig(&Keypair::new());
    let (authority, dollars) = (home.authority(0), home.authority_dollars(0));
    let before = (
        env.idle(&home),
        env.token_balance(&home.dollars),
        env.token_balance(&dollars),
        env.venue_cash(&authority),
    );
    let withdraw = env.ix_venue_withdraw(&authority, &dollars, 1);
    expect_err(
        env.execute(&home, withdraw),
        station_error(StationError::AuthorityChanged),
    );
    let place = env.ix_venue_place(&authority, vec![order(spot(), true, UNIT / 100, PRICE)]);
    expect_err(
        env.execute(&home, place),
        station_error(StationError::AuthorityChanged),
    );
    expect_err(
        env.unwind_as_vault(&home, WAD / 4),
        station_error(StationError::AuthorityChanged),
    );
    let after = (
        env.idle(&home),
        env.token_balance(&home.dollars),
        env.token_balance(&dollars),
        env.venue_cash(&authority),
    );
    assert_eq!(after, before);
    assert_eq!(env.position(&authority, spot()), 0);
}

/// The authority as it always is passes the check made after every call;
/// given data, or another owner, it doesn't.
#[test]
fn the_check_after_a_call_takes_only_a_system_account_with_no_data() {
    let key = Pubkey::new_unique();
    let check = |owner: Pubkey, len: usize| {
        let (mut lamports, mut data) = (1_000_000u64, vec![0u8; len]);
        let info = AccountInfo::new(
            &key,
            true,
            false,
            &mut lamports,
            &mut data,
            &owner,
            false,
            0,
        );
        guard::authority_untouched(&info).is_ok()
    };
    assert!(check(solana_sdk_ids::system_program::ID, 0));
    assert!(!check(solana_sdk_ids::system_program::ID, 1));
    assert!(!check(spl_token::ID, 0));
    assert!(!check(spl_token::ID, MULTISIG_LEN));
    assert!(!check(Pubkey::new_unique(), 0));
}

/// The station's own dollar account at every position of every allowed
/// instruction, and past the end: refused. The station counts what reaches
/// that account only around its own transfers, so nothing a venue call sent
/// there would ever be counted.
#[test]
fn the_stations_own_dollar_account_is_never_in_a_venue_call() {
    let (mut env, home, _) = world();
    let mut tried = 0;
    for (what, ix) in allowed(&env, &home) {
        for at in 0..=ix.accounts.len() {
            let mut changed = ix.clone();
            let meta = AccountMeta::new(home.dollars, false);
            if at == ix.accounts.len() {
                changed.accounts.push(meta);
            } else {
                changed.accounts[at] = meta;
            }
            let result = env.execute(&home, changed);
            tried += 1;
            assert_eq!(
                failed_code(&result),
                Some(station_error(StationError::StationOutOfPlace)),
                "{what} with the station's dollars at {at} was not refused: {:?}",
                result.as_ref().err().map(|f| &f.err)
            );
        }
    }
    assert!(tried >= 30, "only {tried} placements were tried");
}

/// What that keeps out: a venue whose code sends the dollars it was funded
/// with to the station's own account rather than back to its authority's.
/// The call is refused, and nothing moves.
#[test]
fn a_venue_call_cannot_send_dollars_to_the_stations_own_account() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.turn_venue_hostile();
    let before = (
        env.idle(&home),
        env.token_balance(&home.dollars),
        env.token_balance(&home.authority_dollars(0)),
    );
    let venue = shonin_local_venue::ID;
    let authority = home.authority(0);
    let ix = Instruction {
        program_id: venue,
        accounts: vec![
            // The stand-in `place`'s three, as its rule pins them.
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(
                shonin_local_venue::account_address_of(&venue, &authority),
                false,
            ),
            AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&venue), false),
            // The hostile venue moves one dollar-unit from the first of these
            // to the second, signed by the authority.
            AccountMeta::new(home.authority_dollars(0), false),
            AccountMeta::new(home.dollars, false),
            AccountMeta::new_readonly(env.mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: shonin_local_venue::instruction::Place {
            orders: vec![OrderRequest {
                market: spl_token::ID.to_bytes(),
                is_buy: true,
                size: 1,
                limit_price: attack::TAKE,
                reduce_only: false,
            }],
        }
        .data(),
    };
    expect_err(
        env.execute_funded(&home, ix, 1_000),
        station_error(StationError::StationOutOfPlace),
    );
    let after = (
        env.idle(&home),
        env.token_balance(&home.dollars),
        env.token_balance(&home.authority_dollars(0)),
    );
    assert_eq!(after, before);
}

/// The same for a driver's close: a close whose `place` passes the station's
/// own dollar account is undone.
#[test]
fn a_drivers_close_naming_the_stations_own_dollar_account_is_refused() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("main", &[Declared::test()]);
    env.start_trading(&home);
    env.set_mode(mode::PASS_ON_LAST);
    let (mut accounts, mut sizes) = env.groups(&home, &home.drivers);
    accounts.push(AccountMeta::new_readonly(home.dollars, false));
    sizes[0] += 1;
    let ix = env.ix_unwind_with(&home, &home.vault, WAD / 4, false, (accounts, sizes));
    let vault = env.usd_balance(&home.vault);
    expect_err(
        env.as_vault(&home, ix),
        station_error(StationError::StationOutOfPlace),
    );
    assert_eq!(env.usd_balance(&home.vault), vault);
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
}
