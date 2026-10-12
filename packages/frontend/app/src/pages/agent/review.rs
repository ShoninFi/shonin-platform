//! `/agents/:id/review`: the whole AI review. The report card first (six
//! areas, one rating each), then the Reviewer's summary (a few lines, with
//! the rest a button away), area by area with every finding, the author's
//! claims, what the review couldn't see, and what a review is and isn't;
//! beside it, where the report came from.

use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use shared::{Area, Rating, Review, Severity, Source, Strategy};
use ui::icons::Icon;

use super::parts::{self, card, more_in_app};
use super::{facts, live, side};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::on_click;
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::rating::{rating, rating_icon};
use crate::format;
use crate::route::DOCS;
use crate::store::Agent;
use crate::view::{El, View, el, text};

pub fn body(agent: &Rc<Agent>) -> El {
    el("div")
        .class("ag-layout")
        .class("ag-layout--tab")
        .child(el("div").class("ag-main").child(live(agent, main)))
        .child(
            el("aside")
                .class("ag-side")
                .attr("aria-label", "About this report, deposit and withdraw")
                .child(live(agent, about))
                .child(side::mini_exit(agent)),
        )
}

fn main(strategy: &Strategy) -> View {
    let Some(review) = &strategy.review else {
        return card("ag-found-title", "No review yet", Some(Icon::Review))
            .child(el("p").class("ag-body").text(
                "The Reviewer's report isn't on the chain for this agent yet. Every agent is reviewed before it is built.",
            ))
            .into();
    };
    View::Many(vec![
        report_card(strategy, review).into(),
        found(strategy, review).into(),
        areas(strategy, review).into(),
        claims(strategy, review),
        limits(review),
        is_and_isnt().into(),
    ])
}

fn sealed(strategy: &Strategy, review: &Review) -> bool {
    review.sealed || matches!(strategy.bundle.source, Source::Sealed)
}

/// The paragraphs of the Reviewer's summary, as it wrote them.
pub fn paragraphs(summary: &str) -> Vec<String> {
    summary
        .split("\n\n")
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The id of the summary's text, which its button controls.
pub const SUMMARY_ID: &str = "ag-summary";

/// The Reviewer's summary, held to about three lines with a button that
/// opens the rest. The whole text is always in the page: the clamp is drawn
/// only where scripts run (CSS `scripting`), and the button shows only when
/// the app has measured that the text is longer than its lines.
fn summary(summary: &str) -> View {
    let expanded = Mutable::new(false);
    let overflows = Mutable::new(false);
    let mut parts = paragraphs(summary);
    if let Some(first) = parts.first_mut() {
        *first = format!("“{first}");
    }
    if let Some(last) = parts.last_mut() {
        last.push('”');
    }
    let text = el("blockquote")
        .class("ag-quote__text")
        .class("ag-summary")
        .attr("id", SUMMARY_ID)
        .class_signal(
            "ag-summary--clamp",
            true,
            expanded.signal().map(|expanded| !expanded),
        )
        .children(parts.into_iter().map(|part| el("p").text(part)))
        .hook({
            let expanded = expanded.clone();
            let overflows = overflows.clone();
            move |dom| {
                dominator::with_node!(dom, element => {
                    .future(super::measure_clamp(
                        element,
                        futures_signals::signal::always(String::new()),
                        expanded,
                        overflows,
                    ))
                })
            }
        });
    let label = |expanded: bool| {
        if expanded {
            "Show less of the summary"
        } else {
            "Read the Reviewer's full summary"
        }
    };
    let toggle = el("button")
        .attr("type", "button")
        .class("ag-more")
        .class("ag-summary__more")
        .attr("aria-controls", SUMMARY_ID)
        .attr_signal(
            "hidden",
            Some(String::new()),
            map_ref! {
                let overflows = overflows.signal(),
                let expanded = expanded.signal() => (!*overflows && !*expanded).then(String::new)
            },
        )
        .attr_signal(
            "aria-expanded",
            Some("false".to_string()),
            expanded.signal().map(|expanded| Some(expanded.to_string())),
        )
        .child(
            el("span").text_signal(
                label(false),
                expanded
                    .signal()
                    .map(move |expanded| label(expanded).to_string()),
            ),
        )
        .child(icon(Icon::ChevronDown, 16));
    let toggle = on_click(toggle, move || {
        expanded.replace_with(|expanded| !*expanded);
    });
    View::Many(vec![text.into(), toggle.into()])
}

fn found(strategy: &Strategy, review: &Review) -> El {
    let sealed = sealed(strategy, review);
    let read = match (sealed, facts::source_size(strategy)) {
        (true, _) => "Written by the Reviewer, which read the sealed source".to_string(),
        (false, Some((files, lines))) => format!(
            "Written by the Reviewer, after reading all {} lines in {} files",
            format::number(lines as f64, 0),
            format::number(files as f64, 0)
        ),
        (false, None) => "Written by the Reviewer, after reading its source".to_string(),
    };
    let accurate = review.rating(Area::AccurateDescription);
    let verdict = match accurate {
        Rating::LooksFine => {
            "The Reviewer judged what the code does first, then checked it against this. They match."
        }
        Rating::SomeConcerns => {
            "The Reviewer judged what the code does first, then checked it against this. Some of it doesn't match."
        }
        Rating::SeriousConcerns => {
            "The Reviewer judged what the code does first, then checked it against this. It doesn't match."
        }
        Rating::CouldntTell => {
            "The Reviewer judged what the code does first, then checked it against this. It couldn't tell whether they match."
        }
    };
    let byline = el("figcaption")
        .class("ag-quote__by")
        .child(icon(Icon::Review, 16))
        .text(read);
    let byline = if sealed {
        byline
    } else {
        byline.child(
            crate::web::app_link(format!("/agents/{}/source", strategy.slug))
                .class("ag-quote__source")
                .text("Read the source yourself"),
        )
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-found")
        .attr("aria-labelledby", "ag-found-title")
        .child(
            el("h2")
                .class("ag-card__title")
                .attr("id", "ag-found-title")
                .text("What the Reviewer found"),
        )
        .child(
            el("figure")
                .class("ag-quote")
                .class("ag-quote--well")
                .child(summary(&review.summary))
                .child(byline),
        )
        .child(View::from(sealed.then(|| {
            el("p")
                .class("ag-sealed-note")
                .child(icon(Icon::Lock, 16))
                .text("A sealed report gives ratings, a summary and findings by area and severity. It never names files, gives line numbers or quotes the code.")
                .into()
        })))
        .child(
            el("div")
                .class("ag-says")
                .child(el("p").class("sh-label").text("The Author says"))
                .child(
                    el("div")
                        .child(
                            el("p")
                                .class("ag-says__text")
                                .text(strategy.description.clone()),
                        )
                        .child(
                            el("p")
                                .class("ag-says__verdict")
                                .child(icon(rating_icon(accurate), 16))
                                .text(verdict),
                        ),
                ),
        )
}

fn area_anchor(area: Area) -> String {
    format!("area-{}", area.index() + 1)
}

fn one_line(review: &Review, area: Area) -> String {
    match facts::lead_finding(review, area) {
        Some(finding) => finding.summary.clone(),
        None => "Nothing to report.".to_string(),
    }
}

/// The six areas and their ratings: the first thing the tab says.
fn report_card(strategy: &Strategy, review: &Review) -> El {
    let sealed = sealed(strategy, review);
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-card--flush")
        .class("ag-report-card")
        .attr("aria-labelledby", "ag-card-title")
        .child(
            el("div")
                .class("ag-report-card__head")
                .child(
                    el("div")
                        .class("ag-found__top")
                        .child(el("p").class("sh-label").text(if sealed {
                            "AI review · sealed report"
                        } else {
                            "AI review · public report"
                        }))
                        .child(
                            el("p")
                                .class("ag-found__when")
                                .child(icon(Icon::Clock, 16))
                                .text(format!(
                                    "Reviewed {}, before the build",
                                    facts::full_day(review.reviewed)
                                )),
                        ),
                )
                .child(
                    el("div")
                        .class("ag-card__head")
                        .child(
                            el("h2")
                                .class("ag-found__title")
                                .attr("id", "ag-card-title")
                                .text("Report card"),
                        )
                        .child(el("p").class("ag-muted").text("Six areas, one rating each")),
                )
                .child(View::from(review.refused.then(|| {
                    notice(
                        NoticeTone::Danger,
                        View::Many(vec![
                            el("strong").text("Refused.").into(),
                            text(
                                " The Reviewer judged this code written to do harm, so it was never built.",
                            ),
                        ]),
                    )
                    .into()
                }))),
        )
        .child(
            super::parts::scroll("The six areas and their ratings").child(
                el("table")
                    .class("sh-table")
                    .class("ag-report")
                    .child(
                        el("caption")
                            .class("sh-vh")
                            .text("The six areas and their ratings"),
                    )
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("Area"))
                                .child(el("th").attr("scope", "col").text("Rating"))
                                .child(el("th").attr("scope", "col").text("In one line"))
                                .child(
                                    el("th")
                                        .attr("scope", "col")
                                        .child(el("span").class("sh-vh").text("Go to area")),
                                ),
                        ),
                    )
                    .child(el("tbody").children(Area::ALL.into_iter().map(|area| {
                        let rated = review.rating(area);
                        el("tr")
                            .class("sh-row")
                            .class_if(rated == Rating::SomeConcerns, "ag-report__row--some")
                            .class_if(rated == Rating::SeriousConcerns, "ag-report__row--serious")
                            .child(
                                el("th")
                                    .attr("scope", "row")
                                    .class("ag-report__area")
                                    .text(area.label()),
                            )
                            .child(el("td").child(rating(rated)))
                            .child(
                                el("td")
                                    .class("ag-report__line")
                                    .text(one_line(review, area)),
                            )
                            .child(
                                el("td").class("sh-r").child(
                                    parts::in_page_link(area_anchor(area))
                                        .class("ag-report__go")
                                        .attr("aria-label", format!("Go to {}", area.label()))
                                        .child(icon(Icon::ChevronRight, 18)),
                                ),
                            )
                    }))),
            ),
        )
        .child(
            el("div")
                .class("ag-legend")
                .child(el("span").class("ag-muted").text("The four ratings:"))
                .children(
                    [
                        Rating::LooksFine,
                        Rating::SomeConcerns,
                        Rating::SeriousConcerns,
                        Rating::CouldntTell,
                    ]
                    .into_iter()
                    .map(rating),
                ),
        )
}

fn severity_tone(severity: Severity) -> Tone {
    match severity {
        Severity::Note => Tone::Neutral,
        Severity::Concern => Tone::Caution,
        Severity::Serious => Tone::Loss,
    }
}

fn areas(strategy: &Strategy, review: &Review) -> El {
    let public = !sealed(strategy, review);
    el("section")
        .class("ag-areas")
        .attr("aria-labelledby", "ag-areas-title")
        .child(
            el("div")
                .class("ag-areas__head")
                .child(
                    el("h2")
                        .class("ag-section-title")
                        .attr("id", "ag-areas-title")
                        .text("Area by area"),
                )
                .child(
                    el("p")
                        .class("ag-muted")
                        .text("What the Reviewer looked for, and what it found"),
                ),
        )
        .children(Area::ALL.into_iter().map(|area| {
            let findings: Vec<&shared::Finding> = {
                let mut list: Vec<&shared::Finding> = review.findings_in(area).collect();
                list.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
                list
            };
            el("section")
                .class("sh-card")
                .class("ag-card")
                .class("ag-area")
                .attr("id", area_anchor(area))
                .attr("aria-labelledby", format!("{}-title", area_anchor(area)))
                .child(
                    el("div")
                        .class("ag-card__head")
                        .child(
                            el("h3")
                                .class("ag-area__title")
                                .attr("id", format!("{}-title", area_anchor(area)))
                                .child(
                                    el("span")
                                        .class("ag-area__n")
                                        .text((area.index() + 1).to_string()),
                                )
                                .text(area.label()),
                        )
                        .child(rating(review.rating(area))),
                )
                .child(
                    el("p")
                        .class("ag-area__looked")
                        .child(el("strong").text("What we looked for: "))
                        .text(area.description()),
                )
                .child(if findings.is_empty() {
                    el("p").class("ag-muted").text("No findings in this area.")
                } else {
                    el("ul")
                        .class("ag-findings")
                        .children(findings.into_iter().map(|finding| {
                            let glyph = match finding.severity {
                                Severity::Note => Icon::Check,
                                Severity::Concern => Icon::Concern,
                                Severity::Serious => Icon::Serious,
                            };
                            el("li")
                                .class("ag-finding")
                                .class(match finding.severity {
                                    Severity::Note => "ag-finding--note",
                                    Severity::Concern => "ag-finding--concern",
                                    Severity::Serious => "ag-finding--serious",
                                })
                                .child(icon(glyph, 18))
                                .child(
                                    el("div")
                                        .class("ag-finding__body")
                                        .child(
                                            el("p")
                                                .class("ag-finding__summary")
                                                .text(finding.summary.clone()),
                                        )
                                        .child(
                                            el("p")
                                                .class("ag-finding__detail")
                                                .text(finding.detail.clone()),
                                        )
                                        .child(View::from(
                                            (public && finding.file.is_some()).then(|| {
                                                let place = match (&finding.file, finding.line) {
                                                    (Some(file), Some(line)) => {
                                                        format!("{file}:{line}")
                                                    }
                                                    (Some(file), None) => file.clone(),
                                                    _ => String::new(),
                                                };
                                                let code = el("code").class("ag-code").text(place);
                                                // A file the source holds opens on the Source tab.
                                                let held = finding.file.as_ref().filter(|file| {
                                                    strategy
                                                        .bundle
                                                        .source_files
                                                        .iter()
                                                        .any(|held| held.path == **file)
                                                });
                                                el("p")
                                                    .class("ag-finding__where")
                                                    .child(match held {
                                                        Some(file) => crate::web::app_link(
                                                            format!(
                                                                "/agents/{}/source#{}",
                                                                strategy.slug,
                                                                super::source::file_id(file)
                                                            ),
                                                        )
                                                        .class("ag-finding__file")
                                                        .attr(
                                                            "aria-label",
                                                            format!("Open {file} in the source"),
                                                        )
                                                        .child(code),
                                                        None => code,
                                                    })
                                                    .into()
                                            }),
                                        )),
                                )
                                .child(badge(
                                    severity_tone(finding.severity),
                                    None,
                                    facts::severity_word(finding.severity),
                                ))
                        }))
                })
        }))
}

fn claims(strategy: &Strategy, review: &Review) -> View {
    if strategy.claims.is_empty() {
        return View::Empty;
    }
    let accurate = review.rating(Area::AccurateDescription);
    card("ag-claims-title", "Claims checked against the code", None)
        .child(el("p").class("ag-muted").text(
            "The Author wrote these limits into shonin.toml. They're fixed once published. The Reviewer checked each one against what the code actually does, and rates them together under Accurate description.",
        ))
        .child(
            el("ul")
                .class("ag-claim-list")
                .children(strategy.claims.iter().map(|claim| el("li").text(format!("“{claim}”")))),
        )
        .child(
            el("p")
                .class("ag-claims__verdict")
                .text("Accurate description: ")
                .child(rating(accurate))
                .child(
                    parts::in_page_link(area_anchor(Area::AccurateDescription))
                        .text("See what it found"),
                ),
        )
        .into()
}

fn limits(review: &Review) -> View {
    if review.limits.is_empty() {
        return View::Empty;
    }
    card("ag-limits-title", "What the review couldn't see", None)
        .child(
            el("p")
                .class("ag-muted")
                .text("Said by the Reviewer, in its own words."),
        )
        .child(
            el("ul").class("ag-bullets").children(
                review
                    .limits
                    .iter()
                    .map(|limit| el("li").text(limit.clone())),
            ),
        )
        .into()
}

fn is_and_isnt() -> El {
    let column = |label: &'static str, glyph: Icon, items: &[(&'static str, &'static str)]| {
        el("div")
            .class("sh-card")
            .class("ag-isnt__col")
            .child(el("p").class("sh-label").text(label))
            .child(el("ul").children(items.iter().map(|(lead, rest)| {
                el("li").child(icon(glyph, 18)).child(
                    el("span")
                        .child(View::from(
                            (!lead.is_empty()).then(|| el("strong").text(*lead).into()),
                        ))
                        .text(*rest),
                )
            })))
    };
    el("section")
        .class("ag-isnt")
        .attr("aria-labelledby", "ag-isnt-title")
        .child(el("h2").class("ag-section-title").attr("id", "ag-isnt-title").text("What the review is, and isn't"))
        .child(
            el("div")
                .class("ag-isnt__cols")
                .child(column(
                    "It is",
                    Icon::Check,
                    &[
                        ("", "A reading of all the code, done before it was built, by a model locked inside an attested machine."),
                        ("", "A check of what the code does against what its Author says."),
                        ("", "Public, and tied on the chain to the exact source it read."),
                    ],
                ))
                .child(column(
                    "It isn't",
                    Icon::Close,
                    &[
                        ("A prediction.", " It never rates how much an agent might make or lose."),
                        ("A guarantee.", " A model can miss things, and “Looks fine” isn't a promise."),
                        ("What keeps your money safe.", " That's the attested machine, the trade-only keys and the smart contracts."),
                    ],
                )),
        )
        .child(notice(
            NoticeTone::Info,
            View::Many(vec![
                text("Code written to do harm is refused and never built. Everything else goes ahead, however it's rated, so read the ratings before you deposit. "),
                el("a").attr("href", format!("{DOCS}review/")).text("How the review works").into(),
            ]),
        ))
}

fn about(strategy: &Strategy) -> View {
    let Some(review) = &strategy.review else {
        return View::Empty;
    };
    let row = |key: &'static str, value: View| {
        el("div")
            .child(el("dt").text(key))
            .child(el("dd").child(value))
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-about")
        .attr("aria-labelledby", "ag-about-title")
        .child(el("h2").class("ag-card__title").class("ag-card__title--s").attr("id", "ag-about-title").text("About this report"))
        .child(
            el("dl")
                .class("ag-kv")
                .child(row("Reviewer image", parts::chip(&review.reviewer_image, "reviewer image").into()))
                .child(row("Model", parts::chip(&review.model_id_hash, "model hash").into()))
                .child(row("Prompt", parts::chip(&review.prompt_hash, "prompt hash").into()))
                .child(row("Reviewed", text(facts::full_day(review.reviewed))))
                .child(row("Source it read", parts::chip(&strategy.bundle.source_hash, "source hash").into()))
                .child(row("Report hash", parts::chip(&review.report_hash, "report hash").into())),
        )
        .child(el("p").class("ag-note").text(
            "The chain ties this report to the source it read, and that source to the build now trading.",
        ))
        .child(more_in_app(format!("/agents/{}/verify", strategy.slug), "Check the whole chain"))
        .into()
}
