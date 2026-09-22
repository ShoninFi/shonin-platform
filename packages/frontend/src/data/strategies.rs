//! Every strategy on the platform.
//!
//! Each one is described by the things that make it itself — what it does,
//! where it trades, what its review found — and everything derivable is
//! derived: hashes, the record, the attestation, trades, events and the
//! vault's own numbers all follow from the same seed.

use shared::{
    Allowances, Approval, Attestation, Audit, AuditStatus, BlessedKey, Bundle, Chain, Claim,
    ClaimState, Custody, EventKind, ExitClass, ExitPath, Fees, Finding, FindingCategory, Health,
    HealthEntry, HealthLevel, HealthSignal, Image, InTransit, KeyRole, Kill, Leg, LogLine,
    ModelFile, Review, Severity, Side, Source, SourceFile, Standing, Strategy, Tank, Trade,
    Valuation, Vault, VaultEvent, VaultState, VenueStatus,
};
use time::Duration;

use super::series::{self, Rng, Shape};
use super::source;
use super::{address, as_of, days_ago, hex, hours_ago, now, sha256, tx};

// ------------------------------------------------------------- allowances

fn light() -> Allowances {
    Allowances {
        fuel_per_sec: 2_000_000,
        memory_mib: 256,
        requests_per_min: 120,
        bytes_per_day: 50_000_000,
        streams: 4,
        inference_ms_per_min: 0,
        placement: "any".to_string(),
    }
}

fn quoting() -> Allowances {
    Allowances {
        fuel_per_sec: 40_000_000,
        memory_mib: 1024,
        requests_per_min: 6_000,
        bytes_per_day: 5_000_000_000,
        streams: 32,
        inference_ms_per_min: 0,
        placement: "near hyperliquid".to_string(),
    }
}

fn thinking() -> Allowances {
    Allowances {
        fuel_per_sec: 8_000_000,
        memory_mib: 2048,
        requests_per_min: 600,
        bytes_per_day: 400_000_000,
        streams: 8,
        inference_ms_per_min: 2_000,
        placement: "near robinhood-chain".to_string(),
    }
}

fn playing() -> Allowances {
    Allowances {
        fuel_per_sec: 24_000_000,
        memory_mib: 512,
        requests_per_min: 1_200,
        bytes_per_day: 120_000_000,
        streams: 6,
        inference_ms_per_min: 0,
        placement: "near solana".to_string(),
    }
}

// ------------------------------------------------------------------- legs

fn hyperliquid_perps(share: f64) -> Leg {
    Leg {
        venue: "Hyperliquid".to_string(),
        chain: Chain::HyperEvm,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "HyperEVM read precompiles: positions, balances, margin".to_string(),
        },
        account: "hl-main".to_string(),
        key: "an agent wallet the vault contract approved, trade-only".to_string(),
        can: vec!["Place orders".to_string(), "Cancel orders".to_string()],
        cannot: vec![
            "Withdraw".to_string(),
            "Send USDC or spot".to_string(),
            "Transfer between spot and perp".to_string(),
            "Approve another agent".to_string(),
        ],
        status: VenueStatus::Ok,
        note: None,
    }
}

fn hyperliquid_spot(share: f64) -> Leg {
    Leg {
        venue: "Hyperliquid spot".to_string(),
        chain: Chain::HyperEvm,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "HyperEVM read precompiles: spot balances and marks".to_string(),
        },
        account: "hl-main".to_string(),
        key: "the same agent wallet, trade-only".to_string(),
        can: vec!["Place and cancel spot orders".to_string()],
        cannot: vec!["Withdraw".to_string(), "Send to any address".to_string()],
        status: VenueStatus::Ok,
        note: None,
    }
}

fn stock_tokens(share: f64) -> Leg {
    Leg {
        venue: "Stock tokens".to_string(),
        chain: Chain::RobinhoodChain,
        custody: Custody::ChainEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "Chainlink per-token feeds; the lower of feed and market when the feed is \
                     stale"
                .to_string(),
        },
        account: "a Safe owned by the vault contract".to_string(),
        key: "a role scoped to the allowlisted routers".to_string(),
        can: vec!["Swap stock tokens through allowlisted routers".to_string()],
        cannot: vec![
            "Send to any receiver but the Safe".to_string(),
            "Call any other contract".to_string(),
        ],
        status: VenueStatus::Ok,
        note: Some(
            "The cash market behind these tokens closes. While it is shut, withdrawals are \
             valued at the lower of the feed and the on-chain market, deposits at the higher."
                .to_string(),
        ),
    }
}

fn arcus_perps(share: f64) -> Leg {
    Leg {
        venue: "Arcus".to_string(),
        chain: Chain::RobinhoodChain,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Unwinder,
        last_resort: Some(
            "The exchange's own emergency settlement, whose behaviour in normal operation isn't \
             documented."
                .to_string(),
        ),
        share_of_value: share,
        valuation: Valuation::AttestedRead {
            image: "the unwinder image, signed with a key blessed for this vault".to_string(),
        },
        account: "an exchange account owned by the vault's address".to_string(),
        key: "a trade-only API key".to_string(),
        can: vec![
            "Place, cancel and modify orders".to_string(),
            "Close positions reduce-only".to_string(),
        ],
        cannot: vec![
            "Withdraw, which needs a separate key".to_string(),
            "Send anywhere but the owner's own address".to_string(),
        ],
        status: VenueStatus::Ok,
        note: Some(
            "Positions here close only through the exchange's API, so a claim on this leg is \
             served by an unwinder rather than by the contract."
                .to_string(),
        ),
    }
}

fn arena_game(share: f64) -> Leg {
    Leg {
        venue: "Arena".to_string(),
        chain: Chain::Solana,
        custody: Custody::ChainEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "stake escrowed in an open match, valued at cost; balances priced by Pyth"
                .to_string(),
        },
        account: "a smart account whose only delegate is the guard program".to_string(),
        key: "a session key that acts only through the guard".to_string(),
        can: vec![
            "Enter a match at an allowlisted stake".to_string(),
            "Play a move".to_string(),
            "Settle to the vault's own account".to_string(),
        ],
        cannot: vec![
            "Move funds anywhere else".to_string(),
            "Call any program but the game's".to_string(),
        ],
        status: VenueStatus::Ok,
        note: Some(
            "Stake in an open match is valued at what it cost, never at what the strategy \
             hopes to win."
                .to_string(),
        ),
    }
}

// ------------------------------------------------------------- the builder

/// What makes a strategy itself. Everything else is derived.
struct Spec {
    id: &'static str,
    version: u32,
    title: &'static str,
    description: &'static str,
    author: &'static str,
    standing: Standing,
    days_live: u32,
    shape: Shape,
    chain: Chain,
    legs: Vec<Leg>,
    exit_class: ExitClass,
    source: Source,
    allowances: Allowances,
    models: Vec<ModelFile>,
    secrets: Vec<&'static str>,
    config: Vec<(&'static str, &'static str)>,
    review: Option<Review>,
    limits: Vec<&'static str>,
    dependencies: Vec<&'static str>,
    supersedes: Option<&'static str>,
    superseded_by: Option<&'static str>,
    /// How the project reads in the source tab.
    project: Project,
    /// Share of the vault kept idle on its home chain.
    idle_share: f64,
    daily_charge: f64,
    health: HealthShape,
    claims: Vec<Claim>,
    in_transit: Option<InTransit>,
    kill: Option<Kill>,
    markets: Vec<&'static str>,
}

#[derive(Clone, Copy)]
enum Project {
    Basis,
    Quoter,
    Catalyst,
    Arena,
}

#[derive(Clone, Copy, PartialEq)]
enum HealthShape {
    Quiet,
    Busy,
    Warning,
    None,
}

impl Spec {
    fn new(id: &'static str, version: u32, author: &'static str, days_live: u32) -> Self {
        Self {
            id,
            version,
            title: "",
            description: "",
            author,
            standing: if days_live >= Strategy::DAYS_TO_RANK {
                Standing::Ranked
            } else {
                Standing::New
            },
            days_live,
            shape: Shape::new(days_live, 0.14, 0.09, 200_000.0, 800_000.0),
            chain: Chain::HyperEvm,
            legs: Vec::new(),
            exit_class: ExitClass::Direct,
            source: Source::Sealed,
            allowances: light(),
            models: Vec::new(),
            secrets: Vec::new(),
            config: Vec::new(),
            review: None,
            limits: Vec::new(),
            dependencies: Vec::new(),
            supersedes: None,
            superseded_by: None,
            project: Project::Basis,
            idle_share: 0.05,
            daily_charge: 1.10,
            health: HealthShape::Quiet,
            claims: Vec::new(),
            in_transit: None,
            kill: None,
            markets: vec!["BTC-PERP"],
        }
    }

    fn slug(&self) -> String {
        format!("{}-v{}", self.id, self.version)
    }
}

fn published(repo: &str, seed: &str) -> Source {
    Source::Published {
        repo: repo.to_string(),
        commit: hex(seed, 40),
    }
}

fn build(spec: Spec) -> Strategy {
    let slug = spec.slug();
    let registered = days_ago(spec.days_live);

    let record = (spec.standing != Standing::BuildFailed)
        .then(|| series::record(&slug, as_of(), spec.shape));

    let venues: Vec<(&str, &str)> = spec
        .legs
        .iter()
        .map(|leg| (venue_key(&leg.venue), account_key(&leg.account)))
        .collect();
    let models: Vec<&str> = spec
        .models
        .iter()
        .map(|model| model.name.as_str())
        .collect();
    let manifest = source::manifest(&source::Manifest {
        id: spec.id,
        version: spec.version,
        title: spec.title,
        venues: &venues,
        config: &spec.config,
        allowances: &spec.allowances,
        models: &models,
        secrets: &spec.secrets,
        supersedes: spec.supersedes,
    });

    let source_files: Vec<SourceFile> = match spec.source {
        // A sealed project's files aren't served, so the tab shows what is
        // known about it rather than pretending to a tree.
        Source::Sealed => Vec::new(),
        Source::Published { .. } => match spec.project {
            Project::Basis => source::basis_project(spec.id, &manifest),
            Project::Quoter => source::quoter_project(spec.id, &manifest),
            Project::Catalyst => source::catalyst_project(spec.id, &manifest),
            Project::Arena => source::arena_project(spec.id, &manifest),
        },
    };

    let build_log: Vec<LogLine> = if spec.standing == Standing::BuildFailed {
        source::failed_build_log(spec.id)
    } else {
        source::build_log(
            spec.id,
            142,
            &spec
                .models
                .iter()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
        )
    };

    let bundle = Bundle {
        bundle_hash: sha256(&format!("{slug}-bundle")),
        component_hash: sha256(&format!("{slug}-component")),
        source_hash: sha256(&format!("{slug}-source")),
        manifest_hash: sha256(&format!("{slug}-manifest")),
        world: "shonin:strategy@0.1.0".to_string(),
        toolchain: "rust 1.98.0 · wasm32-wasip3 · cargo-component 0.24.1".to_string(),
        builder_image: sha256("builder-image"),
        models: spec.models.clone(),
        allowances: spec.allowances.clone(),
        secrets: spec.secrets.iter().map(|s| (*s).to_string()).collect(),
        config: spec
            .config
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        source: spec.source.clone(),
        source_files,
        build_log,
        built_in_seconds: 142,
    };

    let value = record
        .as_ref()
        .and_then(|record| record.aum.last())
        .map_or(0.0, |point| point.value);
    let share_price = record
        .as_ref()
        .and_then(|record| record.value_per_share.last())
        .map_or(1.0, |point| point.value);
    let high_water_mark = record.as_ref().map_or(1.0, |record| {
        record
            .value_per_share
            .iter()
            .map(|point| point.value)
            .fold(1.0, f64::max)
    });

    let value = match spec.standing {
        // A killed vault has been unwound and paid out; a failed build never
        // had a vault at all.
        Standing::Killed => value * 0.04,
        Standing::BuildFailed => 0.0,
        _ => value,
    };

    let vault = Vault {
        address: address(&format!("{slug}-vault")),
        chain: spec.chain,
        asset: spec.chain.asset().to_string(),
        state: match spec.standing {
            Standing::BuildFailed => VaultState::Closed,
            Standing::Killed => VaultState::Killed,
            Standing::Retired => VaultState::Closed,
            _ => VaultState::Open,
        },
        created: registered,
        value,
        idle: value * spec.idle_share,
        shares: if share_price > 0.0 {
            value / share_price
        } else {
            0.0
        },
        share_price,
        high_water_mark,
        last_crystallised: as_of(),
        fees: Fees::standard(),
        tank: Tank {
            balance: spec.daily_charge * 23.0,
            daily_charge: spec.daily_charge,
            target: spec.daily_charge * 30.0,
            autofill_bps: 10_000,
            debt: 0.0,
        },
        exit_class: spec.exit_class,
        freeze_after_days: 7,
        claims: spec.claims.clone(),
        in_transit: spec.in_transit.clone(),
        audits: audits(spec.chain),
    };

    let attestation = (spec.standing != Standing::BuildFailed).then(|| attestation(&slug, &spec));
    let health = match spec.health {
        HealthShape::None => None,
        shape => Some(health(&slug, shape, &spec)),
    };

    Strategy {
        id: spec.id.to_string(),
        slug: slug.clone(),
        version: spec.version,
        title: spec.title.to_string(),
        description: spec.description.to_string(),
        author: spec.author.to_string(),
        standing: spec.standing,
        registered,
        days_live: spec.days_live,
        supersedes: spec.supersedes.map(str::to_string),
        superseded_by: spec.superseded_by.map(str::to_string),
        bundle,
        vault,
        legs: spec.legs.clone(),
        review: spec.review.clone(),
        past_reviews: Vec::new(),
        health,
        attestation,
        trades: trades(&slug, &spec),
        events: events(&slug, &spec, share_price),
        record,
        kill: spec.kill.clone(),
        limits: spec.limits.iter().map(|s| (*s).to_string()).collect(),
        dependencies: spec.dependencies.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn venue_key(venue: &str) -> &'static str {
    match venue {
        "Hyperliquid" | "Hyperliquid spot" => "hyperliquid",
        "Arcus" => "robinhood-arcus",
        "Stock tokens" => "robinhood-chain",
        _ => "arena",
    }
}

fn account_key(account: &str) -> &'static str {
    if account.starts_with("hl-") {
        "hl-main"
    } else if account.contains("Safe") {
        "rh-safe"
    } else if account.contains("smart account") {
        "sol-main"
    } else {
        "arcus-main"
    }
}

fn audits(chain: Chain) -> Vec<Audit> {
    vec![
        Audit {
            subject: format!("Vault contracts on {}", chain.label()),
            status: AuditStatus::Audited,
            note: "Two firms, both reports published, findings closed.".to_string(),
        },
        Audit {
            subject: "Executor image".to_string(),
            status: AuditStatus::InProgress,
            note: "Review underway; the image is reproducible from published source.".to_string(),
        },
        Audit {
            subject: "Attestation proof guest".to_string(),
            status: AuditStatus::Unaudited,
            note: "Not yet reviewed.".to_string(),
        },
        Audit {
            subject: "Unwinder image".to_string(),
            status: AuditStatus::InProgress,
            note: "Reviewed alongside the executor image.".to_string(),
        },
    ]
}

fn attestation(slug: &str, spec: &Spec) -> Attestation {
    let mut keys = vec![BlessedKey {
        address: address(&format!("{slug}-primary")),
        zone: match spec.chain {
            Chain::HyperEvm => "asia-northeast1-a".to_string(),
            Chain::RobinhoodChain => "us-east4-b".to_string(),
            Chain::Solana => "us-central1-c".to_string(),
            Chain::Local => "this machine".to_string(),
        },
        blessed_until: now() + Duration::hours(51),
        proof_tx: tx(&format!("{slug}-proof-primary")),
        role: KeyRole::Primary,
    }];

    if spec.standing.is_live() {
        keys.push(BlessedKey {
            address: address(&format!("{slug}-standby")),
            zone: match spec.chain {
                Chain::HyperEvm => "asia-northeast1-c".to_string(),
                Chain::RobinhoodChain => "us-east4-c".to_string(),
                Chain::Solana => "us-central1-a".to_string(),
                Chain::Local => "this machine".to_string(),
            },
            blessed_until: now() + Duration::hours(49),
            proof_tx: tx(&format!("{slug}-proof-standby")),
            role: KeyRole::Standby,
        });
    }

    if matches!(spec.exit_class, ExitClass::Served { .. }) {
        keys.push(BlessedKey {
            address: address(&format!("{slug}-unwinder")),
            zone: "us-east4-a".to_string(),
            blessed_until: now() + Duration::hours(62),
            proof_tx: tx(&format!("{slug}-proof-unwinder")),
            role: KeyRole::Unwinder,
        });
    }

    Attestation {
        image: Image {
            digest: sha256("executor-image-7"),
            allowlisted: days_ago(34),
            audit: AuditStatus::InProgress,
            kind: "executor".to_string(),
        },
        builder: Image {
            digest: sha256("builder-image"),
            allowlisted: days_ago(96),
            audit: AuditStatus::Audited,
            kind: "builder".to_string(),
        },
        keys,
        approvals: spec
            .legs
            .iter()
            .map(|leg| Approval {
                venue: leg.venue.clone(),
                detail: format!(
                    "{} · {}",
                    leg.key,
                    address(&format!("{slug}-{}", leg.venue))
                ),
                custody: leg.custody,
            })
            .collect(),
        trust_root: "gcp-cs-tdx".to_string(),
        blessing_ttl_hours: 72,
    }
}

fn health(slug: &str, shape: HealthShape, spec: &Spec) -> Health {
    let mut rng = Rng::seeded(&format!("{slug}-health"));
    let busy = matches!(shape, HealthShape::Busy);

    let restarts = match shape {
        HealthShape::Warning => 3.0,
        _ => f64::from(rng.int(0, 1) as i32),
    };
    let rejections = match shape {
        HealthShape::Warning => 7.2,
        HealthShape::Busy => rng.range(1.2, 3.4),
        _ => rng.range(0.1, 0.9),
    };
    let suspended_share: f64 = spec
        .legs
        .iter()
        .filter(|leg| leg.status == VenueStatus::Suspended)
        .map(|leg| leg.share_of_value * 100.0)
        .sum();
    let quota = if busy {
        rng.range(8.0, 14.0)
    } else {
        rng.range(0.0, 2.0)
    };
    let claim_age = spec
        .claims
        .iter()
        .filter(|claim| !matches!(claim.state, ClaimState::Paid))
        .map(|claim| f64::from((as_of() - claim.booked).whole_days() as i32) * 24.0)
        .fold(0.0, f64::max);
    let transit = spec
        .in_transit
        .as_ref()
        .map_or(0.0, |transit| f64::from(transit.elapsed_minutes));
    let tank_days = 23.0;

    let signals = vec![
        HealthSignal {
            name: "Restarts".to_string(),
            value: restarts,
            unit: "in the last hour".to_string(),
            warn_at: 3.0,
            error_at: 10.0,
            lower_is_worse: false,
            window: "1 hour".to_string(),
            note: (restarts > 0.0).then(|| {
                "A restart rebuilds the program and reads positions back from the venue."
                    .to_string()
            }),
        },
        HealthSignal {
            name: "Rejected submissions".to_string(),
            value: rejections,
            unit: "% of the hour's submissions".to_string(),
            warn_at: 5.0,
            error_at: 25.0,
            lower_is_worse: false,
            window: "1 hour".to_string(),
            note: None,
        },
        HealthSignal {
            name: "Venue errors".to_string(),
            value: suspended_share,
            unit: "% of the vault on a suspended venue".to_string(),
            warn_at: 0.01,
            error_at: 25.0,
            lower_is_worse: false,
            window: "now".to_string(),
            note: (suspended_share > 0.0).then(|| {
                "A suspended venue is told to the strategy, which decides what to do.".to_string()
            }),
        },
        HealthSignal {
            name: "Quota hits".to_string(),
            value: quota,
            unit: "% of calls in the hour".to_string(),
            warn_at: 10.0,
            error_at: 50.0,
            lower_is_worse: false,
            window: "1 hour".to_string(),
            note: busy.then(|| {
                "A call past its quota returns an error the strategy handles; it never stalls."
                    .to_string()
            }),
        },
        HealthSignal {
            name: "Oldest unsettled claim".to_string(),
            value: claim_age,
            unit: "hours".to_string(),
            warn_at: 1.0,
            error_at: 168.0,
            lower_is_worse: false,
            window: "now".to_string(),
            note: (claim_age > 0.0)
                .then(|| "An unserved claim freezes the vault after seven days.".to_string()),
        },
        HealthSignal {
            name: "Stale value".to_string(),
            value: if spec
                .legs
                .iter()
                .any(|leg| matches!(leg.valuation, Valuation::AttestedRead { .. }))
            {
                rng.range(1.0, 6.0)
            } else {
                0.0
            },
            unit: "minutes since the last posted or attested value".to_string(),
            warn_at: 7.5,
            error_at: 15.0,
            lower_is_worse: false,
            window: "now".to_string(),
            note: None,
        },
        HealthSignal {
            name: "Capital in transit".to_string(),
            value: transit,
            unit: "minutes".to_string(),
            warn_at: 30.0,
            error_at: 60.0,
            lower_is_worse: false,
            window: "now".to_string(),
            note: spec.in_transit.as_ref().map(|transit| {
                format!(
                    "{} is moving from {} to {} over {}.",
                    money(transit.amount),
                    transit.from,
                    transit.to,
                    transit.route
                )
            }),
        },
        HealthSignal {
            name: "Tank runway".to_string(),
            value: tank_days,
            unit: "days of charges".to_string(),
            warn_at: 7.0,
            error_at: 0.0,
            lower_is_worse: true,
            window: "now".to_string(),
            note: Some(
                "Running costs are the author's, paid from the tank. An empty tank becomes a \
                 debt against future author fees."
                    .to_string(),
            ),
        },
    ];

    let mut entries = vec![
        HealthEntry {
            at: hours_ago(1),
            level: HealthLevel::Ok,
            message: "Usage statement signed and charged to the tank.".to_string(),
        },
        HealthEntry {
            at: hours_ago(9),
            level: HealthLevel::Ok,
            message: "Blessing renewed; the standby re-attested at the same time.".to_string(),
        },
    ];
    match shape {
        HealthShape::Warning => entries.insert(
            0,
            HealthEntry {
                at: hours_ago(3),
                level: HealthLevel::Warning,
                message: "Three restarts in the hour: a feed dropped its connection and the \
                          program was rebuilt each time."
                    .to_string(),
            },
        ),
        HealthShape::Busy => entries.insert(
            0,
            HealthEntry {
                at: hours_ago(4),
                level: HealthLevel::Ok,
                message: "Quote rate held at the venue's limit for eleven minutes; the shared \
                          stream absorbed it."
                    .to_string(),
            },
        ),
        _ => {}
    }

    Health {
        signals,
        entries,
        updated: hours_ago(0),
        signed_by: address(&format!("{slug}-primary")),
    }
}

fn money(amount: f64) -> String {
    format!("{amount:.0}")
}

fn trades(slug: &str, spec: &Spec) -> Vec<Trade> {
    if spec.standing == Standing::BuildFailed || spec.markets.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng::seeded(&format!("{slug}-trades"));
    let venue = spec
        .legs
        .first()
        .map_or("Hyperliquid".to_string(), |leg| leg.venue.clone());

    (0..48)
        .map(|index| {
            let market = (*rng.pick(&spec.markets)).to_string();
            let price = base_price(&market) * rng.range(0.985, 1.015);
            let size = rng.range(0.4, 14.0);
            let side = if rng.chance(0.5) {
                Side::Buy
            } else {
                Side::Sell
            };
            let kind = *rng.pick(&["maker", "maker", "taker", "reduce-only"]);
            Trade {
                at: now() - Duration::minutes(i64::from(index) * rng.int(7, 90)),
                venue: venue.clone(),
                market,
                side,
                size,
                price,
                notional: size * price,
                kind: kind.to_string(),
                reference: tx(&format!("{slug}-fill-{index}")),
            }
        })
        .collect()
}

fn base_price(market: &str) -> f64 {
    match market {
        "BTC-PERP" | "BTC/USDC" => 96_400.0,
        "ETH-PERP" | "ETH/USDC" => 4_180.0,
        "SOL-PERP" => 214.0,
        "NVDA" | "NVDA-PERP" => 187.2,
        "SPY" | "SPY-PERP" => 641.0,
        "QQQ" | "QQQ-PERP" => 572.0,
        "AAPL" => 246.5,
        "TSLA" => 412.0,
        "match: tier 2" => 25.0,
        _ => 100.0,
    }
}

fn events(slug: &str, spec: &Spec, share_price: f64) -> Vec<VaultEvent> {
    if spec.standing == Standing::BuildFailed {
        return vec![VaultEvent {
            at: now() - Duration::days(i64::from(spec.days_live)),
            kind: EventKind::TankFunded,
            summary: "Vault created with its initial tank; the build was its first charge."
                .to_string(),
            amount: Some(46.20),
            reference: tx(&format!("{slug}-create")),
        }];
    }

    let mut rng = Rng::seeded(&format!("{slug}-events"));
    let mut events = Vec::new();

    if let Some(kill) = &spec.kill {
        events.push(VaultEvent {
            at: kill.at,
            kind: EventKind::Killed,
            summary: kill.reason.clone(),
            amount: None,
            reference: kill.reference.clone(),
        });
    }

    for claim in &spec.claims {
        events.push(VaultEvent {
            at: claim.booked.with_hms(9, 14, 0).expect("a real time"),
            kind: EventKind::ClaimBooked,
            summary: format!(
                "Withdrawal beyond the idle balance: {:.2} shares pending while {:.1}% of every \
                 position is closed.",
                claim.shares,
                claim.fraction_closed * 100.0
            ),
            amount: None,
            reference: tx(&format!("{slug}-claim-{}", claim.id)),
        });
    }

    for index in 0..18 {
        let hours = index * rng.int(3, 26);
        let at = now() - Duration::hours(hours);
        let (kind, summary, amount) = match index % 6 {
            0 => (
                EventKind::Crystallised,
                "Daily crystallisation: the streaming fee, and profit fees above the high-water \
                 mark, minted as new shares."
                    .to_string(),
                Some(rng.range(3.0, 140.0)),
            ),
            1 => {
                let assets = rng.range(500.0, 90_000.0);
                (
                    EventKind::Deposited,
                    format!(
                        "Deposit at {:.4} a share, after the 0.1% transaction fee kept in the \
                         pool.",
                        share_price * rng.range(0.98, 1.0)
                    ),
                    Some(assets),
                )
            }
            2 => (
                EventKind::Withdrawn,
                "Withdrawal paid from the idle balance in the same transaction, at exit-side \
                 marks."
                    .to_string(),
                Some(rng.range(300.0, 40_000.0)),
            ),
            3 => (
                EventKind::CapitalMoved,
                format!(
                    "Capital moved between the vault's own accounts: {} to {}.",
                    spec.legs
                        .first()
                        .map_or("the contract", |leg| leg.account.as_str()),
                    spec.legs
                        .last()
                        .map_or("the contract", |leg| leg.account.as_str())
                ),
                Some(rng.range(5_000.0, 120_000.0)),
            ),
            4 => (
                EventKind::TankCharged,
                "Daily usage statement, signed by the executor and capped at the declared \
                 allowances."
                    .to_string(),
                Some(spec.daily_charge),
            ),
            _ => (
                EventKind::Blessed,
                "Executor keys re-attested and blessed for another 72 hours.".to_string(),
                None,
            ),
        };
        events.push(VaultEvent {
            at,
            kind,
            summary,
            amount,
            reference: tx(&format!("{slug}-event-{index}")),
        });
    }

    events.push(VaultEvent {
        at: days_ago(spec.days_live)
            .with_hms(11, 2, 0)
            .expect("a real time"),
        kind: EventKind::TankFunded,
        summary: "Vault created with its initial tank: the metered build, the review, and a \
                  week at the declared allowances."
            .to_string(),
        amount: Some(spec.daily_charge * 7.0 + 1.0 + 38.0),
        reference: tx(&format!("{slug}-create")),
    });

    events.sort_by_key(|event| std::cmp::Reverse(event.at));
    events
}

// ------------------------------------------------------------- the reviews

fn review(
    score: u8,
    summary: &str,
    findings: Vec<Finding>,
    limits: Vec<&str>,
    reviewed_days_ago: u32,
    seed: &str,
) -> Review {
    Review {
        score,
        findings,
        summary: summary.to_string(),
        reviewed: days_ago(reviewed_days_ago),
        reviewer_image: sha256("reviewer-image-4"),
        model_id_hash: sha256("reviewer-model"),
        prompt_hash: sha256("reviewer-prompt"),
        report_hash: sha256(&format!("{seed}-report")),
        limits: limits.into_iter().map(str::to_string).collect(),
        bought_by: "the vault's tank, when the strategy was created".to_string(),
    }
}

fn finding(
    category: FindingCategory,
    severity: Severity,
    summary: &str,
    detail: &str,
    file: &str,
    line: u32,
) -> Finding {
    Finding {
        category,
        severity,
        summary: summary.to_string(),
        detail: detail.to_string(),
        file: file.to_string(),
        line,
    }
}

const REVIEW_LIMITS: [&str; 3] = [
    "Reading code is not predicting behaviour. A reviewer that has read every line still cannot \
     say what a strategy will do in a market nobody has seen.",
    "A score is not a proof. A model can score a dangerous strategy well, or miss a risk.",
    "The review informs; it never protects. What a depositor can rely on is that only the \
     committed bundle can use the vault's keys, that funds cannot leave the vault's own \
     accounts, that they can always exit, and that the record is the chain.",
];

const MODEL_LIMIT: &str = "Weights are data, not source. The review judges this model by its inputs, outputs and \
     operators, and by what the code does with its output. Its behaviour is unverified.";

// -------------------------------------------------------------- the roster

/// Every strategy, in the order the registry saw them.
pub fn all() -> Vec<Strategy> {
    vec![
        funding_basis_v3(),
        mm_quoter_v7(),
        weekend_carry_v2(),
        catalyst_v1(),
        arena_v1(),
        funding_basis_v2(),
        mm_quoter_v6(),
        arena_v2(),
    ]
}

fn funding_basis_v3() -> Strategy {
    let mut spec = Spec::new("funding-basis", 3, "shonin", 214);
    spec.title = "Hyperliquid funding basis";
    spec.description = "Holds spot on HyperCore and shorts the perpetual against it to collect funding, and \
         unwinds when the carry stops paying. Enters when annualised funding is above 9% and \
         exits below 3%. Both legs sit in one venue-enforced account, so there is no bridge in \
         the loop. Never holds unhedged delta above 2% of the vault's value.";
    spec.shape = Shape::new(214, 0.176, 0.082, 420_000.0, 3_240_000.0)
        .with_shock(0.62, -0.021)
        .with_trades(18.0);
    spec.legs = vec![hyperliquid_perps(0.58), hyperliquid_spot(0.37)];
    spec.source = published("github.com/shonin/funding-basis", "funding-basis-v3");
    spec.project = Project::Basis;
    spec.config = vec![
        ("entry_apr", "0.09"),
        ("exit_apr", "0.03"),
        ("target_notional_pct", "0.80"),
        ("max_gross_leverage", "2.0"),
        ("max_unhedged_delta_pct", "0.02"),
    ];
    spec.markets = vec!["BTC-PERP", "ETH-PERP", "BTC/USDC", "ETH/USDC"];
    spec.daily_charge = 1.15;
    spec.supersedes = Some("funding-basis@2");
    spec.limits = vec![
        "Gross leverage never exceeds 2×.",
        "Unhedged delta never exceeds 2% of the vault's value.",
        "No single order above 5% of the vault's value.",
        "Both legs are cut together if the vault falls 10% from its peak.",
        "Only the venue's own perpetuals, no builder-deployed markets.",
    ];
    spec.dependencies = vec![
        "The venue's own funding feed, through the executor's shared data layer.",
        "Nothing else. It reads no host the author controls.",
    ];
    spec.review = Some(review(
        82,
        "Two legs on one venue, with the size of both derived from the vault's value and capped \
         in code. Entry and exit are a single funding threshold with hysteresis, and the \
         drawdown rule closes both legs together rather than leaving one naked. Rejections back \
         off. The margin rebalance moves capital only between the vault's own accounts.",
        vec![finding(
            FindingCategory::ExternalDependence,
            Severity::Note,
            "The hedge leg depends on one venue's funding feed",
            "The entry and exit decision reads a single funding stream. If that feed stalls, the \
             strategy holds its current position rather than unwinding, until the stream \
             recovers. That is a deliberate choice, stated in the description, but a depositor \
             should know the position is held, not flattened, when the feed goes quiet.",
            "src/lib.rs",
            41,
        )],
        REVIEW_LIMITS.to_vec(),
        212,
        "funding-basis-v3",
    ));
    build(spec)
}

fn funding_basis_v2() -> Strategy {
    let mut spec = Spec::new("funding-basis", 2, "shonin", 301);
    spec.standing = Standing::Superseded;
    spec.title = "Hyperliquid funding basis";
    spec.description = "The earlier version of this strategy. It runs exactly as it always has; the successor \
         is a separate vault that depositors choose to move to. Enters above 12% annualised \
         funding and exits below 4%.";
    spec.shape = Shape::new(301, 0.121, 0.094, 260_000.0, 318_000.0).with_trades(14.0);
    spec.legs = vec![hyperliquid_perps(0.55), hyperliquid_spot(0.40)];
    spec.source = published("github.com/shonin/funding-basis", "funding-basis-v2");
    spec.config = vec![
        ("entry_apr", "0.12"),
        ("exit_apr", "0.04"),
        ("target_notional_pct", "0.75"),
        ("max_gross_leverage", "2.0"),
    ];
    spec.markets = vec!["BTC-PERP", "BTC/USDC"];
    spec.superseded_by = Some("funding-basis-v3");
    spec.daily_charge = 1.05;
    spec.limits = vec![
        "Gross leverage never exceeds 2×.",
        "No single order above 5% of the vault's value.",
        "Both legs are cut together if the vault falls 10% from its peak.",
    ];
    spec.dependencies = vec!["The venue's own funding feed, through the shared data layer."];
    spec.review = Some(review(
        79,
        "The same shape as its successor, with wider entry and exit thresholds and no margin \
         rebalancing. Bounded, and honest about what it depends on.",
        vec![finding(
            FindingCategory::MissingFailureHandling,
            Severity::Note,
            "A rejected hedge order is retried on the next tick without widening",
            "When the spot leg is rejected the strategy re-submits the same order on its next \
             pass. There is a back-off on the perp leg but not on the hedge, so a persistently \
             rejected hedge can sit unfilled while the perp leg is already on. The successor \
             changes this.",
            "src/lib.rs",
            68,
        )],
        REVIEW_LIMITS.to_vec(),
        299,
        "funding-basis-v2",
    ));
    build(spec)
}

fn mm_quoter_v7() -> Strategy {
    let mut spec = Spec::new("mm-quoter", 7, "shonin", 161);
    spec.title = "Perpetual market making";
    spec.description = "Quotes two-sided on a small set of perpetuals, leans against inventory, and widens or \
         pulls entirely when the book goes stale. The source is sealed; the attested builder \
         compiled it and the reviewer read it.";
    spec.shape = Shape::new(161, 0.229, 0.148, 300_000.0, 1_150_000.0)
        .with_shock(0.41, -0.038)
        .with_trades(340.0);
    spec.legs = vec![hyperliquid_perps(0.94)];
    spec.source = Source::Sealed;
    spec.project = Project::Quoter;
    spec.allowances = quoting();
    spec.config = vec![
        ("markets", "ETH-PERP, BTC-PERP, SOL-PERP"),
        ("spread_bps", "6"),
        ("depth", "4"),
        ("max_book_age_ms", "400"),
        ("max_position_pct", "0.25"),
    ];
    spec.markets = vec!["ETH-PERP", "BTC-PERP", "SOL-PERP"];
    spec.daily_charge = 34.60;
    spec.idle_share = 0.06;
    spec.health = HealthShape::Busy;
    spec.limits = vec![
        "Inventory never exceeds 25% of the vault's value in any one market.",
        "Quotes are pulled entirely when the book is older than 400ms.",
        "Maker-only except when flattening: no market order is ever sent.",
        "Quoting stops for the day after a 4% loss.",
    ];
    spec.dependencies = vec![
        "The venue's public book feed, through the executor's shared stream.",
        "No host the author controls.",
    ];
    spec.review = Some(review(
        74,
        "A quoting loop with inventory skew and a stale-book cut-out. Position limits are a \
         share of the vault's value and are applied before quotes are built, so a large deposit \
         widens the ladder rather than the risk. The daily loss rule stops quoting but does not \
         flatten, which is stated in the description.",
        vec![
            finding(
                FindingCategory::UnboundedRisk,
                Severity::Concern,
                "The inventory limit is per market, not across markets",
                "Each market's position is capped at 25% of the vault's value, and three markets \
                 are quoted. In a correlated move the strategy can end up long all three at once, \
                 which is 75% of the vault's value in the same direction. Nothing in the code \
                 caps the total.",
                "src/inventory.rs",
                52,
            ),
            finding(
                FindingCategory::MissingFailureHandling,
                Severity::Note,
                "A restart re-subscribes but does not re-check its own open orders first",
                "After a restart the program subscribes to the book and begins quoting from the \
                 ladder it computes, cancelling what it finds afterwards. For a few hundred \
                 milliseconds, orders from before and after the restart can both rest on the \
                 book.",
                "src/lib.rs",
                27,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        159,
        "mm-quoter-v7",
    ));
    build(spec)
}

fn mm_quoter_v6() -> Strategy {
    let mut spec = Spec::new("mm-quoter", 6, "shonin", 190);
    spec.standing = Standing::Killed;
    spec.title = "Perpetual market making";
    spec.description = "The earlier version of the quoter, with a single market and a simpler inventory rule. \
         Stopped by the admin key after it kept submitting into a venue that was rejecting more \
         than a quarter of its orders, for six hours. A kill unwinds every position and brings \
         the cash home, so every withdrawal since is paid from the idle balance in one \
         transaction.";
    spec.shape = Shape::new(190, 0.041, 0.161, 400_000.0, 90_000.0)
        .with_shock(0.79, -0.061)
        .with_trades(280.0);
    spec.legs = vec![hyperliquid_perps(0.95)];
    spec.source = Source::Sealed;
    spec.project = Project::Quoter;
    spec.allowances = quoting();
    spec.config = vec![("markets", "ETH-PERP"), ("spread_bps", "8")];
    spec.markets = vec!["ETH-PERP"];
    spec.daily_charge = 28.90;
    spec.health = HealthShape::None;
    spec.kill = Some(Kill {
        at: hours_ago(62),
        reason: "Killed: submissions continued for six hours while the venue rejected more \
                 than a quarter of them."
            .to_string(),
        outcome: "Every position was closed reduce-only inside the exit band and the cash \
                  brought home. The vault holds only its idle balance, so each withdrawal is \
                  paid in one transaction."
            .to_string(),
        reference: tx("mm-quoter-v6-kill"),
    });
    spec.limits = vec![
        "Inventory never exceeded 30% of the vault's value.",
        "Quoting stopped for the day after a 5% loss.",
    ];
    spec.dependencies = vec!["The venue's public book feed."];
    spec.review = Some(review(
        61,
        "A simpler ancestor of the current quoter, whose failure handling was the weak part: \
         rejected orders were retried on a fixed interval with no ceiling and no venue-health \
         check, which is exactly the behaviour that ended it.",
        vec![
            finding(
                FindingCategory::MissingFailureHandling,
                Severity::Serious,
                "Rejected orders are retried on a fixed interval, indefinitely",
                "There is no back-off and no check of the venue's health before re-submitting. A \
                 venue that rejects everything will be asked again every few seconds for as long \
                 as the strategy runs.",
                "src/lib.rs",
                77,
            ),
            finding(
                FindingCategory::UnboundedRisk,
                Severity::Concern,
                "Inventory could reach 30% of the vault in one market",
                "One market, one position, and a limit set at nearly a third of the vault. The \
                 later version lowers it.",
                "src/inventory.rs",
                44,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        188,
        "mm-quoter-v6",
    ));
    build(spec)
}

fn weekend_carry_v2() -> Strategy {
    let mut spec = Spec::new("weekend-carry", 2, "shonin", 133);
    spec.chain = Chain::RobinhoodChain;
    spec.title = "Weekend carry on tokenised equities";
    spec.description = "Stock tokens trade around the clock; the cash market behind them does not. From Friday \
         evening to Monday morning the token's price is set purely by on-chain flow. This \
         strategy holds the token and shorts the perpetual against it, sized to the funding on \
         offer, and carries it into Monday's open.";
    spec.shape = Shape::new(133, 0.143, 0.088, 180_000.0, 842_000.0)
        .with_shock(0.55, -0.026)
        .with_trades(9.0);
    spec.legs = vec![stock_tokens(0.46), arcus_perps(0.49)];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = published("github.com/shonin/weekend-carry", "weekend-carry-v2");
    spec.config = vec![
        ("underlyings", "SPY, QQQ, NVDA"),
        ("max_gross_leverage", "2.0"),
        ("max_order_pct", "0.05"),
        ("cut_drawdown_pct", "0.10"),
    ];
    spec.markets = vec!["SPY", "QQQ", "NVDA", "SPY-PERP", "NVDA-PERP"];
    spec.daily_charge = 4.80;
    spec.in_transit = Some(InTransit {
        amount: 84_000.0,
        from: "the vault contract".to_string(),
        to: "the vault's Safe".to_string(),
        route: "the one allowlisted route".to_string(),
        elapsed_minutes: 4,
        timeout_minutes: 60,
    });
    spec.claims = vec![Claim {
        id: 41,
        owner: address("claimant-41"),
        shares: 18_402.61,
        fraction_closed: 0.071,
        booked: days_ago(5),
        state: ClaimState::Waiting,
        settled_for: None,
    }];
    spec.limits = vec![
        "Gross leverage never exceeds 2×.",
        "Three named underlyings, and nothing else.",
        "No single order above 5% of the vault's value.",
        "Both legs are cut together if the vault falls 10% from its peak.",
        "The perp leg is unwound before the spot leg, so a Monday open never meets a naked short.",
    ];
    spec.dependencies = vec![
        "The exchange's API for the perpetual leg, read inside the measured image.",
        "The per-token price feeds the vault contract values the spot leg with.",
    ];
    spec.review = Some(review(
        79,
        "A two-leg carry with a fixed universe of three names and both legs cut together. The \
         unwind order is explicit and correct: the hedge comes off before the thing it hedges. \
         Sizing is a share of the vault throughout, and the exposure is kept small because \
         weekend liquidity is thin.",
        vec![
            finding(
                FindingCategory::ExternalDependence,
                Severity::Concern,
                "One leg's value is read from the exchange rather than from a chain",
                "The perpetual leg lives in an exchange's own books, so the vault learns what it \
                 is worth from an attested read rather than from chain state. The read runs in a \
                 measured image and is signed for this vault, and a stale read closes the door on \
                 deposits and withdrawals, but a wrong answer from the exchange's API would \
                 misprice the vault until the next read.",
                "src/lib.rs",
                58,
            ),
            finding(
                FindingCategory::KnownFailurePattern,
                Severity::Note,
                "Carry sizing rises as funding rises, with no ceiling on the funding input",
                "The target is proportional to the funding on offer. Funding is clamped by the \
                 venue in practice, but the code does not clamp it, so an anomalous funding \
                 print would size the position at the leverage cap rather than at the cap the \
                 author probably intended.",
                "src/limits.rs",
                23,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        131,
        "weekend-carry-v2",
    ));
    build(spec)
}

fn catalyst_v1() -> Strategy {
    let mut spec = Spec::new("catalyst", 1, "shonin", 41);
    spec.chain = Chain::RobinhoodChain;
    spec.title = "Event-driven equities";
    spec.description = "Reads regulator filings and wire headlines as they arrive, scores them with a model in \
         the bundle, takes a bounded position in the name, and exits on a timer or a stop. The \
         code that turns news into trades was read by the reviewer, the keys to its feeds are \
         sealed to the bundle, and whatever it concludes it can only trade this vault's own \
         accounts.";
    spec.shape = Shape::new(41, 0.216, 0.194, 220_000.0, 412_000.0).with_trades(31.0);
    spec.legs = vec![stock_tokens(0.52), arcus_perps(0.43)];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = Source::Sealed;
    spec.project = Project::Catalyst;
    spec.allowances = thinking();
    spec.models = vec![ModelFile {
        name: "catalyst-scorer".to_string(),
        opset: 17,
        hash: sha256("catalyst-scorer"),
        size_mib: 184.0,
        purpose: "Scores a filing or headline for how far it moves the name, from −1 to 1."
            .to_string(),
    }];
    spec.secrets = vec!["wire_api_key"];
    spec.config = vec![
        ("universe", "94 names"),
        ("max_name_pct", "0.04"),
        ("hold_period_hours", "36"),
        ("stop_pct", "0.03"),
    ];
    spec.markets = vec!["NVDA", "AAPL", "TSLA", "SPY", "NVDA-PERP"];
    spec.daily_charge = 12.40;
    spec.health = HealthShape::Warning;
    spec.limits = vec![
        "No name above 4% of the vault's value.",
        "Positions expire after 36 hours whatever the model says.",
        "A 3% move against a position closes it.",
        "The model's output scales a cap the code already set; it can never set the size itself.",
        "If inference fails or runs over budget, hand-written rules take over at a smaller size.",
    ];
    spec.dependencies = vec![
        "Regulator filings, which are free and clearly licensed.",
        "One commercial wire feed, behind a key sealed to this bundle.",
        "A model in the bundle, whose weights nobody can read.",
    ];
    spec.review = Some(review(
        66,
        "The plumbing is careful: inference is budgeted, its failure path is a smaller \
         hand-written position rather than a stall, and the per-name cap is applied to the \
         vault's value before the model's score is used at all. What pulls the score down is how \
         much of the outcome rests on things the review cannot see: a model's weights, and feeds \
         that can be withdrawn or rate-limited.",
        vec![
            finding(
                FindingCategory::ModelDependence,
                Severity::Concern,
                "Most positions are sized by a model whose behaviour cannot be verified",
                "The scorer decides both the direction and the scale of nearly every position. \
                 The code bounds what that can mean, with a cap per name, a hold period and a \
                 stop, so a badly wrong score costs a bounded amount. But the reviewer cannot \
                 read the weights, and the live record is the only evidence that the model is \
                 any good.",
                "src/lib.rs",
                47,
            ),
            finding(
                FindingCategory::ExternalDependence,
                Severity::Concern,
                "One commercial feed is a single point of failure for the strategy's edge",
                "Filings are free; the wire is not, and it is the faster of the two. If the \
                 licence lapses or the provider rate-limits the key, the strategy keeps running \
                 on filings alone and its behaviour changes materially, with nothing in the code \
                 to signal that it has.",
                "src/lib.rs",
                29,
            ),
            finding(
                FindingCategory::MissingFailureHandling,
                Severity::Note,
                "A dropped websocket is reconnected without bounding the reconnect rate",
                "The host reconnects with back-off underneath, so this is bounded in practice \
                 rather than in the strategy. The restarts in this vault's health report are this \
                 path.",
                "src/lib.rs",
                24,
            ),
        ],
        {
            let mut limits = vec![MODEL_LIMIT];
            limits.extend(REVIEW_LIMITS);
            limits
        },
        39,
        "catalyst-v1",
    ));
    build(spec)
}

fn arena_v1() -> Strategy {
    let mut spec = Spec::new("arena", 1, "shonin", 27);
    spec.chain = Chain::Solana;
    spec.title = "On-chain match play";
    spec.description = "Plays one on-chain game for real stakes, continuously, inside limits written in its own \
         code. A match escrow is margin at a venue: the payout destination is fixed to this \
         vault's own account when a match is entered, and the most a match can cost is its \
         stake. The riskiest strategy on the leaderboard still cannot move a dollar to a \
         stranger.";
    spec.shape = Shape::new(27, -0.088, 0.238, 140_000.0, 178_000.0).with_trades(64.0);
    spec.legs = vec![arena_game(0.91)];
    spec.source = published("github.com/shonin/arena", "arena-v1");
    spec.project = Project::Arena;
    spec.allowances = playing();
    spec.config = vec![
        ("tier", "2"),
        ("max_stake_pct", "0.005"),
        ("max_open_matches", "10"),
        ("max_escrow_pct", "0.03"),
        ("daily_loss_cap_pct", "0.05"),
    ];
    spec.markets = vec!["match: tier 2"];
    spec.daily_charge = 7.20;
    spec.idle_share = 0.12;
    spec.limits = vec![
        "No borrowing, ever.",
        "A stake per match of at most 0.5% of the vault's value.",
        "At most ten matches at once, and at most 3% of the vault escrowed at any moment.",
        "A daily loss of 5% stops it entering matches until tomorrow.",
        "Open matches are always played out: forfeiting to exit faster would guarantee the loss \
         it was avoiding.",
    ];
    spec.dependencies = vec![
        "The game's own program and its matchmaking service.",
        "Real randomness, drawn from the host: a bot whose next move can be predicted can be beaten.",
    ];
    spec.review = Some(review(
        71,
        "Every limit this strategy has is a share of the vault's value, checked before it enters \
         a match, and the stopping rule is correct for the venue: it stops entering and lets open \
         matches settle rather than forfeiting them. An open match counts at what it cost, never \
         at what it might win.",
        vec![
            finding(
                FindingCategory::UnboundedRisk,
                Severity::Note,
                "The daily loss cap resets on a clock the strategy reads itself",
                "The cap uses the host's clock to decide when tomorrow starts, so it is per \
                 calendar day rather than rolling, and a bad run spanning midnight can spend two \
                 caps within a few hours.",
                "src/limits.rs",
                38,
            ),
            finding(
                FindingCategory::KnownFailurePattern,
                Severity::Concern,
                "The edge has to clear a rake on every match",
                "Every match pays the venue a known rake. The strategy needs a win rate above \
                 the rake to make money at all, and if its opponents are other bots that margin \
                 is thin or negative. The code is honest and bounded; the economics are the risk, \
                 and only the live record speaks to them.",
                "src/play.rs",
                74,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        25,
        "arena-v1",
    ));
    build(spec)
}

fn arena_v2() -> Strategy {
    let mut spec = Spec::new("arena", 2, "shonin", 3);
    spec.chain = Chain::Solana;
    spec.standing = Standing::BuildFailed;
    spec.title = "On-chain match play";
    spec.description = "A resubmission whose build did not produce the same bytes twice, so there is no bundle, \
         no vault and no record. The log is public, as every build log is. A fixed resubmission \
         is a new vault.";
    spec.shape = Shape::new(3, 0.0, 0.0, 0.0, 0.0);
    spec.legs = vec![];
    spec.source = published("github.com/shonin/arena", "arena-v2");
    spec.project = Project::Arena;
    spec.health = HealthShape::None;
    spec.markets = vec![];
    spec.daily_charge = 0.0;
    spec.limits = vec![];
    spec.dependencies = vec![];
    build(spec)
}
