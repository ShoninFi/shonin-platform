//! Who may deposit: a vault refuses US and sanctioned persons, its record's
//! list and every pinned driver's; a depositor states they are not one by
//! signing the deposit (or `record_statement`) transaction, whose instruction
//! names the hash of the list, and the holder's account records it. The list
//! only grows: `refresh_restricted` reads the drivers the registry approves
//! now, a depositor states again, and a deposit still waiting under the
//! shorter list goes back. The counterpart of the statement tests of
//! `Vault.t.sol`, `OutsizedRestrictedListIsSkipped` (in part) and the
//! statement half of
//! `VaultProperties.t_a_usage_bill_and_a_statement_for_one_vault_are_refused_by_another`.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_vault::codes;
use solana_signer::Signer;

fn refresh(env: &mut Env, v: &V, drivers: &[(u8, [u8; 32])]) -> TxResult {
    use anchor_lang::{InstructionData, ToAccountMetas};
    let payer = env.payer.insecure_clone();
    let mut accounts = shonin_vault::accounts::RefreshRestricted {
        payer: payer.pubkey(),
        vault: v.vault,
        restricted: shonin_vault::state::restricted_address(&v.vault).0,
        system_program: solana_sdk_ids::system_program::ID,
    }
    .to_account_metas(None);
    for (_, name) in drivers {
        let slot = shonin_registry::read::approval_address(ApprovalKind::Driver, name);
        let implementation = env.trading_driver(*name);
        let revoked =
            shonin_registry::read::revoked_address(ApprovalKind::Driver, name, &implementation);
        accounts.push(AccountMeta::new_readonly(slot, false));
        accounts.push(AccountMeta::new_readonly(revoked, false));
        accounts.push(AccountMeta::new_readonly(implementation, false));
    }
    let ix = solana_instruction::Instruction {
        program_id: shonin_vault::ID,
        accounts,
        data: shonin_vault::instruction::RefreshRestricted {
            drivers: drivers.iter().map(|(i, _)| *i).collect(),
        }
        .data(),
    };
    env.send(vec![ix], &[&payer])
}

#[test]
fn a_vault_refuses_the_baseline_its_record_and_its_drivers_and_the_author_states_at_activation() {
    let mut env = Env::with_vaults();
    let names = [test_driver()];
    let (id, bundle) = env.ready_with(&names, vec![name("CA"), name("SANCTIONED"), name("US")]);
    let drivers = [Declared::test()];
    // The author's statement must name the list as the vault will have it.
    let (list, hash) = env.restricted_for(id, &drivers);
    assert_eq!(codes::text(&list), "CA,KP,SANCTIONED,US");
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let wrong = codes::hash(&codes::baseline());
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, wrong, terms, &[]);
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::NoStatement),
    );
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, hash, terms, &[]);
    expect_ok(env.send_fitting(vec![ix], &[&author]));
    let v = env.v_of(id, bundle, &[shonin_test_driver::ID]);
    assert_eq!(env.restricted_list(&v), list);
    assert_eq!(env.vault(&v).restricted_hash, hash);
    assert_eq!(env.holder(&v, &author.pubkey()).unwrap().statement, hash);
    // The platform holds fee shares without one.
    assert_eq!(
        env.holder(&v, &env.platform.pubkey()).unwrap().statement,
        [0; 32]
    );
}

#[test]
fn a_longer_list_needs_a_new_statement_and_a_pending_deposit_goes_back() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    env.deposit(&v, &alice, 1_000_000_000);
    let before = env.usd_balance(&alice.pubkey());
    // The stand-in driver's terms now exclude another country: a new
    // implementation of `local` is approved.
    expect_ok(env.propose(ApprovalKind::Driver, local(), shonin_test_driver::ID));
    env.skip(TIMELOCK);
    expect_ok(refresh(&mut env, &v, &[(0, local())]));
    assert_eq!(codes::text(&env.restricted_list(&v)), "KP,SANCTIONED,US");
    // Reading it again adds nothing.
    let hash = env.vault(&v).restricted_hash;
    expect_ok(refresh(&mut env, &v, &[(0, local())]));
    assert_eq!(env.vault(&v).restricted_hash, hash);
    env.price(&v);
    assert_eq!(
        env.usd_balance(&alice.pubkey()) - before,
        1_000_000_000,
        "the pending deposit went back"
    );
    env.give(&alice.pubkey(), 100_000_000);
    let ix = env.ix_request_deposit(&v, &alice.pubkey(), &alice.pubkey(), 100_000_000, None);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NoStatement),
    );
    env.deposit(&v, &alice, 100_000_000);
    assert_eq!(env.holder(&v, &alice.pubkey()).unwrap().statement, hash);
}

#[test]
fn a_refresh_names_the_drivers_it_reads_and_each_only_once() {
    let (mut env, v) = Env::with_vault();
    expect_code(
        refresh(&mut env, &v, &[(0, local()), (0, local())]),
        vault_error(VaultError::WrongAccount),
    );
    expect_code(
        refresh(&mut env, &v, &[(1, local())]),
        vault_error(VaultError::WrongAccount),
    );
    // A program that isn't the approved implementation standing in for it.
    use anchor_lang::{InstructionData, ToAccountMetas};
    let payer = env.payer.insecure_clone();
    let mut accounts = shonin_vault::accounts::RefreshRestricted {
        payer: payer.pubkey(),
        vault: v.vault,
        restricted: shonin_vault::state::restricted_address(&v.vault).0,
        system_program: solana_sdk_ids::system_program::ID,
    }
    .to_account_metas(None);
    let slot = shonin_registry::read::approval_address(ApprovalKind::Driver, &local());
    let revoked = shonin_registry::read::revoked_address(
        ApprovalKind::Driver,
        &local(),
        &shonin_local_driver::ID,
    );
    accounts.push(AccountMeta::new_readonly(slot, false));
    accounts.push(AccountMeta::new_readonly(revoked, false));
    accounts.push(AccountMeta::new_readonly(shonin_test_driver::ID, false));
    let ix = solana_instruction::Instruction {
        program_id: shonin_vault::ID,
        accounts,
        data: shonin_vault::instruction::RefreshRestricted { drivers: vec![0] }.data(),
    };
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::WrongAccount),
    );
    // Leaving every driver out changes nothing.
    let hash = env.vault(&v).restricted_hash;
    expect_ok(refresh(&mut env, &v, &[]));
    assert_eq!(env.vault(&v).restricted_hash, hash);
}

#[test]
fn a_statement_for_one_vault_is_not_a_statement_for_another() {
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::local()]);
    let w = env.open(&[Declared::test()]);
    let alice = env.alice.insecure_clone();
    let hash_v = env.vault(&v).restricted_hash;
    let ix = env.ix_record_statement(&v, &alice.pubkey(), hash_v);
    expect_ok(env.send(vec![ix], &[&alice]));
    // Her statement is recorded on her holding in `v`; `w` has its own list
    // and her holding there has none.
    env.give(&alice.pubkey(), 100_000_000);
    let ix = env.ix_request_deposit(&w, &alice.pubkey(), &alice.pubkey(), 100_000_000, None);
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NoStatement),
    );
    let ix = env.ix_request_deposit(
        &w,
        &alice.pubkey(),
        &alice.pubkey(),
        100_000_000,
        Some(hash_v),
    );
    expect_code(
        env.send(vec![ix], &[&alice]),
        vault_error(VaultError::NoStatement),
    );
    let hash_w = env.vault(&w).restricted_hash;
    assert_ne!(hash_v, hash_w);
    let ix = env.ix_request_deposit(
        &w,
        &alice.pubkey(),
        &alice.pubkey(),
        100_000_000,
        Some(hash_w),
    );
    expect_ok(env.send(vec![ix], &[&alice]));
}

#[test]
fn withdrawals_are_never_gated() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 1_000_000_000);
    expect_ok(env.propose(ApprovalKind::Driver, local(), shonin_test_driver::ID));
    env.skip(TIMELOCK);
    expect_ok(refresh(&mut env, &v, &[(0, local())]));
    // Her statement is for the shorter list; she leaves all the same.
    let paid = env.redeem_as(&v, &alice, shares);
    assert!(paid > 990_000_000);
}
