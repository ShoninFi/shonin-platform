//! Containment: a venue's code, whatever it becomes, reaches only what the
//! station's authority at that venue owns.
//!
//! A program can call any program the transaction names anywhere, not only
//! those among its own accounts, so what the guard refuses to see in a venue
//! call can still be called from it. What stops that call reaching anyone
//! else's money is the one signature the venue holds: the station's authority
//! at that venue, a program-derived address kept for it alone. Another
//! venue's accounts answer only to that venue's own authority, and the
//! station's dollar account only to the station, which never signs a venue
//! call. So a venue lent its authority can't withdraw the station's account
//! at another venue, as itself or as that account's master, and can't move a
//! dollar out of another authority's dollar account or the station's.
//!
//! Each case here sends a venue call whose code has been replaced, through
//! the station with the approved key, with the other venue's program and
//! accounts named elsewhere in the same transaction, and checks it is
//! refused, by the runtime's signer check, by the other venue's master
//! check, or by the token program's owner check, and that nothing moved.
//!
//! The guard still refuses the programs no venue call has any use for,
//! anywhere among its accounts: the station, the vault's program, Shonin's
//! other programs and the loaders.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::InstructionData;
use anchor_spl::token::spl_token;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_test_driver::mode;
use shonin_test_hostile_venue::{attack, how};
use solana_instruction::error::InstructionError;
use solana_instruction::Instruction;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

/// The first venue, whose code is replaced, and the second, which keeps its
/// own.
const A: usize = 0;
const B: usize = 1;

/// What the token program answers when the signer isn't the account's owner.
const OWNER_MISMATCH: u32 = 4;

/// A station with a driver at the stand-in venue (A) and at its copy (B),
/// money at both and idle at the station, a gift sitting uncounted in B's
/// authority's dollar account; then A's upgrade authority replaces A's code
/// with the hostile venue's.
fn two_venues() -> (Env, Home) {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("two", &[Declared::local(), Declared::local_2()]);
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in [A, B] {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
    }
    env.give(&home.authority(B), 1_000_000);
    env.turn_venue_hostile();
    (env, home)
}

/// Everything a reach past venue A could move: B's dollars and the
/// station's account there, B's authority's dollar account, the station's
/// own dollar account and what it has counted, and A's authority's dollar
/// account, where a withdrawal would land.
#[derive(Debug, PartialEq, Eq)]
struct Holdings {
    b_vault: u64,
    b_cash: i128,
    b_dollars: u64,
    a_dollars: u64,
    station_dollars: u64,
    idle: u64,
}

fn holdings(env: &Env, home: &Home) -> Holdings {
    let b = shonin_local_venue_2::ID;
    Holdings {
        b_vault: env.token_balance(&shonin_local_venue::vault_address_of(&b)),
        b_cash: env
            .margin_account_at(&b, &home.authority(B))
            .map_or(0, |account| account.cash),
        b_dollars: env.token_balance(&home.authority_dollars(B)),
        a_dollars: env.token_balance(&home.authority_dollars(A)),
        station_dollars: env.token_balance(&home.dollars),
        idle: env.idle(home),
    }
}

/// A harmless instruction that names venue B's program and its accounts, so
/// they are in the transaction without being in venue A's call: B's driver
/// describing itself, handed them as accounts it ignores.
fn names_b(home: &Home) -> Instruction {
    let b = shonin_local_venue_2::ID;
    Instruction {
        program_id: shonin_local_driver_2::ID,
        accounts: vec![
            AccountMeta::new_readonly(b, false),
            AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&b), false),
            AccountMeta::new_readonly(shonin_local_venue::vault_address_of(&b), false),
            AccountMeta::new_readonly(
                shonin_local_venue::account_address_of(&b, &home.authority(B)),
                false,
            ),
            AccountMeta::new_readonly(home.authority(B), false),
            AccountMeta::new_readonly(home.authority_dollars(B), false),
        ],
        data: shonin_local_driver_2::instruction::Describe {}.data(),
    }
}

/// Venue A's pinned `place`, its three constrained accounts as they must be,
/// then `extra`; the first order names `target` and picks the attack by its
/// limit price.
fn place_attacking(
    home: &Home,
    target: Pubkey,
    which: u64,
    extra: Vec<AccountMeta>,
) -> Instruction {
    let a = shonin_local_venue::ID;
    let mut accounts = vec![
        AccountMeta::new_readonly(home.authority(A), false),
        AccountMeta::new(
            shonin_local_venue::account_address_of(&a, &home.authority(A)),
            false,
        ),
        AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&a), false),
    ];
    accounts.extend(extra);
    Instruction {
        program_id: a,
        accounts,
        data: shonin_local_venue::instruction::Place {
            orders: vec![OrderRequest {
                market: target.to_bytes(),
                is_buy: true,
                size: 1,
                limit_price: which,
                reduce_only: false,
            }],
        }
        .data(),
    }
}

/// The accounts after A's three for a withdrawal from the station's account
/// at B, laid out as B's `withdraw` takes them, landing in A's authority's
/// dollar account; then `master` for the attack that names one.
fn withdrawal_from_b(env: &Env, home: &Home, master: Pubkey) -> Vec<AccountMeta> {
    let b = shonin_local_venue_2::ID;
    vec![
        AccountMeta::new(
            shonin_local_venue::account_address_of(&b, &home.authority(B)),
            false,
        ),
        AccountMeta::new(home.authority_dollars(A), false),
        AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&b), false),
        AccountMeta::new(shonin_local_venue::vault_address_of(&b), false),
        AccountMeta::new_readonly(env.mint, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(master, false),
    ]
}

/// The accounts after A's three for a transfer of one dollar-unit from
/// `from` to A's authority's dollar account, naming `owner` as the signer
/// for the attack that names one.
fn transfer_from(home: &Home, from: Pubkey, owner: Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(from, false),
        AccountMeta::new(home.authority_dollars(A), false),
        AccountMeta::new_readonly(owner, false),
        AccountMeta::new_readonly(spl_token::ID, false),
    ]
}

/// The executor sends `venue_ix` through driver A, B's program and accounts
/// named by another instruction of the same transaction.
fn send_beside_b(env: &mut Env, home: &Home, venue_ix: Instruction) -> TxResult {
    let executor = env.executor.insecure_clone();
    let call = env.ix_execute(home, &executor.pubkey(), A as u8, venue_ix);
    env.send(vec![names_b(home), call], &[&executor])
}

fn instruction_error(result: &TxResult) -> Option<InstructionError> {
    match result {
        Err(failed) => match &failed.err {
            TransactionError::InstructionError(_, error) => Some(error.clone()),
            _ => None,
        },
        Ok(_) => None,
    }
}

fn logs(result: &TxResult) -> String {
    match result {
        Ok(meta) => meta.logs.join("\n"),
        Err(failed) => failed.meta.logs.join("\n"),
    }
}

#[test]
fn a_venue_cannot_withdraw_the_stations_account_at_another_venue_as_itself() {
    let (mut env, home) = two_venues();
    let before = holdings(&env, &home);
    let b = shonin_local_venue_2::ID;
    let extra = withdrawal_from_b(&env, &home, home.authority(B));
    let ix = place_attacking(&home, b, attack::WITHDRAW_AS_ITSELF, extra);

    // With B nowhere in the transaction, A can't call it at all.
    let executor = env.executor.insecure_clone();
    let alone = env.ix_execute(&home, &executor.pubkey(), A as u8, ix.clone());
    let result = env.send(vec![alone], &[&executor]);
    assert_eq!(
        instruction_error(&result),
        Some(InstructionError::MissingAccount)
    );

    // Named elsewhere in the transaction, B is called, with A's authority as
    // the master, and refuses: the account belongs to B's own authority.
    let result = send_beside_b(&mut env, &home, ix);
    assert!(
        logs(&result).contains(&format!("Program {b} invoke [3]")),
        "B was never called:\n{}",
        logs(&result)
    );
    assert_eq!(
        code_of(&result),
        Some(anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds)),
        "{}",
        logs(&result)
    );
    assert_eq!(holdings(&env, &home), before);
}

#[test]
fn a_venue_cannot_withdraw_the_stations_account_at_another_venue_as_its_master() {
    let (mut env, home) = two_venues();
    let before = holdings(&env, &home);
    let b = shonin_local_venue_2::ID;
    // As B's authority, the account's master: the runtime refuses a
    // signature A was never lent.
    let extra = withdrawal_from_b(&env, &home, home.authority(B));
    let ix = place_attacking(&home, b, attack::WITHDRAW_AS_ITS_MASTER, extra);
    let result = send_beside_b(&mut env, &home, ix);
    assert_eq!(
        instruction_error(&result),
        Some(InstructionError::PrivilegeEscalation),
        "{}",
        logs(&result)
    );
    assert_eq!(holdings(&env, &home), before);

    // As the station: its address has no place in a venue call, and it
    // never signs one.
    let extra = withdrawal_from_b(&env, &home, home.station);
    let ix = place_attacking(&home, b, attack::WITHDRAW_AS_ITS_MASTER, extra);
    expect_code(
        send_beside_b(&mut env, &home, ix),
        station_error(StationError::StationOutOfPlace),
    );
    assert_eq!(holdings(&env, &home), before);
}

#[test]
fn a_venue_cannot_move_another_authoritys_dollars() {
    let (mut env, home) = two_venues();
    let before = holdings(&env, &home);
    assert_eq!(before.b_dollars, 1_000_000);
    let from = home.authority_dollars(B);

    // Signed by A's authority, which doesn't own it.
    let extra = transfer_from(&home, from, home.authority(B));
    let ix = place_attacking(&home, spl_token::ID, attack::TAKE, extra);
    let result = send_beside_b(&mut env, &home, ix);
    assert_eq!(code_of(&result), Some(OWNER_MISMATCH), "{}", logs(&result));
    assert_eq!(holdings(&env, &home), before);

    // Naming its owner, B's authority, as the signer.
    let extra = transfer_from(&home, from, home.authority(B));
    let ix = place_attacking(&home, spl_token::ID, attack::TAKE_AS_ITS_OWNER, extra);
    let result = send_beside_b(&mut env, &home, ix);
    assert_eq!(
        instruction_error(&result),
        Some(InstructionError::PrivilegeEscalation),
        "{}",
        logs(&result)
    );
    assert_eq!(holdings(&env, &home), before);
}

#[test]
fn a_venue_cannot_move_the_stations_own_dollars() {
    let (mut env, home) = two_venues();
    let before = holdings(&env, &home);
    assert!(before.station_dollars > 0);

    // The station's dollar account has no place in a venue call, so the
    // call is refused before the venue runs, whoever it names as the signer
    // (A's authority, which the token program would refuse as not its
    // owner, as it does for another authority's dollars above).
    let extra = transfer_from(&home, home.dollars, home.authority(B));
    let ix = place_attacking(&home, spl_token::ID, attack::TAKE, extra);
    expect_code(
        send_beside_b(&mut env, &home, ix),
        station_error(StationError::StationOutOfPlace),
    );
    assert_eq!(holdings(&env, &home), before);

    // Or its owner, the station, whose address has no place in a venue call
    // either.
    let extra = transfer_from(&home, home.dollars, home.station);
    let ix = place_attacking(&home, spl_token::ID, attack::TAKE_AS_ITS_OWNER, extra);
    expect_code(
        send_beside_b(&mut env, &home, ix),
        station_error(StationError::StationOutOfPlace),
    );
    assert_eq!(holdings(&env, &home), before);
}

/// A venue reaches the token program when its own accounts don't name it,
/// since the station's instruction does: what it can do there is only what
/// its authority's signature allows, on its authority's own accounts, and
/// the station refuses even that when it leaves one open to someone else.
#[test]
fn a_venue_reaches_a_program_its_accounts_dont_name_and_still_only_as_its_authority() {
    let (mut env, home) = two_venues();
    let before = holdings(&env, &home);
    let mut ix = env.ix_venue_deposit(
        &home.authority(A),
        &home.authority_dollars(A),
        how::DELEGATE,
    );
    // The stand-in's `deposit` takes the token program at a position its
    // rule leaves to the executor.
    let token_program_at = 6;
    assert_eq!(ix.accounts[token_program_at].pubkey, spl_token::ID);
    ix.accounts[token_program_at] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
    assert!(ix.accounts.iter().all(|meta| meta.pubkey != spl_token::ID));
    let result = env.execute_funded(&home, ix, 0);
    assert!(
        logs(&result).contains(&format!("Program {} invoke [3]", spl_token::ID)),
        "the token program was never called:\n{}",
        logs(&result)
    );
    expect_code(result, station_error(StationError::LeftOpen));
    assert_eq!(holdings(&env, &home), before);
}

// Programs no venue call may name ----------------------------------------------

/// Every program a venue call may never name, wherever it stands.
fn never_named(env: &mut Env) -> Vec<(&'static str, Pubkey)> {
    let _ = env;
    vec![
        ("the station", shonin_station::ID),
        ("the vault's program", shonin_test_vault::ID),
        ("the registry", shonin_registry::ID),
        ("publishing", shonin_publishing::ID),
        ("prices", shonin_prices::ID),
        ("profiles", shonin_profiles::ID),
        ("the development checker", shonin_dev_verifier::ID),
        (
            "the original loader",
            solana_sdk_ids::bpf_loader_deprecated::ID,
        ),
        ("the loader", solana_sdk_ids::bpf_loader::ID),
        (
            "the upgradeable loader",
            solana_sdk_ids::bpf_loader_upgradeable::ID,
        ),
        ("the fourth loader", solana_sdk_ids::loader_v4::ID),
        ("the native loader", solana_sdk_ids::native_loader::ID),
    ]
}

#[test]
fn a_venue_call_naming_a_program_no_venue_needs_is_refused_wherever_it_stands() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    let place = |env: &Env| {
        env.ix_venue_place(
            &home.authority(0),
            vec![order(spot(), true, UNIT / 100, PRICE)],
        )
    };
    for (what, program) in never_named(&mut env) {
        // At the end, and among the markets, where the venue reads nothing.
        for at_end in [true, false] {
            let mut ix = place(&env);
            let meta = AccountMeta::new_readonly(program, false);
            if at_end {
                ix.accounts.push(meta);
            } else {
                ix.accounts.insert(3, meta);
            }
            let result = env.execute(&home, ix);
            assert_eq!(
                failed_code(&result),
                Some(station_error(StationError::ReachesPinned)),
                "a call naming {what} was not refused"
            );
        }
    }
    assert_eq!(env.position(&home.authority(0), spot()), 0);
    // The programs a venue's own instructions take to move and create its
    // accounts pass.
    for program in [
        spl_token::ID,
        anchor_spl::token_2022::ID,
        anchor_spl::associated_token::ID,
        solana_sdk_ids::system_program::ID,
    ] {
        let mut ix = place(&env);
        ix.accounts.push(AccountMeta::new_readonly(program, false));
        expect_ok(env.execute(&home, ix));
    }
    assert_eq!(
        env.position(&home.authority(0), spot()),
        4 * (UNIT / 100) as i128
    );
}

/// The same for a driver's close: a close whose `place` names one of them is
/// undone.
#[test]
fn a_drivers_close_naming_a_program_no_venue_needs_is_refused() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("names", &[Declared::test()]);
    env.start_trading(&home);
    env.set_mode(mode::PASS_ON_LAST);
    let with_last = |env: &Env, last: Pubkey| {
        let (mut accounts, mut sizes) = env.groups(&home, &home.drivers);
        accounts.push(AccountMeta::new_readonly(last, false));
        sizes[0] += 1;
        env.ix_unwind_with(&home, &home.vault, WAD / 4, false, (accounts, sizes))
    };
    for (what, program) in never_named(&mut env) {
        let ix = with_last(&env, program);
        let result = env.as_vault(&home, ix);
        assert_eq!(
            code_of(&result),
            Some(station_error(StationError::ReachesPinned)),
            "a close naming {what} was not refused"
        );
    }
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
}

// Funding a venue call -------------------------------------------------------

#[test]
fn an_exit_is_never_funded() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    let idle = env.idle(&home);
    let withdraw = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), 1);
    expect_code(
        env.execute_funded(&home, withdraw.clone(), 1),
        station_error(StationError::FundedExit),
    );
    assert_eq!(env.idle(&home), idle);
    assert_eq!(env.usd_balance(&home.authority(0)), 0);
    // Unfunded, it brings its dollar home and counts it.
    expect_ok(env.execute(&home, withdraw));
    assert_eq!(env.idle(&home), idle + 1);
}

/// Funding is lent for the one call: what the call doesn't use comes straight
/// back to the station's dollar account and is counted again, and the venue
/// authority's dollar account is left as it was found.
#[test]
fn what_a_funded_call_leaves_unused_comes_back_counted() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    let idle = env.idle(&home);
    let station_dollars = env.usd_balance(&home.station);
    let executor = env.executor.insecure_clone();

    // A trade that takes none of it.
    let place = env.ix_venue_place(
        &home.authority(0),
        vec![order(spot(), true, UNIT / 100, PRICE)],
    );
    let meta = expect_ok(env.execute_funded(&home, place, 1_000_000));
    let executed: Vec<shonin_station::events::Executed> = events(&meta.logs);
    assert_eq!(executed.len(), 1);
    assert_eq!(
        (executed[0].funded, executed[0].returned),
        (1_000_000, 1_000_000)
    );
    assert_eq!(env.idle(&home), idle);
    assert_eq!(env.usd_balance(&home.station), station_dollars);
    assert_eq!(env.usd_balance(&home.authority(0)), 0);

    // A deposit that takes part of it.
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 400_000);
    let ix = env.ix_execute_funded(&home, &executor.pubkey(), 0, deposit, 1_000_000);
    let meta = expect_ok(env.send(vec![ix], &[&executor]));
    let executed: Vec<shonin_station::events::Executed> = events(&meta.logs);
    assert_eq!(
        (executed[0].funded, executed[0].returned),
        (1_000_000, 600_000)
    );
    assert_eq!(env.idle(&home), idle - 400_000);
    assert_eq!(env.usd_balance(&home.authority(0)), 0);

    // Never more than the station has counted.
    let idle = env.idle(&home);
    let place = env.ix_venue_place(
        &home.authority(0),
        vec![order(spot(), true, UNIT / 100, PRICE)],
    );
    expect_code(
        env.execute_funded(&home, place, idle + 1),
        station_error(StationError::Unaccounted),
    );
}

/// Dollars someone sends a venue authority's dollar account unasked are
/// counted by nobody and spent by nothing: not funding, not a deposit, not
/// the station's value, and they stay where they were sent.
#[test]
fn a_gift_to_a_venue_authority_counts_for_nothing_and_is_never_spent() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    let value = env.value(&home);
    let idle = env.idle(&home);
    env.give(&home.authority(0), 5_000_000);
    assert_eq!(env.value(&home), value);

    // Spent by a deposit with nothing funded: refused.
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
    expect_code(
        env.execute(&home, deposit),
        station_error(StationError::Unaccounted),
    );
    // A deposit of more than it was funded: refused.
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 2);
    expect_code(
        env.execute_funded(&home, deposit, 1),
        station_error(StationError::Unaccounted),
    );
    // A withdrawal beside it brings home only what came from the venue.
    let withdraw = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), 7);
    expect_ok(env.execute(&home, withdraw));
    assert_eq!(env.idle(&home), idle + 7);
    assert_eq!(env.usd_balance(&home.authority(0)), 5_000_000);

    // A full unwind sends home everything counted, and not the gift.
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(outcome.sent_home, FUNDS);
    assert_eq!(env.usd_balance(&home.authority(0)), 5_000_000);
}
