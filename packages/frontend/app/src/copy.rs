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

/// Live days before an agent leaves the "new" list for the leaderboard.
const DAYS_NEW: u32 = Strategy::DAYS_NEW;

/// The live days an agent needs before it is on the leaderboard, as a sentence
/// says it.
pub fn days_new() -> String {
    DAYS_NEW.to_string()
}

/// The line the whole product hangs on.
pub const PITCH: &str = "Smart contracts are public and auditable. Trading agents should be too.";

pub const HOME_LEDE: &str = "Authors publish trading agents as source: real programs that watch the market, compute, \
     and decide. Depositors pool capital behind the ones they trust. Attested executors run \
     them, and can only move funds between a vault's own accounts.";

/// Where the long version of all of this lives.
pub const HANDBOOK_NOTE: &str = "This page is the short version. The handbook covers the same system at length: what an \
     agent is and what it can reach, what a deposit costs across a good year and a bad one, and \
     how to check every claim on this site against the chain yourself.";

/// What a depositor is promised, what it rules out, and where it is enforced.
pub const GUARANTEES: [(&str, &str, &str); 6] = [
    (
        "Agent identity",
        "Only the committed bundle can use a vault's keys, and the keys exist only inside \
         attested enclaves.",
        "Rules out swapping agents, or trading by hand.",
    ),
    (
        "Funds stay home",
        "Money moves only between the vault's own accounts. Venues and contracts enforce that \
         themselves.",
        "Rules out transfers out, loans to related parties, rehypothecation.",
    ),
    (
        "Exit any time",
        "A withdrawal is priced at the next valuation, seconds away, and paid from the cash; \
         one the cash can't cover becomes a claim that anyone can settle.",
        "Rules out gated redemptions and depending on one operator.",
    ),
    (
        "Honest records",
        "Every trade is on-chain, and the vault contract computes its own value.",
        "Rules out backfilled records and self-reported value.",
    ),
    (
        "Nothing changes under you",
        "A published agent is frozen. A changed agent is a new agent, with a vault of its own.",
        "Rules out being switched onto an agent you didn't pick.",
    ),
    (
        "Auditable source",
        "Every bundle is compiled by us from a Rust project, and published unless the author \
         seals it. Either way, the reviewer read it.",
        "Rules out trading on code nobody has seen.",
    ),
];

/// The trust bar, in one line.
pub const HALT_NEVER_STEAL: &str = "Every lever anyone has ends in a halt, never a theft. Anyone can switch an agent off; \
     nobody, including us, can take money out of a vault.";

/// How the platform works, end to end.
pub fn how_it_works() -> [(&'static str, String); 4] {
    [
        (
            "An attested reviewer reads the source",
            "Before anything is built, a pinned model rates the code in six areas and lists \
             what a careful reader would worry about. Only malicious code is refused."
                .to_string(),
        ),
        (
            "An attested builder compiles it",
            "With a pinned recipe, signing the mapping from the source it read to the bundle it \
             made."
                .to_string(),
        ),
        (
            "Its author activates a vault, and depositors choose",
            format!(
                "Shares are minted at the value the contract computes from the chain. The \
                 agent is labelled new until it has {DAYS_NEW} live days."
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

pub const FEE_NOTE: &str = "Fees are measured at every valuation and paid on the first one on or after Monday as new \
     shares, so paying them never forces a sale, and the share value shown is always after fees. \
     Profit fees are only taken above the high-water mark, and money joining above it joins at its \
     own price. A leaver pays the fee their own shares owe. The transaction fee stays in the pool, \
     so timing a deposit around a price update can't take value from the depositors who stay.";

pub const TANK_NOTE: &str = "Running costs are the author's, not the depositors'. Each vault has a tank the author funds \
     and can keep topped up from their own shares: metered compute, and gas where a chain charges \
     it, never more than the allowances the author set.";

/// What anyone can check for themselves, how, and what they need to do it.
pub const VERIFY_CHECKS: [(&str, &str, &str); 5] = [
    (
        "The vault's keys belong to attested executors running the committed agent",
        "Read the blessing on the attestation registry: each key's image digest, the agent \
         hash it may run, the vault it may act for, and when it expires.",
        "A chain RPC",
    ),
    (
        "The agent's bytecode came from its published source",
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
             shonin.toml naming the chain your vault lives on, what the agent does and claims, \
             and the drivers it trades through."
                .to_string(),
        ),
        (
            "Write your limits in code",
            "There is no platform risk policy. A leverage cap or a drawdown stop is code you \
             write, claimed in your shonin.toml, and checked by the reviewer."
                .to_string(),
        ),
        (
            "Submit it for $1",
            "Upload the project, sealed if you would rather nobody else read it, and pay $1 from \
             the same wallet. The payment gives your agent its id."
                .to_string(),
        ),
        (
            "It is reviewed, then built",
            "The reviewer reads the source first; then the build runs offline on a pinned recipe. \
             The queue shows how it ended, never a build log."
                .to_string(),
        ),
        (
            "Activate it when you're ready",
            format!(
                "Read the review, then activate: choose its allowances and fund its tank, and the \
                 vault opens, labelled new until it has {DAYS_NEW} live days."
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
            "Plain facts, sorted as you like".to_string(),
            "The return over 30 days and since opening, the deepest drop and the money in the \
             vault, each read from the vault's own contract. Pick the column to sort by."
                .to_string(),
        ),
        (
            format!("{DAYS_NEW} days before the leaderboard"),
            format!(
                "An agent is open for deposits from its first day, listed separately as new \
                 until it has {DAYS_NEW} live days."
            ),
        ),
        (
            "The size it was earned at".to_string(),
            "Every record shows the capital behind it. Nothing is extrapolated to a size the \
             agent never traded."
                .to_string(),
        ),
    ]
}

pub const REVIEW_NOTE: &str = "The reviewer reads the source, sealed or not, before it is built, and rates it in six areas. \
     It never rates expected profit, and it only refuses code written to do harm.";

/// Shown in place of a figure the platform has not answered for yet. A page
/// says nothing rather than a number it made up.
pub const REACHING: &str = "Reaching the platform for the terms it is running under today.";

/// The exit band is the vault's own, so the sentence takes it.
pub fn exit_note(band: &str) -> String {
    format!(
        "A withdrawal is valued at exit-side marks, so the leaver pays the spread rather than \
         the pool. It is priced at the next valuation, seconds away: if the cash covers it, the \
         shares are burned and paid then. If not, a claim is booked: the leaver's fraction of \
         every position is closed inside a {band} band around the venue's oracle price and paid \
         in the same transaction. Anyone can send it."
    )
}

/// Terms worth defining wherever they first appear.
pub const TERMS: [(&str, &str); 12] = [
    (
        "vault",
        "Pooled capital behind one agent, with its own accounts on every venue that agent uses.",
    ),
    (
        "bundle",
        "An agent's code: one WASM component plus any models it calls, compiled by us from \
         submitted source and identified by one hash.",
    ),
    (
        "value per share",
        "The vault's value, computed by its own contract from on-chain balances and prices, \
         divided by the shares outstanding.",
    ),
    (
        "high-water mark",
        "The highest value per share a vault has paid fees at. Profit fees are only taken above \
         it.",
    ),
    (
        "tank",
        "The author's balance for the agent's running costs. It is excluded from what shares \
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
        "deepest drop",
        "The largest fall in value per share from an earlier high.",
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
        "Agents span venues and chains, and every vault page shows each venue's custody \
         level.",
    ),
    (
        "Not a promise about returns",
        "An agent can lose everything in its vault. Only its own code limits that.",
    ),
];
