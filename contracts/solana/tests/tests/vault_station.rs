//! The vault and its station. The vault reaches the station only by calling
//! it, signing as itself; the station asks the vault, through `station_view`,
//! only when someone else stops or unwinds it. The vault books what came home
//! by the rise in the station's running total of what it has sent home,
//! whoever made it send, never by its own balance. The counterpart of the
//! vault halves of the EVM station tests: `NoStoppedUnwindBeforeSettlement`,
//! `StoppedStationClosesOnlyForTheVault`, `UnsettledCannotBeHeld`,
//! `StoppedUnwindBringsHomeWhatItCan`, `UnwindThatBringsNothingHomeStillEmpties`
//! and `DriverCannotReachTheVault` (a driver's or a venue's call can't name the
//! vault program), with the real vault where the station tests use the
//! stand-in.

use anchor_lang::{AnchorDeserialize, InstructionData, ToAccountMetas};
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_station::state::vault::VaultView;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn view(env: &mut Env, v: &V) -> VaultView {
    let ix = solana_instruction::Instruction {
        program_id: shonin_vault::ID,
        accounts: shonin_vault::accounts::StationView { vault: v.vault }.to_account_metas(None),
        data: shonin_vault::instruction::StationView {}.data(),
    };
    let meta = env.simulate(vec![ix]);
    VaultView::try_from_slice(&meta.return_data.data).unwrap()
}

#[test]
fn the_vault_tells_the_station_when_it_halted_its_grace_and_whether_it_owes_a_settlement() {
    let (mut env, v) = Env::with_vault();
    assert_eq!(
        view(&mut env, &v),
        VaultView {
            halted_at: 0,
            halt_grace: HALT_GRACE,
            unsettled: false
        }
    );
    let alice = env.alice.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(view(&mut env, &v).unsettled);
    env.settle_all(&v);
    assert!(!view(&mut env, &v).unsettled);
    expect_ok(env.kill(&v));
    let halted = env.vault(&v).halted_at;
    assert_eq!(view(&mut env, &v).halted_at, halted);
    // The station obeys it: no stop inside the grace, one after it.
    expect_code(env.stop(&v.home), station_error(StationError::NotHalted));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    assert!(env.station(&v.home).stopped);
}

#[test]
fn what_anyone_makes_the_station_send_home_is_booked_by_its_total_and_nothing_else_is() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 50_000_000_000));
    let cash = env.vault(&v).cash;
    // The executor unwinds a fraction of the running station directly: it
    // comes home outside any call of the vault's.
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&v.home, &executor.pubkey(), WAD / 2, true);
    expect_ok(env.send(vec![ix], &[&executor]));
    assert_eq!(env.vault(&v).cash, cash, "booked before the vault looked");
    // A stranger's gift to the vault's own account.
    env.give(&v.vault, 7_000_000_000);
    env.price(&v);
    let vault = env.vault(&v);
    assert_eq!(
        vault.cash,
        cash + 25_000_000_000,
        "the send home was not booked as cash"
    );
    assert_eq!(vault.station_booked, env.sent_home(&v.home));
    let held = env.usd_balance(&v.vault);
    assert!(held >= vault.cash + vault.tank.balance + 7_000_000_000);
}

#[test]
fn a_stopped_stations_unwind_by_anyone_waits_for_settlement_and_comes_home_as_cash() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 150_000_000_000);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.redeem(&v, &alice, a / 2);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_unwind_stopped(&v.home, &carol.pubkey(), &[0]);
    expect_code(
        env.send_fitting(vec![ix], &[&carol]),
        station_error(StationError::Unsettled),
    );
    env.settle_all(&v);
    let ix = env.ix_unwind_stopped(&v.home, &carol.pubkey(), &[0]);
    expect_ok(env.send_fitting(vec![ix], &[&carol]));
    let cash = env.vault(&v).cash;
    env.price(&v);
    assert!(env.vault(&v).cash > cash + 140_000_000_000);
    assert!(env.vault(&v).ready_to_close());
}

#[test]
fn a_stopped_station_closes_a_part_only_for_the_vault() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 190_000_000_000);
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    expect_ok(env.stop(&v.home));
    // A stranger can't close a part of a stopped station...
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_unwind(&v.home, &carol.pubkey(), WAD / 2, false);
    expect_code(
        env.send_fitting(vec![ix], &[&carol]),
        station_error(StationError::NotAllowed),
    );
    // ...the vault can, for its own claim.
    expect_ok(env.settle_claim(&v, id));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    approx(
        env.usd_balance(&alice.pubkey()) as u128,
        100_000_000_000 * 999 / 1000,
        400_000_000,
    );
}

#[test]
fn the_vault_signs_only_for_its_own_station() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::local()]);
    let w = env.open(&[Declared::local()]);
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.approve_executor(&v.home);
    // `w`'s station named for `v`'s.
    let mut ix = env.ix_move(&v, &env.executor.pubkey(), 1_000_000, true);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == v.home.station {
            meta.pubkey = w.home.station;
        } else if meta.pubkey == v.home.dollars {
            meta.pubkey = w.home.dollars;
        }
    }
    let executor = env.executor.insecure_clone();
    expect_failure(env.send(vec![ix], &[&executor]));
    // Its valuation reads its own station only.
    let mut ix = env.ix_record_valuation(&v);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == v.home.station {
            meta.pubkey = w.home.station;
        }
    }
    env.to_next_slot();
    let payer = env.payer.insecure_clone();
    expect_failure(env.send_fitting(vec![ix], &[&payer]));
}

#[test]
fn a_station_answers_the_vault_from_the_vaults_own_account() {
    // The station program is created with this program as the one whose
    // vaults may create stations: a vault of any other program is refused.
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::local()]);
    assert_eq!(env.station(&v.home).vault_program, shonin_vault::ID);
    assert_eq!(env.station(&v.home).vault_dollars, v.home.vault_dollars);
    let config: shonin_station::state::Config =
        env.load(&shonin_station::config_address()).unwrap();
    assert_eq!(config.vault_program, shonin_vault::ID);
    let _ = shonin_vault::instruction::StationView {}.data();
}
