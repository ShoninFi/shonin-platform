//! Every instruction of the registry, publishing, prices and profiles, sent
//! by anyone with any inputs: each is refused exactly when, and for exactly
//! the reason, the rules say, and what it changes is what the rules allow.
//! Cases are drawn from a seeded generator; `SHONIN_PROPERTY_RUNS` and
//! `SHONIN_PROPERTY_SEED` run them deeper or differently.

use anchor_lang::prelude::Pubkey;
use shonin_publishing::error::PublishingError;
use shonin_registry::error::RegistryError;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// A fresh fixture every few cases, so cases also meet each other's state.
const CASES_PER_FIXTURE: u64 = 4;

fn pick_caller(env: &Env, rng: &mut Rng) -> Keypair {
    match rng.below(4) {
        0 => env.admin.insecure_clone(),
        1 => env.alice.insecure_clone(),
        2 => env.builder.insecure_clone(),
        _ => Keypair::new(),
    }
}

fn fund(env: &mut Env, key: &Keypair) {
    env.svm.airdrop(&key.pubkey(), 1_000_000_000_000).unwrap();
}

/// The outcome, as the code it ended with (zero for success).
fn outcome(result: &TxResult) -> u32 {
    match result {
        Ok(_) => 0,
        Err(_) => code_of(result).unwrap_or(u32::MAX),
    }
}

#[track_caller]
fn expect_outcome(result: &TxResult, want: u32, what: &str) {
    let got = outcome(result);
    if got != want {
        let logs = match result {
            Err(failed) => failed.meta.logs.join("\n"),
            Ok(meta) => meta.logs.join("\n"),
        };
        panic!("{what}: expected {want}, got {got}\n{logs}");
    }
}

// The registry ---------------------------------------------------------------------

/// Only the admin sets the timelock, never past a year and never under the
/// floor fixed at setup; a raise applies at once, a cut waits out the
/// timelock it cuts.
#[test]
fn the_timelock_moves_only_as_the_rules_allow() {
    let mut rng = Rng::new(seed());
    for _ in 0..runs(24) {
        let mut env = Env::bare();
        let floor = rng.below(30 * DAY + 1);
        let deployer = env.deployer.insecure_clone();
        let ix = env.ix_initialize_registry(
            &deployer.pubkey(),
            shonin_registry::InitializeArgs {
                admin: env.admin.pubkey(),
                cluster_name: CLUSTER.to_string(),
                timelock: floor + DAY,
                min_timelock: floor,
                blessing_ttl: BLESSING_TTL,
            },
            shonin_dev_verifier::ID,
            None,
            env.checker,
        );
        expect_ok(env.send(vec![ix], &[&deployer]));
        let caller = pick_caller(&env, &mut rng);
        fund(&mut env, &caller);
        let value = rng.below(400 * DAY + 1);
        let current = env.timelock();
        let want = if caller.pubkey() != env.admin.pubkey() {
            registry_error(RegistryError::NotAdmin)
        } else if value > shonin_common::MAX_TIMELOCK {
            registry_error(RegistryError::TooLong)
        } else if value < floor {
            registry_error(RegistryError::TooShort)
        } else {
            0
        };
        let ix = env.ix_set_timelock(&caller.pubkey(), value);
        let result = env.send(vec![ix], &[&caller]);
        expect_outcome(&result, want, "set_timelock");
        if result.is_ok() && value >= current {
            assert_eq!(env.timelock(), value, "a raise didn't apply at once");
        } else if result.is_ok() {
            assert_eq!(env.timelock(), current, "a cut applied at once");
            env.skip(current);
            assert_eq!(env.timelock(), value);
        }
        if rng.bool() {
            env.skip(400 * DAY);
        }
        assert!(env.timelock() >= floor, "the timelock went under its floor");
    }
}

/// Every allowlist change is the admin's alone. A proposal never counts
/// before its timelock has passed; a revocation counts at once.
#[test]
fn every_allowlist_change_is_the_admins_and_only_a_removal_is_immediate() {
    let mut rng = Rng::new(seed() + 1);
    let mut env = Env::new();
    for case in 0..runs(48) {
        if case % (CASES_PER_FIXTURE * 3) == 0 {
            env = Env::new();
        }
        let caller = pick_caller(&env, &mut rng);
        fund(&mut env, &caller);
        let which = rng.below(12);
        let name_bytes = if rng.below(3) == 0 {
            match which {
                0 | 1 => executor_image(),
                2 | 3 => project(),
                _ => name("local"),
            }
        } else {
            rng.bytes()
        };
        let implementation = match rng.below(4) {
            0 => env.driver,
            1 => env.final_program(),
            2 => Pubkey::default(),
            _ => Pubkey::new_unique(),
        };
        let checker = if rng.bool() {
            env.checker
        } else {
            env.create_checker(Keypair::new().pubkey())
        };
        let kind = match rng.below(5) {
            0 => ImageKind::None,
            1 => ImageKind::Executor,
            2 => ImageKind::Unwinder,
            3 => ImageKind::Builder,
            _ => ImageKind::Reviewer,
        };
        let me = caller.pubkey();
        let ix = match which {
            0 => env.ix_propose_image(&me, name_bytes, kind),
            1 => env.ix_revoke_image(&me, name_bytes),
            2 => env.ix_propose_project(&me, name_bytes),
            3 => env.ix_revoke_project(&me, name_bytes),
            4 => env.ix_propose_verifier(&me, shonin_dev_verifier::ID, None, checker),
            5 => env.ix_retire_verifier(&me, checker),
            6 => env.ix_propose(&me, ApprovalKind::Driver, name_bytes, implementation, None),
            7 => env.ix_revoke(&me, ApprovalKind::Driver, name_bytes, implementation),
            8 => env.ix_propose(&me, ApprovalKind::Bridge, name_bytes, implementation, None),
            9 => env.ix_revoke(&me, ApprovalKind::Bridge, name_bytes, implementation),
            10 => env.ix_withdraw(&me, ApprovalKind::Driver, name_bytes),
            _ => env.ix_withdraw(&me, ApprovalKind::Bridge, name_bytes),
        };
        let image_before = env.is_image_allowed(name_bytes);
        let project_before = env.is_project_allowed(name_bytes);
        let verifier_before = env.is_verifier_allowed(&checker);
        let trading_before = env.trading_driver(name_bytes);
        let bridge_before = env.bridge_of(name_bytes);
        let result = env.send(vec![ix], &[&caller]);
        if me != env.admin.pubkey() {
            expect_outcome(
                &result,
                registry_error(RegistryError::NotAdmin),
                "an allowlist change by anyone else",
            );
            continue;
        }
        let allowed_refusals = [
            RegistryError::AlreadyProposed,
            RegistryError::NoKind,
            RegistryError::NoImplementation,
            RegistryError::NotFinal,
            RegistryError::TooManyVerifiers,
        ]
        .map(registry_error);
        let got = outcome(&result);
        assert!(
            got == 0 || allowed_refusals.contains(&got),
            "an allowlist change refused for no rule: {got} (op {which})"
        );
        if got != 0 {
            continue;
        }
        match which {
            0 => assert_eq!(
                env.is_image_allowed(name_bytes),
                image_before,
                "an image counted before its timelock"
            ),
            1 => assert!(
                !env.is_image_allowed(name_bytes),
                "a revoked image still counts"
            ),
            2 => assert_eq!(
                env.is_project_allowed(name_bytes),
                project_before,
                "a project counted early"
            ),
            3 => assert!(
                !env.is_project_allowed(name_bytes),
                "a revoked project still counts"
            ),
            4 => assert_eq!(
                env.is_verifier_allowed(&checker),
                verifier_before,
                "a checker counted early"
            ),
            5 => assert!(
                !env.is_verifier_allowed(&checker),
                "a retired checker still counts"
            ),
            6 => assert_eq!(
                env.trading_driver(name_bytes),
                trading_before,
                "a driver counted early"
            ),
            7 => {
                assert!(
                    env.is_revoked(ApprovalKind::Driver, name_bytes, implementation),
                    "a revoked driver still trades"
                );
                assert_ne!(
                    env.trading_driver(name_bytes),
                    implementation,
                    "a revoked driver is still pinned"
                );
            }
            8 => assert_eq!(
                env.bridge_of(name_bytes),
                bridge_before,
                "a bridge counted early"
            ),
            9 => assert!(
                env.is_revoked(ApprovalKind::Bridge, name_bytes, implementation),
                "a revoked bridge still carries"
            ),
            10 => assert_eq!(
                env.trading_driver(name_bytes),
                trading_before,
                "a withdrawal changed the driver"
            ),
            _ => assert_eq!(
                env.bridge_of(name_bytes),
                bridge_before,
                "a withdrawal changed the bridge"
            ),
        }
    }
}

/// Anyone may bring a proof; a key is blessed only for an allowed image and
/// project, for the work its kind does, for what the instance named, within
/// its time to live; and a blessing only ever moves later.
#[test]
fn a_blessing_is_granted_exactly_as_the_rules_say_and_only_moves_later() {
    let mut rng = Rng::new(seed() + 2);
    let mut env = Env::new();
    let mut granted = 0;
    for case in 0..runs(64) {
        if case % (CASES_PER_FIXTURE * 4) == 0 {
            env = Env::new();
        }
        let caller = pick_caller(&env, &mut rng);
        fund(&mut env, &caller);
        let image = match rng.below(4) {
            0 => executor_image(),
            1 => builder_image(),
            2 => reviewer_image(),
            _ => rng.bytes(),
        };
        let signer = match rng.below(6) {
            0 => env.builder.pubkey(),
            1 => env.reviewer.pubkey(),
            2 => Pubkey::default(),
            _ => Keypair::new().pubkey(),
        };
        let kind_wants_vault = image == executor_image();
        // Usually the target its kind serves.
        let target = if rng.mostly() {
            if kind_wants_vault {
                1
            } else {
                0
            }
        } else {
            rng.below(3)
        };
        let cluster = env.cluster();
        let (vault_chain, vault, strategy_hash) = match target {
            0 => ([0; 32], [0; 32], [0; 32]),
            1 => (cluster, env.alice.pubkey().to_bytes(), hash("a bundle")),
            _ => (cluster, [0; 32], hash("a bundle")),
        };
        let project_ok = rng.mostly();
        let honest_root = rng.mostly();
        let attested = rng.mostly();
        let now = env.now();
        let issued_at = if rng.mostly() {
            now.saturating_sub(rng.below(BLESSING_TTL))
        } else {
            (now + HOUR).saturating_sub(rng.below(101 * HOUR))
        };
        let leaf = blessing_leaf(
            &vault_chain,
            &vault,
            &strategy_hash,
            &cluster,
            &signer.to_bytes(),
        );
        let journal = Journal {
            image_digest: image,
            project_id: if project_ok {
                project()
            } else {
                hash("another project")
            },
            signer,
            vaults_root: if honest_root { leaf } else { keccak(&[&leaf]) },
            issued_at,
        };

        let want = if !attested {
            registry_error(RegistryError::ProofRejected)
        } else if !env.is_image_allowed(image) {
            registry_error(RegistryError::ImageNotAllowed)
        } else if !project_ok {
            registry_error(RegistryError::ProjectNotAllowed)
        } else if signer == Pubkey::default() {
            registry_error(RegistryError::NoSigner)
        } else {
            let kind = env.image_of(image).unwrap().kind;
            let one_vault = kind == ImageKind::Executor || kind == ImageKind::Unwinder;
            let known = env.key_info(&signer);
            if if one_vault { target != 1 } else { target != 0 } {
                registry_error(RegistryError::WrongKindForVault)
            } else if !honest_root {
                registry_error(RegistryError::NotAmongTheVaults)
            } else if issued_at > now || issued_at + BLESSING_TTL <= now {
                registry_error(RegistryError::Stale)
            } else if known.is_some_and(|key| {
                key.kind != ImageKind::None
                    && (key.image_digest != image || key.project_id != project())
            }) {
                registry_error(RegistryError::NotThisInstance)
            } else {
                0
            }
        };
        let before = env.blessed_until(&signer, vault_chain, vault, strategy_hash);
        let attestor = if attested {
            env.attestor.insecure_clone()
        } else {
            Keypair::new()
        };
        let result = env.bless_by(
            &caller,
            &attestor,
            &journal,
            vault_chain,
            vault,
            strategy_hash,
            vec![],
        );
        expect_outcome(&result, want, "bless");
        let after = env.blessed_until(&signer, vault_chain, vault, strategy_hash);
        assert!(after >= before, "a blessing moved earlier");
        if result.is_ok() {
            granted += 1;
            assert_eq!(after, (issued_at + BLESSING_TTL).max(before));
            assert_eq!(
                env.key_info(&signer).unwrap().kind,
                env.image_of(image).unwrap().kind
            );
        }
        if rng.below(4) == 0 {
            env.skip(rng.below(BLESSING_TTL));
        }
    }
    assert!(granted > 0, "no case reached a blessing");
}

// Publishing -------------------------------------------------------------------------

/// Anyone may send a registration; it sticks only for the author who paid for
/// the id, once, with something built, a sorted list, a blessed builder's
/// signature and a blessed reviewer's go-ahead.
#[test]
fn a_registration_sticks_exactly_as_the_rules_say() {
    let mut rng = Rng::new(seed() + 3);
    let mut env = Env::new();
    let mut registered = 0;
    for case in 0..runs(48) {
        if case % (CASES_PER_FIXTURE * 3) == 0 {
            env = Env::new();
        }
        let author = env.author.insecure_clone();
        let id = match rng.below(5) {
            0 => env.ready().0,
            1 => 1000 + rng.below(1000),
            _ => env.submit_as(&author),
        };
        let who = match rng.below(5) {
            0 => Keypair::new().pubkey(),
            1 => Pubkey::default(),
            _ => author.pubkey(),
        };
        let built = rng.mostly();
        let sorted = rng.mostly();
        let builder_signs = rng.mostly();
        let reviewer_clears = rng.mostly();
        let mut reg = Registration::new(id, who, if built { hash("bundle") } else { [0; 32] });
        if !sorted {
            reg.lists.restricted.reverse();
        }
        let want = if env.strategy_of(id).is_some() {
            publishing_error(PublishingError::AlreadyRegistered)
        } else if !built {
            publishing_error(PublishingError::NothingBuilt)
        } else if who == Pubkey::default() || env.submission_of(id).map(|s| s.author) != Some(who) {
            publishing_error(PublishingError::NotSubmittedByTheAuthor)
        } else if !sorted {
            publishing_error(PublishingError::NotSorted)
        } else if !builder_signs {
            publishing_error(PublishingError::NotTheBuilder)
        } else if !reviewer_clears {
            publishing_error(PublishingError::NotCleared)
        } else {
            0
        };
        let builder = if builder_signs {
            env.builder.insecure_clone()
        } else {
            Keypair::new()
        };
        let reviewer = if reviewer_clears {
            env.reviewer.insecure_clone()
        } else {
            Keypair::new()
        };
        let bundle_before = env.strategy_of(id).map(|s| s.bundle_hash);
        let result = env.register_with(&reg, &builder, &reviewer);
        expect_outcome(&result, want, "register");
        if result.is_ok() {
            registered += 1;
            let strategy = env.strategy_of(id).unwrap();
            assert_eq!(strategy.author, who);
            assert_eq!(strategy.bundle_hash, reg.bundle_hash);
        } else {
            assert_eq!(
                env.strategy_of(id).map(|s| s.bundle_hash),
                bundle_before,
                "a refused registration changed the record"
            );
        }
    }
    assert!(registered > 0, "no case reached a registration");
}

fn pick_strategy(env: &mut Env, rng: &mut Rng) -> u64 {
    let author = env.author.insecure_clone();
    match rng.below(6) {
        0 | 1 => {
            let id = env.submit_as(&author);
            env.register(id, author.pubkey(), keccak(&[b"bundle", &id.to_le_bytes()]));
            id
        }
        2 => env.ready().0,
        3 | 4 => env.submit_as(&author),
        _ => 1000 + rng.below(1000),
    }
}

fn a_review(
    env: &Env,
    id: u64,
    right_bundle: bool,
    right_source: bool,
    rating: u64,
) -> ReviewInput {
    let bundle = env.strategy_of(id).map_or([0; 32], |s| s.bundle_hash);
    let mut review = review_for(
        if right_bundle {
            bundle
        } else {
            hash("another bundle")
        },
        1,
    );
    if !right_source {
        review.source_hash = hash("another source");
    }
    let area = (rating % 6) as usize;
    review.ratings[area] = match rating % 10 {
        0 => 0,
        9 => 5,
        r => r as u8 % 4 + 1,
    };
    review
}

fn bad_rating(rating: u64) -> bool {
    rating.is_multiple_of(10) || rating % 10 == 9
}

fn pick_reviewer(env: &Env, rng: &mut Rng) -> Keypair {
    match rng.below(3) {
        0 | 1 => env.reviewer.insecure_clone(),
        _ => env.alice.insecure_clone(),
    }
}

#[test]
fn a_review_is_recorded_exactly_as_the_rules_say() {
    let mut rng = Rng::new(seed() + 4);
    let mut env = Env::new();
    let mut reviewed = 0;
    for case in 0..runs(48) {
        if case % (CASES_PER_FIXTURE * 3) == 0 {
            env = Env::new();
        }
        let id = pick_strategy(&mut env, &mut rng);
        let caller = pick_reviewer(&env, &mut rng);
        let (right_bundle, right_source, rating) = (rng.mostly(), rng.mostly(), rng.word());
        let review = a_review(&env, id, right_bundle, right_source, rating);
        let stored = env.review_of(id);
        let want = if !env.is_blessed_as(&caller.pubkey(), ImageKind::Reviewer) {
            publishing_error(PublishingError::NotTheReviewer)
        } else if stored.as_ref().is_some_and(|r| r.reviewed_at != 0) {
            publishing_error(PublishingError::AlreadyReviewed)
        } else if bad_rating(rating) {
            publishing_error(PublishingError::BadRating)
        } else if env.strategy_of(id).is_none() {
            publishing_error(PublishingError::NotRegistered)
        } else if !right_bundle || !right_source {
            publishing_error(PublishingError::WrongBundle)
        } else {
            0
        };
        let result = env.submit_review(&caller, id, review);
        expect_outcome(&result, want, "submit_review");
        if result.is_ok() {
            reviewed += 1;
            assert!(env.has_review(id));
        } else {
            assert_eq!(
                env.review_of(id).map(|r| r.reviewed_at),
                stored.map(|r| r.reviewed_at),
                "a refused review changed the record"
            );
        }
    }
    assert!(reviewed > 0, "no case reached a review");
}

#[test]
fn a_refusal_is_recorded_exactly_as_the_rules_say() {
    let mut rng = Rng::new(seed() + 5);
    let mut env = Env::new();
    let mut refused = 0;
    for case in 0..runs(48) {
        if case % (CASES_PER_FIXTURE * 3) == 0 {
            env = Env::new();
        }
        let id = pick_strategy(&mut env, &mut rng);
        let caller = pick_reviewer(&env, &mut rng);
        let (no_bundle, has_source, rating) = (rng.mostly(), rng.mostly(), rng.word());
        let mut review = a_review(&env, id, true, true, rating);
        if no_bundle {
            review.bundle_hash = [0; 32];
        }
        if !has_source {
            review.source_hash = [0; 32];
        }
        let stored = env.review_of(id);
        let want = if !env.is_blessed_as(&caller.pubkey(), ImageKind::Reviewer) {
            publishing_error(PublishingError::NotTheReviewer)
        } else if stored.as_ref().is_some_and(|r| r.reviewed_at != 0) {
            publishing_error(PublishingError::AlreadyReviewed)
        } else if bad_rating(rating) {
            publishing_error(PublishingError::BadRating)
        } else if env.submission_of(id).is_none() {
            publishing_error(PublishingError::NotSubmitted)
        } else if env.strategy_of(id).is_some() {
            publishing_error(PublishingError::AlreadyRegistered)
        } else if review.bundle_hash != [0; 32] {
            publishing_error(PublishingError::WrongBundle)
        } else if !has_source {
            publishing_error(PublishingError::NoSource)
        } else {
            0
        };
        let result = env.submit_refusal(&caller, id, review);
        expect_outcome(&result, want, "submit_refusal");
        if result.is_ok() {
            refused += 1;
            assert!(env.review_of(id).unwrap().refused);
            assert!(!env.has_review(id), "a refused strategy reads as reviewed");
        }
    }
    assert!(refused > 0, "no case reached a refusal");
}

/// Anyone pays the fee for the next id; the fee reaches the platform and
/// nobody else.
#[test]
fn a_submission_takes_the_fee_to_the_platform_and_the_next_id() {
    let mut rng = Rng::new(seed() + 6);
    let mut env = Env::new();
    for _ in 0..runs(32) {
        let caller = pick_caller(&env, &mut rng);
        fund(&mut env, &caller);
        let funded = rng.bool();
        if funded {
            env.give(&caller.pubkey(), PUBLISH_FEE);
        }
        let count = env.publishing_config().count;
        let platform_before = env.usd_balance(&env.platform.pubkey());
        let caller_before = env.usd_balance(&caller.pubkey());
        let ix = env.ix_submit(&caller.pubkey(), rng.bytes());
        let result = env.send(vec![ix], &[&caller]);
        assert_eq!(result.is_ok(), caller_before >= PUBLISH_FEE, "submit");
        assert_eq!(
            env.publishing_config().count,
            if result.is_ok() { count + 1 } else { count }
        );
        if result.is_ok() {
            assert_eq!(
                env.usd_balance(&env.platform.pubkey()),
                platform_before + PUBLISH_FEE,
                "the fee didn't reach the platform"
            );
            assert_eq!(
                env.usd_balance(&caller.pubkey()),
                caller_before - PUBLISH_FEE
            );
            assert_eq!(
                env.submission_of(count + 1).unwrap().author,
                caller.pubkey()
            );
        } else {
            assert_eq!(env.usd_balance(&env.platform.pubkey()), platform_before);
        }
    }
}

// Prices and profiles -------------------------------------------------------------

/// Only the admin schedules prices, and a change only ever starts after the
/// notice: the price in force now never moves.
#[test]
fn a_price_change_is_the_admins_and_never_moves_the_price_in_force() {
    let mut rng = Rng::new(seed() + 7);
    let mut env = Env::new();
    for _ in 0..runs(32) {
        let caller = pick_caller(&env, &mut rng);
        fund(&mut env, &caller);
        env.skip(rng.below(60 * DAY));
        let current = env.current_prices();
        let length = env.history().len();
        let next = UnitPrices {
            vcpu_hour: rng.word() as u128,
            gib_hour: rng.word() as u128,
            gb_egress: (rng.word() as u128) << 64,
            inference_second: rng.word() as u128,
        };
        let ix = env.ix_schedule(&caller.pubkey(), next);
        let result = env.send(vec![ix], &[&caller]);
        let want = if caller.pubkey() == env.admin.pubkey() {
            0
        } else {
            prices_error(shonin_prices::PricesError::NotAdmin)
        };
        expect_outcome(&result, want, "schedule");
        assert_eq!(
            env.current_prices(),
            current,
            "the price in force moved without notice"
        );
        let history = env.history();
        assert_eq!(
            history.len(),
            if result.is_ok() { length + 1 } else { length }
        );
        if result.is_ok() {
            assert_eq!(history[length].effective_from, env.now() + PRICE_NOTICE);
            assert_eq!(history[length].prices, next);
        }
    }
}

/// Anyone sets their own profile; a handle is one person's at a time, and
/// the handle they leave is free for someone else.
#[test]
fn a_handle_is_one_authors_at_a_time() {
    let mut rng = Rng::new(seed() + 8);
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    expect_ok(env.set_profile(&alice, "alice", "", vec![], vec![]));
    let stranger = Keypair::new();
    fund(&mut env, &stranger);
    for _ in 0..runs(48) {
        let caller = match rng.below(3) {
            0 => env.alice.insecure_clone(),
            1 => env.bob.insecure_clone(),
            _ => stranger.insecure_clone(),
        };
        let other = format!("handle-{}", rng.below(4));
        let handle = match rng.below(4) {
            0 => "alice".to_string(),
            1 => "bob".to_string(),
            2 => String::new(),
            _ => other,
        };
        let links = rng.below(3) as usize;
        let urls = if rng.bool() { links } else { (links + 1) % 3 };
        let labels: Vec<String> = (0..links).map(|i| format!("l{i}")).collect();
        let urls: Vec<String> = (0..urls).map(|i| format!("u{i}")).collect();
        let previous = env
            .profile_of(&caller.pubkey())
            .map(|p| p.handle)
            .unwrap_or_default();
        let owner = env.author_of_handle(&handle);
        let want = if handle.is_empty() {
            profiles_error(shonin_profiles::ProfilesError::EmptyHandle)
        } else if labels.len() != urls.len() {
            profiles_error(shonin_profiles::ProfilesError::MismatchedLinks)
        } else if owner != Pubkey::default() && owner != caller.pubkey() {
            profiles_error(shonin_profiles::ProfilesError::HandleTaken)
        } else {
            0
        };
        let result = env.set_profile(&caller, &handle, "", labels, urls);
        expect_outcome(&result, want, "set_profile");
        if result.is_ok() {
            assert_eq!(env.author_of_handle(&handle), caller.pubkey());
            if previous != handle && !previous.is_empty() {
                assert_eq!(
                    env.author_of_handle(&previous),
                    Pubkey::default(),
                    "a handle left behind stayed taken"
                );
            }
        } else if !handle.is_empty() {
            assert_eq!(
                env.author_of_handle(&handle),
                owner,
                "a refused profile moved a handle"
            );
        }
    }
}
