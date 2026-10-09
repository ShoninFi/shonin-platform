//! Creating a station: only the vault can, at the one address its own address
//! derives, once; with an exit band no wider than the cap, each declared
//! driver once and each at a venue of its own, each the implementation the
//! registry approves at that moment, a program nobody can change, giving an
//! allowlist a station may hold. And the station program is set up once, by
//! its deployer.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::{InstructionData, ToAccountMetas};
use shonin_registry::error::RegistryError;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_test_driver::mode;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn ready() -> Env {
    let mut env = Env::new();
    env.set_up_stations();
    env
}

#[test]
fn a_station_is_where_its_vault_says_and_holds_what_it_was_told() {
    let mut env = ready();
    let home = env.open_vault("home");
    expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local()]));
    let station = env.station(&home);
    assert_eq!(home.station, shonin_station::station_address(&home.vault));
    assert_eq!(station.vault, home.vault);
    assert_eq!(station.vault_program, shonin_test_vault::ID);
    assert_eq!(station.vault_dollars, home.vault_dollars);
    assert_eq!(station.dollars, home.dollars);
    assert_eq!(station.mint, env.mint);
    assert_eq!(station.bundle_hash, home.bundle);
    assert_eq!(station.cluster, env.cluster());
    assert_eq!(station.exit_band_bps, EXIT_BAND);
    assert_eq!(station.idle, 0);
    assert!(!station.stopped);
    assert_eq!(station.payer, env.payer.pubkey());
    assert_eq!(station.drivers.len(), 1);
    let pinned = &station.drivers[0];
    assert_eq!(pinned.name, local());
    assert_eq!(pinned.program, shonin_local_driver::ID);
    assert_eq!(pinned.venue, shonin_local_venue::ID);
    assert_eq!(
        pinned.rules,
        shonin_local_driver::logic::allowlist(&shonin_local_venue::ID).rules
    );
    // Its dollar account exists, is the station's, and holds nothing.
    let dollars = env.svm.get_account(&home.dollars).unwrap();
    assert_eq!(dollars.owner, anchor_spl::token::ID);
    assert_eq!(&dollars.data[32..64], home.station.as_ref());
    assert_eq!(env.token_balance(&home.dollars), 0);
    // Its authority at the venue is its own address there, pinned with its
    // bump, and that authority's dollar account exists, is the authority's,
    // and holds nothing.
    let home = env.home_of("home", &[shonin_local_driver::ID]);
    let (authority, bump) = shonin_station::venue_authority(&home.station, &shonin_local_venue::ID);
    assert_eq!(pinned.authority, authority);
    assert_eq!(pinned.authority_bump, bump);
    assert_eq!(pinned.authority, home.authority(0));
    assert_eq!(pinned.dollars, home.authority_dollars(0));
    let dollars = env.svm.get_account(&pinned.dollars).unwrap();
    assert_eq!(dollars.owner, anchor_spl::token::ID);
    assert_eq!(&dollars.data[32..64], authority.as_ref());
    assert_eq!(env.token_balance(&pinned.dollars), 0);
}

#[test]
fn only_the_vault_creates_its_station() {
    let mut env = ready();
    let home = env.open_vault("home");
    // A wallet signing as the vault, with the vault's seeds: not the vault.
    let wallet = env.alice.insecure_clone();
    let mut fake = home.clone();
    fake.vault = wallet.pubkey();
    fake.station = shonin_station::station_address(&wallet.pubkey());
    fake.vault_dollars = env.token_account(&wallet.pubkey());
    fake.dollars =
        anchor_spl::associated_token::get_associated_token_address(&fake.station, &env.mint);
    let ix = env.ix_create(&fake, &wallet.pubkey(), EXIT_BAND, &[Declared::local()]);
    expect_code(
        env.send(vec![ix], &[&wallet]),
        station_error(StationError::NotAVault),
    );

    // Another vault of the same program, signing with its own seeds for this
    // vault's station: its address is not this vault's.
    let other = env.open_vault("other");
    let mut ix = env.ix_create(&other, &env.payer.pubkey(), EXIT_BAND, &[Declared::local()]);
    // Swap in this vault's seeds: they derive this vault, not the signer.
    let mut args: shonin_station::instruction::Create =
        anchor_lang::AnchorDeserialize::deserialize(&mut &ix.data[8..]).unwrap();
    args.args.vault_seeds[1] = home.label.to_vec();
    args.args.vault_seeds[2] = vec![
        Pubkey::find_program_address(
            &[shonin_test_vault::VAULT_SEED, &home.label],
            &shonin_test_vault::ID,
        )
        .1,
    ];
    ix.data = args.data();
    let call = env.ix_call(&other, ix, 0);
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![call], &[&payer]),
        station_error(StationError::NotAVault),
    );
    assert!(env
        .svm
        .get_account(&home.station)
        .is_none_or(|a| a.data.is_empty()));
}

#[test]
fn a_station_is_created_once_and_never_again() {
    let mut env = ready();
    let home = env.open_vault("home");
    expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local()]));
    let before = env.svm.get_account(&home.station).unwrap();
    // A second create, even with other drivers or another band, fails and
    // changes nothing.
    expect_failure(env.create_station(&home, 500, &[Declared::local(), Declared::test()]));
    assert_eq!(env.svm.get_account(&home.station).unwrap(), before);
}

#[test]
fn lamports_or_a_dollar_account_made_ahead_never_stop_a_station_being_created() {
    let mut env = ready();
    let home = env.open_vault("home");
    // Someone sends lamports to the station's address and makes its dollar
    // account first.
    let payer = env.payer.insecure_clone();
    let send = anchor_lang::solana_program::system_instruction::transfer(
        &payer.pubkey(),
        &home.station,
        1_000_000,
    );
    expect_ok(env.send(vec![send], &[&payer]));
    env.token_account(&home.station);
    expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local()]));
    assert_eq!(env.station(&home).drivers.len(), 1);
}

#[test]
fn an_exit_band_wider_than_the_cap_is_refused() {
    let mut env = ready();
    let home = env.open_vault("home");
    expect_code(
        env.create_station(&home, 1_001, &[Declared::local()]),
        station_error(StationError::BandTooWide),
    );
    expect_ok(env.create_station(&home, 1_000, &[Declared::local()]));
}

#[test]
fn a_strategy_listing_a_driver_twice_cannot_have_a_station() {
    let mut env = ready();
    let home = env.open_vault("home");
    expect_code(
        env.create_station(&home, EXIT_BAND, &[Declared::local(), Declared::local()]),
        station_error(StationError::DuplicateDriver),
    );
}

/// One driver per venue: two drivers reading the station's account at one
/// venue would each count it, and the station would value itself twice
/// over. Drivers at different venues are pinned side by side.
#[test]
fn two_drivers_over_one_venue_are_refused() {
    let mut env = ready();
    let home = env.open_vault("shared");
    expect_code(
        env.create_station(&home, EXIT_BAND, &[Declared::local(), Declared::test()]),
        station_error(StationError::SharedVenue),
    );
    expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local(), Declared::local_2()]));
}

#[test]
fn more_drivers_than_a_station_holds_are_refused() {
    let mut env = ready();
    let home = env.open_vault("home");
    let drivers: Vec<Declared> = (0..5)
        .map(|i| Declared {
            name: name(&format!("d{i}")),
            program: shonin_local_driver::ID,
        })
        .collect();
    expect_code(
        env.create_station(&home, EXIT_BAND, &drivers),
        station_error(StationError::TooManyDrivers),
    );
}

#[test]
fn a_driver_never_approved_or_still_waiting_or_revoked_is_refused() {
    let mut env = ready();
    let home = env.open_vault("home");
    // Never proposed.
    let never = Declared {
        name: name("never"),
        program: shonin_local_driver::ID,
    };
    expect_code(
        env.create_station(&home, EXIT_BAND, &[never]),
        station_error(StationError::NotApproved),
    );
    // Proposed, still inside its timelock.
    expect_ok(env.propose(
        ApprovalKind::Driver,
        name("waiting"),
        shonin_local_driver::ID,
    ));
    let waiting = Declared {
        name: name("waiting"),
        program: shonin_local_driver::ID,
    };
    expect_code(
        env.create_station(&home, EXIT_BAND, &[waiting]),
        station_error(StationError::NotApproved),
    );
    // Revoked.
    expect_ok(env.revoke(ApprovalKind::Driver, test_driver(), shonin_test_driver::ID));
    expect_code(
        env.create_station(&home, EXIT_BAND, &[Declared::test()]),
        station_error(StationError::NotApproved),
    );
}

#[test]
fn a_program_other_than_the_approved_one_is_refused() {
    let mut env = ready();
    let home = env.open_vault("home");
    // The test driver passed for the name the stand-in driver is approved
    // under.
    let imposter = Declared {
        name: local(),
        program: shonin_test_driver::ID,
    };
    expect_code(
        env.create_station(&home, EXIT_BAND, &[imposter]),
        station_error(StationError::NotApproved),
    );
}

#[test]
fn the_registrys_records_must_be_its_own() {
    let mut env = ready();
    let home = env.open_vault("home");
    let payer = env.payer.pubkey();
    let mut ix = env.ix_create(&home, &payer, EXIT_BAND, &[Declared::local()]);
    // The approval slot of another name in place of this one's. The group is
    // the program, its program data, the slot, the revocation record, then
    // the authority and its dollar account.
    let n = ix.accounts.len();
    ix.accounts[n - 4].pubkey =
        shonin_registry::read::approval_address(ApprovalKind::Driver, &test_driver());
    expect_code(
        env.as_vault(&home, ix),
        registry_error(RegistryError::AccountSubstituted),
    );
    // A look-alike slot owned by someone else, at the real address: refused
    // too, since a record is only ever read from the registry's own account.
    let mut ix = env.ix_create(&home, &payer, EXIT_BAND, &[Declared::local()]);
    let n = ix.accounts.len();
    ix.accounts[n - 3].pubkey = Pubkey::new_unique();
    expect_code(
        env.as_vault(&home, ix),
        registry_error(RegistryError::AccountSubstituted),
    );
}

#[test]
fn a_driver_program_that_can_still_be_changed_is_never_pinned() {
    let mut env = ready();
    let home = env.open_vault("home");
    // A final program deployed under the upgradeable loader, approved, then
    // shown with a program-data account that isn't its own.
    let program = env.upgradeable_program(None);
    let admin = env.admin.pubkey();
    let propose = env.ix_propose(
        &admin,
        ApprovalKind::Driver,
        name("upgradeable"),
        program,
        Some(program_data_address(&program)),
    );
    expect_ok(env.as_admin(propose));
    env.skip(TIMELOCK);
    let declared = Declared {
        name: name("upgradeable"),
        program,
    };
    let payer = env.payer.pubkey();
    let mut ix = env.ix_create(&home, &payer, EXIT_BAND, &[declared]);
    let n = ix.accounts.len();
    ix.accounts[n - 5] =
        AccountMeta::new_readonly(program_data_address(&shonin_station::ID), false);
    expect_code(
        env.as_vault(&home, ix),
        station_error(StationError::NotFinal),
    );
}

#[test]
fn an_allowlist_naming_a_program_a_station_never_calls_is_refused() {
    for bad in [
        mode::VENUE_IS_STATION,
        mode::VENUE_IS_REGISTRY,
        mode::VENUE_IS_TOKEN,
        mode::VENUE_IS_SYSTEM,
        mode::VENUE_IS_VAULT,
        mode::AMBIGUOUS,
    ] {
        let mut env = ready();
        let home = env.open_vault("home");
        env.set_mode(bad);
        expect_code(
            env.create_station(&home, EXIT_BAND, &[Declared::test()]),
            station_error(StationError::BadAllowlist),
        );
    }
}

/// No loader is ever a venue, whatever a driver says: each deploys, writes
/// or hands over programs on the signature of their authority. Nor is any of
/// Shonin's own programs.
#[test]
fn no_loader_and_no_shonin_program_is_ever_a_venue() {
    use shonin_driver::{Allowlist, Class, Own, Position, Rule};
    let allowlist = |venue| Allowlist {
        fixed: vec![],
        venue,
        rules: vec![Rule {
            discriminator: vec![1, 0, 0, 0],
            class: Class::Exit,
            positions: vec![Position {
                index: 0,
                own: Own::Station,
            }],
        }],
    };
    let vault_program = shonin_test_vault::ID;
    for loader in [
        solana_sdk_ids::bpf_loader::ID,
        solana_sdk_ids::bpf_loader_deprecated::ID,
        solana_sdk_ids::bpf_loader_upgradeable::ID,
        solana_sdk_ids::loader_v4::ID,
        solana_sdk_ids::native_loader::ID,
        shonin_registry::ID,
        shonin_publishing::ID,
        shonin_prices::ID,
        shonin_profiles::ID,
        shonin_dev_verifier::ID,
        shonin_station::ID,
    ] {
        assert!(
            shonin_station::guard::validate(&allowlist(loader), &vault_program).is_err(),
            "{loader} is accepted as a venue"
        );
    }
    assert!(
        shonin_station::guard::validate(&allowlist(shonin_local_venue::ID), &vault_program).is_ok()
    );
}

/// A driver only answers, and is never called with the station's signature:
/// no driver's venue may be one of the station's driver programs, its own or
/// another's.
#[test]
fn no_driver_program_is_ever_a_venue() {
    for (bad, venue, drivers) in [
        (
            mode::VENUE_IS_ITSELF,
            shonin_test_driver::ID,
            vec![Declared::test()],
        ),
        (
            mode::VENUE_IS_LOCAL_DRIVER,
            shonin_local_driver::ID,
            vec![Declared::local(), Declared::test()],
        ),
    ] {
        let mut env = ready();
        let home = env.open_vault("home");
        env.set_mode(bad);
        // The test driver's group carries the station's authority at the
        // venue its allowlist names, so the venue alone is what is refused.
        let payer = env.payer.pubkey();
        let mut ix = env.ix_create(&home, &payer, EXIT_BAND, &drivers);
        let named = Home {
            drivers: vec![shonin_test_driver::ID],
            ..home.clone()
        };
        let authority = shonin_station::venue_authority(&home.station, &venue).0;
        let dollars =
            anchor_spl::associated_token::get_associated_token_address(&authority, &env.mint);
        // The test driver's group is the last.
        for (from, to) in [
            (named.authority(0), authority),
            (named.authority_dollars(0), dollars),
        ] {
            let at = ix
                .accounts
                .iter()
                .rposition(|meta| meta.pubkey == from)
                .unwrap();
            ix.accounts[at].pubkey = to;
        }
        expect_code(
            env.as_vault(&home, ix),
            station_error(StationError::BadAllowlist),
        );
    }
}

/// Every pinned account a rule names is one the allowlist gave, and the
/// allowlist pins no more accounts than a station keeps.
#[test]
fn an_allowlist_pinning_accounts_it_does_not_give_or_too_many_is_refused() {
    use shonin_driver::{Allowlist, Class, Own, Position, Rule, MAX_FIXED};
    let allowlist = |own: Own, fixed: Vec<Vec<Pubkey>>| Allowlist {
        venue: shonin_local_venue::ID,
        rules: vec![Rule {
            discriminator: vec![1, 0, 0, 0],
            class: Class::Exit,
            positions: vec![Position { index: 0, own }],
        }],
        fixed,
    };
    let validate = |allowlist: Allowlist| {
        shonin_station::guard::validate(&allowlist, &shonin_test_vault::ID).is_ok()
    };
    let one = || vec![vec![Pubkey::new_unique()]];
    assert!(validate(allowlist(Own::Fixed(0), one())));
    assert!(validate(allowlist(Own::StationToken(Some(0)), one())));
    assert!(validate(allowlist(Own::StationToken(None), vec![])));
    assert!(!validate(allowlist(Own::Fixed(1), one())));
    assert!(!validate(allowlist(Own::StationToken(Some(1)), one())));
    assert!(!validate(allowlist(Own::Fixed(0), vec![vec![]])));
    let most = vec![vec![Pubkey::new_unique(); MAX_FIXED]];
    assert!(validate(allowlist(Own::Fixed(0), most)));
    let too_many = vec![vec![Pubkey::new_unique(); MAX_FIXED], one().remove(0)];
    assert!(!validate(allowlist(Own::Fixed(0), too_many)));
}

#[test]
fn a_driver_that_answers_garbage_or_nothing_is_never_pinned() {
    for (bad, error) in [
        (mode::GARBAGE, StationError::BadAnswer),
        (mode::SILENT, StationError::DriverSilent),
    ] {
        let mut env = ready();
        let home = env.open_vault("home");
        env.set_mode(bad);
        expect_code(
            env.create_station(&home, EXIT_BAND, &[Declared::test()]),
            station_error(error),
        );
    }
}

#[test]
fn the_accounts_for_each_driver_must_be_counted_out_exactly() {
    let mut env = ready();
    let home = env.open_vault("home");
    let payer = env.payer.pubkey();
    let ix = env.ix_create(&home, &payer, EXIT_BAND, &[Declared::local()]);
    for sizes in [vec![], vec![3], vec![5], vec![4, 0]] {
        let mut args: shonin_station::instruction::Create =
            anchor_lang::AnchorDeserialize::deserialize(&mut &ix.data[8..]).unwrap();
        args.args.driver_accounts = sizes.clone();
        let mut changed = ix.clone();
        changed.data = args.data();
        let result = env.as_vault(&home, changed);
        assert_eq!(
            code_of(&result),
            Some(station_error(StationError::WrongDrivers)),
            "groups {sizes:?} were taken"
        );
    }
}

#[test]
fn the_vaults_dollar_account_must_be_its_own() {
    let mut env = ready();
    let home = env.open_vault("home");
    let mut other = home.clone();
    other.vault_dollars = env.token_account(&env.alice.pubkey());
    let payer = env.payer.pubkey();
    let ix = env.ix_create(&other, &payer, EXIT_BAND, &[Declared::local()]);
    expect_failure(env.as_vault(&home, ix));
}

#[test]
fn the_station_program_is_set_up_once_by_its_deployer() {
    let mut env = Env::new();
    // Anyone else is refused.
    let alice = env.alice.insecure_clone();
    let ix = env.ix_initialize_stations(&alice.pubkey(), shonin_test_vault::ID);
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::NotTheDeployer),
    );
    env.set_up_stations();
    let deployer = env.deployer.insecure_clone();
    let ix = env.ix_initialize_stations(&deployer.pubkey(), Pubkey::new_unique());
    expect_failure(env.send(vec![ix], &[&deployer]));
    let config: shonin_station::state::Config =
        env.load(&shonin_station::config_address()).unwrap();
    assert_eq!(config.vault_program, shonin_test_vault::ID);
    assert_eq!(config.cluster, env.cluster());
}

#[test]
fn a_look_alike_station_account_is_never_read_as_one() {
    let (mut env, home) = Env::with_station();
    // An account at another address holding a copy of the station's bytes,
    // owned by the station program, can't stand in for it: every
    // instruction checks the station is at its vault's address.
    let copy = env.svm.get_account(&home.station).unwrap();
    let fake = Pubkey::new_unique();
    env.svm
        .set_account(
            fake,
            Account {
                lamports: copy.lamports,
                data: copy.data.clone(),
                owner: shonin_station::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let mut moved = home.clone();
    moved.station = fake;
    let executor = env.executor.insecure_clone();
    let ix = env.ix_approve_key(&moved, &executor.pubkey(), 0, &executor.pubkey());
    expect_code(
        env.send(vec![ix], &[&executor]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
}

/// The interface's discriminators are the ones Anchor gives every driver's
/// instructions, `unwind` included, and the vault stand-in's.
#[test]
fn the_interfaces_discriminators_are_the_programs_own() {
    use anchor_lang::Discriminator;
    use shonin_driver::discriminator as d;
    macro_rules! answers_all {
        ($driver:ident) => {
            assert_eq!($driver::instruction::Describe::DISCRIMINATOR, d::DESCRIBE);
            answers_all!($driver, without describe);
        };
        ($driver:ident, without describe) => {
            assert_eq!($driver::instruction::Allowlist::DISCRIMINATOR, d::ALLOWLIST);
            assert_eq!($driver::instruction::Restricted::DISCRIMINATOR, d::RESTRICTED);
            assert_eq!($driver::instruction::Value::DISCRIMINATOR, d::VALUE);
            assert_eq!($driver::instruction::Close::DISCRIMINATOR, d::CLOSE);
            assert_eq!($driver::instruction::Sweep::DISCRIMINATOR, d::SWEEP);
            assert_eq!($driver::instruction::Unwind::DISCRIMINATOR, d::UNWIND);
        };
    }
    answers_all!(shonin_local_driver);
    answers_all!(shonin_local_driver_2);
    answers_all!(shonin_local_driver_3);
    // The test driver answers what a station asks; it describes nothing.
    answers_all!(shonin_test_driver, without describe);
    answers_all!(shonin_orca_driver);
    assert_eq!(
        shonin_test_vault::instruction::StationView::DISCRIMINATOR,
        shonin_station::state::vault::STATION_VIEW
    );
    let _ = Instruction::new_with_bytes(Pubkey::default(), &[], vec![]);
    let _ = Keypair::new();
    let _ = shonin_station::accounts::Value {
        station: Pubkey::default(),
    }
    .to_account_metas(None);
}
