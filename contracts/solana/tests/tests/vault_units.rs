//! The fallback's units: a holder's fraction of what the station holds,
//! outside the shares. Anyone may serve them (the vault trades on its own
//! chain only), closing exactly that fraction and paying all of it to them;
//! dust units are cleared; units nobody could serve for a week freeze the
//! vault; a serve waits for settlement, as a claim does. The counterpart of
//! the EVM `ProRataServesExactly`, `UnitsKeepTheirFraction`,
//! `UnitsRescaleAfterServe`, `UnservedUnitsFreeze`, `ServeWaitsForSettlement`,
//! `ServeNeedsItsWorthHome`, `ServeNeedsMoneyHome`, `DustUnitsAreCleared`
//! and the units test of `Vault.t.sol`.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// Alice out through the fallback, holding units for about half the
/// station: $150,000 of the vault's $200,000 at the venue in spot.
fn alice_holds_units() -> (Env, V, Keypair) {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.trade(
        &v,
        150_000_000_000,
        vec![order(spot(), true, 2 * UNIT, PRICE)],
    );
    env.set_spot(PRICE, 100);
    env.price(&v);
    let id = env.redeem(&v, &alice, a);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &alice, id));
    (env, v, alice)
}

#[test]
fn serving_a_unit_holder_closes_exactly_their_fraction() {
    let (mut env, v, alice) = alice_holds_units();
    env.price(&v);
    let price_before = env.share_price(&v);
    let units = env.holder(&v, &alice.pubkey()).unwrap().units;
    let paid_before = env.usd_balance(&alice.pubkey());
    expect_ok(env.serve(&v, &alice.pubkey()));
    let ix = env.ix_collect_units(&v, &alice.pubkey());
    expect_ok(env.send(vec![ix], &[&alice]));
    let served = (env.usd_balance(&alice.pubkey()) - paid_before) as u128;
    env.price(&v);
    assert!(
        env.share_price(&v) >= price_before - price_before / 100_000,
        "bob paid for alice's close"
    );
    // Her fraction of the station at the exit side: 30k idle and 2 BTC at the
    // bid.
    let fair = (30_000_000_000u128 + 2 * 59_700_000_000) * units / WAD as u128;
    assert!(
        served <= fair + 1,
        "alice was paid more than her exit-side fraction"
    );
    approx_rel(served, fair, 1_000_000_000_000_000);
    let vault = env.vault(&v);
    assert_eq!(vault.units, 0);
    assert_eq!(vault.unit_scale, 0);
    // Units served are gone: serving again is nothing.
    expect_code(
        env.serve(&v, &alice.pubkey()),
        vault_error(VaultError::Nothing),
    );
}

#[test]
fn units_nobody_could_serve_for_a_week_let_anyone_freeze_the_vault() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::test()]);
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.trade(&v, 150_000_000_000, vec![order(spot(), true, UNIT, PRICE)]);
    env.set_mode(shonin_test_driver::mode::FAIL);
    let id = env.redeem(&v, &alice, a);
    let earliest = env.request(&v, id).unwrap().earliest;
    env.warp(earliest + DAY + 1);
    expect_ok(env.take_pro_rata(&v, &alice, id));
    assert!(env.holder(&v, &alice.pubkey()).unwrap().units > 0);
    expect_failure(env.serve(&v, &alice.pubkey()));
    let ix = env.ix_freeze_units(&v, &alice.pubkey());
    expect_code(env.send(vec![ix], &[&bob]), vault_error(VaultError::NotYet));
    let ix = env.ix_freeze_units(&v, &bob.pubkey());
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::NotAllowed),
    );
    env.skip(7 * DAY);
    let ix = env.ix_freeze_units(&v, &alice.pubkey());
    expect_ok(env.send(vec![ix], &[&bob]));
    assert_eq!(env.vault(&v).state, VaultState::Frozen);
}

#[test]
fn a_serve_waits_for_settlement() {
    let (mut env, v, alice) = alice_holds_units();
    let bob = env.bob.insecure_clone();
    let b = env.shares(&v, &bob.pubkey());
    env.redeem(&v, &bob, b / 2);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(env.vault(&v).owed(), "nothing waits to settle");
    expect_code(
        env.serve(&v, &alice.pubkey()),
        vault_error(VaultError::NotYet),
    );
    env.settle_all(&v);
    expect_ok(env.serve(&v, &alice.pubkey()));
}

#[test]
fn a_serve_on_a_stopped_station_waits_for_settlement_and_a_strangers_unwind_too() {
    let (mut env, v, alice) = alice_holds_units();
    let bob = env.bob.insecure_clone();
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    let b = env.shares(&v, &bob.pubkey());
    env.redeem(&v, &bob, b / 2);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(env.vault(&v).owed());
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    // The station asks the vault, which says a priced valuation waits.
    let ix = env.ix_unwind(&v.home, &carol.pubkey(), WAD, false);
    expect_code(
        env.send(vec![ix], &[&carol]),
        station_error(StationError::Unsettled),
    );
    expect_code(
        env.serve(&v, &alice.pubkey()),
        vault_error(VaultError::NotYet),
    );
}

#[test]
fn a_unit_holder_is_not_served_while_their_worth_stays_at_the_venue() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::test()]);
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.trade(
        &v,
        150_000_000_000,
        vec![order(spot(), true, 2 * UNIT, PRICE)],
    );
    let id = env.redeem(&v, &alice, a);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &alice, id));
    let held = env.holder(&v, &alice.pubkey()).unwrap().units;
    // The venue keeps the proceeds as margin: the serve is undone, the units
    // wait, nothing credited.
    env.set_mode(shonin_test_driver::mode::KEEP_PROCEEDS);
    expect_failure(env.serve(&v, &alice.pubkey()));
    let after = env.holder(&v, &alice.pubkey()).unwrap();
    assert_eq!(after.units, held);
    assert_eq!(after.unit_credit, 0);
    env.set_mode(shonin_test_driver::mode::HONEST);
    expect_ok(env.serve(&v, &alice.pubkey()));
    assert_eq!(env.holder(&v, &alice.pubkey()).unwrap().units, 0);
}

#[test]
fn dust_units_are_cleared_and_never_block_a_send_to_the_station() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.trade(
        &v,
        100_000_000_000,
        vec![order(spot(), true, 3 * UNIT / 2, PRICE)],
    );
    let dust = env.vault(&v).total_supply / 1_000_000_000_000;
    let id = env.redeem(&v, &bob, dust);
    env.set_spot(PRICE, 2_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    env.skip(DAY);
    expect_ok(env.take_pro_rata(&v, &bob, id));
    env.set_spot(PRICE, 0);
    assert!(env.vault(&v).units > 0, "no units were minted");
    expect_ok(env.serve(&v, &bob.pubkey()));
    assert_eq!(env.vault(&v).units, 0, "the dust units were not cleared");
    env.price(&v);
    expect_ok(env.to_station(&v, 1_000_000_000));
}

#[test]
fn units_keep_their_fraction_when_a_claim_is_served_and_when_fees_are_paid() {
    let (mut env, v, alice) = alice_holds_units();
    let bob = env.bob.insecure_clone();
    let units_before = env.holder(&v, &alice.pubkey()).unwrap().units;
    let vault = env.vault(&v);
    let fraction_before = units_before * vault.scale() / WAD as u128;
    let exit_before = env.value(&v.home).exit as u128;
    // A payday mints fee shares: the units' fraction of the station is
    // theirs for good, whatever is minted later.
    env.gain(&v, 10_000_000_000);
    env.skip(8 * DAY);
    env.price(&v);
    let vault = env.vault(&v);
    let fraction_after =
        env.holder(&v, &alice.pubkey()).unwrap().units * vault.scale() / WAD as u128;
    assert_eq!(
        fraction_after, fraction_before,
        "fee shares diluted the units"
    );
    // Bob's claim closes the shares' part only; the units' worth is kept.
    let b = env.shares(&v, &bob.pubkey());
    env.redeem(&v, &bob, b);
    env.price(&v);
    let exit_mid = env.value(&v.home).exit as u128;
    let worth_mid = exit_mid * fraction_after / WAD as u128;
    expect_ok(env.settle_claim(&v, b_claim(&env, &v, &bob)));
    let vault = env.vault(&v);
    let fraction_end = env.holder(&v, &alice.pubkey()).unwrap().units * vault.scale() / WAD as u128;
    let exit_end = env.value(&v.home).exit as u128;
    approx_rel(
        exit_end * fraction_end / WAD as u128,
        worth_mid,
        1_000_000_000_000_000,
    );
    assert!(exit_before > 0);
}

/// Bob's open claim's id.
fn b_claim(env: &Env, v: &V, bob: &Keypair) -> u64 {
    let count = env.vault(v).request_count;
    (1..=count)
        .rev()
        .find(|id| env.claim(v, *id).is_some_and(|c| c.owner == bob.pubkey()))
        .expect("bob has a claim")
}
