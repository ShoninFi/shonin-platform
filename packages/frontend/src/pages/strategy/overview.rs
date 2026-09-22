//! What a depositor reads before deciding: what the strategy does, what the
//! review found, where the money sits, who is stopping it leaving, and how
//! they would get it back.

use std::rc::Rc;

use dominator::{Dom, html, text};
use shared::{ExitClass, Strategy, Valuation};

use crate::copy;
use crate::format as fmt;
use crate::pages::invest;
use crate::pages::parts;
use crate::pages::strategy::fee_table;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render(strategy: &Rc<Strategy>) -> Dom {
    html!("div", {
        .class(["grid", "grid--aside"])
        .child(html!("div", {
            .class("stack-lg")
            .child(what_it_does(strategy))
            .child(record(strategy))
            .child(review(strategy))
            .child(where_the_money_is(strategy))
            .child(leaving(strategy))
            .child(limits(strategy))
        }))
        .child(html!("div", {
            .class("stack")
            .children(aside(strategy))
        }))
    })
}

fn what_it_does(strategy: &Rc<Strategy>) -> Dom {
    ui::section(
        "What it does",
        vec![
            html!("p", { .text(&strategy.description) }),
            html!("p", {
                .class(["faint", "small"])
                .text(
                    "The author's own words. The review reads the code, not the description."
                )
            }),
        ],
    )
}

fn record(strategy: &Rc<Strategy>) -> Dom {
    let Some(record) = &strategy.record else {
        return ui::section(
            "The record",
            vec![ui::empty(
                "This strategy never traded, so there is no record to read.",
            )],
        );
    };

    ui::section_with_aside(
        "The record",
        Some(ui::action_link(
            &format!("/strategies/{}/record", strategy.slug),
            "The whole record",
        )),
        vec![
            parts::record_stats(strategy),
            ui::notice(
                Tone::Info,
                "The size it was earned at",
                &format!(
                    "This record was earned at an average of {}, and at most {}. Returns at a much larger \
                     size are unknown.",
                    fmt::money_compact(record.average_aum),
                    fmt::money_compact(record.peak_aum)
                ),
            ),
            if strategy.days_live < Strategy::DAYS_TO_RANK {
                ui::notice(
                    Tone::Warn,
                    "This strategy is labelled new",
                    &format!(
                        "It has run for {} and made {} trades. It is open for deposits, and won't be ranked \
                         until it has {} live days.",
                        fmt::days(strategy.days_live),
                        fmt::count(record.trades),
                        Strategy::DAYS_TO_RANK
                    ),
                )
            } else {
                ui::note(
                    "Returns are read from the vault's own value per share, which the contract \
                     computes from on-chain state. Nobody reports them.",
                )
            },
        ],
    )
}

fn review(strategy: &Rc<Strategy>) -> Dom {
    let Some(review) = &strategy.review else {
        return ui::section(
            "The review",
            vec![ui::empty(
                "Review pending. The score appears here when the reviewer has signed it.",
            )],
        );
    };

    let findings: Vec<Dom> = review
        .findings
        .iter()
        .take(2)
        .map(|finding| {
            html!("li", {
                .child(html!("span", { .text(&finding.summary) }))
                .child(text(" "))
                .child(html!("span", {
                    .class(["faint", "small"])
                    .text(&format!("{} · {}", finding.category.label(), finding.severity.label()))
                }))
            })
        })
        .collect();

    let more = review.findings.len().saturating_sub(2);

    ui::section_with_aside(
        "The review",
        Some(ui::action_link(
            &format!("/strategies/{}/review", strategy.slug),
            "The full report",
        )),
        vec![
            html!("div", {
                .class(["row", "row--wrap"])
                .child(ui::score(review.score, review.band()))
                .child(ui::muted(&format!(
                    "{} finding{}",
                    review.findings.len(),
                    if review.findings.len() == 1 { "" } else { "s" }
                )))
            }),
            html!("p", { .text(&review.summary) }),
            if findings.is_empty() {
                ui::note("No findings.")
            } else {
                html!("div", {
                    .class("stack-sm")
                    .child(html!("ul", { .children(findings) }))
                    .apply_if(more > 0, |dom| dom.child(ui::note(&format!(
                        "and {more} more in the full report"
                    ))))
                })
            },
            ui::note(copy::REVIEW_NOTE),
        ],
    )
}

fn where_the_money_is(strategy: &Rc<Strategy>) -> Dom {
    if strategy.legs.is_empty() {
        return ui::section(
            "Where the money is",
            vec![ui::empty("This strategy has no venue accounts.")],
        );
    }

    let rows: Vec<Row> = strategy
        .legs
        .iter()
        .map(|leg| {
            Row::new(vec![
                html!("div", {
                    .child(html!("div", { .class("table__primary").text(&leg.venue) }))
                    .child(html!("div", {
                        .class(["faint", "small"])
                        .text(leg.chain.label())
                    }))
                }),
                parts::custody_badge(leg.custody),
                html!("div", {
                    .child(html!("div", { .text(leg.valuation.label()) }))
                    .child(html!("div", {
                        .class(["faint", "small"])
                        .text(&valuation_detail(&leg.valuation))
                    }))
                }),
                ui::number(&fmt::percent(leg.share_of_value, 0)),
                if leg.status == shared::VenueStatus::Suspended {
                    ui::badge_dot("suspended", Tone::Error)
                } else {
                    ui::badge_dot("trading", Tone::Ok)
                },
            ])
        })
        .collect();

    let details: Vec<Dom> = strategy
        .legs
        .iter()
        .map(|leg| {
            ui::card(vec![
                html!("div", {
                    .class(["row", "row--between", "row--wrap"])
                    .child(html!("h3", { .class("subtitle").text(&leg.venue) }))
                    .child(parts::custody_badge(leg.custody))
                }),
                ui::kv(vec![
                    ("The vault's account".to_string(), ui::muted(&leg.account)),
                    ("What the executor holds".to_string(), ui::muted(&leg.key)),
                    ("It can".to_string(), ui::bullets(leg.can.iter().cloned())),
                    (
                        "It cannot".to_string(),
                        ui::bullets(leg.cannot.iter().cloned()),
                    ),
                ]),
                match &leg.note {
                    Some(note) => ui::note(note),
                    None => html!("span"),
                },
            ])
        })
        .collect();

    ui::section(
        "Where the money is, and who stops it leaving",
        vec![
            parts::venue_split(&strategy.legs),
            table(
                vec![
                    Column::new("Venue").wide(),
                    Column::new("Custody"),
                    Column::new("Valued by").wide(),
                    Column::num("Share"),
                    Column::new("State"),
                ],
                rows,
            ),
            ui::note(
                "Custody says who refuses a withdrawal to a stranger. Whatever the code does, \
                 funds can only move between this vault's own accounts.",
            ),
            html!("div", { .class(["grid", "grid--2"]).children(details) }),
        ],
    )
}

fn valuation_detail(valuation: &Valuation) -> String {
    match valuation {
        Valuation::OnChain { source } => source.clone(),
        Valuation::Posted { carrier, chain } => format!(
            "computed on {} and posted home over {carrier}",
            chain.label()
        ),
        Valuation::AttestedRead { image } => image.clone(),
    }
}

fn leaving(strategy: &Rc<Strategy>) -> Dom {
    let vault = &strategy.vault;

    let mut children = vec![
        html!("div", {
            .class(["row", "row--wrap"])
            .child(parts::exit_badge(vault.exit_class))
            .child(ui::muted(&vault.exit_class.summary()))
        }),
        ui::para(copy::EXIT_NOTE),
    ];

    if let ExitClass::Served { operators } = vault.exit_class {
        let resorts: Vec<Dom> = strategy
            .legs
            .iter()
            .filter_map(|leg| {
                leg.last_resort.as_ref().map(|resort| {
                    html!("li", {
                        .child(html!("strong", { .text(&format!("{}: ", leg.venue)) }))
                        .child(text(resort))
                    })
                })
            })
            .collect();

        children.push(ui::notice(
            Tone::Info,
            "This vault is served, not direct",
            &format!(
                "At least one venue can only be unwound off-chain, so a claim there is served by an \
                 unwinder that any allowlisted operator can run; {operators} can today. A claim \
                 unserved for {} days freezes the vault.",
                vault.freeze_after_days
            ),
        ));
        if !resorts.is_empty() {
            children.push(ui::subheading("If nobody serves a claim"));
            children.push(html!("ul", { .children(resorts) }));
        }
    }

    ui::section("Leaving", children)
}

fn limits(strategy: &Rc<Strategy>) -> Dom {
    if strategy.limits.is_empty() && strategy.dependencies.is_empty() {
        return html!("span");
    }

    let mut children = Vec::new();
    if !strategy.limits.is_empty() {
        children.push(ui::note(
            "These limits live in the strategy's own code, where the reviewer reads them. There \
             is no platform risk policy.",
        ));
        children.push(ui::bullets(strategy.limits.iter().cloned()));
    }
    if !strategy.dependencies.is_empty() {
        children.push(ui::subheading("What it depends on"));
        children.push(ui::bullets(strategy.dependencies.iter().cloned()));
    }

    ui::section("What it will not do", children)
}

fn aside(strategy: &Rc<Strategy>) -> Vec<Dom> {
    let vault = &strategy.vault;

    let mut cards = vec![
        invest::panel(strategy),
        ui::card_titled(
            "The vault",
            None,
            vec![ui::kv_rows(vec![
                ("Value".to_string(), ui::number(&fmt::money(vault.value))),
                (
                    "Value per share".to_string(),
                    ui::number(&fmt::share_price(vault.share_price)),
                ),
                (
                    "High-water mark".to_string(),
                    ui::number(&fmt::share_price(vault.high_water_mark)),
                ),
                (
                    "Shares".to_string(),
                    ui::number(&fmt::number(vault.shares, 0)),
                ),
                (
                    "Idle balance".to_string(),
                    html!("div", {
                        .child(ui::number(&fmt::money(vault.idle)))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text("withdrawals up to this are paid in one transaction")
                        }))
                    }),
                ),
                ("Asset".to_string(), ui::value(&vault.asset)),
                ("Chain".to_string(), ui::value(vault.chain.label())),
                ("Contract".to_string(), ui::address(&vault.address)),
                ("Opened".to_string(), ui::value(&fmt::date(vault.created))),
            ])],
        ),
    ];

    if let Some(transit) = &vault.in_transit {
        cards.push(ui::notice(
            Tone::Info,
            "Capital in transit",
            &format!(
                "{} is moving from {} to {} over {}. It counts in the vault's value, and times out \
                 after {} minutes.",
                fmt::money(transit.amount),
                transit.from,
                transit.to,
                transit.route,
                transit.timeout_minutes
            ),
        ));
    }

    cards.push(ui::card_titled(
        "The tank",
        Some(ui::badge(
            &format!("{} left", fmt::days(vault.tank.runway_days() as u32)),
            if vault.tank.runway_days() < 7.0 {
                Tone::Warn
            } else {
                Tone::Ok
            },
        )),
        vec![
            ui::kv_rows(vec![
                (
                    "Balance".to_string(),
                    ui::number(&fmt::money_exact(vault.tank.balance)),
                ),
                (
                    "Charges".to_string(),
                    ui::number(&fmt::per_day(vault.tank.daily_charge)),
                ),
                (
                    "Auto-fill target".to_string(),
                    ui::number(&fmt::money_exact(vault.tank.target)),
                ),
            ]),
            ui::note(copy::TANK_NOTE),
        ],
    ));

    cards.push(ui::card_titled(
        "Fees",
        None,
        vec![fee_table(), ui::note(copy::FEE_NOTE)],
    ));

    let audits: Vec<Dom> = vault
        .audits
        .iter()
        .map(|audit| {
            html!("div", {
                .class("stack-sm")
                .child(html!("div", {
                    .class(["row", "row--between", "row--wrap"])
                    .child(ui::value(&audit.subject))
                    .child(ui::badge(audit.status.label(), Tone::from_audit(audit.status)))
                }))
                .child(html!("div", { .class(["faint", "small"]).text(&audit.note) }))
            })
        })
        .collect();

    cards.push(ui::card_titled(
        "Audit status",
        None,
        vec![
            ui::note("Vaults open before every audit is finished, so the page says where each one stands."),
            html!("div", { .class("stack").children(audits) }),
        ],
    ));

    cards.push(ui::card_titled(
        "Which code is trading this",
        Some(ui::action_link(
            &format!("/strategies/{}/verify", strategy.slug),
            "Check it",
        )),
        vec![ui::kv_rows(vec![
            ("Bundle".to_string(), ui::hash(&strategy.bundle.bundle_hash)),
            (
                "Source".to_string(),
                ui::value(strategy.bundle.source.label()),
            ),
            (
                "Executors".to_string(),
                ui::value(&match &strategy.attestation {
                    Some(attestation) => {
                        let keys = attestation.keys.len();
                        format!("{keys} blessed key{}", if keys == 1 { "" } else { "s" })
                    }
                    None => "none running".to_string(),
                }),
            ),
        ])],
    ));

    cards
}
