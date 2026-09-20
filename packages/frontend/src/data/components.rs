//! The component registry: reusable pieces of a strategy, always published
//! with their source, always pinned by hash.

use shared::{AuditStatus, Component};

use super::source::build_log;
use super::{days_ago, sha256};

struct Spec {
    slug: &'static str,
    name: &'static str,
    version: &'static str,
    author: &'static str,
    first_party: bool,
    published_days_ago: u32,
    summary: &'static str,
    description: &'static str,
    exports: &'static [&'static str],
    imports: &'static [&'static str],
    api: &'static [(&'static str, &'static str)],
    versions: &'static [(&'static str, &'static str)],
    warnings: &'static [&'static str],
    audit: AuditStatus,
}

fn build(spec: Spec) -> Component {
    Component {
        slug: spec.slug.to_string(),
        name: spec.name.to_string(),
        version: spec.version.to_string(),
        hash: sha256(&format!("component-{}", spec.slug)),
        author: spec.author.to_string(),
        first_party: spec.first_party,
        published: days_ago(spec.published_days_ago),
        summary: spec.summary.to_string(),
        description: spec.description.to_string(),
        exports: spec.exports.iter().map(|s| (*s).to_string()).collect(),
        imports: spec.imports.iter().map(|s| (*s).to_string()).collect(),
        api: spec
            .api
            .iter()
            .map(|(item, doc)| ((*item).to_string(), (*doc).to_string()))
            .collect(),
        repo: format!("github.com/{}/{}", spec.author.replace('.', "-"), spec.slug),
        build_log: build_log(spec.slug, 38, &[]),
        versions: spec
            .versions
            .iter()
            .map(|(version, note)| {
                (
                    (*version).to_string(),
                    sha256(&format!("component-{}-{version}", spec.slug)),
                    (*note).to_string(),
                )
            })
            .collect(),
        review_warnings: spec.warnings.iter().map(|s| (*s).to_string()).collect(),
        audit: spec.audit,
    }
}

pub fn all() -> Vec<Component> {
    vec![
        build(Spec {
            slug: "hedge",
            name: "hedge",
            version: "1.2",
            author: "shonin",
            first_party: true,
            published_days_ago: 243,
            summary: "Keeps a target hedge ratio between two legs, even on different venues.",
            description: "Takes the position you have and the position you want hedged against it, and \
                 returns the orders that close the gap. It knows nothing about prices beyond \
                 what you pass it, holds no state of its own between calls, and can only ever \
                 produce actions for the vault's own accounts — like everything else in a \
                 bundle, it runs inside the strategy's instance with the strategy's world.",
            exports: &["shonin:hedge/hedge@1.2.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Hedge::new(tolerance) -> Hedge",
                    "A hedger that ignores gaps smaller than the tolerance, so it doesn't churn.",
                ),
                (
                    "Hedge::step(&book, market) -> Vec<Action>",
                    "The orders that bring the hedge leg back within tolerance of the target.",
                ),
                (
                    "Hedge::ratio(&book) -> f64",
                    "How well hedged the book is now, as a fraction of the target ratio.",
                ),
            ],
            versions: &[
                (
                    "1.2",
                    "Handles a leg being scaled down mid-hedge, which is what a claim does.",
                ),
                (
                    "1.1",
                    "Tolerance is a fraction of the target rather than an absolute size.",
                ),
                ("1.0", "First release."),
            ],
            warnings: &[],
            audit: AuditStatus::Audited,
        }),
        build(Spec {
            slug: "twap-exec",
            name: "twap-exec",
            version: "0.9",
            author: "shonin",
            first_party: true,
            published_days_ago: 243,
            summary: "Splits a target position change into child orders over time or volume.",
            description: "Working a large change into the market in slices, with back-off when the venue \
                 rejects and a hard stop on the total worked. It does not decide what the target \
                 is; it only decides how to get there without asking for all of it at once.",
            exports: &["shonin:twap/exec@0.9.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Twap::new(slice, interval) -> Twap",
                    "A worker that sends at most one slice per interval.",
                ),
                (
                    "Twap::step(market, delta) -> Vec<Action>",
                    "The next slice towards the target change, or nothing if it is too soon.",
                ),
                (
                    "Twap::on_rejected(reason)",
                    "Backs off. Repeated rejections widen the interval rather than repeating.",
                ),
            ],
            versions: &[
                ("0.9", "Exponential back-off on repeated rejections."),
                ("0.8", "First release."),
            ],
            warnings: &[],
            audit: AuditStatus::Audited,
        }),
        build(Spec {
            slug: "mm-quoter",
            name: "mm-quoter",
            version: "2.1",
            author: "shonin",
            first_party: true,
            published_days_ago: 219,
            summary: "Computes bid and ask ladders from fair value, inventory and volatility.",
            description: "The arithmetic of quoting, separated from the decisions around it: given a fair \
                 value, an inventory skew and a size limit, it produces a ladder, and given your \
                 current orders it produces the difference. What to quote, when to pull and how \
                 much inventory to carry stay in the strategy, where the review reads them.",
            exports: &["shonin:mm/quoter@2.1.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Quoter::quotes(fair, skew, max) -> Ladder",
                    "A two-sided ladder, skewed against inventory and capped at max.",
                ),
                (
                    "Quoter::diff(&ladder) -> Vec<Action>",
                    "The cancels and places that turn the resting orders into this ladder.",
                ),
                ("Quoter::cancel_all() -> Vec<Action>", "Pull everything."),
            ],
            versions: &[
                (
                    "2.1",
                    "Ladder sizes taper rather than stepping, which fills better.",
                ),
                (
                    "2.0",
                    "Skew is a function of inventory usage, not absolute size.",
                ),
                ("1.8", "First release."),
            ],
            warnings: &[],
            audit: AuditStatus::Audited,
        }),
        build(Spec {
            slug: "rebalancer",
            name: "rebalancer",
            version: "1.0",
            author: "shonin",
            first_party: true,
            published_days_ago: 219,
            summary: "Moves capital between the vault's own accounts when margin or exposure drifts.",
            description: "Produces the transfer that brings margin usage back under a ceiling. Every \
                 destination it can name is one of the vault's own registered accounts — there \
                 is no other kind of destination, because the contracts and venues enforce that \
                 themselves, whatever a component asks for.",
            exports: &["shonin:rebalance/mover@1.0.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Rebalancer::new(ceiling) -> Rebalancer",
                    "Acts when margin usage passes the ceiling.",
                ),
                (
                    "Rebalancer::step(&book) -> Option<Action>",
                    "The transfer to make, or nothing if the book is inside its limits.",
                ),
            ],
            versions: &[("1.0", "First release.")],
            warnings: &[],
            audit: AuditStatus::Audited,
        }),
        build(Spec {
            slug: "follow-wallet",
            name: "follow-wallet",
            version: "1.4",
            author: "shonin",
            first_party: true,
            published_days_ago: 201,
            summary: "Tracks a public account's positions and emits the orders to match them.",
            description: "Turns one account's visible positions into a target shape for yours, scaled \
                 however you choose. It has no opinion about whether following that account is a \
                 good idea; that judgement, and the limits around it, belong in the strategy.",
            exports: &["shonin:follow/wallet@1.4.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Follow::target(positions, scale) -> Target",
                    "The shape to hold, scaled from the followed account's positions.",
                ),
                (
                    "Follow::orders(&book, target) -> Vec<Action>",
                    "The orders that move your book towards that shape.",
                ),
            ],
            versions: &[
                (
                    "1.4",
                    "Ignores markets the caller excludes, by name or by kind.",
                ),
                (
                    "1.2",
                    "Scales by account value rather than by position count.",
                ),
                ("1.0", "First release."),
            ],
            warnings: &[
                "Reviews of strategies composing this component have repeatedly noted that \
                 mirroring arrives after the move it copies, and that the followed account owes \
                 the vault nothing.",
            ],
            audit: AuditStatus::InProgress,
        }),
        build(Spec {
            slug: "mcp",
            name: "mcp",
            version: "1.0",
            author: "shonin",
            first_party: true,
            published_days_ago: 167,
            summary: "Speaks the Model Context Protocol over the host's HTTP.",
            description: "Initialise, discover tools, call them, handle errors and retries. It lives in \
                 the bundle rather than in the executor image on purpose: a protocol client is \
                 not something that has to be trusted, so it belongs where the author can \
                 version it and the reviewer can read it, not behind an image timelock.",
            exports: &["shonin:mcp/client@1.0.0"],
            imports: &[
                "wasi:http/outgoing-handler@0.3.0",
                "shonin:strategy/types@0.1.0",
            ],
            api: &[
                ("Mcp::connect(url, token) -> Mcp", "Initialises a session."),
                (
                    "Mcp::tools() -> Vec<Tool>",
                    "Discovers what the server offers.",
                ),
                (
                    "Mcp::call(tool, args) -> Result<Value>",
                    "Calls a tool, with retries and a bounded back-off.",
                ),
            ],
            versions: &[("1.0", "First release.")],
            warnings: &[],
            audit: AuditStatus::InProgress,
        }),
        build(Spec {
            slug: "oauth",
            name: "oauth",
            version: "1.1",
            author: "shonin",
            first_party: true,
            published_days_ago: 167,
            summary: "Holds a sealed refresh token and keeps an access token current.",
            description: "The owner completes the authorisation once on their own machine and seals the \
                 refresh token to the bundle hash. This component exchanges it for access tokens \
                 as they expire. The sealed value never appears in a log or a record, and only \
                 an attested executor running that exact bundle can read it.",
            exports: &["shonin:oauth/tokens@1.1.0"],
            imports: &[
                "wasi:http/outgoing-handler@0.3.0",
                "shonin:strategy/secrets@0.1.0",
            ],
            api: &[
                (
                    "OAuth::new(refresh) -> OAuth",
                    "Wraps a sealed refresh token read from `secrets`.",
                ),
                (
                    "OAuth::access() -> Result<String>",
                    "A current access token, refreshed when it is close to expiry.",
                ),
            ],
            versions: &[
                (
                    "1.1",
                    "Refreshes early rather than on failure, so a call never fails twice.",
                ),
                ("1.0", "First release."),
            ],
            warnings: &[],
            audit: AuditStatus::InProgress,
        }),
        build(Spec {
            slug: "funding-rank",
            name: "funding-rank",
            version: "0.4",
            author: "kestrel.lab",
            first_party: false,
            published_days_ago: 138,
            summary: "Ranks perpetuals by funding, with hysteresis so a ranking doesn't flap.",
            description: "A community component. It takes funding rates and returns an order, holding a \
                 position in the ranking until the gap to the next one is wide enough to be \
                 worth acting on. It does arithmetic on numbers the strategy already has: it \
                 reads nothing, holds nothing and can produce no actions of its own.",
            exports: &["kestrel:funding/rank@0.4.0"],
            imports: &["shonin:strategy/types@0.1.0"],
            api: &[
                (
                    "Rank::new(threshold) -> Rank",
                    "A ranking that only changes when the gap exceeds the threshold.",
                ),
                (
                    "Rank::top(rates, n) -> Vec<Market>",
                    "The n best-paying markets, under that hysteresis.",
                ),
            ],
            versions: &[
                (
                    "0.4",
                    "Hysteresis is a rate difference rather than a position difference.",
                ),
                ("0.3", "First release."),
            ],
            warnings: &[
                "One review noted that this component has had far less use under real capital \
                 than the first-party components beside it.",
            ],
            audit: AuditStatus::Unaudited,
        }),
    ]
}
