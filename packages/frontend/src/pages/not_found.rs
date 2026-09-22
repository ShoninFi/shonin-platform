//! A page that isn't there.

use dominator::{Dom, html};

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
            .child(shell::button_link("/verify", "Verify", false))
        }))
    })
}
