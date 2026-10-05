//! A bundle nobody built from source nobody read cannot be registered: a
//! registration needs a blessed builder's signature over everything it
//! registers and a blessed reviewer's go-ahead for that source, both bound to
//! this cluster, this program, the strategy's record and their purpose, and
//! it registers once, for the author who paid for the id.

use anchor_lang::prelude::Pubkey;
use shonin_publishing::error::PublishingError;
use shonin_publishing::state::MAX_RESTRICTED;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn registration(env: &mut Env) -> Registration {
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    Registration::new(id, author.pubkey(), hash("bundle"))
}

fn keys(env: &Env) -> (Keypair, Keypair) {
    (env.builder.insecure_clone(), env.reviewer.insecure_clone())
}

#[test]
fn a_strategy_registers_with_a_builder_signature_and_a_reviewer_go_ahead() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    expect_ok(env.register_with(&reg, &builder, &reviewer));
    let strategy = env.strategy_of(reg.id).expect("registered");
    assert_eq!(strategy.bundle_hash, hash("bundle"));
    assert_eq!(strategy.source_hash, hash("source"));
    assert_eq!(strategy.manifest_hash, hash("manifest"));
    assert_eq!(strategy.author, env.author.pubkey());
    assert_eq!(strategy.registered_at, env.now());
    assert_eq!(strategy.lists.chains[0].chain, name("solana"));
    assert_eq!(strategy.lists.chains[0].drivers[0], name("local"));
    assert_eq!(
        strategy.lists.restricted,
        vec![name("SANCTIONED"), name("US")]
    );
}

#[test]
fn a_signature_from_anyone_but_a_builder_is_refused() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (executor, reviewer) = (env.executor.insecure_clone(), env.reviewer.insecure_clone());
    let executor_pubkey = executor.pubkey();
    env.bless_executor_key(executor_pubkey, hash("vault"), hash("bundle"));
    expect_code(
        env.register_with(&reg, &executor, &reviewer),
        publishing_error(PublishingError::NotTheBuilder),
    );
    // Nor a key nobody ever blessed.
    expect_code(
        env.register_with(&reg, &Keypair::new(), &reviewer),
        publishing_error(PublishingError::NotTheBuilder),
    );
    assert!(env.strategy_of(reg.id).is_none());
}

#[test]
fn nothing_registers_without_the_reviewer_go_ahead() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let builder = env.builder.insecure_clone();
    expect_code(
        env.register_with(&reg, &builder, &builder),
        publishing_error(PublishingError::NotCleared),
    );
    let digest = env.registration_digest(&reg);
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.register_signed(&reg, &builder, Some(digest), &reviewer, None),
        publishing_error(PublishingError::NotCleared),
    );
}

/// A transaction with no signature at all from the builder registers nothing.
#[test]
fn an_absent_signature_is_never_a_builder_signature() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    expect_code(
        env.register_signed(&reg, &builder, None, &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
    assert!(
        env.strategy_of(reg.id).is_none(),
        "registered with no builder signature at all"
    );
    assert!(!env.is_blessed_as(&Pubkey::default(), ImageKind::Builder));
}

#[test]
fn a_go_ahead_is_for_one_strategy_and_one_source() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let author = env.author.insecure_clone();
    let other = env.submit_as(&author);
    let (builder, reviewer) = keys(&env);
    let digest = env.registration_digest(&reg);
    let for_other = env.clearance_digest(other, reg.source_hash);
    expect_code(
        env.register_signed(&reg, &builder, Some(digest), &reviewer, Some(for_other)),
        publishing_error(PublishingError::NotCleared),
    );
    let for_other_source = env.clearance_digest(reg.id, hash("another source"));
    expect_code(
        env.register_signed(
            &reg,
            &builder,
            Some(digest),
            &reviewer,
            Some(for_other_source),
        ),
        publishing_error(PublishingError::NotCleared),
    );
}

#[test]
fn a_builder_cannot_register_a_different_bundle_than_it_signed() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let signed = env.registration_digest(&reg);
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    let mut different = reg.clone();
    different.bundle_hash = hash("a different bundle");
    expect_code(
        env.register_signed(
            &different,
            &builder,
            Some(signed),
            &reviewer,
            Some(clearance),
        ),
        publishing_error(PublishingError::NotTheBuilder),
    );
}

#[test]
fn the_chains_bridges_and_restricted_list_are_part_of_what_the_builder_signed() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let signed = env.registration_digest(&reg);
    let clearance = env.clearance_digest(reg.id, reg.source_hash);

    let mut changed = reg.clone();
    changed.lists.chains[0].drivers[0] = name("something-else");
    expect_code(
        env.register_signed(&changed, &builder, Some(signed), &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
    let mut changed = reg.clone();
    changed.lists.bridges.push(name("local-bridge"));
    expect_code(
        env.register_signed(&changed, &builder, Some(signed), &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
    let mut changed = reg.clone();
    changed.lists.restricted.pop();
    expect_code(
        env.register_signed(&changed, &builder, Some(signed), &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
    let mut changed = reg.clone();
    changed.manifest_hash = hash("another manifest");
    expect_code(
        env.register_signed(&changed, &builder, Some(signed), &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
}

#[test]
fn an_id_is_registered_only_for_the_wallet_that_paid_for_it() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    let id = env.submit_as(&alice);
    let reg = Registration::new(id, env.author.pubkey(), hash("bundle"));
    let (builder, reviewer) = keys(&env);
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NotSubmittedByTheAuthor),
    );
    // And an id nobody paid for registers for nobody.
    let unpaid = Registration::new(99, env.author.pubkey(), hash("bundle"));
    expect_code(
        env.register_with(&unpaid, &builder, &reviewer),
        publishing_error(PublishingError::NotSubmittedByTheAuthor),
    );
    // Nor for the zero author.
    let nobody = Registration::new(id, Pubkey::default(), hash("bundle"));
    expect_code(
        env.register_with(&nobody, &builder, &reviewer),
        publishing_error(PublishingError::NotSubmittedByTheAuthor),
    );
}

#[test]
fn a_strategy_registers_once() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    expect_ok(env.register_with(&reg, &builder, &reviewer));
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::AlreadyRegistered),
    );
    let mut other = reg.clone();
    other.bundle_hash = hash("a later bundle");
    expect_code(
        env.register_with(&other, &builder, &reviewer),
        publishing_error(PublishingError::AlreadyRegistered),
    );
    assert_eq!(env.strategy_of(reg.id).unwrap().bundle_hash, hash("bundle"));
}

#[test]
fn the_restricted_list_is_sorted_and_without_repeats() {
    let mut env = Env::new();
    let mut reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    reg.lists.restricted = vec![name("US"), name("US")];
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NotSorted),
    );
    reg.lists.restricted = vec![name("US"), name("SANCTIONED")];
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NotSorted),
    );
}

/// Zero means "nothing" everywhere else (not registered, or refused), so a
/// registration must name a source and a bundle.
#[test]
fn a_strategy_cannot_register_with_no_bundle_or_no_source() {
    let mut env = Env::new();
    let mut reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    reg.bundle_hash = [0; 32];
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NothingBuilt),
    );
    reg.bundle_hash = hash("bundle");
    reg.source_hash = [0; 32];
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NothingBuilt),
    );
}

/// The two signatures a registration needs are over distinct, purposed
/// digests: a reviewer's clearance never passes as a builder's signature over
/// the same strategy, nor the reverse.
#[test]
fn neither_signature_serves_the_other_purpose() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let registration_digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    assert_ne!(registration_digest, clearance);

    // The reviewer signs the registration digest: a signature from a
    // non-builder, however blessed.
    expect_code(
        env.register_signed(
            &reg,
            &reviewer,
            Some(registration_digest),
            &reviewer,
            Some(clearance),
        ),
        publishing_error(PublishingError::NotTheBuilder),
    );
    // The builder signs the clearance digest: not a reviewer.
    expect_code(
        env.register_signed(
            &reg,
            &builder,
            Some(registration_digest),
            &builder,
            Some(clearance),
        ),
        publishing_error(PublishingError::NotCleared),
    );
    // Each signing the other's digest.
    expect_code(
        env.register_signed(
            &reg,
            &builder,
            Some(clearance),
            &reviewer,
            Some(registration_digest),
        ),
        publishing_error(PublishingError::NotTheBuilder),
    );
    assert!(env.strategy_of(reg.id).is_none());
}

/// A registration signed for another cluster does not register here, nor
/// one signed for another strategy's record.
#[test]
fn registration_signatures_are_bound_to_this_cluster_and_this_record() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let devnet = cluster_value("devnet");
    let elsewhere = shonin_publishing::state::registration_digest(
        &devnet,
        reg.id,
        &reg.author,
        &reg.source_hash,
        &reg.bundle_hash,
        &reg.manifest_hash,
        &reg.lists,
    );
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    expect_code(
        env.register_signed(&reg, &builder, Some(elsewhere), &reviewer, Some(clearance)),
        publishing_error(PublishingError::NotTheBuilder),
    );
    let digest = env.registration_digest(&reg);
    let cleared_elsewhere =
        shonin_publishing::state::clearance_digest(&devnet, reg.id, &reg.source_hash);
    expect_code(
        env.register_signed(
            &reg,
            &builder,
            Some(digest),
            &reviewer,
            Some(cleared_elsewhere),
        ),
        publishing_error(PublishingError::NotCleared),
    );
    // Signed for another id's record by the same author.
    let author = env.author.insecure_clone();
    let other = env.submit_as(&author);
    let mut for_other = reg.clone();
    for_other.id = other;
    let other_digest = env.registration_digest(&for_other);
    expect_code(
        env.register_signed(
            &reg,
            &builder,
            Some(other_digest),
            &reviewer,
            Some(clearance),
        ),
        publishing_error(PublishingError::NotTheBuilder),
    );
    assert_ne!(
        shonin_publishing::state::strategy_address(reg.id),
        shonin_publishing::state::strategy_address(other)
    );
}

/// A builder whose blessing has lapsed, or whose image was revoked, signs
/// nothing that registers.
#[test]
fn a_builder_no_longer_blessed_registers_nothing() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    expect_ok(env.revoke_image(builder_image()));
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NotTheBuilder),
    );

    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    env.skip(BLESSING_TTL + 1);
    expect_code(
        env.register_with(&reg, &builder, &reviewer),
        publishing_error(PublishingError::NotTheBuilder),
    );
}

/// A strategy's lists can be longer than one transaction holds: they are
/// assembled in pieces, every one bounded, and register as a whole.
#[test]
fn a_long_registration_is_assembled_in_pieces() {
    let mut env = Env::new();
    let mut reg = registration(&mut env);
    let mut codes: Vec<[u8; 32]> = (0..MAX_RESTRICTED as u32)
        .map(|i| keccak(&[&i.to_le_bytes()]))
        .collect();
    codes.sort();
    reg.lists.restricted = codes.clone();
    let payer = env.payer.insecure_clone();
    let mut staged = reg.clone();
    staged.lists.restricted = codes[..16].to_vec();
    let ix = env.ix_stage(&payer.pubkey(), &staged);
    expect_ok(env.send(vec![ix], &[&payer]));
    for chunk in codes[16..].chunks(16) {
        let ix = extend(&payer.pubkey(), reg.id, chunk.to_vec());
        expect_ok(env.send(vec![ix], &[&payer]));
    }
    // One more is more than a strategy may carry.
    let ix = extend(&payer.pubkey(), reg.id, vec![[0xFF; 32]]);
    expect_code(
        env.send(vec![ix], &[&payer]),
        publishing_error(PublishingError::TooMany),
    );

    let (builder, reviewer) = keys(&env);
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        env.ix_register(
            &payer.pubkey(),
            &payer.pubkey(),
            reg.id,
            &builder.pubkey(),
            &reviewer.pubkey(),
        ),
    ];
    expect_ok(env.send(ixs, &[&payer]));
    assert_eq!(env.strategy_of(reg.id).unwrap().lists.restricted, codes);
}

fn extend(stager: &Pubkey, id: u64, restricted: Vec<[u8; 32]>) -> Instruction {
    use anchor_lang::{InstructionData, ToAccountMetas};
    Instruction {
        program_id: shonin_publishing::ID,
        accounts: shonin_publishing::accounts::Extend {
            stager: *stager,
            draft: draft_address(id, stager),
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None),
        data: shonin_publishing::instruction::Extend {
            strategy_id: id,
            more: Lists {
                chains: vec![],
                bridges: vec![],
                restricted,
            },
        }
        .data(),
    }
}

#[test]
fn a_registration_names_no_more_than_a_strategy_may() {
    let mut env = Env::new();
    let mut reg = registration(&mut env);
    reg.lists.chains = (0..9)
        .map(|i| ChainNeed {
            chain: name(&format!("chain{i}")),
            drivers: vec![],
        })
        .collect();
    let payer = env.payer.insecure_clone();
    let ix = env.ix_stage(&payer.pubkey(), &reg);
    expect_code(
        env.send(vec![ix], &[&payer]),
        publishing_error(PublishingError::TooMany),
    );
    reg.lists.chains = vec![ChainNeed {
        chain: name("solana"),
        drivers: (0..9).map(|i| name(&format!("driver{i}"))).collect(),
    }];
    let ix = env.ix_stage(&payer.pubkey(), &reg);
    expect_code(
        env.send(vec![ix], &[&payer]),
        publishing_error(PublishingError::TooMany),
    );
}

// Signatures carried by another instruction -------------------------------------

/// The Ed25519 program checks the bytes its offsets point at, which may be
/// another instruction's. A reader that took the message from the Ed25519
/// instruction's own bytes would believe the builder signed whatever sits
/// there. Here the builder signed only its own key, read from the
/// registration's bytes, while the Ed25519 instruction carries the
/// registration digest at the same offset: nothing registers.
#[test]
fn a_signature_over_other_bytes_passed_through_another_instruction_never_counts() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let payer = env.payer.insecure_clone();
    let stage = env.ix_stage(&payer.pubkey(), &reg);
    expect_ok(env.send(vec![stage], &[&payer]));

    let register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        reg.id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    // The register instruction's bytes 16..48 are the builder's key.
    assert_eq!(&register.data[16..48], builder.pubkey().as_ref());
    let signed_bytes = builder.pubkey().to_bytes();
    let signature = builder.sign_message(&signed_bytes);
    let digest = env.registration_digest(&reg);

    // [count, pad, offsets] then the digest at 16, the key at 48, the
    // signature at 80. The message is read from instruction 3 (the
    // registration, after the compute budget and two Ed25519 instructions).
    let mut data = vec![1u8, 0];
    for word in [80u16, u16::MAX, 48, u16::MAX, 16, 32, 3] {
        data.extend_from_slice(&word.to_le_bytes());
    }
    data.extend_from_slice(&digest);
    data.extend_from_slice(builder.pubkey().as_ref());
    data.extend_from_slice(signature.as_ref());
    let crafted = Instruction {
        program_id: solana_sdk_ids::ed25519_program::ID,
        accounts: vec![],
        data,
    };
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    let ixs = vec![crafted, Env::ed25519(&reviewer, &clearance), register];
    expect_code(
        env.send(ixs, &[&payer]),
        publishing_error(PublishingError::NotTheBuilder),
    );
    assert!(env.strategy_of(reg.id).is_none());
}

/// A signature the Ed25519 program would reject fails the whole transaction:
/// it never reaches the registration.
#[test]
fn a_forged_signature_fails_the_transaction() {
    let mut env = Env::new();
    let reg = registration(&mut env);
    let (builder, reviewer) = keys(&env);
    let payer = env.payer.insecure_clone();
    let stage = env.ix_stage(&payer.pubkey(), &reg);
    expect_ok(env.send(vec![stage], &[&payer]));
    let digest = env.registration_digest(&reg);
    let mut forged = Env::ed25519(&Keypair::new(), &digest);
    // The stranger's signature, presented under the builder's key.
    forged.data[16..48].copy_from_slice(builder.pubkey().as_ref());
    let clearance = env.clearance_digest(reg.id, reg.source_hash);
    let register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        reg.id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    let result = env.send(
        vec![forged, Env::ed25519(&reviewer, &clearance), register],
        &[&payer],
    );
    expect_failure(result);
    assert!(env.strategy_of(reg.id).is_none());
}
