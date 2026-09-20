//! The public build queue, and one build's page.

use dominator::{Dom, html};
use shared::{Job, JobState, LogLevel};

use crate::data;
use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

fn tone(state: &JobState) -> Tone {
    match state {
        JobState::Queued { .. } => Tone::Neutral,
        JobState::Building { .. } => Tone::Info,
        JobState::Succeeded => Tone::Ok,
        JobState::Failed { .. } => Tone::Error,
    }
}

pub fn list() -> Dom {
    let rows: Vec<Row> = data::get()
        .jobs
        .iter()
        .map(|job| {
            Row::linked(
                format!("/queue/{}", job.id),
                vec![
                    html!("span", { .class(["table__primary", "num"]).text(&job.id) }),
                    html!("div", {
                        .child(html!("div", { .text(&job.subject) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!("{} by {}", job.kind, job.author))
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
            .child(html!("h1", { .class("title").text("Build queue") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "First come, first served, one job in flight per author. Every job shows its \
                     state and its full log, whether it succeeded or failed."
                )
            }))
        }))
        .child(table(
            vec![
                Column::new("Build"),
                Column::new("Subject").wide(),
                Column::new("State"),
                Column::new("Detail").wide(),
                Column::new("Submitted"),
                Column::num("Took"),
            ],
            rows,
        ))
        .child(ui::section(
            "What makes a build trustworthy",
            vec![html!("div", {
                .class(["grid", "grid--2"])
                .child(ui::card(vec![
                    ui::subheading("A pinned, offline recipe"),
                    ui::para(
                        "Builds run in a published image that fixes the compiler, the tooling, \
                         the target and the flags. Dependencies come from a pinned snapshot with \
                         a lockfile, and the sandbox has no network, so a build script can't \
                         fetch anything.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("Built twice, bytes must match"),
                    ui::para(
                        "Every job compiles the project twice and compares. A project whose \
                         output isn't stable is rejected with the difference rather than \
                         published as something nobody can reproduce.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("An attested builder signs it"),
                    ui::para(
                        "The build the registry accepts runs in an enclave on an image sitting \
                         on the same allowlist and timelock as executor images. It signs the \
                         mapping from source to bundle, and the registry accepts nothing else.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("Anyone can rebuild"),
                    ui::para(
                        "The recipe and the builder image are public, so a third party can \
                         rebuild any published strategy from its source and compare hashes. A \
                         disagreement is a public, checkable claim rather than an accusation.",
                    ),
                ]))
            })],
        ))
    })
}

fn detail_of(state: &JobState) -> String {
    match state {
        JobState::Queued { position } => format!("position {position}"),
        JobState::Building { step, percent } => format!("{step} · {percent}%"),
        JobState::Succeeded => "signed and registered".to_string(),
        JobState::Failed { reason } => reason.clone(),
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
                .child(html!("h1", { .class("title").text(&format!("Build {}", job.id)) }))
                .child(html!("p", {
                    .class("muted")
                    .text(&format!("{} · {} by {}", job.subject, job.kind, job.author))
                }))
            }))
            .child(ui::badge(job.state.label(), tone(&job.state)))
        }))
        .child(facts(job))
        .child(outcome(job))
        .child(log(job))
    })
}

fn facts(job: &'static Job) -> Dom {
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
    ];
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
            "Strategy".to_string(),
            ui::inline_link(&format!("/strategies/{slug}"), slug),
        ));
    }

    ui::section("The job", vec![ui::kv(rows)])
}

fn outcome(job: &'static Job) -> Dom {
    match &job.state {
        JobState::Failed { reason } => ui::notice(
            Tone::Error,
            "This build failed",
            &format!(
                "{reason}. The build is charged, because it ran: the vault closes and the author \
                 reclaims the rest of its tank. A fixed resubmission is a new vault — an author \
                 can't quietly try twenty times and show you the twentieth."
            ),
        ),
        JobState::Succeeded => ui::notice(
            Tone::Ok,
            "Signed and registered",
            "The attested builder signed the mapping from this source to this bundle, and the \
             registry accepted the signature. A vault's executors are blessed for that bundle \
             hash, so the chain from published source to running code is unbroken.",
        ),
        JobState::Building { step, percent } => ui::notice(
            Tone::Info,
            "Building",
            &format!("{step}, {percent}% through."),
        ),
        JobState::Queued { position } => ui::notice(
            Tone::Info,
            "Waiting for a builder",
            &format!(
                "Position {position} in the queue. Jobs are taken first come, first served, and \
                 an author can have one in flight at a time, so a busy author can't crowd out \
                 everyone else."
            ),
        ),
    }
}

fn log(job: &'static Job) -> Dom {
    if job.log.is_empty() {
        return ui::section(
            "The log",
            vec![ui::empty(
                "Nothing yet: this job is still waiting for a builder.",
            )],
        );
    }

    let lines: Vec<Dom> = job
        .log
        .iter()
        .enumerate()
        .map(|(index, line)| {
            html!("div", {
                .class("log__line")
                .apply(|dom| match line.level {
                    LogLevel::Error => dom.class("log__line--err"),
                    LogLevel::Warn => dom.class("log__line--warn"),
                    LogLevel::Plain => dom,
                })
                .child(html!("span", { .class("log__num").text(&(index + 1).to_string()) }))
                .child(html!("span", { .text(&line.text) }))
            })
        })
        .collect();

    ui::section(
        "The log",
        vec![
            html!("div", { .class("log").children(lines) }),
            ui::note("A failed build's log is as public as a successful one's."),
        ],
    )
}
