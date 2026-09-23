//! The words the app says about itself.
//!
//! Kept in one place so the pre-rendered page and the app say exactly the
//! same thing, and so the product's copy can be read and edited on its own.
//!
//! Where a sentence states a number as a fact, it takes it from whatever
//! enforces it rather than spelling it out: the platform's own terms for the
//! fees and the timelocks, and the constants below for the two the record
//! maths is defined by. A block of copy with a figure in it is a function for
//! that reason.

use shared::Strategy;

/// Live days before the leaderboard will rank a strategy.
const DAYS_TO_RANK: u32 = Strategy::DAYS_TO_RANK;

/// The annualised volatility every record is rescaled to, as a percentage.
const VOLATILITY_TARGET_PCT: f64 = Strategy::VOLATILITY_TARGET * 100.0;

/// The volatility every record is normalised to, as a sentence says it.
pub fn volatility_target() -> String {
    format!("{VOLATILITY_TARGET_PCT:.0}%")
}

/// The live days a strategy needs before the leaderboard will rank it, as a
/// sentence says it.
pub fn days_to_rank() -> String {
    DAYS_TO_RANK.to_string()
}

/// The line the whole product hangs on.
pub const PITCH: &str = "Smart contracts are public and auditable. Trading bots should be too.";

pub const HOME_LEDE: &str = "Authors publish trading strategies as source. Depositors pool capital behind the ones they \
     trust. Attested executors run them, and can only move funds between a vault's own \
     accounts.";

/// What a depositor is promised, what it rules out, and where it is enforced.
pub const GUARANTEES: [(&str, &str, &str); 6] = [
    (
        "Strategy identity",
        "Only the committed bundle can use a vault's keys, and the keys exist only inside \
         attested enclaves.",
        "Rules out swapping strategies, or trading by hand.",
    ),
    (
        "Funds stay home",
        "Money moves only between the vault's own accounts. Venues and contracts enforce that \
         themselves.",
        "Rules out transfers out, loans to related parties, rehypothecation.",
    ),
    (
        "Exit any time",
        "A withdrawal is paid from the idle balance at once, or booked as a claim the contract \
         settles itself.",
        "Rules out gated redemptions and depending on one operator.",
    ),
    (
        "Honest records",
        "Every trade is on-chain, and the vault contract computes its own value.",
        "Rules out backfilled records and self-reported value.",
    ),
    (
        "Nothing changes under you",
        "A published strategy is frozen. A new version is a separate vault you choose to join.",
        "Rules out being switched onto a strategy you didn't pick.",
    ),
    (
        "Auditable source",
        "Every bundle is compiled by us from a Rust project, and published unless the author \
         seals it. Either way, the reviewer read it.",
        "Rules out trading on code nobody has seen.",
    ),
];

/// The trust bar, in one line.
pub const HALT_NEVER_STEAL: &str = "Every lever anyone has ends in a halt, never a theft. Anyone can switch a strategy off; \
     nobody, including us, can take money out of a vault.";

/// How the platform works, end to end.
pub fn how_it_works() -> [(&'static str, String); 4] {
    [
        (
            "An author publishes source",
            "An attested builder compiles the Rust project with a pinned recipe and signs the \
             mapping from the source it read to the bundle it made."
                .to_string(),
        ),
        (
            "An attested reviewer reads it",
            "A pinned model scores whether the code is reasonable and lists what a careful \
             reader would worry about. A score, not a gate."
                .to_string(),
        ),
        (
            "A vault opens, and depositors choose",
            format!(
                "Shares are minted at the value the contract computes from the chain. The \
                 strategy is labelled new until it has {DAYS_TO_RANK} live days."
            ),
        ),
        (
            "Executors trade, and the chain records it",
            "Attested executors hold the vault's keys and can only move funds between its own \
             accounts. Every trade is public."
                .to_string(),
        ),
    ]
}

pub const FEE_NOTE: &str = "Fees crystallise daily as new shares, so paying them never forces a sale. Profit fees are \
     only taken on a new high-water mark. The transaction fee stays in the pool, so timing a \
     deposit around a price update can't take value from the depositors who stay.";

pub const TANK_NOTE: &str = "Running costs are the author's, not the depositors'. Each vault has a tank the author funds \
     and their fees refill: metered compute, and gas where a chain charges it.";

/// What anyone can check for themselves, how, and what they need to do it.
pub const VERIFY_CHECKS: [(&str, &str, &str); 5] = [
    (
        "The vault's keys belong to attested executors running the committed strategy",
        "Read the blessing on the attestation registry: each key's image digest, the strategy \
         hash it may run, the vault it may act for, and when it expires.",
        "A chain RPC",
    ),
    (
        "The strategy's bytecode came from its published source",
        "Rebuild the source with the published builder image and compare the bundle hash.",
        "The source and the builder image",
    ),
    (
        "These are all the vault's trades",
        "Read the vault's accounts on each venue. Every order, fill and transfer is already a \
         public, timestamped record.",
        "Chain RPCs and venue APIs",
    ),
    (
        "The returns are real",
        "Read value per share from the vault contract over time. It computes its own value from \
         on-chain state.",
        "A chain RPC",
    ),
    (
        "The image running today is one the allowlist permits",
        "Check the image digest against the registry's allowlist and the timelock on the \
         proposal that added it.",
        "A chain RPC and the executor's source",
    ),
];

pub const VERIFY_LIMIT: &str = "What the chain can't prove is that nothing happened outside it, and it doesn't need to: a \
     vault's keys can only move funds between the vault's own accounts.";

/// What an author does, from a template to an open vault.
pub fn publish_steps() -> [(&'static str, String); 5] {
    [
        (
            "Start from a template",
            "An ordinary Cargo project: your code, your dependencies, a pinned toolchain, and a \
             manifest that describes the strategy, its venues, its config and the compute it \
             needs."
                .to_string(),
        ),
        (
            "Write your limits in code",
            "There is no platform risk policy. A leverage cap or a drawdown stop is code you \
             write, and code the reviewer reads."
                .to_string(),
        ),
        (
            "Submit, and fund the vault's tank",
            "Pack the project and upload it, sealed if you would rather nobody else read it. \
             The build and the review are the tank's first charges."
                .to_string(),
        ),
        (
            "The queue builds it, and the reviewer reads it",
            "The build runs offline on a pinned recipe, and the reviewer reads the same \
             source. The log is public either way."
                .to_string(),
        ),
        (
            "The vault opens",
            format!(
                "Deposits from the moment it is registered, labelled new until it has \
                 {DAYS_TO_RANK} live days."
            ),
        ),
    ]
}

/// How the leaderboard decides what to show.
pub fn ranking() -> [(String, String); 4] {
    [
        (
            "Live records only".to_string(),
            "A record is a vault's own on-chain history. No backtests, no paper trading."
                .to_string(),
        ),
        (
            "Normalised for risk".to_string(),
            format!(
                "Returns are rescaled to a common {VOLATILITY_TARGET_PCT:.0}% annualised \
                 volatility, estimated only from the days before each one, so the list doesn't \
                 simply reward leverage."
            ),
        ),
        (
            format!("{DAYS_TO_RANK} days before ranking"),
            format!(
                "A strategy is open for deposits from its first day, labelled new. It isn't \
                 ranked until it has {DAYS_TO_RANK} live days."
            ),
        ),
        (
            "The size it was earned at".to_string(),
            "Every record shows the capital behind it. Nothing is extrapolated to a size the \
             strategy never traded."
                .to_string(),
        ),
    ]
}

pub const REVIEW_NOTE: &str = "The reviewer reads the source, sealed or not, and scores whether the code is reasonable. It \
     never rates expected profit, and it is never a gate.";

/// Shown in place of a figure the platform has not answered for yet. A page
/// says nothing rather than a number it made up.
pub const REACHING: &str = "Reaching the platform for the terms it is running under today.";

/// The exit band is the vault's own, so the sentence takes it.
pub fn exit_note(band: &str) -> String {
    format!(
        "A withdrawal is valued at exit-side marks, so the leaver pays the spread rather than \
         the pool. If the idle balance covers it, the shares are burned and paid in the same \
         transaction. If not, a claim is booked: the leaver's fraction of every position is \
         closed inside a {band} band around the venue's oracle price, and once the cash is home \
         the claim is paid. Anyone can send the settle transaction."
    )
}

/// Terms worth defining wherever they first appear.
pub const TERMS: [(&str, &str); 12] = [
    (
        "vault",
        "Pooled capital behind one strategy version, with its own accounts on every venue that \
         strategy uses.",
    ),
    (
        "bundle",
        "A strategy's code: one WASM component plus any models it calls, compiled by us from \
         submitted source and identified by one hash.",
    ),
    (
        "value per share",
        "The vault's value, computed by its own contract from on-chain balances and prices, \
         divided by the shares outstanding.",
    ),
    (
        "high-water mark",
        "The highest value per share a vault has crystallised fees at. Profit fees are only \
         taken above it.",
    ),
    (
        "tank",
        "The author's balance for the strategy's running costs. It is excluded from what shares \
         are worth.",
    ),
    (
        "claim",
        "What a withdrawal becomes when the idle balance can't pay it: the shares are pending \
         until the leaver's fraction of the vault is closed and the cash is home.",
    ),
    (
        "exit class",
        "Whether a vault's claims are served by the contract alone (direct) or need an unwinder \
         any allowlisted operator can run (served).",
    ),
    (
        "custody level",
        "Who refuses a withdrawal to a stranger: the venue, or contracts on the chain.",
    ),
    (
        "attested read",
        "A venue account's value read from the venue's API inside a measured image and signed \
         with a key blessed for that vault, where the value isn't on a chain.",
    ),
    (
        "blessing",
        "The on-chain authorisation of an executor key, for one vault and one bundle hash, valid \
         until its expiry.",
    ),
    (
        "normalised return",
        "A strategy's return after rescaling to a common volatility target.",
    ),
    (
        "sealed source",
        "Source stored encrypted and released only to attested builders, reviewers and \
         executors. The reviewer still reads it.",
    ),
];

/// What the platform deliberately doesn't do.
pub const NOT_THIS: [(&str, &str); 5] = [
    (
        "Not a rating agency",
        "The review informs a depositor. It never predicts profit, and no guarantee depends on \
         it.",
    ),
    (
        "Not a curated fund platform",
        "Nobody picks managers. Anyone can publish, and the public record decides.",
    ),
    (
        "Not a backtest gallery",
        "We run no simulations and display no author's. A record starts at the first live \
         trade.",
    ),
    (
        "Not a single-venue product",
        "Strategies span venues and chains, and every vault page shows each venue's custody \
         level.",
    ),
    (
        "Not a promise about returns",
        "A strategy can lose everything in its vault. Only its own code limits that.",
    ),
];
