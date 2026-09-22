//! The health report: how a strategy is running, each signal against the
//! threshold that turns it into a warning or an error.

use dominator::{Dom, html};
use shared::{HealthLevel, Strategy};

use crate::data;
use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;

pub fn render(strategy: &'static Strategy) -> Dom {
    let Some(health) = &strategy.health else {
        return ui::section(
            "Nothing is running",
            vec![ui::empty(
                "This strategy isn't running, so there is no health report for it.",
            )],
        );
    };

    let level = health.level();

    html!("div", {
        .class("stack-lg")
        .child(ui::section(
            "How it is running",
            vec![
                html!("div", {
                    .class(["row", "row--wrap"])
                    .child(ui::badge_dot(level.label(), Tone::from_level(level)))
                    .child(ui::muted(&health.summary()))
                }),
                ui::kv(vec![
                    (
                        "Last report".to_string(),
                        ui::value(&format!(
                            "{} · {}",
                            fmt::ago(health.updated, data::now()),
                            fmt::timestamp(health.updated)
                        )),
                    ),
                    ("Signed by".to_string(), ui::address(&health.signed_by)),
                ]),
                ui::note(
                    "Signed by the executor running the strategy. It is evidence of how the strategy is \
                     running, not the record of what it did; nothing here stops anything on its \
                     own.",
                ),
            ],
        ))
        .child(signals(health))
        .child(entries(health))
        .child(consequences(strategy, level))
    })
}

fn signals(health: &'static shared::Health) -> Dom {
    let cards: Vec<Dom> = health
        .signals
        .iter()
        .map(|signal| {
            let level = signal.level();
            let fraction = if signal.lower_is_worse {
                1.0 - (signal.value / signal.warn_at.max(f64::MIN_POSITIVE)).clamp(0.0, 1.0)
            } else {
                (signal.value / signal.error_at.max(f64::MIN_POSITIVE)).clamp(0.0, 1.0)
            };

            ui::card(vec![
                html!("div", {
                    .class(["row", "row--between", "row--wrap", "row--top"])
                    .child(html!("div", {
                        .child(html!("div", { .class("stat__label").text(&signal.name) }))
                        .child(html!("div", {
                            .class("stat__value")
                            .class("stat__value--sm")
                            .apply(|dom| match level {
                                HealthLevel::Error => dom.class("down"),
                                HealthLevel::Warning => dom.style("color", "var(--warn)"),
                                HealthLevel::Ok => dom,
                            })
                            .text(&fmt::number(signal.value, 1))
                        }))
                        .child(html!("div", { .class("stat__note").text(&signal.unit) }))
                    }))
                    .child(ui::badge(level.label(), Tone::from_level(level)))
                }),
                ui::meter(
                    fraction,
                    match level {
                        HealthLevel::Error => Tone::Error,
                        HealthLevel::Warning => Tone::Warn,
                        HealthLevel::Ok => Tone::Accent,
                    },
                ),
                html!("div", {
                    .class(["faint", "small", "num"])
                    .text(&format!(
                        "warning at {} · error at {} · over {}",
                        fmt::number(signal.warn_at, 1),
                        fmt::number(signal.error_at, 1),
                        signal.window
                    ))
                }),
                match &signal.note {
                    Some(note) => ui::note(note),
                    None => html!("span"),
                },
            ])
        })
        .collect();

    ui::section(
        "Signals",
        vec![html!("div", { .class(["grid", "grid--2"]).children(cards) })],
    )
}

fn entries(health: &'static shared::Health) -> Dom {
    let items: Vec<Dom> = health
        .entries
        .iter()
        .map(|entry| {
            html!("li", {
                .class("timeline__item")
                .child(html!("span", {
                    .class("timeline__when")
                    .text(&fmt::ago(entry.at, data::now()))
                }))
                .child(html!("div", {
                    .class(["row", "row--top"])
                    .child(ui::badge(entry.level.label(), Tone::from_level(entry.level)))
                    .child(html!("span", { .class("grow").text(&entry.message) }))
                }))
            })
        })
        .collect();

    ui::section(
        "Recently",
        vec![html!("ul", { .class("timeline").children(items) })],
    )
}

fn consequences(strategy: &'static Strategy, level: HealthLevel) -> Dom {
    let mut children = vec![ui::para(
        "A health report informs; it never gates. Deposits and withdrawals carry on whatever \
         it says.",
    )];

    if level != HealthLevel::Ok {
        children.push(ui::notice(
            Tone::Warn,
            "What happens if this doesn't recover",
            "The admin key can kill a vault by hand. Every position is closed reduce-only inside \
             the exit band and the cash brought home, so every withdrawal afterwards is paid in \
             one transaction.",
        ));
    }

    if !strategy.vault.claims.is_empty() {
        children.push(ui::notice(
            Tone::Info,
            "A claim is outstanding",
            &format!(
                "An unserved claim freezes this vault after {} days. Until then settlement is retried \
                 rather than filled at any price.",
                strategy.vault.freeze_after_days
            ),
        ));
    }

    ui::section("What this means", children)
}
