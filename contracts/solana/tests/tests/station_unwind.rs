//! Closing a fraction for a leaver: every driver is asked about the band
//! before anything is sold, so a market outside it anywhere closes nothing;
//! then a dust account is left where it is; then a close that would pay the
//! leaver nothing is refused; then each close is made and swept, and what
//! came home is measured against the fraction's worth. A close that can't
//! bring its worth home fails the whole transaction (`ShortSweep`), so it
//! sells nothing: whether the venue keeps the proceeds as margin, whether
//! they went to repay the account's debt, or whether nothing came home for a
//! fraction worth something; and the stayers are never left short of their
//! part. Each check allows the venue's rounding, a dollar-unit per order the
//! close placed, and no more, and that rounding is the leaver's. Never more
//! than the fraction's worth goes home; the station's counted dollars go home
//! in the same fraction. A close of part of an account goes through
//! wherever a close of all of it would.

use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_test_driver::mode;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn trading() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    (env, home)
}

/// A station trading through the test driver, honest until told otherwise.
fn trading_through_the_test_driver() -> (Env, Home) {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("hostile", &[Declared::test()]);
    env.start_trading(&home);
    (env, home)
}

/// `amount` at the venue behind `size` of perp bought at [`PRICE`].
fn long_perp(env: &mut Env, home: &Home, amount: u64, size: u128) {
    let vault = home.vault;
    env.give(&vault, amount);
    expect_ok(env.pull(home, amount));
    env.approve_executor(home);
    env.fund_venue(home, amount);
    env.place(home, vec![order(perp(), true, size, PRICE)]);
    assert_eq!(env.idle(home), 0, "the station keeps nothing idle");
}

#[test]
fn the_vault_unwinds_a_leavers_fraction_and_the_cash_comes_home() {
    let (mut env, home) = trading();
    assert_eq!(env.value(&home).mark, FUNDS);
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert!(!outcome.blocked);
    assert_eq!(outcome.sent_home, FUNDS / 4);
    assert_eq!(
        received,
        FUNDS / 4,
        "the vault measured something else arriving"
    );
    assert_eq!(
        env.position(&home.authority(0), spot()),
        (UNIT * 3 / 4) as i128
    );
    assert_eq!(
        env.position(&home.authority(0), perp()),
        -((UNIT * 3 / 4) as i128)
    );
    assert_eq!(env.value(&home).mark, FUNDS * 3 / 4);
}

#[test]
fn the_counted_dollars_go_home_in_the_same_fraction() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    // 50k idle at the station, 50k at the venue; plus a stranger's gift.
    env.give(&home.station, 7_000_000);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 5));
    assert_eq!(outcome.sent_home, FUNDS / 5);
    assert_eq!(env.idle(&home), FUNDS / 2 - FUNDS / 10);
    // The gift is still there, counted for nothing.
    assert_eq!(
        env.usd_balance(&home.station),
        FUNDS / 2 - FUNDS / 10 + 7_000_000
    );
}

#[test]
fn closing_all_of_a_running_station_brings_everything_home() {
    let (mut env, home) = trading();
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(outcome.sent_home, FUNDS);
    assert_eq!(received, FUNDS);
    assert_eq!(env.position(&home.authority(0), spot()), 0);
    assert_eq!(env.position(&home.authority(0), perp()), 0);
    assert!(
        env.held_markets(&home.authority(0)).is_empty(),
        "a flat market is still listed"
    );
    assert_eq!(env.value(&home), Valuation::default());
}

#[test]
fn nothing_closes_when_a_market_is_outside_the_band() {
    let (mut env, home) = trading();
    env.set_prices(spot(), PRICE, PRICE, PRICE * 94 / 100, PRICE);
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert!(outcome.blocked);
    assert_eq!(outcome.sent_home, 0);
    assert_eq!(received, 0);
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    assert_eq!(env.position(&home.authority(0), perp()), -(UNIT as i128));
}

/// The band is checked on both sides of the venue's own price: a bid pushed
/// up, an ask pushed down, a mark either way, each blocks the close.
#[test]
fn a_price_outside_the_band_on_either_side_blocks_the_close() {
    let high = PRICE * 102 / 100;
    let low = PRICE * 98 / 100;
    let cases = [
        ("bid above", PRICE, PRICE, high, high),
        ("ask below", PRICE, PRICE, low, low),
        ("mark above", high, PRICE, PRICE, PRICE),
        ("mark below", low, PRICE, PRICE, PRICE),
        ("ask above", PRICE, PRICE, PRICE, high),
        ("bid below", PRICE, PRICE, low, PRICE),
    ];
    for (what, mark, oracle, bid, ask) in cases {
        let (mut env, home) = trading();
        env.set_prices(perp(), mark, oracle, bid, ask);
        assert!(
            env.value(&home).outside,
            "{what}: the valuation saw nothing outside"
        );
        let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 2));
        assert!(outcome.blocked, "{what}: the close went ahead");
        assert_eq!(
            env.position(&home.authority(0), perp()),
            -(UNIT as i128),
            "{what}"
        );
    }
}

#[test]
fn a_close_that_cant_bring_its_worth_home_sells_nothing() {
    let (mut env, home) = trading_through_the_test_driver();
    env.set_mode(mode::KEEP_PROCEEDS);
    let result = env.unwind_as_vault(&home, WAD / 4);
    expect_err(result, station_error(StationError::ShortSweep));
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    assert_eq!(env.usd_balance(&home.vault), 0);
}

#[test]
fn never_more_than_the_fractions_worth_goes_home() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("sweeper", &[Declared::test()]);
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS);
    env.place(&home, vec![order(spot(), true, UNIT, PRICE)]);
    // A sweep that brings back all the venue's cash, not just the fraction's.
    env.set_mode(mode::SWEEP_ALL);
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert_eq!(outcome.sent_home, FUNDS / 4);
    assert_eq!(received, FUNDS / 4);
    // The rest came back too and stays here, counted: everyone's.
    assert_eq!(env.venue_cash(&home.authority(0)), 0);
    let left = env.idle(&home);
    // 40k of cash, 15k more from the sale, 25k of it the leaver's.
    assert_eq!(left, FUNDS - PRICE + PRICE / 4 - FUNDS / 4);
    assert_eq!(env.usd_balance(&home.station), left);
    assert_eq!(env.value(&home).mark, FUNDS * 3 / 4);
}

/// The venue's rounding on a leaver's close is the leaver's: a third of an
/// account whose orders each round against it is paid a unit less than a
/// third of its worth, and the stayers keep their whole part.
#[test]
fn after_a_leavers_close_the_stayers_keep_their_whole_share() {
    let (mut env, home) = trading();
    let before = env.value(&home);
    assert_eq!(before.exit, FUNDS);
    let part = shonin_driver::mul_div(before.exit, WAD / 3, WAD);
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 3));
    assert_eq!(outcome.sent_home, received);
    assert_eq!(
        env.value(&home).exit,
        before.exit - part,
        "the stayers were left short of their part"
    );
    // The close's two orders rounded a unit away at the venue: the leaver's.
    assert_eq!(outcome.sent_home, part - 1);
}

/// A close that loses more than its rounding leaves the stayers short of
/// their part, and is undone: here a driver whose close also buys spot at the
/// ask, which a sale would fetch only the bid for.
#[test]
fn a_close_that_would_leave_the_stayers_short_sells_nothing() {
    let (mut env, home) = trading_through_the_test_driver();
    // A spread inside the band.
    env.set_prices(
        spot(),
        PRICE,
        PRICE,
        PRICE * 996 / 1000,
        PRICE * 1004 / 1000,
    );
    let value = env.value(&home);
    assert!(!value.outside);
    env.set_mode(mode::TRADE_IN_CLOSE);
    expect_err(
        env.unwind_as_vault(&home, WAD / 4),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(env.value(&home), value);
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
}

/// 12.5k at the venue behind 2 BTC of perp sold short at [`PRICE`], then the
/// price up half a percent everywhere, inside the band: worth about 11.9k,
/// under the 12.06k of margin the venue asks of 120.6k of notional.
fn short_under_margin() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    let amount = 12_500_000_000;
    expect_ok(env.pull(&home, amount));
    env.approve_executor(&home);
    env.fund_venue(&home, amount);
    env.place(&home, vec![order(perp(), false, 2 * UNIT, PRICE)]);
    let up = PRICE * 1_005 / 1_000;
    env.set_prices(perp(), up, up, up, up);
    let value = env.value(&home);
    assert!(!value.outside);
    assert!(value.exit > 11_000_000_000 && value.exit < 12_060_000_000);
    (env, home)
}

/// Taking out a fraction's proceeds keeps the account's margin ratio, so a
/// leaver's close of an account under the venue's margin goes through as a
/// close of all of it does.
#[test]
fn a_partial_close_goes_through_wherever_the_full_one_would() {
    for fraction in [WAD, WAD / 2, WAD / 3] {
        let (mut env, home) = short_under_margin();
        let exit = env.value(&home).exit;
        let (outcome, received) = ok(env.unwind_as_vault(&home, fraction));
        assert_eq!(outcome.sent_home, received);
        let part = shonin_driver::mul_div(exit, fraction, WAD);
        assert!(
            outcome.sent_home <= part && outcome.sent_home + 2 >= part,
            "{} of {part}",
            outcome.sent_home
        );
    }
}

/// 30 BTC bought at 60k with 190k and `extra` units more: at the edge bid the
/// account is worth `extra - 20` units.
fn levered_at_the_edge(extra: u64) -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    long_perp(&mut env, &home, 190_000_000_000 + extra, 30 * UNIT);
    const FALLEN: u64 = 54_208_754_208;
    const EDGE_BID: u64 = 53_666_666_666;
    env.set_prices(perp(), FALLEN, FALLEN, EDGE_BID, FALLEN);
    assert!(!env.value(&home).outside, "the market is outside the band");
    (env, home)
}

/// A leaver's part worth thousands at the mark is never sold for them while
/// at the bid it is worth no more than the rounding its one order may keep:
/// here two units at the bid, half of which is one.
#[test]
fn a_part_worth_no_more_than_its_rounding_at_the_bid_is_never_sold() {
    let (mut env, home) = levered_at_the_edge(22);
    let value = env.value(&home);
    assert_eq!(value.exit, 2);
    assert!(value.mark > 16_000_000_000, "mark {}", value.mark);
    let before = env.position(&home.authority(0), perp());
    expect_err(
        env.unwind_as_vault(&home, WAD / 2),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(env.position(&home.authority(0), perp()), before);
    assert_eq!(env.value(&home), value);
}

#[test]
fn worth_one_unit_at_the_bid_the_close_is_refused_and_sells_nothing() {
    let (mut env, home) = levered_at_the_edge(21);
    assert_eq!(env.value(&home).exit, 1);
    let before = env.position(&home.authority(0), perp());
    expect_err(
        env.unwind_as_vault(&home, WAD / 2),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(env.position(&home.authority(0), perp()), before);
}

/// A venue may round each order's proceeds down by a unit, and no more: a
/// close that comes home further short than a unit per order it placed is
/// undone. Worth four units at the bid, half the position sold at the bid
/// would pay the venue's debt and bring nothing home for a fraction worth
/// two units, through one order.
#[test]
fn a_close_more_than_a_unit_per_order_short_sells_nothing() {
    let (mut env, home) = levered_at_the_edge(24);
    let value = env.value(&home);
    assert_eq!(value.exit, 4);
    assert!(value.mark > 16_000_000_000);
    let before = env.position(&home.authority(0), perp());
    expect_err(
        env.unwind_as_vault(&home, WAD / 2),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(
        env.position(&home.authority(0), perp()),
        before,
        "a close that paid nobody sold"
    );
    assert_eq!(env.value(&home), value, "the stayers' worth moved");
}

#[test]
fn a_close_on_a_long_with_negative_venue_cash_sells_nothing() {
    let (mut env, home) = Env::with_station();
    long_perp(&mut env, &home, 190_000_000_000, 3_180_000_000_000_000_000);
    assert_eq!(env.venue_cash(&home.authority(0)), -800_000_000);
    let before = env.position(&home.authority(0), perp());
    let value = env.value(&home);
    // Selling half brings 94,600 home of a 95,000 share: inside the band, but
    // the shortfall went to the account's debt, so the account is left worth
    // more than the stayers' share. The leaver never pays it: undone.
    expect_err(
        env.unwind_as_vault(&home, WAD / 2),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(env.position(&home.authority(0), perp()), before);
    assert_eq!(env.value(&home), value);
}

#[test]
fn a_close_on_a_long_with_no_venue_debt_is_paid_in_full() {
    let (mut env, home) = Env::with_station();
    long_perp(&mut env, &home, 190_000_000_000, 3_166_666_600_000_000_000);
    assert!(env.venue_cash(&home.authority(0)) >= 0);
    let exit = env.value(&home).exit;
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 2));
    assert_eq!(outcome.sent_home, exit / 2);
    assert_eq!(received, exit / 2);
}

#[test]
fn an_account_under_water_at_the_bid_is_never_sold_for_a_leaver() {
    let (mut env, home) = Env::with_station();
    long_perp(&mut env, &home, 190_000_000_000, 30 * UNIT);
    // Worth something at the mark and nothing at the bid.
    env.set_prices(
        perp(),
        54_000_000_000,
        54_000_000_000,
        53_460_000_000,
        54_000_000_000,
    );
    let value = env.value(&home);
    assert!(!value.outside);
    assert_eq!(value.exit, 0);
    assert!(value.mark > 0);
    let before = env.position(&home.authority(0), perp());
    expect_err(
        env.unwind_as_vault(&home, WAD / 2),
        station_error(StationError::ShortSweep),
    );
    assert_eq!(env.position(&home.authority(0), perp()), before);
}

/// Each order's proceeds round down at the venue, so a leaver's part of a
/// market worth less than a unit sells for nothing, and an account holding
/// several such markets is worth several units while every part of it sells
/// for nothing. A close may come home a unit short per order it placed, so
/// such an account never holds a leaver's close: here 99 units in each of
/// three markets, of which a 1% leaver's part is worth two units and
/// brings none home.
#[test]
fn a_sub_cent_account_in_several_markets_does_not_hold_a_close() {
    const DOLLAR: u64 = 1_000_000;
    let (mut env, home) = Env::with_station();
    let markets = [name("A"), name("B"), name("C")];
    let operator = env.operator.insecure_clone();
    let list: Vec<Instruction> = markets
        .iter()
        .map(|m| env.ix_list_market(*m, false, DOLLAR))
        .collect();
    expect_ok(env.send(list, &[&operator]));
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 297);
    // 99 units' worth of each, bought with exactly the account's cash.
    let size = 99 * (UNIT / 1_000_000);
    env.place(
        &home,
        markets
            .iter()
            .map(|m| order(*m, true, size, DOLLAR))
            .collect(),
    );
    assert_eq!(env.venue_cash(&home.authority(0)), 0);
    let value = env.value(&home);
    assert_eq!(value.mark - env.idle(&home), 297);
    assert!(!value.outside);

    let idle = env.idle(&home);
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 100));
    assert!(!outcome.blocked);
    // The leaver's 1% of the idle dollars; their part of the account sold
    // for nothing, two units short through three orders.
    assert_eq!(outcome.sent_home, idle / 100);
    assert_eq!(received, idle / 100);
    for market in markets {
        assert!(env.position(&home.authority(0), market) < size as i128);
    }
}

/// An account worth no more than a few dollar-units (one more than the
/// orders its close would place) is left where it is by a fractional close,
/// so its rounding can't hold every leaver's close; a full unwind still
/// sells it.
#[test]
fn a_dust_account_is_left_where_it_is_by_a_fraction_and_sold_by_the_whole() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 3);
    // About one unit's worth of spot (which costs two, rounded up), and one
    // unit of cash: worth 2, with one order to close.
    env.place(&home, vec![order(spot(), true, 16_666_667, PRICE)]);
    assert_eq!(env.value(&home).mark - env.idle(&home), 2);
    let idle = env.idle(&home);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 2));
    assert_eq!(
        outcome.sent_home,
        idle / 2,
        "the dust account was sold for a fraction"
    );
    assert_eq!(env.position(&home.authority(0), spot()), 16_666_667);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(
        env.position(&home.authority(0), spot()),
        0,
        "the whole left the dust account"
    );
    assert!(outcome.sent_home >= idle - idle / 2);
}

/// The band first, whatever any account reads as: a dust account whose mark
/// is pushed outside the band blocks the close rather than being skipped.
#[test]
fn a_dust_account_outside_the_band_still_blocks_the_close() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 2);
    env.place(&home, vec![order(spot(), true, 16_666_667, PRICE)]);
    env.set_prices(spot(), PRICE * 2, PRICE, PRICE, PRICE);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 2));
    assert!(outcome.blocked);
    assert_eq!(outcome.sent_home, 0);
}

/// With two drivers, a market outside the band at the second venue blocks
/// the first venue's close too: a leaver is never half-sold.
#[test]
fn a_market_outside_the_band_at_one_venue_closes_nothing_at_another() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("two", &[Declared::local(), Declared::local_2()]);
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..2 {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 2);
        env.place_at(
            &home,
            index as u8,
            vec![order(spot(), true, UNIT / 2, PRICE)],
        );
    }
    env.set_prices_at(&VENUES[1], spot(), PRICE, PRICE, PRICE * 90 / 100, PRICE);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert!(outcome.blocked);
    assert_eq!(env.position(&home.authority(0), spot()), (UNIT / 2) as i128);
    assert_eq!(
        env.position_at(&VENUES[1], &home.authority(1), spot()),
        (UNIT / 2) as i128
    );
}

#[test]
fn who_may_unwind_a_running_station() {
    let (mut env, home) = trading();
    // A passer-by may not, for any fraction.
    let alice = env.alice.insecure_clone();
    for fraction in [1, WAD / 2, WAD] {
        expect_err(
            env.unwind_as(&home, &alice, fraction),
            station_error(StationError::NotAllowed),
        );
    }
    // The executor may, with its records.
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&home, &executor.pubkey(), WAD / 10, true);
    expect_ok(env.send(vec![ix], &[&executor]));
    // Without them it is a passer-by.
    let ix = env.ix_unwind(&home, &executor.pubkey(), WAD / 10, false);
    expect_code(
        env.send(vec![ix], &[&executor]),
        station_error(StationError::NotAllowed),
    );
    // Another vault's executor is not this one's.
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    env.bless_executor_key(stranger.pubkey(), [9; 32], home.bundle);
    let ix = env.ix_unwind(&home, &stranger.pubkey(), WAD / 10, true);
    expect_code(
        env.send(vec![ix], &[&stranger]),
        station_error(StationError::NotAllowed),
    );
    // No fraction, or more than all, is nothing to close.
    for fraction in [0, WAD + 1] {
        let result = env.unwind_as_vault(&home, fraction);
        expect_err(result, station_error(StationError::NotAllowed));
    }
}

#[test]
fn an_unwinder_blessed_for_the_vault_may_unwind_a_running_station() {
    let (mut env, home) = trading();
    let unwinder = Keypair::new();
    env.svm.airdrop(&unwinder.pubkey(), 1_000_000_000).unwrap();
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let leaf = env.leaf_for(home.vault.to_bytes(), home.bundle, unwinder.pubkey());
    let journal = env.journal_for(unwinder_image(), unwinder.pubkey(), leaf);
    let cluster = env.cluster();
    expect_ok(env.bless(
        &journal,
        cluster,
        home.vault.to_bytes(),
        home.bundle,
        vec![],
    ));
    let ix = env.ix_unwind(&home, &unwinder.pubkey(), WAD / 2, true);
    expect_ok(env.send(vec![ix], &[&unwinder]));
    assert_eq!(env.usd_balance(&home.vault), FUNDS / 2);
}

#[test]
fn money_comes_out_through_a_revoked_driver() {
    let (mut env, home) = trading();
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    assert_eq!(
        env.value(&home).mark,
        FUNDS,
        "a revoked driver's holdings stopped counting"
    );
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 2));
    assert_eq!(outcome.sent_home, FUNDS / 2);
}

/// Whatever a pinned driver prepares, the station checks it as it checks the
/// executor's own: no other program, no instruction off the list, no foreign
/// account where the station's own must stand, the station's address only
/// where a rule puts it, nothing that takes counted dollars out, and nothing
/// outside the accounts it was given. Nothing moves.
#[test]
fn no_driver_can_send_the_stations_dollars_anywhere() {
    let cases = [
        (mode::FOREIGN_PROGRAM, StationError::WrongProgram),
        (mode::FOREIGN_ACCOUNT, StationError::ForeignAccount),
        (mode::STATION_OUT_OF_PLACE, StationError::StationOutOfPlace),
        (mode::WRONG_DISCRIMINATOR, StationError::NotOnTheAllowlist),
        (mode::OUT_OF_RANGE, StationError::BadCall),
    ];
    for (bad, error) in cases {
        let (mut env, home) = trading_through_the_test_driver();
        env.set_mode(bad);
        let vault_before = env.usd_balance(&home.vault);
        let venue_before = env.venue_vault_balance();
        expect_err(env.unwind_as_vault(&home, WAD / 4), station_error(error));
        assert_eq!(env.usd_balance(&home.vault), vault_before);
        assert_eq!(env.venue_vault_balance(), venue_before);
        assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    }
    // And a close that would put dollars from the authority's dollar account
    // into the venue is refused for taking money out, even when what it
    // would take is only what someone sent that account unasked: that stays
    // where it is, counted by nobody.
    let (mut env, home) = Env::new_with_idle_test_station();
    let authority = home.authority(0);
    env.give(&authority, 1_000_000);
    let idle = env.idle(&home);
    env.set_mode(mode::DEPOSIT_IN_CLOSE);
    expect_err(
        env.unwind_as_vault(&home, WAD / 4),
        station_error(StationError::Unaccounted),
    );
    assert_eq!(env.usd_balance(&authority), 1_000_000);
    assert_eq!(env.idle(&home), idle);
}

#[test]
fn a_driver_that_fails_or_answers_garbage_holds_the_close_and_moves_nothing() {
    for (bad, error) in [
        (mode::FAIL, None),
        (mode::GARBAGE, Some(StationError::BadAnswer)),
        (mode::SILENT, Some(StationError::DriverSilent)),
    ] {
        let (mut env, home) = trading_through_the_test_driver();
        env.set_mode(bad);
        let result = env.unwind_as_vault(&home, WAD / 4);
        match error {
            Some(error) => expect_err(result, station_error(error)),
            None => assert!(result.is_err()),
        }
        assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    }
}

/// The accounts a driver reads are the venue's own, all of them: a market
/// left out, a market added, another station's account, a look-alike venue,
/// each is refused rather than valued or closed.
#[test]
fn a_driver_given_the_wrong_accounts_refuses_rather_than_answers() {
    let (mut env, home) = trading();
    let other = env.new_home("other", &[Declared::local()]);
    env.start_trading(&other);
    let wrong = shonin_local_driver::DriverError::WrongAccounts;
    let edits: Vec<(&str, Edit)> = vec![
        (
            "a market left out",
            Box::new(|ix| {
                ix.pop();
            }),
        ),
        (
            "a market given twice",
            Box::new(|ix| {
                let n = ix.len();
                ix[n - 2] = ix[n - 1].clone();
            }),
        ),
        (
            "a look-alike market",
            Box::new(|ix| {
                let n = ix.len();
                ix[n - 1].pubkey = shonin_local_venue::venue_address();
            }),
        ),
        (
            "another station's account",
            Box::new(move |ix| {
                let n = ix.len();
                ix[n - 3].pubkey = shonin_local_venue::account_address(&other.authority(0));
            }),
        ),
        (
            "another venue program",
            Box::new(|ix| {
                let n = ix.len();
                ix[n - 8].pubkey = shonin_test_driver::ID;
            }),
        ),
    ];
    for (what, edit) in edits {
        let mut ix = env.ix_unwind(&home, &home.vault, WAD / 4, false);
        let mut accounts = ix.accounts.clone();
        edit(&mut accounts);
        ix.accounts = accounts;
        // The groups' sizes still say one driver of the original size, so an
        // added account makes the groups wrong for the station first.
        let result = env.as_vault(&home, ix);
        let code = code_of(&result);
        let discriminator =
            anchor_error(anchor_lang::error::ErrorCode::AccountDiscriminatorMismatch);
        assert!(
            code == Some(driver_error(wrong))
                || code == Some(station_error(StationError::WrongDrivers))
                || code == Some(discriminator),
            "{what}: {:?}",
            result.as_ref().err().map(|f| f.meta.logs.join("\n"))
        );
        assert_eq!(
            env.position(&home.authority(0), spot()),
            UNIT as i128,
            "{what}"
        );
    }
    // The wrong driver program in the group.
    let mut ix = env.ix_unwind(&home, &home.vault, WAD / 4, false);
    let n = ix.accounts.len();
    // The group is the driver program, the authority and its dollar account,
    // then the driver's nine accounts: the program is eleven from the end.
    ix.accounts[n - 11].pubkey = shonin_test_driver::ID;
    expect_code(
        env.as_vault(&home, ix),
        station_error(StationError::WrongDrivers),
    );
}

/// A leaver's claim on a vault holding a levered long at the stand-in venue
/// waits for the full unwind. The venue buys a perp long with the account's
/// cash, so a long levered above one times leaves the cash negative; selling
/// a fraction of the long repays debt before it frees any cash, so its worth
/// can't come home and the close is refused (`ShortSweep`), moving nothing.
/// The full unwind, which everyone shares, sells all of it and brings it all
/// home. The EVM stand-in venue behaves the same way.
#[test]
fn a_levered_long_waits_for_the_full_unwind_which_brings_it_home() {
    // 100,000 at the venue behind 3 BTC of perp bought at the price: 180,000
    // of notional, 80,000 of it owed, well inside the venue's margin.
    let levered = || {
        let (mut env, home) = Env::with_station();
        expect_ok(env.pull(&home, FUNDS));
        env.approve_executor(&home);
        env.fund_venue(&home, FUNDS);
        env.place(&home, vec![order(perp(), true, 3 * UNIT, PRICE)]);
        assert_eq!(env.venue_cash(&home.authority(0)), -80_000_000_000);
        let value = env.value(&home);
        assert!(!value.outside);
        assert_eq!(value.exit, FUNDS);
        (env, home)
    };
    for fraction in [WAD / 2, WAD / 4, WAD / 10] {
        let (mut env, home) = levered();
        expect_err(
            env.unwind_as_vault(&home, fraction),
            station_error(StationError::ShortSweep),
        );
        assert_eq!(
            env.position(&home.authority(0), perp()),
            3 * UNIT as i128,
            "a refused close sold something"
        );
        assert_eq!(env.usd_balance(&home.vault), 0);
    }
    let (mut env, home) = levered();
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(outcome.sent_home, FUNDS);
    assert_eq!(received, FUNDS);
    assert_eq!(env.position(&home.authority(0), perp()), 0);
}

/// A close whose leaver's part rounds to nothing still places an order in
/// every market the account holds, and the venue rounds each against the
/// account: a buy costs every part of a dollar-unit it takes, a sale brings
/// only whole ones. The leaver is paid nothing, so the rounding can't be
/// taken from them; it falls on the stayers, at most one dollar-unit per
/// order per close.
#[test]
fn a_close_that_pays_the_leaver_nothing_costs_the_stayers_at_most_a_unit_per_order() {
    let (mut env, home) = trading();
    let orders = env.held_markets(&home.authority(0)).len() as u64;
    assert_eq!(orders, 2);
    let before = env.value(&home);
    assert!(!before.outside);
    let closes = 10u64;
    let mut paid = 0u64;
    for _ in 0..closes {
        // One part in 10^18 of every position.
        let (outcome, received) = ok(env.unwind_as_vault(&home, 1));
        assert!(!outcome.blocked);
        paid += received;
    }
    let after = env.value(&home);
    assert_eq!(paid, 0, "the leaver was paid something");
    let cost = before.exit - after.exit;
    assert!(
        cost <= closes * orders,
        "{closes} closes that paid the leaver nothing took {cost} dollar-units from the stayers"
    );
}

/// A change to an instruction's accounts.
type Edit = Box<dyn Fn(&mut Vec<anchor_lang::prelude::AccountMeta>)>;

trait IdleTestStation {
    fn new_with_idle_test_station() -> (Env, Home);
}

impl IdleTestStation for Env {
    /// A station on the test driver with counted dollars left idle beside a
    /// position.
    fn new_with_idle_test_station() -> (Env, Home) {
        let mut env = Env::new();
        env.set_up_stations();
        let home = env.new_home("idle", &[Declared::test()]);
        expect_ok(env.pull(&home, FUNDS));
        env.approve_executor(&home);
        env.fund_venue(&home, FUNDS / 2);
        env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
        (env, home)
    }
}
