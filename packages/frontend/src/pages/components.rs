//! The component registry, and one component's page.

use dominator::{Dom, html};
use shared::{Component, LogLevel, Strategy};

use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn list() -> Dom {
    let rows: Vec<Row> = data::get()
        .components
        .iter()
        .map(|component| {
            let users = data::composing(&component.slug);
            let capital: f64 = users.iter().map(|strategy| strategy.capital()).sum();
            let live = users
                .iter()
                .filter(|strategy| strategy.standing.is_live())
                .count();

            Row::linked(
                format!("/components/{}", component.slug),
                vec![
                    html!("div", {
                        .child(html!("div", { .class("table__primary").text(&component.name) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&component.summary)
                        }))
                    }),
                    ui::value(&component.version),
                    if component.first_party {
                        ui::badge("first-party", Tone::Accent)
                    } else {
                        ui::muted(&component.author)
                    },
                    ui::number(&users.len().to_string()),
                    ui::number(&live.to_string()),
                    ui::number(&fmt::money_compact(capital)),
                    ui::badge(component.audit.label(), Tone::from_audit(component.audit)),
                ],
            )
        })
        .collect();

    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text("Components") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "A component is a reusable piece of a strategy: it does one job, it is \
                     always published with its source, and a strategy pins it by hash."
                )
            }))
        }))
        .child(ui::section(
            "The registry",
            vec![
                table(
                    vec![
                        Column::new("Component").wide(),
                        Column::new("Version"),
                        Column::new("Author"),
                        Column::num("Strategies"),
                        Column::num("Live vaults"),
                        Column::num("Capital behind them"),
                        Column::new("Audit"),
                    ],
                    rows,
                ),
                ui::note(
                    "Reuse under real capital is the strongest signal a component has, so it is \
                     the column that matters most here. Components earn their authors nothing: \
                     they are free and public, and an author who wants revenue publishes a \
                     strategy that uses one.",
                ),
            ],
        ))
        .child(ui::section(
            "Why a bad component can't steal",
            vec![
                html!("div", {
                    .class(["grid", "grid--2"])
                    .child(ui::card(vec![
                        ui::subheading("What it can't reach"),
                        ui::bullets(vec![
                            "A component sees only what the strategy passes it.".to_string(),
                            "The vault's venue keys never enter the sandbox: they live in the \
                             executor's signer.".to_string(),
                            "There are no raw sockets and no filesystem, so it can't fetch or \
                             write anything on its own.".to_string(),
                            "Whatever it produces can only trade the vault's own accounts, which \
                             the venues and contracts enforce.".to_string(),
                        ]),
                    ]))
                    .child(ui::card(vec![
                        ui::subheading("What remains"),
                        ui::para(
                            "Being quietly wrong. A hedging component with a subtle sign error \
                             loses money inside every limit anyone enforces. Against that there \
                             is what there always is: public source, the review, and a live \
                             on-chain record.",
                        ),
                    ]))
                }),
            ],
        ))
    })
}

pub fn detail(slug: &str) -> Dom {
    let Some(component) = data::component(slug) else {
        return crate::pages::not_found::render();
    };
    let users = data::composing(slug);
    let capital: f64 = users.iter().map(|strategy| strategy.capital()).sum();

    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class(["row", "row--wrap", "row--between", "row--top"])
            .child(html!("div", {
                .child(html!("h1", { .class("title").text(&component.name) }))
                .child(html!("p", { .class("muted").text(&component.summary) }))
            }))
            .child(ui::badges(vec![
                ui::badge(&format!("v{}", component.version), Tone::Neutral),
                if component.first_party {
                    ui::badge("first-party", Tone::Accent)
                } else {
                    ui::badge("community", Tone::Info)
                },
                ui::badge(component.audit.label(), Tone::from_audit(component.audit)),
            ]))
        }))
        .child(html!("div", {
            .class(["grid", "grid--aside"])
            .child(html!("div", {
                .class("stack-lg")
                .child(ui::section("What it is", vec![html!("p", { .text(&component.description) })]))
                .child(interface(component))
                .child(usage(component, &users, capital))
                .child(versions(component))
                .child(warnings(component))
                .child(build_log(component))
            }))
            .child(html!("div", {
                .class("stack")
                .child(identity(component))
            }))
        }))
    })
}

fn interface(component: &'static Component) -> Dom {
    let rows: Vec<Row> = component
        .api
        .iter()
        .map(|(item, doc)| {
            Row::new(vec![
                html!("span", { .class("num").text(item) }),
                ui::muted(doc),
            ])
        })
        .collect();

    ui::section(
        "Interface",
        vec![
            table(
                vec![Column::new("Item"), Column::new("What it does").wide()],
                rows,
            ),
            ui::kv(vec![
                (
                    "Exports".to_string(),
                    ui::bullets(component.exports.iter().cloned()),
                ),
                (
                    "Imports".to_string(),
                    ui::bullets(component.imports.iter().cloned()),
                ),
            ]),
            ui::note(
                "A component has no privileges of its own. It runs inside the strategy's \
                 instance, with the same world and the same absence of sockets and filesystem.",
            ),
        ],
    )
}

fn usage(component: &'static Component, users: &[&'static Strategy], capital: f64) -> Dom {
    if users.is_empty() {
        return ui::section(
            "Used under real capital",
            vec![ui::empty("No strategy composes this component yet.")],
        );
    }

    let rows: Vec<Row> = users
        .iter()
        .map(|strategy| {
            Row::linked(
                parts::path_of(strategy),
                vec![
                    html!("span", { .class("table__primary").text(&strategy.name()) }),
                    ui::muted(&strategy.author),
                    parts::standing_badge(strategy.standing),
                    ui::number(&fmt::money_compact(strategy.capital())),
                    ui::hash(
                        &strategy
                            .bundle
                            .components
                            .iter()
                            .find(|pin| pin.slug == component.slug)
                            .map_or_else(String::new, |pin| pin.hash.clone()),
                    ),
                ],
            )
        })
        .collect();

    ui::section_with_aside(
        "Used under real capital",
        Some(ui::badge(
            &format!("{} behind it", fmt::money_compact(capital)),
            Tone::Accent,
        )),
        vec![
            table(
                vec![
                    Column::new("Strategy").wide(),
                    Column::new("Author"),
                    Column::new("Standing"),
                    Column::num("Capital"),
                    Column::new("Pinned hash").wide(),
                ],
                rows,
            ),
            ui::note(
                "Each strategy names the exact hash it composes, so a new version of this \
                 component never reaches a running vault. A component author can see here \
                 exactly who they would be asking to move.",
            ),
        ],
    )
}

fn versions(component: &'static Component) -> Dom {
    let rows: Vec<Row> = component
        .versions
        .iter()
        .map(|(version, hash, note)| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(version) }),
                ui::hash(hash),
                ui::muted(note),
            ])
        })
        .collect();

    ui::section(
        "Versions",
        vec![
            table(
                vec![
                    Column::new("Version"),
                    Column::new("Hash"),
                    Column::new("What changed").wide(),
                ],
                rows,
            ),
            ui::note(
                "A component is immutable once published: a fix is a new version with a new \
                 hash, and strategies adopt it by publishing new versions of themselves.",
            ),
        ],
    )
}

fn warnings(component: &'static Component) -> Dom {
    if component.review_warnings.is_empty() {
        return html!("span");
    }

    ui::section(
        "Raised in strategies' reviews",
        vec![
            ui::note(
                "A component isn't reviewed on its own: the reviewer reads it as part of every \
                 strategy that composes it, and what it says lands here.",
            ),
            html!("div", {
                .class("stack-sm")
                .children(component.review_warnings.iter().map(|warning| {
                    ui::notice(Tone::Warn, "From a review", warning)
                }))
            }),
        ],
    )
}

fn build_log(component: &'static Component) -> Dom {
    let lines: Vec<Dom> = component
        .build_log
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
        "The build log",
        vec![html!("div", { .class("log").children(lines) })],
    )
}

fn identity(component: &'static Component) -> Dom {
    ui::card_titled(
        "Identity",
        None,
        vec![
            ui::kv_rows(vec![
                ("Hash".to_string(), ui::hash(&component.hash)),
                ("Version".to_string(), ui::value(&component.version)),
                ("Author".to_string(), ui::muted(&component.author)),
                ("Source".to_string(), ui::muted(&component.repo)),
                (
                    "Published".to_string(),
                    ui::value(&fmt::date(component.published)),
                ),
            ]),
            ui::note(
                "Components are never sealed: a shared dependency nobody can read is the \
                 opposite of the point.",
            ),
        ],
    )
}
