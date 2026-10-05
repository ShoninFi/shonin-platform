//! Fees are measured at every valuation and paid on payday, the first priced
//! valuation on or after Monday 00:00 UTC; a leaver pays their own fee as a
//! slice of their shares; money joining above the high-water mark blends it.
//! The counterpart of the EVM `Vault.t.sol` fee tests and
//! `VaultProperties.t_a_vault_that_never_traded_owes_a_profit_fee_on_its_own_transaction_fees`.
//!
//! The EVM tests make the pool gain by sending the station dollars, which an
//! EVM station counts; a Solana station counts only what it accounted for, so
//! here the pool gains through the price of a spot position at the venue.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_signer::Signer;

/// Monday 00:00 UTC of the week after the one `t` falls in.
fn next_monday_after(t: u64) -> u64 {
    let day = t / DAY;
    let since = day + 3;
    (since - since % 7 + 4) * DAY
}

/// A fresh fee week: payday has just run, the next is a week away.
fn fresh_week(env: &mut Env, v: &V) {
    let at = next_monday_after(env.vault(v).last_payment) + 1;
    env.warp(at);
    env.price(v);
}

fn to_payday(env: &mut Env, v: &V) {
    fresh_week(env, v);
}

/// Alice's $100,000 in the vault, $80,000 of it in spot at the venue.
fn with_spot() -> (Env, V) {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    (env, v)
}

#[test]
fn the_holding_fee_dilutes_the_price_and_is_handed_over_on_payday() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    fresh_week(&mut env, &v);
    let before = env.share_price(&v);
    env.skip(73 * DAY / 20);
    env.price(&v);
    // A hundredth of a year at 1%: a ten-thousandth lower, the shares held by
    // the vault until payday.
    approx(
        env.share_price(&v),
        before - before / 10_000,
        before / 100_000,
    );
    assert!(env.vault(&v).unpaid_holding > 0);
    let platform_before = env.shares(&v, &env.platform.pubkey());
    to_payday(&mut env, &v);
    assert_eq!(env.vault(&v).unpaid_holding, 0);
    assert!(env.shares(&v, &env.platform.pubkey()) > platform_before);
}

#[test]
fn the_profit_fee_is_owed_all_week_and_paid_on_monday_without_moving_the_price() {
    let (mut env, v) = with_spot();
    fresh_week(&mut env, &v);
    env.skip(3 * DAY);
    env.price(&v);
    env.gain(&v, 10_000_000_000);
    env.price(&v);
    let owed = env.vault(&v).latest;
    // 15% of the gain is owed, and the price is shown after it.
    approx(owed.fee, 1_500_000_000, 5_000_000);
    let author = env.author.pubkey();
    let platform = env.platform.pubkey();
    let (author_before, platform_before) = (env.shares(&v, &author), env.shares(&v, &platform));
    let price_before = env.share_price(&v);
    to_payday(&mut env, &v);
    let to_author = env.shares(&v, &author) - author_before;
    let to_platform = env.shares(&v, &platform) - platform_before;
    assert_eq!(env.vault(&v).latest.fee, 0);
    approx(env.share_price(&v), price_before, price_before / 10_000);
    // $1,500 of fee: two thirds to the author, a third (and the holding fee)
    // to the platform.
    approx_rel(
        to_author * env.share_price(&v) / 1_000_000_000_000_000_000,
        1_000_000_000,
        10_000_000_000_000_000,
    );
    assert!(to_platform * 2 > to_author);
}

#[test]
fn a_gain_given_back_before_payday_mints_nothing() {
    let (mut env, v) = with_spot();
    fresh_week(&mut env, &v);
    env.skip(3 * DAY);
    env.price(&v);
    env.gain(&v, 10_000_000_000);
    env.price(&v);
    assert!(env.vault(&v).latest.fee > 0);
    let author = env.author.pubkey();
    let before = env.shares(&v, &author);
    env.gain(&v, -10_000_000_000);
    to_payday(&mut env, &v);
    assert_eq!(env.shares(&v, &author), before);
}

#[test]
fn a_deposit_above_the_mark_blends_it() {
    let (mut env, v) = with_spot();
    env.gain(&v, 20_000_000_000);
    env.price(&v);
    let price_before = env.share_price(&v);
    let vault = env.vault(&v);
    let (fee_before, mark_before) = (vault.latest.fee, vault.mark);
    let bob = env.bob.insecure_clone();
    env.deposit_as(&v, &bob, 100_000_000_000);
    let vault = env.vault(&v);
    assert!(vault.mark > mark_before);
    approx(env.share_price(&v), price_before, price_before / 10_000);
    approx(vault.latest.fee, fee_before, fee_before / 1_000 + 1);
}

#[test]
fn a_deposit_below_the_mark_leaves_it_and_rides_the_recovery_free() {
    let (mut env, v) = with_spot();
    env.gain(&v, -10_000_000_000);
    env.price(&v);
    let mark_before = env.vault(&v).mark;
    let bob = env.bob.insecure_clone();
    env.deposit_as(&v, &bob, 100_000_000_000);
    assert_eq!(env.vault(&v).mark, mark_before);
}

#[test]
fn a_withdrawal_pays_its_own_fee_and_moves_nobody_elses_price() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    env.gain(&v, 20_000_000_000);
    env.price(&v);
    let price_before = env.share_price(&v);
    let paid = env.redeem_as(&v, &alice, shares);
    // Half of 220k, less 15% of half the 20k gain, less 0.1%; a little less
    // for the venue's spread on the exit side.
    approx(
        paid as u128,
        (110_000_000_000 - 1_500_000_000) * 999 / 1000,
        50_000_000,
    );
    assert!(
        env.shares(&v, &env.author.pubkey()) > 0,
        "the slice went to the author"
    );
    assert!(env.share_price(&v) >= price_before - price_before / 100_000);
}

#[test]
fn a_withdrawal_is_paid_at_the_exit_side_and_the_spread_stays_with_the_pool() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 120_000_000_000);
    env.set_spot(PRICE, 200);
    env.price(&v);
    let latest = env.vault(&v).latest;
    assert!(latest.gross_exit < latest.gross);
    let price_before = env.share_price(&v);
    let paid = env.redeem_as(&v, &alice, shares);
    assert!(paid < 100_000_000_000 - 100_000_000);
    assert!(env.share_price(&v) >= price_before);
}

#[test]
fn a_vault_that_never_traded_owes_a_profit_fee_on_its_own_transaction_fees() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.redeem_as(&v, &alice, shares);
    // The transaction fees stayed in the pool and raised value per share
    // above the mark: a sliver of them is owed as profit, as the formula
    // says, and nothing else is.
    env.price(&v);
    let fee = env.vault(&v).latest.fee;
    assert!(fee > 0);
    assert!(fee < 100_000_000, "{fee}");
}

#[test]
fn the_price_is_the_same_either_side_of_payday() {
    let (mut env, v) = with_spot();
    env.gain(&v, 5_000_000_000);
    fresh_week(&mut env, &v);
    env.skip(6 * DAY);
    env.gain(&v, 5_000_000_000);
    env.price(&v);
    let before = env.share_price(&v);
    to_payday(&mut env, &v);
    approx(env.share_price(&v), before, before / 10_000);
}
