//! What the allowlist is for: an image or a project nobody had time to object
//! to cannot hold a key, removing one is immediate, only the admin changes it,
//! and the timelock that protects all of it can only be cut slowly, never
//! below its floor, and never past a year.

use shonin_registry::error::RegistryError;
use shonin_registry::InitializeArgs;
use shonin_solana_tests::*;
use solana_signer::Signer;

fn initialize(env: &mut Env, timelock: u64, min_timelock: u64) -> TxResult {
    let deployer = env.deployer.insecure_clone();
    let ix = env.ix_initialize_registry(
        &deployer.pubkey(),
        InitializeArgs {
            admin: env.admin.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock,
            min_timelock,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        env.checker,
    );
    env.send(vec![ix], &[&deployer])
}

// The allowlist ---------------------------------------------------------------

#[test]
fn an_image_waits_out_its_timelock() {
    let mut env = Env::new();
    let image = hash("a new executor");
    expect_ok(env.propose_image(image, ImageKind::Executor));
    assert!(!env.is_image_allowed(image));
    env.skip(TIMELOCK - 1);
    assert!(!env.is_image_allowed(image));
    env.skip(1);
    assert!(env.is_image_allowed(image));
}

#[test]
fn revoking_an_image_takes_effect_at_once() {
    let mut env = Env::new();
    assert!(env.is_image_allowed(builder_image()));
    expect_ok(env.revoke_image(builder_image()));
    assert!(!env.is_image_allowed(builder_image()));
    // And the keys it holds stop counting in the same instant.
    assert!(!env.is_blessed_as(&env.builder.pubkey(), ImageKind::Builder));
}

#[test]
fn a_project_waits_out_its_timelock_and_is_revoked_at_once() {
    let mut env = Env::new();
    let other = hash("another operator");
    expect_ok(env.propose_project(other));
    assert!(!env.is_project_allowed(other));
    env.skip(TIMELOCK);
    assert!(env.is_project_allowed(other));
    expect_ok(env.revoke_project(other));
    assert!(!env.is_project_allowed(other));
}

#[test]
fn only_the_admin_changes_the_allowlist() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    let me = alice.pubkey();
    let driver = env.final_program();
    let checker = env.create_checker(me);
    let attempts = vec![
        env.ix_propose_image(&me, hash("mine"), ImageKind::Executor),
        env.ix_revoke_image(&me, builder_image()),
        env.ix_propose_project(&me, hash("mine")),
        env.ix_revoke_project(&me, project()),
        env.ix_propose(&me, ApprovalKind::Driver, name("local"), driver, None),
        env.ix_withdraw(&me, ApprovalKind::Driver, name("local")),
        env.ix_revoke(&me, ApprovalKind::Driver, name("local"), env.driver),
        env.ix_propose(
            &me,
            ApprovalKind::Bridge,
            name("local-bridge"),
            driver,
            None,
        ),
        env.ix_revoke(&me, ApprovalKind::Bridge, name("local-bridge"), env.bridge),
        env.ix_propose_verifier(&me, shonin_dev_verifier::ID, None, checker),
        env.ix_retire_verifier(&me, env.checker),
        env.ix_set_timelock(&me, 0),
    ];
    for ix in attempts {
        expect_code(
            env.send(vec![ix], &[&alice]),
            registry_error(RegistryError::NotAdmin),
        );
    }
    assert!(env.is_image_allowed(builder_image()));
    assert!(env.is_project_allowed(project()));
    assert!(env.is_verifier_allowed(&env.checker));
    assert_eq!(env.trading_driver(name("local")), env.driver);
    assert_eq!(env.timelock(), TIMELOCK);
}

#[test]
fn an_image_must_name_its_kind() {
    let mut env = Env::new();
    expect_code(
        env.propose_image(hash("mistyped kind"), ImageKind::None),
        registry_error(RegistryError::NoKind),
    );
}

#[test]
fn an_image_is_proposed_once() {
    let mut env = Env::new();
    expect_code(
        env.propose_image(builder_image(), ImageKind::Builder),
        registry_error(RegistryError::AlreadyProposed),
    );
}

/// A revocation of a project never proposed revoked nothing: it can still be
/// proposed, and counts once its timelock passes; it can't be proposed twice.
#[test]
fn a_project_revoked_before_it_was_proposed_can_still_be_proposed() {
    let mut env = Env::new();
    let project = hash("future operator");
    expect_ok(env.revoke_project(project));
    expect_ok(env.propose_project(project));
    env.skip(TIMELOCK);
    assert!(
        env.is_project_allowed(project),
        "the project is blocked for good"
    );
    expect_code(
        env.propose_project(project),
        registry_error(RegistryError::AlreadyProposed),
    );
}

#[test]
fn an_image_revoked_before_it_was_proposed_can_still_be_proposed() {
    let mut env = Env::new();
    let image = hash("future image");
    expect_ok(env.revoke_image(image));
    expect_ok(env.propose_image(image, ImageKind::Executor));
    env.skip(TIMELOCK);
    assert!(env.is_image_allowed(image), "the image is blocked for good");
}

/// The re-proposal path that clears a stale revocation only reaches ids never
/// proposed: a revoked image or project cannot be re-proposed, and so cannot
/// come back early.
#[test]
fn revoked_images_and_projects_cannot_be_reproposed() {
    let mut env = Env::new();
    expect_ok(env.revoke_image(builder_image()));
    expect_code(
        env.propose_image(builder_image(), ImageKind::Builder),
        registry_error(RegistryError::AlreadyProposed),
    );
    expect_ok(env.revoke_project(project()));
    expect_code(
        env.propose_project(project()),
        registry_error(RegistryError::AlreadyProposed),
    );
    assert!(!env.is_blessed_as(&env.builder.pubkey(), ImageKind::Builder));
}

// The timelock ----------------------------------------------------------------

#[test]
fn raising_the_timelock_applies_at_once() {
    let mut env = Env::new();
    expect_ok(env.set_timelock(TIMELOCK * 2));
    assert_eq!(env.timelock(), TIMELOCK * 2);
    assert_eq!(env.pending_timelock().0, 0);
}

#[test]
fn lowering_the_timelock_waits_out_the_current_one() {
    let mut env = Env::new();
    expect_ok(env.set_timelock(HOUR));
    assert_eq!(env.timelock(), TIMELOCK);
    assert_eq!(env.pending_timelock(), (HOUR, env.now() + TIMELOCK));

    // Anything proposed meanwhile still waits the old, longer timelock.
    let image = hash("slipped in");
    expect_ok(env.propose_image(image, ImageKind::Executor));
    env.skip(TIMELOCK - 1);
    assert_eq!(env.timelock(), TIMELOCK);
    assert!(!env.is_image_allowed(image));
    env.skip(1);
    assert_eq!(env.timelock(), HOUR);
    assert!(env.is_image_allowed(image));
}

#[test]
fn a_raise_cancels_a_pending_lowering() {
    let mut env = Env::new();
    expect_ok(env.set_timelock(HOUR));
    expect_ok(env.set_timelock(TIMELOCK));
    env.skip(TIMELOCK);
    assert_eq!(env.timelock(), TIMELOCK);
}

/// Two lowerings in a row: the second discards the first and waits the
/// timelock in force when it was asked for, so the first never lands early.
#[test]
fn a_second_lowering_replaces_the_first_and_waits_the_full_current_timelock() {
    let mut env = Env::new();
    let t0 = env.now();
    expect_ok(env.set_timelock(HOUR)); // effective t0 + 2 days
    env.skip(DAY);
    expect_ok(env.set_timelock(0)); // effective t0 + 1 day + 2 days
    env.skip(DAY); // t0 + 2 days: the first lowering would have landed now
    assert_eq!(env.timelock(), TIMELOCK, "the discarded lowering applied");
    assert_eq!(env.pending_timelock(), (0, t0 + 3 * DAY));
    env.skip(DAY);
    assert_eq!(env.timelock(), 0);
}

/// Without a ceiling, a value near the top would overflow every proposal and
/// every attempt to lower it again, so one admin transaction could stop
/// anything ever being added to the registry.
#[test]
fn a_timelock_near_the_top_cannot_be_set() {
    let mut env = Env::new();
    expect_code(
        env.set_timelock(u64::MAX),
        registry_error(RegistryError::TooLong),
    );
}

#[test]
fn the_timelock_has_a_ceiling_and_can_always_be_lowered_again() {
    let mut env = Env::new();
    let ceiling = shonin_common::MAX_TIMELOCK;
    expect_code(
        env.set_timelock(ceiling + 1),
        registry_error(RegistryError::TooLong),
    );
    expect_ok(env.set_timelock(ceiling));
    expect_ok(env.propose_image(hash("any image"), ImageKind::Executor));
    expect_ok(env.set_timelock(TIMELOCK));
    env.skip(ceiling);
    assert_eq!(env.timelock(), TIMELOCK);
}

// The floor, fixed at setup ---------------------------------------------------

/// The timelock a deployment starts with is held to the same ceiling as a
/// later one, so a setup with the wrong unit (milliseconds, say) can't ship a
/// registry whose every proposal overflows.
#[test]
fn the_initial_timelock_is_held_to_the_same_ceiling_as_set_timelock() {
    let mut env = Env::bare();
    expect_code(
        initialize(&mut env, u64::MAX - DAY, 0),
        registry_error(RegistryError::TooLong),
    );
    expect_code(
        initialize(&mut env, 0, shonin_common::MAX_TIMELOCK + 1),
        registry_error(RegistryError::TooLong),
    );
    expect_ok(initialize(&mut env, 365 * DAY, 0));
    expect_ok(env.propose_image(hash("any image"), ImageKind::Executor));
}

#[test]
fn the_timelock_never_goes_below_the_deployments_floor() {
    let mut env = Env::bare();
    expect_code(
        initialize(&mut env, DAY, 2 * DAY),
        registry_error(RegistryError::TooShort),
    );
    expect_ok(initialize(&mut env, 2 * DAY, 2 * DAY));
    assert_eq!(env.config().min_timelock, 2 * DAY);
    expect_code(env.set_timelock(0), registry_error(RegistryError::TooShort));
    expect_code(
        env.set_timelock(2 * DAY - 1),
        registry_error(RegistryError::TooShort),
    );
    expect_ok(env.set_timelock(3 * DAY));
    assert_eq!(env.timelock(), 3 * DAY);
    // A lowering to the floor itself is fine, and waits out the current one.
    expect_ok(env.set_timelock(2 * DAY));
    env.skip(3 * DAY);
    assert_eq!(env.timelock(), 2 * DAY);
}

#[test]
fn a_development_deployment_can_have_no_timelock() {
    let mut env = Env::bare();
    expect_ok(initialize(&mut env, 0, 0));
    expect_ok(env.propose_image(hash("any image"), ImageKind::Executor));
    assert!(env.is_image_allowed(hash("any image")));
}

/// The cluster a deployment serves is fixed at setup, as the value its leaves
/// and digests carry, and a development build never claims mainnet.
#[test]
fn the_cluster_is_fixed_at_setup_and_a_development_build_never_claims_mainnet() {
    let mut env = Env::bare();
    let deployer = env.deployer.insecure_clone();
    let ix = env.ix_initialize_registry(
        &deployer.pubkey(),
        InitializeArgs {
            admin: env.admin.pubkey(),
            cluster_name: shonin_common::MAINNET_CLUSTER.to_string(),
            timelock: TIMELOCK,
            min_timelock: TIMELOCK,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        env.checker,
    );
    expect_code(
        env.send(vec![ix], &[&deployer]),
        registry_error(RegistryError::WrongCluster),
    );
    expect_ok(initialize(&mut env, TIMELOCK, 0));
    assert_eq!(env.config().cluster, cluster_value("localnet"));
    assert_eq!(env.config().cluster, keccak(&[b"solana:localnet"]));
}
