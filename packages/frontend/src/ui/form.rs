//! Form controls, each bound to a `Mutable` the page owns.

use dominator::{Dom, EventOptions, clone, events, html, with_node};
use futures_signals::signal::{Mutable, SignalExt};
use wasm_bindgen::JsCast;

/// A labelled field wrapper: the label, the control, and a hint under it.
pub fn field(label: &str, hint: Option<&str>, control: Dom) -> Dom {
    html!("label", {
        .class("field")
        .child(html!("span", { .class("field__label").text(label) }))
        .child(control)
        .apply(|dom| match hint {
            Some(hint) => dom.child(html!("span", { .class("field__hint").text(hint) })),
            None => dom,
        })
    })
}

/// A single-line text input.
pub fn input(value: &Mutable<String>, placeholder: &str) -> Dom {
    let value = value.clone();
    html!("input" => web_sys::HtmlInputElement, {
        .class("input")
        .attr("type", "text")
        .attr("placeholder", placeholder)
        .prop_signal("value", value.signal_cloned())
        .with_node!(element => {
            .event(clone!(value => move |_: events::Input| {
                value.set_neq(element.value());
            }))
        })
    })
}

/// A multi-line input, for anything that is pasted rather than typed.
pub fn textarea(value: &Mutable<String>, placeholder: &str) -> Dom {
    let value = value.clone();
    html!("textarea" => web_sys::HtmlTextAreaElement, {
        .class("textarea")
        .attr("placeholder", placeholder)
        .prop_signal("value", value.signal_cloned())
        .with_node!(element => {
            .event(clone!(value => move |_: events::Input| {
                value.set_neq(element.value());
            }))
        })
    })
}

/// An amount, with its unit and a button that fills in everything available.
pub fn amount(value: &Mutable<String>, unit: &str, max: f64) -> Dom {
    let value = value.clone();
    let unit = unit.to_string();

    html!("div", {
        .class("amount")
        .child(html!("input" => web_sys::HtmlInputElement, {
            .class("amount__input")
            .attr("type", "text")
            .attr("inputmode", "decimal")
            .attr("placeholder", "0.00")
            .attr("aria-label", &format!("Amount in {unit}"))
            .prop_signal("value", value.signal_cloned())
            .with_node!(element => {
                .event(clone!(value => move |_: events::Input| {
                    // Only a number belongs in an amount, so anything else is
                    // dropped as it is typed rather than rejected afterwards.
                    let cleaned: String = element
                        .value()
                        .chars()
                        .filter(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    if cleaned != element.value() {
                        element.set_value(&cleaned);
                    }
                    value.set_neq(cleaned);
                }))
            })
        }))
        .child(html!("span", { .class("amount__unit").text(&unit) }))
        .child(html!("button", {
            .class(["btn", "btn--quiet", "btn--sm", "amount__max"])
            .attr("type", "button")
            .text("Max")
            .event(clone!(value => move |_: events::Click| {
                value.set_neq(format!("{max:.2}"));
            }))
        }))
    })
}

/// Read an amount field as a number.
pub fn parse_amount(value: &str) -> f64 {
    value.trim().parse::<f64>().unwrap_or(0.0)
}

/// A dropdown of fixed choices.
pub fn select(options: Vec<(String, String)>, selected: &Mutable<String>) -> Dom {
    let selected = selected.clone();
    html!("select" => web_sys::HtmlSelectElement, {
        .class("select")
        .children(options.into_iter().map(|(value, label)| {
            html!("option", {
                .attr("value", &value)
                .prop_signal("selected", selected.signal_ref({
                    let value = value.clone();
                    move |current| *current == value
                }))
                .text(&label)
            })
        }))
        .with_node!(element => {
            .event(clone!(selected => move |_: events::Change| {
                selected.set_neq(element.value());
            }))
        })
    })
}

/// A switch, for something that is either on or off.
pub fn toggle(label: &str, on: &Mutable<bool>) -> Dom {
    let on = on.clone();
    html!("button", {
        .class("toggle")
        .attr("type", "button")
        .attr_signal("aria-pressed", on.signal().map(|on| if on { "true" } else { "false" }))
        .child(html!("span", { .class("toggle__track") }))
        .child(html!("span", { .text(label) }))
        .event(clone!(on => move |_: events::Click| on.set(!on.get())))
    })
}

/// A checkbox, for a choice in a list of them.
pub fn checkbox(label: &str, checked: &Mutable<bool>) -> Dom {
    let checked = checked.clone();
    html!("label", {
        .class(["row", "small"])
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "checkbox")
            .prop_signal("checked", checked.signal())
            .with_node!(element => {
                .event(clone!(checked => move |_: events::Change| {
                    checked.set_neq(element.checked());
                }))
            })
        }))
        .child(html!("span", { .text(label) }))
    })
}

/// A place to drop a file. It reports the file's name and size; nothing is
/// uploaded anywhere.
pub fn dropzone(prompt: &str, chosen: &Mutable<Option<String>>) -> Dom {
    let chosen = chosen.clone();
    let over = Mutable::new(false);
    let prompt = prompt.to_string();

    html!("label", {
        .class("dropzone")
        .class_signal("is-over", over.signal())
        .child(html!("input" => web_sys::HtmlInputElement, {
            .class("visually-hidden")
            .attr("type", "file")
            .with_node!(element => {
                .event(clone!(chosen => move |_: events::Change| {
                    let name = element
                        .files()
                        .and_then(|files| files.get(0))
                        .map(|file| format!("{} · {:.0} KiB", file.name(), file.size() / 1024.0));
                    chosen.set(name);
                }))
            })
        }))
        .child_signal(chosen.signal_cloned().map(clone!(prompt => move |chosen| {
            Some(match chosen {
                Some(name) => html!("span", { .class("num").text(&name) }),
                None => html!("span", { .text(&prompt) }),
            })
        })))
        // Dropping a file on a page normally makes the browser open it, so
        // both of these have to be preventable.
        .apply(clone!(over => move |dom| {
            dom.event_with_options(
                &EventOptions::preventable(),
                clone!(over => move |event: events::DragOver| {
                    event.prevent_default();
                    over.set_neq(true);
                }),
            )
        }))
        .event(clone!(over => move |_: events::DragLeave| over.set_neq(false)))
        .apply(clone!(over, chosen => move |dom| {
            dom.event_with_options(
                &EventOptions::preventable(),
                move |event: events::Drop| {
                    event.prevent_default();
                    over.set_neq(false);
                    let name = event
                        .data_transfer()
                        .and_then(|transfer| transfer.files())
                        .and_then(|files| files.get(0))
                        .map(|file| format!("{} · {:.0} KiB", file.name(), file.size() / 1024.0));
                    if name.is_some() {
                        chosen.set(name);
                    }
                },
            )
        }))
    })
}

/// The value of an input element, whatever kind it is.
pub fn value_of(target: Option<web_sys::EventTarget>) -> Option<String> {
    target
        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
}
