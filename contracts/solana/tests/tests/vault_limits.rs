//! A vault's heaviest instructions at the worst case its limits permit:
//! each fits the compute a transaction may ask for and the accounts it may
//! lock, and goes on the wire through a lookup table where it must.
//!
//! - A claim's close through every driver a station may declare, each
//!   venue account holding every market it may hold, with funding owed on
//!   every position: the station takes what its drivers are allowed
//!   (`COMPUTE_PER_DRIVER` each), and the vault's own work around it must fit
//!   the `CALLER_COMPUTE` the station leaves its caller. What it says is said
//!   whole, however long the venues' logs run.
//! - A valuation of the same station.
//! - A settlement of the most requests one takes ([`MAX_SETTLE`]), each of
//!   the costliest kind.
//! - Activation of a vault through every driver.
//! - The keeper's close of a halted vault, in one transaction.

use litesvm::types::TransactionMetadata;
use shonin_driver::{COMPUTE_PER_DRIVER, MAX_MARKETS};
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_station::state::{CALLER_COMPUTE, MAX_COMPUTE, MAX_DRIVERS};
use shonin_vault::events::{ClaimPaid, ClaimTried, FromStation, Transfer, Withdrawn};
use shonin_vault::state::MAX_SETTLE;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The markets each venue lists: its spot and perp, and six more.
fn markets() -> Vec<[u8; 32]> {
    let mut markets = vec![spot(), perp()];
    markets.extend((0..6).map(|i| name(&format!("COIN-{i}"))));
    assert_eq!(markets.len(), MAX_MARKETS);
    markets
}

fn accrue_at(env: &Env, venue: &Pubkey, market: [u8; 32], paid_per_unit: i128) -> Instruction {
    use anchor_lang::{InstructionData, ToAccountMetas};
    Instruction {
        program_id: *venue,
        accounts: shonin_local_venue::accounts::Operate {
            operator: env.operator.pubkey(),
            venue: shonin_local_venue::venue_address_of(venue),
            market: shonin_local_venue::market_address_of(venue, &market),
        }
        .to_account_metas(None),
        data: shonin_local_venue::instruction::AccrueFunding { paid_per_unit }.data(),
    }
}

/// A vault on every driver a station may declare, alice's and bob's money in
/// it, most of it at the venues with every market held at each and funding
/// owed on every position, and a claim of alice's booked.
fn at_the_limits() -> (Env, V, u64) {
    let mut env = Env::with_vaults();
    let v = env.open(&Declared::one_per_venue());
    assert_eq!(v.home.drivers.len(), MAX_DRIVERS);
    let operator = env.operator.insecure_clone();
    for venue in VENUES {
        let more: Vec<Instruction> = markets()[2..]
            .iter()
            .map(|id| env.ix_list_market_at(&venue, *id, true, PRICE))
            .collect();
        expect_ok(env.send(more, &[&operator]));
    }
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let shares = env.deposit_as(&v, &alice, 300_000_000_000);
    env.deposit_as(&v, &bob, 300_000_000_000);
    let executor = env.executor.insecure_clone();
    for index in 0..MAX_DRIVERS {
        expect_ok(env.approve_key_at(&v.home, &executor, index, &executor.pubkey()));
    }
    expect_ok(env.to_station(&v, 540_000_000_000));
    for index in 0..MAX_DRIVERS {
        env.fund_venue_at(&v.home, index as u8, 180_000_000_000);
        let orders: Vec<OrderRequest> = markets()
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let size = UNIT / 10 + 7_777_777_777 * (i as u128 + 1);
                if id != spot() && i % 2 == 1 {
                    order(id, false, size, 0)
                } else {
                    order(id, true, size, PRICE)
                }
            })
            .collect();
        for batch in orders.chunks(4) {
            env.place_at(&v.home, index as u8, batch.to_vec());
        }
    }
    for (index, venue) in VENUES.into_iter().enumerate() {
        let accrue: Vec<Instruction> = markets()
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, id)| {
                let paid = 1_234_567 + i as i128;
                accrue_at(&env, &venue, *id, if i % 2 == 1 { -paid } else { paid })
            })
            .collect();
        for batch in accrue.chunks(4) {
            expect_ok(env.send(batch.to_vec(), &[&operator]));
        }
        assert_eq!(
            env.held_markets_at(&venue, &v.home.authority(index)).len(),
            MAX_MARKETS
        );
    }
    let id = env.redeem(&v, &alice, shares);
    env.price(&v);
    assert_eq!(
        env.claim(&v, id).expect("booked").state,
        ClaimState::Pending
    );
    (env, v, id)
}

/// Every account `ixs` name, for the lookup table they are sent through.
fn table(ixs: &[Instruction]) -> Vec<Pubkey> {
    let mut keys: Vec<Pubkey> = Vec::new();
    for key in ixs
        .iter()
        .flat_map(|ix| ix.accounts.iter().map(|meta| meta.pubkey))
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Sends `ixs` through a lookup table (which checks the wire and the account
/// locks) and answers with the compute they took.
#[track_caller]
fn measure(env: &mut Env, ixs: Vec<Instruction>, signer: &Keypair, what: &str) -> u64 {
    measured(env, ixs, signer, what).compute_units_consumed
}

/// The same, answering with the transaction's metadata.
#[track_caller]
fn measured(
    env: &mut Env,
    ixs: Vec<Instruction>,
    signer: &Keypair,
    what: &str,
) -> TransactionMetadata {
    let table = table(&ixs);
    let meta = expect_ok(env.send_through_table(ixs, &[signer], &table));
    let used = meta.compute_units_consumed;
    println!("{what}: {used} compute units, {} calls", calls(&meta));
    assert!(used <= MAX_COMPUTE, "{what} used {used} compute units");
    meta
}

/// The most calls a transaction makes, its top-level instructions included.
const MAX_CALLS: usize = 64;

/// The calls a transaction made: each top-level instruction (the compute
/// budget's included) and every call under it.
fn calls(meta: &TransactionMetadata) -> usize {
    meta.inner_instructions.len() + meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
}

#[test]
fn a_claims_close_through_every_driver_at_every_market_fits_the_room_the_station_leaves() {
    let (mut env, v, id) = at_the_limits();
    let payer = env.payer.insecure_clone();
    let alice = env.alice.pubkey();
    let before = env.usd_balance(&alice);
    let ix = env.ix_settle_claim(&v, &payer.pubkey(), id);
    let used = measure(&mut env, vec![ix], &payer, "a claim's close at the limits");
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    assert!(env.usd_balance(&alice) > before);
    // The station's part is held to its drivers' budgets; the rest is the
    // vault's and must fit what the station leaves its caller.
    assert!(
        used <= MAX_DRIVERS as u64 * COMPUTE_PER_DRIVER + CALLER_COMPUTE,
        "{used}"
    );
}

/// The same close says everything it booked and paid through the program's
/// own instruction data: the venues it called log first, and at the limits
/// the logs run past their cap, which a node truncates.
#[test]
fn a_claim_closed_at_the_limits_says_everything_through_instruction_data() {
    let (mut env, v, id) = at_the_limits();
    let payer = env.payer.insecure_clone();
    let alice = env.alice.pubkey();
    let shares = env.claim(&v, id).unwrap().shares;
    let ix = env.ix_settle_claim(&v, &payer.pubkey(), id);
    let meta = measured(&mut env, vec![ix], &payer, "a claim's close at the limits");
    let claim = env.claim(&v, id).unwrap();
    assert_eq!(claim.state, ClaimState::Paid);

    let home: Vec<FromStation> = emitted(&meta);
    assert_eq!(home.len(), 1);
    assert!(home[0].assets > 0 && home[0].to_units == 0);
    let withdrawn: Vec<Withdrawn> = emitted(&meta);
    assert_eq!(withdrawn.len(), 1);
    assert_eq!(withdrawn[0].owner, alice);
    assert_eq!(withdrawn[0].shares, shares);
    assert_eq!(withdrawn[0].assets, claim.paid);
    let burned: Vec<Transfer> = emitted(&meta);
    assert!(burned
        .iter()
        .any(|t| t.from == v.vault && t.to == Pubkey::default() && t.value == shares));
    let paid: Vec<ClaimPaid> = emitted(&meta);
    assert_eq!(paid.len(), 1);
    assert_eq!((paid[0].claim_id, paid[0].assets), (id, claim.paid));
    let tried: Vec<ClaimTried> = emitted(&meta);
    assert_eq!(tried.len(), 1);
    assert_eq!(tried[0].claim_id, id);
    assert!(!tried[0].blocked);
    // Nothing of the vault's is left to the logs.
    assert!(events::<ClaimPaid>(&meta.logs).is_empty());
    assert!(events::<ClaimTried>(&meta.logs).is_empty());
    assert!(calls(&meta) < MAX_CALLS, "{}", calls(&meta));
}

#[test]
fn a_valuation_through_every_driver_at_every_market_fits() {
    let (mut env, v, _) = at_the_limits();
    env.to_next_slot();
    let payer = env.payer.insecure_clone();
    let ix = env.ix_record_valuation(&v);
    let meta = measured(&mut env, vec![ix], &payer, "a valuation at the limits");
    let used = meta.compute_units_consumed;
    assert!(used < MAX_COMPUTE / 2, "{used}");
    assert!(calls(&meta) < MAX_CALLS / 2, "{}", calls(&meta));
}

/// The most requests one settlement takes, each of the costliest kind: a
/// queued claim paid with its profit fee, which hands its fee slice to the
/// author and the platform, burns the rest, pays its owner and says so, six
/// calls in all (each event is a call of the program's own); and money the
/// station sent home since the vault last looked, booked and said first. A
/// transaction makes at most 64 calls, the top-level instructions included,
/// with room left for a priority fee's.
#[test]
fn a_settlement_of_the_most_requests_one_takes_fits() {
    use anchor_lang::InstructionData;
    let mut env = Env::with_vaults();
    let v = env.open(&[Declared::local()]);
    let mut people = Vec::new();
    for _ in 0..MAX_SETTLE {
        let who = Keypair::new();
        env.svm.airdrop(&who.pubkey(), 1_000_000_000).unwrap();
        env.deposit(&v, &who, 1_000_000_000);
        people.push(who);
    }
    let ix = env.ix_author(
        &v,
        shonin_vault::instruction::SetIdleFloor { bps: 0 }.data(),
    );
    expect_ok(env.as_author(ix));
    env.skip(DAY);
    env.price(&v);
    // A gain at the venue, so every exit owes a profit fee.
    env.deploy_spot(&v, 1_000_000_000);
    env.gain(&v, 200_000_000);
    env.price(&v);
    assert!(env.vault(&v).latest.fee_exit > 0);
    // The rest of the cash at the station: every withdrawal becomes a claim.
    let cash = env.vault(&v).cash;
    expect_ok(env.to_station(&v, cash));
    let mut claims = Vec::new();
    for who in &people {
        let shares = env.shares(&v, &who.pubkey());
        claims.push(env.redeem(&v, who, shares / 10));
    }
    env.price(&v);
    // The cash home again covers each: every claim is queued.
    expect_ok(env.from_station(&v, cash));
    for id in &claims {
        assert_eq!(env.claim(&v, *id).unwrap().state, ClaimState::Pending);
        expect_ok(env.settle_claim(&v, *id));
        assert_eq!(env.claim(&v, *id).unwrap().state, ClaimState::Queued);
    }
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    // The executor's key closes a tenth of the station directly: money home
    // the settlement books.
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&v.home, &executor.pubkey(), 100_000_000_000_000_000, true);
    expect_ok(env.send_fitting(vec![ix], &[&executor]));
    let sent = env.station(&v.home).sent_home;
    assert!(sent > env.vault(&v).station_booked);
    let payer = env.payer.insecure_clone();
    let cursor = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, cursor, MAX_SETTLE);
    let meta = measured(
        &mut env,
        vec![ix],
        &payer,
        "a settlement of the most requests",
    );
    let used = meta.compute_units_consumed;
    let vault = env.vault(&v);
    assert_eq!(vault.request_cursor, vault.priced_through);
    assert_eq!(vault.station_booked, sent);
    for id in &claims {
        assert_eq!(env.claim(&v, *id).unwrap().state, ClaimState::Paid);
    }
    assert!(used < MAX_COMPUTE / 2, "{used}");
    // Two for the compute budget, the settlement, the arrival, and six for
    // each request.
    assert_eq!(calls(&meta), 2 + 1 + 1 + 6 * MAX_SETTLE);
    assert!(calls(&meta) < MAX_CALLS, "{}", calls(&meta));
    // One more is refused: the batch is bound by the calls a transaction
    // may make.
    for who in people.iter().chain([&people[0]]) {
        env.redeem(&v, who, 1);
    }
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let cursor = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, cursor, MAX_SETTLE + 1);
    expect_code(
        env.send_fitting(vec![ix], &[&payer]),
        vault_error(VaultError::TooMany),
    );
}

#[test]
fn activating_a_vault_through_every_driver_fits() {
    let mut env = Env::with_vaults();
    let drivers = Declared::one_per_venue();
    let names: Vec<[u8; 32]> = drivers.iter().map(|d| d.name).collect();
    let (id, bundle) = env.ready_with(&names, vec![name("SANCTIONED"), name("US")]);
    let (_, statement) = env.restricted_for(id, &drivers);
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    let used = measure(
        &mut env,
        vec![ix],
        &author,
        "activation through every driver",
    );
    assert!(used < MAX_COMPUTE / 2, "{used}");
}

#[test]
fn the_keepers_close_of_a_halted_vault_fits_one_transaction() {
    let (mut env, v, _) = at_the_limits();
    expect_ok(env.kill(&v));
    env.skip(HALT_GRACE);
    let payer = env.payer.insecure_clone();
    // Stop, unwind everything, value and close, in one transaction, so
    // nobody can send the station anything between the unwind and the
    // valuation that finds it empty.
    env.to_next_slot();
    let stop = env.ix_stop(&v.home);
    let unwind = env.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
    let value = env.ix_record_valuation(&v);
    let close = env.ix_close_vault(&v);
    let used = measure(
        &mut env,
        vec![stop, unwind, value, close],
        &payer,
        "the keeper's close at the limits",
    );
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    println!("{used}");
}

#[test]
fn a_one_venue_vaults_every_instruction_fits_the_wire_without_a_lookup_table() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 150_000_000_000);
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    let payer = env.payer.insecure_clone();
    let sizes = [
        (
            "a valuation",
            env.wire_size(vec![env.ix_record_valuation(&v)], &[&payer]),
        ),
        (
            "a claim's close",
            env.wire_size(
                vec![env.ix_settle_claim(&v, &payer.pubkey(), id)],
                &[&payer],
            ),
        ),
        (
            "a fallback",
            env.wire_size(
                vec![env.ix_take_pro_rata(&v, &alice.pubkey(), id)],
                &[&payer, &alice],
            ),
        ),
    ];
    for (what, size) in sizes {
        println!("{what}: {size} bytes");
        assert!(size <= PACKET_DATA_SIZE, "{what} takes {size} bytes");
    }
    // Activating through one driver fits too; through three it takes a table
    // (see `activating_a_vault_through_every_driver_fits`).
    let (id, bundle) = env.ready_with(&[local()], vec![name("SANCTIONED"), name("US")]);
    let (_, statement) = env.restricted_for(id, &[Declared::local()]);
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    let ix = env.ix_activate(
        id,
        bundle,
        &[Declared::local()],
        MINIMUM_TANK,
        statement,
        terms,
        &[],
    );
    let size = env.wire_size(vec![ix], &[&author]);
    println!("an activation through one driver: {size} bytes");
}
