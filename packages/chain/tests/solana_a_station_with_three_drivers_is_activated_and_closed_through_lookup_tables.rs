//! A station with a driver at each of three venues, on a real local cluster:
//! its activation, its valuations and its close in one transaction each
//! name more accounts than the wire carries by address, so each goes as a
//! version-0 transaction through its paying key's own lookup table, and
//! each is read back from the cluster.
//!
//! The test deploys the programs itself (`contracts/solana/target/deploy`,
//! built by `task contracts:solana:build`) to a Surfpool of its own, and
//! plays every role with the development keys. Without Surfpool 1.2.0 or
//! later (`SHONIN_SURFPOOL`, or the one on the path), the Solana CLI or the
//! built programs, it says so and passes, as CI has no cluster.

#![cfg(feature = "solana")]

mod three_venues;

use std::time::{Duration, Instant};

use chain::Role;
use chain::solana::programs::{local_venue, prices, station, vault};
use chain::solana::{Pubkey, Signer, TransactionFailed, name, tables};

use three_venues::{
    AT_EACH, DEPOSIT, PATIENCE, Platformed, SPOT, UNIT, connect, start, version_of,
};

#[tokio::test]
async fn a_station_with_three_drivers_is_activated_valued_and_closed_through_lookup_tables() {
    let Some(cluster) = start(&[]) else { return };
    let p = Platformed::set_up(&cluster).await;
    let (client, platform) = (&p.client, &p.platform);
    let (author, viewer, keeper, executor, attestor, admin) = (
        p.role(Role::Author),
        p.role(Role::Viewer),
        p.role(Role::Keeper),
        p.role(Role::Executor),
        p.role(Role::Attestor),
        p.role(Role::Admin),
    );
    let (id, bundle) = p.publish().await;

    // Activated by its author on three drivers: too large by address.
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
            prices::Allowances::default(),
            500,
            factory.terms.minimum_tank,
            statement,
            vault::factory::terms_hash(&factory.terms),
        )
        .await
        .unwrap();
    let by_address = tables::wire_size(&tables::by_address(
        std::slice::from_ref(&activate),
        &author.pubkey(),
        Default::default(),
    ));
    assert!(
        by_address > chain::solana::client::PACKET_DATA_SIZE,
        "{by_address} bytes by address"
    );
    let activated = client.send(vec![activate], &[author]).await.unwrap();
    assert_eq!(version_of(client, &activated).await, 0);
    assert!(
        !activated.setup.is_empty(),
        "the author's table was made for it"
    );
    assert!(
        activated.rent() > 0,
        "the author paid the table's rent with the vault's"
    );
    let v = platform.vault_accounts(id);
    let record = platform.station(client, &v.station).await.unwrap();
    let pinned: Vec<(Pubkey, Pubkey)> = record
        .drivers
        .iter()
        .map(|driver| (driver.program, driver.venue))
        .collect();
    assert_eq!(
        pinned,
        p.drivers
            .iter()
            .zip(p.venues)
            .map(|(driver, venue)| (driver.program, venue))
            .collect::<Vec<_>>(),
        "the station pins a driver at each venue"
    );
    assert_eq!(
        platform.vault(client, &v.vault).await.unwrap().state,
        vault::state::VaultState::Open
    );

    // A depositor's money priced by valuations through all three drivers,
    // the keeper's own table made for them.
    let statement = platform
        .vault(client, &v.vault)
        .await
        .unwrap()
        .restricted_hash;
    let deposit = platform.ix_request_deposit(
        &v.vault,
        &viewer.pubkey(),
        &viewer.pubkey(),
        DEPOSIT,
        Some(statement),
        1,
    );
    client.send(vec![deposit], &[viewer]).await.unwrap();
    let started = Instant::now();
    let mut valued_through_a_table = false;
    while platform
        .shares(client, &v.vault, &viewer.pubkey())
        .await
        .unwrap()
        == 0
    {
        if let Some(valued) = p.keep(&v).await {
            valued_through_a_table |= version_of(client, &valued).await == 0;
        }
        assert!(started.elapsed() < PATIENCE, "the deposit was never priced");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(valued_through_a_table, "a valuation went through a table");

    // The executor puts money to work at each venue through the guard.
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
        .ix_move(client, &v, &bundle, &executor.pubkey(), 3 * AT_EACH, true)
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
        let open = through(
            platform.venue_open_at(venue, &executor.pubkey(), &authority),
            0,
        )
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
        let market: local_venue::Market = client
            .load(&local_venue::market_address_of(venue, &name(SPOT)))
            .await
            .unwrap()
            .unwrap();
        let limit = market.ask + market.ask / 100;
        let order = local_venue::OrderRequest {
            market: name(SPOT),
            is_buy: true,
            size: (AT_EACH as u128 / 2) * UNIT / limit as u128,
            limit_price: limit,
            reduce_only: false,
        };
        let place = platform
            .venue_place_at(client, venue, &authority, vec![order])
            .await
            .unwrap();
        let buy = through(place, 0).await.unwrap();
        client.send(vec![buy], &[executor]).await.unwrap();
        let held = platform
            .margin_account_at(client, venue, &authority)
            .await
            .unwrap()
            .map_or(0, |account| account.size_in(&name(SPOT)));
        assert!(held > 0, "the station holds spot at venue {index}");
    }

    // Stopped by the admin, then closed by the keeper in one transaction
    // through all three venues once the halt's grace has passed.
    client
        .send(vec![platform.ix_kill(&admin.pubkey(), &v.vault)], &[admin])
        .await
        .unwrap();
    let halted = platform.vault(client, &v.vault).await.unwrap();
    client
        .rpc
        .time_travel(halted.halted_at + halted.timers.halt_grace + 1)
        .await
        .unwrap();
    // The keeper as it starts again: its table is found on the cluster, not
    // made anew, and the close is simulated through it as it is sent.
    let restarted = connect(&cluster).await;
    let started = Instant::now();
    let closed = loop {
        platform.settle_all(client, keeper, &v).await.unwrap();
        let vault = platform.vault(client, &v.vault).await.unwrap();
        let stopped = platform.station(client, &v.station).await.unwrap().stopped;
        let ixs = platform
            .ixs_close_in_one(client, &v, &vault, &keeper.pubkey(), stopped)
            .await
            .unwrap();
        let size = client.wire_size(ixs.clone(), &keeper.pubkey()).unwrap();
        assert!(
            size <= chain::solana::client::PACKET_DATA_SIZE,
            "{size} bytes as sent"
        );
        let sent = match restarted.simulate(ixs.clone(), &keeper.pubkey()).await {
            Ok(_) => restarted.send(ixs, &[keeper]).await,
            Err(error) => Err(error),
        };
        match sent {
            Ok(landed) => break landed,
            // Outside the slot's valuation window: the next one.
            Err(error) if error.downcast_ref::<TransactionFailed>().is_some() => {
                assert!(
                    started.elapsed() < PATIENCE,
                    "the close never went through: {error:#}"
                );
            }
            Err(error) => panic!("the close could not be sent: {error:#}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(version_of(client, &closed).await, 0);
    let keeper_tables = client
        .rpc
        .program_accounts(&tables::PROGRAM, &tables::owned_by(&keeper.pubkey()))
        .await
        .unwrap();
    assert_eq!(keeper_tables.len(), 1, "the keeper's one table, reused");
    assert!(
        closed
            .record
            .instructions
            .iter()
            .any(|call| call.program == station::ID
                && p.venues.iter().all(|venue| call.accounts.contains(venue))),
        "the close unwinds the station through all three venues"
    );
    assert_eq!(
        platform.vault(client, &v.vault).await.unwrap().state,
        vault::state::VaultState::Closed
    );
    let record = platform.station(client, &v.station).await.unwrap();
    for (index, venue) in p.venues.iter().enumerate() {
        let held = platform
            .margin_account_at(client, venue, &record.drivers[index].authority)
            .await
            .unwrap()
            .map_or(0, |account| account.size_in(&name(SPOT)));
        assert_eq!(held, 0, "nothing is left at venue {index}");
    }
}
