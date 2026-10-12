//! Segmented control: one of a few views of the same data (a chart's range,
//! price or money in vault). Buttons with `aria-pressed` in a labelled group.

use futures_signals::signal::Mutable;

use crate::view::{El, el};

/// A segmented control over `options`, bound to `selected`.
pub fn segmented<T: Copy + PartialEq + 'static>(
    label: &'static str,
    options: &[(T, &'static str)],
    selected: &Mutable<T>,
) -> El {
    el("div")
        .class("sh-seg")
        .attr("role", "group")
        .attr("aria-label", label)
        .children(options.iter().map(|(value, words)| {
            let value = *value;
            let selected = selected.clone();
            el("button")
                .attr("type", "button")
                .attr_from("aria-pressed", &selected, move |current| {
                    Some((*current == value).to_string())
                })
                .text(*words)
                .hook(move |dom| {
                    dom.event(move |_: dominator::events::Click| selected.set_neq(value))
                })
        }))
}
