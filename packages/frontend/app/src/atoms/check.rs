//! Checkbox and radio: a real input under the drawn box, with a visible
//! label; the whole row is the 44px target.

use futures_signals::signal::Mutable;
use ui::icons::Icon;

use super::icon::icon;
use crate::view::{El, el};

/// A checkbox bound to `checked`.
pub fn checkbox(id: String, label: impl Into<String>, checked: &Mutable<bool>) -> El {
    let toggle = checked.clone();
    el("label")
        .class("sh-check")
        .attr("for", id.clone())
        .child(
            el("input")
                .attr("type", "checkbox")
                .attr("id", id)
                .attr_from("checked", checked, |on| on.then(String::new))
                .hook(move |dom| {
                    dominator::with_node!(dom, input => {
                        .prop_signal("checked", toggle.signal())
                        .event(move |_: dominator::events::Change| {
                            toggle.set_neq(wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input)
                                .is_some_and(web_sys::HtmlInputElement::checked));
                        })
                    })
                }),
        )
        .child(el("span").class("sh-check__box").child(icon(Icon::Check, 14)))
        .child(el("span").text(label.into()))
}

/// One radio in a group named `name`, chosen while `selected` is `value`.
pub fn radio<T: Copy + PartialEq + 'static>(
    name: &'static str,
    id: String,
    label: impl Into<String>,
    value: T,
    selected: &Mutable<T>,
) -> El {
    let choose = selected.clone();
    let checked = selected.clone();
    el("label")
        .class("sh-check")
        .class("sh-check--radio")
        .attr("for", id.clone())
        .child(
            el("input")
                .attr("type", "radio")
                .attr("name", name)
                .attr("id", id)
                .attr_from("checked", selected, move |current| {
                    (*current == value).then(String::new)
                })
                .hook(move |dom| {
                    dom.prop_signal(
                        "checked",
                        futures_signals::signal::SignalExt::map(checked.signal(), move |current| {
                            current == value
                        }),
                    )
                    .event(move |_: dominator::events::Change| choose.set_neq(value))
                }),
        )
        .child(el("span").class("sh-check__box"))
        .child(el("span").text(label.into()))
}
