//! The source, manifests and build logs the code tab shows.
//!
//! Every strategy on the platform is compiled by the build service from a
//! submitted Rust project, so a strategy page can show the project itself.

use shared::{LogLevel, LogLine, SourceFile};

fn file(path: &str, body: &str) -> SourceFile {
    SourceFile {
        path: path.to_string(),
        bytes: body.len() as u32,
        body: body.to_string(),
    }
}

fn line(text: &str) -> LogLine {
    LogLine {
        text: text.to_string(),
        level: LogLevel::Plain,
    }
}

fn warn(text: &str) -> LogLine {
    LogLine {
        text: text.to_string(),
        level: LogLevel::Warn,
    }
}

fn error(text: &str) -> LogLine {
    LogLine {
        text: text.to_string(),
        level: LogLevel::Error,
    }
}

/// The log of a build that worked: pinned recipe, offline, built twice and
/// compared, every model loaded, the component instantiated, then signed.
pub fn build_log(name: &str, seconds: u32, models: &[&str]) -> Vec<LogLine> {
    let mut log = vec![
        line(&format!("queued  {name}")),
        line("builder sha256:ae77b1… (allowlisted, timelock cleared)"),
        line("network disabled for the duration of the build"),
        line("fetching the pinned registry snapshot"),
        line("cargo build --locked --release --target wasm32-wasip3"),
        line("   Compiling shonin-strategy-sdk v0.1.0"),
        line(&format!("   Compiling {name} v0.1.0")),
        line("    Finished `release` profile in 71.4s"),
        line("pass 1 complete"),
        line("cargo build --locked --release --target wasm32-wasip3   (second pass)"),
        line("    Finished `release` profile in 68.9s"),
        line("pass 2 complete"),
        line("comparing outputs: byte-identical ✓"),
        line("instantiating the component in the executor's runtime ✓"),
    ];

    for model in models {
        log.push(line(&format!(
            "loading model {model}: ok, 41 ms cold, inside the declared inference allowance"
        )));
    }

    log.extend([
        line("world shonin:strategy@0.1.0 satisfied ✓"),
        line(&format!("build finished in {seconds}s")),
        line("signing (sourceHash, manifestHash) → bundleHash with the blessed builder key"),
        line("registry accepted the signature ✓"),
    ]);
    log
}

/// The log of a build that didn't work. It's as public as one that did: an
/// author can't quietly try twenty times and show you the twentieth.
pub fn failed_build_log(name: &str) -> Vec<LogLine> {
    vec![
        line(&format!("queued  {name}")),
        line("builder sha256:ae77b1… (allowlisted, timelock cleared)"),
        line("network disabled for the duration of the build"),
        line("cargo build --locked --release --target wasm32-wasip3"),
        line("    Finished `release` profile in 64.2s"),
        line("pass 1 complete"),
        line("cargo build --locked --release --target wasm32-wasip3   (second pass)"),
        line("    Finished `release` profile in 63.8s"),
        line("pass 2 complete"),
        line("comparing outputs: 2 sections differ"),
        warn("  .rodata differs at offset 0x0004a118 (48 bytes)"),
        warn("  .rodata differs at offset 0x0004c0a0 (16 bytes)"),
        line("locating the difference"),
        error("  build script build.rs:41 writes SystemTime::now() into the binary"),
        error("  a build whose output isn't stable can't be published: nobody could reproduce it"),
        line("no bundle produced; the build is charged because it ran"),
        line("the vault is closed and the rest of its tank reclaimable by the author"),
    ]
}

// ------------------------------------------------------------ the projects

/// A two-leg carry strategy: the shape most vaults on the platform have.
pub fn basis_project(id: &str, manifest: &str) -> Vec<SourceFile> {
    vec![
        file(
            "Cargo.toml",
            &cargo_toml(
                id,
                &["shonin-hedge", "shonin-twap-exec", "shonin-rebalancer"],
            ),
        ),
        file("shonin.toml", manifest),
        file(
            "src/lib.rs",
            r#"//! Collects funding on a perpetual while holding the other leg, and
//! unwinds when the carry no longer pays for the risk.
//!
//! The strategy is one long-lived program. Nothing schedules it: it waits on
//! the streams it chose to open and submits when it decides to.

use shonin::{data, state, vault, venues, Verdict};
use shonin_hedge::Hedge;
use shonin_rebalancer::Rebalancer;
use shonin_twap_exec::Twap;
use shonin_hyperliquid as hl;

mod book;
mod limits;

use book::Book;
use limits::Limits;

#[shonin::main]
async fn run(config: Config) -> Result<(), String> {
    let limits = Limits::from(&config);
    let mut book = Book::restore(state::get("book")).await?;

    // Positions are read back from the venue, never trusted to the cache:
    // this program may have been restarted on another machine a second ago.
    book.reconcile(venues::account_state("hyperliquid", hl::query::positions()).await?)?;

    let mut funding = data::subscribe("hyperliquid", hl::feed::funding(&config.market))?;
    let mut fills = venues::account_events("hyperliquid");
    let mut events = vault::events();

    let mut twap = Twap::new(limits.slice_notional, limits.slice_interval);
    let mut hedge = Hedge::new(limits.hedge_tolerance);
    let mut rebalancer = Rebalancer::new(limits.margin_ceiling);

    loop {
        shonin::select! {
            message = funding.next() => {
                let apr = hl::Funding::decode(&message?)?.annualised();
                let target = match book.phase() {
                    Phase::Flat if apr >= config.entry_apr => limits.target_notional(book.value()),
                    Phase::Open if apr <= config.exit_apr => Notional::ZERO,
                    _ => book.target(),
                };
                book.set_target(limits.clamp(target));
            }
            message = fills.next() => {
                let fill = hl::Fill::decode(&message?)?;
                book.on_fill(&fill);
                twap.on_fill(&fill);
            }
            event = events.next() => match event? {
                // The contract is closing this fraction of every position to
                // pay a claim. Shrink the target so the hedge stays matched.
                vault::Event::ClaimPending(fraction) => book.scale(1.0 - fraction),
                vault::Event::Halting(_) => break,
            },
        }

        if book.drawdown_from_peak() > limits.max_drawdown {
            book.set_target(Notional::ZERO);
        }

        let orders = twap.step(&config.market, book.target_delta());
        let hedges = hedge.step(&book, &config.hedge_market);
        for (venue, actions) in [
            ("hyperliquid", hl::encode(&orders)),
            ("hyperliquid", hl::encode(&hedges)),
        ] {
            for verdict in venues::submit(venue, actions).await {
                if let Verdict::Rejected(reason) = verdict {
                    // A rejection is ordinary. Back off, never spin.
                    twap.on_rejected(&reason);
                }
            }
        }

        if let Some(move_) = rebalancer.step(&book) {
            venues::submit("hyperliquid", hl::encode(&[move_])).await;
        }

        state::set("book", book.snapshot());
    }

    Ok(())
}
"#,
        ),
        file(
            "src/limits.rs",
            r#"//! Everything this strategy will not do. There is no platform risk
//! policy, so the limits live here, where the review reads them.

use shonin::Notional;

pub struct Limits {
    pub max_gross_leverage: f64,
    pub max_order_share: f64,
    pub max_drawdown: f64,
    pub margin_ceiling: f64,
    pub hedge_tolerance: f64,
    pub slice_notional: Notional,
    pub slice_interval: core::time::Duration,
}

impl Limits {
    /// Never size a target beyond what the vault can carry at this leverage.
    pub fn clamp(&self, target: Notional) -> Notional {
        target.min(self.ceiling())
    }

    /// No single order is a large share of the vault, whatever the signal
    /// says, so a deposit can never turn into one proportionally large
    /// market order.
    pub fn slice(&self, value: Notional) -> Notional {
        self.slice_notional.min(value * self.max_order_share)
    }
}
"#,
        ),
        file(
            "src/book.rs",
            r#"//! The strategy's view of its own positions, rebuilt from the venue
//! after every restart. The state cache is a convenience, never a source
//! of truth: it may be empty after a failover.

use shonin::{Notional, Price};

pub struct Book {
    target: Notional,
    position: Notional,
    hedge: Notional,
    peak_value: Price,
}

impl Book {
    /// A restart reads positions, balances and open orders back from the
    /// venue, and only then trusts anything it cached.
    pub fn reconcile(&mut self, state: shonin_hyperliquid::AccountState) -> Result<(), String> {
        self.position = state.position(&self.market)?;
        self.hedge = state.spot_balance(&self.hedge_market)?;
        Ok(())
    }

    /// A claim is being settled: the contract closed this fraction of every
    /// position, so both legs shrink together and the hedge stays matched.
    pub fn scale(&mut self, factor: f64) {
        self.target = self.target * factor;
        self.position = self.position * factor;
        self.hedge = self.hedge * factor;
    }
}
"#,
        ),
    ]
}

/// A quoting strategy: the shape that needs fuel, bandwidth and placement.
pub fn quoter_project(id: &str, manifest: &str) -> Vec<SourceFile> {
    vec![
        file(
            "Cargo.toml",
            &cargo_toml(id, &["shonin-mm-quoter", "shonin-twap-exec"]),
        ),
        file("shonin.toml", manifest),
        file(
            "src/lib.rs",
            r#"//! Two-sided quoting on a small set of perpetuals, with inventory skew
//! and a hard stop on stale books.

use shonin::{data, vault, venues, Verdict};
use shonin_hyperliquid as hl;
use shonin_mm_quoter::{Ladder, Quoter};

mod inventory;
mod limits;

#[shonin::main]
async fn run(config: Config) -> Result<(), String> {
    let limits = limits::Limits::from(&config);
    let mut quoter = Quoter::new(config.spread_bps, config.depth);
    let mut inventory = inventory::Inventory::restore(&config).await?;

    // One upstream connection per feed, shared with every other strategy on
    // this executor, so quoting doesn't burn the venue's per-IP limits.
    let mut books = data::subscribe("hyperliquid", hl::feed::book(&config.markets))?;
    let mut fills = venues::account_events("hyperliquid");
    let mut events = vault::events();

    loop {
        shonin::select! {
            message = books.next() => {
                let book = hl::Book::decode(&message?)?;

                // A book we can't trust is a book we don't quote into.
                if book.age() > config.max_book_age {
                    venues::submit("hyperliquid", hl::encode(&quoter.cancel_all())).await;
                    continue;
                }

                let fair = book.microprice();
                let skew = inventory.skew(&book.market);
                let ladder: Ladder = quoter.quotes(fair, skew, limits.max_position);
                let actions = hl::encode(&quoter.diff(&ladder));
                for verdict in venues::submit("hyperliquid", actions).await {
                    if let Verdict::Rejected(reason) = verdict {
                        quoter.on_rejected(&reason);
                    }
                }
            }
            message = fills.next() => inventory.on_fill(hl::Fill::decode(&message?)?),
            event = events.next() => match event? {
                vault::Event::ClaimPending(fraction) => inventory.scale(1.0 - fraction),
                vault::Event::Halting(_) => {
                    venues::submit("hyperliquid", hl::encode(&quoter.cancel_all())).await;
                    break;
                }
            },
        }
    }

    Ok(())
}
"#,
        ),
        file(
            "src/inventory.rs",
            r#"//! Inventory: how far the book has pushed us one way, and how hard to
//! lean against it. Rebuilt from the venue on every restart.

pub struct Inventory {
    position: Notional,
    limit: Notional,
}

impl Inventory {
    /// Quotes widen and then pull entirely as inventory approaches its limit,
    /// so the strategy can never accumulate without bound.
    pub fn skew(&self, market: &Market) -> Skew {
        let usage = (self.position / self.limit).clamp(-1.0, 1.0);
        Skew::from_usage(usage)
    }
}
"#,
        ),
    ]
}

/// An event-driven strategy that reads the open internet and calls a model.
pub fn catalyst_project(id: &str, manifest: &str) -> Vec<SourceFile> {
    vec![
        file(
            "Cargo.toml",
            &cargo_toml(id, &["shonin-twap-exec", "shonin-hedge"]),
        ),
        file("shonin.toml", manifest),
        file(
            "src/lib.rs",
            r#"//! Reads filings and wire headlines as they arrive, scores them with a
//! model in the bundle, and takes a bounded position in the name.
//!
//! Inputs aren't part of the record, and don't need to be: whatever this
//! program reads, it can only trade the vault's own accounts.

use shonin::{http, inference, secrets, vault, venues, websocket, Verdict};
use shonin_robinhood as rh;

mod conviction;
mod limits;

#[shonin::main]
async fn run(config: Config) -> Result<(), String> {
    let limits = limits::Limits::from(&config);
    let mut book = conviction::Book::new(config.universe.clone());

    // Regulator filings are free and clearly licensed, so they come first.
    let mut filings = websocket::connect(&config.filings_url, vec![]).await?;
    // The commercial wire needs a key, sealed to this bundle. Nobody outside
    // an enclave running exactly this code can read it.
    let wire_key = secrets::get("wire_api_key").ok_or("wire_api_key is not sealed")?;
    let mut wire = websocket::connect(&config.wire_url, auth(&wire_key)).await?;
    let mut events = vault::events();

    loop {
        let item = shonin::select! {
            message = filings.messages().next() => Item::filing(message?),
            message = wire.messages().next() => Item::headline(message?),
            event = events.next() => {
                match event? {
                    vault::Event::ClaimPending(fraction) => { book.scale(1.0 - fraction); continue }
                    vault::Event::Halting(_) => break,
                }
            }
        };

        let Some(name) = book.resolve(&item) else { continue };

        // The model scores the item. If it can't, hand-written rules do, and
        // the position is smaller for it: inference never stalls the program.
        let score = match inference::infer("catalyst-scorer", &item.features()).await {
            Ok(output) => output.scalar("score").unwrap_or(0.0).clamp(-1.0, 1.0),
            Err(inference::Error::QuotaExceeded(_)) => book.fallback_score(&item),
            Err(other) => { book.note_failure(other); continue }
        };

        // Whatever the model says, the code decides the size, and the size is
        // bounded by the vault's value and by the name's own cap.
        let target = limits.position_for(&name, score, book.value());
        let orders = book.orders_towards(&name, target);
        for verdict in venues::submit("robinhood-chain", rh::encode(&orders)).await {
            if let Verdict::Rejected(reason) = verdict {
                book.on_rejected(&name, &reason);
            }
        }

        book.expire_stale(limits.hold_period);
    }

    Ok(())
}
"#,
        ),
        file(
            "src/limits.rs",
            r#"//! What this strategy will not do, whatever it reads.

impl Limits {
    /// A model's output can never size a position on its own: it scales a
    /// cap the code already set, and the cap is a share of the vault.
    pub fn position_for(&self, name: &Name, score: f64, value: Notional) -> Notional {
        let cap = (value * self.max_name_share).min(self.max_name_notional);
        cap * score.clamp(-1.0, 1.0) * self.conviction_scale
    }
}
"#,
        ),
    ]
}

/// A strategy whose venue is a game: the adapter interface never mentions
/// prices, which is the point.
pub fn arena_project(id: &str, manifest: &str) -> Vec<SourceFile> {
    vec![
        file("Cargo.toml", &cargo_toml(id, &[])),
        file("shonin.toml", manifest),
        file(
            "src/lib.rs",
            r#"//! Plays one on-chain game for real stakes, inside limits written here.
//!
//! A match escrow is margin at a venue that happens to be a game: the payout
//! destination is fixed to the vault's own account when the match is entered,
//! so the most a match can cost is its stake.

use shonin::{vault, venues, Verdict};
use shonin_arena as game;

mod play;
mod limits;

#[shonin::main]
async fn run(config: Config) -> Result<(), String> {
    let limits = limits::Limits::from(&config);
    let mut table = play::Table::new(&config);
    let mut matches = venues::account_events("arena");
    let mut events = vault::events();

    loop {
        // Enter only while every limit allows it: stake per match, matches at
        // once, total escrow, and the daily loss cap.
        while limits.may_enter(&table) {
            let stake = limits.stake(table.value());
            match venues::submit("arena", game::encode(&[game::enter(config.tier, stake)])).await {
                verdicts if verdicts.iter().all(Verdict::is_accepted) => table.entered(stake),
                _ => break,
            }
        }

        shonin::select! {
            message = matches.next() => match game::Event::decode(&message?)? {
                // A move is due. Mixed play needs real randomness: a bot whose
                // next move can be predicted is a bot that can be beaten.
                game::Event::MoveDue(state) => {
                    let move_ = table.choose(&state, shonin::random::u64());
                    venues::submit("arena", game::encode(&[move_])).await;
                }
                game::Event::Settled(result) => table.settled(result),
            },
            event = events.next() => match event? {
                // Nothing can cancel a match in progress. Forfeiting to exit
                // faster would guarantee the loss it was avoiding, so open
                // matches are played out and the leaver's fraction paid when
                // they settle.
                vault::Event::ClaimPending(_) => table.stop_entering(),
                vault::Event::Halting(_) => { table.stop_entering(); break }
            },
        }

        if table.loss_today() > limits.daily_loss_cap {
            table.idle_until_tomorrow();
        }
    }

    // Stopping means settling: play out what's open, enter nothing new.
    table.drain().await
}
"#,
        ),
    ]
}

fn cargo_toml(id: &str, helpers: &[&str]) -> String {
    let mut out = format!(
        "[package]\n\
         name = \"{id}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2024\"\n\
         \n\
         [lib]\n\
         crate-type = [\"cdylib\"]\n\
         \n\
         [dependencies]\n\
         shonin = \"0.1\"\n"
    );
    for (index, helper) in helpers.iter().enumerate() {
        // Ordinary crates, pinned by Cargo.lock like any dependency.
        out += &format!("{helper} = \"1.{index}\"\n");
    }
    out
}

/// What a manifest is written from.
pub struct Manifest<'a> {
    pub id: &'a str,
    pub version: u32,
    pub title: &'a str,
    pub venues: &'a [(&'a str, &'a str)],
    pub config: &'a [(&'a str, &'a str)],
    pub allowances: &'a shared::Allowances,
    pub models: &'a [&'a str],
    pub secrets: &'a [&'a str],
    pub supersedes: Option<&'a str>,
}

/// The manifest, in the form an author writes it.
pub fn manifest(manifest: &Manifest<'_>) -> String {
    let Manifest {
        id,
        version,
        title,
        venues,
        config,
        allowances,
        models,
        secrets,
        supersedes,
    } = manifest;
    let mut out = format!(
        "# The strategy manifest. It is compiled with the code, so the\n\
         # description, the config and the bundle are hashed as one thing.\n\
         \n\
         [strategy]\n\
         id = \"{id}\"\n\
         version = {version}\n"
    );
    if let Some(earlier) = *supersedes {
        out += &format!("supersedes = \"{earlier}\"\n");
    }
    out += &format!(
        "\n\
         [description]\n\
         title = \"{title}\"\n\
         \n\
         [venues]\n"
    );
    for (venue, account) in venues.iter() {
        out += &format!("{venue} = {{ accounts = [\"{account}\"] }}\n");
    }

    out += "\n[config]\n";
    for (key, value) in config.iter() {
        out += &format!("{key} = \"{value}\"\n");
    }

    if !models.is_empty() {
        out += "\n[models]\n";
        for model in models.iter() {
            out += &format!(
                "{model} = {{ format = \"onnx\", opset = 17, path = \"models/{model}.onnx\" }}\n"
            );
        }
    }

    out += &format!(
        "\n\
         [runtime.allowances]\n\
         fuel_per_sec = {}\n\
         memory_mib = {}\n\
         inference_ms_per_min = {}\n\
         placement = \"{}\"\n\
         \n\
         [runtime.allowances.bandwidth]\n\
         requests_per_min = {}\n\
         bytes_per_day = {}\n\
         streams = {}\n",
        allowances.fuel_per_sec,
        allowances.memory_mib,
        allowances.inference_ms_per_min,
        allowances.placement,
        allowances.requests_per_min,
        allowances.bytes_per_day,
        allowances.streams,
    );

    if secrets.is_empty() {
        out += "\nsecrets = []\n";
    } else {
        out += &format!(
            "\nsecrets = [{}]\n",
            secrets
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    out += "\nfees = \"platform\"   # 0.5% a year of value; 10% of gains to the author, 5% to Shonin\n";
    out
}
