//! The tank: the author's dollars for running costs, never counted in value.
//! A usage total signed by the vault's executor (an Ed25519 signature in the
//! same transaction, over a digest bound to the cluster, the program, the
//! vault and the purpose) is charged up to the allowances' cost for the time
//! it covers, what a collection leaves of the cap kept for the next up to a
//! day's cost; the author withdraws after a fresh bill or, with no executor,
//! through an escape that waits a day and lapses a day later; auto-fill
//! redeems the author's shares like any withdrawal. The counterpart of the
//! tank tests of `Vault.t.sol`, `UsageReplayRefused`,
//! `UsageCapCarriesForADay`, `FillNeverExceedsCash`,
//! `CloseCannotBeStalledByTheTank` and the usage half of
//! `VaultProperties.t_a_usage_bill_and_a_statement_for_one_vault_are_refused_by_another`.

use anchor_lang::InstructionData;
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn platform_balance(env: &Env) -> u64 {
    env.usd_balance(&env.platform.pubkey())
}

/// The author's withdrawal of `assets`, against the executor's total.
fn withdraw(env: &mut Env, v: &V, assets: u64, total: u64, as_of: u64) -> TxResult {
    let executor = env.executor.insecure_clone();
    let digest = env.usage_digest(v, total, as_of);
    let usage = Usage {
        total,
        as_of,
        signer: executor.pubkey(),
    };
    let author = env.author.insecure_clone();
    env.token_account(&author.pubkey());
    let ixs = vec![
        Env::ed25519(&executor, &digest),
        env.ix_withdraw_tank(v, assets, usage),
    ];
    env.send(ixs, &[&author])
}

#[test]
fn keep_charges_the_bill_up_to_the_allowances_for_the_time_elapsed() {
    let (mut env, v) = Env::with_vault();
    env.skip(DAY);
    let as_of = env.now();
    let before = platform_balance(&env);
    // Asked for 50 dollars; the allowances cost 2 a day.
    expect_ok(env.keep_signed(&v, 50_000_000, as_of));
    approx((platform_balance(&env) - before) as u128, 2_000_000, 1);
    assert_eq!(env.vault(&v).tank.usage_settled, 50_000_000);
    // The rest is written off: no debt, and the same bill can't be collected
    // twice.
    expect_code(
        env.keep_signed(&v, 50_000_000, as_of),
        vault_error(VaultError::Stale),
    );
    approx((platform_balance(&env) - before) as u128, 2_000_000, 1);
}

#[test]
fn a_block_of_bills_cannot_charge_more_than_the_time_elapsed() {
    let (mut env, v) = Env::with_vault();
    let as_of = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 2_000_000, as_of));
    for i in 2..=10u64 {
        expect_code(
            env.keep_signed(&v, i * 2_000_000, as_of),
            vault_error(VaultError::Stale),
        );
    }
    assert_eq!(
        platform_balance(&env),
        before,
        "no time passed, so nothing was earned"
    );
}

#[test]
fn the_bill_stops_at_the_halt() {
    let (mut env, v) = Env::with_vault();
    expect_ok(env.kill(&v));
    env.skip(30 * DAY);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 100_000_000, as_of));
    // Ten minutes of grace at 2 dollars a day.
    assert!(platform_balance(&env) - before <= 2_000_000 * HALT_GRACE / DAY + 1);
}

#[test]
fn only_the_executor_bills_and_a_total_never_goes_backwards() {
    let (mut env, v) = Env::with_vault();
    // An unwinder blessed for this vault may not bill it.
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let builder = env.builder.pubkey();
    env.bless_service_key(builder_image(), builder);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let unwinder = Keypair::new();
    let journal = env.journal_for(
        unwinder_image(),
        unwinder.pubkey(),
        env.leaf_for(v.vault.to_bytes(), v.bundle, unwinder.pubkey()),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, v.vault.to_bytes(), v.bundle, vec![]));
    let as_of = env.now();
    expect_code(
        env.keep_signed_by(&v, &unwinder, 2_000_000, as_of),
        vault_error(VaultError::NotBlessed),
    );
    // A stranger's key, blessed for nothing.
    let stranger = Keypair::new();
    expect_code(
        env.keep_signed_by(&v, &stranger, 2_000_000, as_of),
        vault_error(VaultError::NotBlessed),
    );
    expect_ok(env.keep_signed(&v, 2_000_000, as_of));
    env.skip(1);
    let later = env.now();
    expect_code(
        env.keep_signed(&v, 1_000_000, later),
        vault_error(VaultError::Stale),
    );
    // Nor signed in the future.
    expect_code(
        env.keep_signed(&v, 3_000_000, later + 10),
        vault_error(VaultError::Stale),
    );
}

#[test]
fn a_total_signed_for_one_vault_or_purpose_is_refused_by_another() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::local()]);
    let w = env.open(&[Declared::local()]);
    env.skip(DAY);
    let as_of = env.now();
    let executor = env.executor.insecure_clone();
    // The executor's total for `w`, carried to `v`.
    let digest = env.usage_digest(&w, 2_000_000, as_of);
    let usage = Usage {
        total: 2_000_000,
        as_of,
        signer: executor.pubkey(),
    };
    let payer = env.payer.insecure_clone();
    let ixs = vec![
        Env::ed25519(&executor, &digest),
        env.ix_keep(&v, &payer.pubkey(), Some(usage)),
    ];
    expect_code(
        env.send(ixs, &[&payer]),
        vault_error(VaultError::NoSignature),
    );
    // No signature in the transaction at all.
    let ix = env.ix_keep(&v, &payer.pubkey(), Some(usage));
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NoSignature),
    );
    // A signature over the right digest by another key than the one named.
    let stranger = Keypair::new();
    let digest = env.usage_digest(&v, 2_000_000, as_of);
    let ixs = vec![
        Env::ed25519(&stranger, &digest),
        env.ix_keep(&v, &payer.pubkey(), Some(usage)),
    ];
    expect_code(
        env.send(ixs, &[&payer]),
        vault_error(VaultError::NoSignature),
    );
    // The instructions sysvar stood in for by another account.
    let digest = env.usage_digest(&v, 2_000_000, as_of);
    let mut keep = env.ix_keep(&v, &payer.pubkey(), Some(usage));
    for meta in keep.accounts.iter_mut() {
        if meta.pubkey == anchor_lang::solana_program::sysvar::instructions::ID {
            meta.pubkey = anchor_lang::solana_program::sysvar::clock::ID;
        }
    }
    let ixs = vec![Env::ed25519(&executor, &digest), keep];
    expect_code(
        env.send(ixs, &[&payer]),
        vault_error(VaultError::WrongAccount),
    );
    expect_ok(env.keep_signed(&v, 2_000_000, as_of));
}

#[test]
fn replaying_the_last_bill_does_not_erase_the_carried_cap() {
    let (mut env, v) = Env::with_vault();
    env.skip(DAY);
    let as_of1 = env.now();
    expect_ok(env.keep_signed(&v, 2_000_000, as_of1));
    let paid = platform_balance(&env);
    env.skip(10 * DAY);
    let ix = env.ix_set_allowances(&v, allowances_costing(1_000_000));
    expect_ok(env.as_author(ix));
    approx(env.vault(&v).tank.cap_carried, 20_000_000, 2);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    expect_code(
        env.keep_signed(&v, 2_000_000, as_of1),
        vault_error(VaultError::Stale),
    );
    assert_eq!(platform_balance(&env), paid);
    env.skip(DAY);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of2 = env.now();
    expect_ok(env.keep_signed(&v, 100_000_000, as_of2));
    // Owed: ten days at $2 plus one at $1.
    approx((platform_balance(&env) - paid) as u128, 21_000_000, 3);
}

#[test]
fn unused_cap_is_carried_so_the_moment_of_collection_writes_off_nothing() {
    let (mut env, v) = Env::with_vault();
    // Ten days pass; a small bill is collected, using little of the cap
    // earned. What is left is kept for the next collection, up to a day's
    // cost at the allowances ($2): allowance unused for longer isn't saved.
    env.skip(10 * DAY);
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of1 = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 1_000_000, as_of1));
    approx((platform_balance(&env) - before) as u128, 1_000_000, 1);
    approx(env.vault(&v).tank.cap_carried, 2_000_000, 2);
    // A burst used inside the day before it was collected arrives with the
    // next day's bill: charged in full against the day carried and the day
    // since, whenever the collection happened to fall.
    env.skip(DAY);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of2 = env.now();
    expect_ok(env.keep_signed(&v, 5_000_000, as_of2));
    approx((platform_balance(&env) - before) as u128, 5_000_000, 3);
    // Collected early in a day or late, the same usage is charged the same.
    env.skip(DAY / 2);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of3 = env.now();
    expect_ok(env.keep_signed(&v, 5_500_000, as_of3));
    env.skip(DAY / 2);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let as_of4 = env.now();
    expect_ok(env.keep_signed(&v, 7_000_000, as_of4));
    approx((platform_balance(&env) - before) as u128, 7_000_000, 4);
}

/// Allowance left unused for days is kept only up to a day's cost, so a
/// total far above the allowances is charged a day's carried cap and the
/// time since the last collection, no more.
#[test]
fn days_of_unused_allowance_cannot_be_charged_in_one_collection() {
    let (mut env, v) = Env::with_vault();
    let executor = env.executor.pubkey();
    // Ten days collected daily with nothing used: $20 of allowance unused.
    for _ in 0..10 {
        env.skip(DAY);
        env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
        let at = env.now();
        expect_ok(env.keep_signed(&v, 0, at));
    }
    let carried = env.vault(&v).tank.cap_carried;
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), 500_000_000);
    let fund = env.ix_fund_tank(&v, &author.pubkey(), 500_000_000);
    expect_ok(env.send(vec![fund], &[&author]));

    env.skip(HOUR);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let late = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 500_000_000, late));
    let charged = (platform_balance(&env) - before) as u128;

    let day = 2_000_000u128;
    assert!(carried <= day + 1, "{carried} carried, more than a day");
    approx(charged, day + day / 24, 3);
}

/// A month of small daily collections leaves no more than a day's cap
/// behind: a bill a minute after the last is about a day's cost, not the
/// month's unused allowance.
#[test]
fn a_bill_after_weeks_of_small_collections_is_capped_at_about_a_day() {
    let (mut env, v) = Env::with_vault();
    let executor = env.executor.pubkey();
    for i in 1..=30u64 {
        env.skip(DAY);
        env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
        let at = env.now();
        expect_ok(env.keep_signed(&v, i * 10_000, at));
    }
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), 200_000_000);
    let fund = env.ix_fund_tank(&v, &author.pubkey(), 200_000_000);
    expect_ok(env.send(vec![fund], &[&author]));

    env.skip(60);
    env.bless_executor_key(executor, v.vault.to_bytes(), v.bundle);
    let at = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 200_000_000, at));
    let charged = (platform_balance(&env) - before) as u128;
    let day = 2_000_000u128;
    assert!(
        charged <= day + day / (24 * 60) + 3,
        "{charged} charged, more than a day and a minute"
    );
}

#[test]
fn the_author_withdraws_the_tank_after_a_fresh_bill() {
    let (mut env, v) = Env::with_vault();
    env.skip(DAY);
    let as_of = env.now();
    expect_ok(withdraw(&mut env, &v, 10_000_000, 1_000_000, as_of));
    assert_eq!(env.usd_balance(&env.author.pubkey()), 10_000_000);
    env.skip(HOUR);
    expect_code(
        withdraw(&mut env, &v, 10_000_000, 2_000_000, as_of + 1),
        vault_error(VaultError::Stale),
    );
    // Only the author.
    let alice = env.alice.insecure_clone();
    let executor = env.executor.pubkey();
    let mut ix = env.ix_withdraw_tank(
        &v,
        1,
        Usage {
            total: 1_000_000,
            as_of,
            signer: executor,
        },
    );
    ix.accounts[0].pubkey = alice.pubkey();
    expect_failure(env.send(vec![ix], &[&alice]));
}

#[test]
fn with_no_executor_the_author_withdraws_after_a_day_and_the_request_lapses() {
    let (mut env, v) = Env::with_vault();
    let author = env.author.pubkey();
    env.token_account(&author);
    let ix = env.ix_withdraw_tank_unsigned(&v, 10_000_000);
    expect_code(env.as_author(ix.clone()), vault_error(VaultError::NotYet));
    let ask = env.ix_author(
        &v,
        shonin_vault::instruction::RequestTankWithdrawal {}.data(),
    );
    expect_ok(env.as_author(ask.clone()));
    env.skip(DAY - 1);
    expect_code(env.as_author(ix.clone()), vault_error(VaultError::NotYet));
    env.skip(1);
    expect_ok(env.as_author(ix.clone()));
    assert_eq!(env.usd_balance(&author), 10_000_000);
    // A request lapses a day after it is ready.
    expect_ok(env.as_author(ask.clone()));
    env.skip(2 * DAY + 1);
    expect_code(
        env.as_author(ix.clone()),
        vault_error(VaultError::NotAllowed),
    );
    expect_ok(env.as_author(ask));
    expect_code(env.as_author(ix.clone()), vault_error(VaultError::NotYet));
    env.skip(DAY);
    expect_ok(env.as_author(ix));
    assert_eq!(env.usd_balance(&author), 20_000_000);
}

#[test]
fn auto_fill_redeems_the_authors_shares_like_any_withdrawal() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    env.skip(3 * DAY);
    env.price(&v);
    env.gain(&v, 20_000_000_000);
    env.skip(8 * DAY);
    env.price(&v);
    let author = env.author.pubkey();
    let shares = env.shares(&v, &author);
    assert!(shares > 0);
    let keep_at = env.vault(&v).tank.balance + 100_000_000;
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetTankKeep { assets: keep_at }.data(),
    );
    expect_ok(env.as_author(ix));
    expect_ok(env.keep_unsigned(&v));
    assert!(env.vault(&v).tank.fill_pending);
    // A second keep queues no second fill.
    let count = env.vault(&v).request_count;
    expect_ok(env.keep_unsigned(&v));
    assert_eq!(env.vault(&v).request_count, count);
    let price_before = env.share_price(&v);
    env.price(&v);
    approx(
        env.vault(&v).tank.balance as u128,
        keep_at as u128,
        1_000_000,
    );
    assert!(env.shares(&v, &author) < shares);
    // Priced at the exit side with the transaction fee: nobody else pays.
    assert!(env.share_price(&v) + 1 >= price_before);
}

#[test]
fn a_fill_never_becomes_a_claim_and_never_pays_more_than_the_cash() {
    let (mut env, v) = Env::with_vault();
    let author = env.author.insecure_clone();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &author, 100_000_000_000);
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 150_000_000_000));
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetTankKeep {
            assets: 100_000_000_000,
        }
        .data(),
    );
    expect_ok(env.as_author(ix));
    expect_ok(env.keep_unsigned(&v));
    let cash = env.vault(&v).cash;
    env.price(&v);
    let vault = env.vault(&v);
    assert_eq!(vault.claim_count, 0);
    assert!(vault.tank.balance <= MINIMUM_TANK + cash);
    assert!(vault.cash < cash, "nothing was filled");
}

#[test]
fn a_change_of_allowances_closes_the_cap_at_the_old_rate() {
    let (mut env, v) = Env::with_vault();
    env.skip(DAY);
    let ix = env.ix_set_allowances(&v, allowances_costing(10_000_000));
    expect_ok(env.as_author(ix));
    env.skip(DAY);
    let as_of = env.now();
    let before = platform_balance(&env);
    expect_ok(env.keep_signed(&v, 100_000_000, as_of));
    approx((platform_balance(&env) - before) as u128, 12_000_000, 2);
}

#[test]
fn a_top_up_from_shares_is_queued_and_priced_like_a_withdrawal() {
    let (mut env, v) = Env::with_vault();
    let author = env.author.insecure_clone();
    let shares = env.deposit_as(&v, &author, 1_000_000_000);
    let tank = env.vault(&v).tank.balance;
    let ix = env.ix_top_up_from_shares(&v, shares / 2);
    expect_ok(env.as_author(ix));
    assert_eq!(
        env.vault(&v).tank.balance,
        tank,
        "filled before it was priced"
    );
    env.price(&v);
    approx_rel(
        (env.vault(&v).tank.balance - tank) as u128,
        500_000_000 * 999 / 1000,
        1_000_000_000_000_000,
    );
}

#[test]
fn anyone_funds_the_tank_and_the_tank_never_counts_in_value() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    env.price(&v);
    let price = env.share_price(&v);
    env.give(&alice.pubkey(), 50_000_000);
    let ix = env.ix_fund_tank(&v, &alice.pubkey(), 50_000_000);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(
        env.vault(&v).tank.balance,
        MINIMUM_TANK - DEAD_ASSETS + 50_000_000
    );
    env.price(&v);
    approx(env.share_price(&v), price, price / 10_000);
}
