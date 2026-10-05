//! The stand-in venue's driver, built on the real driver interface: it says
//! what it is, gives the allowlist a station pins (the venue program, its
//! five instructions, and the station's own accounts in each), values the
//! station's account at the mark and at what a sale would fetch, says whether
//! any market is outside the band on either side of the venue's own price,
//! closes a fraction of every position with one reduce-only order each inside
//! the band (or at any price at the full band), answering the close with the
//! valuation it was prepared from, and sweeps no more than the
//! account's cash home. It reads only accounts it can prove are the venue's
//! own, and needs every market the account holds; every answer fits the
//! return data.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::{AnchorDeserialize, AnchorSerialize, Discriminator};
use shonin_driver::{
    discriminator, Allowlist, Class, Close, Custody, Description, ExitPath, Own, Valuation,
    ValuationKind, MAX_RETURN_DATA,
};
use shonin_local_driver::DriverError;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_instruction::Instruction;

/// The driver asked directly, as a station asks it, with the accounts
/// `edit` leaves.
fn ask_with(
    env: &mut Env,
    home: &Home,
    data: Vec<u8>,
    edit: impl FnOnce(&mut Vec<AccountMeta>),
) -> std::result::Result<Vec<u8>, FailedTx> {
    // The station's authority at the venue, its dollar account, then the
    // driver's own accounts: the group after its program.
    let mut accounts: Vec<AccountMeta> = env
        .driver_group(&home.station, &shonin_local_driver::ID)
        .into_iter()
        .skip(1)
        .map(|meta| AccountMeta::new_readonly(meta.pubkey, false))
        .collect();
    edit(&mut accounts);
    let ix = Instruction {
        program_id: shonin_local_driver::ID,
        accounts,
        data,
    };
    let payer = env.payer.insecure_clone();
    env.send(vec![ix], &[&payer])
        .map(|meta| meta.return_data.data)
}

fn ask<T: AnchorDeserialize>(env: &mut Env, home: &Home, data: Vec<u8>) -> T {
    let bytes = ok(ask_with(env, home, data, |_| {}));
    assert!(bytes.len() <= MAX_RETURN_DATA);
    T::try_from_slice(&bytes).unwrap()
}

fn data(discriminator: [u8; 8], args: impl AnchorSerialize) -> Vec<u8> {
    shonin_driver::data(discriminator, &args)
}

/// The orders in a prepared `place`.
fn orders_of(plan: &Plan) -> Vec<OrderRequest> {
    assert_eq!(plan.instructions.len(), 1);
    let prepared = &plan.instructions[0];
    assert_eq!(
        &prepared.data[..8],
        shonin_local_venue::instruction::Place::DISCRIMINATOR
    );
    Vec::<OrderRequest>::try_from_slice(&prepared.data[8..]).unwrap()
}

fn trading() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    (env, home)
}

#[test]
fn it_describes_itself_and_reaches_only_the_venue() {
    let (mut env, home) = Env::with_station();
    let description: Description = ask(&mut env, &home, data(discriminator::DESCRIBE, ()));
    assert_eq!(description.name, local());
    assert_eq!(description.custody, Custody::Station);
    assert_eq!(description.exit_path, ExitPath::Contract);
    assert_eq!(description.valuation, ValuationKind::OnChain);
    let allowlist: Allowlist = ask(&mut env, &home, data(discriminator::ALLOWLIST, ()));
    assert_eq!(allowlist.venue, shonin_local_venue::ID);
    assert_eq!(allowlist.rules.len(), 5);
    let restricted: Vec<[u8; 32]> = ask(&mut env, &home, data(discriminator::RESTRICTED, ()));
    assert!(
        restricted.is_empty(),
        "the stand-in venue's terms exclude nobody"
    );
}

/// Wherever the venue moves dollars, the allowlist pins the station's own
/// account there (its authority's at the venue); the authority signs as
/// master and nowhere else; and only
/// withdrawing and closing are exits a revoked driver still allows.
#[test]
fn the_allowlist_pins_the_stations_own_accounts_where_money_moves() {
    use shonin_local_venue::instruction as venue;
    let allowlist = shonin_local_driver::logic::allowlist(&shonin_local_venue::ID);
    let rule = |d: &[u8]| {
        allowlist
            .rules
            .iter()
            .find(|r| r.discriminator == d)
            .unwrap()
    };
    let own = |d: &[u8], index: u8| {
        rule(d)
            .positions
            .iter()
            .find(|p| p.index == index)
            .map(|p| p.own.clone())
    };
    let account = Own::StationAddress(shonin_local_venue::ACCOUNT_SEED.to_vec());
    assert_eq!(own(venue::Deposit::DISCRIMINATOR, 0), Some(Own::Station));
    assert_eq!(own(venue::Deposit::DISCRIMINATOR, 1), Some(account.clone()));
    assert_eq!(
        own(venue::Deposit::DISCRIMINATOR, 2),
        Some(Own::StationDollars)
    );
    assert_eq!(own(venue::Withdraw::DISCRIMINATOR, 0), Some(Own::Station));
    assert_eq!(
        own(venue::Withdraw::DISCRIMINATOR, 2),
        Some(Own::StationDollars)
    );
    assert_eq!(own(venue::Place::DISCRIMINATOR, 0), Some(Own::Station));
    assert_eq!(
        own(venue::OpenAccount::DISCRIMINATOR, 1),
        Some(Own::Station)
    );
    assert_eq!(
        own(venue::CloseAccount::DISCRIMINATOR, 0),
        Some(Own::Station)
    );
    for (d, class) in [
        (venue::OpenAccount::DISCRIMINATOR, Class::Trade),
        (venue::Deposit::DISCRIMINATOR, Class::Trade),
        (venue::Place::DISCRIMINATOR, Class::Trade),
        (venue::Withdraw::DISCRIMINATOR, Class::Exit),
        (venue::CloseAccount::DISCRIMINATOR, Class::Exit),
    ] {
        assert_eq!(rule(d).class, class);
    }
    // Nothing the operator does is on it.
    for d in [
        venue::SetPrices::DISCRIMINATOR,
        venue::ListMarket::DISCRIMINATOR,
        venue::Post::DISCRIMINATOR,
        venue::AccrueFunding::DISCRIMINATOR,
        venue::SetFunding::DISCRIMINATOR,
        venue::Initialize::DISCRIMINATOR,
    ] {
        assert!(!allowlist.rules.iter().any(|r| r.discriminator == d));
    }
}

#[test]
fn a_close_takes_the_fraction_of_every_position_and_a_sweep_brings_its_share_home() {
    let (mut env, home) = trading();
    let close = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD / 4, EXIT_BAND)),
    );
    // The close carries the valuation it was prepared from: what `value`
    // answers at the same band.
    let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, EXIT_BAND));
    assert_eq!(close.value, value);
    assert!(value.mark > 0 && value.exit > 0 && !value.outside);
    let plan = close.plan;
    assert!(!plan.outside);
    let orders = orders_of(&plan);
    assert_eq!(orders.len(), 2);
    let spot_order = orders.iter().find(|o| o.market == spot()).unwrap();
    assert!(!spot_order.is_buy && spot_order.reduce_only);
    assert_eq!(spot_order.size, UNIT / 4);
    assert_eq!(spot_order.limit_price, (PRICE * 9_900).div_ceil(10_000));
    let perp_order = orders.iter().find(|o| o.market == perp()).unwrap();
    assert!(perp_order.is_buy && perp_order.reduce_only);
    assert_eq!(perp_order.limit_price, PRICE * 10_100 / 10_000);
    // The sweep withdraws the share asked for, to the dollar account of the
    // station's authority at the venue.
    let sweep: Plan = ask(&mut env, &home, data(discriminator::SWEEP, FUNDS / 4));
    let prepared = &sweep.instructions[0];
    assert_eq!(
        &prepared.data[..8],
        shonin_local_venue::instruction::Withdraw::DISCRIMINATOR
    );
    assert_eq!(u64::try_from_slice(&prepared.data[8..]).unwrap(), FUNDS / 4);
    assert_eq!(
        prepared.accounts[2].index, 1,
        "the withdrawal goes anywhere but the authority's dollar account"
    );
}

#[test]
fn a_fraction_rounds_up_so_nothing_is_left_behind_by_rounding() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS);
    env.place(&home, vec![order(spot(), true, 3, PRICE)]);
    let plan = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD / 2, EXIT_BAND)),
    )
    .plan;
    assert_eq!(orders_of(&plan)[0].size, 2);
    let plan = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (1u64, EXIT_BAND)),
    )
    .plan;
    assert_eq!(orders_of(&plan)[0].size, 1);
}

#[test]
fn nothing_is_closed_when_a_market_is_outside_the_band() {
    let (mut env, home) = trading();
    env.set_prices(spot(), PRICE, PRICE, PRICE * 94 / 100, PRICE);
    let close = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD / 4, EXIT_BAND)),
    );
    assert!(close.plan.outside);
    assert!(close.plan.instructions.is_empty());
    let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, EXIT_BAND));
    assert!(value.outside);
    assert_eq!(close.value, value);
}

#[test]
fn the_band_is_checked_on_both_sides_of_the_venues_own_price() {
    let (mut env, home) = trading();
    let band = |price: u64, bps: u64, up: bool| {
        if up {
            price * (BPS + bps) / BPS
        } else {
            price * (BPS - bps) / BPS
        }
    };
    // Just inside the band on every side: inside.
    for (market, long) in [(spot(), true), (perp(), false)] {
        let _ = long;
        env.set_prices(
            market,
            band(PRICE, 99, true),
            PRICE,
            band(PRICE, 99, false),
            band(PRICE, 99, true),
        );
    }
    let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, EXIT_BAND));
    assert!(!value.outside);
    // Each of the mark, bid and ask a little past it on either side: outside.
    for which in 0..6 {
        let mut p = [PRICE, PRICE, PRICE];
        p[which / 2] = band(PRICE, 101, which % 2 == 0);
        env.set_prices(perp(), p[0], PRICE, p[1], p[2]);
        let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, EXIT_BAND));
        assert!(value.outside, "price {which} past the band went unnoticed");
        env.set_prices(perp(), PRICE, PRICE, PRICE, PRICE);
    }
}

#[test]
fn at_the_full_band_a_close_takes_any_price() {
    let (mut env, home) = trading();
    env.set_prices(spot(), PRICE * 3, PRICE, 1, PRICE * 5);
    env.set_prices(perp(), PRICE * 3, PRICE, 1, PRICE * 5);
    let plan = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD, 10_000u16)),
    )
    .plan;
    assert!(!plan.outside);
    let orders = orders_of(&plan);
    assert_eq!(
        orders
            .iter()
            .find(|o| o.market == spot())
            .unwrap()
            .limit_price,
        0
    );
    assert_eq!(
        orders
            .iter()
            .find(|o| o.market == perp())
            .unwrap()
            .limit_price,
        u64::MAX
    );
    let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, 10_000u16));
    assert!(!value.outside);
}

#[test]
fn a_sweep_takes_no_more_than_the_accounts_cash() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 1_000_000_000);
    let sweep: Plan = ask(&mut env, &home, data(discriminator::SWEEP, u64::MAX));
    let prepared = &sweep.instructions[0];
    assert_eq!(
        u64::try_from_slice(&prepared.data[8..]).unwrap(),
        1_000_000_000
    );
    // A levered account with negative cash sweeps nothing.
    env.place(&home, vec![order(perp(), true, UNIT / 10, PRICE)]);
    assert!(env.venue_cash(&home.authority(0)) < 0);
    let sweep: Plan = ask(&mut env, &home, data(discriminator::SWEEP, u64::MAX));
    assert!(sweep.instructions.is_empty());
}

#[test]
fn an_account_never_opened_is_worth_nothing_and_closes_nothing() {
    let (mut env, home) = Env::with_station();
    let value: Valuation = ask(&mut env, &home, data(discriminator::VALUE, EXIT_BAND));
    assert_eq!(value, Valuation::default());
    let close = ask::<Close>(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD, EXIT_BAND)),
    );
    assert_eq!(close, Close::default());
    let plan: Plan = ask(&mut env, &home, data(discriminator::SWEEP, 1u64));
    assert_eq!(plan, Plan::default());
}

/// Every account the driver reads must be the venue's account it stands for:
/// each substitute is refused rather than read.
#[test]
fn the_driver_reads_only_the_venues_own_accounts_and_all_of_them() {
    let (mut env, home) = trading();
    let other = env.new_home("other", &[Declared::local()]);
    env.start_trading(&other);
    let stranger = Pubkey::new_unique();
    // Indexes into the accounts the driver is given.
    let substitutions: Vec<(&str, usize, Pubkey)> = vec![
        ("the venue program", 2, shonin_test_driver::ID),
        ("the venue's settings", 3, stranger),
        ("the venue's dollar account", 4, home.authority_dollars(0)),
        ("the mint", 5, stranger),
        ("the token program", 6, anchor_spl::token_2022::ID),
        (
            "another station's venue account",
            7,
            shonin_local_venue::account_address(&other.authority(0)),
        ),
    ];
    for (what, at, substitute) in substitutions {
        let result = ask_with(
            &mut env,
            &home,
            data(discriminator::VALUE, EXIT_BAND),
            |accounts| {
                accounts[at].pubkey = substitute;
            },
        );
        assert_eq!(
            failed_code(&result),
            Some(driver_error(DriverError::WrongAccounts)),
            "{what}"
        );
    }
    // A held market left out, or given twice.
    let result = ask_with(
        &mut env,
        &home,
        data(discriminator::VALUE, EXIT_BAND),
        |accounts| {
            accounts.pop();
        },
    );
    assert_eq!(
        failed_code(&result),
        Some(driver_error(DriverError::WrongAccounts))
    );
    let result = ask_with(
        &mut env,
        &home,
        data(discriminator::VALUE, EXIT_BAND),
        |accounts| {
            let last = accounts[accounts.len() - 1].clone();
            accounts.push(last);
        },
    );
    assert_eq!(
        failed_code(&result),
        Some(driver_error(DriverError::WrongAccounts))
    );
    // Too few accounts to say anything.
    let result = ask_with(
        &mut env,
        &home,
        data(discriminator::VALUE, EXIT_BAND),
        |accounts| {
            accounts.truncate(5);
        },
    );
    assert_eq!(
        failed_code(&result),
        Some(driver_error(DriverError::WrongAccounts))
    );
}

#[test]
fn every_answer_fits_the_return_data_with_every_market_held() {
    let (mut env, home) = Env::with_station();
    let operator = env.operator.insecure_clone();
    let more: Vec<Instruction> = (0..6)
        .map(|i| env.ix_list_market(name(&format!("COIN-{i}")), true, PRICE))
        .collect();
    expect_ok(env.send(more, &[&operator]));
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS);
    let mut orders = vec![
        order(spot(), true, UNIT / 100, PRICE),
        order(perp(), false, UNIT / 100, PRICE),
    ];
    orders.extend((0..6).map(|i| {
        order(
            name(&format!("COIN-{i}")),
            i % 2 == 0,
            UNIT / 100,
            if i % 2 == 0 { PRICE } else { 0 },
        )
    }));
    // Four orders a transaction: a real executor would use an address
    // lookup table for more.
    for batch in orders.chunks(4) {
        env.place(&home, batch.to_vec());
    }
    assert_eq!(
        env.held_markets(&home.authority(0)).len(),
        shonin_local_venue::MAX_HELD
    );
    let bytes = ok(ask_with(
        &mut env,
        &home,
        data(discriminator::CLOSE, (WAD, EXIT_BAND)),
        |_| {},
    ));
    assert!(
        bytes.len() <= MAX_RETURN_DATA,
        "a close of every market is {} bytes",
        bytes.len()
    );
    let plan = Close::try_from_slice(&bytes).unwrap().plan;
    assert_eq!(orders_of(&plan).len(), shonin_local_venue::MAX_HELD);
    // And the whole close of all of them fits one transaction and its compute.
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert!(!outcome.blocked);
    assert!(env.held_markets(&home.authority(0)).is_empty());
}
