//! The pro-rata fallback: a withdrawal nothing has priced for a day takes its
//! share of the cash now and units for the rest; a claim nobody could serve
//! for a day from its booking may too; a deposit goes back only when nothing
//! could price it. Each pays its own profit fee and the transaction fee, and
//! none is open while a priced valuation waits to settle or once the vault
//! is ready to close. The counterpart of the EVM `Vault.t.sol` fallback
//! test, `DepositFallbackOnlyWhenNothingCanPrice`, `FallbackPaysTheProfitFee`,
//! `FallbackChargesTheFeeItsSharesOwe`, `ClaimFallbackFromBooking`,
//! `QueuedClaimFallback`, `QueuedClaimFallsBackAsARequest`,
//! `RedeemFallbackWaitsForPricedWithdrawals`, `FallbackBesideACashOnlyClaim`,
//! `ClosingVaultRefusesTheFallback`, `FallbacksInAnyOrderNeverHoldTheQueue`
//! and `FallbackTakesCannotStallTheQueue`.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_signer::Signer;

#[test]
fn with_no_valuation_for_a_day_a_leaver_takes_the_cash_share_and_units() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 150_000_000_000));
    let id = env.redeem(&v, &alice, a);
    expect_code(
        env.take_pro_rata(&v, &alice, id),
        vault_error(VaultError::NotYet),
    );
    env.skip(DAY + SLOT);
    // Only its owner takes it.
    let bob_takes = env.ix_take_pro_rata(&v, &bob.pubkey(), id);
    expect_code(
        env.send(vec![bob_takes], &[&bob]),
        vault_error(VaultError::NotAllowed),
    );
    expect_ok(env.take_pro_rata(&v, &alice, id));
    // About half of the 50k left in cash, and units for the rest.
    approx(
        env.usd_balance(&alice.pubkey()) as u128,
        25_000_000_000,
        50_000_000,
    );
    let units = env.holder(&v, &alice.pubkey()).unwrap().units;
    approx_rel(units, WAD as u128 / 2, 10_000_000_000_000_000);
    // Nothing goes out to the station while units are owed.
    expect_code(env.to_station(&v, 1), vault_error(VaultError::WrongState));
    // Money coming home is split: alice's part to her units.
    expect_ok(env.from_station(&v, 150_000_000_000));
    let ix = env.ix_collect_units(&v, &alice.pubkey());
    expect_ok(env.send(vec![ix], &[&alice]));
    approx(
        env.usd_balance(&alice.pubkey()) as u128,
        100_000_000_000,
        200_000_000,
    );
    // Twice is nothing.
    let ix = env.ix_collect_units(&v, &alice.pubkey());
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::Nothing),
    );
}

#[test]
fn a_deposit_a_valuation_could_price_is_not_sent_back() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    let id = env.deposit(&v, &bob, 10_000_000_000);
    let earliest = env.request(&v, id).unwrap().earliest;
    env.warp(earliest + DAY + 1);
    expect_code(
        env.take_pro_rata(&v, &bob, id),
        vault_error(VaultError::NotAllowed),
    );
    env.price(&v);
    assert!(
        env.request(&v, id).unwrap().done,
        "the deposit was not priced"
    );
    assert!(env.shares(&v, &bob.pubkey()) > 0, "bob has no shares");
}

#[test]
fn a_deposit_goes_back_when_the_market_is_outside_the_band() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 50_000_000_000);
    let id = env.deposit(&v, &bob, 10_000_000_000);
    env.set_prices(spot(), PRICE, PRICE, PRICE * 90 / 100, PRICE);
    let earliest = env.request(&v, id).unwrap().earliest;
    env.warp(earliest + DAY + 1);
    expect_ok(env.take_pro_rata(&v, &bob, id));
    assert_eq!(
        env.usd_balance(&bob.pubkey()),
        10_000_000_000,
        "bob's deposit did not come back"
    );
    assert_eq!(env.vault(&v).escrow, 0);
}

#[test]
fn a_deposit_nobody_could_value_for_a_week_goes_back_without_asking_the_station() {
    // On Solana a driver that fails fails the transaction asking it, so a
    // station that can't be valued can't be shown to be one. A deposit
    // nothing priced for as long as an unpaid claim takes to freeze a vault
    // goes back without the station being asked.
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::test()]);
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    let id = env.deposit(&v, &bob, 10_000_000_000);
    env.set_mode(shonin_test_driver::mode::FAIL);
    let earliest = env.request(&v, id).unwrap().earliest;
    env.warp(earliest + DAY + 1);
    expect_failure(env.take_pro_rata(&v, &bob, id));
    env.warp(earliest + 7 * DAY);
    expect_ok(env.take_pro_rata(&v, &bob, id));
    assert_eq!(env.usd_balance(&bob.pubkey()), 10_000_000_000);
}

#[test]
fn the_fallback_pays_the_profit_fee_its_shares_owe() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 180_000_000_000);
    env.gain(&v, 20_000_000_000);
    env.price(&v);
    let author = env.author.pubkey();
    let author_before = env.shares(&v, &author);
    let id = env.redeem(&v, &alice, a);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &alice, id));
    // A slice of her shares went to the author and the platform: the
    // fallback is a way out, never a way round the fee.
    assert!(env.shares(&v, &author) > author_before, "no fee was taken");
}

#[test]
fn the_fallback_waits_while_a_priced_valuation_waits_to_settle() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    let first = env.redeem(&v, &alice, a);
    env.skip(DAY + SLOT);
    // Bob's withdrawal is priced but not yet settled: its cash comes first.
    env.redeem(&v, &bob, b / 2);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    expect_code(
        env.take_pro_rata(&v, &alice, first),
        vault_error(VaultError::NotYet),
    );
    env.settle_all(&v);
    // Priced at that valuation, alice's is now an ordinary paid exit.
    assert!(env.request(&v, first).is_none_or(|r| r.done));
}

#[test]
fn a_claim_nobody_could_serve_for_a_day_from_its_booking_takes_the_fallback() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.trade(&v, 190_000_000_000, vec![order(spot(), true, UNIT, PRICE)]);
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    env.set_spot(PRICE, 600);
    // Valuations keep being recorded (void), and still the day counts from
    // its booking.
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NotYet),
    );
    env.skip(DAY);
    let ix = env.ix_take_pro_rata_claim(&v, &bob.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::NotAllowed),
    );
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Fallback);
    assert!(env.holder(&v, &alice.pubkey()).unwrap().units > 0);
    assert!(env.usd_balance(&alice.pubkey()) > 0);
}

#[test]
fn a_claim_the_cash_covers_is_refused_the_fallback_and_a_queued_one_falls_back_as_a_request() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 190_000_000_000));
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    expect_ok(env.from_station(&v, 190_000_000_000));
    env.skip(DAY);
    // The cash covers it at the last priced valuation: it can be served now,
    // at a price.
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NotAllowed),
    );
    // Queued, it is a request like any other: a day from its request.
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Queued);
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NotYet),
    );
    env.skip(DAY + SLOT);
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Fallback);
    assert_eq!(env.vault(&v).queued_claims, 0);
    // The queued request is done and passed by settlement, never paid twice.
    let request = env.claim(&v, id).unwrap().request;
    assert!(env.request(&v, request).unwrap().done);
    let paid = env.usd_balance(&alice.pubkey());
    env.price(&v);
    assert_eq!(env.usd_balance(&alice.pubkey()), paid);
}

#[test]
fn fallbacks_taken_in_any_order_never_hold_the_queue() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000);
    const N: u64 = 20;
    let mut first = 0;
    for i in 0..N {
        let id = env.redeem(&v, &bob, 1);
        if i == 0 {
            first = id;
        }
    }
    let earliest = env.request(&v, first).unwrap().earliest;
    env.warp(earliest + DAY + 1);
    for i in (0..N).rev() {
        expect_ok(env.take_pro_rata(&v, &bob, first + i));
    }
    // Alice now wants out; the keeper settles before it values.
    let a = env.shares(&v, &alice.pubkey());
    env.redeem(&v, &alice, a);
    env.settle_all(&v);
    env.price(&v);
    assert!(
        env.usd_balance(&alice.pubkey()) > 99_000_000_000,
        "alice was never paid"
    );
}

#[test]
fn a_vault_ready_to_close_refuses_the_fallback() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    let id = env.redeem(&v, &alice, a / 2);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.skip(DAY + SLOT);
    // A valuation after the halt finds the station empty: the vault only waits
    // to close, and pays through its own door.
    env.price(&v);
    let vault = env.vault(&v);
    if vault.ready_to_close() {
        let r = env.request(&v, id);
        if r.is_some_and(|r| !r.done) {
            expect_code(
                env.take_pro_rata(&v, &alice, id),
                vault_error(VaultError::WrongState),
            );
        }
        let ix = env.ix_request_redeem(&v, &bob.pubkey(), 1, &bob.pubkey());
        expect_code(
            env.send(vec![ix], &[&bob]),
            vault_error(VaultError::WrongState),
        );
    } else {
        panic!("the vault should be ready to close");
    }
}
