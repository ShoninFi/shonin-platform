//! Every strategy on the platform.
//!
//! Each one is described by the things that make it itself — what it does,
//! where it trades, what its review found — and everything derivable is
//! derived: hashes, the record, the attestation, trades, events and the
//! vault's own numbers all follow from the same seed.

use shared::{
    Allowances, Approval, Attestation, Audit, AuditStatus, BlessedKey, Bundle, Chain, Claim,
    ClaimState, ComponentPin, Connected, Custody, EventKind, ExitClass, ExitPath, Fees, Finding,
    FindingCategory, Health, HealthEntry, HealthLevel, HealthSignal, Image, InTransit, KeyRole,
    Kill, Leg, LogLine, ModelFile, Product, Review, Severity, Side, Source, SourceFile, Standing,
    Strategy, Tank, Trade, Valuation, Vault, VaultEvent, VaultState, VenueStatus,
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

fn agentic() -> Allowances {
    Allowances {
        fuel_per_sec: 1_000_000,
        memory_mib: 256,
        requests_per_min: 120,
        bytes_per_day: 20_000_000,
        streams: 2,
        inference_ms_per_min: 400,
        placement: "any".to_string(),
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
            source: "Chainlink per-token feeds, with the corporate-action multiplier already \
                    applied; the lower of feed and market price when the feed is stale"
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
            "The cash market behind these tokens closes. While it is shut the feed holds its \
             last price, so withdrawals are valued at the lower of the feed and the on-chain \
             market and deposits at the higher."
                .to_string(),
        ),
    }
}

fn arcus_perps(share: f64, status: VenueStatus) -> Leg {
    Leg {
        venue: "Arcus".to_string(),
        chain: Chain::RobinhoodChain,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Unwinder,
        last_resort: Some(
            "The exchange's own emergency settlement. Its behaviour in normal operation isn't \
             documented, so this vault says so rather than assuming it."
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
        status,
        note: match status {
            VenueStatus::Suspended => Some(
                "Suspended: order rejections have been above a quarter of submissions for two \
                 hours, and the venue's own status page reports degraded matching."
                    .to_string(),
            ),
            VenueStatus::Ok => Some(
                "Positions here can only be closed through the exchange's API, so a claim on \
                 this leg is served by an unwinder rather than by the contract."
                    .to_string(),
            ),
        },
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

fn broker_account(share: f64) -> Leg {
    Leg {
        venue: "Robinhood Agentic".to_string(),
        chain: Chain::Solana,
        custody: Custody::BrokerEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::AttestedRead {
            image: "an hourly read of the account inside the measured image, posted to the \
                    account's entry"
                .to_string(),
        },
        account: "the owner's own brokerage account".to_string(),
        key: "a token the broker only lets trade".to_string(),
        can: vec![
            "Place orders in the owner's agentic account".to_string(),
            "Read positions, balances and transfers".to_string(),
        ],
        cannot: vec![
            "Withdraw funds".to_string(),
            "Transfer, stake or lend".to_string(),
            "Touch any other account the owner holds".to_string(),
        ],
        status: VenueStatus::Ok,
        note: Some(
            "The owner can revoke the token at the broker at any time, without the strategy's \
             cooperation."
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
    components: Vec<(&'static str, &'static str, &'static str)>,
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
    connected: Option<Connected>,
    markets: Vec<&'static str>,
}

#[derive(Clone, Copy)]
enum Project {
    Basis,
    Quoter,
    Catalyst,
    Arena,
    Agent,
}

#[derive(Clone, Copy, PartialEq)]
enum HealthShape {
    Quiet,
    Busy,
    Warning,
    Failing,
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
            components: Vec::new(),
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
            connected: None,
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
            Project::Agent => source::agent_project(spec.id, &manifest),
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
        components: spec
            .components
            .iter()
            .map(|(slug, name, version)| ComponentPin {
                slug: (*slug).to_string(),
                name: (*name).to_string(),
                version: (*version).to_string(),
                hash: sha256(&format!("component-{slug}")),
            })
            .collect(),
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

    let product = if let Some(connected) = spec.connected.clone() {
        Product::Connected(connected)
    } else {
        Product::Vault(Box::new(Vault {
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
        }))
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
        product,
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
        "Arena" => "arena",
        _ => "robinhood-agentic",
    }
}

fn account_key(account: &str) -> &'static str {
    if account.starts_with("hl-") {
        "hl-main"
    } else if account.contains("Safe") {
        "rh-safe"
    } else if account.contains("smart account") {
        "sol-main"
    } else if account.contains("exchange") {
        "arcus-main"
    } else {
        "owner"
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
            note: "Review underway; the image is reproducible from published source in the \
                   meantime."
                .to_string(),
        },
        Audit {
            subject: "Attestation proof guest".to_string(),
            status: AuditStatus::Unaudited,
            note: "Not yet reviewed. Vaults open before every audit is finished, and every page \
                   says where each one stands."
                .to_string(),
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
        HealthShape::Failing => 4.0,
        HealthShape::Warning => 3.0,
        _ => f64::from(rng.int(0, 1) as i32),
    };
    let rejections = match shape {
        HealthShape::Failing => 31.4,
        HealthShape::Warning => 7.2,
        HealthShape::Busy => rng.range(1.2, 3.4),
        _ => rng.range(0.1, 0.9),
    };
    let suspended_share = if shape == HealthShape::Failing {
        spec.legs
            .iter()
            .filter(|leg| leg.status == VenueStatus::Suspended)
            .map(|leg| leg.share_of_value * 100.0)
            .sum()
    } else {
        0.0
    };
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
            note: (claim_age > 0.0).then(|| {
                "An unserved claim freezes the vault after seven days: no new deposits, and the \
                 home chain's trading approvals revoked."
                    .to_string()
            }),
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
                 debt against future author fees; it never touches depositors' capital."
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
        HealthShape::Failing => entries.insert(
            0,
            HealthEntry {
                at: hours_ago(2),
                level: HealthLevel::Error,
                message: "Arcus rejected 31% of submissions this hour and is marked suspended. \
                          The strategy has been told."
                    .to_string(),
            },
        ),
        HealthShape::Warning => entries.insert(
            0,
            HealthEntry {
                at: hours_ago(3),
                level: HealthLevel::Warning,
                message: "Three restarts in the hour: a wire feed dropped its connection and \
                          the program was rebuilt each time."
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
        "HYPE-PERP" => 39.4,
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
        funding_rotator_v2(),
        weekend_carry_v2(),
        follow_northstar_v1(),
        catalyst_v1(),
        basis_arcus_v1(),
        arena_v1(),
        funding_basis_v2(),
        mm_eth_perp_v6(),
        basis_rh_v1(),
        vol_harvest_v1(),
        steady_dividends_v2(),
        index_rebalance_v1(),
        earnings_drift_v1(),
    ]
}

fn funding_basis_v3() -> Strategy {
    let mut spec = Spec::new("funding-basis", 3, "shonin", 214);
    spec.title = "Hyperliquid funding basis";
    spec.description = "Holds spot on HyperCore and shorts the perpetual against it to collect funding, and \
         unwinds when the carry stops paying. Enters when annualised funding is above 9% and \
         exits below 3%. Both legs sit inside one venue-enforced account, so there is no bridge \
         in the loop and no second custody level to explain. Never holds unhedged delta above \
         2% of the vault's value.";
    spec.shape = Shape::new(214, 0.176, 0.082, 420_000.0, 3_240_000.0)
        .with_shock(0.62, -0.021)
        .with_trades(18.0);
    spec.legs = vec![hyperliquid_perps(0.58), hyperliquid_spot(0.37)];
    spec.source = published("github.com/shonin/funding-basis", "funding-basis-v3");
    spec.project = Project::Basis;
    spec.components = vec![
        ("hedge", "hedge", "1.2"),
        ("twap-exec", "twap-exec", "0.9"),
        ("rebalancer", "rebalancer", "1.0"),
    ];
    spec.config = vec![
        ("entry_apr", "0.09"),
        ("exit_apr", "0.03"),
        ("target_notional_pct", "0.80"),
        ("max_gross_leverage", "2.0"),
        ("max_unhedged_delta_pct", "0.02"),
    ];
    spec.markets = vec!["BTC-PERP", "ETH-PERP", "BTC/USDC", "ETH/USDC"];
    spec.daily_charge = 1.15;
    spec.superseded_by = None;
    spec.supersedes = Some("funding-basis@2");
    spec.limits = vec![
        "Gross leverage never exceeds 2×.",
        "Unhedged delta never exceeds 2% of the vault's value.",
        "No single order above 5% of the vault's value.",
        "Both legs are cut together if the vault falls 10% from its peak.",
        "No builder-deployed markets: only the venue's own perpetuals.",
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
         off. The margin rebalance moves capital only between the vault's own accounts, which \
         the contract enforces anyway.",
        vec![finding(
            FindingCategory::ExternalDependence,
            Severity::Note,
            "The hedge leg depends on one venue's funding feed",
            "The entry and exit decision reads a single funding stream. If that feed stalls, the \
             strategy holds its current position rather than unwinding, until the stream \
             recovers. That is a deliberate choice in the code and it is stated in the \
             description, but a depositor should know the position is held, not flattened, when \
             the feed goes quiet.",
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
    spec.description = "The earlier version of this strategy. It runs exactly as it always has: nothing about a \
         published strategy can be changed, so the successor is a separate vault that depositors \
         choose to move to. Enters above 12% annualised funding and exits below 4%.";
    spec.shape = Shape::new(301, 0.121, 0.094, 260_000.0, 318_000.0).with_trades(14.0);
    spec.legs = vec![hyperliquid_perps(0.55), hyperliquid_spot(0.40)];
    spec.source = published("github.com/shonin/funding-basis", "funding-basis-v2");
    spec.components = vec![("hedge", "hedge", "1.1"), ("twap-exec", "twap-exec", "0.9")];
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
             version changes this.",
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
    let mut spec = Spec::new("mm-quoter", 7, "tidewater", 161);
    spec.title = "Perpetual market making";
    spec.description = "Quotes two-sided on a small set of perpetuals, leans against inventory, and widens or \
         pulls entirely when the book goes stale. Inventory is the product: more of it means \
         more size on more markets. The source is sealed; the attested builder compiled it and \
         the reviewer read it.";
    spec.shape = Shape::new(161, 0.229, 0.148, 300_000.0, 1_150_000.0)
        .with_shock(0.41, -0.038)
        .with_trades(340.0);
    spec.legs = vec![hyperliquid_perps(0.94)];
    spec.source = Source::Sealed;
    spec.project = Project::Quoter;
    spec.allowances = quoting();
    spec.components = vec![
        ("mm-quoter", "mm-quoter", "2.1"),
        ("twap-exec", "twap-exec", "0.9"),
    ];
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
        "No market order is ever sent: the strategy is maker-only except when flattening.",
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
                 milliseconds, orders from before the restart and orders after it can both rest \
                 on the book.",
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

fn funding_rotator_v2() -> Strategy {
    let mut spec = Spec::new("funding-rotator", 2, "kestrel.lab", 133);
    spec.title = "Funding rotation across perpetuals";
    spec.description = "Watches funding across every perpetual on one venue and holds the carry on whichever \
         two pay best, rotating as the ranking changes. One venue, one account, both legs \
         hedged; the rotation is the only moving part.";
    spec.shape = Shape::new(133, 0.124, 0.071, 210_000.0, 764_000.0).with_trades(26.0);
    spec.legs = vec![hyperliquid_perps(0.61), hyperliquid_spot(0.33)];
    spec.source = published(
        "github.com/kestrel-lab/funding-rotator",
        "funding-rotator-v2",
    );
    spec.components = vec![
        ("hedge", "hedge", "1.2"),
        ("twap-exec", "twap-exec", "0.9"),
        ("funding-rank", "funding-rank", "0.4"),
    ];
    spec.config = vec![
        ("hold_count", "2"),
        ("rotate_threshold_apr", "0.035"),
        ("max_gross_leverage", "1.8"),
    ];
    spec.markets = vec!["BTC-PERP", "ETH-PERP", "SOL-PERP", "HYPE-PERP"];
    spec.daily_charge = 2.40;
    spec.limits = vec![
        "At most two markets held at once.",
        "Gross leverage never exceeds 1.8×.",
        "A rotation only happens when the ranking gap is above 3.5% annualised, so the strategy \
         cannot churn.",
    ];
    spec.dependencies = vec!["The venue's funding feeds, through the shared data layer."];
    spec.review = Some(review(
        85,
        "The most bounded strategy of its kind on the platform: two positions at a time, a \
         rotation threshold that prevents churn, leverage capped below 2×, and every size \
         derived from the vault's value. Failure handling is thorough — rejections, feed gaps \
         and restarts all have explicit paths.",
        vec![finding(
            FindingCategory::UnauditedDependencies,
            Severity::Note,
            "Composes a component with little use elsewhere",
            "`funding-rank` is used by this strategy and one other. It is public and pinned by \
             hash, and it only ranks numbers the strategy already has, so the exposure is \
             small — but it has had far less use under real capital than the first-party \
             components beside it.",
            "Cargo.toml",
            11,
        )],
        REVIEW_LIMITS.to_vec(),
        131,
        "funding-rotator-v2",
    ));
    build(spec)
}

fn weekend_carry_v2() -> Strategy {
    let mut spec = Spec::new("weekend-carry", 2, "kestrel.lab", 133);
    spec.chain = Chain::RobinhoodChain;
    spec.title = "Weekend carry on tokenised equities";
    spec.description = "Stock tokens trade around the clock; the cash market behind them does not, and neither \
         does the channel that keeps token and underlying together. From Friday evening to \
         Monday morning the token's price is set purely by on-chain flow. This strategy holds \
         the token and shorts the perpetual against it, sized to the funding on offer, and \
         carries it into Monday's open.";
    spec.shape = Shape::new(133, 0.143, 0.088, 180_000.0, 842_000.0)
        .with_shock(0.55, -0.026)
        .with_trades(9.0);
    spec.legs = vec![stock_tokens(0.46), arcus_perps(0.49, VenueStatus::Ok)];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = published("github.com/kestrel-lab/weekend-carry", "weekend-carry-v2");
    spec.components = vec![("hedge", "hedge", "1.2"), ("twap-exec", "twap-exec", "0.9")];
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
         Sizing is a share of the vault throughout. The strategy is honest that weekend \
         liquidity is thin, and keeps its exposure small because of it.",
        vec![
            finding(
                FindingCategory::ExternalDependence,
                Severity::Concern,
                "One leg's value is read from the exchange rather than from a chain",
                "The perpetual leg lives in an exchange's own books, so the vault learns what it \
                 is worth from an attested read rather than from chain state. The read runs in a \
                 measured image and is signed for this vault, and a stale read closes the door on \
                 deposits and withdrawals — but a wrong answer from the exchange's API would \
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

fn follow_northstar_v1() -> Strategy {
    let mut spec = Spec::new("follow-northstar", 1, "northwind", 96);
    spec.title = "Mirror one public trader";
    spec.description = "Tracks one public account's perpetual positions and holds the same shape, scaled to \
         the vault's value. Following a wallet is one strategy among many here: what makes this \
         one checkable is that you can read exactly how it scales, what it refuses to copy, and \
         what it does when the account it follows goes quiet.";
    spec.shape = Shape::new(96, 0.157, 0.132, 120_000.0, 418_000.0)
        .with_shock(0.73, -0.052)
        .with_trades(21.0);
    spec.legs = vec![hyperliquid_perps(0.92)];
    spec.source = published(
        "github.com/northwind/follow-northstar",
        "follow-northstar-v1",
    );
    spec.components = vec![
        ("follow-wallet", "follow-wallet", "1.4"),
        ("twap-exec", "twap-exec", "0.9"),
    ];
    spec.config = vec![
        ("target", "a public account on the venue"),
        ("scale_pct", "0.60"),
        ("max_leverage", "2.5"),
        ("ignore_markets", "builder-deployed"),
    ];
    spec.markets = vec!["BTC-PERP", "ETH-PERP", "SOL-PERP"];
    spec.daily_charge = 1.90;
    spec.limits = vec![
        "Never more than 2.5× gross leverage, whatever the followed account does.",
        "Builder-deployed markets are ignored entirely.",
        "Positions are scaled to 60% of what proportional copying would give.",
        "If the followed account is quiet for 48 hours, positions are closed.",
    ];
    spec.dependencies = vec![
        "The venue's public data for one account's positions, read through the shared data layer.",
    ];
    spec.review = Some(review(
        63,
        "A mirror with a leverage cap and a market exclusion list, both applied before anything \
         is submitted. The strategy's risk is not really in its code: it is in the account it \
         follows, which can change what it does at any time and owes this vault nothing.",
        vec![
            finding(
                FindingCategory::ExternalDependence,
                Severity::Serious,
                "The strategy does whatever one outside account does",
                "Every position this vault takes is a copy of a third party's. That party is not \
                 the author, is not accountable to depositors, and can change strategy, take on \
                 leverage or stop trading without notice. The leverage cap bounds the size of the \
                 mistake, not whether it happens.",
                "src/lib.rs",
                34,
            ),
            finding(
                FindingCategory::KnownFailurePattern,
                Severity::Concern,
                "Copying arrives after the move it is copying",
                "Positions are mirrored once they are visible on-chain, so this vault buys after \
                 the account it follows has bought. In fast markets the fill it gets is \
                 systematically worse than the one it is copying, and the code does not measure \
                 or bound that gap.",
                "src/lib.rs",
                61,
            ),
            finding(
                FindingCategory::MissingFailureHandling,
                Severity::Note,
                "A restart re-derives the target from positions, not from the followed account",
                "After a restart the program treats its own current positions as the target \
                 until the next update from the account it follows. If that account moved during \
                 the outage, the vault stays on the old shape until the next change.",
                "src/lib.rs",
                22,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        94,
        "follow-northstar-v1",
    ));
    build(spec)
}

fn catalyst_v1() -> Strategy {
    let mut spec = Spec::new("catalyst", 1, "halcyon.works", 41);
    spec.chain = Chain::RobinhoodChain;
    spec.title = "Event-driven equities";
    spec.description = "Reads regulator filings and wire headlines as they arrive, scores them with a model in \
         the bundle, takes a bounded position in the name, and exits on a timer or a stop. Every \
         news bot asks you to trust what it reads and who runs it. This one asks you to trust \
         neither: the code that turns news into trades was read by the reviewer, the keys to its \
         feeds are sealed to the bundle, and whatever it concludes it can only trade this \
         vault's own accounts.";
    spec.shape = Shape::new(41, 0.216, 0.194, 220_000.0, 412_000.0).with_trades(31.0);
    spec.legs = vec![stock_tokens(0.52), arcus_perps(0.43, VenueStatus::Ok)];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = Source::Sealed;
    spec.project = Project::Catalyst;
    spec.allowances = thinking();
    spec.components = vec![("twap-exec", "twap-exec", "0.9"), ("hedge", "hedge", "1.2")];
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
         much of the outcome rests on things the review cannot see — a model's weights, and \
         feeds that can be withdrawn or rate-limited.",
        vec![
            finding(
                FindingCategory::ModelDependence,
                Severity::Concern,
                "Most positions are sized by a model whose behaviour cannot be verified",
                "The scorer decides both the direction and the scale of nearly every position. \
                 The code bounds what that can mean — a cap per name, a hold period, a stop — so \
                 a badly wrong score costs a bounded amount. But the reviewer cannot read the \
                 weights, and the live record is the only evidence that the model is any good.",
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

fn basis_arcus_v1() -> Strategy {
    let mut spec = Spec::new("basis-arcus", 1, "kestrel.lab", 63);
    spec.chain = Chain::RobinhoodChain;
    spec.title = "Equity perpetual basis";
    spec.description = "Holds the spot token and shorts the equity perpetual against it whenever the perpetual \
         trades rich. A simple carry, on a venue whose positions can only be closed through its \
         own API — which is why this vault's exit class is served rather than direct, and why \
         the page says how many operators can serve a claim.";
    spec.shape = Shape::new(63, 0.061, 0.104, 140_000.0, 268_000.0)
        .with_shock(0.88, -0.044)
        .with_trades(11.0);
    spec.legs = vec![
        stock_tokens(0.44),
        arcus_perps(0.51, VenueStatus::Suspended),
    ];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = published("github.com/kestrel-lab/basis-arcus", "basis-arcus-v1");
    spec.components = vec![("hedge", "hedge", "1.2")];
    spec.config = vec![
        ("underlyings", "NVDA, TSLA"),
        ("entry_bps", "35"),
        ("exit_bps", "8"),
        ("max_gross_leverage", "1.6"),
    ];
    spec.markets = vec!["NVDA", "TSLA", "NVDA-PERP"];
    spec.daily_charge = 3.60;
    spec.health = HealthShape::Failing;
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
        "Two names, and nothing else.",
        "Gross leverage never exceeds 1.6×.",
        "Both legs are cut together if the vault falls 8% from its peak.",
    ];
    spec.dependencies = vec![
        "The exchange's API for the perpetual leg, read inside the measured image.",
        "The per-token price feeds the contract values the spot leg with.",
    ];
    spec.review = Some(review(
        71,
        "A conventional two-name carry, bounded and readable. The live problem is not in the \
         code: the venue holding one leg is rejecting submissions, and a claim against this \
         vault has been waiting five days for that leg to close inside the exit band.",
        vec![finding(
            FindingCategory::ExternalDependence,
            Severity::Concern,
            "Half the vault sits where only an unwinder can close it",
            "The perpetual leg closes through the exchange's own API, so serving a withdrawal \
             claim needs an operator running the unwinder image, not just anyone sending a \
             transaction. The vault page says so, says how many operators can do it, and says \
             what the venue's own last resort is.",
            "shonin.toml",
            18,
        )],
        REVIEW_LIMITS.to_vec(),
        61,
        "basis-arcus-v1",
    ));
    build(spec)
}

fn arena_v1() -> Strategy {
    let mut spec = Spec::new("arena", 1, "moraine", 27);
    spec.chain = Chain::Solana;
    spec.title = "On-chain match play";
    spec.description = "Plays one on-chain game for real stakes, continuously, inside limits written in its own \
         code. It is here to prove something about the platform rather than about games: a match \
         escrow is margin at a venue, the payout destination is fixed to this vault's own account \
         when a match is entered, and the most a match can cost is its stake. The riskiest \
         strategy on the leaderboard still cannot move a dollar to a stranger.";
    spec.shape = Shape::new(27, -0.088, 0.238, 140_000.0, 178_000.0).with_trades(64.0);
    spec.legs = vec![arena_game(0.91)];
    spec.source = published("github.com/moraine/arena", "arena-v1");
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
         matches settle rather than forfeiting them. The valuation rule is the conservative one — \
         an open match counts at what it cost, never at what it might win.",
        vec![
            finding(
                FindingCategory::UnboundedRisk,
                Severity::Note,
                "The daily loss cap resets on a clock the strategy reads itself",
                "The cap uses the host's clock to decide when tomorrow starts. That is fine and \
                 nothing depends on it being precise, but it means the cap is per calendar day \
                 rather than rolling, and a bad run spanning midnight can spend two caps within a \
                 few hours.",
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

fn mm_eth_perp_v6() -> Strategy {
    let mut spec = Spec::new("mm-eth-perp", 6, "tidewater", 247);
    spec.standing = Standing::Retired;
    spec.title = "Single-market quoting";
    spec.description = "Retired by its author after the successor started. It quoted one perpetual with a \
         simpler inventory rule. Its record stays here, as every record does: what an author \
         has published does not disappear when it stops working.";
    spec.shape = Shape::new(247, 0.041, 0.161, 400_000.0, 90_000.0)
        .with_shock(0.79, -0.061)
        .with_trades(280.0);
    spec.legs = vec![hyperliquid_perps(0.95)];
    spec.source = Source::Sealed;
    spec.project = Project::Quoter;
    spec.allowances = quoting();
    spec.components = vec![("mm-quoter", "mm-quoter", "1.8")];
    spec.config = vec![("markets", "ETH-PERP"), ("spread_bps", "8")];
    spec.markets = vec!["ETH-PERP"];
    spec.daily_charge = 28.90;
    spec.health = HealthShape::None;
    spec.limits = vec![
        "Inventory never exceeded 30% of the vault's value.",
        "Quoting stopped for the day after a 5% loss.",
    ];
    spec.dependencies = vec!["The venue's public book feed."];
    spec.review = Some(review(
        68,
        "A simpler ancestor of the author's current quoter, with a single market and a looser \
         inventory bound.",
        vec![finding(
            FindingCategory::UnboundedRisk,
            Severity::Concern,
            "Inventory could reach 30% of the vault in one market",
            "One market, one position, and a limit set at nearly a third of the vault. The \
             author's later version lowers it.",
            "src/inventory.rs",
            44,
        )],
        REVIEW_LIMITS.to_vec(),
        245,
        "mm-eth-perp-v6",
    ));
    build(spec)
}

fn basis_rh_v1() -> Strategy {
    let mut spec = Spec::new("basis-rh", 1, "northwind", 88);
    spec.chain = Chain::RobinhoodChain;
    spec.standing = Standing::Killed;
    spec.title = "Index token carry";
    spec.description = "Held index tokens against their perpetuals. Stopped by the admin key after the \
         strategy kept submitting into a venue that had been suspended for six hours, and its \
         health report showed rejections above a quarter of submissions for most of a day. A \
         kill unwinds every position and brings the cash home, so every withdrawal after it is \
         paid from the idle balance in one transaction.";
    spec.shape = Shape::new(88, -0.121, 0.142, 300_000.0, 196_000.0)
        .with_shock(0.91, -0.068)
        .with_trades(12.0);
    spec.legs = vec![stock_tokens(0.51), arcus_perps(0.44, VenueStatus::Ok)];
    spec.exit_class = ExitClass::Served { operators: 2 };
    spec.source = published("github.com/northwind/basis-rh", "basis-rh-v1");
    spec.components = vec![("hedge", "hedge", "1.1")];
    spec.config = vec![("underlyings", "SPY, QQQ"), ("max_gross_leverage", "2.0")];
    spec.markets = vec!["SPY", "QQQ", "SPY-PERP"];
    spec.daily_charge = 3.10;
    spec.health = HealthShape::None;
    spec.kill = Some(Kill {
        at: hours_ago(62),
        reason: "Killed: submissions to a suspended venue continued for six hours, with \
                 rejections above a quarter of the hour's submissions."
            .to_string(),
        outcome: "Every position was closed reduce-only inside the exit band and the cash \
                  brought home. The vault holds only its idle balance, so each withdrawal is \
                  paid in one transaction."
            .to_string(),
        reference: tx("basis-rh-kill"),
    });
    spec.limits = vec![
        "Two names, and nothing else.",
        "Gross leverage never exceeded 2×.",
    ];
    spec.dependencies = vec!["The exchange's API for the perpetual leg."];
    spec.review = Some(review(
        58,
        "A plain carry whose failure handling was the weak part: rejected submissions were \
         retried on a fixed interval with no ceiling and no venue-health check, which is exactly \
         the behaviour that ended it.",
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
                FindingCategory::MissingFailureHandling,
                Severity::Concern,
                "A suspended venue does not change what the strategy does",
                "The host tells the strategy when a venue is suspended. This code ignores the \
                 signal, so it keeps trying to trade a venue that has told it not to.",
                "src/lib.rs",
                52,
            ),
        ],
        REVIEW_LIMITS.to_vec(),
        86,
        "basis-rh-v1",
    ));
    build(spec)
}

fn vol_harvest_v1() -> Strategy {
    let mut spec = Spec::new("vol-harvest", 1, "northwind", 3);
    spec.standing = Standing::BuildFailed;
    spec.title = "Short volatility carry";
    spec.description = "The build did not produce the same bytes twice, so there is no bundle, no vault and no \
         record. The log below is public, as every build log is: an author cannot quietly try \
         twenty times and show you the twentieth. A fixed resubmission is a new vault.";
    spec.shape = Shape::new(3, 0.0, 0.0, 0.0, 0.0);
    spec.legs = vec![];
    spec.source = published("github.com/northwind/vol-harvest", "vol-harvest-v1");
    spec.health = HealthShape::None;
    spec.markets = vec![];
    spec.daily_charge = 0.0;
    spec.limits = vec![];
    spec.dependencies = vec![];
    build(spec)
}

// ------------------------------------------------------ connected accounts

fn steady_dividends_v2() -> Strategy {
    let mut spec = Spec::new("steady-dividends", 2, "pallas.qt", 168);
    spec.chain = Chain::Solana;
    spec.title = "Dividend capture";
    spec.description = "Buys into names ahead of their ex-dividend dates and sells once the drop has been \
         recovered, in the owner's own brokerage account. It never holds more than a fifth of \
         the account in one name, and it never sells anything the owner bought themselves.";
    spec.shape = Shape::new(168, 0.094, 0.061, 4_000_000.0, 18_400_000.0).with_trades(4.0);
    spec.legs = vec![broker_account(1.0)];
    spec.source = published(
        "github.com/pallas-qt/steady-dividends",
        "steady-dividends-v2",
    );
    spec.project = Project::Agent;
    spec.allowances = agentic();
    spec.components = vec![("mcp", "mcp", "1.0"), ("oauth", "oauth", "1.1")];
    spec.secrets = vec!["broker_refresh_token"];
    spec.config = vec![
        ("max_name_pct", "0.20"),
        ("hold_after_ex_days", "9"),
        ("min_yield_pct", "0.015"),
    ];
    spec.markets = vec![];
    spec.daily_charge = 0.32;
    spec.connected = Some(Connected {
        broker: "Robinhood Agentic".to_string(),
        broker_limits: vec![
            "It can place orders in the owner's agentic account.".to_string(),
            "It cannot withdraw funds.".to_string(),
            "It cannot transfer, stake or lend.".to_string(),
            "It cannot touch any other account the owner holds.".to_string(),
        ],
        accounts: 412,
        total_value: 18_400_000.0,
        read_cadence: "hourly".to_string(),
        registry: "the connected-account registry, with one entry and one tank per account"
            .to_string(),
        requirements: vec![
            "An agentic account at the broker.".to_string(),
            "A one-time authorisation, completed on the owner's own machine.".to_string(),
            "A funded tank: running costs are the owner's, and a broker account needs no gas."
                .to_string(),
            "No hand-trading in the same account: the fee is on the account's whole gain."
                .to_string(),
        ],
        monthly_cost: 9.60,
    });
    spec.limits = vec![
        "No more than 20% of the account in any one name.",
        "It only sells what it bought.",
        "It stops for the day if the account falls 2%.",
    ];
    spec.dependencies = vec![
        "The broker's own interface, over a token the owner can revoke at any time.",
        "A dividend calendar, read from a public source.",
    ];
    spec.review = Some(review(
        81,
        "Bounded and narrow: a fifth of the account per name, a fixed holding period, and an \
         explicit refusal to sell anything the strategy did not buy — which matters more here \
         than in a vault, because the account is the owner's and may hold things they care \
         about.",
        vec![finding(
            FindingCategory::ExternalDependence,
            Severity::Note,
            "The dividend calendar comes from one public source",
            "A wrong or late calendar entry means a position taken at the wrong time. The \
             consequence is bounded by the per-name cap, and the source is named in the \
             description.",
            "src/plan.rs",
            31,
        )],
        REVIEW_LIMITS.to_vec(),
        166,
        "steady-dividends-v2",
    ));
    build(spec)
}

fn index_rebalance_v1() -> Strategy {
    let mut spec = Spec::new("index-rebalance", 1, "pallas.qt", 121);
    spec.chain = Chain::Solana;
    spec.title = "Index rebalancing";
    spec.description = "Keeps the owner's account at a target weighting they choose, rebalancing when a \
         holding drifts more than three points from its target. The least exciting agent in the \
         catalogue, and the one whose code is easiest to read end to end.";
    spec.shape = Shape::new(121, 0.071, 0.048, 3_000_000.0, 9_700_000.0).with_trades(1.0);
    spec.legs = vec![broker_account(1.0)];
    spec.source = published("github.com/pallas-qt/index-rebalance", "index-rebalance-v1");
    spec.project = Project::Agent;
    spec.allowances = agentic();
    spec.components = vec![("mcp", "mcp", "1.0"), ("oauth", "oauth", "1.1")];
    spec.secrets = vec!["broker_refresh_token"];
    spec.config = vec![("drift_pct", "0.03"), ("min_trade_usd", "50")];
    spec.markets = vec![];
    spec.daily_charge = 0.21;
    spec.connected = Some(Connected {
        broker: "Robinhood Agentic".to_string(),
        broker_limits: vec![
            "It can place orders in the owner's agentic account.".to_string(),
            "It cannot withdraw funds.".to_string(),
            "It cannot transfer, stake or lend.".to_string(),
            "It cannot touch any other account the owner holds.".to_string(),
        ],
        accounts: 233,
        total_value: 9_700_000.0,
        read_cadence: "hourly".to_string(),
        registry: "the connected-account registry, with one entry and one tank per account"
            .to_string(),
        requirements: vec![
            "An agentic account at the broker.".to_string(),
            "A target weighting, set when the account is connected.".to_string(),
            "A funded tank for running costs.".to_string(),
        ],
        monthly_cost: 6.30,
    });
    spec.limits = vec![
        "It only rebalances towards the weights the owner set.",
        "No trade below $50, so drift never turns into churn.",
        "It never takes the account to cash.",
    ];
    spec.dependencies = vec!["The broker's own interface, and nothing else."];
    spec.review = Some(review(
        88,
        "The whole strategy is a difference between two weight vectors and a minimum trade size. \
         There is nothing to steer it, no model, and no outside feed. The highest score in the \
         catalogue, and the review is careful to say that means it is well-bounded, not that it \
         will make money.",
        vec![],
        REVIEW_LIMITS.to_vec(),
        119,
        "index-rebalance-v1",
    ));
    build(spec)
}

fn earnings_drift_v1() -> Strategy {
    let mut spec = Spec::new("earnings-drift", 1, "halcyon.works", 44);
    spec.chain = Chain::Solana;
    spec.title = "Post-earnings drift";
    spec.description = "Holds names that surprised at earnings for a fixed window afterwards, in the owner's \
         own account. Labelled new: it has run for a few weeks, and a few weeks of a record is \
         not something to lean on.";
    spec.shape = Shape::new(44, 0.132, 0.121, 900_000.0, 2_140_000.0).with_trades(3.0);
    spec.legs = vec![broker_account(1.0)];
    spec.source = Source::Sealed;
    spec.project = Project::Agent;
    spec.allowances = agentic();
    spec.components = vec![("mcp", "mcp", "1.0"), ("oauth", "oauth", "1.1")];
    spec.secrets = vec!["broker_refresh_token", "estimates_api_key"];
    spec.config = vec![
        ("max_name_pct", "0.15"),
        ("hold_days", "21"),
        ("min_surprise_pct", "0.05"),
    ];
    spec.markets = vec![];
    spec.daily_charge = 0.44;
    spec.connected = Some(Connected {
        broker: "Robinhood Agentic".to_string(),
        broker_limits: vec![
            "It can place orders in the owner's agentic account.".to_string(),
            "It cannot withdraw funds.".to_string(),
            "It cannot transfer, stake or lend.".to_string(),
            "It cannot touch any other account the owner holds.".to_string(),
        ],
        accounts: 96,
        total_value: 2_140_000.0,
        read_cadence: "hourly".to_string(),
        registry: "the connected-account registry, with one entry and one tank per account"
            .to_string(),
        requirements: vec![
            "An agentic account at the broker.".to_string(),
            "A one-time authorisation, completed on the owner's own machine.".to_string(),
            "A funded tank for running costs.".to_string(),
        ],
        monthly_cost: 13.20,
    });
    spec.limits = vec![
        "No more than 15% of the account in any one name.",
        "Positions are closed after 21 days whatever happens.",
        "It only acts on surprises above 5%.",
    ];
    spec.dependencies = vec![
        "An estimates feed, behind a key sealed to this bundle.",
        "The broker's own interface.",
    ];
    spec.review = Some(review(
        69,
        "A fixed window and a fixed cap, with the surprise threshold as the only judgement in \
         the code. The estimates feed is the dependency worth knowing about: the strategy's \
         whole signal comes from one provider.",
        vec![finding(
            FindingCategory::ExternalDependence,
            Severity::Concern,
            "The signal comes entirely from one commercial estimates feed",
            "Every position this strategy takes rests on one provider's numbers for what was \
             expected. If those numbers are wrong, late or withheld, the strategy either acts on \
             a bad surprise or stops acting, and the code does not distinguish the two.",
            "src/plan.rs",
            44,
        )],
        REVIEW_LIMITS.to_vec(),
        42,
        "earnings-drift-v1",
    ));
    build(spec)
}
