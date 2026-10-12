//! Every decoder against the programs' own types: each is filled with a
//! distinct value in every field, written as the program writes it (its
//! discriminator, then Borsh), and each field read back here must be the
//! value put in. Two fields of the same type swapped, a field missed or a
//! discriminator changed fails.

use anchor_lang::{AccountSerialize, Discriminator};
use shonin_vault::state as vs;

use super::Pubkey;
use super::accounts::{
    self, Claim, ClaimState, Event, HaltReason, Holder, MarginAccount, Request, RequestKind,
    Station, StationLayout, Vault, VaultState, discriminator as d, events_in, token_amount,
};

fn key(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn written<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    // Anchor gives an account room it may not fill.
    out.extend_from_slice(&[0u8; 64]);
    out
}

fn a_vault() -> vs::Vault {
    vs::Vault {
        strategy_id: 1,
        bundle_hash: [2; 32],
        author: key(3),
        admin: key(4),
        platform: key(5),
        mint: key(6),
        dollars: key(7),
        station: key(8),
        cluster: [9; 32],
        created_at: 10,
        exit_band_bps: 11,
        fees: vs::Fees {
            holding_bps: 12,
            author_bps: 13,
            platform_bps: 14,
            transaction_bps: 15,
        },
        timers: vs::Timers {
            slot_interval: 16,
            slot_lead: 17,
            slot_window: 18,
            fallback_after: 19,
            usage_freshness: 20,
            tank_escape_delay: 21,
            floor_delay: 22,
            halt_grace: 23,
            freeze_after: 24,
            wind_down: 25,
        },
        minimum_deposit: 26,
        drivers: vec![[27; 32], [28; 32]],
        bump: 29,
        state: vs::VaultState::WindingDown,
        halted_at: 30,
        retired_at: 31,
        halt_reason: vs::HaltReason::Retired,
        total_supply: 32,
        pending_shares: 33,
        unpaid_holding: 34,
        dead_shares: 35,
        cash: 36,
        escrow: 37,
        mark: 38,
        last_holding: 39,
        last_payment: 40,
        tank: vs::Tank {
            balance: 41,
            keep_at: 42,
            usage_settled: 43,
            usage_as_of: 44,
            last_collection: 45,
            escape_requested_at: 46,
            cap_carried: 47,
            fill_pending: true,
            fill_request: 48,
        },
        allowances: vs::Allowed {
            fuel_per_sec: 49,
            memory_mib: 50,
            requests_per_min: 51,
            bytes_per_day: 52,
            streams: 53,
            inference_ms_per_min: 54,
            gas_per_day: 55,
        },
        allowances_since: 56,
        floor_bps: 57,
        pending_floor_bps: 58,
        pending_floor_at: 59,
        request_count: 60,
        request_cursor: 61,
        exit_cursor: 62,
        priced_through: 63,
        tail_earliest: 64,
        tail_start: 65,
        previous_earliest: 66,
        previous_start: 67,
        valuation_count: 68,
        last_valued_at: 69,
        priced: true,
        latest: vs::Snapshot {
            index: 70,
            time: 71,
            gross: 72,
            gross_exit: 73,
            supply: 74,
            fee: 75,
            fee_exit: 76,
            cash: 77,
            extra: 78,
            sent: 79,
        },
        exit_paid: 80,
        deposit_net: 81,
        deposit_supply: 82,
        deposits_for: 83,
        claim_count: 84,
        queued_claims: 85,
        units: 86,
        unit_scale: 87,
        unit_reserve: 88,
        per_unit: {
            let mut bytes = [0u8; 32];
            bytes[0] = 89;
            bytes[31] = 1;
            bytes
        },
        sent_total: 90,
        station_booked: 91,
        restricted_hash: [92; 32],
        held_total: 93,
        held_author: 94,
        held_platform: 95,
    }
}

#[test]
fn every_discriminator_is_the_programs_own() {
    assert_eq!(d::VAULT, vs::Vault::DISCRIMINATOR);
    assert_eq!(d::HOLDER, vs::Holder::DISCRIMINATOR);
    assert_eq!(d::REQUEST, vs::Request::DISCRIMINATOR);
    assert_eq!(d::CLAIM, vs::Claim::DISCRIMINATOR);
    assert_eq!(d::STATION, shonin_station::state::Station::DISCRIMINATOR);
    assert_eq!(
        d::MARGIN_ACCOUNT,
        shonin_local_venue::MarginAccount::DISCRIMINATOR
    );
    use shonin_vault::events as e;
    assert_eq!(d::REQUESTED, e::Requested::DISCRIMINATOR);
    assert_eq!(d::DEPOSITED, e::Deposited::DISCRIMINATOR);
    assert_eq!(d::REFUNDED, e::Refunded::DISCRIMINATOR);
    assert_eq!(d::WITHDRAWN, e::Withdrawn::DISCRIMINATOR);
    assert_eq!(d::CLAIM_BOOKED, e::ClaimBooked::DISCRIMINATOR);
    assert_eq!(d::CLAIM_TRIED, e::ClaimTried::DISCRIMINATOR);
    assert_eq!(d::CLAIM_PAID, e::ClaimPaid::DISCRIMINATOR);
    assert_eq!(d::PRO_RATA_TAKEN, e::ProRataTaken::DISCRIMINATOR);
    assert_eq!(d::UNITS_PAID, e::UnitsPaid::DISCRIMINATOR);
    assert_eq!(d::HELD, e::Held::DISCRIMINATOR);
    assert_eq!(d::HELD_COLLECTED, e::HeldCollected::DISCRIMINATOR);
    assert_eq!(d::TANK_FUNDED, e::TankFunded::DISCRIMINATOR);
    assert_eq!(d::TANK_WITHDRAWN, e::TankWithdrawn::DISCRIMINATOR);
    assert_eq!(d::TANK_FILLED, e::TankFilled::DISCRIMINATOR);
    assert_eq!(d::EVENT_CALL, anchor_lang::event::EVENT_IX_TAG_LE);
}

#[test]
fn a_vault_account_reads_back_every_field_the_program_wrote() {
    let v = Vault::read(&written(&a_vault())).unwrap();
    assert_eq!(v.strategy_id, 1);
    assert_eq!(v.bundle_hash, [2; 32]);
    assert_eq!(v.author, key(3));
    assert_eq!(v.admin, key(4));
    assert_eq!(v.platform, key(5));
    assert_eq!(v.mint, key(6));
    assert_eq!(v.dollars, key(7));
    assert_eq!(v.station, key(8));
    assert_eq!(v.cluster, [9; 32]);
    assert_eq!(v.created_at, 10);
    assert_eq!(v.exit_band_bps, 11);
    assert_eq!(
        (
            v.fees.holding_bps,
            v.fees.author_bps,
            v.fees.platform_bps,
            v.fees.transaction_bps
        ),
        (12, 13, 14, 15)
    );
    let t = v.timers;
    assert_eq!(
        [
            t.slot_interval,
            t.slot_lead,
            t.slot_window,
            t.fallback_after,
            t.usage_freshness,
            t.tank_escape_delay,
            t.floor_delay,
            t.halt_grace,
            t.freeze_after,
            t.wind_down
        ],
        [16, 17, 18, 19, 20, 21, 22, 23, 24, 25]
    );
    assert_eq!(v.minimum_deposit, 26);
    assert_eq!(v.drivers, vec![[27; 32], [28; 32]]);
    assert_eq!(v.state, VaultState::WindingDown);
    assert_eq!(v.halted_at, 30);
    assert_eq!(v.retired_at, 31);
    assert_eq!(v.halt_reason, HaltReason::Retired);
    assert_eq!(v.total_supply, 32);
    assert_eq!(v.pending_shares, 33);
    assert_eq!(v.unpaid_holding, 34);
    assert_eq!(v.dead_shares, 35);
    assert_eq!(v.cash, 36);
    assert_eq!(v.escrow, 37);
    assert_eq!(v.mark, 38);
    let tank = v.tank;
    assert_eq!(
        [
            tank.balance,
            tank.keep_at,
            tank.usage_settled,
            tank.usage_as_of,
            tank.last_collection,
            tank.escape_requested_at
        ],
        [41, 42, 43, 44, 45, 46]
    );
    assert_eq!(tank.cap_carried, 47);
    assert!(tank.fill_pending);
    assert_eq!(tank.fill_request, 48);
    let a = v.allowances;
    assert_eq!(
        (
            a.fuel_per_sec,
            a.memory_mib,
            a.requests_per_min,
            a.bytes_per_day,
            a.streams,
            a.inference_ms_per_min,
            a.gas_per_day
        ),
        (49, 50, 51, 52, 53, 54, 55)
    );
    assert_eq!(v.allowances_since, 56);
    assert_eq!(v.floor_bps, 57);
    assert_eq!(v.pending_floor_bps, 58);
    assert_eq!(v.pending_floor_at, 59);
    assert_eq!(v.request_count, 60);
    assert_eq!(v.request_cursor, 61);
    assert_eq!(v.priced_through, 63);
    assert_eq!(v.valuation_count, 68);
    assert_eq!(v.last_valued_at, 69);
    assert!(v.priced);
    let s = v.latest;
    assert_eq!((s.index, s.time, s.cash), (70, 71, 77));
    assert_eq!(
        [
            s.gross,
            s.gross_exit,
            s.supply,
            s.fee,
            s.fee_exit,
            s.extra,
            s.sent
        ],
        [72, 73, 74, 75, 76, 78, 79]
    );
    assert_eq!(v.claim_count, 84);
    assert_eq!(v.queued_claims, 85);
    assert_eq!(v.units, 86);
    assert_eq!(v.unit_scale, 87);
    assert_eq!(v.unit_reserve, 88);
    assert_eq!(
        v.per_unit,
        alloy_primitives::U256::from(89u8) + (alloy_primitives::U256::from(1u8) << 248)
    );
    assert_eq!(v.restricted_hash, [92; 32]);
    assert_eq!(v.held_total, 93);
    assert_eq!(v.held_author, 94);
    assert_eq!(v.held_platform, 95);
}

#[test]
fn every_vault_state_and_halt_reason_reads_as_the_program_numbers_it() {
    for (theirs, ours) in [
        (vs::VaultState::None, VaultState::None),
        (vs::VaultState::Open, VaultState::Open),
        (vs::VaultState::WindingDown, VaultState::WindingDown),
        (vs::VaultState::Frozen, VaultState::Frozen),
        (vs::VaultState::Killed, VaultState::Killed),
        (vs::VaultState::Closed, VaultState::Closed),
    ] {
        let mut vault = a_vault();
        vault.state = theirs;
        assert_eq!(Vault::read(&written(&vault)).unwrap().state, ours);
    }
    for (theirs, ours) in [
        (vs::HaltReason::None, HaltReason::None),
        (vs::HaltReason::Killed, HaltReason::Killed),
        (vs::HaltReason::Sealed, HaltReason::Sealed),
        (vs::HaltReason::Frozen, HaltReason::Frozen),
        (vs::HaltReason::Retired, HaltReason::Retired),
    ] {
        let mut vault = a_vault();
        vault.halt_reason = theirs;
        assert_eq!(Vault::read(&written(&vault)).unwrap().halt_reason, ours);
    }
}

#[test]
fn another_accounts_bytes_never_read_as_a_vault() {
    let holder = written(&a_holder());
    assert!(Vault::read(&holder).is_none());
    assert!(Vault::read(&[]).is_none());
    let mut short = written(&a_vault());
    short.truncate(200);
    assert!(Vault::read(&short).is_none());
}

#[test]
fn ready_to_close_follows_the_program() {
    let mut vault = a_vault();
    vault.halted_at = 100;
    vault.priced = true;
    vault.latest.time = 101;
    vault.minimum_deposit = 10_000_000;
    vault.latest.cash = 5_000;
    vault.latest.gross = 5_001;
    vault.latest.extra = 0;
    assert_eq!(
        Vault::read(&written(&vault)).unwrap().ready_to_close(),
        vault.ready_to_close()
    );
    assert!(vault.ready_to_close());
    vault.latest.gross = 5_000_000;
    assert_eq!(
        Vault::read(&written(&vault)).unwrap().ready_to_close(),
        vault.ready_to_close()
    );
    assert!(!vault.ready_to_close());
}

fn a_holder() -> vs::Holder {
    vs::Holder {
        vault: key(1),
        owner: key(2),
        shares: 3,
        statement: [4; 32],
        units: 5,
        unit_debt: 6,
        unit_credit: 7,
        units_since: 8,
        pending: 9,
        payer: key(10),
        bump: 11,
    }
}

#[test]
fn a_holder_reads_back_every_field() {
    let h = Holder::read(&written(&a_holder())).unwrap();
    assert_eq!((h.vault, h.owner, h.payer), (key(1), key(2), key(10)));
    assert_eq!((h.shares, h.units, h.unit_debt), (3, 5, 6));
    assert_eq!(h.statement, [4; 32]);
    assert_eq!((h.unit_credit, h.units_since), (7, 8));
    assert_eq!(h.pending, 9);
}

#[test]
fn units_claimable_is_what_the_programs_accrue_credits() {
    // 3 units at 2.5 dollar-units each (scaled), 4 already counted, 1
    // credited: earned 7 (7.5 rounded down), 3 more, 4 in all.
    let scale = alloy_primitives::U256::from(accounts::UNIT_SCALE);
    let per_unit = scale * alloy_primitives::U256::from(5u8) / alloy_primitives::U256::from(2u8);
    let mut holder = a_holder();
    holder.units = 3;
    holder.unit_debt = 4;
    holder.unit_credit = 1;
    let ours = Holder::read(&written(&holder))
        .unwrap()
        .units_claimable(per_unit)
        .unwrap();
    let mut vault = a_vault();
    vault.per_unit = per_unit.to_le_bytes();
    shonin_vault::ledger::accrue(&vault, &mut holder).unwrap();
    assert_eq!(ours, holder.unit_credit);
    assert_eq!(ours, 4);
    // Earned no more than the debt: only the credit.
    holder.unit_debt = 100;
    holder.unit_credit = 2;
    let ours = Holder::read(&written(&holder))
        .unwrap()
        .units_claimable(per_unit)
        .unwrap();
    shonin_vault::ledger::accrue(&vault, &mut holder).unwrap();
    assert_eq!(ours, holder.unit_credit);
}

#[test]
fn a_request_reads_back_every_field_and_its_filter_offsets_hold() {
    let request = vs::Request {
        vault: key(1),
        id: 2,
        owner: key(3),
        kind: vs::RequestKind::TankFill,
        earliest: 4,
        done: true,
        amount: 5,
        reference: 6,
        payer: key(7),
        held: 8,
        held_for: key(9),
        bump: 10,
        reserved: [11; 16],
    };
    let bytes = written(&request);
    let r = Request::read(&bytes).unwrap();
    assert_eq!(
        (r.vault, r.owner, r.payer, r.held_for),
        (key(1), key(3), key(7), key(9))
    );
    assert_eq!((r.id, r.earliest, r.reference, r.held), (2, 4, 6, 8));
    assert_eq!(r.kind, RequestKind::TankFill);
    assert!(r.done);
    assert_eq!(r.amount, 5);
    use accounts::request_at as at;
    assert_eq!(&bytes[at::VAULT..at::VAULT + 32], key(1).as_ref());
    assert_eq!(&bytes[at::ID..at::ID + 8], &2u64.to_le_bytes());
    assert_eq!(&bytes[at::HELD_FOR..at::HELD_FOR + 32], key(9).as_ref());
    for (theirs, ours) in [
        (vs::RequestKind::Deposit, RequestKind::Deposit),
        (vs::RequestKind::Redeem, RequestKind::Redeem),
        (vs::RequestKind::Claim, RequestKind::Claim),
        (vs::RequestKind::TankFill, RequestKind::TankFill),
    ] {
        let mut request = request.clone();
        request.kind = theirs;
        assert_eq!(Request::read(&written(&request)).unwrap().kind, ours);
    }
}

#[test]
fn a_claim_reads_back_every_field_and_its_owner_offset_holds() {
    let claim = vs::Claim {
        id: 1,
        owner: key(2),
        shares: 3,
        booked_at: 4,
        last_attempt_at: 5,
        state: vs::ClaimState::Queued,
        paid: 6,
        request: 7,
        payer: key(8),
        held: 9,
        bump: 10,
    };
    let bytes = written(&claim);
    let c = Claim::read(&bytes).unwrap();
    assert_eq!(
        (
            c.id,
            c.booked_at,
            c.last_attempt_at,
            c.paid,
            c.request,
            c.held
        ),
        (1, 4, 5, 6, 7, 9)
    );
    assert_eq!((c.owner, c.payer), (key(2), key(8)));
    assert_eq!(c.shares, 3);
    assert_eq!(c.state, ClaimState::Queued);
    let at = accounts::CLAIM_OWNER_AT;
    assert_eq!(&bytes[at..at + 32], key(2).as_ref());
    for (theirs, ours) in [
        (vs::ClaimState::None, ClaimState::None),
        (vs::ClaimState::Pending, ClaimState::Pending),
        (vs::ClaimState::Waiting, ClaimState::Waiting),
        (vs::ClaimState::Queued, ClaimState::Queued),
        (vs::ClaimState::Paid, ClaimState::Paid),
        (vs::ClaimState::Fallback, ClaimState::Fallback),
    ] {
        let mut claim = claim.clone();
        claim.state = theirs;
        assert_eq!(Claim::read(&written(&claim)).unwrap().state, ours);
    }
}

/// A driver's rules with a position of every kind the first programs knew.
fn first_rules(n: u8) -> Vec<shonin_driver::Rule> {
    use shonin_driver::{Class, Own, Position, Rule};
    vec![
        Rule {
            discriminator: vec![n, n, n],
            class: Class::Exit,
            positions: vec![
                Position {
                    index: 1,
                    own: Own::StationToken(Some(2)),
                },
                Position {
                    index: 3,
                    own: Own::StationAddress(vec![4, 5]),
                },
                Position {
                    index: 6,
                    own: Own::Fixed(7),
                },
            ],
        },
        Rule {
            discriminator: vec![],
            class: Class::Trade,
            positions: vec![
                Position {
                    index: 0,
                    own: Own::StationToken(None),
                },
                Position {
                    index: 1,
                    own: Own::Station,
                },
                Position {
                    index: 2,
                    own: Own::StationDollars,
                },
            ],
        },
    ]
}

fn first_pinned(n: u8) -> shonin_station::state::first::Pinned {
    shonin_station::state::first::Pinned {
        name: [n; 32],
        program: key(n + 1),
        venue: key(n + 2),
        authority: key(n + 3),
        authority_bump: n + 4,
        dollars: key(n + 5),
        key: key(n + 6),
        rules: first_rules(n),
        fixed: vec![vec![key(n + 7)], vec![key(n + 8), key(n + 9)]],
        addresses: vec![shonin_station::state::StationAddress {
            prefix: b"account".to_vec(),
            address: key(n + 10),
        }],
    }
}

fn first_station(drivers: &[u8]) -> shonin_station::state::first::Station {
    shonin_station::state::first::Station {
        vault: key(1),
        vault_program: key(2),
        vault_dollars: key(3),
        dollars: key(4),
        mint: key(5),
        bundle_hash: [6; 32],
        cluster: [7; 32],
        exit_band_bps: 8,
        idle: 9,
        sent_home: 10,
        stopped: true,
        stopped_at: 11,
        payer: key(12),
        bump: 13,
        drivers: drivers.iter().map(|n| first_pinned(*n)).collect(),
    }
}

/// A station in the current layout with every driver's new lists filled,
/// and a rule naming each kind of position the current programs added.
fn current_station(drivers: &[u8]) -> shonin_station::state::Station {
    use shonin_driver::{Class, Own, Position, Rule};
    use shonin_station::state::{PositionMint, Station as TheirStation};
    let mut station = TheirStation::from(first_station(drivers));
    for (i, driver) in station.drivers.iter_mut().enumerate() {
        let n = drivers[i];
        driver.rules.push(Rule {
            discriminator: vec![n, 1],
            class: Class::Trade,
            positions: vec![
                Position {
                    index: 0,
                    own: Own::StationMint(2),
                },
                Position {
                    index: 1,
                    own: Own::StationPositionToken(0),
                },
                Position {
                    index: 2,
                    own: Own::StationPositionHeld,
                },
            ],
        });
        driver.position_mints = (0..=i as u8)
            .map(|slot| PositionMint {
                address: key(n + 11 + slot),
                bump: slot,
            })
            .collect();
        driver.token_accounts = vec![key(n + 15), key(n + 16)];
    }
    station
}

/// An account's bytes exactly as the program writes them: a station is made
/// at its size, with no room past its record.
fn exact<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    out
}

/// The first layout's bytes: the station's discriminator, then Borsh.
fn first_written(station: &shonin_station::state::first::Station) -> Vec<u8> {
    use anchor_lang::AnchorSerialize;
    let mut out = shonin_station::state::Station::DISCRIMINATOR.to_vec();
    station.serialize(&mut out).unwrap();
    out
}

fn assert_groups(ours: &Station, drivers: &[u8]) {
    assert_eq!(ours.vault, key(1));
    assert!(ours.stopped);
    assert_eq!(ours.drivers.len(), drivers.len());
    for (driver, n) in ours.drivers.iter().zip(drivers.iter().copied()) {
        assert_eq!(driver.name, [n; 32]);
        assert_eq!(driver.pinned.program, key(n + 1));
        assert_eq!(driver.pinned.venue, key(n + 2));
        assert_eq!(driver.pinned.authority, key(n + 3));
        assert_eq!(driver.pinned.dollars, key(n + 5));
    }
}

#[test]
fn a_station_reads_back_each_pinned_driver_in_order_past_its_rules() {
    for drivers in [&[20u8][..], &[20, 40], &[20, 40, 60]] {
        let station = current_station(drivers);
        assert!(station.drivers.iter().all(|d| !d.position_mints.is_empty()));
        let (ours, layout) = Station::read_with_layout(&exact(&station)).unwrap();
        assert_eq!(layout, StationLayout::Current);
        assert_groups(&ours, drivers);
    }
}

#[test]
fn a_first_layout_station_reads_back_the_driver_groups_its_migrated_form_does() {
    use shonin_station::state::Station as TheirStation;
    for drivers in [&[20u8][..], &[20, 40], &[20, 40, 60]] {
        let old = first_station(drivers);
        let (ours, layout) = Station::read_with_layout(&first_written(&old)).unwrap();
        assert_eq!(layout, StationLayout::First);
        assert_groups(&ours, drivers);
        // What `migrate_station` writes: the same values, the new lists empty.
        let migrated = exact(&TheirStation::from(old));
        let (after, layout) = Station::read_with_layout(&migrated).unwrap();
        assert_eq!(layout, StationLayout::Current);
        assert_eq!(after, ours);
    }
}

#[test]
fn a_station_with_bytes_past_its_record_is_not_read() {
    for drivers in [&[20u8][..], &[20, 40]] {
        for data in [
            first_written(&first_station(drivers)),
            exact(&current_station(drivers)),
        ] {
            assert!(Station::read(&data).is_some());
            for extra in [&[1u8][..], &[0], &[0; 64], &[0, 0, 0, 7]] {
                let mut longer = data.clone();
                longer.extend_from_slice(extra);
                assert_eq!(Station::read(&longer), None, "{extra:?} past the record");
            }
        }
    }
}

/// Every account the decoders are shown, read by the app's decoder and by
/// the station program's own rule, must come out the same: the same layout,
/// the same driver groups, or refused by both.
#[test]
fn the_app_reads_every_station_as_the_station_program_does() {
    use shonin_station::state::{Layout, Station as TheirStation};
    let mut cases = Vec::new();
    for drivers in [&[][..], &[20u8], &[20, 40], &[20, 40, 60]] {
        let old = first_written(&first_station(drivers));
        let new = exact(&current_station(drivers));
        let migrated = exact(&TheirStation::from(first_station(drivers)));
        for data in [old, new, migrated] {
            for cut in 1..=9.min(data.len() - 8) {
                cases.push(data[..data.len() - cut].to_vec());
            }
            for extra in [0u8, 1, 0xff] {
                let mut longer = data.clone();
                longer.push(extra);
                cases.push(longer);
            }
            let mut other = data.clone();
            other[3] ^= 0x10;
            cases.push(other);
            cases.push(data);
        }
    }
    cases.push(Vec::new());
    cases.push(d::STATION.to_vec());
    cases.push(vec![0xab; 803]);
    let mut read = 0;
    for data in &cases {
        let ours = Station::read_with_layout(data);
        let theirs = TheirStation::read_any(data);
        match (&ours, &theirs) {
            (None, None) => {}
            (Some((ours, our_layout)), Some((theirs, their_layout))) => {
                read += 1;
                let same_layout = matches!(
                    (our_layout, their_layout),
                    (StationLayout::Current, Layout::Current)
                        | (StationLayout::First, Layout::First)
                );
                assert!(same_layout, "{our_layout:?} against {their_layout:?}");
                assert_eq!(ours.vault, theirs.vault);
                assert_eq!(ours.stopped, theirs.stopped);
                let names: Vec<_> = theirs.drivers.iter().map(|d| d.name).collect();
                assert_eq!(
                    ours.drivers.iter().map(|d| d.name).collect::<Vec<_>>(),
                    names
                );
                for (o, t) in ours.drivers.iter().zip(&theirs.drivers) {
                    assert_eq!(o.pinned.program, t.program);
                    assert_eq!(o.pinned.venue, t.venue);
                    assert_eq!(o.pinned.authority, t.authority);
                    assert_eq!(o.pinned.dollars, t.dollars);
                }
            }
            _ => panic!(
                "the app read {:?} where the program read {:?}",
                ours.map(|(_, l)| l),
                theirs.map(|(_, l)| l)
            ),
        }
    }
    assert!(read >= 12, "only {read} read");
}

#[test]
fn a_venue_account_lists_its_held_markets_in_order_at_the_venues_addresses() {
    use shonin_local_venue::{MarginAccount as Theirs, Position};
    let account = Theirs {
        master: key(1),
        payer: key(2),
        cash: -3,
        positions: vec![
            Position {
                market: [4; 32],
                size: 5,
                funding_index: -6,
            },
            Position {
                market: [7; 32],
                size: -8,
                funding_index: 9,
            },
        ],
        ratio_value: 10,
        ratio_gross: 11,
        bump: 12,
    };
    let ours = MarginAccount::read(&written(&account)).unwrap();
    assert_eq!(ours.master, key(1));
    assert_eq!(ours.markets, vec![[4; 32], [7; 32]]);
    let venue = shonin_local_venue::ID;
    assert_eq!(
        ours.market_accounts(&venue),
        vec![
            shonin_local_venue::market_address_of(&venue, &[4; 32]),
            shonin_local_venue::market_address_of(&venue, &[7; 32]),
        ]
    );
}

#[test]
fn a_token_accounts_amount_is_read_from_the_token_programs_layout() {
    use anchor_lang::solana_program::program_pack::Pack;
    let account = anchor_spl::token::spl_token::state::Account {
        mint: key(1),
        owner: key(2),
        amount: 1_234_567,
        state: anchor_spl::token::spl_token::state::AccountState::Initialized,
        ..Default::default()
    };
    let mut bytes = vec![0u8; 165];
    account.pack_into_slice(&mut bytes);
    assert_eq!(token_amount(&bytes), Some(1_234_567));
    assert_eq!(accounts::token_mint_owner(&bytes), Some((key(1), key(2))));
    assert_eq!(token_amount(&bytes[..100]), None);
}

fn said(event: impl anchor_lang::Event) -> Vec<u8> {
    event.data()
}

#[test]
fn every_event_a_flow_reads_comes_back_with_its_fields() {
    use shonin_vault::events as e;
    let cases: Vec<(Vec<u8>, Event)> = vec![
        (
            said(e::Requested {
                request_id: 1,
                address: key(2),
                owner: key(3),
                kind: vs::RequestKind::Redeem,
                earliest: 4,
                amount: 5,
            }),
            Event::Requested {
                request_id: 1,
                address: key(2),
                owner: key(3),
                kind: RequestKind::Redeem,
                earliest: 4,
                amount: 5,
            },
        ),
        (
            said(e::Deposited {
                request_id: 1,
                owner: key(2),
                assets: 3,
                shares: 4,
            }),
            Event::Deposited {
                request_id: 1,
                owner: key(2),
                assets: 3,
                shares: 4,
            },
        ),
        (
            said(e::Refunded {
                request_id: 1,
                to: key(2),
                assets: 3,
            }),
            Event::Refunded {
                request_id: 1,
                to: key(2),
                assets: 3,
            },
        ),
        (
            said(e::Withdrawn {
                request_id: 1,
                owner: key(2),
                shares: 3,
                assets: 4,
                fee_shares: 5,
            }),
            Event::Withdrawn {
                request_id: 1,
                owner: key(2),
                shares: 3,
                assets: 4,
                fee_shares: 5,
            },
        ),
        (
            said(e::ClaimBooked {
                claim_id: 1,
                owner: key(2),
                shares: 3,
            }),
            Event::ClaimBooked {
                claim_id: 1,
                owner: key(2),
                shares: 3,
            },
        ),
        (
            said(e::ClaimTried {
                claim_id: 1,
                blocked: true,
            }),
            Event::ClaimTried {
                claim_id: 1,
                blocked: true,
            },
        ),
        (
            said(e::ClaimPaid {
                claim_id: 1,
                assets: 2,
            }),
            Event::ClaimPaid {
                claim_id: 1,
                assets: 2,
            },
        ),
        (
            said(e::ProRataTaken {
                owner: key(1),
                shares: 2,
                assets: 3,
                units: 4,
            }),
            Event::ProRataTaken {
                owner: key(1),
                shares: 2,
                assets: 3,
                units: 4,
            },
        ),
        (
            said(e::UnitsPaid {
                owner: key(1),
                assets: 2,
            }),
            Event::UnitsPaid {
                owner: key(1),
                assets: 2,
            },
        ),
        (
            said(e::Held {
                to: key(1),
                assets: 2,
            }),
            Event::Held {
                to: key(1),
                assets: 2,
            },
        ),
        (
            said(e::HeldCollected {
                to: key(1),
                assets: 2,
            }),
            Event::HeldCollected {
                to: key(1),
                assets: 2,
            },
        ),
        (
            said(e::TankFunded {
                from: key(1),
                assets: 2,
            }),
            Event::TankFunded {
                from: key(1),
                assets: 2,
            },
        ),
        (
            said(e::TankWithdrawn { assets: 1 }),
            Event::TankWithdrawn { assets: 1 },
        ),
        (
            said(e::TankFilled {
                shares: 1,
                assets: 2,
            }),
            Event::TankFilled {
                shares: 1,
                assets: 2,
            },
        ),
    ];
    for (bytes, expected) in cases {
        assert_eq!(Event::read(&bytes), Some(expected));
    }
    // An event this module doesn't read.
    assert_eq!(Event::read(&said(e::MarkRaised { mark: 1 })), None);
}

fn base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn events_are_read_from_the_vaults_own_log_lines_and_its_calls_of_itself_only() {
    use shonin_vault::events as e;
    let vault = super::ids::VAULT.to_string();
    let deposited = said(e::Deposited {
        request_id: 7,
        owner: key(2),
        assets: 3,
        shares: 4,
    });
    let paid = said(e::UnitsPaid {
        owner: key(5),
        assets: 6,
    });
    let logs = vec![
        format!("Program {vault} invoke [1]"),
        "Program log: Instruction: CollectUnits".to_string(),
        format!("Program data: {}", encode(&paid)),
        // Another program saying the vault's event counts for nothing.
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]".to_string(),
        format!("Program data: {}", encode(&deposited)),
        "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success".to_string(),
        format!("Program {vault} consumed 1000 of 200000 compute units"),
        format!("Program {vault} success"),
    ];
    let mut call = d::EVENT_CALL.to_vec();
    call.extend_from_slice(&deposited);
    let calls = vec![
        (super::ids::VAULT, call.clone()),
        // The same data to another program isn't the vault's.
        (key(9), call),
    ];
    let found = events_in(&logs, &calls, base64);
    assert_eq!(
        found,
        vec![
            Event::UnitsPaid {
                owner: key(5),
                assets: 6
            },
            Event::Deposited {
                request_id: 7,
                owner: key(2),
                assets: 3,
                shares: 4
            },
        ]
    );
}

/// A venue program the vault calls can log whatever text it likes, but its
/// lines always read `Program log: ...`: shaped like the runtime's own
/// (`Program <vault> invoke [2]`, `Program <it> success`), they never make
/// its `Program data:` the vault's, and a line out of step with the
/// invocations ends what is trusted.
#[test]
fn a_called_program_s_own_log_lines_never_make_its_data_the_vault_s() {
    use shonin_vault::events as e;
    let vault = super::ids::VAULT.to_string();
    let venue = "Venue111111111111111111111111111111111111111";
    let forged = said(e::Refunded {
        request_id: 7,
        to: key(2),
        assets: 999,
    });
    let real = said(e::Refunded {
        request_id: 7,
        to: key(2),
        assets: 3,
    });
    let logs = vec![
        format!("Program {vault} invoke [1]"),
        format!("Program {venue} invoke [2]"),
        // The venue's own lines, shaped like the runtime's.
        format!("Program log: Program {venue} success"),
        format!("Program log: Program {vault} invoke [2]"),
        format!("Program data: {}", encode(&forged)),
        format!("Program {venue} consumed 1000 of 200000 compute units"),
        format!("Program {venue} success"),
        format!("Program data: {}", encode(&real)),
        format!("Program {vault} success"),
    ];
    assert_eq!(
        events_in(&logs, &[], base64),
        vec![Event::Refunded {
            request_id: 7,
            to: key(2),
            assets: 3
        }]
    );
    // An invocation the runtime couldn't have logged (a depth out of step)
    // ends what is trusted: nothing after it is the vault's.
    let logs = vec![
        format!("Program {vault} invoke [1]"),
        format!("Program {vault} invoke [3]"),
        format!("Program data: {}", encode(&forged)),
    ];
    assert_eq!(events_in(&logs, &[], base64), vec![]);
    // An end naming another program than the one running, likewise.
    let logs = vec![
        format!("Program {venue} invoke [1]"),
        format!("Program {vault} success"),
        format!("Program data: {}", encode(&forged)),
    ];
    assert_eq!(events_in(&logs, &[], base64), vec![]);
}

#[test]
fn every_vault_error_number_names_the_programs_own_error() {
    use shonin_vault::error::VaultError as E;
    let all = [
        E::WrongState,
        E::NotAllowed,
        E::TooSmall,
        E::NoStatement,
        E::NotBlessed,
        E::OutsideWindow,
        E::Unsettled,
        E::AlreadyValued,
        E::NotYet,
        E::Nothing,
        E::BelowFloor,
        E::Stale,
        E::NotTheDeployer,
        E::NotAdmin,
        E::NotTheAuthor,
        E::NotReady,
        E::VaultExists,
        E::TankTooSmall,
        E::OutOfRange,
        E::TermsChanged,
        E::ChainTwice,
        E::OtherChainsNotYet,
        E::WrongMint,
        E::WrongAccount,
        E::WrongCursor,
        E::StationSilent,
        E::ListFull,
        E::NoSignature,
        E::Overflow,
        E::TooMany,
    ];
    for error in all {
        let code = u32::from(error);
        assert_eq!(accounts::vault_error(code), Some(error.name().as_str()));
    }
    assert_eq!(accounts::vault_error(5999), None);
    assert_eq!(accounts::vault_error(6000 + all.len() as u32), None);
}

#[test]
fn a_signed_totals_records_are_found_from_the_signers_key_record_as_the_services_find_them() {
    use shonin_registry::state::{ImageKind, KeyInfo};
    let signer = key(1);
    let info = KeyInfo {
        image_digest: [2; 32],
        project_id: [3; 32],
        kind: ImageKind::Executor,
        strategy_hash: [4; 32],
        vault_chain: [5; 32],
        vault: [6; 32],
        trust_root: [7; 32],
        verifier: key(8),
        blessed_until: 9,
        bump: 10,
    };
    assert_eq!(d::KEY_INFO, KeyInfo::DISCRIMINATOR);
    let vault = key(11);
    let records =
        accounts::key_records(&signer, &written(&info), &[12; 32], &vault, &[13; 32]).unwrap();
    assert_eq!(
        records.key_info,
        shonin_registry::read::key_address(&signer)
    );
    assert_eq!(
        records.image,
        shonin_registry::read::image_address(&[2; 32])
    );
    assert_eq!(
        records.project,
        shonin_registry::read::project_address(&[3; 32])
    );
    assert_eq!(
        records.blessing,
        shonin_registry::read::blessing_address(&signer, &[12; 32], &vault.to_bytes(), &[13; 32])
    );
    assert!(
        accounts::key_records(&signer, &written(&a_holder()), &[12; 32], &vault, &[13; 32])
            .is_none()
    );
}
