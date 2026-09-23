//! The public feed: kills with their reasons, allowlist changes, venues that
//! stopped behaving, and vaults that froze.

use dominator::{Dom, html};
use shared::FeedEvent;

use crate::data;
use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(html!("div", {
            .class("stack")
            .child(html!("h1", { .class("title").text("Status") }))
            .child(html!("p", {
                .class("lede")
                .text(
                    "Every kill with its reason, every change to the image allowlist, and every \
                     venue that stopped behaving."
                )
            }))
        }))
        .child(html!("div", {
            .class("stack")
            .children(data::feed().iter().map(|event| entry(event)))
        }))
        .child(ui::section(
            "What can be done to a running vault",
            vec![html!("div", {
                .class(["grid", "grid--3"])
                .child(ui::card(vec![
                    ui::subheading("A kill"),
                    ui::para(
                        "The admin key can stop any vault. Every position is closed reduce-only inside the \
                         exit band and the cash brought home, to depositors and nowhere else.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("An image change"),
                    ui::para(&match data::terms() {
                        Some(terms) => format!(
                            "A new executor image is proposed publicly and blessed only after a \
                             timelock of {}. Revoking one is instant.",
                            fmt::day_count(terms.image_timelock_days)
                        ),
                        None => "A new executor image is proposed publicly and blessed only after \
                                 a timelock. Revoking one is instant."
                            .to_string(),
                    }),
                ]))
                .child(ui::card(vec![
                    ui::subheading("A freeze"),
                    ui::para(&match data::terms() {
                        Some(terms) => format!(
                            "A claim that goes unserved for {} freezes its vault: no new \
                             deposits, and only unwinding remains possible.",
                            fmt::day_count(terms.freeze_after_days)
                        ),
                        None => "A claim that goes unserved past the freeze period freezes its \
                                 vault: no new deposits, and only unwinding remains possible."
                            .to_string(),
                    }),
                ]))
            })],
        ))
    })
}

fn entry(event: &FeedEvent) -> Dom {
    let tone = if event.kind.is_alarming() {
        Tone::Warn
    } else {
        Tone::Neutral
    };

    ui::card(vec![
        html!("div", {
            .class(["row", "row--between", "row--wrap", "row--top"])
            .child(html!("h2", { .class("subtitle").text(&event.title) }))
            .child(ui::badge(event.kind.label(), tone))
        }),
        html!("div", {
            .class(["row", "row--wrap", "small", "faint"])
            .child(html!("span", { .text(&fmt::ago(event.at, data::now())) }))
            .child(html!("span", { .text(&fmt::timestamp(event.at)) }))
            .child(ui::hash(&event.reference))
        }),
        html!("p", { .text(&event.detail) }),
        match &event.link {
            Some(link) => ui::action_link(link, "Read more"),
            None => html!("span"),
        },
    ])
}
