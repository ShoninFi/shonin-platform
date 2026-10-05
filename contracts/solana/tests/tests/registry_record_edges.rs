//! The registry's records as Solana accounts. Lamports sent
//! ahead to any record's address never block it; a blessing reads only its
//! own records, whatever is handed in; settling a slot touches exactly the
//! revocation record it must, and a revocation that must settle another
//! proposal is held to that record; and nothing that is not a final program
//! this code understands reads as final.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::solana_program::system_instruction;
use shonin_registry::error::RegistryError;
use shonin_registry::read as registry;
use shonin_solana_tests::*;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

const DRIVER: ApprovalKind = ApprovalKind::Driver;

fn vault() -> [u8; 32] {
    hash("vault")
}
fn bundle() -> [u8; 32] {
    hash("bundle")
}

fn fund_ahead(env: &mut Env, addresses: &[Pubkey]) {
    let payer = env.payer.insecure_clone();
    let transfers = addresses
        .iter()
        .map(|to| system_instruction::transfer(&payer.pubkey(), to, 1_000_000))
        .collect();
    expect_ok(env.send(transfers, &[&payer]));
}

// Lamports sent ahead ---------------------------------------------------------------

/// Every record the registry makes, pre-funded: the settings and the first
/// checker's record at setup, then an image, a project, a checker, a slot, a
/// revocation record, a key and a blessing.
#[test]
fn lamports_sent_ahead_never_block_any_registry_record() {
    let mut env = Env::bare();
    let first_checker = env.checker;
    fund_ahead(
        &mut env,
        &[
            registry::config_address(),
            registry::verifier_address(&first_checker),
        ],
    );
    env.initialize_registry(TIMELOCK, 0);

    let image = hash("funded image");
    let project_id = hash("funded project");
    let next_checker = env.create_checker(Keypair::new().pubkey());
    let driver = env.final_program();
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    fund_ahead(
        &mut env,
        &[
            registry::image_address(&image),
            registry::project_address(&project_id),
            registry::verifier_address(&next_checker),
            registry::approval_address(DRIVER, &name("funded")),
            registry::revoked_address(DRIVER, &name("funded"), &driver),
            registry::key_address(&executor),
            registry::blessing_address(&executor, &cluster, &vault(), &bundle()),
        ],
    );
    expect_ok(env.propose_image(image, ImageKind::Executor));
    expect_ok(env.propose_project(project_id));
    let ix = env.ix_propose_verifier(
        &env.admin.pubkey(),
        shonin_dev_verifier::ID,
        None,
        next_checker,
    );
    expect_ok(env.as_admin(ix));
    expect_ok(env.propose(DRIVER, name("funded"), driver));
    expect_ok(env.revoke(DRIVER, name("funded"), driver));
    env.skip(TIMELOCK);
    assert!(env.is_image_allowed(image));
    assert!(env.is_project_allowed(project_id));
    assert!(env.is_verifier_allowed(&next_checker));
    assert!(env.is_revoked(DRIVER, name("funded"), driver));

    let journal = Journal {
        image_digest: image,
        project_id,
        signer: executor,
        vaults_root: env.leaf_for(vault(), bundle()),
        issued_at: env.now(),
    };
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    assert!(env.is_blessed_for(&executor, cluster, vault(), bundle()));
}

// A blessing reads only its own records ------------------------------------------------

/// The image, project, key and blessing accounts of a bless are the ones the
/// journal and the arguments name; the settings are the registry's own.
#[test]
fn a_blessing_reads_only_the_records_its_journal_names() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let journal = env.journal_for(executor_image(), executor, env.leaf_for(vault(), bundle()));
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, &journal);
    let cluster = env.cluster();
    let honest = env.ix_bless(
        &payer.pubkey(),
        &journal,
        seal,
        cluster,
        vault(),
        bundle(),
        vec![],
    );
    // 0 payer, 1 config, 2 image, 3 project, 4 key, 5 blessing.
    let seeds = anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds);
    let owner = anchor_error(anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram);
    let swaps: Vec<(usize, Pubkey, u32)> = vec![
        (1, publishing_config(), owner),
        (2, registry::image_address(&builder_image()), seeds),
        (2, registry::project_address(&project()), seeds),
        (3, registry::project_address(&hash("another")), seeds),
        (4, registry::key_address(&env.builder.pubkey()), seeds),
        (
            5,
            registry::blessing_address(&executor, &cluster, &hash("another vault"), &bundle()),
            seeds,
        ),
        (
            5,
            registry::blessing_address(&env.builder.pubkey(), &[0; 32], &[0; 32], &[0; 32]),
            seeds,
        ),
    ];
    for (index, substitute, code) in swaps {
        let mut ix = honest.clone();
        ix.accounts[index].pubkey = substitute;
        expect_code(env.send(vec![ed25519.clone(), ix], &[&payer]), code);
    }
    assert!(!env.is_blessed_for(&executor, cluster, vault(), bundle()));
    expect_ok(env.send(vec![ed25519, honest], &[&payer]));
    assert!(env.is_blessed_for(&executor, cluster, vault(), bundle()));
}

/// A record planted at a key's address but owned by another program is not
/// the key's record: a station reading it finds the key never blessed.
#[test]
fn a_planted_key_record_reads_as_never_blessed() {
    let mut env = Env::new();
    let signer = Keypair::new().pubkey();
    let mut data = Vec::new();
    data.extend_from_slice(<KeyInfo as anchor_lang::Discriminator>::DISCRIMINATOR);
    data.extend_from_slice(&executor_image());
    data.extend_from_slice(&project());
    data.push(1); // Executor
    data.extend_from_slice(&bundle());
    data.extend_from_slice(&env.cluster());
    data.extend_from_slice(&vault());
    data.extend_from_slice(&shonin_dev_verifier::TRUST_ROOT);
    data.extend_from_slice(env.checker.as_ref());
    data.extend_from_slice(&u64::MAX.to_le_bytes());
    data.push(255);
    env.svm
        .set_account(
            registry::key_address(&signer),
            Account {
                lamports: 10_000_000,
                data,
                owner: shonin_publishing::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    assert!(!env.is_blessed_as(&signer, ImageKind::Executor));
}

// Settling a slot ----------------------------------------------------------------------

/// A revocation that settles a re-proposal of another implementation must
/// clear that implementation's old revocation, so it needs the record, and
/// writable: handed read-only, or not at all, the revocation is refused and
/// nothing changes; handed as it should be, both take effect.
#[test]
fn a_revocation_that_settles_another_proposal_is_held_to_that_proposals_record() {
    let mut env = Env::new();
    let a = env.driver;
    let b = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, name("local"), b));
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(name("local")), b);
    assert!(!env.is_revoked(DRIVER, name("local"), b));

    let admin = env.admin.pubkey();
    let record = registry::revoked_address(DRIVER, &name("local"), &b);
    let mut honest = env.ix_revoke(&admin, DRIVER, name("local"), a);
    // 0 admin, 1 config, 2 slot, 3 revoked, 4 next_revoked.
    assert_eq!(honest.accounts[4].pubkey, record);

    let mut read_only = honest.clone();
    read_only.accounts[4] = AccountMeta::new_readonly(record, false);
    expect_failure(env.as_admin(read_only));
    let mut missing = honest.clone();
    missing.accounts[4] = AccountMeta::new_readonly(shonin_registry::ID, false);
    expect_code(
        env.as_admin(missing),
        registry_error(RegistryError::MissingRevocationRecord),
    );
    assert!(
        !env.is_revoked(DRIVER, name("local"), a),
        "a refused revocation stuck"
    );
    assert_eq!(env.trading_driver(name("local")), b);

    honest.accounts[4] = AccountMeta::new(record, false);
    expect_ok(env.as_admin(honest));
    assert!(env.is_revoked(DRIVER, name("local"), a));
    assert!(!env.is_revoked(DRIVER, name("local"), b));
    assert_eq!(env.trading_driver(name("local")), b);
    let slot: shonin_registry::state::ApprovalSlot = env
        .load(&registry::approval_address(DRIVER, &name("local")))
        .unwrap();
    assert_eq!(slot.current, b);
    assert_eq!(slot.next, Pubkey::default());
}

/// Withdrawing when the waiting proposal is already ready settles it first
/// and then finds nothing to withdraw; the whole instruction is undone, so
/// the old revocation record is not cleared early, and the ready proposal
/// still reads as approved.
#[test]
fn a_withdrawal_that_finds_the_proposal_ready_changes_nothing() {
    let mut env = Env::new();
    let b = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, name("local"), b));
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    expect_code(
        env.withdraw(DRIVER, name("local")),
        registry_error(RegistryError::NoImplementation),
    );
    let record: shonin_registry::state::RevokedRecord = env
        .load(&registry::revoked_address(DRIVER, &name("local"), &b))
        .unwrap();
    assert!(record.revoked, "the failed withdrawal cleared the record");
    assert!(!env.is_revoked(DRIVER, name("local"), b));
    assert_eq!(env.trading_driver(name("local")), b);
}

/// Revoking the ready, unsettled re-proposal itself: it is settled as the
/// implementation in force and revoked in the same instruction, and the
/// record it carried is not handed in twice.
#[test]
fn revoking_a_ready_unsettled_reproposal_settles_and_revokes_it() {
    let mut env = Env::new();
    let b = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, name("local"), b));
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    let admin = env.admin.pubkey();
    let ix = env.ix_revoke(&admin, DRIVER, name("local"), b);
    assert_eq!(
        ix.accounts[4].pubkey,
        shonin_registry::ID,
        "the record was handed in twice"
    );
    expect_ok(env.as_admin(ix));
    assert!(env.is_revoked(DRIVER, name("local"), b));
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
    let slot: shonin_registry::state::ApprovalSlot = env
        .load(&registry::approval_address(DRIVER, &name("local")))
        .unwrap();
    assert_eq!(slot.current, b);
    assert_eq!(slot.next, Pubkey::default());
    // Proposed once more, it waits a full timelock again.
    expect_ok(env.propose(DRIVER, name("local"), b));
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(name("local")), b);
}

// What reads as final --------------------------------------------------------------------

/// Nothing this code does not understand reads as final: a loader's own
/// account, a program-data account with a malformed authority, a
/// program-data account passed as the program.
#[test]
fn nothing_but_a_final_program_reads_as_final() {
    let mut env = Env::new();
    let admin = env.admin.pubkey();

    // The upgradeable loader itself: executable, owned by the native loader.
    let ix = env.ix_propose(
        &admin,
        DRIVER,
        name("t"),
        solana_sdk_ids::bpf_loader_upgradeable::ID,
        None,
    );
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // Program data whose authority byte is neither 0 nor 1.
    let program = env.upgradeable_program(None);
    let data_address = program_data_address(&program);
    let mut account = env.svm.get_account(&data_address).unwrap();
    account.data[12] = 2;
    env.svm.set_account(data_address, account).unwrap();
    let ix = env.ix_propose(&admin, DRIVER, name("t"), program, Some(data_address));
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // The program-data account handed in as the program.
    let finalized = env.upgradeable_program(None);
    let ix = env.ix_propose(
        &admin,
        DRIVER,
        name("t"),
        program_data_address(&finalized),
        Some(program_data_address(&finalized)),
    );
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // And a final one, with its program data, is accepted.
    let ix = env.ix_propose(
        &admin,
        DRIVER,
        name("t"),
        finalized,
        Some(program_data_address(&finalized)),
    );
    expect_ok(env.as_admin(ix));
}

/// The settings never change after setup: there is no instruction that
/// moves the admin, the cluster, the floor or the TTL, and the admin's
/// instructions leave them as they were.
#[test]
fn the_settings_fixed_at_setup_never_move() {
    let mut env = Env::new();
    let before = env.config();
    expect_ok(env.set_timelock(3 * DAY));
    expect_ok(env.propose_image(hash("x"), ImageKind::Executor));
    expect_ok(env.revoke_image(hash("x")));
    let checker = env.propose_checker(Keypair::new().pubkey());
    expect_ok(env.retire_verifier(checker));
    let after = env.config();
    assert_eq!(after.admin, before.admin);
    assert_eq!(after.cluster, before.cluster);
    assert_eq!(after.min_timelock, before.min_timelock);
    assert_eq!(after.blessing_ttl, before.blessing_ttl);
    assert_eq!(after.verifiers, before.verifiers);
}
