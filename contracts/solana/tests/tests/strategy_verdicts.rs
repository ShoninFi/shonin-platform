//! A review nobody wrote cannot be posted: one per strategy, ever, from a
//! blessed reviewer key, of the bundle and source the strategy's record
//! holds, every area rated, with the image and the time the registry itself
//! knows. A refusal is recorded against the source it read, and a refused
//! strategy never reads as reviewed.

use shonin_publishing::error::PublishingError;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// A strategy submitted and registered, but not reviewed.
fn unreviewed(env: &mut Env) -> (u64, [u8; 32]) {
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let bundle = keccak(&[b"bundle", &id.to_le_bytes()]);
    env.register(id, author.pubkey(), bundle);
    (id, bundle)
}

#[test]
fn only_a_blessed_reviewer_posts_a_review() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    let alice = env.alice.insecure_clone();
    expect_code(
        env.submit_review(&alice, id, review_for(bundle, 1)),
        publishing_error(PublishingError::NotTheReviewer),
    );
    // Nor a blessed builder: a key does only its own kind's job.
    let builder = env.builder.insecure_clone();
    expect_code(
        env.submit_review(&builder, id, review_for(bundle, 1)),
        publishing_error(PublishingError::NotTheReviewer),
    );

    let mut review = review_for(bundle, 1);
    review.ratings[1] = 3;
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_review(&reviewer, id, review));
    let stored = env.review_of(id).unwrap();
    assert!(!stored.refused);
    assert!(env.has_review(id));
    assert_eq!(stored.ratings[1], 3);
    assert_eq!(stored.ratings[0], 1);
    assert_eq!(stored.report_hash, hash("report"));
    assert_eq!(stored.finding_count, 2);
}

#[test]
fn a_review_cannot_follow_code_nobody_read() {
    let mut env = Env::new();
    let (id, _) = unreviewed(&mut env);
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.submit_review(&reviewer, id, review_for(hash("some other bundle"), 1)),
        publishing_error(PublishingError::WrongBundle),
    );
}

#[test]
fn a_review_names_the_source_the_record_holds() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    let mut review = review_for(bundle, 1);
    review.source_hash = hash("another source");
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.submit_review(&reviewer, id, review),
        publishing_error(PublishingError::WrongBundle),
    );
}

#[test]
fn a_strategy_not_registered_has_no_review() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.submit_review(&reviewer, id, review_for(hash("bundle"), 1)),
        publishing_error(PublishingError::NotRegistered),
    );
}

#[test]
fn a_strategy_is_reviewed_once() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_review(&reviewer, id, review_for(bundle, 1)));
    expect_code(
        env.submit_review(&reviewer, id, review_for(bundle, 2)),
        publishing_error(PublishingError::AlreadyReviewed),
    );
    // Nor refused after it.
    expect_code(
        env.submit_refusal(&reviewer, id, review_for([0; 32], 3)),
        publishing_error(PublishingError::AlreadyReviewed),
    );
    assert_eq!(env.review_of(id).unwrap().ratings, [1; 6]);
}

#[test]
fn every_area_carries_a_rating() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    let reviewer = env.reviewer.insecure_clone();
    let mut review = review_for(bundle, 1);
    review.ratings[5] = 0;
    expect_code(
        env.submit_review(&reviewer, id, review),
        publishing_error(PublishingError::BadRating),
    );
    review.ratings[5] = 5;
    expect_code(
        env.submit_review(&reviewer, id, review),
        publishing_error(PublishingError::BadRating),
    );
    review.ratings[5] = 4;
    expect_ok(env.submit_review(&reviewer, id, review));
}

/// The record says the image the registry knows the reviewer key by and the
/// cluster's time, not anything the key claims: the review it sends has no
/// room to claim either.
#[test]
fn a_review_records_the_image_the_registry_knows_the_reviewer_by() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    env.skip(HOUR);
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_review(&reviewer, id, review_for(bundle, 1)));
    let stored = env.review_of(id).unwrap();
    assert_eq!(stored.reviewer_image, reviewer_image());
    assert_eq!(stored.reviewed_at, env.now());
}

/// A reviewer that is no longer blessed posts nothing.
#[test]
fn a_reviewer_no_longer_blessed_posts_nothing() {
    let mut env = Env::new();
    let (id, bundle) = unreviewed(&mut env);
    expect_ok(env.revoke_project(project()));
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.submit_review(&reviewer, id, review_for(bundle, 1)),
        publishing_error(PublishingError::NotTheReviewer),
    );
}

// Refusals -------------------------------------------------------------------------

#[test]
fn a_refusal_is_recorded_against_the_source() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let mut refusal = review_for([0; 32], 4);
    refusal.ratings[0] = 3;
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_refusal(&reviewer, id, refusal));
    let stored = env.review_of(id).unwrap();
    assert!(stored.refused);
    assert!(!env.has_review(id));
    assert_eq!(stored.source_hash, hash("source"));
    assert_eq!(stored.reviewer_image, reviewer_image());
}

#[test]
fn a_refusal_is_only_for_a_submitted_strategy_not_yet_registered() {
    let mut env = Env::new();
    let reviewer = env.reviewer.insecure_clone();
    expect_code(
        env.submit_refusal(&reviewer, 42, review_for([0; 32], 1)),
        publishing_error(PublishingError::NotSubmitted),
    );
    let (id, _) = unreviewed(&mut env);
    expect_code(
        env.submit_refusal(&reviewer, id, review_for([0; 32], 1)),
        publishing_error(PublishingError::AlreadyRegistered),
    );
}

#[test]
fn a_refusal_names_the_source_it_read_and_no_bundle() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reviewer = env.reviewer.insecure_clone();
    let mut refusal = review_for([0; 32], 3);
    refusal.source_hash = [0; 32];
    expect_code(
        env.submit_refusal(&reviewer, id, refusal),
        publishing_error(PublishingError::NoSource),
    );
    expect_code(
        env.submit_refusal(&reviewer, id, review_for(hash("bundle"), 3)),
        publishing_error(PublishingError::WrongBundle),
    );
}

#[test]
fn only_a_blessed_reviewer_refuses() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    expect_code(
        env.submit_refusal(&stranger, id, review_for([0; 32], 3)),
        publishing_error(PublishingError::NotTheReviewer),
    );
}

/// A clearance exists (one reviewer instance cleared the source), the build
/// registers, and a second reviewer instance refuses before the review is
/// posted. The record stands, but it never has a review a vault can follow.
#[test]
fn a_refused_strategy_never_reads_as_reviewed_even_if_registered() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_refusal(&reviewer, id, review_for([0; 32], 3)));
    let bundle = keccak(&[b"bundle", &id.to_le_bytes()]);
    env.register(id, author.pubkey(), bundle);
    assert!(env.strategy_of(id).is_some());
    assert!(!env.has_review(id));
    expect_code(
        env.submit_review(&reviewer, id, review_for(bundle, 1)),
        publishing_error(PublishingError::AlreadyReviewed),
    );
}
