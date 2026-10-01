//! The public queue, and one job's page.
//!
//! A job shows where it is and how it ended, never a build log: a compiler's
//! output can quote file names and source, which would leak sealed code.

use dominator::{Dom, html};
use shared::{Job, JobState};

use crate::data;
use crate::format as fmt;
use crate::pages::publish;
use crate::pages::strategy::review;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

fn tone(state: &JobState) -> Tone {
    match state {
        JobState::AwaitingPayment | JobState::Queued { .. } => Tone::Neutral,
        JobState::Reviewing | JobState::Building => Tone::Info,
        JobState::Ready | JobState::Activated => Tone::Ok,
        JobState::Refused | JobState::Failed { .. } => Tone::Error,
    }
}

pub fn list() -> Dom {
    let rows: Vec<Row> = data::jobs()
        .iter()
        .map(|job| {
            Row::linked(
                format!("/queue/{}", job.id),
                vec![
                    html!("span", { .class(["table__primary", "num"]).text(&job.id) }),
                    html!("div", {
                        .child(html!("div", { .text(&job.title) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!("by {} · {}", job.author, job.chain.label()))
                        }))
                    }),
                    ui::badge(job.state.label(), tone(&job.state)),
                    ui::muted(&detail_of(&job.state)),
                    ui::muted(&fmt::ago(job.submitted, data::now())),
                    if job.duration_seconds > 0 {
                        ui::number(&fmt::duration(job.duration_seconds))
                    } else {
                        ui::muted("—")
                    },
                ],
            )
        })
        .collect();

    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text("Publishing queue") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "First come, first served, one job in flight per author. Every job shows how it \
                     ended, refused and failed ones too, and never a build log."
                )
            }))
        }))
        .child(if rows.is_empty() {
            ui::empty("Nothing has been submitted yet.")
        } else {
            table(
                vec![
                    Column::new("Job"),
                    Column::new("Agent").wide(),
                    Column::new("State"),
                    Column::new("Detail").wide(),
                    Column::new("Submitted"),
                    Column::num("Took"),
                ],
                rows,
            )
        })
        .child(ui::section(
            "What makes a build trustworthy",
            vec![html!("div", {
                .class(["grid", "grid--2"])
                .child(ui::card(vec![
                    ui::subheading("Read before it is built"),
                    ui::para(
                        "The attested reviewer reads the source before anything compiles it, so no \
                         build script runs unread. Code written to do harm is refused and never built.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("One pinned recipe"),
                    ui::para(
                        "Every job is built the same way: the same compiler, the same tooling, the same \
                         SDK, and the dependency graph the author locked.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("An attested builder signs it"),
                    ui::para(
                        "The build runs on an allowlisted image, which signs the mapping from source to \
                         bundle. The registry accepts nothing without that and the reviewer's go-ahead.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("Anyone can rebuild"),
                    ui::para(
                        "The recipe and the builder image are public, so anyone can rebuild a published \
                         agent from its source and compare hashes, and an author can see why a build \
                         failed by running the same recipe.",
                    ),
                ]))
            })],
        ))
    })
}

fn detail_of(state: &JobState) -> String {
    match state {
        JobState::AwaitingPayment => "waiting for the $1 from the uploading wallet".to_string(),
        JobState::Queued { position } => format!("position {position}"),
        JobState::Reviewing => "the reviewer is reading it".to_string(),
        JobState::Building => "compiling".to_string(),
        JobState::Ready => "reviewed, built and registered".to_string(),
        JobState::Activated => "its vault is open".to_string(),
        JobState::Refused => "refused as malicious".to_string(),
        JobState::Failed { outcome } => outcome.label().to_string(),
    }
}

pub fn detail(id: &str) -> Dom {
    let Some(job) = data::job(id) else {
        return crate::pages::not_found::render();
    };

    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class(["row", "row--wrap", "row--between", "row--top"])
            .child(html!("div", {
                .child(html!("h1", { .class("title").text(&job.title) }))
                .child(html!("p", {
                    .class("muted")
                    .text(&format!("Job {} · by {} · {}", job.id, job.author, job.chain.label()))
                }))
            }))
            .child(ui::badge(job.state.label(), tone(&job.state)))
        }))
        .child(outcome(&job))
        .apply_if(job.state == JobState::Ready, |dom| dom.child(publish::activate_panel(&job)))
        .child(facts(&job))
        .apply_if(job.review.is_some(), |dom| {
            dom.child(review::report(job.review.as_ref().expect("checked")))
        })
    })
}

fn facts(job: &Job) -> Dom {
    let mut rows = vec![
        (
            "Submitted".to_string(),
            ui::value(&format!(
                "{} · {}",
                fmt::ago(job.submitted, data::now()),
                fmt::timestamp(job.submitted)
            )),
        ),
        ("Source hash".to_string(), ui::hash(&job.source_hash)),
        (
            "Source".to_string(),
            ui::value(if job.sealed { "sealed" } else { "published" }),
        ),
    ];
    if let Some(id) = job.strategy_id {
        rows.push(("Agent id".to_string(), ui::number(&id.to_string())));
    }
    if let Some(bundle) = &job.bundle_hash {
        rows.push(("Bundle hash".to_string(), ui::hash(bundle)));
    }
    if job.duration_seconds > 0 {
        rows.push((
            "Took".to_string(),
            ui::number(&fmt::duration(job.duration_seconds)),
        ));
    }
    if let Some(slug) = &job.strategy_slug {
        rows.push((
            "Agent".to_string(),
            ui::inline_link(&format!("/agents/{slug}"), slug),
        ));
    }
    ui::section("The job", vec![ui::kv(rows)])
}

fn outcome(job: &Job) -> Dom {
    match &job.state {
        JobState::AwaitingPayment => ui::notice(
            Tone::Info,
            "Waiting for the $1",
            "The upload is in. The job starts when the wallet that uploaded it pays the fee to \
             publish; a payment from any other wallet starts nothing.",
        ),
        JobState::Queued { position } => ui::notice(
            Tone::Info,
            "Waiting its turn",
            &format!(
                "Position {position} in the queue: first come, first served, one job in flight per \
                 author."
            ),
        ),
        JobState::Reviewing => ui::notice(
            Tone::Info,
            "The reviewer is reading it",
            "Nothing is built until the reviewer has read the source.",
        ),
        JobState::Building => ui::notice(
            Tone::Info,
            "Building",
            "The reviewer let it through, and the builder is compiling it with the pinned recipe.",
        ),
        JobState::Ready => ui::notice(
            Tone::Ok,
            "Ready to activate",
            "It is reviewed, built and registered. Nothing runs and nothing is listed until its \
             author activates it, whenever they like.",
        ),
        JobState::Activated => ui::notice(Tone::Ok, "Activated", "Its vault is open for deposits."),
        JobState::Refused => ui::notice(
            Tone::Error,
            "Refused as malicious",
            "The reviewer found code written to do harm, so nothing was built. The $1 is spent, \
             and the report below is public.",
        ),
        JobState::Failed { outcome } => ui::notice(
            Tone::Error,
            "This job failed",
            &format!(
                "{}. The $1 is spent, and nothing was created. To see why a build failed, run the \
                 same recipe yourself; a fixed resubmission is a new agent.",
                capitalise(outcome.label())
            ),
        ),
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
