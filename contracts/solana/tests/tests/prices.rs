//! The published price list: a change waits out its notice and never reaches
//! back, the cost of an allowance is integrated over the history, each piece
//! rounding down in the tank's favour, and pricing a recent interval stays
//! cheap however long the history grows.

use shonin_prices::PricesError;
use shonin_solana_tests::*;
use solana_signer::Signer;

/// The Rust `Allowances::thinking()` shape, with no gas.
fn thinking() -> Allowances {
    Allowances {
        fuel_per_sec: 4_000_000,
        memory_mib: 512,
        requests_per_min: 120,
        bytes_per_day: 500_000_000,
        streams: 4,
        inference_ms_per_min: 2000,
        gas_per_day: 0,
    }
}

fn modest() -> Allowances {
    Allowances {
        fuel_per_sec: 2_000_000,
        memory_mib: 256,
        requests_per_min: 120,
        bytes_per_day: 50_000_000,
        streams: 4,
        ..Allowances::default()
    }
}

fn close(a: u128, b: u128, tolerance: u128) -> bool {
    a.abs_diff(b) <= tolerance
}

/// The same figure the Rust side computes for the modest shape:
/// 2 cores × $0.15 × 24 + 0.25 GiB × $0.01 × 24 + 0.05 GB × $0.15.
#[test]
fn a_day_at_the_modest_allowances_costs_what_the_app_says() {
    let env = Env::new();
    assert_eq!(env.daily_cost(&modest()), 7_267_500);
    assert_eq!(env.cost_of(&modest(), 1000, 1000 + DAY), 7_267_500);
}

/// 4 × 0.15 × 24 + 0.5 × 0.01 × 24 + 0.5 × 0.15 + 2880 × 0.15/3600 = 14.715.
/// The chain holds the inference price truncated, so it lands at most one
/// micro-dollar under.
#[test]
fn a_day_at_the_thinking_shape_agrees_with_the_rust_formula() {
    let env = Env::new();
    let cap = env.daily_cost(&thinking());
    assert!(close(cap, 14_715_000, 1));
    assert!(cap <= 14_715_000, "rounds down, in the tank's favour");
}

#[test]
fn inference_and_gas_are_priced() {
    let env = Env::new();
    let mut a = Allowances {
        inference_ms_per_min: 2000,
        ..Allowances::default()
    };
    // Two seconds a minute is 2,880 seconds a day at the processor rate.
    assert!(close(env.daily_cost(&a), 120_000, 1));
    a.inference_ms_per_min = 0;
    a.gas_per_day = 3_000_000;
    assert_eq!(env.daily_cost(&a), 3_000_000);
    assert_eq!(env.cost_of(&a, 0, 12 * HOUR), 1_500_000);
}

#[test]
fn a_price_change_waits_out_its_notice_and_never_reaches_back() {
    let mut env = Env::new();
    let mut doubled = published_prices();
    doubled.vcpu_hour *= 2;
    doubled.gib_hour *= 2;
    doubled.gb_egress *= 2;
    let start = env.now();
    expect_ok(env.schedule(doubled));

    assert_eq!(env.daily_cost(&modest()), 7_267_500);
    env.skip(PRICE_NOTICE);
    assert_eq!(env.daily_cost(&modest()), 2 * 7_267_500);

    // A day before the change at the old prices and a day after at the new.
    let change = start + PRICE_NOTICE;
    assert_eq!(
        env.cost_of(&modest(), change - DAY, change + DAY),
        3 * 7_267_500
    );
    assert_eq!(env.cost_of(&modest(), start, start + DAY), 7_267_500);
}

#[test]
fn only_the_admin_schedules_prices() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    let ix = env.ix_schedule(&alice.pubkey(), published_prices());
    expect_code(
        env.send(vec![ix], &[&alice]),
        prices_error(PricesError::NotAdmin),
    );
    assert_eq!(env.history().len(), 1);
}

/// Integration across three periods equals the sum of the pieces, and each
/// piece is a whole number of days at a known daily figure.
#[test]
fn cost_integrates_across_three_price_periods() {
    let mut env = Env::new();
    let a = thinking();
    let t0 = env.now();
    let mut p2 = published_prices();
    p2.vcpu_hour *= 3;
    let mut p3 = published_prices();
    p3.gib_hour = 0;
    p3.inference_second *= 10;

    expect_ok(env.schedule(p2)); // effective t0 + notice
    env.skip(5 * DAY);
    expect_ok(env.schedule(p3)); // effective t0 + 5 days + notice

    let c1 = t0 + PRICE_NOTICE;
    let c2 = t0 + 5 * DAY + PRICE_NOTICE;
    let from = c1 - 3 * DAY;
    let to = c2 + 2 * DAY;

    let whole = env.cost_of(&a, from, to);
    let pieces = env.cost_of(&a, from, c1) + env.cost_of(&a, c1, c2) + env.cost_of(&a, c2, to);
    // Each period rounds down on its own inside the cost too, so the two
    // agree to within a unit per period.
    assert!(close(whole, pieces, 3));

    // p2 is 4 × 0.45 × 24 + 0.12 + 0.075 + 0.12 = $43.515 a day;
    // p3 is 14.4 + 0 + 0.075 + 1.2 = $15.675 a day.
    env.warp(c1);
    let day_at_p2 = env.daily_cost(&a);
    assert!(close(day_at_p2, 43_515_000, 1));
    assert!(close(env.cost_of(&a, c1, c2), 5 * day_at_p2, 5));
    assert!(close(env.cost_of(&a, from, c1), 3 * 14_715_000, 3));
    env.warp(c2);
    assert!(close(env.daily_cost(&a), 15_675_000, 1));
    assert!(close(env.cost_of(&a, c2, to), 2 * 15_675_000, 2));
    assert_eq!(env.history().len(), 3);
}

/// Two schedules at the same moment make a zero-length period; the later one
/// wins everywhere, and the empty period contributes nothing.
#[test]
fn two_schedules_at_one_moment_leave_the_later_in_force() {
    let mut env = Env::new();
    let mut p2 = published_prices();
    p2.vcpu_hour *= 2;
    let mut p3 = published_prices();
    p3.vcpu_hour *= 4;
    let admin = env.admin.insecure_clone();
    let ixs = vec![
        env.ix_schedule(&admin.pubkey(), p2),
        env.ix_schedule(&admin.pubkey(), p3),
    ];
    expect_ok(env.send(ixs, &[&admin]));
    let change = env.now() + PRICE_NOTICE;

    env.skip(PRICE_NOTICE);
    assert_eq!(env.current_prices().vcpu_hour, 4 * 150_000_000_000_000_000);
    let a = Allowances {
        fuel_per_sec: 1_000_000, // one core: $3.60 a day, $14.40 at 4x
        ..Allowances::default()
    };
    assert_eq!(env.cost_of(&a, change, change + DAY), 14_400_000);
    assert_eq!(env.cost_of(&a, change - DAY, change), 3_600_000);
    let history = env.history();
    assert_eq!(history[1].effective_from, history[2].effective_from);
}

/// An empty or backwards interval costs nothing; a boundary is exact.
#[test]
fn empty_and_boundary_intervals() {
    let env = Env::new();
    let a = allowances_costing(86_400); // one unit a second
    let t = env.now();
    assert_eq!(env.cost_of(&a, t, t), 0);
    assert_eq!(env.cost_of(&a, t + 1, t), 0);
    assert_eq!(env.cost_of(&a, t, t + 1), 1);
    assert_eq!(env.cost_of(&a, t, t + DAY), 86_400);
}

/// Splitting an interval anywhere never charges more than the whole: each
/// piece rounds down on its own, so the sum of pieces is at most the whole.
#[test]
fn pieces_never_cost_more_than_the_whole() {
    let env = Env::new();
    let a = Allowances {
        fuel_per_sec: 1, // a millionth of a core, so rounding bites
        gas_per_day: 7,
        ..Allowances::default()
    };
    let t = env.now();
    let whole = env.cost_of(&a, t, t + DAY);
    let pieces: u128 = (0..24)
        .map(|i| env.cost_of(&a, t + i * HOUR, t + (i + 1) * HOUR))
        .sum();
    assert!(pieces <= whole);
}

/// The widest inputs the types allow integrate without overflowing: the
/// 256-bit products carry them, and the sum fits.
#[test]
fn the_cost_of_the_largest_allowances_at_the_largest_prices_does_not_overflow() {
    let mut env = Env::new();
    let top = UnitPrices {
        vcpu_hour: u128::MAX,
        gib_hour: u128::MAX,
        gb_egress: u128::MAX,
        inference_second: u128::MAX,
    };
    expect_ok(env.schedule(top));
    env.skip(PRICE_NOTICE);
    let a = Allowances {
        fuel_per_sec: u64::MAX,
        memory_mib: u32::MAX,
        requests_per_min: u32::MAX,
        bytes_per_day: u64::MAX,
        streams: u32::MAX,
        inference_ms_per_min: u32::MAX,
        gas_per_day: u64::MAX,
    };
    let ix = view(shonin_prices::instruction::CostOf {
        allowances: a,
        from: 0,
        to: u64::MAX,
    });
    let whole = env.simulate(vec![ix]).return_data.data;
    assert!(whole.iter().any(|byte| *byte != 0));
    let ix = view(shonin_prices::instruction::DailyCost { allowances: a });
    let day = env.simulate(vec![ix]).return_data.data;
    assert!(day.iter().any(|byte| *byte != 0));
}

fn view(data: impl anchor_lang::InstructionData) -> solana_instruction::Instruction {
    use anchor_lang::ToAccountMetas;
    solana_instruction::Instruction {
        program_id: shonin_prices::ID,
        accounts: shonin_prices::accounts::View {
            prices: shonin_prices::read::prices_address(),
        }
        .to_account_metas(None),
        data: data.data(),
    }
}

/// A long history doesn't make a recent interval dear to price: only the
/// periods it touches are read, and the answer is the same.
#[test]
fn a_long_history_prices_a_recent_interval_the_same_and_cheaply() {
    let mut env = Env::new();
    let a = thinking();
    let now = env.now();
    let (_, short_history) = env.cost_of_on_chain(a, now, now + DAY);

    let mut p = published_prices();
    for _ in 0..200 {
        p.vcpu_hour += 1;
        expect_ok(env.schedule(p));
        env.skip(DAY);
    }
    env.skip(PRICE_NOTICE);
    let from = env.now();
    let (cost, long_history) = env.cost_of_on_chain(a, from, from + DAY);
    assert!(close(cost, env.daily_cost(&a), 1));
    assert_eq!(cost, env.cost_of(&a, from, from + DAY));
    assert!(
        long_history < short_history + 5_000,
        "a recent day reads a handful of periods, not the whole history: {short_history} then {long_history}"
    );
    assert_eq!(env.history().len(), 201);
}

/// With a fixed notice every later schedule starts no earlier than an earlier
/// one, so the history stays in order; were the clock ever to read earlier,
/// a schedule that would start before one already made is refused.
#[test]
fn a_change_never_starts_before_one_already_scheduled() {
    let mut env = Env::new();
    expect_ok(env.schedule(published_prices()));
    let now = env.now();
    env.warp(now - 1);
    expect_code(
        env.schedule(published_prices()),
        prices_error(PricesError::Backwards),
    );
}

/// Each change is paid for by the admin, and the history only grows.
#[test]
fn a_change_is_paid_for_by_the_admin_and_the_history_only_grows() {
    let mut env = Env::new();
    let admin = env.admin.pubkey();
    let before = env.lamports(&admin);
    let mut p = published_prices();
    p.gib_hour = 1;
    expect_ok(env.schedule(p));
    assert!(env.lamports(&admin) < before);
    let history = env.history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].effective_from, 0);
    assert_eq!(history[0].prices, published_prices());
    assert_eq!(history[1].effective_from, env.now() + PRICE_NOTICE);
    assert_eq!(history[1].prices, p);
}

/// The prices answer as the vault will read them.
#[test]
fn prices_answer_with_their_settings() {
    let mut env = Env::new();
    let list: shonin_prices::PriceList = env.load(&shonin_prices::read::prices_address()).unwrap();
    assert_eq!(list.notice, PRICE_NOTICE);
    assert_eq!(list.admin, env.admin.pubkey());
    assert_eq!(env.history().len(), 1);
    assert_eq!(env.current_prices().vcpu_hour, 150_000_000_000_000_000);
    assert_eq!(env.daily_cost(&allowances_costing(5_000_000)), 5_000_000);
    let now = env.now();
    let (on_chain, _) = env.cost_of_on_chain(allowances_costing(5_000_000), now, now + DAY);
    assert_eq!(on_chain, 5_000_000);
}
