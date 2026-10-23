//! Segmented control: one of a few views of the same data (a chart's range,
//! price or money in vault). Buttons with `aria-pressed` in a labelled group.

use std::borrow::Cow;

use futures_signals::signal::Mutable;

use crate::view::{El, el};

/// A segmented control over `options`, bound to `selected`. A label may be
/// fixed words or read at build time: a vault's asset, say.
pub fn segmented<T: Copy + PartialEq + 'static, W: Clone + Into<Cow<'static, str>>>(
    label: &'static str,
    options: &[(T, W)],
    selected: &Mutable<T>,
) -> El {
    let set = selected.clone();
    segmented_by(label, options, selected, move |value| set.set_neq(value))
}

/// The same, where a pick goes through `pick` rather than straight to
/// `selected`: what else must change with it changes in the same step.
pub fn segmented_by<T: Copy + PartialEq + 'static, W: Clone + Into<Cow<'static, str>>>(
    label: &'static str,
    options: &[(T, W)],
    selected: &Mutable<T>,
    pick: impl Fn(T) + Clone + 'static,
) -> El {
    el("div")
        .class("sh-seg")
        .attr("role", "group")
        .attr("aria-label", label)
        .children(options.iter().map(|(value, words)| {
            let value = *value;
            let pick = pick.clone();
            el("button")
                .attr("type", "button")
                .attr_from("aria-pressed", selected, move |current| {
                    Some((*current == value).to_string())
                })
                .text(words.clone())
                .hook(move |dom| dom.event(move |_: dominator::events::Click| pick(value)))
        }))
}
