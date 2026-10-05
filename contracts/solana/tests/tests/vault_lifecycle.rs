//! A vault's life: retire winds down, evict halts, the admin's kill and the
//! sealed wind-down halt; after the grace anyone stops the station and
//! unwinds it; once a valuation after the halt finds it empty anyone closes
//! the vault, the fees are paid one last time, the dead shares' part goes to
//! the author's tank, and every holder, claim and late request is paid
//! through the closed vault's doors. The counterpart of the lifecycle tests
//! of `Vault.t.sol`, `SealedWindDown`, `HaltedVaultMayCloseInsideItsGrace`,
//! `ClosedVaultPaysClaimsThroughItsDoor`, `ClosedVaultIsNeverUnsettled`,
//! `StragglerSettledAfterTheClose`, `CloseConservesEveryDollar`,
//! `DustCannotHoldOffTheClose`, `HaltedVaultClosesInOneTransaction`,
//! `LifecycleProperties`, `CloseCannotBeStalledByTheTank`, `IdleFloorHolds`
//! and the idle floor tests of `Vault.t.sol`.

use anchor_lang::InstructionData;
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn redeem_closed(env: &mut Env, v: &V, who: &Keypair) -> u64 {
    let shares = env.shares(v, &who.pubkey());
    let before = env.usd_balance(&who.pubkey());
    env.token_account(&who.pubkey());
    let ix = env.ix_redeem_closed(v, &who.pubkey(), shares, &who.pubkey());
    expect_ok(env.send(vec![ix], &[who]));
    env.usd_balance(&who.pubkey()) - before
}

#[test]
fn retire_stops_deposits_and_evict_halts_after_the_wind_down() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    // Only the author retires.
    let mut ix = env.ix_author(&v, shonin_vault::instruction::Retire {}.data());
    ix.accounts[0].pubkey = alice.pubkey();
    expect_failure(env.send(vec![ix], &[&alice]));
    let ix = env.ix_author(&v, shonin_vault::instruction::Retire {}.data());
    expect_ok(env.as_author(ix));
    assert_eq!(env.vault(&v).state, VaultState::WindingDown);
    env.give(&alice.pubkey(), 100_000_000);
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_request_deposit(
        &v,
        &alice.pubkey(),
        &alice.pubkey(),
        100_000_000,
        Some(hash),
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::WrongState),
    );
    let evict = env.ix_anyone(&v, shonin_vault::instruction::Evict {}.data());
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![evict.clone()], &[&payer]),
        vault_error(VaultError::NotYet),
    );
    env.skip(7 * DAY);
    expect_ok(env.send(vec![evict.clone()], &[&payer]));
    assert!(env.vault(&v).halted_at > 0);
    // Withdrawals still work through it all.
    env.skip(1);
    expect_code(
        env.send(vec![evict], &[&payer]),
        vault_error(VaultError::WrongState),
    );
}

#[test]
fn only_the_admin_kills_and_winds_a_sealed_vault_down() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let author = env.author.insecure_clone();
    for who in [&alice, &author] {
        for data in [
            shonin_vault::instruction::Kill {}.data(),
            shonin_vault::instruction::WindDownSealed {}.data(),
        ] {
            let ix = env.ix_admin(&v, &who.pubkey(), data);
            expect_failure(env.send(vec![ix], &[who]));
        }
    }
    let admin = env.admin.insecure_clone();
    let ix = env.ix_admin(
        &v,
        &admin.pubkey(),
        shonin_vault::instruction::WindDownSealed {}.data(),
    );
    expect_ok(env.send(vec![ix], &[&admin]));
    let vault = env.vault(&v);
    assert_eq!(vault.state, VaultState::Killed);
    assert_eq!(vault.halt_reason, shonin_vault::state::HaltReason::Sealed);
    // Once halted, a kill changes nothing it can't.
    expect_code(env.kill(&v), vault_error(VaultError::WrongState));
}

#[test]
fn a_killed_vault_unwinds_closes_and_pays_everyone() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 50_000_000_000);
    env.trade(
        &v,
        120_000_000_000,
        vec![
            order(spot(), true, UNIT, PRICE),
            order(perp(), false, UNIT, 0),
        ],
    );
    expect_ok(env.kill(&v));
    // The station can't be stopped inside the grace: it asks the vault.
    expect_code(env.stop(&v.home), station_error(StationError::NotHalted));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    let payer = env.payer.insecure_clone();
    let unwind = env.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
    expect_ok(env.send_fitting(vec![unwind], &[&payer]));
    // Not before a valuation after the halt has found the station empty.
    expect_code(env.close_vault(&v), vault_error(VaultError::NotYet));
    env.price(&v);
    // The station's sends were booked as cash.
    approx(env.vault(&v).cash as u128, 150_000_000_000, 100_000_000);
    expect_ok(env.close_vault(&v));
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    let to_alice = redeem_closed(&mut env, &v, &alice);
    let to_bob = redeem_closed(&mut env, &v, &bob);
    approx(to_alice as u128, 100_000_000_000, 200_000_000);
    approx(to_bob as u128, 50_000_000_000, 100_000_000);
    // Closed for good: no valuation, no close again.
    env.to_next_slot();
    expect_code(env.value_vault(&v), vault_error(VaultError::WrongState));
    expect_code(env.close_vault(&v), vault_error(VaultError::WrongState));
}

#[test]
fn a_closed_vault_leaves_nothing_stranded_with_the_dead_shares() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.price(&v);
    let vault = env.vault(&v);
    let tank_before = vault.tank.balance;
    assert!(vault.dead_shares > 0);
    let dead_part = vault.cash as u128 * vault.dead_shares / vault.total_supply;
    expect_ok(env.close_vault(&v));
    let vault = env.vault(&v);
    assert_eq!(vault.dead_shares, 0, "the dead shares outlived the close");
    approx(
        vault.tank.balance as u128,
        tank_before as u128 + dead_part,
        10,
    );
    redeem_closed(&mut env, &v, &alice);
    for who in [env.platform.insecure_clone(), env.author.insecure_clone()] {
        if env.shares(&v, &who.pubkey()) != 0 {
            redeem_closed(&mut env, &v, &who);
        }
    }
    let vault = env.vault(&v);
    assert_eq!(vault.total_supply, 0);
    assert!(vault.cash <= 1, "money was stranded in the closed vault");
    // The author takes the tank at once.
    let ix = env.ix_withdraw_tank_unsigned(&v, u64::MAX);
    expect_ok(env.as_author(ix));
    assert_eq!(env.vault(&v).tank.balance, 0);
}

#[test]
fn a_halted_vault_closes_and_its_tank_can_be_taken_inside_the_grace() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    let tank = env.vault(&v).tank.balance;
    expect_ok(env.kill(&v));
    let halted = env.vault(&v).halted_at;
    env.skip(60);
    env.price(&v);
    assert!(env.now() < halted + HALT_GRACE, "still inside the grace");
    expect_ok(env.close_vault(&v));
    let author = env.author.pubkey();
    env.token_account(&author);
    let before = env.usd_balance(&author);
    let ix = env.ix_withdraw_tank_unsigned(&v, u64::MAX);
    expect_ok(env.as_author(ix));
    assert!(env.usd_balance(&author) - before >= tank);
}

#[test]
fn a_closed_vault_pays_a_claim_through_its_own_door_and_refuses_its_fallback() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 190_000_000_000);
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    expect_ok(env.kill(&v));
    env.wind_up(&v);
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    let supply = env.vault(&v).total_supply;
    env.skip(30 * DAY);
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::WrongState),
    );
    expect_code(
        env.settle_claim(&v, id),
        vault_error(VaultError::WrongState),
    );
    let ix = env.ix_claim_closed(&v, id);
    let payer = env.payer.insecure_clone();
    expect_ok(env.send(vec![ix], &[&payer]));
    approx(
        env.usd_balance(&alice.pubkey()) as u128,
        100_000_000_000,
        300_000_000,
    );
    assert_eq!(
        env.vault(&v).total_supply + a,
        supply,
        "shares were minted after the close"
    );
    // Paid once.
    let ix = env.ix_claim_closed(&v, id);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NotAllowed),
    );
}

#[test]
fn requests_made_after_the_last_valuation_are_settled_at_the_closed_vaults_price() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetTankKeep {
            assets: 100_000_000_000,
        }
        .data(),
    );
    expect_ok(env.as_author(ix));
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    // A valuation after the halt, then requests that came too late for it.
    env.to_boundary();
    env.skip(SLOT - 4);
    let straggler = env.redeem(&v, &alice, a / 2);
    let deposit = {
        // Deposits stopped at the kill; a fill was asked for.
        expect_ok(env.keep_unsigned(&v));
        env.vault(&v).request_count
    };
    env.to_boundary();
    expect_ok(env.value_vault(&v));
    env.settle_all(&v);
    assert!(
        env.request(&v, straggler).is_some_and(|r| !r.done),
        "not a straggler"
    );
    expect_ok(env.close_vault(&v));
    // A closed vault never reads as unsettled.
    assert!(!env.vault(&v).owed());
    let payer = env.payer.insecure_clone();
    let ix = env.ix_settle_closed(&v, straggler);
    expect_ok(env.send(vec![ix], &[&payer]));
    approx(
        env.usd_balance(&alice.pubkey()) as u128,
        50_000_000_000,
        100_000_000,
    );
    let ix = env.ix_settle_closed(&v, deposit);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert!(!env.vault(&v).tank.fill_pending, "the fill didn't lapse");
    // Each once.
    let ix = env.ix_settle_closed(&v, straggler);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NotAllowed),
    );
}

#[test]
fn a_halted_vault_takes_no_deposit_and_closes_with_none_in_escrow() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    expect_ok(env.kill(&v));
    // Requests are refused once killed; this one was made before.
    let hash = env.vault(&v).restricted_hash;
    let ix = env.ix_record_statement(&v, &bob.pubkey(), hash);
    expect_ok(env.send(vec![ix], &[&bob]));
    let ix = env.ix_request_deposit(&v, &bob.pubkey(), &bob.pubkey(), 10_000_000, None);
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::WrongState),
    );
    env.wind_up(&v);
    assert_eq!(env.vault(&v).escrow, 0);
}

#[test]
fn a_strangers_micro_dollar_at_the_station_cannot_hold_off_the_close() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 50_000_000_000);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    let payer = env.payer.insecure_clone();
    let unwind = env.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
    expect_ok(env.send_fitting(vec![unwind], &[&payer]));
    // A stranger's dollar-unit at the station's account counts for nothing,
    // and a tenth of the minimum deposit at the venue counts as empty.
    env.give(&v.home.dollars, 1);
    env.price(&v);
    expect_ok(env.close_vault(&v));
}

#[test]
fn the_keeper_stops_unwinds_values_and_closes_in_one_transaction() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 60_000_000_000);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    env.to_next_slot();
    let payer = env.payer.insecure_clone();
    let ixs = vec![
        env.ix_stop(&v.home),
        env.ix_unwind(&v.home, &payer.pubkey(), WAD, false),
        env.ix_record_valuation(&v),
        env.ix_close_vault(&v),
    ];
    expect_ok(env.send_fitting(ixs, &[&payer]));
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    // Every dollar is accounted for: cash, the tank, and nothing else.
    let vault = env.vault(&v);
    assert!(env.usd_balance(&v.vault) >= vault.cash + vault.tank.balance + vault.escrow);
}

#[test]
fn a_close_conserves_every_dollar() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 70_000_000_000);
    env.deposit_as(&v, &bob, 30_000_000_000);
    env.venue_reserves(10_000_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    env.gain(&v, 8_000_000_000);
    env.price(&v);
    expect_ok(env.kill(&v));
    env.wind_up(&v);
    let vault = env.vault(&v);
    let balance = env.usd_balance(&v.vault);
    assert!(
        balance
            >= vault.cash
                + vault.tank.balance
                + vault.escrow
                + vault.unit_reserve
                + vault.held_total,
        "the vault owes more than it holds"
    );
    let mut paid = 0;
    for who in [
        alice,
        bob,
        env.author.insecure_clone(),
        env.platform.insecure_clone(),
    ] {
        if env.shares(&v, &who.pubkey()) != 0 {
            paid += redeem_closed(&mut env, &v, &who);
        }
    }
    let vault = env.vault(&v);
    assert_eq!(vault.total_supply, 0);
    assert!(
        vault.cash <= 4,
        "{} left in a closed vault nobody owns",
        vault.cash
    );
    assert!(paid > 100_000_000_000);
}

#[test]
fn the_station_cant_take_cash_below_the_idle_floor() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.approve_executor(&v.home);
    // A 5% floor of about 100k.
    expect_code(
        env.to_station(&v, 96_000_000_000),
        vault_error(VaultError::BelowFloor),
    );
    expect_ok(env.to_station(&v, 94_000_000_000));
    // A run of sends can't shrink the value the floor is measured on.
    expect_code(
        env.to_station(&v, 2_000_000_000),
        vault_error(VaultError::BelowFloor),
    );
}

#[test]
fn lowering_the_floor_waits_a_day_and_raising_it_doesnt() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.approve_executor(&v.home);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 0 }.data(),
    );
    expect_ok(env.as_author(ix));
    expect_code(
        env.to_station(&v, 96_000_000_000),
        vault_error(VaultError::BelowFloor),
    );
    env.skip(DAY);
    env.price(&v);
    expect_ok(env.to_station(&v, 96_000_000_000));
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 9_000 }.data(),
    );
    expect_ok(env.as_author(ix));
    assert_eq!(env.vault(&v).floor_bps, 9_000);
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 10_001 }.data(),
    );
    expect_code(env.as_author(ix), vault_error(VaultError::NotAllowed));
}

#[test]
fn only_the_key_the_station_approved_moves_money_and_only_while_the_vault_runs() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    // Blessed, but not approved at the station.
    expect_code(
        env.to_station(&v, 1_000_000),
        vault_error(VaultError::NotBlessed),
    );
    env.approve_executor(&v.home);
    // A stranger signing.
    let ix = env.ix_move(&v, &alice.pubkey(), 1_000_000, true);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NotBlessed),
    );
    expect_ok(env.to_station(&v, 1_000_000));
    expect_ok(env.from_station(&v, 1_000_000));
    // Once halted, nothing goes out.
    expect_ok(env.kill(&v));
    expect_code(
        env.to_station(&v, 1_000_000),
        vault_error(VaultError::WrongState),
    );
}

/// What the station sends home after the close is the pool's: every closed
/// door books it before paying, so two equal holders are paid the same
/// whichever leaves first and whether anyone books it in between.
#[test]
fn equal_holders_of_a_closed_vault_are_paid_the_same_whatever_arrives_after_the_close() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &bob, 1_000_000_000);
    env.price(&v);
    let a = env.shares(&v, &alice.pubkey());
    let b = env.shares(&v, &bob.pubkey());
    assert_eq!(a, b, "one slot, one price");

    // Fifty cents of counted idle at the station: below the dust a halted
    // vault may close with.
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 500_000));
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    env.settle_all(&v);
    expect_ok(env.close_vault(&v));
    assert_eq!(env.vault(&v).state, VaultState::Closed);

    // Anyone brings the station's idle dollars home after the close.
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    env.unwind_stopped(&v.home, &carol, &[])
        .expect("a stopped station's idle dollars come home");
    let sent = env.sent_home(&v.home);
    assert_eq!(sent, env.vault(&v).station_booked + 500_000);

    // Alice leaves first; her door books the arrival before paying her.
    let paid_alice = redeem_closed(&mut env, &v, &alice);
    assert_eq!(env.vault(&v).station_booked, sent);

    // A settle in between books nothing more...
    let payer = env.payer.insecure_clone();
    let from = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, from, 0);
    expect_ok(env.send(vec![ix], &[&payer]));

    // ...and Bob, holding what Alice held, is paid what she was.
    let paid_bob = redeem_closed(&mut env, &v, &bob);
    approx(paid_alice as u128, paid_bob as u128, 2);
    assert!(paid_alice >= 1_000_000_000 - 2_000_000 + 250_000 - 2);
}

/// The closed door pays only an address someone can sign for.
#[test]
fn a_closed_vault_refuses_to_pay_the_default_address() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 100_000_000);
    expect_ok(env.kill(&v));
    env.wind_up(&v);
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    let ix = env.ix_redeem_closed(&v, &alice.pubkey(), shares, &Pubkey::default());
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::TooSmall),
    );
    let paid = redeem_closed(&mut env, &v, &alice);
    approx(paid as u128, 100_000_000, 1_000_000);
}
