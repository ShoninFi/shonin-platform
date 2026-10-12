//! Switch: a setting that takes effect the moment it is flipped. A button with
//! `role="switch"` and `aria-checked`; the On/Off word is always visible and
//! the whole row is the 44px target.

use futures_signals::signal::Mutable;

use crate::view::{El, el};

pub fn switch(
    title: &'static str,
    detail: Option<&'static str>,
    on: &Mutable<bool>,
    disabled: bool,
) -> El {
    let toggle = on.clone();
    let text = el("span")
        .class("sh-switch__text")
        .child(el("span").class("sh-switch__title").text(title));
    let text = match detail {
        Some(detail) => text.child(el("span").class("sh-switch__detail").text(detail)),
        None => text,
    };
    el("button")
        .attr("type", "button")
        .class("sh-switch")
        .attr("role", "switch")
        .attr_from("aria-checked", on, |on| Some(on.to_string()))
        .attr_if(disabled, "disabled", "")
        .child(text)
        .child(
            el("span")
                .class("sh-switch__state")
                .attr("aria-hidden", "true")
                .text_from(on, |on| if *on { "On".into() } else { "Off".into() }),
        )
        .child(
            el("span")
                .class("sh-switch__track")
                .attr("aria-hidden", "true")
                .child(el("span").class("sh-switch__knob")),
        )
        .hook(move |dom| {
            dom.event(move |_: dominator::events::Click| {
                toggle.replace_with(|on| !*on);
            })
        })
}
