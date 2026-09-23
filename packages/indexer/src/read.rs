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
use chain::bindings::strategies::{IStrategyRegistry, VersionStatus};
use chain::bindings::vault::{
    ClaimState as ChainClaimState, IVault, VaultState as ChainVaultState,
};
use chain::bindings::venue::ILocalVenue;
use chain::{Client, Deployments};
use shared::model::activity::{EventKind, Kill, Side, Trade, VaultEvent};
use shared::model::attestation::{Approval, Attestation, BlessedKey, Image, KeyRole};
use shared::model::bundle::{Allowances, Bundle, LogLine, ModelFile};
use shared::model::common::{Audit, AuditStatus, Chain, Point};
use shared::model::registry::{Author, FeedEvent, FeedKind};
use shared::model::review::{Finding, Review};
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
    /// The venue's own spot price, for something to compare a record against.
    pub spot: Vec<Point>,
    pub spot_sampled_to: u64,
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

/// Every vault, as the site shows it.
pub async fn strategies(
    chain: &Client,
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

    // A successor names what it replaces; the page it replaces has to know.
    let claims: Vec<(String, String)> = out
        .iter()
        .filter_map(|strategy| {
            strategy
                .supersedes
                .clone()
                .map(|earlier| (earlier, strategy.slug.clone()))
        })
        .collect();
    for (earlier, successor) in claims {
        if let Some(target) = out.iter_mut().find(|strategy| strategy.slug == earlier) {
            target.superseded_by = Some(successor);
            if target.standing.is_live() {
                target.standing = Standing::Superseded;
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn one(
    chain: &Client,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    address: Address,
    head: u64,
    now: u64,
) -> Result<Strategy> {
    let strategy_id = chain.read(address, IVault::strategyIdCall {}).await?;
    let version = chain.read(address, IVault::versionCall {}).await?;
    let author = chain.read(address, IVault::authorCall {}).await?;
    let created = chain.read(address, IVault::createdAtCall {}).await?;
    let state = chain.read(address, IVault::stateCall {}).await?;
    let bundle_hash = chain.read(address, IVault::bundleHashCall {}).await?;
    let registered = chain
        .read(
            deployments.strategies,
            IStrategyRegistry::versionOfCall {
                strategyId: strategy_id,
                version,
            },
        )
        .await?;

    let id = name_of(strategy_id);
    let slug = format!("{id}-v{version}");
    let days_live = (now.saturating_sub(created as u64)) / 86_400;

    let vault = read_vault(chain, deployments, address, &state, now).await?;
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
    let review = read_review(chain, deployments, store, strategy_id, version).await?;
    let attestation = read_attestation(chain, deployments, address, now).await?;
    let bundle = read_bundle(store, &registered, bundle_hash, store.job_log(&slug));

    let standing = standing(&state, &registered.status, days_live, bundle_hash);
    let kill = events
        .iter()
        .find(|event| event.kind == EventKind::Killed)
        .map(|event| Kill {
            at: event.at,
            reason: "the admin key stopped it".to_string(),
            outcome: "every position was closed and the cash brought home".to_string(),
            reference: event.reference.clone(),
        });

    let manifest = store.manifest(&hash_of(bundle_hash));
    Ok(Strategy {
        id,
        slug,
        version,
        title: manifest
            .as_ref()
            .map(|manifest| manifest.description.title.clone())
            .unwrap_or_else(|| "Waiting for its build".to_string()),
        description: manifest
            .as_ref()
            .map(|manifest| manifest.description.text.clone())
            .unwrap_or_else(|| {
                "This strategy's source has been submitted and is waiting to be built.".to_string()
            }),
        author: handle_of(chain, deployments, author).await,
        standing,
        registered: day(if registered.registeredAt == 0 {
            created as u64
        } else {
            registered.registeredAt as u64
        }),
        days_live: days_live as u32,
        supersedes: (registered.supersedesVersion != 0).then(|| {
            format!(
                "{}-v{}",
                name_of(registered.supersedesId),
                registered.supersedesVersion
            )
        }),
        superseded_by: None,
        bundle,
        vault,
        legs,
        review,
        past_reviews: Vec::new(),
        health: health.get(&address).cloned(),
        attestation,
        record,
        trades,
        events,
        kill,
        limits: manifest
            .as_ref()
            .map(|manifest| manifest.description.limits.clone())
            .unwrap_or_default(),
        dependencies: manifest
            .map(|manifest| manifest.description.dependencies.clone())
            .unwrap_or_default(),
    })
}

fn standing(
    state: &ChainVaultState,
    status: &VersionStatus,
    days_live: u64,
    bundle: B256,
) -> Standing {
    match state {
        ChainVaultState::Killed => Standing::Killed,
        ChainVaultState::Closed if bundle == B256::ZERO => Standing::BuildFailed,
        ChainVaultState::Closed => Standing::Retired,
        _ if matches!(status, VersionStatus::Retired) => Standing::Retired,
        ChainVaultState::Building => Standing::New,
        _ if days_live >= Strategy::DAYS_TO_RANK as u64 => Standing::Ranked,
        _ => Standing::New,
    }
}

async fn read_vault(
    chain: &Client,
    deployments: &Deployments,
    address: Address,
    state: &ChainVaultState,
    now: u64,
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
    let supply = chain
        .read(address, IVault::pendingSharesCall {})
        .await
        .unwrap_or(U256::ZERO);

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

    let _ = (deployments, now, supply);
    Ok(Vault {
        address: address_of(address),
        chain: Chain::Local,
        asset: "USDC".to_string(),
        state: match state {
            ChainVaultState::Building => VaultState::Building,
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
            streaming_bps: fees.streamingBps as u32,
            author_bps: fees.authorBps as u32,
            platform_bps: fees.platformBps as u32,
            transaction_bps: fees.transactionBps as u32,
        },
        exit_band_bps: exit_band as u32,
        tank: Tank {
            balance: dollars(tank.balance),
            daily_charge: dollars(tank.lastCharge.max(tank.dailyCap)),
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
        audits: vec![
            Audit {
                subject: "Vault and factory".to_string(),
                status: AuditStatus::Unaudited,
                note: "Not yet audited. A vault's page says so from the day it opens.".to_string(),
            },
            Audit {
                subject: "Attestation registry".to_string(),
                status: AuditStatus::Unaudited,
                note: "Not yet audited.".to_string(),
            },
        ],
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
        chain: Chain::Local,
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

async fn read_review(
    chain: &Client,
    deployments: &Deployments,
    store: &Store,
    strategy_id: B256,
    version: u32,
) -> Result<Option<Review>> {
    let count = chain
        .read(
            deployments.reviews,
            IReviewRegistry::reviewCountCall {
                strategyId: strategy_id,
                version,
            },
        )
        .await?;
    if count == U256::ZERO {
        return Ok(None);
    }
    let signed = chain
        .read(
            deployments.reviews,
            IReviewRegistry::latestReviewCall {
                strategyId: strategy_id,
                version,
            },
        )
        .await?;

    let report = store.report(&hash_of(signed.reportHash));
    Ok(Some(Review {
        score: signed.score,
        findings: report
            .as_ref()
            .map(|report| report.findings.clone())
            .unwrap_or_default(),
        summary: report
            .as_ref()
            .map(|report| report.summary.clone())
            .unwrap_or_else(|| "The report is not in the store.".to_string()),
        reviewed: day(signed.reviewedAt as u64),
        reviewer_image: hash_of(signed.reviewerImage),
        model_id_hash: hash_of(signed.modelIdHash),
        prompt_hash: hash_of(signed.promptHash),
        report_hash: hash_of(signed.reportHash),
        limits: report
            .map(|report| report.limits)
            .unwrap_or_else(|| vec!["The report is not in the store.".to_string()]),
        bought_by: address_of(signed.boughtBy),
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
            audit: AuditStatus::Unaudited,
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
            audit: AuditStatus::Unaudited,
            kind: "builder".to_string(),
        },
        trust_root: "dev-local".to_string(),
        blessing_ttl_hours: (ttl / 3_600) as u32,
    }))
}

fn read_bundle(
    store: &Store,
    registered: &chain::bindings::strategies::StrategyVersion,
    bundle_hash: B256,
    log: Vec<LogLine>,
) -> Bundle {
    let hash = hash_of(bundle_hash);
    let manifest = store.manifest(&hash);
    let allowances = manifest
        .as_ref()
        .map(|manifest| manifest.runtime.allowances)
        .unwrap_or_else(shared::manifest::Allowances::modest);

    Bundle {
        bundle_hash: hash.clone(),
        component_hash: store
            .component_hash(&hash)
            .unwrap_or_else(|| "waiting for the build".to_string()),
        source_hash: hash_of(registered.sourceHash),
        manifest_hash: hash_of(registered.manifestHash),
        world: "shonin:strategy@0.1.0".to_string(),
        toolchain: store
            .toolchain(&hash)
            .unwrap_or_else(|| "waiting for the build".to_string()),
        builder_image: hash_of(alloy::primitives::keccak256("shonin-dev-image:builder")),
        models: Vec::<ModelFile>::new(),
        allowances: Allowances {
            fuel_per_sec: allowances.fuel_per_sec,
            memory_mib: allowances.memory_mib,
            requests_per_min: allowances.requests_per_min,
            bytes_per_day: allowances.bytes_per_day,
            streams: allowances.streams,
            inference_ms_per_min: allowances.inference_ms_per_min,
            placement: "anywhere".to_string(),
        },
        secrets: manifest
            .as_ref()
            .map(|manifest| manifest.secrets.clone())
            .unwrap_or_default(),
        config: manifest
            .as_ref()
            .map(|manifest| {
                manifest
                    .config
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        source: store.source(&hash_of(registered.sourceHash)),
        source_files: store.source_files(&hash_of(registered.sourceHash)),
        build_log: log,
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
        let slug = format!(
            "{}-v{}",
            name_of(event.inner.strategyId),
            event.inner.version
        );
        out.push(FeedEvent {
            at: moment(log.block_timestamp.unwrap_or_default()),
            kind: FeedKind::StrategyRegistered,
            title: format!("{slug} was registered"),
            detail: "An allowlisted build service compiled it and signed what it made.".to_string(),
            link: Some(format!("/strategies/{slug}")),
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
                title: format!("{} was stopped", strategy.slug),
                detail: kill.outcome.clone(),
                link: Some(format!("/strategies/{}", strategy.slug)),
                reference: kill.reference.clone(),
            });
        }
        if strategy.vault.state == VaultState::Frozen {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::VaultFrozen,
                title: format!("{} froze", strategy.slug),
                detail: "A claim went unserved, so no new deposits and only unwinding is left."
                    .to_string(),
                link: Some(format!("/strategies/{}", strategy.slug)),
                reference: strategy.vault.address.clone(),
            });
        }
        if let Some(review) = &strategy.review {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::ReviewPublished,
                title: format!("{} was reviewed", strategy.slug),
                detail: format!("It scored {} out of 100.", review.score),
                link: Some(format!("/strategies/{}/review", strategy.slug)),
                reference: review.report_hash.clone(),
            });
        }
    }

    out.sort_by_key(|event| std::cmp::Reverse(event.at));
    Ok(out)
}

/// The venue's own spot price, so a record has something to be read against.
pub async fn benchmark(
    chain: &Client,
    deployments: &Deployments,
    history: &mut History,
    head: u64,
) -> Result<Vec<shared::Benchmark>> {
    const EVERY: u64 = 12;
    let mut block = history.spot_sampled_to.max(1);
    let mut taken = 0;
    while block <= head && taken < 400 {
        if (block.is_multiple_of(EVERY) || block == head)
            && let Ok(market) = chain
                .read_at(
                    deployments.venue,
                    ILocalVenue::marketOfCall {
                        market: market_id("BTC"),
                    },
                    BlockId::number(block),
                )
                .await
        {
            let timestamp = chain.block_timestamp_at(block).await.unwrap_or_default();
            history.spot.push(Point {
                date: day(timestamp),
                value: dollars(market.mark),
            });
            taken += 1;
        }
        block += 1;
    }
    history.spot_sampled_to = block;

    Ok(vec![shared::Benchmark {
        label: "The asset itself".to_string(),
        note: "Holding the spot asset over the same stretch, on the same venue.".to_string(),
        series: daily(&history.spot),
    }])
}

fn market_id(name: &str) -> B256 {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    out[..bytes.len()].copy_from_slice(bytes);
    B256::from(out)
}

/// Findings, as the reviewer's report holds them.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub summary: String,
    pub findings: Vec<Finding>,
    pub limits: Vec<String>,
}
