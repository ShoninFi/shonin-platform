//! The agent page's right column: the Exit panel's slot, the tank and the
//! verified build on the overview; a short way in to deposit or withdraw on
//! the other tabs. Sticky from 900px; under the header below that.

use std::rc::Rc;

use shared::{ExitClass, Strategy};
use ui::icons::Icon;

use super::live;
use super::parts::{self, risk_line};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, on_click};
use crate::atoms::gauge::gauge;
use crate::atoms::icon;
use crate::components::exit::{self, Tab};
use crate::format;
use crate::route::DOCS;
use crate::store::Agent;
use crate::view::{El, View, el, text};

/// The Exit panel's place. The panel is the wallet code's; this is its slot.
pub fn exit_slot(agent: &Rc<Agent>) -> El {
    el("div")
        .class("ag-exit-slot")
        .child(exit::panel(&agent.slug))
}

/// How long the agent can keep running: the gauge against 60 days, the
/// runway in words, loud when it has run dry.
pub fn tank(agent: &Rc<Agent>) -> El {
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-tank")
        .class("ag-o-tank")
        .class_from("ag-tank--empty", &agent.tank, |tank| tank.is_empty())
        .attr("aria-labelledby", "ag-tank-title")
        .child(
            el("div")
                .class("ag-tank__head")
                .child(
                    el("h2")
                        .class("ag-tank__title")
                        .attr("id", "ag-tank-title")
                        .child(icon(Icon::Tank, 20))
                        .text("Tank"),
                )
                .child(
                    el("p")
                        .class("ag-tank__balance")
                        .text_from(&agent.tank, |tank| format::money(tank.balance.max(0.0))),
                ),
        )
        .child(gauge(&agent.tank, false))
        .child(el("p").class("ag-tank__runway").text_from(&agent.tank, |tank| {
            if tank.is_empty() {
                "Stopped: tank empty. The agent starts again once the tank has enough to run."
                    .to_string()
            } else {
                tank.sentence()
            }
        }))
        .child(live(agent, auto_fill))
        .child(
            el("p")
                .class("ag-tank__about")
                .text("The tank pays for running the agent. The Author fills it, never your money. ")
                .child(el("a").attr("href", format!("{DOCS}tank/")).text("About the tank")),
        )
}

fn auto_fill(strategy: &Strategy) -> View {
    let keep = strategy.vault.tank.keep_at;
    if keep > 0.0 {
        el("p")
            .class("ag-tank__fill")
            .child(icon(Icon::Check, 16))
            .text(format!(
                "Auto-fill is on: the Author keeps it at {}.",
                format::money(keep)
            ))
            .into()
    } else {
        el("p")
            .class("ag-tank__fill")
            .text("Auto-fill is off: the Author tops it up by hand.")
            .into()
    }
}

/// "Verified build": the one build the chain lets trade this vault.
pub fn verified_build(agent: &Rc<Agent>) -> El {
    let slug = agent.slug.clone();
    el("section")
        .class("ag-built")
        .class("ag-o-build")
        .attr("aria-labelledby", "ag-built-title")
        .child(
            el("h2")
                .class("ag-built__title")
                .attr("id", "ag-built-title")
                .child(icon(Icon::Seal, 20))
                .text("Verified build"),
        )
        .child(live(agent, move |strategy| {
            let build = &strategy.bundle.component_hash;
            View::Many(vec![
                el("p")
                    .class("ag-built__text")
                    .text("This vault is traded only by build ")
                    .child(el("span").class("sh-mono").text(format::short_hash(build)))
                    .text(", which the chain ties to the reviewed source.")
                    .into(),
                el("div")
                    .class("ag-built__row")
                    .child(parts::chip(build, "build hash"))
                    .child(parts::more_in_app(
                        format!("/agents/{slug}/verify"),
                        "Check it yourself",
                    ))
                    .into(),
            ])
        }))
}

/// The exit in one sentence: direct or served.
pub fn exit_sentence(strategy: &Strategy) -> View {
    match strategy.vault.exit_class {
        ExitClass::Direct => View::Many(vec![
            el("strong").text("Direct exit.").into(),
            text(" Its contracts can always finish a withdrawal by themselves."),
        ]),
        ExitClass::Served { .. } => View::Many(vec![
            el("strong").text("Served exit.").into(),
            text(" A booked withdrawal needs an unwinder to finish it."),
        ]),
    }
}

fn buttons(slug: &str) -> El {
    let deposit = {
        let slug = slug.to_string();
        on_click(
            Button::new(Kind::Primary, "Deposit").block().build(),
            move || exit::open_sheet(&slug, Tab::Deposit),
        )
    };
    let withdraw = {
        let slug = slug.to_string();
        on_click(
            Button::new(Kind::Secondary, "Withdraw").block().build(),
            move || exit::open_sheet(&slug, Tab::Withdraw),
        )
    };
    el("div")
        .class("ag-mini-exit__buttons")
        .child(deposit)
        .child(withdraw)
}

/// The review tab's way in: the share price, the exit in a sentence,
/// Deposit and Withdraw.
pub fn mini_exit(agent: &Rc<Agent>) -> El {
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-mini-exit")
        .attr("aria-label", "Deposit or withdraw")
        .child(live(agent, |strategy| {
            let since = crate::model::figures(strategy).since_opening;
            el("div")
                .class("ag-mini-exit__price")
                .child(
                    el("div")
                        .child(el("p").class("sh-label").text("Share price"))
                        .child(
                            el("p")
                                .class("ag-mini-exit__figure")
                                .text(format::price(strategy.vault.share_price)),
                        ),
                )
                .child(View::from(since.map(|since| {
                    let tone = match format::direction(since) {
                        format::Direction::Up => Tone::Gain,
                        format::Direction::Down => Tone::Loss,
                        format::Direction::Flat => Tone::Neutral,
                    };
                    badge(tone, None, format!("{} since opening", format::ret(since))).into()
                })))
                .into()
        }))
        .child(live(agent, |strategy| {
            crate::atoms::notice::notice_with(
                crate::atoms::notice::NoticeTone::Info,
                Icon::Exit,
                exit_sentence(strategy),
            )
            .class("sh-notice--compact")
            .into()
        }))
        .child(buttons(&agent.slug))
        .child(risk_line())
}

/// The verify tab's way out: you can always get out, and how, without us.
pub fn way_out(agent: &Rc<Agent>) -> El {
    let chain = agent.chain;
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-mini-exit")
        .attr("aria-labelledby", "ag-out-title")
        .child(live(agent, move |strategy| {
            let direct = matches!(strategy.vault.exit_class, ExitClass::Direct);
            View::Many(vec![
                el("div")
                    .class("ag-way__head")
                    .child(el("span").class("ag-way__icon").child(icon(Icon::Exit, 20)))
                    .child(
                        el("div")
                            .child(el("p").class("sh-label").text(if direct { "Direct exit" } else { "Served exit" }))
                            .child(
                                el("h2")
                                    .class("ag-way__title")
                                    .attr("id", "ag-out-title")
                                    .text(if direct {
                                        "You can always get out"
                                    } else {
                                        "How you'd get out"
                                    }),
                            ),
                    )
                    .into(),
                el("p")
                    .class("ag-way__text")
                    .text(if direct {
                        "This vault trades only on its own chain, so its contracts can always finish a withdrawal by themselves. You can withdraw straight from the contract, even if everything Shonin runs goes dark."
                    } else {
                        "This vault trades on other chains, so a booked withdrawal needs an unwinder to finish it. A withdrawal its cash covers is paid by the contract alone."
                    })
                    .into(),
                el("div")
                    .class("ag-way__row")
                    .child(el("span").class("ag-way__key").text("Vault contract"))
                    .child(parts::chip(&strategy.vault.address, "vault contract address"))
                    .into(),
            ])
        }))
        .child(buttons(&agent.slug))
        .child(live(agent, move |strategy| {
            View::from(parts::address_url(chain, &strategy.vault.address).map(|url| {
                el("a")
                    .class("sh-link")
                    .class("ag-way__self")
                    .attr("href", url)
                    .attr("target", "_blank")
                    .attr("rel", "noopener noreferrer")
                    .text("Withdraw without this site")
                    .child(icon(Icon::External, 16))
                    .into()
            }))
        }))
        .child(risk_line())
}

/// Every hash on the verify page, each with its copy button, and one button
/// that copies them all with their labels.
pub fn all_hashes(agent: &Rc<Agent>) -> El {
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-hashes")
        .attr("aria-labelledby", "ag-hashes-title")
        .child(
            el("h2")
                .class("ag-card__title")
                .class("ag-card__title--s")
                .attr("id", "ag-hashes-title")
                .text("Every hash on this page"),
        )
        .child(
            el("p")
                .class("ag-hashes__lede")
                .text("Paste them into any block explorer to check them without us."),
        )
        .child(live(agent, |strategy| {
            let rows = facts_hashes(strategy);
            let all: String = rows
                .iter()
                .map(|(label, value)| format!("{label}: {value}"))
                .collect::<Vec<_>>()
                .join("\n");
            View::Many(vec![
                el("dl")
                    .class("ag-hashes__list")
                    .children(rows.iter().map(|(label, value)| {
                        el("div")
                            .child(el("dt").text(*label))
                            .child(el("dd").child(parts::chip(value, "hash")))
                    }))
                    .into(),
                copy_all(all).into(),
            ])
        }))
        .child(
            el("p")
                .class("ag-hashes__note")
                .text("Copies the full-length values, one per line, with labels."),
        )
}

fn copy_all(all: String) -> El {
    let copied = futures_signals::signal::Mutable::new(false);
    on_click(
        Button::new(Kind::Secondary, "Copy all hashes")
            .size(crate::atoms::button::Size::Small)
            .icon_before(Icon::Copy)
            .block()
            .build(),
        {
            let copied = copied.clone();
            move || {
                crate::web::copy(&all);
                copied.set(true);
                let copied = copied.clone();
                gloo_timers::callback::Timeout::new(1_500, move || copied.set(false)).forget();
            }
        },
    )
    .child(
        el("span")
            .class("sh-vh")
            .attr("role", "status")
            .text_from(&copied, |copied| {
                if *copied {
                    "Copied".to_string()
                } else {
                    String::new()
                }
            }),
    )
}

/// The hashes the verify page names, labelled.
pub fn facts_hashes(strategy: &Strategy) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Source", strategy.bundle.source_hash.clone())];
    if let Some(review) = &strategy.review {
        rows.push(("Review", review.report_hash.clone()));
    }
    rows.push(("Build", strategy.bundle.component_hash.clone()));
    if let Some(attestation) = &strategy.attestation {
        rows.push(("Executor image", attestation.image.digest.clone()));
    }
    rows.push(("Vault contract", strategy.vault.address.clone()));
    rows
}
