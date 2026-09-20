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
            .child(ui::note(
                "Nothing here is a promise that a vault will never be halted. It is the record \
                 of when one was, and why — because a halt is the one thing anyone running this \
                 platform can always do, and the only honest answer is to say so out loud.",
            ))
        }))
        .child(html!("div", {
            .class("stack")
            .children(data::get().feed.iter().map(entry))
        }))
        .child(ui::section(
            "What can be done to a running vault",
            vec![html!("div", {
                .class(["grid", "grid--3"])
                .child(ui::card(vec![
                    ui::subheading("A kill"),
                    ui::para(
                        "The admin key can stop any vault. The strategy gets a short grace \
                         period to unwind itself, then every position is closed reduce-only \
                         inside the exit band and the cash brought home. It moves no funds \
                         anywhere but back to depositors.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("An image change"),
                    ui::para(
                        "A new executor image is proposed publicly and can only be blessed after \
                         a two-day timelock — long enough for anyone to rebuild it from source \
                         and object. Revoking one is instant, and never hands a vault's keys to \
                         another runtime.",
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("A freeze"),
                    ui::para(
                        "A claim that goes unserved for seven days freezes its vault: no new \
                         deposits, and the home chain's trading approvals revoked so that only \
                         unwinding remains possible there.",
                    ),
                ]))
            })],
        ))
    })
}

fn entry(event: &'static FeedEvent) -> Dom {
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
