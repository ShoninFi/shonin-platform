//! What the viewer holds: positions across vaults, claims waiting to settle,
//! and how correlated any of it is.

use std::rc::Rc;

use dominator::{Dom, html, text};
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
            .child(shell::button_link("/strategies", "Browse the strategies", true))
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
        .child(correlations(&held))
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
                        .child(html!("div", { .class("table__primary").text(&strategy.name()) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!("by {}", strategy.author))
                        }))
                    }),
                    ui::number(&fmt::number(shares, 2)),
                    ui::number(&fmt::money_exact(*value)),
                    ui::number(&fmt::money_exact(*cost)),
                    ui::signed(profit, &fmt::money_exact(profit)),
                    ui::signed(
                        performance.normalised_annualised,
                        &fmt::percent_signed(performance.normalised_annualised, 1),
                    ),
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
                    Column::num("Normalised"),
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
                        .text(&strategy.name())
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

fn correlations(held: &[(Rc<Strategy>, f64, f64)]) -> Dom {
    if held.is_empty() {
        return html!("span");
    }

    let series = |strategy: &Strategy| {
        strategy
            .record
            .as_ref()
            .map(|record| record.value_per_share.clone())
            .unwrap_or_default()
    };

    let benchmarks = data::benchmarks();
    let mut columns = vec![Column::new("Against").wide()];
    for (strategy, _, _) in held {
        columns.push(Column::num(strategy.name()));
    }
    for benchmark in &benchmarks {
        columns.push(Column::num(benchmark.label.clone()));
    }

    let rows: Vec<Row> = held
        .iter()
        .map(|(strategy, _, _)| {
            let own = series(strategy);
            let mut cells = vec![html!("span", {
                .class("table__primary")
                .text(&strategy.name())
            })];

            for (other, _, _) in held {
                let value = data::correlation(&own, &series(other));
                cells.push(correlation_cell(value, strategy.slug == other.slug));
            }
            for benchmark in &benchmarks {
                cells.push(correlation_cell(
                    data::correlation(&own, &benchmark.series),
                    false,
                ));
            }
            Row::new(cells)
        })
        .collect();

    // The closest pair, which is the one worth saying something about.
    let worst = held
        .iter()
        .flat_map(|(a, _, _)| {
            held.iter()
                .filter(move |(b, _, _)| a.slug != b.slug)
                .map(move |(b, _, _)| {
                    (
                        a.name(),
                        b.name(),
                        data::correlation(&series(a), &series(b)),
                    )
                })
        })
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut children = vec![
        ui::note(
            "How each position has moved with the others and with a few benchmarks. Two things \
             that move together are more of one risk, not less.",
        ),
        table(columns, rows),
    ];

    if let Some((a, b, value)) = worst
        && value > 0.6
    {
        children.push(ui::notice(
            Tone::Warn,
            "Two of your positions move together",
            &format!(
                "{a} and {b} are {} correlated over the days they share.",
                fmt::number(value, 2)
            ),
        ));
    }

    ui::section("How correlated your holdings are", children)
}

fn correlation_cell(value: f64, own: bool) -> Dom {
    if own {
        return ui::muted("—");
    }
    html!("span", {
        .class("num")
        .apply(|dom| {
            if value >= 0.6 {
                dom.class("down")
            } else if value <= 0.2 {
                dom.class("up")
            } else {
                dom
            }
        })
        .child(text(&fmt::number(value, 2)))
    })
}
