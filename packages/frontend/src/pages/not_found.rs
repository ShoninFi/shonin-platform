//! A page that isn't there.

use dominator::{Dom, html};

use crate::ui::primitives as ui;
use crate::ui::shell;

pub fn render() -> Dom {
    html!("div", {
        .class(["page--narrow", "stack"])
        .child(html!("h1", { .class("title").text("Page not found") }))
        .child(html!("p", {
            .class("lede")
            .text("There's no page at this address.")
        }))
        .child(html!("div", {
            .class(["row", "row--wrap"])
            .child(shell::button_link("/", "Home", true))
            .child(shell::button_link("/strategies", "Strategies", false))
            .child(shell::button_link("/accounts", "Connected accounts", false))
            .child(shell::button_link("/verify", "Verify", false))
        }))
        .child(ui::note(
            "Every page in the app has a file of its own, so a link that leads here really is a \
             link to nothing rather than a page that failed to load.",
        ))
    })
}
