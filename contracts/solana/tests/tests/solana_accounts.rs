//! What Solana adds: every program is set up once, by its deployer; the
//! admin must sign; a record is the one its seeds name, owned by the program
//! that made it, so a look-alike or another key's record never stands in; no
//! two records' addresses can coincide; lamports sent ahead to an address
//! never stop its record being made; and a draft, the one record that is
//! closed, refunds whoever paid for it and can't be revived into a second
//! registration.

use std::collections::HashSet;

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::solana_program::system_instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use shonin_publishing::error::PublishingError;
use shonin_registry::error::RegistryError;
use shonin_registry::read as registry;
use shonin_solana_tests::*;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn system_error_already_in_use(result: TxResult) {
    // The system program refuses to create an account that exists.
    expect_code(result, 0);
}

// Setting up -------------------------------------------------------------------

#[test]
fn every_program_is_set_up_once() {
    let mut env = Env::new();
    let deployer = env.deployer.insecure_clone();
    let checker = env.create_checker(deployer.pubkey());
    let ix = env.ix_initialize_registry(
        &deployer.pubkey(),
        shonin_registry::InitializeArgs {
            admin: deployer.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock: 0,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        checker,
    );
    system_error_already_in_use(env.send(vec![ix], &[&deployer]));
    let ix = env.ix_initialize_publishing(&deployer.pubkey(), deployer.pubkey(), 0);
    system_error_already_in_use(env.send(vec![ix], &[&deployer]));
    let ix = env.ix_initialize_prices(&deployer.pubkey(), deployer.pubkey(), 0, published_prices());
    system_error_already_in_use(env.send(vec![ix], &[&deployer]));
    assert_eq!(env.config().admin, env.admin.pubkey());
    assert_eq!(env.publishing_config().platform, env.platform.pubkey());
}

/// A stranger who sets a fresh deployment up first would name the admin or
/// the fee's recipient: only the program's upgrade authority may.
#[test]
fn only_the_deployer_sets_a_program_up() {
    let mut env = Env::bare();
    let stranger = env.alice.insecure_clone();
    let ix = env.ix_initialize_registry(
        &stranger.pubkey(),
        shonin_registry::InitializeArgs {
            admin: stranger.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock: 0,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        env.checker,
    );
    expect_code(
        env.send(vec![ix], &[&stranger]),
        registry_error(RegistryError::NotTheDeployer),
    );
    let ix = env.ix_initialize_publishing(&stranger.pubkey(), stranger.pubkey(), 0);
    expect_code(
        env.send(vec![ix], &[&stranger]),
        publishing_error(PublishingError::NotTheDeployer),
    );
    let ix = env.ix_initialize_prices(&stranger.pubkey(), stranger.pubkey(), 0, published_prices());
    expect_code(
        env.send(vec![ix], &[&stranger]),
        prices_error(shonin_prices::PricesError::NotTheDeployer),
    );
}

/// A program made final before it was set up can never be: nobody holds its
/// upgrade authority.
#[test]
fn a_program_made_final_before_setup_is_never_set_up() {
    let mut env = Env::bare();
    let elf = std::fs::read(format!(
        "{}/../target/deploy/shonin_registry.so",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    env.deploy_upgradeable(shonin_registry::ID, &elf, None);
    let deployer = env.deployer.insecure_clone();
    let ix = env.ix_initialize_registry(
        &deployer.pubkey(),
        shonin_registry::InitializeArgs {
            admin: env.admin.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock: 0,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        env.checker,
    );
    expect_code(
        env.send(vec![ix], &[&deployer]),
        registry_error(RegistryError::NotTheDeployer),
    );
}

#[test]
fn the_development_checker_never_serves_mainnet() {
    let mut env = Env::new();
    let payer = env.payer.insecure_clone();
    let seed = [9u8; 32];
    let key = Keypair::new().pubkey();
    let checker = Pubkey::find_program_address(
        &[shonin_dev_verifier::CHECKER_SEED, key.as_ref(), &seed],
        &shonin_dev_verifier::ID,
    )
    .0;
    let ix = Instruction {
        program_id: shonin_dev_verifier::ID,
        accounts: shonin_dev_verifier::accounts::CreateChecker {
            payer: payer.pubkey(),
            checker,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None),
        data: shonin_dev_verifier::instruction::CreateChecker {
            attestation_key: key,
            seed,
            cluster_name: shonin_common::MAINNET_CLUSTER.to_string(),
        }
        .data(),
    };
    expect_code(
        env.send(vec![ix.clone()], &[&payer]),
        anchor_lang::error::ERROR_CODE_OFFSET
            + shonin_dev_verifier::DevVerifierError::Mainnet as u32,
    );
}

/// A checker, once made, is never made again over itself.
#[test]
fn a_checker_is_never_made_twice() {
    let mut env = Env::new();
    let payer = env.payer.insecure_clone();
    let seed = [7u8; 32];
    let key = env.attestor.pubkey();
    let checker = Pubkey::find_program_address(
        &[shonin_dev_verifier::CHECKER_SEED, key.as_ref(), &seed],
        &shonin_dev_verifier::ID,
    )
    .0;
    let ix = |cluster: &str| Instruction {
        program_id: shonin_dev_verifier::ID,
        accounts: shonin_dev_verifier::accounts::CreateChecker {
            payer: payer.pubkey(),
            checker,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None),
        data: shonin_dev_verifier::instruction::CreateChecker {
            attestation_key: key,
            seed,
            cluster_name: cluster.to_string(),
        }
        .data(),
    };
    expect_ok(env.send(vec![ix(CLUSTER)], &[&payer]));
    system_error_already_in_use(env.send(vec![ix("devnet")], &[&payer]));
}

// Signers ------------------------------------------------------------------------

/// The admin's key must sign: naming it is not enough.
#[test]
fn the_admin_must_sign() {
    let mut env = Env::new();
    let mut ix = env.ix_propose_image(&env.admin.pubkey(), hash("unsigned"), ImageKind::Executor);
    ix.accounts[0] = AccountMeta::new(env.admin.pubkey(), false);
    let alice = env.alice.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::AccountNotSigner),
    );
    assert!(env.image_of(hash("unsigned")).is_none());
}

/// A review is sent by the reviewer key itself: naming it without its
/// signature is refused.
#[test]
fn the_reviewer_must_sign_its_review() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let bundle = hash("bundle");
    env.register(id, author.pubkey(), bundle);
    let payer = env.payer.insecure_clone();
    let mut ix = env.ix_submit_review(
        &payer.pubkey(),
        &env.reviewer.pubkey(),
        id,
        review_for(bundle, 1),
    );
    ix.accounts[0] = AccountMeta::new_readonly(env.reviewer.pubkey(), false);
    expect_code(
        env.send(vec![ix], &[&payer]),
        anchor_error(anchor_lang::error::ErrorCode::AccountNotSigner),
    );
    assert!(!env.has_review(id));
}

// Owners and substitution ---------------------------------------------------------

/// A record at the right address but owned by another program is not the
/// registry's: an image planted that way is not allowed.
#[test]
fn a_record_owned_by_another_program_reads_as_never_recorded() {
    let mut env = Env::new();
    let planted = hash("planted image");
    let address = registry::image_address(&planted);
    let mut data = Vec::new();
    data.extend_from_slice(
        <shonin_registry::state::ImageRecord as anchor_lang::Discriminator>::DISCRIMINATOR,
    );
    data.extend_from_slice(&[1, 1]); // an executor, proposed
    data.extend_from_slice(&0u64.to_le_bytes()); // allowed since the beginning
    data.extend_from_slice(&[0, 255]); // not revoked
    env.svm
        .set_account(
            address,
            Account {
                lamports: 10_000_000,
                data,
                owner: shonin_publishing::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let journal = env.journal_for(
        planted,
        env.executor.pubkey(),
        env.leaf_for(hash("vault"), hash("bundle"), env.executor.pubkey()),
    );
    let cluster = env.cluster();
    expect_code(
        env.bless(&journal, cluster, hash("vault"), hash("bundle"), vec![]),
        registry_error(RegistryError::ImageNotAllowed),
    );
}

/// The records a blessing is read from must be the named key's own: a
/// registration naming one key and handing in another's records is refused.
#[test]
fn another_keys_records_never_stand_in() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let stranger = Keypair::new();
    let payer = env.payer.insecure_clone();
    expect_ok(env.send(vec![env.ix_stage(&payer.pubkey(), &reg)], &[&payer]));
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    // The stranger signs and is named as the builder; the builder's records
    // are handed in.
    let mut register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        id,
        &stranger.pubkey(),
        &reviewer.pubkey(),
    );
    let [key, image, project] = env.records_of(&builder.pubkey());
    register.accounts[6].pubkey = key;
    register.accounts[7].pubkey = image;
    register.accounts[8].pubkey = project;
    let ixs = vec![
        Env::ed25519(&stranger, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_code(
        env.send(ixs, &[&payer]),
        registry_error(RegistryError::AccountSubstituted),
    );

    // Nor the right key's record with another image's or project's.
    let mut register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    let [_, reviewer_image, _] = env.records_of(&reviewer.pubkey());
    register.accounts[7].pubkey = reviewer_image;
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_code(
        env.send(ixs, &[&payer]),
        registry_error(RegistryError::AccountSubstituted),
    );
    assert!(env.strategy_of(id).is_none());
}

/// The registry's settings are read from the registry's own account, never a
/// look-alike.
#[test]
fn a_look_alike_registry_config_is_refused() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let payer = env.payer.insecure_clone();
    expect_ok(env.send(vec![env.ix_stage(&payer.pubkey(), &reg)], &[&payer]));
    let mut register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    register.accounts[5].pubkey = publishing_config();
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_code(
        env.send(ixs, &[&payer]),
        registry_error(RegistryError::AccountSubstituted),
    );
}

/// A review is checked against its own strategy's record, never another's.
#[test]
fn a_review_cannot_borrow_another_strategys_record() {
    let mut env = Env::new();
    let (registered, bundle) = env.ready();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reviewer = env.reviewer.insecure_clone();
    let payer = env.payer.insecure_clone();
    let mut ix = env.ix_submit_review(
        &payer.pubkey(),
        &reviewer.pubkey(),
        id,
        review_for(bundle, 1),
    );
    ix.accounts[7].pubkey = strategy_address(registered);
    expect_code(
        env.send(vec![ix], &[&payer, &reviewer]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
    assert!(env.review_of(id).is_none());
}

/// A refusal can't be passed off against an empty account where the
/// strategy's record would be: the address is checked.
#[test]
fn a_refusal_reads_the_strategys_own_record() {
    let mut env = Env::new();
    let (registered, _) = env.ready();
    let reviewer = env.reviewer.insecure_clone();
    let payer = env.payer.insecure_clone();
    let mut ix = env.ix_submit_refusal(
        &payer.pubkey(),
        &reviewer.pubkey(),
        registered,
        review_for([0; 32], 3),
    );
    ix.accounts[8].pubkey = strategy_address(999);
    expect_code(
        env.send(vec![ix], &[&payer, &reviewer]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
}

// Addresses ----------------------------------------------------------------------

/// Every kind of record has its own prefix, and every field after it a fixed
/// width, so no two records, of one kind or of two, can share an address,
/// whatever ids, keys or names are chosen.
#[test]
fn no_two_records_share_an_address() {
    let mut seen = HashSet::new();
    let mut add = |address: Pubkey| assert!(seen.insert(address), "two records share {address}");
    let words = [[0u8; 32], [1u8; 32], hash("x")];
    for word in words {
        let key = Pubkey::new_from_array(word);
        add(registry::image_address(&word));
        add(registry::project_address(&word));
        add(registry::verifier_address(&key));
        add(registry::key_address(&key));
        add(registry::blessing_address(&key, &word, &word, &word));
        if word != [0; 32] {
            add(registry::blessing_address(
                &key, &[0; 32], &[0; 32], &[0; 32],
            ));
        }
        add(registry::approval_address(ApprovalKind::Driver, &word));
        add(registry::approval_address(ApprovalKind::Bridge, &word));
        add(registry::revoked_address(ApprovalKind::Driver, &word, &key));
        add(registry::revoked_address(ApprovalKind::Bridge, &word, &key));
        add(profile_address(&key));
        add(Pubkey::find_program_address(
            &[shonin_profiles::HANDLE_SEED, &word],
            &shonin_profiles::ID,
        )
        .0);
        add(Pubkey::find_program_address(
            &[shonin_dev_verifier::CHECKER_SEED, key.as_ref(), &word],
            &shonin_dev_verifier::ID,
        )
        .0);
        add(draft_address(
            u64::from_le_bytes(word[..8].try_into().unwrap()),
            &key,
        ));
    }
    for id in [0u64, 1, 2, u64::MAX] {
        add(submission_address(id));
        add(strategy_address(id));
        add(review_address(id));
    }
    add(registry::config_address());
    add(publishing_config());
    add(shonin_prices::read::prices_address());
    // A key's blessing for one vault is not its blessing for another chain's
    // vault at the same address.
    let key = Pubkey::new_unique();
    assert_ne!(
        registry::blessing_address(&key, &cluster_value("localnet"), &[5; 32], &[6; 32]),
        registry::blessing_address(&key, &cluster_value("devnet"), &[5; 32], &[6; 32])
    );
}

/// Lamports sent to an address before its record exists don't stop the
/// record being made.
#[test]
fn lamports_sent_ahead_never_block_a_record() {
    let mut env = Env::new();
    let payer = env.payer.insecure_clone();
    let author = env.author.insecure_clone();
    let id = env.publishing_config().count + 1;
    let addresses = [
        submission_address(id),
        strategy_address(id),
        review_address(id),
        profile_address(&author.pubkey()),
        shonin_profiles::handle_address("carry"),
        registry::key_address(&env.executor.pubkey()),
    ];
    let transfers = addresses
        .iter()
        .map(|to| system_instruction::transfer(&payer.pubkey(), to, 1_000_000))
        .collect();
    expect_ok(env.send(transfers, &[&payer]));

    assert_eq!(env.submit_as(&author), id);
    let bundle = hash("bundle");
    env.register(id, author.pubkey(), bundle);
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_review(&reviewer, id, review_for(bundle, 1)));
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, hash("vault"), bundle);
    assert!(env.has_review(id));
    assert_eq!(env.author_of_handle("carry"), author.pubkey());
}

// Drafts: closing and reviving ----------------------------------------------------

fn discard_ix(stager: &Pubkey, id: u64) -> Instruction {
    Instruction {
        program_id: shonin_publishing::ID,
        accounts: shonin_publishing::accounts::Discard {
            stager: *stager,
            draft: draft_address(id, stager),
        }
        .to_account_metas(None),
        data: shonin_publishing::instruction::Discard { strategy_id: id }.data(),
    }
}

/// A draft discarded refunds its stager, and can be staged again; once
/// registered, it is closed, and nothing registers twice.
#[test]
fn a_discarded_draft_can_be_staged_again_and_nothing_registers_twice() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let alice = env.alice.insecure_clone();
    let before = env.lamports(&alice.pubkey());
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    assert!(env.lamports(&alice.pubkey()) < before);
    expect_ok(env.send(vec![discard_ix(&alice.pubkey(), id)], &[&alice]));
    assert_eq!(env.lamports(&draft_address(id, &alice.pubkey())), 0);
    // Back to where it was, less two transactions' fees.
    assert!(env.lamports(&alice.pubkey()) + 20_000 >= before);

    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let register = env.ix_register(
        &alice.pubkey(),
        &alice.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register.clone(),
    ];
    expect_ok(env.send(ixs.clone(), &[&alice]));
    assert_eq!(
        env.lamports(&draft_address(id, &alice.pubkey())),
        0,
        "the draft outlived its registration"
    );

    // Staged again after the registration: it never registers.
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    expect_code(
        env.send(ixs, &[&alice]),
        publishing_error(PublishingError::AlreadyRegistered),
    );
}

/// Whoever registers pays for the strategy's record; the draft's rent goes
/// back to whoever staged it, not to the sender.
#[test]
fn a_drafts_rent_goes_back_to_whoever_staged_it() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    let draft_rent = env.lamports(&draft_address(id, &alice.pubkey()));
    let alice_before = env.lamports(&alice.pubkey());
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let register = env.ix_register(
        &bob.pubkey(),
        &alice.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_ok(env.send(ixs, &[&bob]));
    assert_eq!(env.lamports(&alice.pubkey()), alice_before + draft_rent);

    // And the refund can't be sent anywhere else.
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    let mut register = env.ix_register(
        &bob.pubkey(),
        &alice.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    register.accounts[1].pubkey = bob.pubkey();
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_code(
        env.send(ixs, &[&bob]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintAddress),
    );
}

/// A draft is its stager's alone: nobody else extends or discards it.
#[test]
fn nobody_else_extends_or_discards_a_draft() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &reg)], &[&alice]));
    let mut discard = discard_ix(&bob.pubkey(), id);
    discard.accounts[1].pubkey = draft_address(id, &alice.pubkey());
    expect_code(
        env.send(vec![discard], &[&bob]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
    assert!(env.lamports(&draft_address(id, &alice.pubkey())) > 0);
}

/// Anyone may stage a draft for any id, but a draft decides nothing: a
/// stranger's draft for the author's id never registers, and never stops the
/// build service's from registering.
#[test]
fn a_strangers_draft_neither_registers_nor_blocks() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let honest = Registration::new(id, author.pubkey(), hash("bundle"));
    let mut forged = honest.clone();
    forged.bundle_hash = hash("the stranger's bundle");
    let alice = env.alice.insecure_clone();
    expect_ok(env.send(vec![env.ix_stage(&alice.pubkey(), &forged)], &[&alice]));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    // The stranger replays the honest signatures against its own draft.
    let digest = env.registration_digest(&honest);
    let clearance = env.clearance_digest(id, honest.source_hash);
    let register = env.ix_register(
        &alice.pubkey(),
        &alice.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        register,
    ];
    expect_code(
        env.send(ixs, &[&alice]),
        publishing_error(PublishingError::NotTheBuilder),
    );
    expect_ok(env.register_with(&honest, &builder, &reviewer));
    assert_eq!(env.strategy_of(id).unwrap().bundle_hash, hash("bundle"));
}

/// A second registration is refused as one, whatever its lists' size.
#[test]
fn a_registration_of_any_size_is_refused_once_registered() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    expect_ok(env.register_with(&reg, &builder, &reviewer));
    let mut longer = reg.clone();
    longer.lists.bridges = vec![name("a"), name("b"), name("c")];
    expect_code(
        env.register_with(&longer, &builder, &reviewer),
        publishing_error(PublishingError::AlreadyRegistered),
    );
}
