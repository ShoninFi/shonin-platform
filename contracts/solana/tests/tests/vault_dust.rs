//! Dust never holds a claim, a serve or a close. A claim worth fewer
//! dollar-units at the station than its close may round away brings nothing
//! home however often it is tried: it is paid its share of the cash and done,
//! so it can't hold a vault open or freeze it. On Solana a station may round
//! each order's proceeds down at its venue, and leaves alone (or closes with
//! nothing home) a leaver's part of a venue account worth up to the square of
//! one more than the orders its close places; so the vault's dust is
//! `drivers * (MAX_MARKETS + 1)^2 + 1` dollar-units, where the EVM vault's is
//! `(drivers + 1)^2`. The counterpart of `DustClaimIsPaid`,
//! `DustAtEveryVenueIsCleared`, `DustLeavesNoUnits`,
//! `DustIsNeverJudgedOnAFailedClose` and `PushedMarkNeverPaysAClaimAsDust`.

use anchor_lang::InstructionData;
use shonin_driver::MAX_MARKETS;
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_signer::Signer;

#[test]
fn the_dust_threshold_covers_every_order_at_every_venue_and_the_idle_part() {
    let mut env = Env::with_vaults();
    let one = env.open(&[Declared::local()]);
    let three = env.open(&Declared::one_per_venue());
    let per_venue = (MAX_MARKETS as u128 + 1).pow(2);
    assert_eq!(env.vault(&one).dust(), per_venue + 1);
    assert_eq!(env.vault(&three).dust(), 3 * per_venue + 1);
}

/// Alice's $90,000, every dollar of the vault's cash sent to the station:
/// a third at the venue, the rest idle there.
fn all_at_the_station() -> (Env, V) {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 90_000_000_000);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 0 }.data(),
    );
    expect_ok(env.as_author(ix));
    env.skip(DAY);
    env.price(&v);
    env.approve_executor(&v.home);
    let cash = env.vault(&v).cash;
    expect_ok(env.to_station(&v, cash));
    env.fund_venue(&v.home, 30_000_000_000);
    assert_eq!(env.vault(&v).cash, 0);
    env.price(&v);
    (env, v)
}

#[test]
fn a_claim_worth_less_than_its_rounding_is_paid_as_dust_not_left_to_freeze_the_vault() {
    let (mut env, v) = all_at_the_station();
    let alice = env.alice.insecure_clone();
    // 2.9 trillionths of a share: about $0.0000029, a payout of a unit or two,
    // which the empty cash can't pay, so it is booked as a claim.
    let id = env.redeem(&v, &alice, 2_900_000_000_000);
    env.price(&v);
    assert_eq!(
        env.claim(&v, id).expect("a claim").state,
        ClaimState::Pending
    );
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    env.skip(8 * DAY);
    expect_code(env.freeze(&v, id), vault_error(VaultError::NotAllowed));
}

#[test]
fn a_claim_worth_more_than_dust_is_never_paid_as_dust_by_a_close_that_brought_nothing_home() {
    let (mut env, v) = all_at_the_station();
    let alice = env.alice.insecure_clone();
    let shares = env.shares(&v, &alice.pubkey());
    let id = env.redeem(&v, &alice, shares / 10);
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    // Its close brings its tenth of everything home, and pays it in full.
    expect_ok(env.settle_claim(&v, id));
    let paid = env.claim(&v, id).unwrap().paid as u128;
    approx_rel(paid, 9_000_000_000 * 999 / 1000, 2_000_000_000_000_000);
}
