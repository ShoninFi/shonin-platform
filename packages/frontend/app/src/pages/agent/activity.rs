//! `/agents/:id/activity`: everything the record holds of what the vault did,
//! its contract's events and its trades on venues, newest first, each linked
//! to its transaction on the chain's explorer.

use std::rc::Rc;

use shared::{Side, Strategy};

use super::overview::event_rows;
use super::parts::{card, tx_link};
use super::{facts, live, side};
use crate::format;
use crate::store::{self, Agent};
use crate::view::{El, View, el};

pub fn body(agent: &Rc<Agent>) -> El {
    el("div")
        .class("ag-layout")
        .class("ag-layout--overview")
        .child(
            el("div")
                .class("ag-main")
                .child(live(agent, events))
                .child(live(agent, trades)),
        )
        .child(
            el("aside")
                .class("ag-side")
                .attr("aria-label", "Deposit, withdraw and running costs")
                .child(side::exit_slot(agent))
                .child(side::tank(agent)),
        )
}

fn this_year() -> i32 {
    store::get().as_of.get().map(|at| at.year()).unwrap_or(2026)
}

fn events(strategy: &Strategy) -> View {
    let rows = event_rows(strategy, this_year());
    let chain = strategy.vault.chain;
    let section = card("ag-events-title", "The vault's events", None)
        .class("ag-card--flush-table")
        .child(el("p").class("ag-muted").text(format!(
            "From the vault contract's own log on {}: deposits, withdrawals, fees, the tank and every change of state.",
            chain.label()
        )));
    if rows.is_empty() {
        return section
            .child(el("p").class("ag-muted").text("Nothing on the record yet."))
            .into();
    }
    section
        .child(
            el("div").class("ag-scroll").child(
                el("table")
                    .class("sh-table")
                    .class("ag-log")
                    .child(
                        el("caption")
                            .class("sh-vh")
                            .text("Vault events, newest first"),
                    )
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("When"))
                                .child(el("th").attr("scope", "col").text("What"))
                                .child(el("th").attr("scope", "col").text("Detail"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Amount"))
                                .child(el("th").attr("scope", "col").text("Transaction")),
                        ),
                    )
                    .child(el("tbody").children(rows.iter().map(|row| {
                        el("tr")
                            .class("sh-row")
                            .child(
                                el("td")
                                    .class("ag-when")
                                    .child(el("span").class("ag-when__day").text(row.day.clone()))
                                    .child(
                                        el("span").class("ag-when__time").text(row.time.clone()),
                                    ),
                            )
                            .child(
                                el("th")
                                    .attr("scope", "row")
                                    .class("ag-what")
                                    .text(row.what),
                            )
                            .child(el("td").class("ag-detail").text(row.detail.clone()))
                            .child(
                                el("td").class("sh-r").class("ag-amount").text(
                                    row.amount
                                        .clone()
                                        .unwrap_or_else(|| format::MISSING.to_string()),
                                ),
                            )
                            .child(el("td").child(tx_link(chain, &row.reference, "Transaction")))
                    }))),
            ),
        )
        .into()
}

fn trades(strategy: &Strategy) -> View {
    let chain = strategy.vault.chain;
    let year = this_year();
    let mut trades: Vec<&shared::Trade> = strategy.trades.iter().collect();
    trades.sort_by_key(|trade| std::cmp::Reverse(trade.at));
    let total = strategy
        .record
        .as_ref()
        .map(|record| record.trades)
        .unwrap_or(trades.len() as u32);
    let section = card("ag-trades-title", "Trades", None)
        .class("ag-card--flush-table")
        .child(el("p").class("ag-muted").text(if trades.is_empty() {
            "Fills on its venues, as anyone reading the chain would see them.".to_string()
        } else {
            format!(
                "Fills on its venues, as anyone reading the chain would see them. The latest {} of {}.",
                format::number(trades.len() as f64, 0),
                format::number(f64::from(total.max(trades.len() as u32)), 0)
            )
        }));
    if trades.is_empty() {
        return section
            .child(
                el("p")
                    .class("ag-muted")
                    .text("No trades on the record yet."),
            )
            .into();
    }
    section
        .child(
            el("div").class("ag-scroll").child(
                el("table")
                    .class("sh-table")
                    .class("ag-log")
                    .child(el("caption").class("sh-vh").text("Trades, newest first"))
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("When"))
                                .child(el("th").attr("scope", "col").text("Market"))
                                .child(el("th").attr("scope", "col").text("Side"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Size"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Price"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Value"))
                                .child(el("th").attr("scope", "col").text("Transaction")),
                        ),
                    )
                    .child(el("tbody").children(trades.into_iter().map(|trade| {
                        let known = !facts::is_unknown(trade.at);
                        el("tr")
                            .class("sh-row")
                            .child(
                                el("td")
                                    .class("ag-when")
                                    .child(el("span").class("ag-when__day").text(if known {
                                        format::day(trade.at.date(), year)
                                    } else {
                                        format::MISSING.to_string()
                                    }))
                                    .child(el("span").class("ag-when__time").text(if known {
                                        format::time_short(trade.at)
                                    } else {
                                        String::new()
                                    })),
                            )
                            .child(
                                el("th")
                                    .attr("scope", "row")
                                    .class("ag-what")
                                    .text(trade.market.clone())
                                    .child(
                                        el("span")
                                            .class("ag-log__venue")
                                            .text(format!("{} · {}", trade.venue, trade.kind)),
                                    ),
                            )
                            .child(
                                el("td")
                                    .class(match trade.side {
                                        Side::Buy => "ag-side--buy",
                                        Side::Sell => "ag-side--sell",
                                    })
                                    .text(match trade.side {
                                        Side::Buy => "Buy",
                                        Side::Sell => "Sell",
                                    }),
                            )
                            .child(
                                el("td")
                                    .class("sh-r")
                                    .class("sh-fig")
                                    .text(size(trade.size)),
                            )
                            .child(
                                el("td")
                                    .class("sh-r")
                                    .class("sh-fig")
                                    .text(format::money(trade.price)),
                            )
                            .child(
                                el("td")
                                    .class("sh-r")
                                    .class("sh-fig")
                                    .text(format::money(trade.notional)),
                            )
                            .child(el("td").child(tx_link(chain, &trade.reference, "Transaction")))
                    }))),
            ),
        )
        .into()
}

/// A trade's size, to as many places as it needs (up to eight).
fn size(value: f64) -> String {
    let text = format!("{value:.8}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".to_string()
    } else {
        text.to_string()
    }
}
