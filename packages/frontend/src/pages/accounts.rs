//! Connected accounts: the catalogue of agents that trade an account its
//! owner keeps at their own broker.

use dominator::{Dom, html};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(header())
        .child(catalogue())
        .child(difference())
        .child(records())
    })
}

fn header() -> Dom {
    html!("div", {
        .class("stack")
        .child(html!("h1", { .class("title").text("Connected accounts") }))
        .child(html!("p", { .class("lede").text(copy::CONNECTED_LEDE) }))
    })
}

fn catalogue() -> Dom {
    let agents = data::agents();
    if agents.is_empty() {
        return ui::empty("No agents yet.");
    }

    let rows: Vec<Row> = agents
        .iter()
        .map(|agent| {
            let connected = agent.connected().expect("an agent has a connected product");
            let performance = data::performance(agent);

            Row::linked(
                parts::path_of(agent),
                vec![
                    html!("div", {
                        .child(html!("div", { .class("table__primary").text(&agent.name()) }))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&agent.title)
                        }))
                    }),
                    ui::muted(&agent.author),
                    ui::value(&connected.broker),
                    ui::number(&fmt::count(connected.accounts)),
                    ui::number(&fmt::money_compact(connected.total_value)),
                    ui::signed(
                        performance.normalised_annualised,
                        &fmt::percent_signed(performance.normalised_annualised, 1),
                    ),
                    parts::review_badge(agent),
                    parts::standing_badge(agent.standing),
                    parts::spark(agent),
                ],
            )
        })
        .collect();

    ui::section(
        "The catalogue",
        vec![
            table(
                vec![
                    Column::new("Strategy").wide(),
                    Column::new("Author"),
                    Column::new("Broker"),
                    Column::num("Accounts"),
                    Column::num("Total value"),
                    Column::num("Normalised"),
                    Column::num("Review"),
                    Column::new("Standing"),
                    Column::new("Shape"),
                ],
                rows,
            ),
            ui::note(
                "The same authors, the same source-first publishing and attested build, the same \
                 review, and the same rule that only the committed code can use an account's \
                 credentials. What differs is whose money it is.",
            ),
        ],
    )
}

fn difference() -> Dom {
    let rows: Vec<Row> = copy::PRODUCTS
        .iter()
        .map(|(row, vault, connected)| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(row) }),
                ui::muted(vault),
                ui::muted(connected),
            ])
        })
        .collect();

    ui::section(
        "How this differs from a vault",
        vec![table(
            vec![
                Column::new("").wide(),
                Column::new("Pooled vault").wide(),
                Column::new("Connected account").wide(),
            ],
            rows,
        )],
    )
}

fn records() -> Dom {
    ui::section(
        "What a record here means",
        vec![
            ui::notice(
                Tone::Info,
                "Labelled attested reads",
                copy::CONNECTED_RECORD_NOTE,
            ),
            html!("div", {
                .class(["grid", "grid--3"])
                .child(ui::card(vec![
                    ui::subheading("The broker holds the line"),
                    ui::para(
                        "The strategy holds a token the broker only lets trade. It cannot \
                         withdraw, transfer, stake or lend, and it cannot touch any other \
                         account you hold. You can revoke it at any time without the strategy's \
                         cooperation.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("What Shonin adds"),
                    ui::para(
                        "Proof that only the committed, reviewed code holds that token, and one \
                         record across every account running the strategy that nobody can \
                         cherry-pick. What it can't add is a contract of ours between you and \
                         your broker.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("What it costs"),
                    ui::para(
                        "The same schedule a vault pays, charged to a tank on your account's \
                         entry rather than minted as shares: 0.5% a year on the account's value, \
                         10% of its gains above its own high-water mark to the author and 5% to \
                         Shonin, plus metered running costs.",
                    ),
                ]))
            }),
        ],
    )
}
