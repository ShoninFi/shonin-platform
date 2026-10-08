//! The fixture itself, and what every transaction must fit: the platform
//! deploys, sets up and answers as the development chain's does, and its
//! heaviest instructions fit one transaction on the wire (every send checks
//! that) and the compute a transaction gets without asking for more.

use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// What an instruction may use without a compute-budget request.
const DEFAULT_COMPUTE: u64 = 200_000;

#[test]
fn the_platform_sets_up_as_the_development_chain_does() {
    let mut env = Env::new();
    assert!(env.is_blessed_as(&env.builder.pubkey(), ImageKind::Builder));
    assert!(env.is_blessed_as(&env.reviewer.pubkey(), ImageKind::Reviewer));
    assert_eq!(env.trading_driver(name("local")), env.driver);
    assert_eq!(env.bridge_of(name("local-bridge")), env.bridge);
    let (id, bundle) = env.ready();
    assert_eq!(env.strategy_of(id).unwrap().bundle_hash, bundle);
    assert!(env.has_review(id));
}

/// The worst blessing: every checker the registry holds is allowed, the
/// newest three answer no and the oldest accepts, for a key whose instance
/// serves sixteen vaults.
#[test]
fn the_heaviest_blessing_fits_one_transaction_and_the_default_compute() {
    let mut env = Env::new();
    for _ in 1..shonin_registry::state::MAX_VERIFIERS {
        env.propose_checker(Keypair::new().pubkey());
    }
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let bundle = hash("bundle");
    let mut leaves: Vec<[u8; 32]> = (0..16u8)
        .map(|i| env.leaf_for([i + 1; 32], bundle, executor))
        .collect();
    // The proof of the first leaf, built level by level.
    let mut proof = Vec::new();
    let mut index = 0usize;
    while leaves.len() > 1 {
        proof.push(leaves[index ^ 1]);
        leaves = leaves
            .chunks(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                if a <= b {
                    keccak(&[&a, &b])
                } else {
                    keccak(&[&b, &a])
                }
            })
            .collect();
        index /= 2;
    }
    let journal = env.journal_for(executor_image(), executor, leaves[0]);
    let cluster = env.cluster();
    let meta = expect_ok(env.bless(&journal, cluster, [1; 32], bundle, proof));
    assert!(env.is_blessed_for(&executor, cluster, [1; 32], bundle));
    assert!(
        meta.compute_units_consumed < DEFAULT_COMPUTE,
        "the heaviest blessing used {} compute units",
        meta.compute_units_consumed
    );
}

/// A registration with both signatures, checked against both keys'
/// blessings, fits too.
#[test]
fn a_registration_fits_one_transaction_and_the_default_compute() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let registration = Registration::new(id, author.pubkey(), hash("bundle"));
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let meta = expect_ok(env.register_with(&registration, &builder, &reviewer));
    assert!(
        meta.compute_units_consumed < DEFAULT_COMPUTE,
        "a registration used {} compute units",
        meta.compute_units_consumed
    );
}
