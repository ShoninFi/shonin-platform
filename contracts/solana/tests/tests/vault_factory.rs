//! The factory: set up once by the key that deployed it, its terms within
//! every floor and ceiling on every change, and one vault per strategy,
//! created when its author activates a registered, reviewed strategy on the
//! terms they read. The counterpart of `ConfigBounds`, the activation tests of
//! `Vault.t.sol` (a replacement factory, another chain), `AdminFootguns` (the
//! factory's part), `Interfaces` (the vault's address is known before it
//! exists) and `test_a_vault_is_initialised_once_and_the_implementation_never`.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_vault::state::{Factory, Terms, VAULT_SEED};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn set_terms(env: &mut Env, admin: &Keypair, terms: Terms) -> TxResult {
    use anchor_lang::{InstructionData, ToAccountMetas};
    let ix = solana_instruction::Instruction {
        program_id: shonin_vault::ID,
        accounts: shonin_vault::accounts::SetTerms {
            admin: admin.pubkey(),
            factory: factory_address(),
        }
        .to_account_metas(None),
        data: shonin_vault::instruction::SetTerms { terms }.data(),
    };
    env.send(vec![ix], &[admin])
}

/// Every term moved just past its floor or ceiling.
fn out_of_range() -> Vec<(&'static str, Terms)> {
    let t = terms();
    let mut cases: Vec<(&'static str, Terms)> = Vec::new();
    let mut add = |what, f: &dyn Fn(&mut Terms)| {
        let mut c = t;
        f(&mut c);
        cases.push((what, c));
    };
    add("holding fee above 5%", &|c| c.fees.holding_bps = 501);
    add("profit fee above 50%", &|c| {
        c.fees.author_bps = 4_000;
        c.fees.platform_bps = 1_001
    });
    add("transaction fee above 1%", &|c| {
        c.fees.transaction_bps = 101
    });
    add("exit band above 10%", &|c| c.exit_band_bps = 1_001);
    add("no dead shares", &|c| c.dead_assets = 0);
    add("a minimum tank no larger than the dead shares", &|c| {
        c.minimum_tank = c.dead_assets
    });
    add("no minimum deposit", &|c| c.minimum_deposit = 0);
    add("a minimum deposit above $100", &|c| {
        c.minimum_deposit = 100_000_001
    });
    add("no slot", &|c| c.timers.slot_interval = 0);
    add("a window as long as the slot", &|c| {
        c.timers.slot_window = c.timers.slot_interval
    });
    add("a lead no longer than the window", &|c| {
        c.timers.slot_lead = c.timers.slot_window
    });
    add("a lead as long as the slot", &|c| {
        c.timers.slot_lead = c.timers.slot_interval
    });
    add("a fallback under a day", &|c| {
        c.timers.fallback_after = DAY - 1
    });
    add("a freeze under a day", &|c| {
        c.timers.freeze_after = DAY - 1;
        c.timers.fallback_after = DAY - 2
    });
    add("a freeze no longer than the fallback", &|c| {
        c.timers.freeze_after = c.timers.fallback_after
    });
    add("no usage freshness", &|c| c.timers.usage_freshness = 0);
    add("usage fresh for over a day", &|c| {
        c.timers.usage_freshness = DAY + 1
    });
    add("an escape under a day", &|c| {
        c.timers.tank_escape_delay = DAY - 1
    });
    add("a floor delay under a day", &|c| {
        c.timers.floor_delay = DAY - 1
    });
    add("a halt grace under ten minutes", &|c| {
        c.timers.halt_grace = 10 * MINUTE - 1
    });
    add("a wind-down under a week", &|c| {
        c.timers.wind_down = 7 * DAY - 1
    });
    add("a slot over a year", &|c| {
        c.timers.slot_interval = 365 * DAY + 1
    });
    add("a fallback over a year", &|c| {
        c.timers.fallback_after = 365 * DAY + 1
    });
    add("a freeze over a year", &|c| {
        c.timers.freeze_after = 365 * DAY + 1
    });
    add("an escape over a year", &|c| {
        c.timers.tank_escape_delay = 365 * DAY + 1
    });
    add("a floor delay over a year", &|c| {
        c.timers.floor_delay = 365 * DAY + 1
    });
    add("a grace over a year", &|c| {
        c.timers.halt_grace = 365 * DAY + 1
    });
    add("a wind-down over a year", &|c| {
        c.timers.wind_down = 365 * DAY + 1
    });
    cases
}

#[test]
fn every_term_has_its_floor_and_ceiling_at_setup_and_on_every_change() {
    let mut env = Env::new();
    env.set_up_stations_for(shonin_vault::ID);
    for (what, bad) in out_of_range() {
        let result = env.initialize_vaults(bad, Pubkey::default());
        assert_eq!(
            code_of(&result),
            Some(vault_error(VaultError::OutOfRange)),
            "{what}"
        );
    }
    expect_ok(env.initialize_vaults(terms(), Pubkey::default()));
    let admin = env.admin.insecure_clone();
    for (what, bad) in out_of_range() {
        let result = set_terms(&mut env, &admin, bad);
        assert_eq!(
            code_of(&result),
            Some(vault_error(VaultError::OutOfRange)),
            "{what}"
        );
    }
    // The harshest terms allowed still give a depositor shares.
    let mut harsh = terms();
    harsh.fees.holding_bps = 500;
    harsh.fees.author_bps = 2_500;
    harsh.fees.platform_bps = 2_500;
    harsh.fees.transaction_bps = 100;
    harsh.exit_band_bps = 1_000;
    expect_ok(set_terms(&mut env, &admin, harsh));
    let v = env.open(&[Declared::local()]);
    let alice = env.alice.insecure_clone();
    let shares = env.deposit_as(&v, &alice, MINIMUM_DEPOSIT);
    assert!(shares > 0);
    assert_eq!(env.vault(&v).fees, harsh.fees);
}

#[test]
fn the_factory_is_set_up_once_by_the_key_that_deployed_it() {
    let mut env = Env::new();
    env.set_up_stations_for(shonin_vault::ID);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_initialize_vaults(&stranger.pubkey(), terms(), Pubkey::default());
    expect_code(
        env.send(vec![ix], &[&stranger]),
        vault_error(VaultError::NotTheDeployer),
    );
    expect_ok(env.initialize_vaults(terms(), Pubkey::default()));
    expect_failure(env.initialize_vaults(terms(), Pubkey::default()));
    let factory: Factory = env.factory();
    assert_eq!(factory.mint, env.mint);
    assert_eq!(factory.cluster, env.cluster());
    assert_eq!(factory.station_program, shonin_station::ID);
    // Only the admin changes the terms.
    let author = env.author.insecure_clone();
    expect_failure(set_terms(&mut env, &author, terms()));
}

#[test]
fn an_activation_is_on_the_terms_the_author_read() {
    let mut env = Env::with_vaults();
    let (id, bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let drivers = [Declared::local()];
    let (_, statement) = env.restricted_for(id, &drivers);
    let read = env.terms_hash();
    let admin = env.admin.insecure_clone();
    let mut changed = terms();
    changed.fees.holding_bps = 200;
    expect_ok(set_terms(&mut env, &admin, changed));
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, read, &[]);
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::TermsChanged),
    );
    let now = env.terms_hash();
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, now, &[]);
    expect_ok(env.send_fitting(vec![ix], &[&author]));
    let v = env.v_of(id, bundle, &[shonin_local_driver::ID]);
    // Fixed at activation: a later change of terms never reaches it.
    expect_ok(set_terms(&mut env, &admin, terms()));
    assert_eq!(env.vault(&v).fees.holding_bps, 200);
}

#[test]
fn only_the_author_activates_a_registered_reviewed_strategy_once() {
    let mut env = Env::with_vaults();
    let drivers = [Declared::local()];
    let (id, bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let (_, statement) = env.restricted_for(id, &drivers);
    let terms = env.terms_hash();
    // Below the minimum tank.
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), 2 * MINIMUM_TANK);
    let ix = env.ix_activate(
        id,
        bundle,
        &drivers,
        MINIMUM_TANK - 1,
        statement,
        terms,
        &[],
    );
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::TankTooSmall),
    );
    // Someone else signing as the author.
    let alice = env.alice.insecure_clone();
    let mut ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == author.pubkey() {
            meta.pubkey = alice.pubkey();
        }
    }
    expect_failure(env.send_fitting(vec![ix], &[&alice]));
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    expect_ok(env.send_fitting(vec![ix], &[&author]));
    // The vault is where anyone could work out it would be.
    let (expected, _) =
        Pubkey::find_program_address(&[VAULT_SEED, &id.to_le_bytes()], &shonin_vault::ID);
    let v = env.v_of(id, bundle, &[shonin_local_driver::ID]);
    assert_eq!(v.vault, expected);
    assert_eq!(env.vault(&v).state, VaultState::Open);
    assert_eq!(env.station(&v.home).vault, v.vault);
    // A second activation of the same strategy is refused.
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    expect_failure(env.send_fitting(vec![ix], &[&author]));

    // Registered but not reviewed.
    let other = env.submit_as(&author);
    let other_bundle = keccak(&[b"bundle", &other.to_le_bytes()]);
    let mut registration = Registration::new(other, author.pubkey(), other_bundle);
    registration.lists.chains = vec![ChainNeed {
        chain: name("solana"),
        drivers: vec![local()],
    }];
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    expect_ok(env.register_with(&registration, &builder, &reviewer));
    let (_, statement) = env.restricted_for(other, &drivers);
    let ix = env.ix_activate(
        other,
        other_bundle,
        &drivers,
        MINIMUM_TANK,
        statement,
        terms,
        &[],
    );
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::NotReady),
    );
}

#[test]
fn a_strategy_trading_on_another_chain_cannot_be_activated() {
    let mut env = Env::with_vaults();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let bundle = keccak(&[b"bundle", &id.to_le_bytes()]);
    let mut registration = Registration::new(id, author.pubkey(), bundle);
    registration.lists.chains = vec![
        ChainNeed {
            chain: name("solana"),
            drivers: vec![local()],
        },
        ChainNeed {
            chain: name("base"),
            drivers: vec![local()],
        },
    ];
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    expect_ok(env.register_with(&registration, &builder, &reviewer));
    expect_ok(env.submit_review(&reviewer, id, review_for(bundle, 1)));
    let drivers = [Declared::local()];
    let (_, statement) = env.restricted_for(id, &drivers);
    let terms = env.terms_hash();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::OtherChainsNotYet),
    );
}

/// A deployment this one replaces, standing in: its factory settings and,
/// for strategies that have one, its vault, at the addresses that program's
/// seeds give.
fn earlier_deployment(env: &mut Env, program: Pubkey, vault_of: Option<u64>) -> Pubkey {
    use anchor_lang::{AccountSerialize, Discriminator};
    let config = Pubkey::find_program_address(&[shonin_vault::state::FACTORY_SEED], &program).0;
    let mut factory = env.factory();
    factory.previous = Pubkey::default();
    let mut data = Vec::new();
    factory.try_serialize(&mut data).unwrap();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            config,
            solana_account::Account {
                lamports,
                data,
                owner: program,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    if let Some(id) = vault_of {
        let vault = shonin_vault::state::vault_address_of(&program, id).0;
        let mut data = shonin_vault::state::Vault::DISCRIMINATOR.to_vec();
        data.extend_from_slice(&[0u8; 64]);
        let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
        env.svm
            .set_account(
                vault,
                solana_account::Account {
                    lamports,
                    data,
                    owner: program,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    }
    config
}

#[test]
fn a_replacement_factory_refuses_a_strategy_that_has_a_vault_from_one_it_replaced() {
    let mut env = Env::new();
    env.set_up_stations_for(shonin_vault::ID);
    let earlier = env.final_program();
    expect_ok(env.initialize_vaults(terms(), earlier));
    let drivers = [Declared::local()];
    let (id, bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let (other, other_bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let config = earlier_deployment(&mut env, earlier, Some(id));
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), 2 * MINIMUM_TANK);
    let (_, statement) = env.restricted_for(id, &drivers);
    let earlier_vault = shonin_vault::state::vault_address_of(&earlier, id).0;
    // The earlier deployment's records must be named.
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::WrongAccount),
    );
    let ix = env.ix_activate(
        id,
        bundle,
        &drivers,
        MINIMUM_TANK,
        statement,
        terms,
        &[(config, earlier_vault)],
    );
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::VaultExists),
    );
    // A strategy with no vault there is activated here.
    let (_, statement) = env.restricted_for(other, &drivers);
    let earlier_other = shonin_vault::state::vault_address_of(&earlier, other).0;
    let ix = env.ix_activate(
        other,
        other_bundle,
        &drivers,
        MINIMUM_TANK,
        statement,
        terms,
        &[(config, earlier_other)],
    );
    expect_ok(env.send_fitting(vec![ix], &[&author]));
}

#[test]
fn the_author_and_the_platform_hold_separately() {
    let mut env = Env::new();
    env.set_up_stations_for(shonin_vault::ID);
    // A factory whose platform is the author: refused at activation, since
    // the fee recipients' holdings must be two accounts.
    let deployer = env.deployer.insecure_clone();
    let mut ix = env.ix_initialize_vaults(&deployer.pubkey(), terms(), Pubkey::default());
    use anchor_lang::{AnchorDeserialize, InstructionData};
    let mut args = shonin_vault::instruction::Initialize::try_from_slice(&ix.data[8..]).unwrap();
    args.args.platform = env.author.pubkey();
    ix.data = args.data();
    expect_ok(env.send(vec![ix], &[&deployer]));
    let (id, bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let drivers = [Declared::local()];
    let (_, statement) = env.restricted_for(id, &drivers);
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let mut ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    let vault = shonin_vault::state::vault_address(id).0;
    let platforms = holder_address(&vault, &env.platform.pubkey());
    let authors = holder_address(&vault, &author.pubkey());
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == platforms {
            meta.pubkey = authors;
        }
    }
    expect_code(
        env.send_fitting(vec![ix], &[&author]),
        vault_error(VaultError::NotAllowed),
    );
}

#[test]
fn the_factory_counts_the_vaults_it_activates() {
    let mut env = Env::with_vaults();
    assert_eq!(env.factory().vault_count, 0);
    env.open(&[Declared::local()]);
    assert_eq!(env.factory().vault_count, 1);
    env.open(&[Declared::local()]);
    assert_eq!(env.factory().vault_count, 2);
}
