//! The public feed: every kill with its reason, every change to the image
//! allowlist, and anything else that happens to a running vault.

use shared::{FeedEvent, FeedKind};

use super::{hours_ago, sha256, tx};

struct Spec {
    hours_ago: u32,
    kind: FeedKind,
    title: &'static str,
    detail: &'static str,
    link: Option<&'static str>,
}

pub fn all() -> Vec<FeedEvent> {
    vec![
        Spec {
            hours_ago: 2,
            kind: FeedKind::VenueSuspended,
            title: "Arcus marked suspended",
            detail:
                "Order rejections stayed above a quarter of submissions for two hours. Every \
                 strategy trading there has been told, and each vault's page shows how much of \
                 its value sits on the venue. Deposits and withdrawals continue; a claim on that \
                 leg waits for the venue rather than for us.",
            link: Some("/strategies/basis-arcus-v1/health"),
        },
        Spec {
            hours_ago: 19,
            kind: FeedKind::ImageProposed,
            title: "Executor image proposed: sha256:9f4c…",
            detail:
                "A patched runtime. It can be blessed once the two-day timelock runs out, which \
                 is there so anyone can rebuild the image from its published source and compare \
                 the digest before it runs anywhere. The previous known-good image stays \
                 allowlisted.",
            link: Some("/verify"),
        },
        Spec {
            hours_ago: 62,
            kind: FeedKind::Kill,
            title: "basis-rh v1 killed",
            detail:
                "The strategy kept submitting into a suspended venue for six hours with \
                 rejections above a quarter of submissions. The vault's positions were closed \
                 reduce-only inside the exit band and the cash brought home, so every withdrawal \
                 since is paid from the idle balance in one transaction.",
            link: Some("/strategies/basis-rh-v1"),
        },
        Spec {
            hours_ago: 74,
            kind: FeedKind::ReviewPublished,
            title: "A fresh review of mm-quoter v7",
            detail:
                "Bought by a depositor under the current reviewer image, at the flat fee anyone \
                 pays. The newest review is shown first and the earlier ones are kept; nothing \
                 is re-reviewed automatically when the reviewer changes.",
            link: Some("/strategies/mm-quoter-v7/review"),
        },
        Spec {
            hours_ago: 120,
            kind: FeedKind::ClaimNote,
            title: "A claim on basis-arcus v1 is waiting for its venue",
            detail:
                "The leaver's fraction of the perpetual leg has not closed inside the exit band, \
                 so the claim is retried rather than filled at any price. If it goes unserved for \
                 seven days the vault freezes: no new deposits, and the home chain's trading \
                 approvals revoked.",
            link: Some("/strategies/basis-arcus-v1"),
        },
        Spec {
            hours_ago: 27 * 24,
            kind: FeedKind::StrategyRegistered,
            title: "arena v1 registered",
            detail:
                "Built from submitted source by the attested builder, signed, and registered. \
                 Its vault opened for deposits the same hour, labelled new, and it will not be \
                 ranked until it has ninety live days.",
            link: Some("/strategies/arena-v1"),
        },
        Spec {
            hours_ago: 34 * 24,
            kind: FeedKind::ImageRevoked,
            title: "Executor image revoked: sha256:2b81…",
            detail:
                "Revoked for cause after a bug was found in its runtime. Revocation is instant, \
                 and it never hands a vault's keys to another runtime: the standby on the \
                 already-allowlisted successor took over, with each vault's own on-chain \
                 approval as the handoff.",
            link: Some("/verify"),
        },
        Spec {
            hours_ago: 41 * 24,
            kind: FeedKind::StrategyRegistered,
            title: "catalyst v1 registered",
            detail:
                "A sealed-source strategy with a model in its bundle. The builder compiled it and \
                 the reviewer read it; what sealing changes is who else can.",
            link: Some("/strategies/catalyst-v1"),
        },
        Spec {
            hours_ago: 63 * 24,
            kind: FeedKind::VenueRestored,
            title: "Stock tokens restored after a chain stall",
            detail:
                "The chain stopped producing blocks for eleven minutes. Nothing was lost: while \
                 it lasted, no transaction landed, including withdrawals, which is disclosed on \
                 every vault that trades there.",
            link: None,
        },
    ]
    .into_iter()
    .map(build)
    .collect()
}

fn build(spec: Spec) -> FeedEvent {
    FeedEvent {
        at: hours_ago(spec.hours_ago),
        kind: spec.kind,
        title: spec.title.to_string(),
        detail: spec.detail.to_string(),
        link: spec.link.map(str::to_string),
        reference: if matches!(spec.kind, FeedKind::ImageProposed | FeedKind::ImageRevoked) {
            sha256(spec.title)
        } else {
            tx(spec.title)
        },
    }
}
