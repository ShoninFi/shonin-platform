//! Tabs, which are links because each one is its own URL, and segmented
//! controls, which are buttons because they only change what is on screen.

use std::rc::Rc;

use dominator::{Dom, clone, events, html, link};
use futures_signals::signal::{Mutable, SignalExt};

/// One tab: where it goes, what it says, and whether it is the page you are on.
pub struct TabItem {
    pub path: String,
    pub label: &'static str,
    pub current: bool,
}

/// A row of tabs. Each is a real link, so it can be opened in a new window
/// and shared.
pub fn tabs(items: Vec<TabItem>, label: &'static str) -> Dom {
    html!("nav", {
        .class("tabs")
        .attr("aria-label", label)
        .children(items.into_iter().map(|item| {
            link!(item.path, {
                .class("tab")
                .apply_if(item.current, |dom| dom.attr("aria-current", "page"))
                .text(item.label)
            })
        }))
    })
}

/// A control that switches between views of the same page without navigating.
pub fn segmented<T>(options: Vec<(T, &'static str)>, selected: &Mutable<T>) -> Dom
where
    T: Clone + PartialEq + 'static,
{
    let selected = Rc::new(selected.clone());

    html!("div", {
        .class("segmented")
        .attr("role", "group")
        .children(options.into_iter().map(|(value, label)| {
            let pressed = selected.signal_ref({
                let value = value.clone();
                move |current| *current == value
            });
            html!("button", {
                .class("segmented__option")
                .attr("type", "button")
                .attr_signal("aria-pressed", pressed.map(|pressed| {
                    if pressed { "true" } else { "false" }
                }))
                .text(label)
                .event(clone!(selected => move |_: events::Click| {
                    selected.set_neq(value.clone());
                }))
            })
        }))
    })
}
