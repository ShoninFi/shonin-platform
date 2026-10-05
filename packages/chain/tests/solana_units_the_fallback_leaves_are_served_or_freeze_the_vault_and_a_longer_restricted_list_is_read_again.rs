//! The keeper's upkeep of the pro-rata fallback and of the restricted list,
//! on a real local cluster, sent through the same builders and the same
//! simulate-then-send the keeper uses:
//!
//! - a withdrawal nobody priced for `fallback_after` takes the fallback and
//!   leaves its owner units; serving them closes exactly their fraction of
//!   the station, worked out here from what the station holds, and it is
//!   theirs to collect;
//! - units nobody serves for `freeze_after` freeze the vault, and not a
//!   moment before;
//! - a driver name whose approved implementation now excludes more people
//!   lengthens the vault's restricted list when it is read again, and a
//!   depositor must state the longer list.
//!
//! The clock is moved forward with Surfpool's time travel. Without Surfpool
//! 1.2.0 or later, the Solana CLI or the built programs, it says so and
//! passes, as CI has no cluster.

#![cfg(feature = "solana")]

mod three_venues;

use std::time::{Duration, Instant};

use alloy::primitives::U256;

use chain::Role;
use chain::solana::platform::{Platform, VaultAccounts, event_authority};
use chain::solana::programs::anchor_lang::{InstructionData, ToAccountMetas};
use chain::solana::programs::{common, registry, station, vault};
use chain::solana::{Instruction, Keypair, Pubkey, Signer, SolanaClient, TransactionFailed};

use three_venues::{AT_EACH, DEPOSIT, LAMPORTS, PATIENCE, Platformed, program_id, start};

use vault::state::{self as vs, Holder, Restricted, Vault, VaultState, WAD};

/// The driver that answers a longer restricted list, approved later as the
/// third driver name's implementation. It must be deployed final, as every
/// driver is.
const LONGER_LIST: &str = "shonin_test_driver";
/// What the second depositor puts in.
const SECOND_DEPOSIT: u64 = 10_000_000_000;
/// What the executor leaves counted at the station, besides what it puts at
/// each venue.
const IDLE_AT_STATION: u64 = AT_EACH;

/// A restricted-list code as the drivers spell it.
fn code(text: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..text.len()].copy_from_slice(text);
    out
}

/// The hash a depositor's statement names, worked out here: keccak256 of
/// the list's Borsh encoding, its length then each code.
fn list_hash(list: &[[u8; 32]]) -> [u8; 32] {
    let mut bytes = (list.len() as u32).to_le_bytes().to_vec();
    for code in list {
        bytes.extend_from_slice(code);
    }
    common::digest::keccak(&[&bytes])
}

/// `a * b / c`, rounded down, in 256 bits.
fn mul_div(a: u128, b: u128, c: u128) -> u128 {
    (U256::from(a) * U256::from(b) / U256::from(c)).to()
}

/// The depositor's own fallback, which the keeper never sends: their
/// redemption request `id`, nothing having priced it.
fn ix_take_pro_rata(
    platform: &Platform,
    v: &VaultAccounts,
    vault: &Vault,
    owner: &Pubkey,
    request: &Pubkey,
    id: u64,
) -> Instruction {
    Instruction {
        program_id: vault::ID,
        accounts: vault::accounts::TakeProRata {
            owner: *owner,
            pool: platform.pool(v, vault),
            station_program: station::ID,
            request: *request,
            holder: vs::holder_address(&v.vault, owner).0,
            to_dollars: platform.dollars(owner),
            system_program: Pubkey::default(),
            event_authority: event_authority(),
            program: vault::ID,
        }
        .to_account_metas(None),
        data: vault::instruction::TakeProRata {
            request_id: id,
            groups: vec![],
        }
        .data(),
    }
}

/// A holder takes what their served units brought home.
fn ix_collect_units(platform: &Platform, v: &VaultAccounts, owner: &Pubkey) -> Instruction {
    Instruction {
        program_id: vault::ID,
        accounts: vault::accounts::CollectUnits {
            owner: *owner,
            vault: v.vault,
            holder: vs::holder_address(&v.vault, owner).0,
            to: platform.dollars(owner),
            vault_dollars: v.vault_dollars,
            mint: platform.mint,
            token_program: chain::solana::programs::anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vault::instruction::CollectUnits {}.data(),
    }
}

/// Simulated as the keeper does before it sends, then sent; panics if
/// either turns it down.
async fn simulate_then_send(client: &SolanaClient, ixs: Vec<Instruction>, signer: &Keypair) {
    client
        .simulate(ixs.clone(), &signer.pubkey())
        .await
        .expect("the simulation accepts it");
    client.send(ixs, &[signer]).await.expect("it lands");
}

/// The program error a simulation turned `ixs` down with.
async fn refused_with(client: &SolanaClient, ixs: Vec<Instruction>, payer: &Pubkey) -> u32 {
    let error = client
        .simulate(ixs, payer)
        .await
        .expect_err("the simulation turns it down");
    let failed = error
        .downcast_ref::<TransactionFailed>()
        .unwrap_or_else(|| panic!("not a refusal: {error:#}"));
    failed
        .custom
        .unwrap_or_else(|| panic!("no program error: {failed}"))
}

/// Everything the station holds, read from the token accounts and the venue
/// accounts themselves: its own dollars, its authority's at each venue, and
/// the cash at each venue (none of which holds a position).
async fn station_holdings(p: &Platformed, v: &VaultAccounts) -> (u64, Vec<u64>) {
    let (client, platform) = (&p.client, &p.platform);
    let record = platform.station(client, &v.station).await.unwrap();
    let mut idle = client.token_balance(&v.station_dollars).await.unwrap();
    let mut at_venues = Vec::new();
    for (pinned, venue) in record.drivers.iter().zip(p.venues) {
        idle += client.token_balance(&pinned.dollars).await.unwrap();
        let account = platform
            .margin_account_at(client, &venue, &pinned.authority)
            .await
            .unwrap()
            .expect("the station has an account at each venue");
        assert!(account.positions.is_empty(), "no position is held");
        at_venues.push(u64::try_from(account.cash).unwrap());
    }
    (idle, at_venues)
}

#[tokio::test]
async fn fallback_units_are_served_at_their_worth_or_freeze_the_vault_and_lists_are_read_again() {
    let Some(cluster) = start(&[(LONGER_LIST, true)]) else {
        return;
    };
    let p = Platformed::set_up(&cluster).await;
    let (client, platform) = (&p.client, &p.platform);
    let (author, viewer, keeper, executor, attestor, admin, deployer) = (
        p.role(Role::Author),
        p.role(Role::Viewer),
        p.role(Role::Keeper),
        p.role(Role::Executor),
        p.role(Role::Attestor),
        p.role(Role::Admin),
        &client.keys().unwrap().deployer,
    );
    let (id, bundle) = p.publish().await;

    // A vault on the three drivers.
    let (_, statement) = platform
        .restricted_for(client, &author.pubkey(), id, &p.drivers)
        .await
        .unwrap();
    let factory = platform.factory(client).await.unwrap();
    let activate = platform
        .ix_activate(
            client,
            &author.pubkey(),
            id,
            &p.drivers,
            chain::solana::programs::prices::Allowances::default(),
            500,
            factory.terms.minimum_tank,
            statement,
            vault::factory::terms_hash(&factory.terms),
        )
        .await
        .unwrap();
    client.send(vec![activate], &[author]).await.unwrap();
    let v = platform.vault_accounts(id);

    // Two depositors, priced: the viewer, and a second with dollars of
    // their own.
    let second = Keypair::new();
    client.airdrop(&second.pubkey(), LAMPORTS).await.unwrap();
    let fund = vec![
        platform.ix_open_dollars(&deployer.pubkey(), &second.pubkey()),
        platform
            .ix_mint_to(&admin.pubkey(), &second.pubkey(), SECOND_DEPOSIT)
            .unwrap(),
    ];
    client.send(fund, &[deployer, admin]).await.unwrap();
    let statement = platform
        .vault(client, &v.vault)
        .await
        .unwrap()
        .restricted_hash;
    for (depositor, assets) in [(viewer, DEPOSIT), (&second, SECOND_DEPOSIT)] {
        let deposit = platform.ix_request_deposit(
            &v.vault,
            &depositor.pubkey(),
            &depositor.pubkey(),
            assets,
            Some(statement),
            1,
        );
        client.send(vec![deposit], &[depositor]).await.unwrap();
    }
    let started = Instant::now();
    loop {
        let mut priced = true;
        for depositor in [viewer.pubkey(), second.pubkey()] {
            priced &= platform.shares(client, &v.vault, &depositor).await.unwrap() != 0;
        }
        if priced {
            break;
        }
        p.keep(&v).await;
        assert!(
            started.elapsed() < PATIENCE,
            "the deposits were never priced"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    platform.settle_all(client, keeper, &v).await.unwrap();

    // The executor puts money at the station and at each venue, holding no
    // position, so what the station holds is plain to read.
    platform
        .bless_executor(
            client,
            executor,
            attestor,
            &executor.pubkey(),
            &v.vault,
            &bundle,
        )
        .await
        .unwrap();
    for index in 0..3 {
        let ix = platform
            .ix_approve_key(
                client,
                &v,
                &bundle,
                &executor.pubkey(),
                index,
                &executor.pubkey(),
            )
            .await
            .unwrap();
        client.send(vec![ix], &[executor]).await.unwrap();
    }
    let to_station = platform
        .ix_move(
            client,
            &v,
            &bundle,
            &executor.pubkey(),
            3 * AT_EACH + IDLE_AT_STATION,
            true,
        )
        .await
        .unwrap();
    client.send(vec![to_station], &[executor]).await.unwrap();
    let record = platform.station(client, &v.station).await.unwrap();
    let key = executor.pubkey();
    for (index, venue) in p.venues.iter().enumerate() {
        let authority = record.drivers[index].authority;
        let through = |venue_ix, fund| {
            platform.ix_execute(client, &v, &bundle, &key, index as u8, venue_ix, fund)
        };
        let open = through(platform.venue_open_at(venue, &key, &authority), 0)
            .await
            .unwrap();
        client.send(vec![open], &[executor]).await.unwrap();
        let deposit = through(
            platform.venue_deposit_at(venue, &authority, AT_EACH),
            AT_EACH,
        )
        .await
        .unwrap();
        client.send(vec![deposit], &[executor]).await.unwrap();
    }

    // Both ask for all their shares back, and nothing prices the requests:
    // once `fallback_after` has passed, each takes the fallback.
    let mut requests = Vec::new();
    for owner in [viewer, &second] {
        let shares = platform
            .shares(client, &v.vault, &owner.pubkey())
            .await
            .unwrap();
        let redeem =
            platform.ix_request_redeem(&v.vault, &owner.pubkey(), shares, &owner.pubkey(), 2);
        client.send(vec![redeem], &[owner]).await.unwrap();
        let address = vs::request_address(&v.vault, &owner.pubkey(), 2).0;
        let request: vs::Request = client.load(&address).await.unwrap().unwrap();
        requests.push((owner, address, request));
    }
    let vault = platform.vault(client, &v.vault).await.unwrap();
    let latest = requests.iter().map(|(_, _, r)| r.earliest).max().unwrap();
    client
        .rpc
        .time_travel(latest + vault.timers.fallback_after + 1)
        .await
        .unwrap();
    assert!(
        platform
            .unit_holders(client, &v.vault)
            .await
            .unwrap()
            .is_empty()
    );
    let mut fractions = Vec::new();
    for (owner, address, request) in &requests {
        let vault = platform.vault(client, &v.vault).await.unwrap();
        // Their part of the vault, from the shares they asked back and the
        // supply before the fallback.
        let part = mul_div(
            request.amount,
            WAD - vault.live_units().unwrap(),
            vault.total_supply + vs::OFFSET,
        );
        fractions.push(part);
        let take = ix_take_pro_rata(platform, &v, &vault, &owner.pubkey(), address, request.id);
        client.send(vec![take], &[*owner]).await.unwrap();
        let after: vs::Request = client.load(address).await.unwrap().unwrap();
        assert!(after.done, "the fallback took the request");
    }
    let vault = platform.vault(client, &v.vault).await.unwrap();
    let holders = platform.unit_holders(client, &v.vault).await.unwrap();
    let mut owners = vec![viewer.pubkey(), second.pubkey()];
    owners.sort();
    assert_eq!(
        holders
            .iter()
            .map(|holder| holder.owner)
            .collect::<Vec<_>>(),
        owners,
        "exactly the two who took the fallback hold units"
    );
    let unit_of = |owner: &Pubkey| -> Holder {
        holders
            .iter()
            .find(|holder| holder.owner == *owner)
            .cloned()
            .unwrap()
    };
    let (first_units, second_units) = (unit_of(&viewer.pubkey()), unit_of(&second.pubkey()));
    assert_eq!(vault.units, first_units.units + second_units.units);
    // The fraction their units claim is their part of the vault, short only
    // of the holding fee charged on every share up to the fallback.
    for (units, part) in [(&first_units, fractions[0]), (&second_units, fractions[1])] {
        let claimed = mul_div(units.units, vault.scale(), WAD);
        assert!(
            claimed <= part && part - claimed <= part / 10_000,
            "units claim {claimed} of the station against a part of {part}"
        );
    }

    // No freeze for units until they have waited `freeze_after`.
    let freeze = platform.ix_freeze_units(&v.vault, &second.pubkey());
    assert_eq!(
        refused_with(client, vec![freeze.clone()], &keeper.pubkey()).await,
        u32::from(vault::error::VaultError::NotYet),
        "a freeze before it is due is refused"
    );

    // The keeper serves the viewer's units: exactly their fraction of what
    // the station holds comes home, credited to them.
    let (idle, at_venues) = station_holdings(&p, &v).await;
    assert_eq!(idle, IDLE_AT_STATION);
    assert_eq!(at_venues, vec![AT_EACH; 3]);
    let fraction = mul_div(first_units.units, vault.scale(), WAD);
    let expected: u128 = std::iter::once(idle)
        .chain(at_venues.iter().copied())
        .map(|held| mul_div(held as u128, fraction, WAD))
        .sum();
    let expected = u64::try_from(expected).unwrap();
    assert!(expected > 0, "the viewer's units are worth something");
    let vault_dollars = client.token_balance(&v.vault_dollars).await.unwrap();
    assert!(!vault.owed(), "no settlement is owed");
    let serve = platform
        .ix_serve_pro_rata(client, &v, &vault, &keeper.pubkey(), &viewer.pubkey())
        .await
        .unwrap();
    simulate_then_send(client, vec![serve], keeper).await;
    let served = platform
        .holder(client, &v.vault, &viewer.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(served.units, 0, "the viewer's units are cleared");
    assert_eq!(
        served.unit_credit, expected,
        "the viewer is credited their fraction of the station's holdings"
    );
    let (idle_after, at_venues_after) = station_holdings(&p, &v).await;
    let held = |idle: u64, at: &[u64]| idle + at.iter().sum::<u64>();
    assert_eq!(
        held(idle, &at_venues) - held(idle_after, &at_venues_after),
        expected,
        "the station gave up exactly what the viewer was credited"
    );
    assert_eq!(
        client.token_balance(&v.vault_dollars).await.unwrap() - vault_dollars,
        expected,
        "and it all came home"
    );
    let after = platform.vault(client, &v.vault).await.unwrap();
    assert_eq!(after.unit_reserve - vault.unit_reserve, expected);
    assert_eq!(after.units, second_units.units, "the second's units stand");
    assert_eq!(
        platform
            .unit_holders(client, &v.vault)
            .await
            .unwrap()
            .iter()
            .map(|holder| holder.owner)
            .collect::<Vec<_>>(),
        vec![second.pubkey()],
        "only the unserved holder is left"
    );
    // And it is theirs to collect.
    let before = client
        .token_balance(&platform.dollars(&viewer.pubkey()))
        .await
        .unwrap();
    let collect = ix_collect_units(platform, &v, &viewer.pubkey());
    client.send(vec![collect], &[viewer]).await.unwrap();
    assert_eq!(
        client
            .token_balance(&platform.dollars(&viewer.pubkey()))
            .await
            .unwrap()
            - before,
        expected,
        "the viewer is paid what their units brought home"
    );

    // The third driver name's implementation is now one whose venue
    // excludes more: the keeper reads each driver alone, finds only that one
    // lengthens the list, and sends it.
    let longer = program_id(LONGER_LIST);
    let propose = platform
        .ix_propose(
            client,
            registry::state::ApprovalKind::Driver,
            p.drivers[2].name,
            longer,
        )
        .await
        .unwrap();
    client.send(vec![propose], &[admin]).await.unwrap();
    let vault = platform.vault(client, &v.vault).await.unwrap();
    assert_eq!(vault.state, VaultState::Open);
    let restricted_address = vs::restricted_address(&v.vault).0;
    let before: Restricted = client.load(&restricted_address).await.unwrap().unwrap();
    assert_eq!(vault.restricted_hash, list_hash(&before.codes));
    let now = client.now().await.unwrap();
    let mut lengthening = Vec::new();
    for (index, name) in vault.drivers.iter().enumerate() {
        let reading = platform.driver_reading(client, name, now).await.unwrap();
        let alone =
            platform.ix_refresh_restricted(&v.vault, &keeper.pubkey(), &[(index as u8, reading)]);
        let simulated = client
            .simulate_reading(vec![alone], &keeper.pubkey(), &[v.vault])
            .await
            .unwrap();
        let account = simulated.accounts[0].clone().unwrap();
        let after =
            <Vault as chain::solana::programs::anchor_lang::AccountDeserialize>::try_deserialize(
                &mut &account.data[..],
            )
            .unwrap();
        if after.restricted_hash != vault.restricted_hash {
            lengthening.push((index as u8, reading));
        }
    }
    assert_eq!(
        lengthening
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        vec![2],
        "only the driver now answering a longer list lengthens it"
    );
    assert_eq!(lengthening[0].1.program, longer);
    let refresh = platform.ix_refresh_restricted(&v.vault, &keeper.pubkey(), &lengthening);
    simulate_then_send(client, vec![refresh], keeper).await;
    let refreshed: Restricted = client.load(&restricted_address).await.unwrap().unwrap();
    let mut expected_list = before.codes.clone();
    for added in [code(b"KP"), code(b"SANCTIONED")] {
        if !expected_list.contains(&added) {
            expected_list.push(added);
        }
    }
    expected_list.sort();
    assert!(
        expected_list.len() > before.codes.len(),
        "the driver adds a code"
    );
    assert_eq!(
        refreshed.codes, expected_list,
        "the list covers the new codes"
    );
    let vault = platform.vault(client, &v.vault).await.unwrap();
    assert_ne!(vault.restricted_hash, statement, "the vault's hash moved");
    assert_eq!(vault.restricted_hash, list_hash(&expected_list));
    // A depositor stating the old list is turned away; the new one is taken.
    let stating = |statement| {
        platform.ix_request_deposit(
            &v.vault,
            &viewer.pubkey(),
            &viewer.pubkey(),
            factory.terms.minimum_deposit,
            Some(statement),
            3,
        )
    };
    assert!(
        client
            .simulate(vec![stating(statement)], &viewer.pubkey())
            .await
            .is_err(),
        "a statement of the old list is refused"
    );
    client
        .simulate(vec![stating(vault.restricted_hash)], &viewer.pubkey())
        .await
        .expect("a statement of the longer list is taken");

    // The second's units go unserved until `freeze_after` has passed since
    // they were left: then the keeper's freeze lands and the vault stops.
    let waiting = platform
        .holder(client, &v.vault, &second.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(waiting.units, 0);
    client
        .rpc
        .time_travel(waiting.units_since + vault.timers.freeze_after + 1)
        .await
        .unwrap();
    simulate_then_send(client, vec![freeze], keeper).await;
    let frozen = platform.vault(client, &v.vault).await.unwrap();
    assert_eq!(frozen.state, VaultState::Frozen);
    assert!(frozen.halted(), "the frozen vault is halted");
}
