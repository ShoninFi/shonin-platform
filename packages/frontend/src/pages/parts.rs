//! Pieces that more than one page shows: how a strategy is labelled, how a
//! record is summarised, and the card a strategy appears as in a list.

use dominator::{Dom, html, link, text};
use shared::{Custody, ExitClass, HealthLevel, Leg, Standing, Strategy, VaultState};

use shared::model::registry::JobState;

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::ui::chart;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;

/// Where a strategy's page is.
pub fn path_of(strategy: &Strategy) -> String {
    format!("/strategies/{}", strategy.slug)
}

/// How a strategy's standing reads.
pub fn standing_badge(standing: Standing) -> Dom {
    let tone = match standing {
        Standing::Ranked => Tone::Accent,
        Standing::New => Tone::Info,
        Standing::Superseded | Standing::Retired => Tone::Neutral,
        Standing::Killed | Standing::BuildFailed => Tone::Error,
    };
    ui::badge(standing.label(), tone)
}

/// Whether the source can be read by anyone.
pub fn source_badge(strategy: &Strategy) -> Dom {
    ui::badge(
        strategy.bundle.source.label(),
        if strategy.bundle.source.is_published() {
            Tone::Ok
        } else {
            Tone::Neutral
        },
    )
}

/// The review score, or that it hasn't arrived.
pub fn review_badge(strategy: &Strategy) -> Dom {
    match &strategy.review {
        Some(review) => ui::score(review.score, review.band()),
        None => ui::badge("review pending", Tone::Neutral),
    }
}

/// How a venue's custody reads: the stronger the enforcement, the better.
pub fn custody_badge(custody: Custody) -> Dom {
    ui::badge(
        custody.label(),
        match custody {
            Custody::VenueEnforced => Tone::Ok,
            Custody::ChainEnforced => Tone::Info,
        },
    )
}

/// Whether leaving needs anyone.
pub fn exit_badge(class: ExitClass) -> Dom {
    ui::badge(
        class.label(),
        match class {
            ExitClass::Direct => Tone::Ok,
            ExitClass::Served { .. } => Tone::Info,
        },
    )
}

/// The health of a running strategy, at a glance.
pub fn health_badge(strategy: &Strategy) -> Option<Dom> {
    let health = strategy.health.as_ref()?;
    let level = health.level();
    Some(ui::badge_dot(level.label(), Tone::from_level(level)))
}

/// Every label a strategy carries, in one row.
pub fn labels(strategy: &Strategy) -> Dom {
    let mut items = vec![
        standing_badge(strategy.standing),
        source_badge(strategy),
        exit_badge(strategy.vault.exit_class),
    ];
    if strategy.vault.state == VaultState::Frozen {
        items.push(ui::badge("frozen", Tone::Error));
    }
    if let Some(health) = health_badge(strategy) {
        items.push(health);
    }
    ui::badges(items)
}

/// The line under a strategy's name: what it is, and who wrote it.
pub fn byline(strategy: &Strategy) -> Dom {
    html!("p", {
        .class("muted")
        .child(text(&format!("{} · by ", strategy.title)))
        .child(link!(format!("/authors/{}", strategy.author), {
            .text(&strategy.author)
        }))
        .child(text(&format!(" · {} live", fmt::days(strategy.days_live))))
    })
}

/// The shape of a record, for a table row or a card.
pub fn spark(strategy: &Strategy) -> Dom {
    match &strategy.record {
        Some(record) => chart::sparkline(&record.value_per_share),
        None => ui::muted("—"),
    }
}

/// A strategy as a card, for the home page and any other list of a few.
pub fn strategy_card(strategy: &Strategy) -> Dom {
    let performance = data::performance(strategy);

    link!(path_of(strategy), {
        .class(["card", "card-link", "stack"])
        .child(html!("div", {
            .class(["row", "row--between", "row--top"])
            .child(html!("div", {
                .child(html!("div", { .class("table__primary").text(&strategy.name()) }))
                .child(html!("div", {
                    .class(["faint", "small"])
                    .text(&format!("by {}", strategy.author))
                }))
            }))
            .child(review_badge(strategy))
        }))
        .child(html!("div", {
            .class(["row", "row--between"])
            .child(ui::signed(
                performance.normalised_annualised,
                &fmt::annualised(performance.normalised_annualised),
            ))
            .child(spark(strategy))
        }))
        .child(html!("div", {
            .class(["row", "row--between", "small", "muted"])
            .child(html!("span", { .text(&fmt::money_compact(strategy.capital())) }))
            .child(html!("span", { .text(&fmt::days(strategy.days_live)) }))
        }))
    })
}

/// The summary figures of a record, as a row of statistics.
pub fn record_stats(strategy: &Strategy) -> Dom {
    let performance = data::performance(strategy);
    let trades = strategy.record.as_ref().map_or(0, |record| record.trades);

    ui::stats(vec![
        ui::stat_toned(
            "Normalised return",
            &fmt::annualised(performance.normalised_annualised),
            Some(&format!(
                "rescaled to {} volatility",
                copy::volatility_target()
            )),
            tone_of(performance.normalised_annualised),
        ),
        ui::stat_toned(
            "Raw return",
            &fmt::annualised(performance.raw_annualised),
            Some("as the vault actually traded"),
            tone_of(performance.raw_annualised),
        ),
        ui::stat_toned(
            "Max drawdown",
            &fmt::percent_signed(performance.max_drawdown, 1),
            Some("deepest fall from a peak"),
            Tone::Neutral,
        ),
        ui::stat(
            "Live",
            &fmt::days(strategy.days_live),
            Some(&format!("{} trades", fmt::count(trades))),
        ),
    ])
}

/// Green above zero, red below, plain at zero.
pub fn tone_of(value: f64) -> Tone {
    if value > 0.0 {
        Tone::Ok
    } else if value < 0.0 {
        Tone::Error
    } else {
        Tone::Neutral
    }
}

/// The colours a venue's share of a vault is drawn in, in order.
pub fn venue_colour(index: usize) -> &'static str {
    const COLOURS: [&str; 4] = [
        "var(--accent)",
        "var(--info)",
        "var(--warn)",
        "var(--text-faint)",
    ];
    COLOURS[index % COLOURS.len()]
}

/// Where a vault's value sits, as one bar with a legend under it.
pub fn venue_split(legs: &[Leg]) -> Dom {
    let parts: Vec<(f64, &'static str, String)> = legs
        .iter()
        .enumerate()
        .map(|(index, leg)| {
            (
                leg.share_of_value,
                venue_colour(index),
                format!("{}: {}", leg.venue, fmt::percent(leg.share_of_value, 0)),
            )
        })
        .collect();

    let idle: f64 = 1.0 - legs.iter().map(|leg| leg.share_of_value).sum::<f64>();
    let mut parts = parts;
    if idle > 0.0 {
        parts.push((
            idle,
            "var(--ink-4)",
            format!("idle in the contract: {}", fmt::percent(idle, 0)),
        ));
    }

    let keys: Vec<Dom> = legs
        .iter()
        .enumerate()
        .map(|(index, leg)| {
            html!("span", {
                .class("legend__key")
                .child(html!("span", {
                    .class("legend__swatch")
                    .style("background", venue_colour(index))
                }))
                .child(text(&format!(
                    "{} · {}",
                    leg.venue,
                    fmt::percent(leg.share_of_value, 0)
                )))
            })
        })
        .chain(std::iter::once(html!("span", {
            .class("legend__key")
            .child(html!("span", {
                .class("legend__swatch")
                .style("background", "var(--ink-4)")
            }))
            .child(text(&format!("idle · {}", fmt::percent(idle.max(0.0), 0))))
        })))
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(ui::split_bar(parts))
        .child(html!("div", { .class("legend").children(keys) }))
    })
}

/// A banner for anything that has gone wrong with a vault, or nothing.
pub fn vault_notices(strategy: &Strategy) -> Vec<Dom> {
    let mut notices = Vec::new();

    if strategy.standing == Standing::BuildFailed {
        // Every tab of a failed build would otherwise be an empty state with
        // no explanation of why it is empty.
        let build = data::build_of(&strategy.slug);
        let why = build
            .as_ref()
            .and_then(|job| match &job.state {
                JobState::Failed { reason } => Some(reason.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "The build did not finish".to_string());

        notices.push(ui::notice_with(
            Tone::Error,
            "This build failed, so there is no bundle and no vault",
            &format!(
                "{why}, so nothing was published. There is no record, no review and no running \
                 code, only the public log."
            ),
            build.map(|job| ui::action_link(&format!("/queue/{}", job.id), "The build log")),
        ));
    }

    if let Some(kill) = &strategy.kill {
        notices.push(ui::notice(
            Tone::Error,
            "This vault was stopped",
            &format!("{} {}", kill.reason, kill.outcome),
        ));
    }

    if let Some(successor) = &strategy.superseded_by {
        notices.push(ui::notice_with(
            Tone::Info,
            "A newer version of this strategy exists",
            "This vault runs exactly as it always has. Moving means withdrawing here and \
             depositing there.",
            Some(ui::action_link(
                &format!("/strategies/{successor}"),
                "See what changed",
            )),
        ));
    }

    {
        let vault = &strategy.vault;
        for claim in &vault.claims {
            let age = (data::as_of() - claim.booked).whole_days();
            let left = i64::from(vault.freeze_after_days) - age;
            notices.push(ui::notice(
                Tone::Warn,
                "A withdrawal claim is waiting",
                &format!(
                    "{} of the vault was closed for it {} ago, and the close hasn't filled inside the \
                     exit band yet. If it goes unserved for {} more day{}, the vault freezes.",
                    fmt::percent(claim.fraction_closed, 1),
                    fmt::days(age.max(0) as u32),
                    left.max(0),
                    if left == 1 { "" } else { "s" }
                ),
            ));
        }
    }

    if let Some(health) = &strategy.health
        && health.level() == HealthLevel::Error
    {
        notices.push(ui::notice_with(
            Tone::Error,
            "This strategy's health report shows an error",
            &health.summary(),
            Some(ui::action_link(
                &format!("/strategies/{}/health", strategy.slug),
                "The health report",
            )),
        ));
    }

    notices
}
