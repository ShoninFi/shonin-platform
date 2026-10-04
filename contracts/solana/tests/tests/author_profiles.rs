//! What an author says about themselves: nothing checked and nothing depends
//! on it, but a handle is one author's at a time, the one they leave is free
//! for someone else, and each author pays for their own profile's room.

use anchor_lang::prelude::Pubkey;
use shonin_profiles::ProfilesError;
use shonin_solana_tests::*;
use solana_signer::Signer;

fn links(n: usize) -> (Vec<String>, Vec<String>) {
    (
        (0..n).map(|i| format!("Link {i}")).collect(),
        (0..n)
            .map(|i| format!("https://example.invalid/{i}"))
            .collect(),
    )
}

#[test]
fn a_handle_belongs_to_one_author() {
    let mut env = Env::new();
    let (author, alice) = (env.author.insecure_clone(), env.alice.insecure_clone());
    let labels = vec!["Notes".to_string()];
    let urls = vec!["https://example.invalid/notes".to_string()];
    expect_ok(env.set_profile(
        &author,
        "carry",
        "I trade basis.",
        labels.clone(),
        urls.clone(),
    ));
    assert_eq!(env.author_of_handle("carry"), author.pubkey());
    let profile = env.profile_of(&author.pubkey()).unwrap();
    assert_eq!(profile.link_urls[0], "https://example.invalid/notes");
    assert_eq!(profile.blurb, "I trade basis.");
    assert_eq!(profile.handle, "carry");

    expect_code(
        env.set_profile(&alice, "carry", "mine now", labels, urls),
        profiles_error(ProfilesError::HandleTaken),
    );
    assert_eq!(env.author_of_handle("carry"), author.pubkey());
}

#[test]
fn a_handle_left_behind_is_free_for_someone_else() {
    let mut env = Env::new();
    let (author, alice) = (env.author.insecure_clone(), env.alice.insecure_clone());
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    expect_ok(env.set_profile(&author, "basis", "", vec![], vec![]));
    assert_eq!(env.author_of_handle("basis"), author.pubkey());
    assert_eq!(
        env.author_of_handle("carry"),
        Pubkey::default(),
        "a handle left behind stayed taken"
    );
    expect_ok(env.set_profile(&alice, "carry", "", vec![], vec![]));
    assert_eq!(env.author_of_handle("carry"), alice.pubkey());
}

#[test]
fn setting_the_same_handle_again_keeps_it() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    expect_ok(env.set_profile(&author, "carry", "first", vec![], vec![]));
    expect_ok(env.set_profile(&author, "carry", "second", vec![], vec![]));
    assert_eq!(env.author_of_handle("carry"), author.pubkey());
    assert_eq!(env.profile_of(&author.pubkey()).unwrap().blurb, "second");
}

#[test]
fn a_profile_needs_a_handle() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    expect_code(
        env.set_profile(&author, "", "", vec![], vec![]),
        profiles_error(ProfilesError::EmptyHandle),
    );
}

#[test]
fn every_link_has_a_label_and_a_url() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let (labels, _) = links(2);
    let (_, urls) = links(1);
    expect_code(
        env.set_profile(&author, "carry", "", labels, urls),
        profiles_error(ProfilesError::MismatchedLinks),
    );
}

#[test]
fn the_join_date_is_set_once() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    let joined = env.now();
    env.skip(DAY);
    expect_ok(env.set_profile(&author, "basis", "", vec![], vec![]));
    assert_eq!(env.profile_of(&author.pubkey()).unwrap().joined_at, joined);
}

/// Each author pays for their own profile's room, and is refunded what a
/// shorter profile no longer needs.
#[test]
fn a_shorter_profile_refunds_its_room_to_the_author() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let (labels, urls) = links(5);
    expect_ok(env.set_profile(&author, "carry", &"x".repeat(400), labels, urls));
    let address = profile_address(&author.pubkey());
    let long = env.lamports(&address);
    let before = env.lamports(&author.pubkey());
    expect_ok(env.set_profile(&author, "carry", "short", vec![], vec![]));
    let short = env.lamports(&address);
    assert!(short < long);
    assert_eq!(
        short,
        env.svm
            .minimum_balance_for_rent_exemption(env.svm.get_account(&address).unwrap().data.len())
    );
    // The author got back the room, less the transaction's fee.
    assert!(env.lamports(&author.pubkey()) + 10_000 > before + (long - short));
    let profile = env.profile_of(&author.pubkey()).unwrap();
    assert_eq!(profile.blurb, "short");
    assert!(profile.link_labels.is_empty());
}

/// The handle being left has to be released, by its own record.
#[test]
fn leaving_a_handle_needs_its_own_record() {
    let mut env = Env::new();
    let (author, alice) = (env.author.insecure_clone(), env.alice.insecure_clone());
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    expect_ok(env.set_profile(&alice, "basis", "", vec![], vec![]));
    let mut ix = env.ix_set_profile(&author.pubkey(), "momentum", "", vec![], vec![]);
    // Leaving "carry", but naming alice's "basis" as the record to release.
    ix.accounts[3].pubkey = shonin_profiles::handle_address("basis");
    expect_code(
        env.send(vec![ix.clone()], &[&author]),
        profiles_error(ProfilesError::OldHandleMissing),
    );
    // Or none at all.
    ix.accounts[3].pubkey = shonin_profiles::ID;
    ix.accounts[3].is_writable = false;
    expect_code(
        env.send(vec![ix], &[&author]),
        profiles_error(ProfilesError::OldHandleMissing),
    );
    assert_eq!(env.author_of_handle("basis"), alice.pubkey());
    assert_eq!(env.author_of_handle("carry"), author.pubkey());
}

/// The handle's record is named by the handle's hash, which must be its own.
#[test]
fn a_handle_is_recorded_under_its_own_hash() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let mut ix = env.ix_set_profile(&author.pubkey(), "carry", "", vec![], vec![]);
    // The record of another handle, with that handle's hash in the data.
    use anchor_lang::InstructionData;
    ix.accounts[2].pubkey = shonin_profiles::handle_address("basis");
    ix.data = shonin_profiles::instruction::SetProfile {
        handle: "carry".to_string(),
        handle_hash: hash("basis"),
        blurb: String::new(),
        link_labels: vec![],
        link_urls: vec![],
    }
    .data();
    expect_code(
        env.send(vec![ix], &[&author]),
        profiles_error(ProfilesError::WrongHandleHash),
    );
}

/// A profile is the author's own: nobody sets it for them.
#[test]
fn nobody_sets_another_authors_profile() {
    let mut env = Env::new();
    let (author, alice) = (env.author.insecure_clone(), env.alice.insecure_clone());
    let mut ix = env.ix_set_profile(&alice.pubkey(), "carry", "", vec![], vec![]);
    ix.accounts[1].pubkey = profile_address(&author.pubkey());
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
    assert!(env.profile_of(&author.pubkey()).is_none());
}
