//! The review tab: the report card, every finding behind it, what produced it,
//! and what it cannot tell you. The job page shows the same report for an
//! agent that hasn't been activated yet, or was refused.

use std::rc::Rc;

use dominator::{Dom, html, text};
use shared::{Area, Rating, Review, Severity, Strategy};

use crate::copy;
use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;

pub fn render(strategy: &Rc<Strategy>) -> Dom {
    match &strategy.review {
        Some(review) => report(review),
        None => ui::section(
            "Review pending",
            vec![ui::empty(
                "The reviewer hasn't posted its report card yet. It appears here when it is signed.",
            )],
        ),
    }
}

/// A review, in full.
pub fn report(review: &Review) -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(card(review))
        .child(findings(review))
        .child(provenance(review))
        .child(limits(review))
    })
}

fn card(review: &Review) -> Dom {
    let rows: Vec<Dom> = Area::ALL
        .into_iter()
        .map(|area| {
            let count = review.findings_in(area).count();
            html!("div", {
                .class("stack-sm")
                .child(html!("div", {
                    .class(["row", "row--between", "row--wrap"])
                    .child(html!("span", { .class("table__primary").text(area.label()) }))
                    .child(ui::rating(review.rating(area)))
                }))
                .child(html!("div", {
                    .class(["faint", "small"])
                    .text(&format!(
                        "{} {}",
                        area.description(),
                        match count {
                            0 => String::new(),
                            1 => "One finding.".to_string(),
                            n => format!("{n} findings."),
                        }
                    ))
                }))
            })
        })
        .collect();

    let mut children = Vec::new();
    if review.refused {
        children.push(ui::notice(
            Tone::Error,
            "Refused as malicious",
            "The reviewer found code written to do harm, so it was never built and has no vault. \
             This report is public all the same.",
        ));
    } else if review.ratings.contains(&Rating::SeriousConcerns) {
        children.push(ui::notice(
            Tone::Error,
            "Serious concerns",
            "At least one area reads as a serious concern. The vault can still open, and whether \
             to back it is each depositor's decision; read the findings first.",
        ));
    }
    if review.sealed {
        children.push(ui::notice(
            Tone::Info,
            "The source is sealed",
            "The reviewer read every line of it. The report names no file or line, and the \
             reviewer is told not to quote the code.",
        ));
    }
    children.extend([
        html!("div", {
            .class(["row", "row--wrap"])
            .child(ui::muted(&format!("reviewed {}", fmt::date(review.reviewed))))
        }),
        html!("div", { .class(["grid", "grid--2"]).children(rows) }),
        ui::subheading("What the reviewer made of the code"),
        html!("p", { .text(&review.summary) }),
        ui::note(copy::REVIEW_NOTE),
        ui::note(
            "Each rating is computed from a battery of narrow questions with published \
             thresholds, and there is no overall score.",
        ),
    ]);
    ui::section("The report card", children)
}

fn findings(review: &Review) -> Dom {
    if review.findings.is_empty() {
        return ui::section(
            "Findings",
            vec![ui::notice(
                Tone::Ok,
                "No findings",
                "The reviewer found nothing worth reporting. That is not a promise it will make \
                 money.",
            )],
        );
    }

    let cards: Vec<Dom> = review
        .findings
        .iter()
        .map(|finding| {
            let place = match (&finding.file, finding.line) {
                (Some(file), Some(line)) => format!("{file}:{line}"),
                (Some(file), None) => file.clone(),
                _ => String::new(),
            };
            ui::card(vec![
                html!("div", {
                    .class(["row", "row--between", "row--wrap", "row--top"])
                    .child(html!("h3", { .class("subtitle").text(&finding.summary) }))
                    .child(ui::badge(
                        finding.severity.label(),
                        match finding.severity {
                            Severity::Serious => Tone::Error,
                            Severity::Concern => Tone::Warn,
                            Severity::Note => Tone::Neutral,
                        },
                    ))
                }),
                html!("div", {
                    .class(["row", "row--wrap", "small"])
                    .child(ui::badge(finding.area.label(), Tone::Info))
                    .apply_if(!place.is_empty(), |dom| dom.child(html!("span", {
                        .class("num")
                        .text(&place)
                    })))
                }),
                html!("p", { .text(&finding.detail) }),
            ])
        })
        .collect();

    ui::section(
        "Findings",
        vec![html!("div", { .class("stack").children(cards) })],
    )
}

fn provenance(review: &Review) -> Dom {
    ui::section(
        "What produced this report",
        vec![
            ui::kv(vec![
                (
                    "Reviewer image".to_string(),
                    ui::hash(&review.reviewer_image),
                ),
                ("Model".to_string(), ui::hash(&review.model_id_hash)),
                (
                    "Prompt, schema and battery".to_string(),
                    ui::hash(&review.prompt_hash),
                ),
                ("Full report".to_string(), ui::hash(&review.report_hash)),
                (
                    "Reviewed".to_string(),
                    ui::value(&fmt::date(review.reviewed)),
                ),
            ]),
            ui::note(
                "The image pins the model, the prompt, the schema and the battery; changing any of \
                 them is an image change behind the allowlist timelock. Each agent is reviewed \
                 once, before it is built.",
            ),
        ],
    )
}

fn limits(review: &Review) -> Dom {
    ui::section(
        "What a review can't tell you",
        vec![ui::bullets(review.limits.iter().cloned())],
    )
}

/// A short line naming the reviewer, for anywhere a review is shown small.
pub fn attribution(review: &Review) -> Dom {
    html!("span", {
        .class(["faint", "small"])
        .child(text(&format!(
            "reviewed by image {}",
            fmt::short_hash(&review.reviewer_image)
        )))
    })
}
