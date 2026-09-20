//! What Shonin is, in plain words.

use dominator::{Dom, html};

use crate::copy;
use crate::pages::strategy::fee_table;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("display").text("About Shonin") }))
            .child(html!("p", { .class("lede").text(copy::PITCH) }))
            .child(html!("p", { .class("lede").text(copy::HOME_LEDE) }))
        }))
        .child(ui::section(
            "The trust bar",
            vec![
                ui::notice(Tone::Accent, "Halt, never steal", copy::HALT_NEVER_STEAL),
                ui::note(
                    "A halt stops a strategy so depositors can leave. A theft would move funds \
                     out of a vault's own accounts, trade it on something other than the \
                     committed strategy, or misstate its value so that value moves between \
                     depositors. Every lever anyone holds has to end in the first.",
                ),
            ],
        ))
        .child(guarantees())
        .child(products())
        .child(fees())
        .child(leaving())
        .child(review())
        .child(not_this())
        .child(glossary())
    })
}

fn guarantees() -> Dom {
    let rows: Vec<Row> = copy::GUARANTEES
        .iter()
        .map(|(title, body, rules_out)| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(title) }),
                ui::muted(body),
                ui::muted(rules_out),
            ])
        })
        .collect();

    ui::section(
        "What you are promised",
        vec![table(
            vec![
                Column::new("Guarantee"),
                Column::new("What it means").wide(),
                Column::new("What it rules out").wide(),
            ],
            rows,
        )],
    )
}

fn products() -> Dom {
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
        "Two products",
        vec![
            ui::para(
                "Pooled vaults are for strategies that need capital and depositors who want \
                 off-chain yield without trusting a manager. Connected accounts are for people \
                 who already hold the assets and would rather keep them where they are. \
                 Everything upstream of the money is shared: the same authors, the same \
                 source-first publishing and attested build, the same review, the same \
                 executors.",
            ),
            table(
                vec![
                    Column::new("").wide(),
                    Column::new("Pooled vault").wide(),
                    Column::new("Connected account").wide(),
                ],
                rows,
            ),
        ],
    )
}

fn fees() -> Dom {
    ui::section(
        "Fees",
        vec![
            fee_table(),
            ui::note(copy::FEE_NOTE),
            ui::note(copy::TANK_NOTE),
        ],
    )
}

fn leaving() -> Dom {
    ui::section("Leaving a vault", vec![ui::para(copy::EXIT_NOTE)])
}

fn review() -> Dom {
    ui::section("The review", vec![ui::para(copy::REVIEW_NOTE)])
}

fn not_this() -> Dom {
    ui::section(
        "What Shonin is not",
        vec![html!("div", {
            .class(["grid", "grid--2"])
            .children(copy::NOT_THIS.iter().map(|(title, body)| {
                ui::card(vec![ui::subheading(title), ui::para(body)])
            }))
        })],
    )
}

fn glossary() -> Dom {
    let rows: Vec<Row> = copy::TERMS
        .iter()
        .map(|(term, definition)| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(term) }),
                ui::muted(definition),
            ])
        })
        .collect();

    ui::section(
        "Words used here",
        vec![table(
            vec![Column::new("Term"), Column::new("What it means").wide()],
            rows,
        )],
    )
}
