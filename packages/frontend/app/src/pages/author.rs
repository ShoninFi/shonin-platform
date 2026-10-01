//! An author's page: everything they have ever published.

use dominator::{Dom, html};
use shared::JobState;

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render(handle: &str) -> Dom {
    let Some(author) = data::author(handle) else {
        return crate::pages::not_found::render();
    };
    let published = data::by_author(handle);

    let live = published
        .iter()
        .filter(|strategy| strategy.standing.is_live())
        .count();
    let capital: f64 = published
        .iter()
        .filter(|strategy| strategy.standing.is_live())
        .map(|strategy| strategy.capital())
        .sum();
    let alarming = published
        .iter()
        .filter(|strategy| {
            strategy
                .review
                .as_ref()
                .is_some_and(|review| review.is_alarming())
        })
        .count();
    // Everything submitted that never got a vault: refused, failed, or ready
    // and not yet activated. It is part of the record too.
    let unopened: Vec<Row> = data::jobs()
        .iter()
        .filter(|job| job.author.eq_ignore_ascii_case(handle))
        .filter(|job| {
            matches!(
                job.state,
                JobState::Ready | JobState::Refused | JobState::Failed { .. }
            )
        })
        .map(|job| {
            Row::linked(
                format!("/queue/{}", job.id),
                vec![
                    html!("span", { .class("table__primary").text(&job.title) }),
                    ui::badge(
                        job.state.label(),
                        match job.state {
                            JobState::Ready => Tone::Ok,
                            _ => Tone::Error,
                        },
                    ),
                    ui::muted(&fmt::date(job.submitted.date())),
                ],
            )
        })
        .collect();

    let rows: Vec<Row> = published
        .iter()
        .map(|strategy| {
            let performance = data::performance(strategy);
            Row::linked(
                parts::path_of(strategy),
                vec![
                    html!("div", {
                        .child(html!("div", { .class("table__primary").text(strategy.name()) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&strategy.title)
                        }))
                    }),
                    parts::standing_badge(strategy.standing),
                    ui::muted(&fmt::date(strategy.registered)),
                    ui::number(&fmt::days(strategy.days_live)),
                    ui::signed(
                        performance.return_since_open,
                        &fmt::percent_signed(performance.return_since_open, 1),
                    ),
                    parts::review_badge(strategy),
                    ui::number(&fmt::money_compact(strategy.capital())),
                    parts::spark(strategy),
                ],
            )
        })
        .collect();

    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text(handle) }))
            .child(html!("p", { .class("lede").text(&author.blurb) }))
            .child(html!("p", {
                .class(["faint", "small"])
                .text(&format!("Publishing since {}.", fmt::date(author.joined)))
            }))
        }))
        .child(ui::stats(vec![
            ui::stat("Published", &published.len().to_string(), Some("all of it below")),
            ui::stat("Live", &live.to_string(), Some("taking deposits")),
            ui::stat(
                "Capital behind them",
                &fmt::money_compact(capital),
                Some("across live agents"),
            ),
            ui::stat(
                "Serious concerns",
                &alarming.to_string(),
                Some("agents whose review flags one"),
            ),
        ]))
        .child(ui::section(
            "Everything they have published",
            vec![
                table(
                    vec![
                        Column::new("Agent").wide(),
                        Column::new("Standing"),
                        Column::new("Registered"),
                        Column::num("Live"),
                        Column::num("Since opening"),
                        Column::num("Review"),
                        Column::num("Capital"),
                        Column::new("Shape"),
                    ],
                    rows,
                ),
                ui::notice(
                    Tone::Info,
                    "This is the whole list",
                    "Live and stopped, and below, everything submitted that never opened a vault. \
                     Nothing here can be hidden by its author.",
                ),
                ui::note(&format!(
                    "An author can publish under a fresh identity, but every agent needs {} \
                     live days before it is on the leaderboard.",
                    copy::days_new()
                )),
            ],
        ))
        .apply_if(!unopened.is_empty(), |dom| dom.child(ui::section(
            "Submitted, and never opened",
            vec![table(
                vec![Column::new("Agent").wide(), Column::new("How it ended"), Column::new("Submitted")],
                unopened,
            )],
        )))
    })
}
