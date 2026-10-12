//! The first-visit strip on /agents, for people arriving from outside: one
//! line on what makes an agent checkable and a link to how it works. Dismissed
//! once, it never returns (remembered in this browser).

use futures_signals::signal::Mutable;
use ui::icons::Icon;

use crate::atoms::{icon, icon_button};
use crate::route::SITE_HOW;
use crate::view::{View, el};

const KEY: &str = "first-visit-strip";

/// The strip, unless this browser dismissed it. `remembered` is whether
/// storage says so (false in the pre-render: there is no storage there).
pub fn strip(remembered: bool) -> View {
    let shown = Mutable::new(!remembered);
    let hide = shown.clone();
    el("div")
        .class("sh-contents")
        .child_from(&shown, move |shown| {
            let hide = hide.clone();
            shown.then(|| {
                el("aside")
                    .class("sh-strip")
                    .attr("aria-label", "First visit")
                    .child(
                        el("div")
                            .class("sh-container")
                            .class("sh-strip__inner")
                            .child(icon(Icon::Info, 20))
                            .child(
                                el("p")
                                    .text("New to Shonin? Every agent here is reviewed, runs in an attested machine and trades with keys that can’t withdraw your money.")
                                    .child(el("a").attr("href", SITE_HOW).text("How it works")),
                            )
                            .child(icon_button(Icon::Close, "Dismiss").hook(move |dom| {
                                dom.event(move |_: dominator::events::Click| {
                                    crate::web::storage_set(KEY, "dismissed");
                                    hide.set(false);
                                    // Focus would otherwise fall to the top of
                                    // the page; the list is what comes next.
                                    if let Some(main) = crate::web::document()
                                        .and_then(|document| document.get_element_by_id("content"))
                                        .and_then(|main| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(main).ok())
                                    {
                                        crate::web::focus(&main);
                                    }
                                })
                            })),
                    )
                    .into()
            })
        })
        .into()
}

/// Whether this browser dismissed the strip.
pub fn dismissed() -> bool {
    crate::web::storage_get(KEY).is_some()
}
