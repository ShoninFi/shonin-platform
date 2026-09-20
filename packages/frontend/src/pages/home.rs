//! The home page: what the platform is, what it guarantees, and the
//! strategies running on it.

use dominator::{Dom, html, link, text};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(hero())
        .child(totals())
        .child(guarantees())
        .child(featured())
        .child(how_it_works())
        .child(second_product())
        .child(verify_note())
    })
}

fn hero() -> Dom {
    html!("section", {
        .class(["panel", "stack"])
        .child(html!("h1", { .class("display").text(copy::PITCH) }))
        .child(html!("p", { .class("lede").text(copy::HOME_LEDE) }))
        .child(html!("div", {
            .class(["row", "row--wrap"])
            .child(shell::button_link("/strategies", "Browse the strategies", true))
            .child(shell::button_link("/about", "How it works", false))
        }))
    })
}

fn totals() -> Dom {
    let live = data::vaults()
        .iter()
        .filter(|strategy| strategy.standing.is_live())
        .count();
    let agents = data::agents().len();

    ui::stats(vec![
        ui::stat(
            "In vaults",
            &fmt::money_compact(data::total_vault_capital()),
            Some(&format!("{live} live strategies")),
        ),
        ui::stat(
            "In connected accounts",
            &fmt::money_compact(data::total_connected_capital()),
            Some(&format!("{agents} agents")),
        ),
        ui::stat(
            "Trades on the record",
            &fmt::count(data::total_trades()),
            Some("every one of them public"),
        ),
        ui::stat(
            "Authors",
            &data::get().authors.len().to_string(),
            Some("anyone can publish"),
        ),
    ])
}

fn guarantees() -> Dom {
    ui::section(
        "What a depositor is promised",
        vec![
            ui::note(copy::HALT_NEVER_STEAL),
            html!("div", {
                .class(["grid", "grid--3"])
                .children(copy::GUARANTEES.iter().map(|(title, body, rules_out)| {
                    ui::card(vec![
                        html!("h3", { .class("subtitle").text(title) }),
                        ui::para(body),
                        html!("p", { .class(["faint", "small"]).text(rules_out) }),
                    ])
                }))
            }),
        ],
    )
}

fn featured() -> Dom {
    let ranked = data::ranked();
    let cards: Vec<Dom> = ranked
        .into_iter()
        .take(4)
        .map(parts::strategy_card)
        .collect();

    ui::section_with_aside(
        "Strategies with a record",
        Some(ui::action_link("/strategies", "The whole leaderboard")),
        vec![
            ui::note(
                "Ranked on live on-chain records, rescaled to one level of risk so the list \
                 doesn't simply reward leverage. Every record shows the size it was earned at.",
            ),
            html!("div", {
                .class(["grid", "grid--4"])
                .children(cards)
            }),
        ],
    )
}

fn how_it_works() -> Dom {
    ui::section(
        "How it works",
        vec![html!("ol", {
            .class("steps")
            .children(copy::HOW_IT_WORKS.iter().map(|(title, body)| {
                html!("li", {
                    .class(["steps__item", "is-done"])
                    .child(html!("div", { .class("steps__title").text(title) }))
                    .child(html!("p", { .class("muted").text(body) }))
                })
            }))
        })],
    )
}

fn second_product() -> Dom {
    ui::section_with_aside(
        "The second product: connected accounts",
        Some(ui::action_link("/accounts", "The catalogue")),
        vec![
            ui::para(copy::CONNECTED_LEDE),
            html!("div", {
                .class(["grid", "grid--2"])
                .children(data::agents().into_iter().take(2).map(parts::strategy_card))
            }),
        ],
    )
}

fn verify_note() -> Dom {
    ui::notice_with(
        Tone::Accent,
        "Nothing here asks to be taken on trust",
        "Which code is trading a vault, that its bundle came from its source, that its trades \
         are real and what it is worth are all things anyone can check with a chain RPC and the \
         published source.",
        Some(html!("div", {
            .class(["row", "row--wrap"])
            .child(ui::action_link("/verify", "How to check it"))
            .child(html!("span", {
                .class(["small", "muted"])
                .child(text("Publishing a strategy? "))
                .child(link!("/publish", { .text("Start here") }))
                .child(text("."))
            }))
        })),
    )
}
