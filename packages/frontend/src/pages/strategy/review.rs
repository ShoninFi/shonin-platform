//! The review tab: the score, every finding behind it, what produced it, and
//! what it cannot tell you.

use std::sync::OnceLock;

use dominator::{Dom, html, text};
use futures_signals::signal::Mutable;
use shared::{FindingCategory, Review, Severity, Strategy};

use crate::copy;
use crate::format as fmt;
use crate::state;
use crate::ui::overlay::{modal, modal_actions};
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;

fn buying() -> &'static Mutable<bool> {
    static BUYING: OnceLock<Mutable<bool>> = OnceLock::new();
    BUYING.get_or_init(|| Mutable::new(false))
}

pub fn render(strategy: &'static Strategy) -> Dom {
    let Some(review) = &strategy.review else {
        return ui::section(
            "Review pending",
            vec![ui::empty(
                "The reviewer hasn't scored this strategy yet. A vault opens for deposits \
                 without waiting for it, and the score appears here when it is signed.",
            )],
        );
    };

    html!("div", {
        .class("stack-lg")
        .child(score(strategy, review))
        .child(findings(review))
        .child(categories(review))
        .child(provenance(review))
        .child(limits(review))
        .child(buy_modal(strategy))
    })
}

fn score(strategy: &'static Strategy, review: &'static Review) -> Dom {
    let serious = review
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Serious)
        .count();

    ui::section_with_aside(
        "The score",
        Some(shell::button("Buy a fresh review", false, || {
            buying().set(true);
        })),
        vec![
            html!("div", {
                .class(["row", "row--wrap"])
                .child(ui::score(review.score, review.band()))
                .child(ui::badge(
                    &format!(
                        "{} finding{}",
                        review.findings.len(),
                        if review.findings.len() == 1 { "" } else { "s" }
                    ),
                    Tone::Neutral,
                ))
                .apply_if(serious > 0, |dom| dom.child(ui::badge(
                    &format!("{serious} serious"),
                    Tone::Error,
                )))
                .child(ui::muted(&format!(
                    "reviewed {}",
                    fmt::date(review.reviewed)
                )))
            }),
            ui::subheading("What the reviewer made of the code"),
            html!("p", { .text(&review.summary) }),
            ui::notice(
                Tone::Info,
                "A score, not a gate",
                &format!(
                    "It didn't stop this vault opening and it protects nobody. {} It is the \
                     author's code that decides what this strategy does, and the chain that \
                     says how that worked out.",
                    copy::REVIEW_NOTE
                ),
            ),
            ui::note(&format!(
                "Read against the code alone, never against {}'s description of it.",
                strategy.author
            )),
        ],
    )
}

fn findings(review: &'static Review) -> Dom {
    if review.findings.is_empty() {
        return ui::section(
            "Findings",
            vec![ui::notice(
                Tone::Ok,
                "No findings",
                "The reviewer found nothing in the fixed categories worth reporting. That means \
                 it read the code and saw nothing it recognised as a problem — not that the \
                 strategy will make money.",
            )],
        );
    }

    let cards: Vec<Dom> = review
        .findings
        .iter()
        .map(|finding| {
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
                    .child(ui::badge(finding.category.label(), Tone::Info))
                    .child(html!("span", {
                        .class("num")
                        .text(&format!("{}:{}", finding.file, finding.line))
                    }))
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

fn categories(review: &'static Review) -> Dom {
    let rows: Vec<Dom> = FindingCategory::all()
        .into_iter()
        .map(|category| {
            let count = review.count(category);
            html!("div", {
                .class("stack-sm")
                .child(html!("div", {
                    .class(["row", "row--between", "row--wrap"])
                    .child(html!("span", { .text(category.label()) }))
                    .child(ui::badge(
                        &count.to_string(),
                        if count == 0 { Tone::Ok } else { Tone::Warn },
                    ))
                }))
                .child(html!("div", {
                    .class(["faint", "small"])
                    .text(category.description())
                }))
            })
        })
        .collect();

    ui::section(
        "What the reviewer looks for",
        vec![
            ui::note(
                "The categories are pinned by the reviewer image, so a score means the same \
                 thing across strategies, and a finding can only be one of these.",
            ),
            html!("div", { .class(["grid", "grid--2"]).children(rows) }),
        ],
    )
}

fn provenance(review: &'static Review) -> Dom {
    ui::section(
        "What produced this score",
        vec![
            ui::kv(vec![
                (
                    "Reviewer image".to_string(),
                    ui::hash(&review.reviewer_image),
                ),
                ("Model".to_string(), ui::hash(&review.model_id_hash)),
                (
                    "Prompt and output schema".to_string(),
                    ui::hash(&review.prompt_hash),
                ),
                ("Full report".to_string(), ui::hash(&review.report_hash)),
                (
                    "Reviewed".to_string(),
                    ui::value(&fmt::date(review.reviewed)),
                ),
                ("Paid for by".to_string(), ui::muted(&review.bought_by)),
            ]),
            ui::note(
                "The image pins the model, the prompt, the output schema and the finding \
                 categories, and changing any of them is an image change behind the allowlist \
                 timelock. The reviewer signs the result with a blessed key, so the score really \
                 is the model's answer — and because the prompt and schema are published, an \
                 author can run the same review themselves before publishing.",
            ),
        ],
    )
}

fn limits(review: &'static Review) -> Dom {
    ui::section(
        "What a review can't tell you",
        vec![ui::bullets(review.limits.iter().cloned())],
    )
}

fn buy_modal(strategy: &'static Strategy) -> Dom {
    modal(buying(), "Buy a fresh review", false, move || {
        vec![
            ui::para(
                "Anyone can buy a new review of any strategy under the current reviewer image, \
                 for the same flat fee the strategy's own tank paid. The newest is shown first \
                 and the older ones are kept, so a score can be re-checked but never quietly \
                 replaced.",
            ),
            ui::kv(vec![
                ("Strategy".to_string(), ui::value(&strategy.name())),
                (
                    "Reviewer image".to_string(),
                    ui::hash(
                        &strategy
                            .review
                            .as_ref()
                            .map_or_else(String::new, |review| review.reviewer_image.clone()),
                    ),
                ),
                ("Fee".to_string(), ui::number("$1.00")),
            ]),
            ui::note(
                "Nothing is re-reviewed automatically when the reviewer image changes, because \
                 a score that moved on its own would say more about the reviewer than about the \
                 strategy.",
            ),
            modal_actions(vec![
                shell::button("Cancel", false, || buying().set(false)),
                shell::button("Buy the review", true, move || {
                    buying().set(false);
                    state::notify(
                        "Queued. A fresh review appears here when the reviewer has signed it.",
                        Tone::Ok,
                    );
                }),
            ]),
        ]
    })
}

/// A short line naming the reviewer, for anywhere a score is shown small.
pub fn attribution(review: &Review) -> Dom {
    html!("span", {
        .class(["faint", "small"])
        .child(text(&format!(
            "scored by reviewer image {}",
            fmt::short_hash(&review.reviewer_image)
        )))
    })
}
