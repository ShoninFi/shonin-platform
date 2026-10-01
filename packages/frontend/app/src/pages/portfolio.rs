//! What the viewer holds: positions across vaults, claims waiting to settle,
//! and what each has done.

use std::rc::Rc;

use dominator::{Dom, html};
use futures_signals::signal::SignalExt;
use shared::Strategy;

use crate::data;
use crate::format as fmt;
use crate::pages::{invest, parts};
use crate::state;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
use crate::ui::table::{Column, Row, table};
use crate::wallet::{self, Holdings};

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text("Your positions") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "What you hold across vaults, read from each vault's own contract."
                )
            }))
        }))
        .child(html!("div", {
            .child_signal(wallet::holdings().signal_cloned().map(|holdings| {
                Some(if holdings.is_connected() {
                    connected(&holdings)
                } else {
                    disconnected()
                })
            }))
        }))
    })
}

fn disconnected() -> Dom {
    html!("div", {
        .class("stack")
        .child(ui::empty(
            "Connect a wallet to deposit into a vault, withdraw from one, and watch a claim \
             settle. Nothing leaves your browser.",
        ))
        .child(html!("div", {
            .class(["row", "row--wrap"])
            .child(shell::button_link("/agents", "Browse the agents", true))
        }))
    })
}

fn connected(holdings: &Holdings) -> Dom {
    let held: Vec<(Rc<Strategy>, f64, f64)> = holdings
        .positions
        .iter()
        .filter_map(|position| {
            let strategy = data::strategy(&position.slug)?;
            let value = position.shares * strategy.vault.share_price;
            Some((strategy, value, position.cost))
        })
        .collect();

    html!("div", {
        .class("stack-lg")
        .child(invest::holdings_summary())
        .child(wallet_card(holdings))
        .child(positions(holdings, &held))
        .child(claims(holdings))
    })
}

fn wallet_card(holdings: &Holdings) -> Dom {
    let address = holdings.address.clone().unwrap_or_default();

    ui::card(vec![html!("div", {
        .class(["row", "row--between", "row--wrap"])
        .child(html!("div", {
            .class("row")
            .child(html!("span", { .class("stat__label").text("Wallet") }))
            .child(ui::address(&address))
        }))
        .child(html!("div", {
            .class(["row", "row--wrap"])
            .child(shell::button("Disconnect", false, || {
                wallet::disconnect();
                state::notify(
                    "Disconnected. What you hold is on the chain, and still there.",
                    Tone::Ok,
                );
            }))
        }))
    })])
}

fn positions(holdings: &Holdings, held: &[(Rc<Strategy>, f64, f64)]) -> Dom {
    if held.is_empty() {
        return ui::section("Positions", vec![ui::empty("You hold nothing yet.")]);
    }

    let rows: Vec<Row> = held
        .iter()
        .map(|(strategy, value, cost)| {
            let profit = value - cost;
            let shares = holdings
                .position(&strategy.slug)
                .map_or(0.0, |position| position.shares);
            let performance = data::performance(strategy);

            Row::linked(
                parts::path_of(strategy),
                vec![
                    html!("div", {
                        .child(html!("div", { .class("table__primary").text(strategy.name()) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!("by {}", strategy.author))
                        }))
                    }),
                    ui::number(&fmt::number(shares, 2)),
                    ui::number(&fmt::money_exact(*value)),
                    ui::number(&fmt::money_exact(*cost)),
                    ui::signed(profit, &fmt::money_exact(profit)),
                    match performance.return_30d {
                        Some(value) => ui::signed(value, &fmt::percent_signed(value, 1)),
                        None => ui::muted("—"),
                    },
                    parts::spark(strategy),
                ],
            )
        })
        .collect();

    ui::section(
        "Positions",
        vec![
            table(
                vec![
                    Column::new("Vault").wide(),
                    Column::num("Shares"),
                    Column::num("Worth"),
                    Column::num("Paid"),
                    Column::num("Profit and loss"),
                    Column::num("30 days"),
                    Column::new("Shape"),
                ],
                rows,
            ),
            ui::note("Shares times the value per share the vault's own contract computes."),
        ],
    )
}

fn claims(holdings: &Holdings) -> Dom {
    if holdings.claims.is_empty() {
        return html!("span");
    }

    let cards: Vec<Dom> = holdings
        .claims
        .iter()
        .filter_map(|claim| {
            let strategy = data::strategy(&claim.slug)?;
            Some(ui::card(vec![
                html!("div", {
                    .class(["row", "row--between", "row--wrap"])
                    .child(html!("a", {
                        .class("table__primary")
                        .attr("href", &parts::path_of(&strategy))
                        .text(strategy.name())
                    }))
                    .child(ui::badge(
                        claim.state.label(),
                        if claim.state == shared::ClaimState::Paid {
                            Tone::Ok
                        } else {
                            Tone::Warn
                        },
                    ))
                }),
                ui::kv(vec![
                    (
                        "Shares pending".to_string(),
                        ui::number(&fmt::number(claim.shares, 2)),
                    ),
                    (
                        "Fraction of the vault closed".to_string(),
                        ui::number(&fmt::percent(claim.fraction_closed, 2)),
                    ),
                    (
                        "Paid".to_string(),
                        match claim.settled_for {
                            Some(paid) => ui::number(&fmt::money_exact(paid)),
                            None => ui::muted("not yet"),
                        },
                    ),
                ]),
                ui::action_link(&parts::path_of(&strategy), "Drive the settlement"),
            ]))
        })
        .collect();

    ui::section(
        "Claims waiting to settle",
        vec![
            ui::note(
                "A withdrawal the idle balance couldn't pay. Your shares stay outstanding until it \
                 settles, and anyone can send the transactions that serve it.",
            ),
            html!("div", { .class(["grid", "grid--2"]).children(cards) }),
        ],
    )
}
