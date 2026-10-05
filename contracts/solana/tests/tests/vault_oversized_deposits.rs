//! Shares are 128-bit. A deposit priced while the pool is worth next to
//! nothing a share would mint more than the vault can count: it goes back to
//! whoever paid it, as a deposit that buys nothing does, and settlement
//! carries on, rather than failing on it for good and holding up every
//! request behind it (no valuation could be recorded, no claim closed and
//! the vault never closed while it was owed).

use anchor_lang::InstructionData;
use shonin_solana_tests::station::events;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_vault::events::Refunded;
use solana_signer::Signer;

#[test]
fn a_deposit_whose_shares_dont_fit_goes_back_and_settlement_carries_on() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    // A $20M vault, every dollar of it deployed (the author's floor at 0).
    env.deposit_as(&v, &alice, 20_000_000_000_000);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 0 }.data(),
    );
    expect_ok(env.as_author(ix));
    env.skip(DAY);
    env.price(&v);
    let cash = env.vault(&v).cash;
    env.deploy_spot(&v, cash);
    assert_eq!(env.vault(&v).cash, 0);
    // The position is wiped out: the pool is worth a few hundred units.
    env.set_spot(1, 0);
    env.price(&v);
    let latest = env.vault(&v).latest;
    assert!(latest.gross < 1_000, "gross {}", latest.gross);

    // A large deposit into the worthless pool, and a withdrawal behind it.
    let assets = 10_000_000_000_000_000;
    let id = env.deposit(&v, &bob, assets);
    let shares = env.shares(&v, &alice.pubkey());
    let behind = env.redeem(&v, &alice, shares / 2);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let supply = env.vault(&v).total_supply;
    let from = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, from, SETTLE_BATCH);
    let payer = env.payer.insecure_clone();
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));

    // Sent back whole to bob, who bought nothing, and said so.
    let vault = env.vault(&v);
    assert!(!vault.owed());
    assert_eq!(env.usd_balance(&bob.pubkey()), assets);
    assert_eq!(env.shares(&v, &bob.pubkey()), 0);
    assert_eq!(vault.escrow, 0);
    assert!(vault.total_supply <= supply);
    let refunded: Vec<Refunded> = emitted(&meta);
    assert_eq!(refunded.len(), 1);
    assert_eq!(refunded[0].request_id, id);
    assert_eq!(refunded[0].assets, assets);
    assert!(events::<Refunded>(&meta.logs).is_empty());
    // The withdrawal behind it was settled, and the vault values on.
    assert!(vault.request_cursor >= behind);
    env.price(&v);
}
