//! A key is blessed only for an allowed image and project, for the work its
//! kind does, for what the instance attested to before it saw the
//! transaction, on this cluster, within a TTL from the attestation. A
//! blessing only ever moves later, so nobody replaying a public seal can
//! extend one, shorten one, or move a key off the vault it serves.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AnchorDeserialize, Discriminator};
use base64::Engine;
use shonin_registry::error::RegistryError;
use shonin_registry::events::Blessed;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn vault() -> [u8; 32] {
    hash("vault")
}
fn bundle() -> [u8; 32] {
    hash("bundle")
}

/// The blessed expiries a transaction announced.
fn announced(meta: &litesvm::types::TransactionMetadata) -> Vec<u64> {
    meta.logs
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
        .filter(|bytes| bytes.starts_with(Blessed::DISCRIMINATOR))
        .map(|bytes| {
            Blessed::deserialize(&mut &bytes[Blessed::DISCRIMINATOR.len()..])
                .unwrap()
                .blessed_until
        })
        .collect()
}

// Refusals ---------------------------------------------------------------------

#[test]
fn a_proof_nobody_signed_is_refused() {
    let mut env = Env::new();
    let journal = env.journal_for(executor_image(), env.executor.pubkey(), [0; 32]);
    let payer = env.payer.insecure_clone();
    let ix = env.ix_bless(
        &payer.pubkey(),
        &journal,
        vec![],
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    expect_code(
        env.send(vec![ix], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
}

#[test]
fn a_proof_signed_by_any_other_key_is_refused() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    let (payer, stranger) = (env.payer.insecure_clone(), Keypair::new());
    expect_code(
        env.bless_by(
            &payer,
            &stranger,
            &journal,
            [0; 32],
            [0; 32],
            [0; 32],
            vec![],
        ),
        registry_error(RegistryError::ProofRejected),
    );
}

/// The seal binds the journal: the attestor's signature over one journal is
/// no proof of another that names a different key.
#[test]
fn a_seal_proves_only_the_journal_it_was_made_for() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let honest = env.journal_for(builder_image(), key, env.service_leaf(key));
    let attestor = env.attestor.insecure_clone();
    let (_, ed25519) = env.seal(&attestor, &honest);
    let mut swapped = honest;
    swapped.signer = env.alice.pubkey();
    let payer = env.payer.insecure_clone();
    let ix = env.ix_bless(
        &payer.pubkey(),
        &swapped,
        vec![],
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    expect_code(
        env.send(vec![ed25519, ix], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
}

#[test]
fn an_image_off_the_allowlist_cannot_bless() {
    let mut env = Env::new();
    let journal = env.journal_for(hash("unknown image"), env.executor.pubkey(), [0; 32]);
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::ImageNotAllowed),
    );
}

#[test]
fn an_image_still_waiting_cannot_bless() {
    let mut env = Env::new();
    expect_ok(env.propose_image(hash("next builder image"), ImageKind::Builder));
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(hash("next builder image"), key, env.service_leaf(key));
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::ImageNotAllowed),
    );
}

#[test]
fn a_project_off_the_allowlist_cannot_bless() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let mut journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    journal.project_id = hash("another operator");
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::ProjectNotAllowed),
    );
}

/// The zero key is never blessed: a reader that took a missing signature for
/// the zero key would otherwise accept it.
#[test]
fn the_zero_key_cannot_be_blessed() {
    let mut env = Env::new();
    let journal = env.journal_for(
        builder_image(),
        Pubkey::default(),
        env.service_leaf(Pubkey::default()),
    );
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::NoSigner),
    );
    assert!(!env.is_blessed_as(&Pubkey::default(), ImageKind::Builder));
}

#[test]
fn a_service_key_names_no_vault() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    let cluster = env.cluster();
    expect_code(
        env.bless(&journal, cluster, [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::WrongKindForVault),
    );
}

/// A vault key is held to naming all three of a vault chain, a vault and a
/// bundle, as a service key is held to naming none.
#[test]
fn an_executor_is_blessed_for_a_chain_and_a_bundle_not_just_a_vault() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let leaf = blessing_leaf(
        &[0; 32],
        &vault(),
        &[0; 32],
        &env.cluster(),
        &executor.to_bytes(),
    );
    let journal = env.journal_for(executor_image(), env.executor.pubkey(), leaf);
    expect_code(
        env.bless(&journal, [0; 32], vault(), [0; 32], vec![]),
        registry_error(RegistryError::WrongKindForVault),
    );
    let cluster = env.cluster();
    let leaf = blessing_leaf(&cluster, &vault(), &[0; 32], &cluster, &executor.to_bytes());
    let journal = env.journal_for(executor_image(), executor, leaf);
    expect_code(
        env.bless(&journal, cluster, vault(), [0; 32], vec![]),
        registry_error(RegistryError::WrongKindForVault),
    );
}

#[test]
fn an_executor_cannot_claim_a_vault_it_did_not_attest_to() {
    let mut env = Env::new();
    let journal = env.journal_for(
        executor_image(),
        env.executor.pubkey(),
        env.leaf_for(hash("my vault"), bundle(), env.executor.pubkey()),
    );
    let cluster = env.cluster();
    expect_code(
        env.bless(&journal, cluster, hash("your vault"), bundle(), vec![]),
        registry_error(RegistryError::NotAmongTheVaults),
    );
}

/// The leaf names the cluster it is blessed on, so a proof made for another
/// cluster (or an EVM chain) proves nothing here.
#[test]
fn a_blessing_proved_for_another_cluster_cannot_be_replayed_here() {
    let mut env = Env::new();
    let devnet = cluster_value("devnet");
    let executor = env.executor.pubkey();
    let leaf = blessing_leaf(&devnet, &vault(), &bundle(), &devnet, &executor.to_bytes());
    let journal = env.journal_for(executor_image(), env.executor.pubkey(), leaf);
    expect_code(
        env.bless(&journal, devnet, vault(), bundle(), vec![]),
        registry_error(RegistryError::NotAmongTheVaults),
    );
    let key = Keypair::new().pubkey();
    let service = blessing_leaf(&[0; 32], &[0; 32], &[0; 32], &devnet, &key.to_bytes());
    let journal = env.journal_for(builder_image(), key, service);
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::NotAmongTheVaults),
    );
}

#[test]
fn a_key_from_one_instance_cannot_be_claimed_by_another_image() {
    let mut env = Env::new();
    let journal = env.journal_for(
        reviewer_image(),
        env.builder.pubkey(),
        env.service_leaf(env.builder.pubkey()),
    );
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::NotThisInstance),
    );
}

/// A key is bound to one image and one project for life; a seal from a
/// different allowed project naming the same key is refused, so no second
/// operator can claim a running key.
#[test]
fn a_key_cannot_be_claimed_by_another_project() {
    let mut env = Env::new();
    let other = hash("another operator");
    expect_ok(env.propose_project(other));
    env.skip(TIMELOCK);
    let builder = env.builder.pubkey();
    env.bless_service_key(builder_image(), builder);
    let mut journal = env.journal_for(builder_image(), builder, env.service_leaf(builder));
    journal.project_id = other;
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::NotThisInstance),
    );
}

/// The leaf is checked after everything else, so a blessing refused only for
/// its leaf would pass every other check: a simulation that leaves the proof
/// out to fit in a transaction learns of anything else wrong first.
#[test]
fn a_blessing_checks_the_leaf_after_everything_else() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let cluster = env.cluster();

    // From the future, and naming a vault the tree doesn't hold.
    let mut journal = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    journal.issued_at = env.now() + 1;
    expect_code(
        env.bless(&journal, cluster, hash("another vault"), bundle(), vec![]),
        registry_error(RegistryError::Stale),
    );

    // Long past its TTL, and with no proof.
    let mut journal = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(
            vault(),
            bundle(),
            Pubkey::new_from_array(hash("another key")),
        ),
    );
    env.skip(BLESSING_TTL + 1);
    journal.issued_at = env.now() - BLESSING_TTL - 1;
    expect_code(
        env.bless(&journal, cluster, vault(), bundle(), vec![]),
        registry_error(RegistryError::Stale),
    );

    // Another image claiming a key, from a tree that doesn't hold it.
    let journal = env.journal_for(reviewer_image(), env.builder.pubkey(), hash("no such tree"));
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::NotThisInstance),
    );
}

// What a blessing grants ---------------------------------------------------------

#[test]
fn an_executor_key_is_blessed_for_one_vault_and_one_bundle() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, vault(), bundle());
    let cluster = env.cluster();

    assert!(env.is_blessed_for(&executor, cluster, vault(), bundle()));
    assert!(!env.is_blessed_for(&executor, cluster, hash("another vault"), bundle()));
    assert!(!env.is_blessed_for(&executor, cluster, vault(), hash("another bundle")));
    // The same address on another vault chain is another vault.
    assert!(!env.is_blessed_for(&executor, cluster_value("devnet"), vault(), bundle()));
    // And an executor key is no service key.
    assert!(!env.is_blessed_as(&executor, ImageKind::Builder));
    assert!(env.is_blessed_as(&executor, ImageKind::Executor));

    let info = env.key_info(&executor).unwrap();
    assert_eq!(info.image_digest, executor_image());
    assert_eq!(info.project_id, project());
    assert_eq!(info.kind, ImageKind::Executor);
    assert_eq!(info.vault_chain, cluster);
    assert_eq!(info.vault, vault());
    assert_eq!(info.strategy_hash, bundle());
    assert_eq!(info.trust_root, shonin_dev_verifier::TRUST_ROOT);
    assert_eq!(info.verifier, env.checker);
    assert_eq!(info.blessed_until, env.now() + BLESSING_TTL);
}

/// A station on this cluster serving a vault that lives on another chain asks
/// by the vault's chain. The leaf carries both chains, so the blessing
/// answers for that vault chain and vault here, and for nothing else.
#[test]
fn a_key_is_blessed_for_a_vault_on_another_chain_by_that_chain() {
    let mut env = Env::new();
    let mut base = [0u8; 32];
    base[24..].copy_from_slice(&8453u64.to_be_bytes());
    let mut evm_vault = [0u8; 32];
    evm_vault[12..].copy_from_slice(&[0xAB; 20]);
    let executor = env.executor.pubkey();
    let leaf = blessing_leaf(
        &base,
        &evm_vault,
        &bundle(),
        &env.cluster(),
        &executor.to_bytes(),
    );
    let journal = env.journal_for(executor_image(), executor, leaf);
    expect_ok(env.bless(&journal, base, evm_vault, bundle(), vec![]));
    assert!(env.is_blessed_for(&executor, base, evm_vault, bundle()));
    assert!(!env.is_blessed_for(&executor, env.cluster(), evm_vault, bundle()));
    assert_eq!(env.key_info(&executor).unwrap().vault_chain, base);
}

/// An unwinder key may be blessed for a vault, and is never blessed as an
/// executor, which is what a station asks before it lets a key trade.
#[test]
fn an_unwinder_key_is_never_blessed_as_an_executor() {
    let mut env = Env::new();
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let unwinder = Keypair::new().pubkey();
    let journal = env.journal_for(
        unwinder_image(),
        unwinder,
        env.leaf_for(vault(), bundle(), unwinder),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    assert!(env.is_blessed_for(&unwinder, cluster, vault(), bundle()));
    assert!(env.is_blessed_as(&unwinder, ImageKind::Unwinder));
    assert!(!env.is_blessed_as(&unwinder, ImageKind::Executor));
}

#[test]
fn a_blessing_expires() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, vault(), bundle());
    env.skip(BLESSING_TTL);
    assert!(env.is_blessed_for(&executor, env.cluster(), vault(), bundle()));
    env.skip(1);
    assert!(!env.is_blessed_for(&executor, env.cluster(), vault(), bundle()));
    assert!(!env.is_blessed_as(&executor, ImageKind::Executor));
}

/// A key blessed for no vault is never blessed for one, whatever is asked.
#[test]
fn nobody_is_blessed_for_the_zero_vault() {
    let env = Env::new();
    assert!(!env.is_blessed_for(&env.builder.pubkey(), [0; 32], [0; 32], [0; 32]));
}

// Time ---------------------------------------------------------------------------

/// A blessing runs for its TTL from the moment the instance attested, so
/// replaying a public seal can never extend it.
#[test]
fn a_lapsed_blessing_cannot_be_renewed_by_replaying_the_old_seal() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let journal = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    env.skip(BLESSING_TTL + 1);
    assert!(!env.is_blessed_for(&executor, cluster, vault(), bundle()));

    // Months later, a stranger replays the first blessing.
    env.skip(90 * DAY);
    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_code(
        env.bless_by(
            &alice,
            &attestor,
            &journal,
            cluster,
            vault(),
            bundle(),
            vec![],
        ),
        registry_error(RegistryError::Stale),
    );
    assert!(
        !env.is_blessed_for(&executor, cluster, vault(), bundle()),
        "an old seal renewed a lapsed blessing"
    );
}

#[test]
fn an_attestation_from_the_future_is_refused() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let mut journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    journal.issued_at = env.now() + 1;
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::Stale),
    );
}

/// Time is the cluster's clock, never the slot: a slot that moves backwards
/// (as one can after the local chain's clock jumps) changes no expiry.
#[test]
fn expiry_follows_the_clock_not_the_slot() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, vault(), bundle());
    let mut clock = env.svm.get_sysvar::<solana_clock::Clock>();
    clock.slot = 1;
    clock.unix_timestamp += (BLESSING_TTL + 1) as i64;
    env.svm.set_sysvar(&clock);
    assert!(!env.is_blessed_for(&executor, env.cluster(), vault(), bundle()));
    let journal = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    assert!(env.is_blessed_for(&executor, cluster, vault(), bundle()));
}

// Revocations reach every key -----------------------------------------------------

#[test]
fn revoking_a_project_stops_its_keys_at_once() {
    let mut env = Env::new();
    let (builder, reviewer) = (env.builder.pubkey(), env.reviewer.pubkey());
    assert!(env.is_blessed_as(&builder, ImageKind::Builder));
    assert!(env.is_blessed_as(&reviewer, ImageKind::Reviewer));
    expect_ok(env.revoke_project(project()));
    assert!(!env.is_project_allowed(project()));
    assert!(
        !env.is_blessed_as(&builder, ImageKind::Builder),
        "builder key outlived its project"
    );
    assert!(
        !env.is_blessed_as(&reviewer, ImageKind::Reviewer),
        "reviewer key outlived its project"
    );
}

#[test]
fn revoking_an_image_stops_its_vault_keys_at_once() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, vault(), bundle());
    expect_ok(env.revoke_image(executor_image()));
    assert!(!env.is_blessed_for(&executor, env.cluster(), vault(), bundle()));
}

// Several vaults, one key -----------------------------------------------------------

/// An instance attested for two vaults holds a key that serves both.
#[test]
fn a_key_attested_for_two_vaults_serves_both() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let (a, b) = (hash("vault A"), hash("vault B"));
    let (root, proof_a, proof_b) = pair(
        env.leaf_for(a, bundle(), executor),
        env.leaf_for(b, bundle(), executor),
    );
    let journal = env.journal_for(executor_image(), executor, root);
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, a, bundle(), proof_a));
    assert!(env.is_blessed_for(&executor, cluster, a, bundle()));
    expect_ok(env.bless(&journal, cluster, b, bundle(), proof_b));
    assert!(env.is_blessed_for(&executor, cluster, b, bundle()));
    assert!(
        env.is_blessed_for(&executor, cluster, a, bundle()),
        "blessing for vault B un-blessed the key for vault A"
    );
}

/// Nobody but the instance can change what its key is blessed for: a
/// stranger replaying the public seal with the other leaf leaves the first
/// vault's blessing standing.
#[test]
fn a_stranger_cannot_move_a_key_to_another_vault() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let (a, b) = (hash("vault A"), hash("vault B"));
    let (root, proof_a, proof_b) = pair(
        env.leaf_for(a, bundle(), executor),
        env.leaf_for(b, bundle(), executor),
    );
    let journal = env.journal_for(executor_image(), executor, root);
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, a, bundle(), proof_a));
    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_ok(env.bless_by(&alice, &attestor, &journal, cluster, b, bundle(), proof_b));
    assert!(
        env.is_blessed_for(&executor, cluster, a, bundle()),
        "a passer-by un-blessed the executor for vault A"
    );
}

/// What a key is reported as serving moves only with a later blessing, so a
/// stranger replaying a public seal with the other leaf can't change what the
/// record and the vault's page say.
#[test]
fn a_stranger_cannot_change_what_a_key_is_reported_as_serving() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let (a, b) = (hash("vault A"), hash("vault B"));
    let (root, proof_a, proof_b) = pair(
        env.leaf_for(a, bundle(), executor),
        env.leaf_for(b, bundle(), executor),
    );
    let journal = env.journal_for(executor_image(), executor, root);
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, a, bundle(), proof_a));
    assert_eq!(env.key_info(&executor).unwrap().vault, a);
    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_ok(env.bless_by(&alice, &attestor, &journal, cluster, b, bundle(), proof_b));
    assert!(env.is_blessed_for(&executor, cluster, a, bundle()));
    assert_eq!(
        env.key_info(&executor).unwrap().vault,
        a,
        "a passer-by re-pointed the key's record"
    );
}

/// Replaying a public seal that is not later than the blessing already
/// recorded changes nothing, and the event says what is recorded, so an
/// indexer that reads expiries from events never shows an earlier one.
#[test]
fn a_replay_that_changes_nothing_does_not_announce_an_earlier_expiry() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    let first = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&first, cluster, vault(), bundle(), vec![]));
    let first_until = env.now() + BLESSING_TTL;

    // The instance attests again a day later; its blessing moves later.
    env.skip(DAY);
    env.bless_executor_key(executor, vault(), bundle());
    let later = env.now() + BLESSING_TTL;
    assert_eq!(
        env.blessed_until(&executor, cluster, vault(), bundle()),
        later
    );

    // A stranger replays the first seal. The chain keeps `later`...
    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    let meta = expect_ok(env.bless_by(
        &alice,
        &attestor,
        &first,
        cluster,
        vault(),
        bundle(),
        vec![],
    ));
    assert_eq!(
        env.blessed_until(&executor, cluster, vault(), bundle()),
        later
    );
    // ...and announces `later`, never `first_until`.
    let expiries = announced(&meta);
    assert_eq!(
        expiries,
        vec![later],
        "a no-op replay announced the older, shorter expiry"
    );
    assert_ne!(expiries[0], first_until);
}

/// A blessing only moves later: an older seal never shortens a current one.
#[test]
fn an_older_seal_never_shortens_a_blessing() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    let old = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    env.skip(HOUR);
    env.bless_executor_key(executor, vault(), bundle());
    let fresh_until = env.now() + BLESSING_TTL;
    expect_ok(env.bless(&old, cluster, vault(), bundle(), vec![]));
    assert_eq!(
        env.blessed_until(&executor, cluster, vault(), bundle()),
        fresh_until
    );
    assert_eq!(env.key_info(&executor).unwrap().blessed_until, fresh_until);
}

// The journal --------------------------------------------------------------------

/// What an instance attests to is laid out as Borsh lays out the journal's
/// fields: image digest, project id, key, root, then the time as eight
/// little-endian bytes. 136 bytes, which the attestation signs as they are.
#[test]
fn the_journal_is_its_fields_in_order() {
    let journal = Journal {
        image_digest: [1; 32],
        project_id: [2; 32],
        signer: Pubkey::new_from_array([3; 32]),
        vaults_root: [4; 32],
        issued_at: 0x0102_0304_0506_0708,
    };
    let bytes = anchor_lang::prelude::borsh::to_vec(&journal).unwrap();
    assert_eq!(bytes.len(), 136);
    assert_eq!(&bytes[0..32], &[1; 32]);
    assert_eq!(&bytes[32..64], &[2; 32]);
    assert_eq!(&bytes[64..96], &[3; 32]);
    assert_eq!(&bytes[96..128], &[4; 32]);
    assert_eq!(&bytes[128..136], &0x0102_0304_0506_0708u64.to_le_bytes());
}

/// The leaf is the EVM contract's: `keccak256(keccak256(abi.encode(vaultChain,
/// vault, strategyHash, chain, key)))`, five 32-byte words.
#[test]
fn the_leaf_is_the_evm_contracts() {
    let mut chain = [0u8; 32];
    chain[31] = 1;
    let words = [[7u8; 32], [8u8; 32], [9u8; 32], chain, [10u8; 32]];
    let mut encoded = Vec::new();
    for word in &words {
        encoded.extend_from_slice(word);
    }
    let expected = keccak(&[&keccak(&[&encoded])]);
    assert_eq!(
        blessing_leaf(&words[0], &words[1], &words[2], &words[3], &words[4]),
        expected
    );
}
