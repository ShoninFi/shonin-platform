//! What Shonin is, in plain words.

use dominator::{Dom, html};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::strategy::fee_table;
use crate::route::HANDBOOK;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
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
            vec![ui::notice(Tone::Accent, "Halt, never steal", copy::HALT_NEVER_STEAL)],
        ))
        .child(guarantees())
        .child(fees())
        .child(leaving())
        .child(review())
        .child(not_this())
        .child(handbook())
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

fn fees() -> Dom {
    // The schedule a vault created today carries, as the factory has it.
    let schedule = match data::terms() {
        Some(terms) => fee_table(&terms.fees),
        None => ui::note(copy::REACHING),
    };
    ui::section(
        "Fees",
        vec![
            schedule,
            ui::note(copy::FEE_NOTE),
            ui::note(copy::TANK_NOTE),
        ],
    )
}

fn leaving() -> Dom {
    let band = match data::terms() {
        Some(terms) => fmt::from_bps(terms.exit_band_bps),
        None => "narrow".to_string(),
    };
    ui::section("Leaving a vault", vec![ui::para(&copy::exit_note(&band))])
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

fn handbook() -> Dom {
    ui::section(
        "The handbook",
        vec![
            ui::para(copy::HANDBOOK_NOTE),
            html!("p", {
                .child(shell::offsite(HANDBOOK, "Read the handbook", None))
            }),
        ],
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
