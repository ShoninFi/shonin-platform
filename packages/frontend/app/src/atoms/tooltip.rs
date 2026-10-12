//! Tooltip: a few words about a control, shown on hover and on keyboard focus,
//! hidden with Escape. The control points at it with `aria-describedby`.

use futures_signals::signal::Mutable;

use crate::view::{El, el};

/// Wrap `trigger` (which must take focus) with a tooltip saying `words`.
/// `id` must be unique on the page.
pub fn tooltip(trigger: El, words: &'static str, id: &'static str) -> El {
    let shown = Mutable::new(false);
    let show = |shown: &Mutable<bool>, value: bool| {
        let shown = shown.clone();
        move || shown.set_neq(value)
    };
    let (enter, leave, focus_in, focus_out) = (
        show(&shown, true),
        show(&shown, false),
        show(&shown, true),
        show(&shown, false),
    );
    let escape = shown.clone();
    el("span")
        .class("sh-tip-anchor")
        .child(trigger.attr("aria-describedby", id))
        .child(
            el("span")
                .class("sh-tip")
                .class("sh-tip--above")
                .attr("role", "tooltip")
                .attr("id", id)
                .attr_from("hidden", &shown, |shown| (!shown).then(String::new))
                .text(words),
        )
        .hook(move |dom| {
            dom.event(move |_: dominator::events::MouseEnter| enter())
                .event(move |_: dominator::events::MouseLeave| leave())
                .event(move |_: dominator::events::FocusIn| focus_in())
                .event(move |_: dominator::events::FocusOut| focus_out())
                .event(move |event: dominator::events::KeyDown| {
                    if event.key() == "Escape" {
                        escape.set_neq(false);
                    }
                })
        })
}
