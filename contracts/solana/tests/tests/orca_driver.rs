//! Orca's driver against Orca's real program: a station opens positions
//! through its guard, the driver values them at Pyth's price with the fees
//! they have earned, and a leaver's fraction is withdrawn, sold inside the
//! band and sent home while the stayers keep theirs; and everything that
//! should be refused is: a foreign destination or pool, a look-alike or
//! unfit price, a pool pushed outside the band, a position left out, a
//! delegate, a position at any mint but the authority's own slots, and a
//! sale the pool can't fill inside the band.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token::spl_token;
use shonin_driver::COMPUTE_PER_DRIVER;
use shonin_solana_tests::orca::*;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The station's range around the pool's price ($150): about $129 to $175.
const LOWER: i32 = -20_480;
const UPPER: i32 = -17_408;

fn world() -> (Env, Home) {
    let mut env = Env::with_orca();
    let home = env.orca_home("orca");
    (env, home)
}

/// A station with about ten SOL's worth in a position at slot 0, Pyth's
/// price where the pool's is.
fn providing() -> (Env, Home) {
    let (mut env, home) = world();
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    env.price_follows_pool();
    (env, home)
}

/// Bob trades back and forth, leaving the price about where it was and
/// fees with every provider in range.
fn trade_back_and_forth(env: &mut Env, rounds: usize) {
    for _ in 0..rounds {
        env.bob_swaps(20 * SOL, true);
        env.bob_swaps(3_000_000_000, false);
    }
    env.price_follows_pool();
}

fn wsol(env: &Env, home: &Home) -> u64 {
    env.token_amount(&env.wsol_of(&home.authority(0)))
}

#[test]
fn a_position_opened_through_the_station_is_valued_at_pyth_s_price() {
    let (mut env, home) = providing();
    let mint = env.slot_mint(&home, 0);
    let position = env.position_state(&mint).expect("the position is open");
    assert!(position.liquidity > 0);
    let value = env.value(&home);
    assert!(!value.outside, "{value:?}");
    // What the station holds is what it was sent, less the pool's fee on
    // what it bought and the price its own purchase moved.
    assert!(value.mark > FUNDS * 99 / 100, "{value:?}");
    assert!(value.mark <= FUNDS + FUNDS / 1_000, "{value:?}");
    // A sale would pay the pool's fee on the SOL, and no more than that.
    assert!(value.exit <= value.mark, "{value:?}");
    assert!(value.exit > value.mark - value.mark / 100, "{value:?}");
}

#[test]
fn the_valuation_counts_fees_earned_exactly_as_orca_credits_them() {
    let (mut env, home) = providing();
    let before = env.value(&home);
    trade_back_and_forth(&mut env, 3);
    let earned = env.value(&home);
    // Orca brings the position's fees up to date, which anyone may ask of
    // it: the valuation counted every unit of them already.
    let mint = env.slot_mint(&home, 0);
    let payer = env.payer.insecure_clone();
    let update = env.ix_update_fees(&mint);
    expect_ok(env.send(vec![update], &[&payer]));
    let position = env.position_state(&mint).unwrap();
    assert!(
        position.fee_owed_a > 0 && position.fee_owed_b > 0,
        "{position:?}"
    );
    let updated = env.value(&home);
    assert_eq!(earned, updated);
    // And the fees are worth something.
    assert!(earned.mark > before.mark, "{before:?} {earned:?}");
}

#[test]
fn the_driver_works_out_a_withdrawal_to_the_unit() {
    let (mut env, home) = providing();
    let mint = env.slot_mint(&home, 0);
    let position = env.position_state(&mint).unwrap();
    let state = env.pool_state();
    let take = position.liquidity / 3;
    let (a, b) = orca_math::withdrawal(state.tick, state.sqrt_price, LOWER, UPPER, take).unwrap();
    let (owner_a, owner_b) = env.owners(&home);
    let (sol_before, idle_before) = (wsol(&env, &home), env.idle(&home));
    let ix = env.ix_decrease(&home.authority(0), &mint, &owner_a, &owner_b, take, a, b);
    expect_ok(env.execute(&home, ix));
    // The SOL stays with the authority; the dollars come home to the
    // station, counted.
    assert_eq!(wsol(&env, &home) - sol_before, a);
    assert_eq!(env.idle(&home) - idle_before, b);
}

#[test]
fn a_leaver_s_fraction_is_withdrawn_sold_and_sent_home_the_stayers_keeping_theirs() {
    let (mut env, home) = providing();
    trade_back_and_forth(&mut env, 2);
    let mint = env.slot_mint(&home, 0);
    let liquidity = env.position_state(&mint).unwrap().liquidity;
    let before = env.value(&home);
    let quarter = WAD / 4;
    let (outcome, received, _) = ok(env.orca_unwind(&home, quarter));
    assert!(!outcome.blocked);
    assert_eq!(outcome.sent_home, received);
    // A quarter of what a sale would fetch, short at most the band.
    let due = before.exit / 4;
    assert!(received <= due, "{received} {due}");
    assert!(
        received >= due - due * EXIT_BAND as u64 / BPS,
        "{received} {due}"
    );
    // A quarter of the liquidity came out, rounded against the leaver.
    let left = env.position_state(&mint).unwrap();
    assert_eq!(left.liquidity, liquidity - liquidity / 4);
    // Every fee was collected: what is left is counted where it lies.
    assert_eq!((left.fee_owed_a, left.fee_owed_b), (0, 0));
    // The stayers keep three quarters of what it was worth, and lose nothing
    // to the leaver's sale.
    env.price_follows_pool();
    let after = env.value(&home);
    assert!(
        after.mark + after.mark / 1_000 >= before.mark - before.mark / 4,
        "{before:?} {after:?}"
    );
}

#[test]
fn a_full_close_leaves_only_an_empty_position_whose_slot_opens_again() {
    let (mut env, home) = providing();
    trade_back_and_forth(&mut env, 1);
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD));
    assert!(!outcome.blocked && received > 0);
    let mint = env.slot_mint(&home, 0);
    let left = env.position_state(&mint).unwrap();
    assert_eq!(
        (left.liquidity, left.fee_owed_a, left.fee_owed_b),
        (0, 0, 0)
    );
    // The sale held back at most a lamport an instruction.
    assert!(wsol(&env, &home) <= 4, "{}", wsol(&env, &home));
    // Nothing is left at Orca; what the sale fetched past the close's
    // worth stays counted at the station.
    let value = env.value(&home);
    assert!(value.mark - env.idle(&home) < 10, "{value:?}");

    // The executor closes the empty position, its rent back to itself, and
    // opens the slot again.
    let executor = env.executor.pubkey();
    let close = env.ix_close_position(&home.authority(0), &executor, &mint);
    expect_ok(env.execute(&home, close));
    assert!(env.position_state(&mint).is_none());
    assert!(env.svm.get_account(&mint).is_none_or(|a| a.lamports == 0));
    expect_ok(env.station_opens(&home, 0, LOWER, UPPER));
    assert!(env.position_state(&mint).is_some());
}

/// With one slot, a range moves in one go: the executor empties the
/// position, closes it and opens the slot again elsewhere, in one
/// transaction.
#[test]
fn a_range_moves_in_one_transaction_through_its_one_slot() {
    let (mut env, home) = providing();
    let mint = env.slot_mint(&home, 0);
    let position = env.position_state(&mint).unwrap();
    let (owner_a, owner_b) = env.owners(&home);
    let authority = home.authority(0);
    let executor = env.executor.insecure_clone();
    let key = executor.pubkey();
    let decrease = env.ix_decrease(
        &authority,
        &mint,
        &owner_a,
        &owner_b,
        position.liquidity,
        0,
        0,
    );
    let collect = env.ix_collect_fees(&authority, &mint, &owner_a, &owner_b);
    let close = env.ix_close_position(&authority, &key, &mint);
    let open = env.ix_open_position(&key, &authority, &mint, LOWER - 640, UPPER - 640);
    let ixs = vec![
        env.ix_execute(&home, &key, 0, decrease),
        env.ix_execute(&home, &key, 0, collect),
        env.ix_execute(&home, &key, 0, close),
        env.ix_execute(&home, &key, 0, open),
    ];
    expect_ok(env.send_fitting(ixs, &[&executor]));
    let moved = env.position_state(&mint).unwrap();
    assert_eq!(
        (moved.tick_lower, moved.tick_upper, moved.liquidity),
        (LOWER - 640, UPPER - 640, 0)
    );
    assert!(!env.value(&home).outside);
}

/// Someone else's liquidity just around the price, so that a sale crosses
/// ticks, as it does in a busy pool.
fn crowd_the_price(env: &mut Env) {
    let tick = env.pool_state().tick.div_euclid(SPACING) * SPACING;
    let bob = env.bob.insecure_clone();
    for offset in 1..4 {
        let mint = Keypair::new();
        // The first starts at the tick just below the price.
        let (lower, upper) = (tick - (offset - 1) * SPACING, tick + offset * SPACING);
        let open = env.ix_open_position(&bob.pubkey(), &bob.pubkey(), &mint.pubkey(), lower, upper);
        expect_ok(env.send(vec![open], &[&bob, &mint]));
        let liquidity = env.liquidity_for(lower, upper, SOL / 5, 30_000_000);
        let add = env.ix_increase(
            &bob.pubkey(),
            &mint.pubkey(),
            &env.wsol_of(&bob.pubkey()),
            &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
            liquidity,
            u64::MAX,
            u64::MAX,
        );
        expect_ok(env.send(vec![add], &[&bob]));
    }
}

/// The most a close may cost: every slot holding a narrow position around
/// the price with fees owed, the authority holding SOL besides, and the sale
/// crossing other providers' ticks.
#[test]
fn a_close_of_every_slot_fits_the_compute_a_driver_may_use() {
    for fraction in [WAD / 2, WAD] {
        let (mut env, home) = world();
        let tick = env.pool_state().tick.div_euclid(SPACING) * SPACING;
        for slot in 0..MAX_SLOTS {
            let width = (slot as i32 + 10) * SPACING;
            env.station_provides_at(&home, slot, tick - width, tick + width, 10 * SOL);
            env.price_follows_pool();
        }
        trade_back_and_forth(&mut env, 2);
        // The price just above a tick others provide from.
        let floor = env.pool_state().tick.div_euclid(SPACING) * SPACING;
        let bob = env.bob.insecure_clone();
        let push = env.ix_swap(
            &bob.pubkey(),
            &env.wsol_of(&bob.pubkey()),
            &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
            100 * SOL,
            orca_math::sqrt_price_at(floor + 3).unwrap(),
            true,
        );
        expect_ok(env.send(vec![push], &[&bob]));
        crowd_the_price(&mut env);
        env.price_follows_pool();
        let before = env.pool_state().tick;
        let (outcome, _, used) = ok(env.orca_unwind(&home, fraction));
        assert!(!outcome.blocked && outcome.sent_home > 0);
        // The sale crossed at least one initialized tick.
        let crossed = before.div_euclid(SPACING) * SPACING;
        assert!(
            env.pool_state().tick < crossed,
            "{before} {}",
            env.pool_state().tick
        );
        println!(
            "a close of {fraction} of {MAX_SLOTS} position at Orca, with fees, took {used} compute units"
        );
        assert!(used <= COMPUTE_PER_DRIVER, "{used}");
    }
}

/// What a close of everything fetches is what the valuation said it would,
/// to the unit: the driver works out the sale as Orca makes it.
#[test]
fn a_full_close_fetches_exactly_what_the_valuation_said() {
    for with_position in [false, true] {
        let (mut env, home) = world();
        crowd_the_price(&mut env);
        if with_position {
            env.station_provides(&home, LOWER, UPPER, 10 * SOL);
            trade_back_and_forth(&mut env, 1);
        } else {
            expect_ok(env.station_buys_sol(&home, 1_500_000_000));
            env.price_follows_pool();
        }
        let value = env.value(&home);
        let (outcome, received, _) = ok(env.orca_unwind(&home, WAD));
        assert_eq!(outcome.sent_home, received);
        // Everything the station counted comes home, the leftover of the
        // valuation's allowance for a leaver's sale included.
        assert!(received >= value.exit, "{received} {value:?}");
        assert!(
            received - value.exit < value.exit / 5_000,
            "{received} {value:?}"
        );
    }
}

// Refusals -------------------------------------------------------------------

#[test]
fn tokens_withdrawn_to_an_account_not_the_authority_s_are_refused() {
    let (mut env, home) = providing();
    let mint = env.slot_mint(&home, 0);
    let (_, owner_b) = env.owners(&home);
    let bob = env.wsol_of(&env.bob.pubkey());
    let ix = env.ix_decrease(&home.authority(0), &mint, &bob, &owner_b, 1_000, 0, 0);
    expect_code(
        env.execute(&home, ix),
        station_error(StationError::ForeignAccount),
    );
}

#[test]
fn a_pool_nobody_listed_is_refused() {
    let (mut env, home) = providing();
    let (owner_a, owner_b) = env.owners(&home);
    let mut ix = env.ix_swap(
        &home.authority(0),
        &owner_a,
        &owner_b,
        1_000,
        orca_math::MAX_SQRT_PRICE,
        false,
    );
    ix.accounts[2].pubkey = Pubkey::new_unique();
    expect_code(
        env.execute_funded(&home, ix, 1_000),
        station_error(StationError::ForeignAccount),
    );
}

#[test]
fn a_look_alike_price_account_is_refused() {
    let (mut env, home) = providing();
    let group = env.orca_group(&home.station);
    let at = Env::group_index(&group, &PRICE_ACCOUNT);
    // A copy of the price at another address, owned by Pyth's receiver.
    let copy = Pubkey::new_unique();
    let price = env.sol_price();
    let now = env.now() as i64;
    env.set_price_account(copy, PYTH_RECEIVER, FEED_ID, price, 1, now, true);
    let mut changed = group.clone();
    changed[at].pubkey = copy;
    expect_err(
        env.try_value_with(&home, changed),
        orca_error(OrcaError::WrongAccounts),
    );
    // The listed address, owned by anyone else.
    env.set_price_account(
        PRICE_ACCOUNT,
        Pubkey::new_unique(),
        FEED_ID,
        price,
        1,
        now,
        true,
    );
    expect_err(
        env.try_value_with(&home, group.clone()),
        orca_error(OrcaError::WrongAccounts),
    );
    // Another feed's price at the listed address.
    env.set_price_account(PRICE_ACCOUNT, PYTH_RECEIVER, [7; 32], price, 1, now, true);
    expect_err(
        env.try_value_with(&home, group),
        orca_error(OrcaError::WrongAccounts),
    );
}

#[test]
fn a_stale_partly_verified_or_uncertain_price_marks_the_valuation_outside_and_blocks_the_close() {
    let (mut env, home) = providing();
    let price = env.sol_price();
    let unfit: [&dyn Fn(&mut Env); 3] = [
        // Older than devnet's fifteen minutes.
        &|env: &mut Env| env.skip(901),
        &|env: &mut Env| env.set_price(price, price / 10_000, false),
        // Less certain than the 1% band.
        &|env: &mut Env| env.set_price(price, price / 50, true),
    ];
    for make_unfit in unfit {
        env.set_price(price, price / 10_000, true);
        assert!(!env.value(&home).outside);
        make_unfit(&mut env);
        assert!(env.value(&home).outside);
        let liquidity = env
            .position_state(&env.slot_mint(&home, 0))
            .unwrap()
            .liquidity;
        let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 4));
        assert!(outcome.blocked && received == 0);
        let after = env
            .position_state(&env.slot_mint(&home, 0))
            .unwrap()
            .liquidity;
        assert_eq!(after, liquidity);
    }
}

#[test]
fn a_pool_pushed_outside_the_band_marks_the_valuation_outside_and_blocks_the_close() {
    for down in [true, false] {
        let (mut env, home) = providing();
        // About 2% either way, Pyth's price staying where it was.
        if down {
            env.bob_swaps(60 * SOL, true);
        } else {
            env.bob_swaps(9_000_000_000, false);
        }
        let value = env.value(&home);
        assert!(value.outside, "{value:?}");
        let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 4));
        assert!(outcome.blocked && received == 0);
    }
}

#[test]
fn a_position_left_out_of_a_valuation_is_refused() {
    let (mut env, home) = providing();
    let group = env.orca_group(&home.station);
    let mint = env.slot_mint(&home, 0);
    let at = Env::group_index(&group, &position_address(&mint));
    // The slot passed as if it held nothing.
    let mut empty = group.clone();
    empty[at] = AccountMeta::new_readonly(mint, false);
    expect_err(
        env.try_value_with(&home, empty),
        orca_error(OrcaError::WrongAccounts),
    );
    // Its token account left out.
    let token =
        get_associated_token_address_with_program_id(&home.authority(0), &mint, &TOKEN_2022);
    let mut missing = group.clone();
    missing.retain(|meta| meta.pubkey != token);
    expect_err(
        env.try_value_with(&home, missing),
        orca_error(OrcaError::WrongAccounts),
    );
    // Its tick arrays left out.
    let mut no_arrays = group;
    let lower = tick_array_address(&pool(), array_start(LOWER));
    let last = no_arrays
        .iter()
        .rposition(|meta| meta.pubkey == lower)
        .unwrap();
    no_arrays.remove(last);
    expect_err(
        env.try_value_with(&home, no_arrays),
        orca_error(OrcaError::WrongAccounts),
    );
}

#[test]
fn a_delegate_on_a_position_s_token_is_refused_by_the_driver() {
    let (mut env, home) = providing();
    let mint = env.slot_mint(&home, 0);
    let token =
        get_associated_token_address_with_program_id(&home.authority(0), &mint, &TOKEN_2022);
    let mut account = env.svm.get_account(&token).unwrap();
    account.data[72..76].copy_from_slice(&[1, 0, 0, 0]);
    account.data[76..108].copy_from_slice(env.bob.pubkey().as_ref());
    account.data[121..129].copy_from_slice(&1u64.to_le_bytes());
    env.svm.set_account(token, account).unwrap();
    let group = env.orca_group(&home.station);
    expect_err(
        env.try_value_with(&home, group),
        orca_error(OrcaError::WrongAccounts),
    );
}

#[test]
fn a_position_a_stranger_delegated_to_the_authority_is_refused() {
    let (mut env, home) = providing();
    // Bob opens a position and makes the station's authority its delegate,
    // which Orca would honour.
    let bob = env.bob.insecure_clone();
    let mint = Keypair::new();
    let open = env.ix_open_position(&bob.pubkey(), &bob.pubkey(), &mint.pubkey(), LOWER, UPPER);
    expect_ok(env.send(vec![open], &[&bob, &mint]));
    let bobs =
        get_associated_token_address_with_program_id(&bob.pubkey(), &mint.pubkey(), &TOKEN_2022);
    let approve = anchor_spl::token_2022::spl_token_2022::instruction::approve(
        &TOKEN_2022,
        &bobs,
        &home.authority(0),
        &bob.pubkey(),
        &[],
        1,
    )
    .unwrap();
    expect_ok(env.send(vec![approve], &[&bob]));
    // The executor can't put the station's money into it.
    let (owner_a, owner_b) = env.owners(&home);
    let mut ix = env.ix_increase(
        &home.authority(0),
        &mint.pubkey(),
        &owner_a,
        &owner_b,
        1_000_000,
        u64::MAX,
        u64::MAX,
    );
    ix.accounts[4].pubkey = bobs;
    expect_code(
        env.execute_funded(&home, ix, 1_000_000),
        station_error(StationError::ForeignAccount),
    );
}

#[test]
fn a_position_at_any_mint_but_the_authority_s_slots_is_refused() {
    let (mut env, home) = world();
    let executor = env.executor.pubkey();
    // A fresh mint of the executor's choosing, signing for itself.
    let mint = Keypair::new();
    let ix = env.ix_open_position(&executor, &home.authority(0), &mint.pubkey(), LOWER, UPPER);
    let call = env.ix_execute(&home, &executor, 0, ix);
    let key = env.executor.insecure_clone();
    expect_code(
        env.send(vec![call], &[&key, &mint]),
        station_error(StationError::ForeignAccount),
    );
    // The authority's mint at a slot past the driver's last.
    let beyond = shonin_driver::position_mint(&shonin_station::ID, &home.authority(0), MAX_SLOTS).0;
    let ix = env.ix_open_position(&executor, &home.authority(0), &beyond, LOWER, UPPER);
    expect_code(
        env.execute(&home, ix),
        station_error(StationError::ForeignAccount),
    );
}

/// SOL the pool can't take inside the band puts the valuation outside: on a
/// pool too thin for what the station holds, nobody is paid at a value no
/// sale inside the band reaches, so the vault's valuations are void and every
/// claim waits, even one whose own fraction the pool could take.
#[test]
fn sol_the_pool_can_t_take_inside_the_band_puts_the_valuation_outside_and_blocks_claims() {
    let (mut env, home) = world();
    // About 60 SOL, far more than the pool takes inside the band.
    expect_ok(env.station_buys_sol(&home, 9_000_000_000));
    env.price_follows_pool();
    let value = env.value(&home);
    assert!(value.outside, "{value:?}");
    // A tenth, about 6 SOL, which the pool alone would take.
    let held = wsol(&env, &home);
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 10));
    assert!(outcome.blocked && received == 0, "{outcome:?} {received}");
    assert_eq!(wsol(&env, &home), held);
}

/// A stopped station holding more SOL than the pool takes inside its band
/// sells nothing while the band can't take it all, and all of it once the
/// band has widened far enough.
#[test]
fn a_stopped_station_holding_more_sol_than_the_band_takes_sells_it_once_the_band_widens() {
    let (mut env, home) = world();
    expect_ok(env.station_buys_sol(&home, 9_000_000_000));
    let held = wsol(&env, &home);
    env.halt_and_stop(&home);
    let payer = env.payer.insecure_clone();
    let mut widenings = 0;
    loop {
        env.price_follows_pool();
        let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
        let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
        let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
        if !outcome.blocked {
            break;
        }
        assert_eq!(wsol(&env, &home), held, "sold while outside");
        widenings += 1;
        assert!(widenings < 8, "the band never let it sell");
        env.skip(shonin_station::state::WIDEN_AFTER);
    }
    assert!(widenings > 0);
    assert!(wsol(&env, &home) <= 2, "{} left", wsol(&env, &home));
}

/// At the full band a stopped station sells as far as three arrays from the
/// pool's price reach, a tick spacing short of their edge, and keeps the
/// rest; the next unwind goes on from there, walking the price across a
/// stretch with no liquidity at all, and the one after sells into the
/// liquidity beyond it.
#[test]
fn at_the_full_band_each_unwind_sells_on_from_where_the_last_stopped_across_empty_stretches() {
    let (mut env, home) = world();
    // Far more SOL than the pool holds liquidity for near its price.
    let bob = env.bob.insecure_clone();
    env.svm.airdrop(&bob.pubkey(), 6_000 * SOL).unwrap();
    env.wrap(&bob, 5_000 * SOL);
    let gift = spl_token::instruction::transfer(
        &spl_token::ID,
        &env.wsol_of(&bob.pubkey()),
        &env.wsol_of(&home.authority(0)),
        &bob.pubkey(),
        &[],
        5_000 * SOL,
    )
    .unwrap();
    expect_ok(env.send(vec![gift], &[&bob]));
    // Bob's wide position ends at -6 arrays; nothing below until a buyer at
    // the ninth array, out of reach of the first two unwinds.
    let far = -9 * SPAN;
    let payer = env.payer.insecure_clone();
    let init = env.ix_initialize_tick_array(&pool(), far, false);
    expect_ok(env.send(vec![init], &[&payer]));
    let mint = Keypair::new();
    let (lower, upper) = (far + SPACING, far + 20 * SPACING);
    let open = env.ix_open_position(&bob.pubkey(), &bob.pubkey(), &mint.pubkey(), lower, upper);
    expect_ok(env.send(vec![open], &[&bob, &mint]));
    let liquidity = env.liquidity_for(lower, upper, 0, 1_000_000_000);
    let add = env.ix_increase(
        &bob.pubkey(),
        &mint.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
        liquidity,
        u64::MAX,
        u64::MAX,
    );
    expect_ok(env.send(vec![add], &[&bob]));
    env.halt_and_stop(&home);
    env.skip(8 * shonin_station::state::WIDEN_AFTER);
    let held = wsol(&env, &home);
    let mut sends = Vec::new();
    let mut ticks = Vec::new();
    for _ in 0..3 {
        let sent = env.sent_home(&home);
        let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
        let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
        let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
        assert!(!outcome.blocked);
        sends.push(env.sent_home(&home) - sent);
        ticks.push(env.pool_state().tick);
    }
    println!("sent {sends:?}, the pool at ticks {ticks:?}");
    // The first sold down to the end of bob's wide position, inside its
    // arrays; the second found nothing to sell into and walked the price on
    // a spacing short of its arrays' edge; the third sold to the buyer.
    assert!(sends[0] > 0 && sends[1] == 0 && sends[2] > 0, "{sends:?}");
    assert!(ticks[0] >= -6 * SPAN, "{ticks:?}");
    assert_eq!(ticks[1], -8 * SPAN + SPACING, "{ticks:?}");
    assert!(ticks[2] < upper, "{ticks:?}");
    assert!(wsol(&env, &home) < held);
}

/// Whoever gives the sale's tick arrays must give a set reaching the sale's
/// limit: the one before the pool's price, which reaches an array less, is
/// refused where its reach falls short, and at the full band, where the sale
/// goes as far as its arrays reach, only the set from the price is taken.
#[test]
fn a_sale_tick_array_set_falling_short_of_the_sale_s_limit_is_refused() {
    let (mut env, home) = providing();
    let group = env.orca_group(&home.station);
    let at = Env::group_index(&group, &oracle_address(&pool())) + 1;
    let base = array_start(env.pool_state().tick);
    let mut before = group.clone();
    before[at..at + 3].clone_from_slice(
        &[base + SPAN, base, base - SPAN]
            .map(|s| AccountMeta::new(tick_array_address(&pool(), s), false)),
    );
    // A band whose limit the set from the price reaches and the one before
    // it doesn't.
    assert!(env.driver_value(&group, 8_000).is_ok());
    expect_err(
        env.driver_value(&before, 8_000),
        orca_error(OrcaError::WrongAccounts),
    );
    // Narrower, both reach it.
    assert!(env.driver_value(&before, 6_400).is_ok());
    // The full band.
    assert!(env.driver_value(&group, 10_000).is_ok());
    expect_err(
        env.driver_value(&before, 10_000),
        orca_error(OrcaError::WrongAccounts),
    );
}

/// Bob provides 400 SOL and as many dollars within about 6% of the price.
fn deepen(env: &mut Env) {
    let tick = env.pool_state().tick.div_euclid(SPACING) * SPACING;
    let bob = env.bob.insecure_clone();
    env.svm.airdrop(&bob.pubkey(), 1_000 * SOL).unwrap();
    env.wrap(&bob, 900 * SOL);
    let mint = Keypair::new();
    let (lower, upper) = (tick - 10 * SPACING, tick + 10 * SPACING);
    let open = env.ix_open_position(&bob.pubkey(), &bob.pubkey(), &mint.pubkey(), lower, upper);
    expect_ok(env.send(vec![open], &[&bob, &mint]));
    let liquidity = env.liquidity_for(lower, upper, 400 * SOL, 60_000_000_000);
    let add = env.ix_increase(
        &bob.pubkey(),
        &mint.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
        liquidity,
        u64::MAX,
        u64::MAX,
    );
    expect_ok(env.send(vec![add], &[&bob]));
}

/// The mark is Pyth's: a pool pushed up inside the band, in the transaction
/// that asks for a valuation, moves it only by what the push did to the
/// position's mix, never by the pushed price.
#[test]
fn a_pool_pushed_inside_the_band_does_not_move_the_mark() {
    let (mut env, home) = world();
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    expect_ok(env.station_buys_sol(&home, 1_500_000_000));
    deepen(&mut env);
    env.price_follows_pool();
    let before = env.value(&home);
    let idle = env.idle(&home);
    let pyth = env.sol_price();
    // Bob buys SOL until the pool's price is 0.9% above Pyth's.
    let target = env.pool_state().sqrt_price * 100_450 / 100_000;
    let bob = env.bob.insecure_clone();
    let push = env.ix_swap(
        &bob.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
        1_000_000_000_000,
        target,
        false,
    );
    expect_ok(env.send(vec![push], &[&bob]));
    assert_eq!(env.sol_price(), pyth);
    let pushed = env.value(&home);
    assert!(!pushed.outside, "{pushed:?}");
    let (at_orca, at_orca_pushed) = (before.mark - idle, pushed.mark - idle);
    assert!(
        at_orca_pushed <= at_orca + at_orca / 2_000,
        "the mark rose with the pushed pool: {at_orca} -> {at_orca_pushed}"
    );
    // While what a sale there would fetch did rise with it.
    assert!(pushed.exit > before.exit, "{before:?} {pushed:?}");
}

/// A leaver's close whose sale takes the price into the next tick array is
/// served: the stayers' check after it values the station from the same
/// arrays the close was prepared with.
#[test]
fn a_leaver_s_close_whose_sale_crosses_into_the_next_tick_array_is_served() {
    let (mut env, home) = world();
    expect_ok(env.station_buys_sol(&home, 1_000_000_000));
    // The pool's price just above the first tick of an array.
    let edge = -3 * SPAN;
    let bob = env.bob.insecure_clone();
    let up = env.ix_swap(
        &bob.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
        1_000_000_000_000,
        orca_math::sqrt_price_at(edge + 3).unwrap(),
        false,
    );
    expect_ok(env.send(vec![up], &[&bob]));
    assert_eq!(array_start(env.pool_state().tick), edge);
    env.price_follows_pool();
    assert!(!env.value(&home).outside);
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 4));
    assert!(!outcome.blocked && received > 0);
    assert_eq!(array_start(env.pool_state().tick), edge - SPAN);
}

/// The sale's tick arrays must be three consecutive arrays of the pool, the
/// first holding its price or the one before it: any other three are
/// refused, so nobody can make a sale look poorer than it is.
#[test]
fn sale_tick_arrays_not_the_pool_s_own_around_its_price_are_refused() {
    let (mut env, home) = providing();
    let group = env.orca_group(&home.station);
    let at = Env::group_index(&group, &oracle_address(&pool())) + 1;
    let base = array_start(env.pool_state().tick);
    let arrays =
        |starts: [i32; 3]| starts.map(|s| AccountMeta::new(tick_array_address(&pool(), s), false));
    // Two arrays on: the price in none of the first two.
    let mut shifted = group.clone();
    shifted[at..at + 3].clone_from_slice(&arrays([base - SPAN, base - 2 * SPAN, base - 3 * SPAN]));
    expect_err(
        env.try_value_with(&home, shifted),
        orca_error(OrcaError::WrongAccounts),
    );
    // Not consecutive.
    let mut gapped = group.clone();
    gapped[at..at + 3].clone_from_slice(&arrays([base, base - 2 * SPAN, base - 3 * SPAN]));
    expect_err(
        env.try_value_with(&home, gapped),
        orca_error(OrcaError::WrongAccounts),
    );
    // Some other account where an array not there yet would be.
    let mut stranger = group.clone();
    stranger[at + 2] = AccountMeta::new(Pubkey::new_unique(), false);
    expect_err(
        env.try_value_with(&home, stranger),
        orca_error(OrcaError::WrongAccounts),
    );
    // The arrays a close was prepared with, the price moved one on: taken.
    let mut before = group;
    before[at..at + 3].clone_from_slice(&arrays([base + SPAN, base, base - SPAN]));
    assert!(env.try_value_with(&home, before).is_ok());
}

/// A pool at the very limit a close's sale may go to is outside: whatever is
/// not outside can be closed, and nothing in between makes a close fail.
#[test]
fn a_pool_at_the_limit_of_a_close_s_sale_is_outside_and_blocks_it() {
    let (mut env, home) = providing();
    let limit =
        sqrt_at(env.sol_price()) * (2 * BPS as u128 - EXIT_BAND as u128) / (2 * BPS as u128) + 1;
    let bob = env.bob.insecure_clone();
    let down = env.ix_swap(
        &bob.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
        1_000 * SOL,
        limit,
        true,
    );
    expect_ok(env.send(vec![down], &[&bob]));
    assert!(env.value(&home).outside);
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 4));
    assert!(outcome.blocked && received == 0);
}

/// A station holding nothing of worth at Orca (an emptied position, wrapped
/// SOL worth under a dollar that anyone could send) is valued at nothing
/// there and never outside, however stale Pyth's price or far the pool:
/// nothing there to value or close.
#[test]
fn a_station_holding_nothing_of_worth_at_orca_is_never_outside() {
    let (mut env, home) = providing();
    let (outcome, _, _) = ok(env.orca_unwind(&home, WAD));
    assert!(!outcome.blocked);
    let mint = env.slot_mint(&home, 0);
    let left = env
        .position_state(&mint)
        .expect("the emptied position stays open");
    assert_eq!(
        (left.liquidity, left.fee_owed_a, left.fee_owed_b),
        (0, 0, 0)
    );
    // Someone sends the authority a little wrapped SOL, worth under a dollar.
    let bob = env.bob.insecure_clone();
    let gift = spl_token::instruction::transfer(
        &spl_token::ID,
        &env.wsol_of(&bob.pubkey()),
        &env.wsol_of(&home.authority(0)),
        &bob.pubkey(),
        &[],
        UNDER_A_DOLLAR,
    )
    .unwrap();
    expect_ok(env.send(vec![gift], &[&bob]));
    // Pyth's price goes stale, and the pool is pushed far off it.
    env.skip(1_000);
    env.bob_swaps(60 * SOL, true);
    let value = env.value(&home);
    assert!(!value.outside, "{value:?}");
    let idle = env.idle(&home);
    assert_eq!((value.mark, value.exit), (idle, idle), "{value:?}");
    // A stopped station's unwind is never held up by it.
    env.halt_and_stop(&home);
    let payer = env.payer.insecure_clone();
    let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
    let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
    assert!(!outcome.blocked);
}

/// Wrapped SOL worth $0.90 at $150, and $1.05.
const UNDER_A_DOLLAR: u64 = 6_000_000;
const OVER_A_DOLLAR: u64 = 7_000_000;

fn send_wsol(env: &mut Env, home: &Home, lamports: u64) {
    let bob = env.bob.insecure_clone();
    let ix = spl_token::instruction::transfer(
        &spl_token::ID,
        &env.wsol_of(&bob.pubkey()),
        &env.wsol_of(&home.authority(0)),
        &bob.pubkey(),
        &[],
        lamports,
    )
    .unwrap();
    expect_ok(env.send(vec![ix], &[&bob]));
}

/// Wrapped SOL worth under a dollar, with nothing else at Orca, is valued at
/// nothing and closed by nobody: the valuation agrees with the close, so a
/// claim's close, a fraction of it, is served, and leaves it where it is.
#[test]
fn wrapped_sol_worth_under_a_dollar_is_valued_at_nothing_and_never_blocks_a_claim() {
    let (mut env, home) = world();
    env.price_follows_pool();
    send_wsol(&mut env, &home, UNDER_A_DOLLAR);
    let idle = env.idle(&home);
    let value = env.value(&home);
    assert_eq!(
        (value.mark, value.exit, value.outside),
        (idle, idle, false),
        "{value:?}"
    );
    for fraction in [WAD / 3, WAD / 2] {
        let (outcome, _, _) = ok(env.orca_unwind(&home, fraction));
        assert!(!outcome.blocked);
    }
    assert_eq!(wsol(&env, &home), UNDER_A_DOLLAR);
    // Nor does it void a valuation when Pyth's price goes stale.
    env.skip(1_000);
    assert!(!env.value(&home).outside);
}

/// A stopped station's unwind of Orca is never held up by wrapped SOL worth
/// under a dollar.
#[test]
fn a_stopped_station_holding_wrapped_sol_worth_under_a_dollar_unwinds() {
    let (mut env, home) = world();
    env.price_follows_pool();
    send_wsol(&mut env, &home, UNDER_A_DOLLAR);
    env.halt_and_stop(&home);
    env.price_follows_pool();
    let payer = env.payer.insecure_clone();
    let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
    let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
    assert!(!outcome.blocked);
}

/// A dollar's worth of wrapped SOL is something held: valued at Pyth's
/// price, and outside on a stale one. With no price to read at all, any
/// wrapped SOL is; with nothing there at all, nothing is outside.
#[test]
fn a_dollar_of_wrapped_sol_or_any_with_no_price_to_read_is_something_held() {
    let (mut env, home) = world();
    env.price_follows_pool();
    let idle = env.idle(&home);
    env.set_price(0, 0, true);
    assert_eq!(
        env.value(&home),
        Valuation {
            mark: idle,
            exit: idle,
            outside: false
        }
    );
    send_wsol(&mut env, &home, 1);
    assert!(env.value(&home).outside);
    env.price_follows_pool();
    send_wsol(&mut env, &home, OVER_A_DOLLAR - 1);
    let value = env.value(&home);
    assert!(!value.outside && value.mark > idle + 1_000_000, "{value:?}");
    env.skip(1_000);
    assert!(env.value(&home).outside);
}

/// A claim whose close would leave under a dollar of wrapped SOL, and no
/// position with liquidity, sells all of it: what remains holds nothing of
/// worth, the station keeps the stayers' part of the proceeds, counted, and
/// the valuations before and after the close agree.
#[test]
fn a_claim_that_would_leave_under_a_dollar_of_wrapped_sol_sells_it_all() {
    for (gift, fraction) in [
        (OVER_A_DOLLAR, WAD / 2),
        (OVER_A_DOLLAR, WAD / 10),
        // $3, of which a 70% claim leaves $0.90.
        (20_000_000, WAD * 7 / 10),
    ] {
        let (mut env, home) = world();
        env.price_follows_pool();
        send_wsol(&mut env, &home, gift);
        let before = env.value(&home);
        let idle = env.idle(&home);
        assert!(!before.outside && before.mark > idle, "{before:?}");
        let (outcome, received, _) = ok(env.orca_unwind(&home, fraction));
        assert!(!outcome.blocked);
        // A lamport or so held back for rounding, worth nothing.
        assert!(wsol(&env, &home) <= 2, "{} left", wsol(&env, &home));
        let after = env.value(&home);
        assert_eq!((after.mark, after.exit), (env.idle(&home), env.idle(&home)));
        // The stayers' part of the Orca exit stays counted at the station.
        let orca = before.exit - idle;
        let stayers = orca - orca * (fraction / 1_000_000_000_000) / 1_000_000;
        let kept = env.idle(&home) - (idle - idle * (fraction / 1_000_000_000_000) / 1_000_000);
        assert!(kept + 2 >= stayers, "kept {kept} of the stayers' {stayers}");
        assert!(received > 0);
    }
}

/// The same for a station whose position was emptied with fees left owed
/// and a little wrapped SOL left over: the close collects every fee, and
/// what would be left under a dollar is sold with the leaver's part.
#[test]
fn an_emptied_position_s_fees_and_a_little_wrapped_sol_are_closed_whole_by_a_claim() {
    let (mut env, home) = world();
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    env.price_follows_pool();
    for _ in 0..3 {
        env.bob_swaps(20 * SOL, true);
        env.bob_swaps(3_000_000_000, false);
    }
    env.price_follows_pool();
    let mint = env.slot_mint(&home, 0);
    let liquidity = env.position_state(&mint).unwrap().liquidity;
    let (owner_a, owner_b) = env.owners(&home);
    let decrease = env.ix_decrease(
        &home.authority(0),
        &mint,
        &owner_a,
        &owner_b,
        liquidity,
        0,
        0,
    );
    expect_ok(env.execute(&home, decrease));
    let position = env.position_state(&mint).unwrap();
    assert!(position.liquidity == 0 && (position.fee_owed_a != 0 || position.fee_owed_b != 0));
    // The executor sells its SOL, keeping $0.45.
    let held = wsol(&env, &home);
    let (owner_a, owner_b) = env.owners(&home);
    let sell = env.ix_swap(
        &home.authority(0),
        &owner_a,
        &owner_b,
        held - 3_000_000,
        orca_math::MIN_SQRT_PRICE,
        true,
    );
    expect_ok(env.execute(&home, sell));
    env.price_follows_pool();
    assert!(!env.value(&home).outside);
    let (outcome, _, _) = ok(env.orca_unwind(&home, WAD / 2));
    assert!(!outcome.blocked);
    let position = env.position_state(&mint).unwrap();
    assert_eq!((position.fee_owed_a, position.fee_owed_b), (0, 0));
    assert!(wsol(&env, &home) <= 2);
    let after = env.value(&home);
    assert_eq!((after.mark, after.exit), (env.idle(&home), env.idle(&home)));
}

/// A pool pushed far above Pyth, beyond any three arrays' reach of the
/// sale's limit, answers outside: the vault's valuation is void and a claim
/// waits, rather than either failing.
#[test]
fn a_pool_pushed_far_above_pyth_is_outside_and_never_an_error() {
    let (mut env, home) = providing();
    assert!(!env.value(&home).outside);
    // Bob buys SOL up to about $570, Pyth staying at $150.
    let bob = env.bob.insecure_clone();
    let wsol = env.wsol_of(&bob.pubkey());
    let usd =
        get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID);
    for tick in [-11_300, -5_000] {
        let limit = orca_math::sqrt_price_at(tick).unwrap();
        let ix = env.ix_swap(&bob.pubkey(), &wsol, &usd, 900_000_000_000, limit, false);
        expect_ok(env.send(vec![ix], &[&bob]));
    }
    let group = env.orca_group(&home.station);
    assert!(ok(env.driver_value(&group, EXIT_BAND)).outside);
    assert!(ok(env.try_value_with(&home, group)).outside);
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 2));
    assert!(outcome.blocked && received == 0);
}

/// A pool pushed up inside the band lifts `exit` above the Pyth-priced
/// `mark` (the vault takes the difference from deposits of that moment), by
/// no more than the band.
#[test]
fn a_push_inside_the_band_lifts_exit_above_the_mark_by_less_than_the_band() {
    let (mut env, home) = world();
    // A station mostly in SOL: $1,500 of it.
    expect_ok(env.station_buys_sol(&home, 1_500_000_000));
    env.price_follows_pool();
    let bob = env.bob.insecure_clone();
    let (bob_wsol, bob_usd) = (
        env.wsol_of(&bob.pubkey()),
        get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID),
    );
    // Up to just inside the band's high root (half the band from Pyth's).
    let pyth_root = sqrt_at(env.sol_price());
    let target = pyth_root * (2 * BPS as u128 + EXIT_BAND as u128) / (2 * BPS as u128) - 2;
    let push = env.ix_swap(
        &bob.pubkey(),
        &bob_wsol,
        &bob_usd,
        1_000_000_000_000,
        target,
        false,
    );
    expect_ok(env.send(vec![push], &[&bob]));
    let pushed = env.value(&home);
    let idle = env.idle(&home);
    let (mark, exit) = (pushed.mark - idle, pushed.exit - idle);
    assert!(!pushed.outside);
    assert!(exit > mark, "mark {mark} exit {exit}");
    assert!(
        exit - mark <= mark * EXIT_BAND as u64 / BPS,
        "mark {mark} exit {exit}"
    );
}

#[test]
fn a_station_s_wrapped_sol_and_dollars_are_never_left_with_a_delegate() {
    let (mut env, home) = providing();
    // The SPL token program is never a venue: approving a delegate over the
    // authority's SOL through the station is refused outright.
    let wsol_account = env.wsol_of(&home.authority(0));
    let approve = spl_token::instruction::approve(
        &spl_token::ID,
        &wsol_account,
        &env.bob.pubkey(),
        &home.authority(0),
        &[],
        1,
    )
    .unwrap();
    expect_code(
        env.execute(&home, approve),
        station_error(StationError::WrongProgram),
    );
}

#[test]
fn the_driver_names_the_cluster_s_own_token_programs() {
    assert_eq!(shonin_orca_driver::TOKEN_PROGRAM, spl_token::ID);
    assert_eq!(
        shonin_orca_driver::TOKEN_2022_PROGRAM,
        anchor_spl::token_2022::ID
    );
    assert_eq!(
        shonin_orca_driver::ASSOCIATED_TOKEN_PROGRAM,
        anchor_spl::associated_token::ID
    );
}

#[test]
fn orca_s_terms_restrict_sanctioned_persons_and_its_close_comes_home_by_contract() {
    let description = shonin_orca_driver::logic::description();
    assert_eq!(description.name, orca());
    assert_eq!(description.custody, shonin_driver::Custody::Station);
    assert_eq!(description.exit_path, shonin_driver::ExitPath::Contract);
}

/// The crowded ticks (the one at or below `from` and each a spacing below
/// it) the price has crossed by `to`.
fn crossed(from: i32, to: i32) -> i32 {
    let top = from.div_euclid(SPACING) * SPACING;
    (0..=40).filter(|i| top - i * SPACING > to).count() as i32
}

/// A stopped station unwound in its fifth week (band 16%), holding about
/// 105 SOL the pool takes moving its price about 8%, with slivers on every
/// tick below the price: each unwind sells up to where its sale stops,
/// crossing as many ticks as a close may, and the next goes on, until it is
/// all sold. The slivers never hold the unwind up.
#[test]
fn slivers_on_every_tick_never_hold_up_a_stopped_station_s_unwind_inside_its_band() {
    for with_slivers in [false, true] {
        let (mut env, home) = world();
        let bob = env.bob.insecure_clone();
        env.svm.airdrop(&bob.pubkey(), 2_000 * SOL).unwrap();
        env.wrap(&bob, 1_000 * SOL);
        send_wsol(&mut env, &home, 105 * SOL);
        if with_slivers {
            env.crowd_below(40);
        }
        env.halt_and_stop(&home);
        env.skip(4 * shonin_station::state::WIDEN_AFTER);
        let payer = env.payer.insecure_clone();
        let mut unwinds = 0;
        while wsol(&env, &home) > 2 {
            env.price_follows_pool();
            assert_eq!(env.station(&home).stopped_band(env.now()), 1_600);
            let (from, held) = (env.pool_state().tick, wsol(&env, &home));
            let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
            let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
            let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
            assert!(!outcome.blocked, "slivers held the unwind up");
            assert!(wsol(&env, &home) < held);
            if with_slivers {
                let n = crossed(from, env.pool_state().tick) as u32;
                assert!(n <= shonin_orca_driver::sale::SWAP_CROSSINGS, "{n}");
            }
            unwinds += 1;
            assert!(unwinds <= 10);
        }
        println!("with slivers {with_slivers}: {unwinds} unwinds");
        assert!(unwinds == 1 || with_slivers);
    }
}

/// At the full band a stopped station's unwind through a crowded pool
/// crosses as many ticks as a sale may and stops short of the next; the next
/// unwind goes on from there.
#[test]
fn at_the_full_band_an_unwind_through_a_crowded_pool_sells_on_a_few_ticks_at_a_time() {
    let (mut env, home) = world();
    expect_ok(env.station_buys_sol(&home, 30_000_000_000));
    env.crowd_below(40);
    env.halt_and_stop(&home);
    env.skip(8 * shonin_station::state::WIDEN_AFTER);
    let payer = env.payer.insecure_clone();
    for _ in 0..3 {
        let (from, held) = (env.pool_state().tick, wsol(&env, &home));
        let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
        let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
        let outcome = Outcome::try_from_slice(&meta.return_data.data).unwrap();
        assert!(!outcome.blocked);
        let n = crossed(from, env.pool_state().tick);
        println!(
            "crossed {n} in {} compute units",
            meta.compute_units_consumed
        );
        assert_eq!(n as u32, shonin_orca_driver::sale::SWAP_CROSSINGS);
        assert!(wsol(&env, &home) < held);
    }
}

/// The sale's limit at `band`, as the driver works it out from Pyth's price.
fn band_limit(env: &Env, band: u16) -> u128 {
    sqrt_at(env.sol_price()) * (2 * BPS as u128 - band as u128) / (2 * BPS as u128) + 1
}

/// What Orca's own swap of `amount` of bob's SOL to `limit` pays him, the
/// pool left as it was found.
fn orca_pays(env: &mut Env, amount: u64, limit: u128) -> u64 {
    let bob = env.bob.insecure_clone();
    let usd =
        get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID);
    let before = env.token_amount(&usd);
    let swap = env.ix_swap(
        &bob.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &usd,
        amount,
        limit,
        true,
    );
    expect_ok(env.send(vec![swap], &[&bob]));
    env.token_amount(&usd) - before
}

/// A sale is worked out exactly where providers' ticks are big, and never
/// above Orca's own swap where they are slivers: a real provider's position
/// below the price is crossed tick for tick and the valuation's `exit` is
/// what Orca's swap of the same SOL to the same limit pays, less the driver's
/// allowance for Orca's share of the fee; with slivers on every tick besides,
/// whose removals the driver makes from the start, it is a little less than
/// Orca pays, never more.
#[test]
fn a_sale_is_exact_through_a_real_provider_s_ticks_and_never_above_orca_s_through_slivers() {
    let mut exits = Vec::new();
    for slivers in [0, 1, 2] {
        let (mut env, home) = world();
        let bob = env.bob.insecure_clone();
        env.svm.airdrop(&bob.pubkey(), 4_000 * SOL).unwrap();
        env.wrap(&bob, 2_000 * SOL);
        send_wsol(&mut env, &home, 30 * SOL);
        // A provider's position one to two spacings below the price, of
        // half the pool's active liquidity.
        let top = env.pool_state().tick.div_euclid(SPACING) * SPACING;
        let big = env.pool_state().liquidity / 2;
        env.provide_ranges(&[(top - 2 * SPACING, top - SPACING)], big);
        match slivers {
            // Slivers on every tick: each tick between two of them changes
            // nothing, the highest adds, the lowest removes.
            1 => env.crowd_below(40),
            // Slivers a spacing apart: every other tick removes.
            2 => {
                let ranges: Vec<(i32, i32)> = (3..=12)
                    .map(|i| (top - 2 * i * SPACING, top - (2 * i - 1) * SPACING))
                    .collect();
                env.provide_ranges(&ranges, 1_000_000_000);
            }
            _ => {}
        }
        env.price_follows_pool();
        let group = env.orca_group(&home.station);
        let value = ok(env.driver_value(&group, 1_000));
        assert!(!value.outside);
        let limit = band_limit(&env, 1_000);
        let paid = orca_pays(&mut env, 30 * SOL, limit);
        let crossed_to = env.pool_state().tick;
        println!(
            "slivers {slivers}: exit {} Orca paid {paid}, tick {top} -> {crossed_to}",
            value.exit
        );
        assert!(
            crossed_to < top - 2 * SPACING,
            "the sale crossed the provider's range"
        );
        assert!(value.exit <= paid);
        if slivers == 0 {
            assert!(
                paid - value.exit <= paid / 5_000 + 2,
                "{} {paid}",
                value.exit
            );
        } else {
            assert!(paid - value.exit <= paid / 1_000, "{} {paid}", value.exit);
        }
        exits.push(value.exit);
    }
    // Slivers that would remove liquidity on the way lower what the sale
    // is worked out to fetch; slivers that would only add never raise it.
    assert_eq!(exits[1], exits[0]);
    assert!(exits[2] < exits[0], "{exits:?}");
}

/// Ticks changing the liquidity by a hundredth of the most that is active on
/// a sale's way count against its crossings; ticks a unit smaller do not.
/// Eight such ticks inside the band make a claim's close of a holding that
/// would cross them all wait; a unit smaller, the claim is served.
#[test]
fn ticks_of_a_hundredth_of_the_liquidity_count_against_a_sale_s_crossings_and_smaller_ones_do_not()
{
    for smaller in [0u128, 1] {
        let mut env = Env::with_orca();
        let home = env.orca_home_with_band("orca", 1_000);
        expect_ok(env.station_buys_sol(&home, 12_000_000_000));
        env.price_follows_pool();
        // Four positions a spacing wide, a spacing apart: eight ticks inside
        // the band, each changing the liquidity by the threshold the sale
        // finds with them in place (a hundredth of the most liquidity on its
        // way, theirs included), or a unit less.
        let active = env.pool_state().liquidity;
        let threshold = |x: u128| (active + x) / shonin_orca_driver::sale::THETA;
        let mut theta = active / (shonin_orca_driver::sale::THETA - 1);
        while theta < threshold(theta) {
            theta += 1;
        }
        while theta > 0 && theta > threshold(theta - 1) {
            theta -= 1;
        }
        let size = theta - smaller;
        assert_eq!(size >= threshold(size), smaller == 0);
        let top = env.pool_state().tick.div_euclid(SPACING) * SPACING;
        let ranges: Vec<(i32, i32)> = (1..=4)
            .map(|i| (top - 2 * i * SPACING, top - (2 * i - 1) * SPACING))
            .collect();
        env.provide_ranges(&ranges, size);
        env.price_follows_pool();
        assert!(!env.value(&home).outside);
        let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 4));
        println!("theta {theta} less {smaller}: {outcome:?} {received}");
        assert_eq!(outcome.blocked, smaller == 0);
    }
}

/// Slivers on every tick below the price never void a valuation and never
/// make a claim wait whose own swap crosses no more of them than a close
/// affords: the claim is served inside the band and the stayers keep theirs.
#[test]
fn slivers_on_every_tick_never_make_a_claim_wait_nor_void_the_valuation() {
    let mut env = Env::with_orca();
    let home = env.orca_home_with_band("orca", 1_000);
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    expect_ok(env.station_buys_sol(&home, 8_000_000_000));
    env.price_follows_pool();
    env.crowd_below(40);
    env.price_follows_pool();
    let group = env.orca_group(&home.station);
    let (value, compute) = ok(env.driver_value_metered(&group, 1_000));
    println!("{value:?} in {compute} compute units");
    assert!(!value.outside && !env.value(&home).outside);
    let from = env.pool_state().tick;
    let (outcome, received, compute) = ok(env.orca_unwind(&home, WAD / 4));
    let n = crossed(from, env.pool_state().tick) as u32;
    println!("served {received}, crossing {n} in {compute}");
    assert!(!outcome.blocked && received > 0);
    assert!(n <= shonin_orca_driver::sale::SWAP_CROSSINGS);
    assert!(!env.value(&home).outside);
}

/// A claim whose own swap would cross more initialized ticks than one close
/// affords, slivers or not, waits; the valuation stands.
#[test]
fn a_claim_whose_own_swap_would_cross_more_ticks_than_a_close_affords_waits_and_the_valuation_stands(
) {
    let mut env = Env::with_orca();
    let home = env.orca_home_with_band("orca", 1_000);
    expect_ok(env.station_buys_sol(&home, 14_000_000_000));
    env.price_follows_pool();
    env.crowd_below(40);
    env.price_follows_pool();
    let from = env.pool_state().tick;
    let (outcome, received, _) = ok(env.orca_unwind(&home, WAD / 10 * 9));
    println!(
        "{outcome:?} {received} crossing {}",
        crossed(from, env.pool_state().tick)
    );
    assert!(outcome.blocked && received == 0);
    assert_eq!(env.pool_state().tick, from);
    assert!(!env.value(&home).outside);
}

/// A station whose position (fees earned) sits above a price three spacings
/// above the boundary between two tick arrays, `dynamic` or fixed, with
/// `extra` SOL besides, and slivers on every tick below the price.
fn at_an_array_boundary(dynamic: bool, extra: u64) -> (Env, Home) {
    let near = -4 * SPAN + 3 * SPACING;
    let mut env = Env::with_orca();
    if dynamic {
        env.make_dynamic(&[-4 * SPAN, -5 * SPAN]);
    }
    let home = env.orca_home_with_band("orca", 1_000);
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    trade_back_and_forth(&mut env, 2);
    let bob = env.bob.insecure_clone();
    env.svm.airdrop(&bob.pubkey(), 40_000 * SOL).unwrap();
    env.wrap(&bob, 20_000 * SOL);
    let limit = orca_math::sqrt_price_at(near + SPACING / 2).unwrap();
    let usd =
        get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID);
    let ix = env.ix_swap(
        &bob.pubkey(),
        &env.wsol_of(&bob.pubkey()),
        &usd,
        15_000 * SOL,
        limit,
        true,
    );
    expect_ok(env.send(vec![ix], &[&bob]));
    env.price_follows_pool();
    send_wsol(&mut env, &home, extra * SOL);
    env.crowd_below(45);
    env.price_follows_pool();
    (env, home)
}

/// A claim's close, a stopped station's unwind and a valuation at their
/// worst for compute: a position with fees decreased and collected, the
/// swap crossing as many initialized ticks as one close affords and into
/// the next tick array, fixed or dynamic, the stayers' part valued again
/// after. Each fits the compute a driver may use.
#[test]
fn closes_unwinds_and_valuations_at_their_worst_fit_the_compute_with_fixed_and_dynamic_arrays() {
    let mut worst_close = 0;
    for dynamic in [false, true] {
        for extra in [34u64, 46, 58] {
            for fraction in [WAD / 2, WAD / 100 * 99, WAD / 1000 * 999, WAD] {
                let (mut env, home) = at_an_array_boundary(dynamic, extra);
                if env.value(&home).outside {
                    continue;
                }
                let from = env.pool_state().tick;
                let (outcome, _, compute) = ok(env.orca_unwind(&home, fraction));
                let n = crossed(from, env.pool_state().tick);
                println!("dynamic {dynamic} extra {extra} fraction {fraction}: {outcome:?} crossed {n} in {compute}");
                if !outcome.blocked {
                    worst_close = worst_close.max(compute);
                }
            }
        }
        let (mut env, home) = at_an_array_boundary(dynamic, 60);
        let group = env.orca_group(&home.station);
        let (_, full) = ok(env.driver_value_metered(&group, BPS as u16));
        let (_, banded) = ok(env.driver_value_metered(&group, 1_000));
        println!("dynamic {dynamic}: valuation at the full band {full}, at 10% {banded}");
        assert!(full < COMPUTE_PER_DRIVER && banded < COMPUTE_PER_DRIVER);
        env.halt_and_stop(&home);
        env.skip(4 * shonin_station::state::WIDEN_AFTER);
        let payer = env.payer.insecure_clone();
        for _ in 0..3 {
            env.price_follows_pool();
            let ix = env.ix_unwind_stopped(&home, &payer.pubkey(), &[0]);
            let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
            println!(
                "dynamic {dynamic}: unwind in {}",
                meta.compute_units_consumed
            );
            assert!(meta.compute_units_consumed < COMPUTE_PER_DRIVER);
        }
    }
    println!("worst close {worst_close}");
    assert!(worst_close > 0);
    // Measured at about 375,000 with dynamic arrays.
    assert!(worst_close <= 380_000, "{worst_close}");
    assert!(worst_close < COMPUTE_PER_DRIVER);
}

/// The vault itself on Orca: a stranger's slivers on every tick below the
/// price, seven or forty, never make a claim wait; it is paid within what
/// the same claim is paid without them.
#[test]
fn the_vault_pays_a_claim_through_a_stranger_s_slivers() {
    use shonin_solana_tests::vault::{self as fixture, ClaimState};
    let mut paid = Vec::new();
    for slivers in [0, 7, 40] {
        let mut env = Env::new_with_mint(Some(shonin_orca_driver::network::DOLLAR_MINT));
        env.set_up_stations_for(shonin_vault::ID);
        env.set_up_orca();
        // Orca's approval waited out a timelock: the builder's and
        // reviewer's keys are blessed again, as their machines renew them.
        let (builder, reviewer) = (env.builder.pubkey(), env.reviewer.pubkey());
        env.bless_service_key(builder_image(), builder);
        env.bless_service_key(reviewer_image(), reviewer);
        let mut terms = fixture::terms();
        terms.exit_band_bps = 1_000;
        expect_ok(env.initialize_vaults(terms, Pubkey::default()));
        let v = env.open(&[Declared {
            name: orca(),
            program: shonin_orca_driver::ID,
        }]);
        let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
        env.price_follows_pool();
        let a = env.deposit_as(&v, &alice, 8_000_000_000);
        env.deposit_as(&v, &bob, 8_000_000_000);
        env.approve_executor(&v.home);
        expect_ok(env.to_station(&v, 15_000_000_000));
        let executor = env.executor.insecure_clone();
        let open = env.ix_open_wsol(&executor.pubkey(), &v.home.authority(0));
        expect_ok(env.send(vec![open], &[&executor]));
        expect_ok(env.station_buys_sol(&v.home, 14_000_000_000));
        if slivers > 0 {
            env.crowd_below(slivers);
        }
        env.price_follows_pool();
        // A quarter of Alice's shares: more than the cash, so a claim.
        let id = env.redeem(&v, &alice, a / 4);
        env.price_follows_pool();
        env.price(&v);
        assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
        env.price_follows_pool();
        assert!(!env.value(&v.home).outside);
        expect_ok(env.settle_claim(&v, id));
        let claim = env.claim(&v, id).unwrap();
        println!("{slivers} slivers: {:?}, paid {}", claim.state, claim.paid);
        assert_eq!(claim.state, ClaimState::Paid);
        paid.push(claim.paid);
    }
    assert!(
        paid[1] <= paid[0] && paid[1] >= paid[0] - paid[0] / 1_000,
        "{paid:?}"
    );
    assert!(
        paid[2] <= paid[0] && paid[2] >= paid[0] - paid[0] / 1_000,
        "{paid:?}"
    );
}
