//! Runtime facts about publishing, prices and profiles that the programs'
//! safety rests on, pinned so a later change can't quietly lose them.
//!
//! - A registration with every list at its bound fits the default compute.
//! - `cost_of` costs a few hundred compute units per period crossed, so an
//!   admin would need thousands of schedules inside one interval to break it.
//! - The Ed25519 scanner finds a signature wherever it sits, and two packed
//!   into one instruction.
//! - Lamports sent ahead beyond the rent stay in the record (nobody else's
//!   loss) and never block it.
//! - A profile's rent is exact on growth and shrink, refunded to the author.
//! - A stager with no account on-chain still gets its draft's rent back.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::system_instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// What an instruction may use without a compute-budget request.
const DEFAULT_COMPUTE: u64 = 200_000;

fn extend_ix(stager: &Pubkey, id: u64, more: Lists) -> Instruction {
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
            more,
        }
        .data(),
    }
}

/// A registration with every list at its bound (8 chains of 8 drivers, 16
/// bridges, 64 restricted codes) is assembled in pieces and registers within
/// the default compute.
#[test]
fn a_registration_at_every_bound_fits_the_default_compute() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let mut reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let chains: Vec<ChainNeed> = (0..8)
        .map(|c| ChainNeed {
            chain: name(&format!("chain{c}")),
            drivers: (0..8).map(|d| name(&format!("driver{c}-{d}"))).collect(),
        })
        .collect();
    let bridges: Vec<[u8; 32]> = (0..16).map(|i| name(&format!("bridge{i}"))).collect();
    let mut restricted: Vec<[u8; 32]> = (0..64u32).map(|i| keccak(&[&i.to_le_bytes()])).collect();
    restricted.sort();
    reg.lists = Lists {
        chains: chains.clone(),
        bridges: bridges.clone(),
        restricted: restricted.clone(),
    };
    let payer = env.payer.insecure_clone();
    let mut staged = reg.clone();
    staged.lists = Lists {
        chains: chains[..2].to_vec(),
        bridges: vec![],
        restricted: vec![],
    };
    expect_ok(env.send(vec![env.ix_stage(&payer.pubkey(), &staged)], &[&payer]));
    for chunk in chains[2..].chunks(2) {
        let more = Lists {
            chains: chunk.to_vec(),
            bridges: vec![],
            restricted: vec![],
        };
        expect_ok(env.send(vec![extend_ix(&payer.pubkey(), id, more)], &[&payer]));
    }
    let more = Lists {
        chains: vec![],
        bridges: bridges.clone(),
        restricted: vec![],
    };
    expect_ok(env.send(vec![extend_ix(&payer.pubkey(), id, more)], &[&payer]));
    for chunk in restricted.chunks(16) {
        let more = Lists {
            chains: vec![],
            bridges: vec![],
            restricted: chunk.to_vec(),
        };
        expect_ok(env.send(vec![extend_ix(&payer.pubkey(), id, more)], &[&payer]));
    }
    let draft = env
        .svm
        .get_account(&draft_address(id, &payer.pubkey()))
        .unwrap();
    assert_eq!(
        draft.lamports,
        env.svm.minimum_balance_for_rent_exemption(draft.data.len()),
        "a grown draft holds exactly its rent"
    );
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let ixs = vec![
        Env::ed25519(&builder, &digest),
        Env::ed25519(&reviewer, &clearance),
        env.ix_register(
            &payer.pubkey(),
            &payer.pubkey(),
            id,
            &builder.pubkey(),
            &reviewer.pubkey(),
        ),
    ];
    let meta = expect_ok(env.send(ixs, &[&payer]));
    assert!(
        meta.compute_units_consumed < DEFAULT_COMPUTE,
        "the largest registration used {} compute units",
        meta.compute_units_consumed
    );
    assert_eq!(env.strategy_of(id).unwrap().lists, reg.lists);
    let record = env.svm.get_account(&strategy_address(id)).unwrap();
    assert_eq!(
        record.lamports,
        env.svm
            .minimum_balance_for_rent_exemption(record.data.len())
    );
}

/// `cost_of` reads only the periods an interval crosses, and each costs a
/// few hundred compute units: 800 changes landing inside one day price in
/// well under the default compute.
#[test]
fn cost_of_pays_a_few_hundred_units_per_period_crossed() {
    let mut env = Env::new();
    let a = Allowances {
        fuel_per_sec: 1_000_000,
        memory_mib: 256,
        bytes_per_day: 50_000_000,
        inference_ms_per_min: 100,
        gas_per_day: 1_000_000,
        ..Allowances::default()
    };
    let admin = env.admin.insecure_clone();
    let start = env.now();
    let (_, one) = env.cost_of_on_chain(a, start, start + DAY);
    let mut p = published_prices();
    for _ in 0..80 {
        let batch: Vec<Instruction> = (0..10)
            .map(|_| {
                p.vcpu_hour += 1;
                env.ix_schedule(&admin.pubkey(), p)
            })
            .collect();
        expect_ok(env.send(batch, &[&admin]));
    }
    assert_eq!(env.history().len(), 801);
    // Every change takes effect at the same moment, so a day either side of
    // it crosses all 800.
    let change = start + PRICE_NOTICE;
    let (cost, many) = env.cost_of_on_chain(a, change - DAY, change + DAY);
    assert_eq!(cost, env.cost_of(&a, change - DAY, change + DAY));
    assert!(
        many < DEFAULT_COMPUTE,
        "800 periods cost {many} compute units"
    );
    let per_period = (many - one) / 800;
    assert!(
        per_period < 400,
        "each period crossed cost {per_period} compute units"
    );
}

/// The signatures may sit anywhere in the transaction, and both may ride in
/// one Ed25519 instruction: the scanner reads every entry of every Ed25519
/// instruction.
#[test]
fn signatures_are_found_wherever_they_sit() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let payer = env.payer.insecure_clone();

    // After the registration.
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    expect_ok(env.send(vec![env.ix_stage(&payer.pubkey(), &reg)], &[&payer]));
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    expect_ok(env.send(
        vec![
            register,
            Env::ed25519(&builder, &digest),
            Env::ed25519(&reviewer, &clearance),
        ],
        &[&payer],
    ));
    assert!(env.strategy_of(id).is_some());

    // Both in one instruction.
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    expect_ok(env.send(vec![env.ix_stage(&payer.pubkey(), &reg)], &[&payer]));
    let digest = env.registration_digest(&reg);
    let clearance = env.clearance_digest(id, reg.source_hash);
    let sig_b: [u8; 64] = builder.sign_message(&digest).as_ref().try_into().unwrap();
    let sig_r: [u8; 64] = reviewer
        .sign_message(&clearance)
        .as_ref()
        .try_into()
        .unwrap();
    // [count=2, pad] [offsets x2] then key, signature, message per entry.
    let header = 2 + 2 * 14;
    let mut data = vec![2u8, 0];
    let mut body = Vec::new();
    for (key, sig, msg) in [
        (builder.pubkey().to_bytes(), sig_b, digest),
        (reviewer.pubkey().to_bytes(), sig_r, clearance),
    ] {
        let key_at = (header + body.len()) as u16;
        body.extend_from_slice(&key);
        let sig_at = (header + body.len()) as u16;
        body.extend_from_slice(&sig);
        let msg_at = (header + body.len()) as u16;
        body.extend_from_slice(&msg);
        for word in [sig_at, u16::MAX, key_at, u16::MAX, msg_at, 32, u16::MAX] {
            data.extend_from_slice(&word.to_le_bytes());
        }
    }
    data.extend_from_slice(&body);
    let packed = Instruction {
        program_id: solana_sdk_ids::ed25519_program::ID,
        accounts: vec![],
        data,
    };
    let register = env.ix_register(
        &payer.pubkey(),
        &payer.pubkey(),
        id,
        &builder.pubkey(),
        &reviewer.pubkey(),
    );
    expect_ok(env.send(vec![packed, register], &[&payer]));
    assert!(env.strategy_of(id).is_some());
}

/// Lamports sent ahead beyond the rent stay in the record: the registering
/// payer pays nothing for the room, and the record is never closed, so the
/// surplus is the sender's loss and nobody else's.
#[test]
fn surplus_lamports_sent_ahead_stay_in_the_record() {
    let mut env = Env::new();
    let payer = env.payer.insecure_clone();
    let author = env.author.insecure_clone();
    let id = env.publishing_config().count + 1;
    let surplus = 1_000_000_000u64;
    let transfers = [strategy_address(id), review_address(id)]
        .iter()
        .map(|to| system_instruction::transfer(&payer.pubkey(), to, surplus))
        .collect();
    expect_ok(env.send(transfers, &[&payer]));
    assert_eq!(env.submit_as(&author), id);
    let bundle = hash("bundle");
    let bob = env.bob.insecure_clone();
    let reg = Registration::new(id, author.pubkey(), bundle);
    expect_ok(env.send(vec![env.ix_stage(&bob.pubkey(), &reg)], &[&bob]));
    let draft_rent = env.lamports(&draft_address(id, &bob.pubkey()));
    let before = env.lamports(&bob.pubkey());
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let ixs = vec![
        Env::ed25519(&builder, &env.registration_digest(&reg)),
        Env::ed25519(&reviewer, &env.clearance_digest(id, reg.source_hash)),
        env.ix_register(
            &bob.pubkey(),
            &bob.pubkey(),
            id,
            &builder.pubkey(),
            &reviewer.pubkey(),
        ),
    ];
    expect_ok(env.send(ixs, &[&bob]));
    // Bob got the draft back and paid only the transaction fee: three
    // signatures' worth, since the two Ed25519 instructions count as
    // signatures for the fee.
    let after = env.lamports(&bob.pubkey());
    assert!(
        after + 15_000 >= before + draft_rent,
        "bob had {before}, draft rent {draft_rent}, now {after}: paid {}",
        (before + draft_rent).saturating_sub(after)
    );
    assert_eq!(env.lamports(&strategy_address(id)), surplus);
    let reviewer = env.reviewer.insecure_clone();
    expect_ok(env.submit_review(&reviewer, id, review_for(bundle, 1)));
    assert_eq!(env.lamports(&review_address(id)), surplus);
}

/// A profile grown then shrunk holds exactly its rent each time; the author
/// pays the growth and is refunded the shrink, with anything donated to the
/// record.
#[test]
fn a_profiles_rent_is_exact_on_growth_and_shrink() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let address = profile_address(&author.pubkey());
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    let small = env.lamports(&address);
    let labels: Vec<String> = (0..4).map(|i| format!("label {i}")).collect();
    let urls: Vec<String> = (0..4)
        .map(|i| format!("https://example.invalid/path/{i}"))
        .collect();
    let before = env.lamports(&author.pubkey());
    expect_ok(env.set_profile(&author, "carry", &"b".repeat(400), labels, urls));
    let big = env.lamports(&address);
    let len = env.svm.get_account(&address).unwrap().data.len();
    assert_eq!(big, env.svm.minimum_balance_for_rent_exemption(len));
    let paid = before - env.lamports(&author.pubkey());
    assert!(paid >= big - small && paid < big - small + 10_000);

    let payer = env.payer.insecure_clone();
    expect_ok(env.send(
        vec![system_instruction::transfer(
            &payer.pubkey(),
            &address,
            5_000_000,
        )],
        &[&payer],
    ));
    let before = env.lamports(&author.pubkey());
    expect_ok(env.set_profile(&author, "carry", "", vec![], vec![]));
    assert_eq!(env.lamports(&address), small);
    let gained = env.lamports(&author.pubkey()) - before;
    assert!(gained + 10_000 >= big - small + 5_000_000 && gained <= big - small + 5_000_000);
}

/// A stager whose account has since been emptied still gets the draft's rent
/// back when someone else registers it.
#[test]
fn a_stager_with_no_account_is_still_refunded() {
    let mut env = Env::new();
    let author = env.author.insecure_clone();
    let id = env.submit_as(&author);
    let reg = Registration::new(id, author.pubkey(), hash("bundle"));
    let stager = Keypair::new();
    env.svm.airdrop(&stager.pubkey(), 10_000_000).unwrap();
    expect_ok(env.send(vec![env.ix_stage(&stager.pubkey(), &reg)], &[&stager]));
    let payer = env.payer.insecure_clone();
    let left = env.lamports(&stager.pubkey());
    expect_ok(env.send(
        vec![system_instruction::transfer(
            &stager.pubkey(),
            &payer.pubkey(),
            left - 5_000,
        )],
        &[&stager],
    ));
    assert_eq!(env.lamports(&stager.pubkey()), 0);
    let (builder, reviewer) = (env.builder.insecure_clone(), env.reviewer.insecure_clone());
    let draft_rent = env.lamports(&draft_address(id, &stager.pubkey()));
    let ixs = vec![
        Env::ed25519(&builder, &env.registration_digest(&reg)),
        Env::ed25519(&reviewer, &env.clearance_digest(id, reg.source_hash)),
        env.ix_register(
            &payer.pubkey(),
            &stager.pubkey(),
            id,
            &builder.pubkey(),
            &reviewer.pubkey(),
        ),
    ];
    expect_ok(env.send(ixs, &[&payer]));
    assert_eq!(env.lamports(&stager.pubkey()), draft_rent);
    assert!(env.strategy_of(id).is_some());
}
