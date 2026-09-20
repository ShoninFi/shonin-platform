//! The words the app says about itself.
//!
//! Kept in one place so the pre-rendered page and the app say exactly the
//! same thing, and so the product's copy can be read and edited on its own.

/// The line the whole product hangs on.
pub const PITCH: &str = "Smart contracts are public and auditable. Trading bots should be too.";

pub const HOME_LEDE: &str = "Shonin is an open platform for verifiable trading strategies. Authors publish strategies as \
     source, which we compile under attestation. Depositors pool capital behind the ones they \
     trust. Attested executors run them around the clock, and can only move funds between a \
     vault's own accounts.";

/// What a depositor is promised, what it rules out, and where it is enforced.
pub const GUARANTEES: [(&str, &str, &str); 6] = [
    (
        "Strategy identity",
        "Only the committed bundle can use a vault's keys. The keys exist inside attested \
         enclaves and nowhere else.",
        "Rules out a manager quietly swapping strategies, or trading by hand.",
    ),
    (
        "Funds stay home",
        "Money moves only between the vault's own accounts. Venues and contracts enforce that \
         themselves, whatever the strategy's code does.",
        "Rules out loans to related parties, transfers out, rehypothecation.",
    ),
    (
        "Exit any time",
        "A withdrawal is paid from the idle balance at once, or booked as a claim that the \
         contract settles itself — and any allowlisted operator can serve the rest.",
        "Rules out gated redemptions and depending on one operator to get your money back.",
    ),
    (
        "Honest records",
        "Every trade is on-chain, and the vault contract computes its own value rather than \
         being told it.",
        "Rules out backfilled or cherry-picked track records and self-reported value.",
    ),
    (
        "Nothing changes under you",
        "A published strategy is frozen. A new version is a separate vault you choose to join.",
        "Rules out being switched onto a strategy you didn't pick.",
    ),
    (
        "Auditable source",
        "Every bundle is compiled by us from a Rust project, and published unless the author \
         seals it. Sealed or not, the reviewer read it.",
        "Rules out trading on code nobody has ever seen.",
    ),
];

/// The trust bar, in one line.
pub const HALT_NEVER_STEAL: &str = "Every lever anyone has ends in a halt, never a theft. Anyone can switch a strategy off; \
     nobody, including us, can take money out of a vault or trade it on anything but the \
     committed strategy.";

/// How the platform works, end to end.
pub const HOW_IT_WORKS: [(&str, &str); 4] = [
    (
        "An author publishes source",
        "They submit a Rust project. An attested builder compiles it twice, checks the bytes \
         match, and signs the mapping from source to bundle. The registry accepts nothing \
         without that signature.",
    ),
    (
        "An attested reviewer reads it",
        "A pinned model scores whether the code is reasonable and lists what a careful reader \
         would worry about. It is a score, not a gate: a low one is loud, and it blocks nothing.",
    ),
    (
        "A vault opens, and depositors choose",
        "Shares are minted at the value the contract computes from the chain in the same \
         transaction. The strategy is labelled new until it has ninety live days.",
    ),
    (
        "Executors trade, and the chain records it",
        "Attested executors hold the vault's keys and can only move funds between its own \
         accounts. Every trade is public, and returns are read from the vault's own value per \
         share.",
    ),
];

/// The fee schedule, as a table: what it is, what it is charged on, who gets
/// it, and the default level.
pub const FEES: [(&str, &str, &str, &str); 4] = [
    (
        "Streaming fee",
        "The vault's value, continuously",
        "Shonin",
        "0.5% a year",
    ),
    (
        "Author fee",
        "Gains above the vault's high-water mark, taken daily",
        "The author, filling the vault's tank first",
        "10% of profit",
    ),
    ("Shonin fee", "The same gains", "Shonin", "5% of profit"),
    (
        "Transaction fee",
        "Every deposit and withdrawal",
        "Nobody: it stays in the pool",
        "0.1%",
    ),
];

pub const FEE_NOTE: &str = "Fees crystallise daily as newly minted shares, so paying them never forces a strategy to \
     sell anything. Profit fees are only ever taken on a new high-water mark: a vault that loses \
     and then recovers pays nothing on the recovery. The transaction fee is not paid to anyone — \
     it stays in the pool, so deposits and withdrawals timed around a price update can't take \
     value from the depositors who stay.";

pub const TANK_NOTE: &str = "Running costs are the author's, not the depositors'. Each vault has a tank the author funds \
     and their fees refill: metered compute, charged against the allowances the strategy \
     declared, and gas where a chain charges it. An empty tank becomes a debt against future \
     author fees.";

/// What anyone can check for themselves, how, and what they need to do it.
pub const VERIFY_CHECKS: [(&str, &str, &str); 5] = [
    (
        "The vault's keys belong to attested executors running the committed strategy",
        "Read the blessing on the attestation registry: it names each key's image digest, the \
         strategy hash it may run, the vault it may act for, and when the blessing expires.",
        "A chain RPC",
    ),
    (
        "The strategy's bytecode came from its published source",
        "Rebuild the source with the published builder image and compare the bundle hash. The \
         recipe is pinned and offline, so anyone gets the same bytes.",
        "The source and the builder image",
    ),
    (
        "These are all the vault's trades",
        "Read the vault's accounts on each venue. Every order, fill and transfer on a venue that \
         can hold vault capital is already a public, timestamped record.",
        "Chain RPCs and venue APIs",
    ),
    (
        "The returns are real",
        "Read value per share from the vault contract over time. It computes its own value from \
         on-chain state; nobody reports it.",
        "A chain RPC",
    ),
    (
        "The image running today is one the allowlist permits",
        "Check the image digest against the registry's allowlist, and the two-day timelock on \
         the proposal that added it. Rebuild the image from its published source and compare.",
        "A chain RPC and the executor's source",
    ),
];

pub const VERIFY_LIMIT: &str = "What the chain can't prove is that nothing happened outside it — and it doesn't need to. A \
     vault's keys can only move funds between the vault's own accounts, and the venues and \
     contracts enforce that themselves.";

/// What an author does, from a template to an open vault.
pub const PUBLISH_STEPS: [(&str, &str); 6] = [
    (
        "Start from a template",
        "An ordinary Cargo project: your code, your dependencies, a pinned toolchain, and a \
         manifest that describes the strategy, its venues, its config and the compute it needs. \
         There is no tool of ours to install.",
    ),
    (
        "Write your limits in code",
        "There is no platform risk policy. If your strategy should never exceed a leverage, or \
         should stop after a drawdown, that is code you write — and code the reviewer reads.",
    ),
    (
        "Test it yourself",
        "Backtesting is yours: your data, your assumptions, your fill model. Nothing you test \
         appears on the leaderboard, and we run no simulations of our own.",
    ),
    (
        "Submit, and fund the vault's tank",
        "Point us at a public repository and commit, or upload the project encrypted to seal it. \
         Creating the vault funds its tank, and the build and the review are its first charges.",
    ),
    (
        "The queue builds it, and the reviewer reads it",
        "The build runs offline on a pinned recipe, twice, and the bytes must match. An attested \
         builder signs the result; the registry accepts nothing else. The log is public either \
         way.",
    ),
    (
        "The vault opens",
        "Deposits from the moment it is registered, labelled new until it has ninety live days. \
         Your record starts at your first trade.",
    ),
];

/// The two products, side by side.
pub const PRODUCTS: [(&str, &str, &str); 6] = [
    (
        "Whose money",
        "Many depositors, pooled in a contract",
        "One owner, in their own account",
    ),
    (
        "Who stops a withdrawal to a stranger",
        "The venue and the vault contract",
        "The broker: the strategy can trade, never withdraw",
    ),
    (
        "How value is known",
        "Computed by the contract from the chain",
        "An attested read of the account, posted on-chain",
    ),
    (
        "How you leave",
        "Withdraw through the contract: paid from idle at once, or as a claim it settles itself",
        "Revoke the strategy's access at your broker and close the entry",
    ),
    (
        "What the author earns",
        "10% of the vault's gains above its high-water mark",
        "10% of the account's gains above its own high-water mark",
    ),
    (
        "Where it lives",
        "The strategies leaderboard",
        "The connected-accounts catalogue",
    ),
];

pub const CONNECTED_LEDE: &str = "A connected account is your own account at your own broker, running a published strategy. \
     The broker supplies custody — the strategy holds a token it only lets trade — and Shonin \
     supplies the rest: proof of which code is trading, a review that read it, and one record \
     across every account running it.";

pub const CONNECTED_RECORD_NOTE: &str = "A connected-account record is the aggregate of the accounts running the strategy, built \
     from hourly attested reads posted on-chain and netted for owners' own deposits and \
     withdrawals. It is labelled as attested reads, one notch below a vault's on-chain record, \
     and it sits in its own catalogue because the two aren't comparable.";

/// How the leaderboard decides what to show.
pub const RANKING: [(&str, &str); 4] = [
    (
        "Live records only",
        "A record is built from a vault's own on-chain history and nothing else. No backtests, \
         no paper trading, and nothing from before the strategy traded here.",
    ),
    (
        "Normalised for risk",
        "Ranking on raw returns would mostly rank leverage. Returns are rescaled to a common 15% \
         annualised volatility target, estimated only from the days before each one, and scaled \
         up no more than three times.",
    ),
    (
        "Ninety days before ranking",
        "A strategy is listed and open for deposits from its first day, labelled new. It isn't \
         ranked until it has ninety live days, so a lucky fortnight never reaches the top.",
    ),
    (
        "The size it was earned at",
        "Returns earned at $200k say little about $20m. Every record shows the capital behind it, \
         and nothing is extrapolated to a size the strategy never traded.",
    ),
];

pub const REVIEW_NOTE: &str = "An attested reviewer reads the source — sealed or not — and scores whether the code is \
     reasonable: bounded, honest about what it depends on, and able to handle failure. It never \
     rates expected profit, and it is never a gate. What a depositor can rely on is elsewhere: \
     only the committed bundle can use the vault's keys, funds can't leave the vault's own \
     accounts, exits don't depend on us, and the record is the chain.";

pub const EXIT_NOTE: &str = "A withdrawal is valued at exit-side marks — a long at the bid, a short at the ask — so the \
     leaver pays the spread rather than the pool. If the idle balance covers it, the shares are \
     burned and paid in the same transaction. If it doesn't, the shares are marked pending and a \
     claim is booked: the leaver's fraction of every position is closed inside a 1% band around \
     the venue's oracle price, and once the cash is home the vault is valued again and the claim \
     paid. Anyone can send the settle transaction.";

/// Terms worth defining wherever they first appear.
pub const TERMS: [(&str, &str); 12] = [
    (
        "vault",
        "Pooled capital behind one strategy version, with its own accounts on every venue that \
         strategy uses. A new version gets a new vault.",
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
        "The highest value per share a vault has crystallised fees at. Profit fees are only ever \
         taken above it.",
    ),
    (
        "tank",
        "The author's balance for the strategy's running costs: metered compute and gas. It is \
         excluded from what shares are worth.",
    ),
    (
        "claim",
        "What a withdrawal becomes when the idle balance can't pay it: the shares are pending \
         while the leaver's fraction of the vault is closed and sold, and are burned at the \
         marks once the cash is home.",
    ),
    (
        "exit class",
        "Whether a vault's claims are served by the contract alone (direct) or need an unwinder \
         any allowlisted operator can run (served).",
    ),
    (
        "custody level",
        "Who refuses a withdrawal to a stranger: the venue, contracts on the chain, or — for a \
         connected account — the broker.",
    ),
    (
        "attested read",
        "A venue account's value read from the venue's API inside a measured image and signed \
         with a key blessed for that vault, used where the value isn't on a chain.",
    ),
    (
        "blessing",
        "The on-chain authorisation of an executor key, for one vault and one bundle hash, valid \
         until its expiry. Only blessed keys can trade a vault's accounts.",
    ),
    (
        "normalised return",
        "A strategy's return after rescaling to a common volatility target, so the leaderboard \
         doesn't simply reward leverage.",
    ),
    (
        "sealed source",
        "Source stored encrypted and released only to attested builders, reviewers and \
         executors. The reviewer still reads it; sealing decides who else does.",
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
        "Nobody picks managers. Anyone can publish, and the public record decides who gets \
         capital.",
    ),
    (
        "Not a backtest gallery",
        "We run no simulations and display no author's. A record starts at a strategy's first \
         live trade.",
    ),
    (
        "Not a single-venue product",
        "Strategies span venues and chains, and every vault page shows each venue's custody level \
         and how much of the vault sits there.",
    ),
    (
        "Not a promise about returns",
        "A strategy can lose everything in its vault. That is market risk, and only the \
         strategy's own code limits it.",
    ),
];
