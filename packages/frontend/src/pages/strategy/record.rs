//! The on-chain record: what the vault is worth day by day, what it gave back
//! along the way, and every trade and contract event behind it.

use std::sync::OnceLock;

use dominator::{Dom, html};
use futures_signals::map_ref;
use futures_signals::signal::Mutable;
use shared::Strategy;

use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::chart::{self, Series};
use crate::ui::primitives as ui;
use crate::ui::table::{Column, Row, table};
use crate::ui::tabs;

/// Which series the main chart shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Value,
    Drawdown,
    Size,
}

/// Which list is under the charts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    Trades,
    Events,
}

fn line() -> &'static Mutable<Line> {
    static LINE: OnceLock<Mutable<Line>> = OnceLock::new();
    LINE.get_or_init(|| Mutable::new(Line::Value))
}

fn listing() -> &'static Mutable<Listing> {
    static LISTING: OnceLock<Mutable<Listing>> = OnceLock::new();
    LISTING.get_or_init(|| Mutable::new(Listing::Trades))
}

pub fn render(strategy: &'static Strategy) -> Dom {
    let Some(record) = &strategy.record else {
        return ui::empty("This strategy never traded, so there is no record.");
    };

    html!("div", {
        .class("stack-lg")
        .child(ui::section(
            "Read from the chain",
            vec![parts::record_stats(strategy), ui::card(vec![summary(strategy)])],
        ))
        .child(ui::section_with_aside(
            "Value per share",
            Some(tabs::segmented(
                vec![
                    (Line::Value, "Value"),
                    (Line::Drawdown, "Drawdown"),
                    (Line::Size, "Size"),
                ],
                line(),
            )),
            vec![html!("div", {
                .child_signal(map_ref! {
                    let line = line().signal() => {
                        Some(main_chart(strategy, *line))
                    }
                })
            })],
        ))
        .child(ui::section(
            "By month",
            vec![
                chart::bar_chart(record.monthly.clone()),
                ui::note(
                    "The month's return on value per share, with fees already in it.",
                ),
            ],
        ))
        .child(ui::section_with_aside(
            "What it did",
            Some(tabs::segmented(
                vec![(Listing::Trades, "Trades"), (Listing::Events, "Vault events")],
                listing(),
            )),
            vec![html!("div", {
                .child_signal(map_ref! {
                    let listing = listing().signal() => {
                        Some(match listing {
                            Listing::Trades => trades(strategy),
                            Listing::Events => events(strategy),
                        })
                    }
                })
            })],
        ))
    })
}

fn summary(strategy: &'static Strategy) -> Dom {
    let performance = data::performance(strategy);
    let record = strategy.record.as_ref().expect("checked by the caller");

    ui::kv(vec![
        (
            "Value per share".to_string(),
            ui::number(&fmt::share_price(
                record
                    .value_per_share
                    .last()
                    .map_or(1.0, |point| point.value),
            )),
        ),
        (
            "Total return".to_string(),
            ui::signed(
                performance.total_return,
                &fmt::percent_signed(performance.total_return, 1),
            ),
        ),
        (
            "Realised volatility".to_string(),
            ui::number(&fmt::percent(performance.volatility, 1)),
        ),
        (
            "Capital behind it".to_string(),
            ui::value(&format!(
                "{} now · {} on average · {} at most",
                fmt::money_compact(strategy.capital()),
                fmt::money_compact(record.average_aum),
                fmt::money_compact(record.peak_aum)
            )),
        ),
    ])
}

fn main_chart(strategy: &'static Strategy, line: Line) -> Dom {
    let record = strategy.record.as_ref().expect("checked by the caller");

    match line {
        Line::Value => html!("div", {
            .class("stack-sm")
            .child(chart::line_chart(
                vec![
                    Series::new("value per share", record.value_per_share.clone()),
                    Series::reference("normalised", record.normalised.clone()),
                ],
                fmt::share_price,
            ))
            .child(ui::note(
                "The solid line is what the vault's contract computed. The dashed line is the same \
                 record rescaled to 15% annualised volatility, which the leaderboard ranks on.",
            ))
        }),
        Line::Drawdown => html!("div", {
            .class("stack-sm")
            .child(chart::area_chart(
                vec![Series::new("drawdown", record.drawdown.clone())],
                |value| fmt::percent_signed(value, 1),
            ))
            .child(ui::note(
                "How far below its own high-water mark the vault sat on each day.",
            ))
        }),
        Line::Size => html!("div", {
            .class("stack-sm")
            .child(chart::area_chart(
                vec![Series::new("capital", record.aum.clone())],
                fmt::money_compact,
            ))
            .child(ui::note(
                "The capital behind the strategy, day by day: the size this record was earned at.",
            ))
        }),
    }
}

fn trades(strategy: &'static Strategy) -> Dom {
    if strategy.trades.is_empty() {
        return ui::empty("No trades yet.");
    }

    let rows: Vec<Row> = strategy
        .trades
        .iter()
        .map(|trade| {
            Row::new(vec![
                ui::muted(&fmt::ago(trade.at, data::now())),
                ui::value(&trade.venue),
                ui::value(&trade.market),
                ui::badge(
                    trade.side.label(),
                    if trade.side == shared::Side::Buy {
                        crate::ui::primitives::Tone::Ok
                    } else {
                        crate::ui::primitives::Tone::Neutral
                    },
                ),
                ui::number(&fmt::number(trade.size, 3)),
                ui::number(&fmt::number(trade.price, 2)),
                ui::number(&fmt::money_compact(trade.notional)),
                ui::muted(&trade.kind),
                ui::hash(&trade.reference),
            ])
        })
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(table(
            vec![
                Column::new("When"),
                Column::new("Venue"),
                Column::new("Market"),
                Column::new("Side"),
                Column::num("Size"),
                Column::num("Price"),
                Column::num("Notional"),
                Column::new("Kind"),
                Column::new("Reference").wide(),
            ],
            rows,
        ))
        .child(ui::note(
            "Every order, fill and transfer on a venue is already a public, timestamped record.",
        ))
    })
}

fn events(strategy: &'static Strategy) -> Dom {
    if strategy.events.is_empty() {
        return ui::empty("No events yet.");
    }

    let rows: Vec<Row> = strategy
        .events
        .iter()
        .map(|event| {
            Row::new(vec![
                ui::muted(&fmt::ago(event.at, data::now())),
                ui::badge(event.kind.label(), crate::ui::primitives::Tone::Neutral),
                ui::muted(&event.summary),
                match event.amount {
                    Some(amount) => ui::number(&fmt::money_exact(amount)),
                    None => ui::muted("—"),
                },
                ui::hash(&event.reference),
            ])
        })
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(table(
            vec![
                Column::new("When"),
                Column::new("Event"),
                Column::new("What happened").wide(),
                Column::num("Amount"),
                Column::new("Reference"),
            ],
            rows,
        ))
        .child(ui::note(
            "The vault contract's own event log.",
        ))
    })
}
