//! Deposits and withdrawals are requests, priced at a valuation after they
//! are made, never at a price anyone could see when asking; a slot settles
//! its exits before its deposits, and every deposit in a slot pays one
//! price. The counterpart of the EVM `Vault.t.sol` request tests,
//! `DepositsInOneSlotPayOnePrice`, `EqualDepositsBuyEqualShares`,
//! `ExitsSettleBeforeDeposits`, `DepositsPricedOnThePoolTheExitsLeft`,
//! `RefundGoesToThePayer`, `PricedWithdrawalsSettleFirst`,
//! `NothingLeavesWhileAPricedValuationWaits` and
//! `VaultProperties.t_a_one_unit_redemption_pays_nothing_and_books_no_claim`.

use anchor_lang::InstructionData;
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// What a share is worth now, after the profit fee the vault owes now: the
/// same formula a valuation uses, on the live state.
fn live_net_price(env: &mut Env, v: &V) -> u128 {
    let station = env.value(&v.home).mark as u128;
    let vault = env.vault(v);
    let gross = vault.cash as u128 + station + 1;
    let supply = vault.total_supply + OFFSET;
    let basis = (shonin_common::math::U256::from(vault.mark)
        * shonin_common::math::U256::from(supply)
        + shonin_common::math::U256::from(PRICE_SCALE - 1))
        / shonin_common::math::U256::from(PRICE_SCALE);
    let basis = basis.as_u128();
    let fee = if gross > basis {
        (gross - basis) * 1500 / 10_000
    } else {
        0
    };
    (gross - fee) * 1_000_000_000_000_000_000 / supply
}

fn snapshot_net_price(env: &Env, v: &V) -> u128 {
    let l = env.vault(v).latest;
    (l.gross - l.fee) * 1_000_000_000_000_000_000 / l.supply
}

#[test]
fn a_first_deposit_buys_at_a_dollar_less_the_transaction_fee() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 1_000_000_000);
    // 1000 dollars less 0.1%, at 1e12 share units a micro-dollar.
    approx_rel(shares, 999_000_000 * OFFSET, 10_000_000_000_000);
    let vault = env.vault(&v);
    assert_eq!(vault.cash, 1_000_000_000 + DEAD_ASSETS);
    assert_eq!(vault.escrow, 0);
}

#[test]
fn a_deposit_is_priced_at_a_later_valuation_never_the_current_one() {
    let (mut env, v) = Env::with_vault();
    // Exactly on a boundary: a request made now waits for the next slot.
    env.to_boundary();
    env.skip(SLOT);
    expect_ok(env.value_vault(&v));
    let alice = env.alice.insecure_clone();
    let id = env.deposit(&v, &alice, 1_000_000_000);
    env.settle_all(&v);
    assert_eq!(env.shares(&v, &alice.pubkey()), 0);
    assert!(!env.request(&v, id).unwrap().done);
    assert_eq!(env.vault(&v).escrow, 1_000_000_000);
    // A valuation recorded in the same slot again is refused; the next slot
    // prices it.
    expect_code(env.value_vault(&v), vault_error(VaultError::AlreadyValued));
    env.price(&v);
    assert!(env.request(&v, id).unwrap().done);
    assert!(env.shares(&v, &alice.pubkey()) > 0);
    assert_eq!(env.vault(&v).escrow, 0);
}

#[test]
fn a_valuation_counts_only_inside_its_window_and_once_a_slot() {
    let (mut env, v) = Env::with_vault();
    env.to_boundary();
    let t = env.now();
    env.warp(t + SLOT + 4);
    expect_code(env.value_vault(&v), vault_error(VaultError::OutsideWindow));
    env.warp(t + 2 * SLOT + 2);
    expect_ok(env.value_vault(&v));
    expect_code(env.value_vault(&v), vault_error(VaultError::AlreadyValued));
    env.warp(t + 2 * SLOT + 3);
    expect_code(env.value_vault(&v), vault_error(VaultError::AlreadyValued));
}

#[test]
fn a_new_valuation_waits_for_the_last_ones_requests() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    env.skip(SLOT);
    expect_code(env.value_vault(&v), vault_error(VaultError::Unsettled));
    // The station is told the same when it asks.
    assert!(env.vault(&v).owed());
    // Anyone can settle them, and then it can.
    env.settle_all(&v);
    assert!(!env.vault(&v).owed());
    expect_ok(env.value_vault(&v));
}

#[test]
fn a_deposit_needs_the_depositors_statement_and_the_minimum() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.give(&alice.pubkey(), 2_000_000_000);
    let hash = env.vault(&v).restricted_hash;
    // No statement recorded and none made.
    let ix = env.ix_request_deposit(&v, &alice.pubkey(), &alice.pubkey(), 1_000_000_000, None);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NoStatement),
    );
    // A statement for another list.
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &alice.pubkey(),
        1_000_000_000,
        Some([7; 32]),
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NoStatement),
    );
    // Nobody states for someone else: bob paying, alice receiving.
    env.give(&bob.pubkey(), 1_000_000_000);
    let ix = env.ix_request_deposit(
        &v,
        &bob.pubkey(),
        &alice.pubkey(),
        1_000_000_000,
        Some(hash),
    );
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::NotAllowed),
    );
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &alice.pubkey(),
        MINIMUM_DEPOSIT - 1,
        Some(hash),
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::TooSmall),
    );
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &alice.pubkey(),
        1_000_000_000,
        Some(hash),
    );
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.holder(&v, &alice.pubkey()).unwrap().statement, hash);
    // Once recorded, the next deposit needs none, and anyone may pay for it.
    let ix = env.ix_request_deposit(&v, &alice.pubkey(), &alice.pubkey(), MINIMUM_DEPOSIT, None);
    expect_ok(env.send(vec![ix], &[&alice]));
    let ix = env.ix_request_deposit(&v, &bob.pubkey(), &alice.pubkey(), MINIMUM_DEPOSIT, None);
    expect_ok(env.send(vec![ix], &[&bob]));
}

#[test]
fn a_deposit_made_before_a_retirement_is_sent_back() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    let ix = env.ix_author(&v, shonin_vault::instruction::Retire {}.data());
    expect_ok(env.as_author(ix));
    env.price(&v);
    assert_eq!(env.shares(&v, &alice.pubkey()), 0);
    assert_eq!(env.usd_balance(&alice.pubkey()), 1_000_000_000);
    assert_eq!(env.vault(&v).escrow, 0);
}

#[test]
fn a_deposit_sent_back_goes_to_whoever_paid_it() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    // Alice states; bob pays for a deposit she receives.
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_record_statement(&v, &alice.pubkey(), hash);
    expect_ok(env.send(vec![ix], &[&alice]));
    env.give(&bob.pubkey(), 1_000_000_000);
    let ix = env.ix_request_deposit(&v, &bob.pubkey(), &alice.pubkey(), 1_000_000_000, None);
    expect_ok(env.send(vec![ix], &[&bob]));
    let ix = env.ix_author(&v, shonin_vault::instruction::Retire {}.data());
    expect_ok(env.as_author(ix));
    env.price(&v);
    assert_eq!(
        env.usd_balance(&bob.pubkey()),
        1_000_000_000,
        "the money went back to its payer"
    );
    assert_eq!(env.usd_balance(&alice.pubkey()), 0);
}

#[test]
fn a_donation_to_the_vault_changes_nothing() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    env.price(&v);
    let before = env.share_price(&v);
    // A stranger's dollars sent to the vault's own account, and to its
    // station's.
    env.give(&v.vault, 1_000_000_000_000);
    env.give(&v.home.station, 1_000_000_000_000);
    env.price(&v);
    approx(env.share_price(&v), before, before / 10_000_000);
    assert_eq!(env.vault(&v).cash, 1_000_000_000 + DEAD_ASSETS);
}

/// A pool above its mark with BTC up 20% since payday, alice and bob in.
fn above_the_mark(env: &mut Env, v: &V) {
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.deposit_as(v, &alice, 100_000_000_000);
    env.deposit_as(v, &bob, 100_000_000_000);
    env.deploy_spot(v, 180_000_000_000);
    let monday = {
        let day = env.vault(v).last_payment / DAY;
        let since = day + 3;
        (since - since % 7 + 4) * DAY
    };
    env.warp(monday + 1);
    env.price(v);
    env.skip(DAY);
    env.set_spot(PRICE * 120 / 100, 0);
    env.price(v);
    assert!(env.vault(v).latest.fee > 0, "a fee is owed above the mark");
}

#[test]
fn two_equal_deposits_in_one_slot_buy_equal_shares_whoever_is_ahead() {
    for extra in [false, true] {
        let (mut env, v) = Env::with_vault();
        above_the_mark(&mut env, &v);
        if extra {
            // The bid a hair above the mark: the valuation carries the exit
            // side's premium, which a deposit pays.
            let p = PRICE * 120 / 100;
            env.set_prices(spot(), p, p, p + 1, p + 2);
        }
        let carol = Keypair::new();
        env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
        let alice = env.alice.insecure_clone();
        let before = env.shares(&v, &alice.pubkey());
        env.deposit(&v, &alice, 100_000_000_000);
        env.deposit(&v, &carol, 100_000_000_000);
        env.price(&v);
        let alice_got = env.shares(&v, &alice.pubkey()) - before;
        approx(env.shares(&v, &carol.pubkey()), alice_got, OFFSET);
    }
}

#[test]
fn an_exit_asked_after_a_mark_raising_deposit_costs_the_stayers_nothing() {
    for exit_first in [false, true] {
        let (mut env, v) = Env::with_vault();
        let alice = env.alice.insecure_clone();
        let bob = env.bob.insecure_clone();
        let shares = env.deposit_as(&v, &alice, 100_000_000_000);
        env.deploy_spot(&v, 90_000_000_000);
        env.set_spot(PRICE * 140 / 100, 0);
        env.price(&v);
        assert!(env.vault(&v).latest.fee > 0, "no profit fee owed");
        let mark_before = env.vault(&v).mark;
        if exit_first {
            env.redeem(&v, &alice, shares);
            env.deposit(&v, &bob, 300_000_000_000);
        } else {
            env.deposit(&v, &bob, 300_000_000_000);
            env.redeem(&v, &alice, shares);
        }
        env.price(&v);
        assert!(
            env.vault(&v).mark > mark_before,
            "the deposit didn't raise the mark"
        );
        assert_eq!(env.shares(&v, &alice.pubkey()), 0, "alice wasn't paid");
        let snapshot = snapshot_net_price(&env, &v);
        let live = live_net_price(&mut env, &v);
        assert!(
            live >= snapshot,
            "the stayers paid for alice's exit: {live} < {snapshot}"
        );
    }
}

#[test]
fn a_one_unit_redemption_pays_nothing_and_books_no_claim() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    let before = env.usd_balance(&alice.pubkey());
    env.redeem(&v, &alice, 1);
    env.price(&v);
    assert_eq!(env.usd_balance(&alice.pubkey()), before);
    assert_eq!(env.vault(&v).claim_count, 0);
}

#[test]
fn priced_withdrawals_are_paid_first_and_nothing_leaves_meanwhile() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    env.redeem(&v, &alice, shares);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    // Between the valuation's pricing and its settlement the cash it priced
    // withdrawals to be paid from stays home.
    expect_code(
        env.to_station(&v, 100_000_000),
        vault_error(VaultError::WrongState),
    );
    env.settle_all(&v);
    assert!(env.usd_balance(&alice.pubkey()) > 99_000_000_000);
    expect_ok(env.to_station(&v, 100_000_000));
}

#[test]
fn a_settlement_starts_where_the_queue_stands_and_names_each_request() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &bob, 1_000_000_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let payer = env.payer.insecure_clone();
    // From anywhere but the cursor (or the exits' cursor), refused.
    let ix = env.ix_settle(&v, 1, 4);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::WrongCursor),
    );
    // Bob's request standing in for alice's, refused.
    let mut ix = env.ix_settle(&v, 0, 2);
    let (first, second) = (env.request_key(&v, 1), env.request_key(&v, 2));
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == first {
            meta.pubkey = second;
        } else if meta.pubkey == second {
            meta.pubkey = first;
        }
    }
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::WrongAccount),
    );
    // A deposit's holder that isn't its owner's, refused.
    let mut ix = env.ix_settle(&v, 0, 2);
    let alice_holder = holder_address(&v.vault, &alice.pubkey());
    let bob_holder = holder_address(&v.vault, &bob.pubkey());
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == alice_holder {
            meta.pubkey = bob_holder;
        } else if meta.pubkey == bob_holder {
            meta.pubkey = alice_holder;
        }
    }
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::WrongAccount),
    );
    env.settle_all(&v);
    assert!(env.shares(&v, &alice.pubkey()) > 0 && env.shares(&v, &bob.pubkey()) > 0);
    // Settled once: settling again changes nothing.
    let before = env.vault(&v);
    let ix = env.ix_settle(&v, before.request_cursor, 4);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(env.vault(&v).total_supply, before.total_supply);
}

#[test]
fn a_valuation_at_a_market_outside_the_band_on_either_side_prices_nothing() {
    for side in ["bid below", "ask above", "mark off the venue's price"] {
        let (mut env, v) = Env::with_vault();
        let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
        let a = env.deposit_as(&v, &alice, 100_000_000_000);
        env.deploy_spot(&v, 50_000_000_000);
        match side {
            "bid below" => env.set_prices(spot(), PRICE, PRICE, PRICE * 97 / 100, PRICE),
            "ask above" => env.set_prices(spot(), PRICE, PRICE, PRICE, PRICE * 103 / 100),
            _ => env.set_prices(
                spot(),
                PRICE * 103 / 100,
                PRICE,
                PRICE * 103 / 100,
                PRICE * 103 / 100,
            ),
        }
        let id = env.redeem(&v, &alice, a / 2);
        let deposit = env.deposit(&v, &bob, 10_000_000_000);
        let count = env.vault(&v).valuation_count;
        let priced = env.vault(&v).latest;
        env.price(&v);
        let vault = env.vault(&v);
        assert_eq!(
            vault.valuation_count,
            count + 1,
            "{side}: no valuation was recorded"
        );
        assert_eq!(
            vault.latest, priced,
            "{side}: a void valuation priced something"
        );
        assert!(
            !env.request(&v, id).unwrap().done,
            "{side}: a withdrawal was paid"
        );
        assert!(
            !env.request(&v, deposit).unwrap().done,
            "{side}: a deposit was bought"
        );
        // Back inside the band, the requests roll on to the next.
        env.set_spot(PRICE, 0);
        env.price(&v);
        assert!(env.request(&v, id).is_none_or(|r| r.done), "{side}");
        assert!(env.request(&v, deposit).unwrap().done, "{side}");
    }
}

#[test]
fn a_bid_above_the_mark_never_pays_a_leaver_more_than_the_mark_value() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 150_000_000_000);
    // The bid half a percent above the mark, inside the band.
    env.set_prices(
        spot(),
        PRICE,
        PRICE,
        PRICE * 1005 / 1000,
        PRICE * 1006 / 1000,
    );
    env.price(&v);
    let latest = env.vault(&v).latest;
    assert_eq!(
        latest.gross_exit, latest.gross,
        "an exit was worth more than the mark"
    );
    assert!(
        latest.extra > 0,
        "a deposit didn't pay the exit side's premium"
    );
    let paid = env.redeem_as(&v, &alice, a);
    assert!((paid as u128) <= latest.gross * a / latest.supply);
}

#[test]
fn a_round_trip_never_returns_more_than_was_put_in() {
    for assets in [MINIMUM_DEPOSIT, 12_345_678, 1_000_000_007, 99_999_999_999] {
        let (mut env, v) = Env::with_vault();
        let alice = env.alice.insecure_clone();
        let bob = env.bob.insecure_clone();
        env.deposit_as(&v, &bob, 50_000_000_000);
        let shares = env.deposit_as(&v, &alice, assets);
        let paid = env.redeem_as(&v, &alice, shares);
        assert!(paid < assets, "{assets} in, {paid} out");
    }
}

#[test]
fn a_tiny_first_deposit_and_a_donation_cannot_round_the_next_depositor_down() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, MINIMUM_DEPOSIT);
    // A large donation to the vault's own account, and to its station's.
    env.give(&v.vault, 1_000_000_000_000);
    env.give(&v.home.station, 1_000_000_000_000);
    let shares = env.deposit_as(&v, &bob, 1_000_000_000);
    approx_rel(shares, 999_000_000 * OFFSET, 10_000_000_000_000_000);
}

/// Nobody can sign for the default address, so a deposit or a withdrawal
/// naming it as receiver is refused, as the EVM vault refuses `address(0)`:
/// the shares or the payout could never be collected. The same holder naming
/// a real receiver goes ahead.
#[test]
fn a_request_naming_the_default_address_as_receiver_is_refused() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000);

    env.give(&alice.pubkey(), MINIMUM_DEPOSIT);
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &Pubkey::default(),
        MINIMUM_DEPOSIT,
        None,
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::TooSmall),
    );

    let ix = env.ix_request_redeem(&v, &alice.pubkey(), shares, &Pubkey::default());
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::TooSmall),
    );
    assert_eq!(env.vault(&v).held_total, 0);

    let bob = env.bob.insecure_clone();
    let ix = env.ix_request_redeem(&v, &alice.pubkey(), shares, &bob.pubkey());
    expect_ok(env.send(vec![ix], &[&alice]));
}
