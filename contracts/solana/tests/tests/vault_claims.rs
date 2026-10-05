//! A withdrawal the cash can't pay becomes a claim, paid in the transaction
//! that closes the leaver's fraction of every position: what the close
//! brought home and their part of the cash just before, less their own
//! profit fee in cash and the transaction fee. The counterpart of the EVM
//! `Claims.t.sol`, the claim tests of `Vault.t.sol`, `ClaimPaidFromCashHome`,
//! `ServableClaimKeepsThePricedPath`, `ClaimWaitsForPricedDeposits`,
//! `RefusedCloseIsNotATry`, `EmptyClaimTryIsNotATry`, `NothingHomeIsNeverAFill`
//! and `WideMarketWithdrawalBecomesAClaim`.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// `amount` of the vault's cash to the venue as `size` BTC spot long against
/// the same perp short, at `PRICE` with no spread.
fn hedge(env: &mut Env, v: &V, amount: u64, size: u128) {
    env.trade(
        v,
        amount,
        vec![
            order(spot(), true, size, PRICE),
            order(perp(), false, size, 0),
        ],
    );
}

/// What `shares` own right now: their part of the cash and of what the
/// shares own of the station, at exit prices.
fn worth(env: &mut Env, v: &V, shares: u128) -> u128 {
    let exit = env.value(&v.home).exit as u128;
    let vault = env.vault(v);
    let units = vault.live_units().unwrap();
    let owned = vault.cash as u128 + exit * (WAD as u128 - units) / WAD as u128;
    owned * shares / vault.total_supply
}

fn spot_size(env: &Env, v: &V) -> u128 {
    env.position(&v.home.authority(0), spot()) as u128
}

/// `who` asks to redeem `shares`, priced into a claim; answers with its id.
fn claim_for(env: &mut Env, v: &V, who: &Keypair, shares: u128) -> u64 {
    let id = env.redeem(v, who, shares);
    env.price(v);
    let claim = env.claim(v, id).expect("it didn't become a claim");
    assert_eq!(claim.state, ClaimState::Pending);
    id
}

fn carol(env: &mut Env) -> Keypair {
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    carol
}

#[test]
fn a_claim_is_paid_in_the_transaction_that_brings_its_fraction_home() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    let alice_worth = worth(&mut env, &v, a);
    let bob_before = worth(&mut env, &v, b);

    expect_ok(env.settle_claim(&v, id));

    let claim = env.claim(&v, id).unwrap();
    assert_eq!(claim.state, ClaimState::Paid, "not paid at its close");
    approx(spot_size(&env, &v), UNIT / 2, UNIT / 1000);
    // Nothing above the mark: only the transaction fee comes off.
    let paid = env.usd_balance(&alice.pubkey()) as u128;
    assert_eq!(paid, claim.paid as u128);
    assert!(
        paid <= alice_worth,
        "she was paid more than her shares owned"
    );
    approx_rel(paid, alice_worth * 999 / 1000, 500_000_000_000_000);
    // The vault holds only holding-fee shares waiting for payday.
    assert_eq!(env.vault(&v).pending_shares, 0, "her shares weren't burned");
    // Bob owns what he owned, and a share of the fee she left behind.
    assert!(worth(&mut env, &v, b) + 1 >= bob_before);
}

#[test]
fn a_paid_claim_cannot_be_tried_again_and_nothing_closes_twice() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    expect_ok(env.settle_claim(&v, id));
    let left = spot_size(&env, &v);
    expect_code(
        env.settle_claim(&v, id),
        vault_error(VaultError::NotAllowed),
    );
    env.price(&v);
    assert_eq!(spot_size(&env, &v), left, "a paid claim closed more");
}

#[test]
fn two_claims_served_one_after_the_other_each_take_their_fraction() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let carol = carol(&mut env);
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    let c = env.deposit_as(&v, &carol, 100_000_000_000);
    hedge(&mut env, &v, 285_000_000_000, UNIT);
    let first = env.redeem(&v, &alice, a);
    let second = env.redeem(&v, &bob, b);
    env.price(&v);
    assert_eq!(env.vault(&v).claim_count, 2);
    let carol_before = worth(&mut env, &v, c);
    let alice_worth = worth(&mut env, &v, a);
    expect_ok(env.settle_claim(&v, first));
    let bob_worth = worth(&mut env, &v, b);
    expect_ok(env.settle_claim(&v, second));
    approx_rel(
        env.usd_balance(&alice.pubkey()) as u128,
        alice_worth * 999 / 1000,
        500_000_000_000_000,
    );
    approx_rel(
        env.usd_balance(&bob.pubkey()) as u128,
        bob_worth * 999 / 1000,
        500_000_000_000_000,
    );
    approx(spot_size(&env, &v), UNIT / 3, UNIT / 500);
    assert!(worth(&mut env, &v, c) + 1 >= carol_before);
}

#[test]
fn a_claim_pays_its_own_profit_fee_in_cash_and_nobody_elses() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 180_000_000_000);
    // A gain at the venue puts the pool above its mark.
    env.gain(&v, 20_000_000_000);
    let id = claim_for(&mut env, &v, &alice, a);
    let alice_worth = worth(&mut env, &v, a);
    let basis = (shonin_common::math::U256::from(env.vault(&v).mark)
        * shonin_common::math::U256::from(a)
        / shonin_common::math::U256::from(PRICE_SCALE))
    .as_u128();
    let fee = (alice_worth - basis) * 1500 / 10_000;
    let bob_before = worth(&mut env, &v, b);
    let author = env.author.pubkey();
    let platform = env.platform.pubkey();
    env.token_account(&author);
    let (author_before, platform_before) = (env.usd_balance(&author), env.usd_balance(&platform));

    expect_ok(env.settle_claim(&v, id));

    let to_author = (env.usd_balance(&author) - author_before) as u128;
    let to_platform = (env.usd_balance(&platform) - platform_before) as u128;
    approx_rel(to_author + to_platform, fee, 1_000_000_000_000_000);
    approx_rel(to_author, to_platform * 2, 1_000_000_000_000_000);
    let gross = alice_worth - fee;
    approx_rel(
        env.usd_balance(&alice.pubkey()) as u128,
        gross - gross / 1000,
        500_000_000_000_000,
    );
    assert!(
        worth(&mut env, &v, b) + 1 >= bob_before,
        "the holder who stayed paid her fee"
    );
}

#[test]
fn a_payday_after_the_close_changes_nothing_she_was_paid() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 180_000_000_000);
    env.gain(&v, 20_000_000_000);
    let id = claim_for(&mut env, &v, &alice, a);
    expect_ok(env.settle_claim(&v, id));
    let paid = env.usd_balance(&alice.pubkey());
    env.skip(8 * DAY);
    env.price(&v);
    assert_eq!(env.usd_balance(&alice.pubkey()), paid);
    assert_eq!(env.claim(&v, id).unwrap().paid, paid);
}

#[test]
fn a_claim_served_while_units_exist_takes_all_its_close_and_the_units_keep_their_value() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let carol = carol(&mut env);
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    let c = env.deposit_as(&v, &carol, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 285_000_000_000));
    // Carol leaves through the fallback: units for a third of the station.
    let request = env.redeem(&v, &carol, c);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &carol, request));
    let carol_units = env.holder(&v, &carol.pubkey()).unwrap().units;
    let exit_before = env.value(&v.home).exit as u128;
    let units_worth = exit_before * carol_units / WAD as u128;

    let id = claim_for(&mut env, &v, &alice, a);
    let alice_worth = worth(&mut env, &v, a);
    let bob_before = worth(&mut env, &v, b);
    expect_ok(env.settle_claim(&v, id));

    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    approx_rel(
        env.usd_balance(&alice.pubkey()) as u128,
        alice_worth * 999 / 1000,
        500_000_000_000_000,
    );
    let vault = env.vault(&v);
    let live = env.holder(&v, &carol.pubkey()).unwrap().units * vault.scale() / WAD as u128;
    let exit_after = env.value(&v.home).exit as u128;
    approx_rel(
        exit_after * live / WAD as u128,
        units_worth,
        100_000_000_000_000,
    );
    assert!(worth(&mut env, &v, b) + 1_000_000 >= bob_before);
}

#[test]
fn a_claim_the_cash_covers_is_priced_at_the_next_valuation_and_its_cash_stays_home() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 190_000_000_000));
    let id = claim_for(&mut env, &v, &alice, a);
    expect_ok(env.from_station(&v, 190_000_000_000));
    // Nothing to close: an ordinary request, priced at the next slot.
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Queued);
    assert_eq!(
        env.usd_balance(&alice.pubkey()),
        0,
        "paid before it was priced"
    );
    // Meanwhile nothing goes out to the station, and it can't be tried
    // again.
    expect_code(env.to_station(&v, 1), vault_error(VaultError::WrongState));
    expect_code(
        env.settle_claim(&v, id),
        vault_error(VaultError::NotAllowed),
    );
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    approx_rel(
        env.usd_balance(&alice.pubkey()) as u128,
        100_000_000_000 * 999 / 1000,
        1_000_000_000_000_000,
    );
    expect_ok(env.to_station(&v, 1));
}

#[test]
fn a_deposit_beside_the_leavers_own_claim_buys_at_the_next_valuation_and_takes_nothing() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let carol = carol(&mut env);
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    let b = env.deposit_as(&v, &bob, 100_000_000_000);
    let c = env.deposit_as(&v, &carol, 100_000_000_000);
    hedge(&mut env, &v, 285_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    let alice_worth = worth(&mut env, &v, a);
    let (bob_before, carol_before) = (worth(&mut env, &v, b), worth(&mut env, &v, c));
    // Alice asks to deposit again, then serves her own claim.
    env.deposit(&v, &alice, 100_000_000_000);
    expect_ok(env.settle_claim(&v, id));
    assert!(
        env.usd_balance(&alice.pubkey()) as u128 <= alice_worth,
        "her claim took part of the deposit"
    );
    env.price(&v);
    let shares = env.shares(&v, &alice.pubkey());
    approx_rel(
        worth(&mut env, &v, shares),
        100_000_000_000 * 999 / 1000,
        1_000_000_000_000_000,
    );
    assert!(worth(&mut env, &v, b) + 1_000_000 >= bob_before);
    assert!(worth(&mut env, &v, c) + 1_000_000 >= carol_before);
}

#[test]
fn a_claim_waits_while_the_market_is_outside_the_band_and_the_try_counts() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    env.set_spot(PRICE, 600);
    expect_ok(env.settle_claim(&v, id));
    let claim = env.claim(&v, id).unwrap();
    assert_eq!(claim.state, ClaimState::Waiting);
    assert_eq!(claim.last_attempt_at, env.now());
    // Back inside the band, it is paid.
    env.set_spot(PRICE, 0);
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
}

#[test]
fn a_claim_unpaid_for_a_week_freezes_the_vault_whatever_was_tried() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    env.set_spot(PRICE, 600);
    env.skip(6 * DAY);
    expect_ok(env.settle_claim(&v, id));
    expect_code(env.freeze(&v, id), vault_error(VaultError::NotYet));
    env.skip(DAY);
    expect_ok(env.freeze(&v, id));
    let vault = env.vault(&v);
    assert_eq!(vault.state, VaultState::Frozen);
    assert!(vault.halted_at > 0);
    // A halted vault can't be frozen again.
    expect_code(env.freeze(&v, id), vault_error(VaultError::WrongState));
}

#[test]
fn a_paid_claim_can_never_freeze_the_vault() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    env.skip(7 * DAY - 1);
    expect_ok(env.settle_claim(&v, id));
    env.skip(DAY);
    expect_code(env.freeze(&v, id), vault_error(VaultError::NotAllowed));
}

#[test]
fn a_close_the_station_refuses_is_not_a_try_and_changes_nothing() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::test()]);
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    let id = claim_for(&mut env, &v, &alice, a);
    let before = env.claim(&v, id).unwrap();
    let position = spot_size(&env, &v);
    // The venue keeps the proceeds: the station can't bring the fraction's
    // worth home and undoes the close.
    env.set_mode(shonin_test_driver::mode::KEEP_PROCEEDS);
    env.skip(HOUR);
    expect_code(
        env.settle_claim(&v, id),
        station_error(StationError::ShortSweep),
    );
    // A failed close fails the whole transaction: the claim is exactly as it
    // was, its freeze clock still running from its booking, and nothing sold.
    let after = env.claim(&v, id).unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.last_attempt_at, before.last_attempt_at);
    assert_eq!(env.usd_balance(&alice.pubkey()), 0);
    assert_eq!(spot_size(&env, &v), position);
    env.set_mode(shonin_test_driver::mode::HONEST);
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
}

#[test]
fn a_withdrawal_asked_while_a_market_is_outside_the_band_becomes_a_claim_after_a_day() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    hedge(&mut env, &v, 190_000_000_000, UNIT);
    env.set_spot(PRICE, 600);
    let id = env.redeem(&v, &alice, a);
    // Every valuation is void while the market is outside the band.
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(
        !env.vault(&v).priced || env.vault(&v).latest.time < env.request(&v, id).unwrap().earliest
    );
    let ix = env.ix_claim_instead(&v, &alice.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NotYet),
    );
    env.skip(DAY);
    // Only its owner may.
    let ix = env.ix_claim_instead(&v, &bob.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::NotAllowed),
    );
    let ix = env.ix_claim_instead(&v, &alice.pubkey(), id);
    expect_ok(env.send(vec![ix], &[&alice]));
    let claim = env.claim(&v, id).unwrap();
    assert_eq!(claim.state, ClaimState::Pending);
    // Booked from when it was due, so the week to a freeze counts from then.
    assert_eq!(claim.booked_at, env.request_earliest_of(&v, id));
    env.set_spot(PRICE, 0);
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
}
