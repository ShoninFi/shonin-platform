//! Each key an instance holds is named in its own leaf, so one attestation
//! and one proof bless every key at once, each only for the vault, bundle
//! and cluster its own leaf names. The instance's key in the journal names
//! the instance and is blessed for nothing by itself, and a key attested by
//! one image is never blessed for another.

use anchor_lang::prelude::Pubkey;
use shonin_registry::error::RegistryError;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn bundle() -> [u8; 32] {
    hash("bundle")
}

/// The root over two leaves, and each leaf's proof.
fn pair(a: [u8; 32], b: [u8; 32]) -> ([u8; 32], Vec<[u8; 32]>, Vec<[u8; 32]>) {
    let root = if a <= b {
        keccak(&[&a, &b])
    } else {
        keccak(&[&b, &a])
    };
    (root, vec![b], vec![a])
}

/// Blesses `key` under `journal`, sealed by the fixture's attestor.
#[allow(clippy::too_many_arguments)]
fn bless_key(
    env: &mut Env,
    journal: &Journal,
    vault_chain: [u8; 32],
    vault: [u8; 32],
    strategy_hash: [u8; 32],
    key: Pubkey,
    proof: Vec<[u8; 32]>,
) -> TxResult {
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, journal);
    let ix = env.ix_bless_key(
        &payer.pubkey(),
        journal,
        seal,
        vault_chain,
        vault,
        strategy_hash,
        key,
        proof,
        3,
    );
    env.send(vec![ed25519, ix], &[&payer])
}

/// An instance key, and two vault keys each committed for its own vault.
struct Instance {
    journal: Journal,
    first: Pubkey,
    second: Pubkey,
    proof_first: Vec<[u8; 32]>,
    proof_second: Vec<[u8; 32]>,
}

fn instance(env: &Env, first_vault: [u8; 32], second_vault: [u8; 32]) -> Instance {
    let (first, second) = (Keypair::new().pubkey(), Keypair::new().pubkey());
    let (root, proof_first, proof_second) = pair(
        env.leaf_for(first_vault, bundle(), first),
        env.leaf_for(second_vault, bundle(), second),
    );
    let journal = env.journal_for(executor_image(), Keypair::new().pubkey(), root);
    Instance {
        journal,
        first,
        second,
        proof_first,
        proof_second,
    }
}

#[test]
fn one_attestation_blesses_each_key_for_its_own_vault_alone() {
    let mut env = Env::new();
    let (a, b) = (hash("vault a"), hash("vault b"));
    let i = instance(&env, a, b);
    let cluster = env.cluster();
    expect_ok(bless_key(
        &mut env,
        &i.journal,
        cluster,
        a,
        bundle(),
        i.first,
        i.proof_first.clone(),
    ));
    expect_ok(bless_key(
        &mut env,
        &i.journal,
        cluster,
        b,
        bundle(),
        i.second,
        i.proof_second.clone(),
    ));
    assert!(env.is_blessed_for(&i.first, cluster, a, bundle()));
    assert!(env.is_blessed_for(&i.second, cluster, b, bundle()));
    assert!(!env.is_blessed_for(&i.first, cluster, b, bundle()));
    assert!(!env.is_blessed_for(&i.second, cluster, a, bundle()));
    assert!(env.is_blessed_as(&i.first, ImageKind::Executor));

    // A key's proof doesn't carry it to the other key's vault.
    for (vault, key, proof) in [
        (b, i.first, i.proof_first.clone()),
        (b, i.first, i.proof_second.clone()),
        (a, i.second, i.proof_second.clone()),
        (a, i.second, i.proof_first.clone()),
    ] {
        expect_code(
            bless_key(&mut env, &i.journal, cluster, vault, bundle(), key, proof),
            registry_error(RegistryError::NotAmongTheVaults),
        );
    }
}

#[test]
fn a_leaf_proves_nothing_for_another_key_cluster_vault_or_bundle() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let vault = hash("vault");
    let journal = env.journal_for(
        executor_image(),
        Keypair::new().pubkey(),
        env.leaf_for(vault, bundle(), key),
    );
    let cluster = env.cluster();
    let devnet = cluster_value("devnet");
    for (vault_chain, vault, strategy_hash, key) in [
        (cluster, vault, bundle(), Keypair::new().pubkey()),
        (devnet, vault, bundle(), key),
        (cluster, hash("another vault"), bundle(), key),
        (cluster, vault, hash("another bundle"), key),
    ] {
        expect_code(
            bless_key(
                &mut env,
                &journal,
                vault_chain,
                vault,
                strategy_hash,
                key,
                vec![],
            ),
            registry_error(RegistryError::NotAmongTheVaults),
        );
    }
    expect_ok(bless_key(
        &mut env,
        &journal,
        cluster,
        vault,
        bundle(),
        key,
        vec![],
    ));

    // A leaf made for another cluster names that cluster, so a journal of
    // such leaves blesses nothing here.
    let elsewhere = blessing_leaf(&devnet, &vault, &bundle(), &devnet, &key.to_bytes());
    let journal = env.journal_for(executor_image(), Keypair::new().pubkey(), elsewhere);
    expect_code(
        bless_key(&mut env, &journal, devnet, vault, bundle(), key, vec![]),
        registry_error(RegistryError::NotAmongTheVaults),
    );
}

#[test]
fn the_instance_key_is_blessed_for_nothing_by_itself() {
    let mut env = Env::new();
    let vault = hash("vault");
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(
        executor_image(),
        Keypair::new().pubkey(),
        env.leaf_for(vault, bundle(), key),
    );
    let cluster = env.cluster();
    expect_ok(bless_key(
        &mut env,
        &journal,
        cluster,
        vault,
        bundle(),
        key,
        vec![],
    ));
    let instance = journal.signer;
    assert!(env.key_info(&instance).is_none());
    assert!(!env.is_blessed_for(&instance, cluster, vault, bundle()));
    assert!(!env.is_blessed_as(&instance, ImageKind::Executor));
    // And it can't be blessed with a leaf that names another key.
    expect_code(
        bless_key(
            &mut env,
            &journal,
            cluster,
            vault,
            bundle(),
            instance,
            vec![],
        ),
        registry_error(RegistryError::NotAmongTheVaults),
    );
}

#[test]
fn a_key_attested_by_one_image_is_never_blessed_for_another() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let builder = env.journal_for(
        builder_image(),
        Keypair::new().pubkey(),
        env.service_leaf(key),
    );
    expect_ok(bless_key(
        &mut env,
        &builder,
        [0; 32],
        [0; 32],
        [0; 32],
        key,
        vec![],
    ));
    assert!(env.is_blessed_as(&key, ImageKind::Builder));
    // Another instance, of another image, naming the same key in its leaf.
    let reviewer = env.journal_for(
        reviewer_image(),
        Keypair::new().pubkey(),
        env.service_leaf(key),
    );
    expect_code(
        bless_key(&mut env, &reviewer, [0; 32], [0; 32], [0; 32], key, vec![]),
        registry_error(RegistryError::NotThisInstance),
    );
    assert!(env.is_blessed_as(&key, ImageKind::Builder));
    assert!(!env.is_blessed_as(&key, ImageKind::Reviewer));
}

#[test]
fn a_service_key_proves_the_leaf_with_no_vault_and_its_own_key() {
    let mut env = Env::new();
    let (builder, other) = (Keypair::new().pubkey(), Keypair::new().pubkey());
    let journal = env.journal_for(
        builder_image(),
        Keypair::new().pubkey(),
        env.service_leaf(builder),
    );
    expect_code(
        bless_key(&mut env, &journal, [0; 32], [0; 32], [0; 32], other, vec![]),
        registry_error(RegistryError::NotAmongTheVaults),
    );
    expect_ok(bless_key(
        &mut env,
        &journal,
        [0; 32],
        [0; 32],
        [0; 32],
        builder,
        vec![],
    ));
    assert!(env.is_blessed_as(&builder, ImageKind::Builder));
}

#[test]
fn a_zero_key_or_a_zero_instance_is_refused() {
    let mut env = Env::new();
    let zero = Pubkey::default();
    let journal = env.journal_for(
        builder_image(),
        Keypair::new().pubkey(),
        env.service_leaf(zero),
    );
    expect_code(
        bless_key(&mut env, &journal, [0; 32], [0; 32], [0; 32], zero, vec![]),
        registry_error(RegistryError::NoSigner),
    );
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), zero, env.service_leaf(key));
    expect_code(
        bless_key(&mut env, &journal, [0; 32], [0; 32], [0; 32], key, vec![]),
        registry_error(RegistryError::NoSigner),
    );
}

#[test]
fn a_root_key_leaf_never_blesses_a_key() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let purpose = shonin_common::digest::root_key_purpose();
    let cluster = env.cluster();
    let vault = hash("vault");
    let journal = env.journal_for(
        executor_image(),
        Keypair::new().pubkey(),
        env.leaf_for(vault, purpose, key),
    );
    expect_code(
        bless_key(&mut env, &journal, cluster, vault, purpose, key, vec![]),
        registry_error(RegistryError::WrongKindForVault),
    );
}

#[test]
fn the_same_attestation_through_a_newer_checker_moves_the_blessing_to_it() {
    let mut env = Env::new();
    let second_key = Keypair::new();
    let second = env.propose_checker(second_key.pubkey());
    env.skip(TIMELOCK);
    let vault = hash("vault");
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(
        executor_image(),
        Keypair::new().pubkey(),
        env.leaf_for(vault, bundle(), key),
    );
    let cluster = env.cluster();
    // Recorded through the first checker: the newer one is asked first and
    // finds no signature of its key.
    expect_ok(bless_key(
        &mut env,
        &journal,
        cluster,
        vault,
        bundle(),
        key,
        vec![],
    ));
    let first = env.checker;
    assert_eq!(env.key_info(&key).unwrap().verifier, first);

    // The same journal, sealed for the newer checker, moves it there without
    // changing how long it lasts.
    let until = env.blessed_until(&key, cluster, vault, bundle());
    let payer = env.payer.insecure_clone();
    let (seal, ed25519) = env.seal(&second_key, &journal);
    let ix = env.ix_bless_key(
        &payer.pubkey(),
        &journal,
        seal,
        cluster,
        vault,
        bundle(),
        key,
        vec![],
        3,
    );
    expect_ok(env.send(vec![ed25519, ix], &[&payer]));
    assert_eq!(env.key_info(&key).unwrap().verifier, second);
    assert_eq!(env.blessed_until(&key, cluster, vault, bundle()), until);

    // So retiring the first checker at the end of the overlap keeps it.
    expect_ok(env.retire_verifier(first));
    assert!(env.is_blessed_for(&key, cluster, vault, bundle()));

    // An older checker never takes it back.
    let attestor = env.attestor.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, &journal);
    let ix = env.ix_bless_key(
        &payer.pubkey(),
        &journal,
        seal,
        cluster,
        vault,
        bundle(),
        key,
        vec![],
        3,
    );
    // The first checker is retired, so it isn't asked: no proof.
    expect_code(
        env.send(vec![ed25519, ix], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
    assert_eq!(env.key_info(&key).unwrap().verifier, second);
}
