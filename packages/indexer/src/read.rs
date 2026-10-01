//! Turning the chain into what the site shows.
//!
//! Everything here is read from contracts and logs. A vault's value per share
//! is asked of the vault itself at each past block, so the record is not a
//! store anyone has to trust: delete this service's memory and it rebuilds
//! the same numbers from the chain.

use std::collections::HashMap;

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::Result;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::IAuthorProfiles;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{
    ClaimState as ChainClaimState, IVault, VaultState as ChainVaultState,
};
use chain::bindings::venue::ILocalVenue;
use chain::{Client, Deployments};
use shared::model::activity::{EventKind, Kill, Side, Trade, VaultEvent};
use shared::model::attestation::{Approval, Attestation, BlessedKey, Image, KeyRole};
use shared::model::bundle::{Bundle, ModelFile};
use shared::model::common::{Chain, Point};
use shared::model::registry::{Author, FeedEvent, FeedKind, Job, JobState};
use shared::model::review::{Rating, Review};
use shared::model::strategy::{Record, Standing, Strategy};
use shared::model::vault::{Claim, ClaimState, Fees, Tank, Vault, VaultState};
use shared::model::venue::{Custody, ExitClass, ExitPath, Leg, Valuation, VenueStatus};
use time::{Date, OffsetDateTime, PrimitiveDateTime};

use crate::store::Store;

/// What the indexer remembers between reads: the samples it has already taken.
#[derive(Default)]
pub struct History {
    /// Value per share, by vault, one point per block sampled.
    pub value_per_share: HashMap<Address, Vec<Point>>,
    pub aum: HashMap<Address, Vec<Point>>,
    /// The last block each vault was sampled at.
    pub sampled_to: HashMap<Address, u64>,
}

/// A moment in chain time.
pub fn moment(timestamp: u64) -> PrimitiveDateTime {
    let when =
        OffsetDateTime::from_unix_timestamp(timestamp as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    PrimitiveDateTime::new(when.date(), when.time())
}

pub fn day(timestamp: u64) -> Date {
    moment(timestamp).date()
}

/// Dollars, as the asset counts them.
fn dollars(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6
}

fn shares(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

fn name_of(id: B256) -> String {
    let end = id.0.iter().position(|byte| *byte == 0).unwrap_or(32);
    String::from_utf8_lossy(&id.0[..end]).to_string()
}

fn hash_of(value: B256) -> String {
    format!("sha256:{}", alloy::hex::encode(value))
}

fn address_of(value: Address) -> String {
    format!("{value:?}")
}

/// Every vault, as the site shows it. `vault_chain` is the chain these
/// vaults live on, by the name a manifest gives it.
#[allow(clippy::too_many_arguments)]
pub async fn strategies(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    head: u64,
    now: u64,
) -> Result<Vec<Strategy>> {
    let addresses = chain
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;

    let mut out = Vec::new();
    for address in &addresses {
        match one(
            chain,
            vault_chain,
            deployments,
            store,
            history,
            health,
            *address,
            head,
            now,
        )
        .await
        {
            Ok(strategy) => out.push(strategy),
            Err(error) => tracing::warn!(vault = %address, %error, "could not read a vault"),
        }
    }

    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn one(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    address: Address,
    head: u64,
    now: u64,
) -> Result<Strategy> {
    let strategy_id = chain.read(address, IVault::strategyIdCall {}).await?;
    let id: u64 = strategy_id.try_into().unwrap_or_default();
    let author = chain.read(address, IVault::authorCall {}).await?;
    let created = chain.read(address, IVault::createdAtCall {}).await?;
    let state = chain.read(address, IVault::stateCall {}).await?;
    let bundle_hash = chain.read(address, IVault::bundleHashCall {}).await?;
    let registered = chain
        .read(
            deployments.strategies,
            IStrategyRegistry::strategyOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;

    let slug = Strategy::slug_for(vault_chain, id);
    let days_live = (now.saturating_sub(created as u64)) / 86_400;

    let vault = read_vault(chain, vault_chain, address, &state).await?;
    let (trades, trade_count) = read_trades(chain, deployments, address).await?;
    let record = sample(
        chain,
        address,
        history,
        head,
        created as u64,
        &vault,
        trade_count,
    )
    .await?;
    let events = read_events(chain, address).await?;
    let legs = read_legs(chain, deployments, address, &vault).await?;
    let review = read_review(chain, deployments, store, strategy_id).await?;
    let attestation = read_attestation(chain, deployments, address, now).await?;
    let bundle = read_bundle(store, &registered, bundle_hash);

    let standing = standing(&state, days_live);
    let kill = events
        .iter()
        .find(|event| event.kind == EventKind::Killed)
        .map(|event| Kill {
            at: event.at,
            reason: "the admin key stopped it".to_string(),
            outcome: "every position was closed and the cash brought home".to_string(),
            reference: event.reference.clone(),
        });

    let manifest = store
        .manifest(&hash_of(registered.manifestHash))
        .map(|canonical| canonical.manifest);
    Ok(Strategy {
        id,
        slug,
        title: manifest
            .as_ref()
            .map(|manifest| manifest.description.title.clone())
            .unwrap_or_else(|| format!("Agent {id}")),
        description: manifest
            .as_ref()
            .map(|manifest| manifest.description.text.clone())
            .unwrap_or_else(|| "Its manifest is not in the store.".to_string()),
        claims: manifest
            .as_ref()
            .map(|manifest| manifest.description.claims.clone())
            .unwrap_or_default(),
        author: handle_of(chain, deployments, author).await,
        standing,
        registered: day(registered.registeredAt),
        days_live: days_live as u32,
        bundle,
        vault,
        legs,
        review,
        health: health.get(&address).cloned(),
        attestation,
        record,
        trades,
        events,
        kill,
        restricted: registered
            .restricted
            .iter()
            .map(|code| name_of(*code))
            .collect(),
        drivers: registered
            .chains
            .iter()
            .flat_map(|need| need.drivers.iter().map(|driver| name_of(*driver)))
            .collect(),
        bridges: registered
            .bridges
            .iter()
            .map(|bridge| name_of(*bridge))
            .collect(),
    })
}

fn standing(state: &ChainVaultState, days_live: u64) -> Standing {
    match state {
        ChainVaultState::Killed => Standing::Killed,
        ChainVaultState::Frozen => Standing::Frozen,
        ChainVaultState::Closed => Standing::Closed,
        _ if days_live >= Strategy::DAYS_NEW as u64 => Standing::Listed,
        _ => Standing::New,
    }
}

async fn read_vault(
    chain: &Client,
    vault_chain: Chain,
    address: Address,
    state: &ChainVaultState,
) -> Result<Vault> {
    let fees = chain.read(address, IVault::feesCall {}).await?;
    let tank = chain.read(address, IVault::tankCall {}).await?;
    let value = chain.read(address, IVault::totalValueCall {}).await?;
    let idle = chain.read(address, IVault::idleCall {}).await?;
    let share_price = chain.read(address, IVault::sharePriceCall {}).await?;
    let high_water = chain.read(address, IVault::highWaterMarkCall {}).await?;
    let crystallised = chain.read(address, IVault::lastCrystallisedCall {}).await?;
    let created = chain.read(address, IVault::createdAtCall {}).await?;
    let freeze_after = chain.read(address, IVault::freezeAfterCall {}).await?;
    let exit_band = chain.read(address, IVault::exitBandBpsCall {}).await?;
    let claim_count = chain.read(address, IVault::claimCountCall {}).await?;

    let mut claims = Vec::new();
    let count: u64 = claim_count.try_into().unwrap_or(0);
    for id in 1..=count {
        let claim = chain
            .read(
                address,
                IVault::claimAtCall {
                    claimId: U256::from(id),
                },
            )
            .await?;
        claims.push(Claim {
            id: id as u32,
            owner: address_of(claim.owner),
            shares: shares(claim.shares),
            fraction_closed: shares(claim.fractionClosed),
            booked: day(claim.bookedAt as u64),
            state: match claim.state {
                ChainClaimState::Paid => ClaimState::Paid,
                ChainClaimState::Waiting => ClaimState::Waiting,
                _ => ClaimState::Pending,
            },
            settled_for: (claim.settledFor > U256::ZERO).then(|| dollars(claim.settledFor)),
        });
    }

    Ok(Vault {
        address: address_of(address),
        chain: vault_chain,
        asset: vault_chain.dollar().to_string(),
        state: match state {
            ChainVaultState::Open => VaultState::Open,
            ChainVaultState::Frozen => VaultState::Frozen,
            ChainVaultState::Killed => VaultState::Killed,
            _ => VaultState::Closed,
        },
        created: day(created as u64),
        value: dollars(value),
        idle: dollars(idle),
        shares: shares(
            chain
                .read(address, IERC20::totalSupplyCall {})
                .await
                .unwrap_or(U256::ZERO),
        ),
        share_price: dollars(share_price),
        high_water_mark: dollars(high_water),
        last_crystallised: day(crystallised as u64),
        fees: Fees {
            holding_bps: fees.streamingBps as u32,
            author_bps: fees.authorBps as u32,
            platform_bps: fees.platformBps as u32,
            transaction_bps: fees.transactionBps as u32,
        },
        exit_band_bps: exit_band as u32,
        tank: Tank {
            balance: dollars(tank.balance),
            // What it is actually charging, once it has charged anything.
            // Before that the cap is the only honest answer.
            daily_charge: if tank.lastCharge.is_zero() {
                dollars(tank.dailyCap)
            } else {
                dollars(tank.lastCharge)
            },
            target: dollars(tank.target),
            autofill_bps: tank.autofillBps as u32,
            debt: dollars(tank.debt),
        },
        // Every venue this vault trades is on its own chain, so the contract
        // serves every claim itself and nobody has to be there for it.
        exit_class: ExitClass::Direct,
        freeze_after_days: freeze_after / 86_400,
        claims,
        in_transit: None,
    })
}

/// The record, sampled from the vault itself at each past block.
#[allow(clippy::too_many_arguments)]
async fn sample(
    chain: &Client,
    address: Address,
    history: &mut History,
    head: u64,
    created: u64,
    vault: &Vault,
    trades: u32,
) -> Result<Option<Record>> {
    let from = history.sampled_to.get(&address).copied();
    let start = from.map(|block| block + 1).unwrap_or(0);
    if start > head {
        return Ok(build_record(history, address, trades));
    }

    // Sampling every block would be a lot of calls on a chain that mines every
    // second; one in twelve is a reading every twelve hours of chain time,
    // which is finer than a daily record needs.
    const EVERY: u64 = 12;
    let mut block = start.max(1);
    let mut taken = 0;
    while block <= head && taken < 400 {
        if (block.is_multiple_of(EVERY) || block == head)
            && let Ok(price) = price_at(chain, address, block).await
        {
            let timestamp = block_time(chain, block).await.unwrap_or(created);
            let entry = history.value_per_share.entry(address).or_default();
            entry.push(Point {
                date: day(timestamp),
                value: price.0,
            });
            history.aum.entry(address).or_default().push(Point {
                date: day(timestamp),
                value: price.1,
            });
            taken += 1;
        }
        block += 1;
    }
    history.sampled_to.insert(address, block.saturating_sub(1));

    // A vault with no history yet still has today.
    let entry = history.value_per_share.entry(address).or_default();
    if entry.is_empty() {
        entry.push(Point {
            date: vault.created,
            value: 1.0,
        });
    }
    Ok(build_record(history, address, trades))
}

fn build_record(history: &History, address: Address, trades: u32) -> Option<Record> {
    let series = history.value_per_share.get(&address)?;
    if series.len() < 2 {
        return None;
    }
    let aum = history.aum.get(&address).cloned().unwrap_or_default();
    Some(Record::from_series(daily(series), daily(&aum), trades))
}

/// One reading a day: the record is daily, and a day with several readings
/// keeps its last.
fn daily(points: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    for point in points {
        match out.last_mut() {
            Some(last) if last.date == point.date => *last = *point,
            _ => out.push(*point),
        }
    }
    out
}

async fn price_at(chain: &Client, address: Address, block: u64) -> Result<(f64, f64)> {
    let price = chain
        .read_at(address, IVault::sharePriceCall {}, BlockId::number(block))
        .await?;
    let value = chain
        .read_at(address, IVault::totalValueCall {}, BlockId::number(block))
        .await?;
    Ok((dollars(price), dollars(value)))
}

async fn block_time(chain: &Client, block: u64) -> Result<u64> {
    chain.block_timestamp_at(block).await
}

/// Every fill this vault has had: the last few in full, for the page, and how
/// many there were altogether, for the record.
async fn read_trades(
    chain: &Client,
    deployments: &Deployments,
    address: Address,
) -> Result<(Vec<Trade>, u32)> {
    let logs = chain
        .logs(
            &Filter::new()
                .address(deployments.venue)
                .event_signature(ILocalVenue::Filled::SIGNATURE_HASH)
                .topic1(address.into_word())
                .from_block(deployments.deployed_block),
        )
        .await?;

    let count = logs.len() as u32;
    let mut trades = Vec::new();
    for log in logs.iter().rev().take(60) {
        let Ok(filled) = log.log_decode::<ILocalVenue::Filled>() else {
            continue;
        };
        let event = filled.inner.data;
        trades.push(Trade {
            at: moment(log.block_timestamp.unwrap_or_default()),
            venue: "Local venue".to_string(),
            market: name_of(event.market),
            side: if event.isBuy { Side::Buy } else { Side::Sell },
            size: shares(event.size),
            price: dollars(event.price),
            notional: dollars(event.notional),
            kind: if event.reduceOnly {
                "reduce-only".to_string()
            } else {
                "taker".to_string()
            },
            reference: log
                .transaction_hash
                .map(|hash| format!("{hash:?}"))
                .unwrap_or_default(),
        });
    }
    Ok((trades, count))
}

async fn read_events(chain: &Client, address: Address) -> Result<Vec<VaultEvent>> {
    let logs = chain
        .logs(&Filter::new().address(address).from_block(0u64))
        .await?;
    let mut events = Vec::new();
    for log in logs.iter().rev().take(60) {
        let at = moment(log.block_timestamp.unwrap_or_default());
        let reference = log
            .transaction_hash
            .map(|hash| format!("{hash:?}"))
            .unwrap_or_default();
        let (kind, summary, amount) = if let Ok(event) = log.log_decode::<IVault::Deposited>() {
            (
                EventKind::Deposited,
                "a depositor came in".to_string(),
                Some(dollars(event.inner.data.assets)),
            )
        } else if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
            (
                EventKind::Withdrawn,
                "a depositor left".to_string(),
                Some(dollars(event.inner.data.assets)),
            )
        } else if let Ok(event) = log.log_decode::<IVault::ClaimBooked>() {
            (
                EventKind::ClaimBooked,
                format!(
                    "a withdrawal the idle balance could not cover: claim {}",
                    event.inner.data.claimId
                ),
                None,
            )
        } else if let Ok(event) = log.log_decode::<IVault::ClaimSettled>() {
            (
                EventKind::ClaimSettled,
                format!("claim {} settled", event.inner.data.claimId),
                Some(dollars(event.inner.data.assets)),
            )
        } else if log.log_decode::<IVault::Crystallised>().is_ok() {
            (
                EventKind::Crystallised,
                "fees crystallised as new shares".to_string(),
                None,
            )
        } else if let Ok(event) = log.log_decode::<IVault::CapitalMoved>() {
            (
                EventKind::CapitalMoved,
                format!(
                    "{} to {}",
                    name_of(event.inner.data.from),
                    name_of(event.inner.data.to)
                ),
                Some(dollars(event.inner.data.amount)),
            )
        } else if let Ok(event) = log.log_decode::<IVault::TankFunded>() {
            (
                EventKind::TankFunded,
                "the tank was topped up".to_string(),
                Some(dollars(event.inner.data.amount)),
            )
        } else if let Ok(event) = log.log_decode::<IVault::TankCharged>() {
            (
                EventKind::TankCharged,
                format!("the tank paid for {}", name_of(event.inner.data.kind)),
                Some(dollars(event.inner.data.amount)),
            )
        } else if log.log_decode::<IVault::Frozen>().is_ok() {
            (
                EventKind::Frozen,
                "a claim went unserved and the vault froze".to_string(),
                None,
            )
        } else if log.log_decode::<IVault::Killed>().is_ok() {
            (EventKind::Killed, "the vault was stopped".to_string(), None)
        } else if let Ok(event) = log.log_decode::<IVault::KeyApproved>() {
            (
                EventKind::Blessed,
                format!("{:?} may trade this vault", event.inner.data.key),
                None,
            )
        } else {
            continue;
        };
        events.push(VaultEvent {
            at,
            kind,
            summary,
            amount,
            reference,
        });
    }
    Ok(events)
}

async fn read_legs(
    chain: &Client,
    deployments: &Deployments,
    address: Address,
    vault: &Vault,
) -> Result<Vec<Leg>> {
    let on_venue = chain
        .read(
            deployments.venue,
            ILocalVenue::accountValueCall { master: address },
        )
        .await?;
    let held = chain
        .read(
            deployments.venue,
            ILocalVenue::marketsHeldByCall { master: address },
        )
        .await?;
    let share = if vault.value > 0.0 {
        (dollars(on_venue) / vault.value).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Ok(vec![Leg {
        venue: "Local venue".to_string(),
        chain: vault.chain,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "the venue's own account reads".to_string(),
        },
        account: address_of(address),
        key: "an agent the vault approved, trade-only".to_string(),
        can: vec![
            "place and cancel orders".to_string(),
            format!(
                "hold positions in {}",
                if held.is_empty() {
                    "any listed market".to_string()
                } else {
                    held.iter()
                        .map(|id| name_of(*id))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        ],
        cannot: vec![
            "withdraw anything".to_string(),
            "approve another key".to_string(),
            "send a dollar anywhere but back to the vault".to_string(),
        ],
        status: VenueStatus::Ok,
        note: None,
    }])
}

/// A strategy's one review, from the chain, with the rest of its report from
/// the store. Refusals have no vault, so they are read with the job instead.
pub async fn read_review(
    chain: &Client,
    deployments: &Deployments,
    store: &Store,
    strategy_id: U256,
) -> Result<Option<Review>> {
    let entry = chain
        .read(
            deployments.reviews,
            IReviewRegistry::reviewOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let signed = entry.review;
    if signed.reviewedAt == 0 {
        return Ok(None);
    }
    let report = store.report(&hash_of(signed.reportHash));
    let missing = || "The report is not in the store.".to_string();
    Ok(Some(Review {
        ratings: signed
            .ratings
            .map(|code| Rating::from_code(code).unwrap_or(Rating::CouldntTell)),
        findings: report
            .as_ref()
            .map(|report| report.findings.clone())
            .unwrap_or_default(),
        summary: report
            .as_ref()
            .map(|report| report.summary.clone())
            .unwrap_or_else(missing),
        reviewed: day(signed.reviewedAt),
        refused: entry.refused,
        sealed: report.as_ref().is_some_and(|report| report.sealed),
        reviewer_image: hash_of(signed.reviewerImage),
        model_id_hash: hash_of(signed.modelIdHash),
        prompt_hash: hash_of(signed.promptHash),
        report_hash: hash_of(signed.reportHash),
        limits: report
            .map(|report| report.limits)
            .unwrap_or_else(|| vec![missing()]),
    }))
}

async fn read_attestation(
    chain: &Client,
    deployments: &Deployments,
    address: Address,
    now: u64,
) -> Result<Option<Attestation>> {
    let logs = chain
        .logs(
            &Filter::new()
                .address(deployments.attestation)
                .event_signature(IAttestationRegistry::Blessed::SIGNATURE_HASH)
                .topic2(address.into_word())
                .from_block(deployments.deployed_block),
        )
        .await?;

    let mut keys = Vec::new();
    for log in logs.iter().rev().take(4) {
        let Ok(blessed) = log.log_decode::<IAttestationRegistry::Blessed>() else {
            continue;
        };
        let signer = blessed.inner.signer;
        let event = blessed.inner.data;
        if event.blessedUntil < now {
            continue;
        }
        keys.push(BlessedKey {
            address: address_of(signer),
            zone: "this machine".to_string(),
            blessed_until: moment(event.blessedUntil),
            proof_tx: log
                .transaction_hash
                .map(|hash| format!("{hash:?}"))
                .unwrap_or_default(),
            role: KeyRole::Primary,
        });
    }
    if keys.is_empty() {
        return Ok(None);
    }

    let image = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: alloy::primitives::keccak256("shonin-dev-image:executor"),
            },
        )
        .await?;
    let builder = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: alloy::primitives::keccak256("shonin-dev-image:builder"),
            },
        )
        .await?;
    let ttl = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessingTtlCall {},
        )
        .await?;

    Ok(Some(Attestation {
        image: Image {
            digest: hash_of(alloy::primitives::keccak256("shonin-dev-image:executor")),
            allowlisted: day(image.allowedAt as u64),
            kind: "executor".to_string(),
        },
        keys,
        approvals: vec![Approval {
            venue: "Local venue".to_string(),
            detail: format!("agent on the vault's account {}", address_of(address)),
            custody: Custody::VenueEnforced,
        }],
        builder: Image {
            digest: hash_of(alloy::primitives::keccak256("shonin-dev-image:builder")),
            allowlisted: day(builder.allowedAt as u64),
            kind: "builder".to_string(),
        },
        trust_root: "dev-local".to_string(),
        blessing_ttl_hours: (ttl / 3_600) as u32,
    }))
}

fn read_bundle(
    store: &Store,
    registered: &chain::bindings::strategies::Strategy,
    bundle_hash: B256,
) -> Bundle {
    let hash = hash_of(bundle_hash);
    let canonical = store.manifest(&hash_of(registered.manifestHash));
    Bundle {
        bundle_hash: hash.clone(),
        component_hash: store
            .component_hash(&hash)
            .unwrap_or_else(|| "not in the store".to_string()),
        source_hash: hash_of(registered.sourceHash),
        manifest_hash: hash_of(registered.manifestHash),
        world: canonical
            .as_ref()
            .map(|canonical| canonical.world.clone())
            .unwrap_or_else(|| "shonin:agent@0.1.0".to_string()),
        toolchain: store
            .toolchain(&hash)
            .unwrap_or_else(|| "not in the store".to_string()),
        builder_image: canonical
            .map(|canonical| canonical.builder_image)
            .unwrap_or_else(|| hash_of(alloy::primitives::keccak256("shonin-dev-image:builder"))),
        models: Vec::<ModelFile>::new(),
        source: store.source(&hash_of(registered.sourceHash)),
        source_files: store.source_files(&hash_of(registered.sourceHash)),
        built_in_seconds: store.built_in_seconds(&hash),
    }
}

/// What to call an author: the handle they chose, or their address if they
/// chose none. Nothing depends on it either way.
async fn handle_of(chain: &Client, deployments: &Deployments, author: Address) -> String {
    match chain
        .read(
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author },
        )
        .await
    {
        Ok(profile) if !profile.handle.is_empty() => profile.handle,
        _ => address_of(author),
    }
}

/// Everyone who has published, plus anyone who said who they are.
pub async fn authors(chain: &Client, deployments: &Deployments) -> Result<Vec<Author>> {
    let mut out: Vec<Author> = Vec::new();
    let owners = chain
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    let mut seen: Vec<Address> = Vec::new();
    for vault in owners {
        let address = chain.read(vault, IVault::authorCall {}).await?;
        if seen.contains(&address) {
            continue;
        }
        seen.push(address);
        let profile = chain
            .read(
                deployments.profiles,
                IAuthorProfiles::profileOfCall { author: address },
            )
            .await?;
        let handle = if profile.handle.is_empty() {
            address_of(address)
        } else {
            profile.handle.clone()
        };
        out.push(Author {
            handle,
            joined: day(profile.joinedAt as u64),
            blurb: if profile.blurb.is_empty() {
                "This author has not said anything about themselves.".to_string()
            } else {
                profile.blurb
            },
            links: profile
                .linkLabels
                .into_iter()
                .zip(profile.linkUrls)
                .collect(),
        });
    }
    Ok(out)
}

/// What is worth announcing, newest first.
pub async fn feed(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    strategies: &[Strategy],
) -> Result<Vec<FeedEvent>> {
    let mut out = Vec::new();

    let registrations = chain
        .logs(
            &Filter::new()
                .address(deployments.strategies)
                .event_signature(IStrategyRegistry::Registered::SIGNATURE_HASH)
                .from_block(deployments.deployed_block),
        )
        .await?;
    for log in registrations {
        let Ok(event) = log.log_decode::<IStrategyRegistry::Registered>() else {
            continue;
        };
        let id: u64 = event.inner.strategyId.try_into().unwrap_or_default();
        out.push(FeedEvent {
            at: moment(log.block_timestamp.unwrap_or_default()),
            kind: FeedKind::StrategyRegistered,
            title: format!("Agent {id} on {} was registered", vault_chain.label()),
            detail: "The reviewer read it first, then an allowlisted build service compiled it and signed what it made."
                .to_string(),
            link: strategies
                .iter()
                .any(|strategy| strategy.id == id)
                .then(|| format!("/agents/{}", Strategy::slug_for(vault_chain, id))),
            reference: hash_of(event.inner.data.bundleHash),
        });
    }

    let images = chain
        .logs(
            &Filter::new()
                .address(deployments.attestation)
                .event_signature(IAttestationRegistry::ImageProposed::SIGNATURE_HASH)
                .from_block(deployments.deployed_block),
        )
        .await?;
    for log in images {
        let Ok(event) = log.log_decode::<IAttestationRegistry::ImageProposed>() else {
            continue;
        };
        out.push(FeedEvent {
            at: moment(log.block_timestamp.unwrap_or_default()),
            kind: FeedKind::ImageProposed,
            title: "An image was proposed for the allowlist".to_string(),
            detail: "It becomes blessable when its timelock runs out, and not before.".to_string(),
            link: Some("/verify".to_string()),
            reference: hash_of(event.inner.imageDigest),
        });
    }

    for strategy in strategies {
        if let Some(kill) = &strategy.kill {
            out.push(FeedEvent {
                at: kill.at,
                kind: FeedKind::Kill,
                title: format!("{} was stopped", strategy.title),
                detail: kill.outcome.clone(),
                link: Some(format!("/agents/{}", strategy.slug)),
                reference: kill.reference.clone(),
            });
        }
        if strategy.vault.state == VaultState::Frozen {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::VaultFrozen,
                title: format!("{} froze", strategy.title),
                detail: "A claim went unserved, so no new deposits and only unwinding is left."
                    .to_string(),
                link: Some(format!("/agents/{}", strategy.slug)),
                reference: strategy.vault.address.clone(),
            });
        }
        if let Some(review) = &strategy.review {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::ReviewPublished,
                title: format!("{} was reviewed", strategy.title),
                detail: if review.is_alarming() {
                    "Its report card has serious concerns.".to_string()
                } else {
                    "Its report card is published.".to_string()
                },
                link: Some(format!("/agents/{}/review", strategy.slug)),
                reference: review.report_hash.clone(),
            });
        }
    }

    out.sort_by_key(|event| std::cmp::Reverse(event.at));
    Ok(out)
}

/// A job, as the builder reports it, with what the chain adds: its review
/// (a refusal's included), whether it has been activated, and who its author
/// is called.
pub async fn job(chain: &Client, deployments: &Deployments, store: &Store, mut job: Job) -> Job {
    if let Ok(author) = job.author.parse::<Address>() {
        job.author = handle_of(chain, deployments, author).await;
    }
    let Some(id) = job.strategy_id else {
        return job;
    };
    let strategy_id = U256::from(id);
    if let Ok(review) = read_review(chain, deployments, store, strategy_id).await {
        job.review = review;
    }
    if let Ok(vault) = chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id,
            },
        )
        .await
        && vault != Address::ZERO
    {
        job.state = JobState::Activated;
        job.strategy_slug = Some(Strategy::slug_for(job.chain, id));
    }
    job
}
