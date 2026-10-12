//! What `migrate_station` needs: the largest station the first programs
//! could have written migrates under the heap and compute a plain
//! transaction has, and the allocation-free walk never turns away a body
//! that decodes whole.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{
    AccountDeserialize, AccountSerialize, AnchorDeserialize, AnchorSerialize, Discriminator,
    InstructionData, ToAccountMetas,
};
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::state::{
    first, walk, Layout, StationAddress, MAX_CREATED, MAX_DRIVERS, STATION_SEED,
};
use solana_account::Account;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;

fn first_bytes(old: &first::Station) -> Vec<u8> {
    let mut data = Station::DISCRIMINATOR.to_vec();
    old.serialize(&mut data).unwrap();
    data
}

fn current_bytes(station: &Station) -> Vec<u8> {
    let mut data = Vec::new();
    station.try_serialize(&mut data).unwrap();
    data
}

fn put_station(env: &mut Env, address: Pubkey, data: Vec<u8>) {
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            address,
            Account {
                lamports,
                data,
                owner: shonin_station::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

fn ix_migrate(payer: &Pubkey, station: &Pubkey) -> Instruction {
    Instruction {
        program_id: shonin_station::ID,
        accounts: shonin_station::accounts::MigrateStation {
            payer: *payer,
            station: *station,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None),
        data: shonin_station::instruction::MigrateStation {}.data(),
    }
}

/// A driver as the first programs could pin it at its largest: every rule,
/// position, fixed set and address at the limits, using only the position
/// kinds the first programs knew.
fn largest_first_pinned(seed: u8) -> first::Pinned {
    use shonin_driver::*;
    let key = |i: u8| Pubkey::new_from_array([seed.wrapping_mul(16).wrapping_add(i); 32]);
    let prefix = |i: usize| vec![i as u8; MAX_SEED];
    let rule = |i: u8| Rule {
        discriminator: vec![i; MAX_DISCRIMINATOR],
        class: if i.is_multiple_of(2) {
            Class::Trade
        } else {
            Class::Exit
        },
        positions: (0..MAX_POSITIONS)
            .map(|p| Position {
                index: p as u8,
                own: Own::StationAddress(prefix(p % MAX_STATION_ADDRESSES)),
            })
            .collect(),
    };
    first::Pinned {
        name: [b'a' + seed; 32],
        program: key(1),
        venue: key(2),
        authority: key(3),
        authority_bump: 255 - seed,
        dollars: key(4),
        key: key(5),
        rules: (0..MAX_RULES as u8).map(rule).collect(),
        fixed: vec![vec![key(6); MAX_FIXED]],
        addresses: (0..MAX_STATION_ADDRESSES)
            .map(|i| StationAddress {
                prefix: prefix(i),
                address: key(7),
            })
            .collect(),
    }
}

/// A first-layout station with every driver at its largest, at its own
/// station address, holding the rent its old size needs.
fn largest_first_station(env: &mut Env) -> (Pubkey, first::Station) {
    let vault = Keypair::new().pubkey();
    let (address, bump) =
        Pubkey::find_program_address(&[STATION_SEED, vault.as_ref()], &shonin_station::ID);
    let old = first::Station {
        vault,
        vault_program: shonin_test_vault::ID,
        vault_dollars: Pubkey::new_from_array([0xa1; 32]),
        dollars: Pubkey::new_from_array([0xa2; 32]),
        mint: env.mint,
        bundle_hash: [0xa3; 32],
        cluster: [0xa4; 32],
        exit_band_bps: 500,
        idle: 1_234_567,
        sent_home: 7_654_321,
        stopped: false,
        stopped_at: 0,
        payer: Pubkey::new_from_array([0xa5; 32]),
        bump,
        drivers: (0..MAX_DRIVERS as u8).map(largest_first_pinned).collect(),
    };
    let data = first_bytes(&old);
    assert!(data.len() <= MAX_CREATED, "{} bytes", data.len());
    put_station(env, address, data);
    (address, old)
}

/// The compute units the station program reported using, from the logs.
fn units_used(logs: &[String]) -> Option<u64> {
    let program = shonin_station::ID.to_string();
    logs.iter().find_map(|line| {
        let rest = line.strip_prefix(&format!("Program {program} consumed "))?;
        rest.split(' ').next()?.parse().ok()
    })
}

/// A plain transaction: no compute budget instruction, so the runtime's
/// default heap (32 KiB) and default compute limit apply. This is what a
/// sender other than the services' client would send.
#[test]
fn the_largest_first_layout_station_migrates_in_a_plain_transaction() {
    let mut env = Env::new();
    env.set_up_stations();
    let (address, old) = largest_first_station(&mut env);
    let before = env.svm.get_account(&address).unwrap();
    assert_eq!(
        Station::read_any(&before.data).map(|(_, layout)| layout),
        Some(Layout::First)
    );

    let payer = env.payer.insecure_clone();
    env.svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    env.svm.expire_blockhash();
    let tx = Transaction::new(
        &[&payer],
        Message::new(
            &[ix_migrate(&payer.pubkey(), &address)],
            Some(&payer.pubkey()),
        ),
        env.svm.latest_blockhash(),
    );
    let result = env.svm.send_transaction(tx);
    let logs = match &result {
        Ok(meta) => meta.logs.clone(),
        Err(failed) => failed.meta.logs.clone(),
    };
    eprintln!(
        "largest first-layout station: {} bytes before, station program used {:?} compute units",
        before.data.len(),
        units_used(&logs)
    );
    let meta = match result {
        Ok(meta) => meta,
        Err(failed) => panic!(
            "the migration of the largest station failed in a plain transaction: {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        ),
    };
    assert!(units_used(&meta.logs).is_some());

    let after = env.svm.get_account(&address).unwrap();
    let expected = Station::from(old);
    assert_eq!(after.data, current_bytes(&expected));
    assert_eq!(after.data.len(), before.data.len() + 8 * MAX_DRIVERS);
    assert_eq!(
        Station::try_deserialize(&mut &after.data[..]).unwrap(),
        expected
    );
    assert!(after.lamports >= env.svm.minimum_balance_for_rent_exemption(after.data.len()));
}

/// `T` decoded from all of `body`, as the program's private `whole` does.
fn whole<T: AnchorDeserialize>(mut body: &[u8]) -> Option<T> {
    let value = T::deserialize(&mut body).ok()?;
    body.is_empty().then_some(value)
}

/// Random multi-byte changes over real records in both layouts, and random
/// cuts and extensions: wherever a body decodes whole in a layout, the walk
/// must go through in that layout, or `read_any` would turn a readable
/// station away before trying to decode it.
#[test]
fn the_walk_never_refuses_a_body_that_decodes_whole() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("full", &Declared::one_per_venue());
    let real = env.station(&home);
    let mut bodies = vec![current_bytes(&real)[8..].to_vec()];
    let old = first::Station {
        vault: real.vault,
        vault_program: real.vault_program,
        vault_dollars: real.vault_dollars,
        dollars: real.dollars,
        mint: real.mint,
        bundle_hash: real.bundle_hash,
        cluster: real.cluster,
        exit_band_bps: real.exit_band_bps,
        idle: real.idle,
        sent_home: real.sent_home,
        stopped: real.stopped,
        stopped_at: real.stopped_at,
        payer: real.payer,
        bump: real.bump,
        drivers: real
            .drivers
            .iter()
            .map(|d| first::Pinned {
                name: d.name,
                program: d.program,
                venue: d.venue,
                authority: d.authority,
                authority_bump: d.authority_bump,
                dollars: d.dollars,
                key: d.key,
                rules: d.rules.clone(),
                fixed: d.fixed.clone(),
                addresses: d.addresses.clone(),
            })
            .collect(),
    };
    bodies.push(first_bytes(&old)[8..].to_vec());
    let (_, largest) = largest_first_station(&mut env);
    bodies.push(first_bytes(&largest)[8..].to_vec());
    bodies.push(current_bytes(&Station::from(largest))[8..].to_vec());

    // A small deterministic generator, so a failure can be reproduced.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut decoded = 0;
    for body in &bodies {
        for _ in 0..3_000 {
            let mut changed = body.clone();
            let changes = 1 + (next() % 4) as usize;
            for _ in 0..changes {
                let at = (next() as usize) % changed.len();
                changed[at] = match next() % 6 {
                    0 => 0,
                    1 => 1,
                    2 => 0xff,
                    3 => changed[at].wrapping_add(1),
                    4 => changed[at].wrapping_sub(1),
                    _ => (next() & 0xff) as u8,
                };
            }
            match next() % 5 {
                0 => {
                    let cut = (next() as usize) % 40;
                    changed.truncate(changed.len().saturating_sub(cut));
                }
                1 => {
                    let more = (next() as usize) % 40;
                    changed.extend(std::iter::repeat_n((next() & 0xff) as u8, more));
                }
                _ => {}
            }
            if whole::<Station>(&changed).is_some() {
                assert!(walk::fills(&changed, Layout::Current));
                decoded += 1;
            }
            if whole::<first::Station>(&changed).is_some() {
                assert!(walk::fills(&changed, Layout::First));
                decoded += 1;
            }
        }
    }
    assert!(decoded > 100, "only {decoded} decoded");
}
