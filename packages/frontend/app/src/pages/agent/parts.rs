//! Small pieces the agent page's tabs share: a card with its heading, a
//! link out to a chain's explorer, a hash with its explorer link, the "can
//! lose money" line.

use shared::Chain;
use ui::icons::Icon;

use crate::atoms::hash::hash;
use crate::atoms::icon;
use crate::store;
use crate::view::{El, View, el};

/// A card for one topic: a heading (with an optional icon), then its body.
pub fn card(id: &'static str, title: &'static str, glyph: Option<Icon>) -> El {
    let heading = el("h2").class("ag-card__title").attr("id", id);
    let heading = match glyph {
        Some(glyph) => heading.child(icon(glyph, 22)),
        None => heading,
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .attr("aria-labelledby", id)
        .child(heading.text(title))
}

/// A card's heading row: the heading on the left, something on the right.
pub fn card_with_aside(
    id: &'static str,
    title: &'static str,
    glyph: Option<Icon>,
    aside: impl Into<View>,
) -> El {
    let heading = el("h2").class("ag-card__title").attr("id", id);
    let heading = match glyph {
        Some(glyph) => heading.child(icon(glyph, 22)),
        None => heading,
    };
    el("section")
        .class("sh-card")
        .class("ag-card")
        .attr("aria-labelledby", id)
        .child(
            el("div")
                .class("ag-card__head")
                .child(heading.text(title))
                .child(aside.into()),
        )
}

/// A standalone link with its arrow.
pub fn more(href: impl Into<String>, words: &'static str) -> El {
    el("a")
        .class("sh-link")
        .attr("href", href.into())
        .text(words)
        .child(icon(Icon::ArrowRight, 18))
}

/// A standalone link inside the app.
pub fn more_in_app(href: impl Into<String>, words: &'static str) -> El {
    crate::web::app_link(href)
        .class("sh-link")
        .text(words)
        .child(icon(Icon::ArrowRight, 18))
}

/// Which network a vault chain is on in this snapshot (`evm:84532`).
pub fn network(chain: Chain) -> Option<String> {
    store::get()
        .chains
        .lock_ref()
        .iter()
        .find(|read| read.chain == chain)
        .map(|read| read.key.clone())
}

/// A transaction on its chain's explorer, opening in a new tab, or the hash
/// alone where the chain has no explorer (a local chain).
pub fn tx_link(chain: Chain, reference: &str, label: &'static str) -> El {
    tx_link_said(chain, reference, label, "")
}

/// [`tx_link`], with `prefix` ("tx ") before the short hash.
pub fn tx_link_said(chain: Chain, reference: &str, label: &'static str, prefix: &str) -> El {
    match network(chain).and_then(|key| super::facts::tx_url(&key, reference)) {
        // The name starts with the short hash shown, then the full words.
        Some(url) => el("a")
            .class("ag-tx")
            .attr("href", url)
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .attr(
                "aria-label",
                format!(
                    "{prefix}{}, {label} {reference} (opens the block explorer)",
                    crate::format::short_hash(reference)
                ),
            )
            .child(
                el("span")
                    .class("sh-mono-s")
                    .attr("aria-hidden", "true")
                    .text(format!("{prefix}{}", crate::format::short_hash(reference))),
            )
            .child(icon(Icon::External, 14)),
        None => el("span")
            .class("ag-tx")
            .class("sh-mono-s")
            .attr("title", reference.to_string())
            .text(format!("{prefix}{}", crate::format::short_hash(reference))),
    }
}

/// An address on its chain's explorer, or nothing where there is none.
pub fn address_url(chain: Chain, address: &str) -> Option<String> {
    network(chain).and_then(|key| super::facts::address_url(&key, address))
}

/// A hash chip with a copy button.
pub fn chip(full: &str, what: &'static str) -> El {
    hash(full, None, what)
}

/// The line that always sits by a money action.
/// A table that may scroll sideways on a narrow screen: a named region the
/// keyboard can reach and scroll.
pub fn scroll(label: &'static str) -> El {
    el("div")
        .class("ag-scroll")
        .attr("role", "region")
        .attr("aria-label", label)
        .attr("tabindex", "0")
}

pub fn risk_line() -> El {
    el("p")
        .class("ag-risk")
        .child(icon(Icon::Concern, 16))
        .text("The agent can lose money. Nothing here stops that.")
}

/// A link to a place on this page (`#id`). Without scripts the browser
/// follows it; in the app it scrolls there and moves focus without asking
/// the router, which would otherwise draw the page again for a new hash.
pub fn in_page_link(id: impl Into<String>) -> El {
    let id = id.into();
    let target = id.clone();
    el("a").attr("href", format!("#{id}")).hook(move |dom| {
        dom.event_with_options(
            &dominator::EventOptions::preventable(),
            move |event: dominator::events::Click| {
                if event.ctrl_key() || event.shift_key() || event.alt_key() {
                    return;
                }
                event.prevent_default();
                go_to(&target);
            },
        )
    })
}

/// Scroll to the element `id` on this page, focus it and put it in the
/// address (replacing the entry, so Back still leaves the page).
pub fn go_to(id: &str) {
    use wasm_bindgen::JsCast;
    let Some(document) = crate::web::document() else {
        return;
    };
    replace_hash(id);
    if let Some(element) = document
        .get_element_by_id(id)
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        if !element.has_attribute("tabindex") {
            let _ = element.set_attribute("tabindex", "-1");
        }
        element.scroll_into_view();
        let _ = element.focus();
    }
}

/// Put `#id` in the address without a navigation.
pub fn replace_hash(id: &str) {
    if let Some(window) = crate::web::window()
        && let Ok(history) = window.history()
    {
        let _ = history.replace_state_with_url(
            &wasm_bindgen::JsValue::NULL,
            "",
            Some(&format!("#{id}")),
        );
    }
}

/// A button that copies `value` and says so for a moment.
pub fn copy_button(value: String, what: &'static str) -> El {
    use futures_signals::signal::Mutable;
    let copied = Mutable::new(false);
    el("button")
        .attr("type", "button")
        .class("sh-btn")
        .class("sh-btn-secondary")
        .class("sh-btn-sm")
        .class("ag-copy")
        .attr("aria-label", format!("Copy {what}"))
        .child_from(&copied, |copied| {
            Some(icon(if *copied { Icon::Check } else { Icon::Copy }, 16).into())
        })
        .child(
            el("span")
                .attr("aria-hidden", "true")
                .text_from(&copied, |copied| {
                    if *copied { "Copied" } else { "Copy" }.to_string()
                }),
        )
        .child(
            el("span")
                .class("sh-vh")
                .attr("role", "status")
                .text_from(&copied, |copied| {
                    if *copied {
                        "Copied".into()
                    } else {
                        String::new()
                    }
                }),
        )
        .hook({
            let copied = copied.clone();
            move |dom| {
                dom.event(move |_: dominator::events::Click| {
                    crate::web::copy(&value);
                    copied.set(true);
                    let copied = copied.clone();
                    gloo_timers::callback::Timeout::new(1_500, move || copied.set(false)).forget();
                })
            }
        })
}

/// Whether `hash` is a store hash: `sha256:` and 64 lowercase hex digits,
/// as the store names every file.
pub fn is_store_hash(hash: &str) -> bool {
    hash.strip_prefix("sha256:").is_some_and(|digits| {
        digits.len() == 64
            && digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Where the API's store serves a file, as an address a reader can use
/// anywhere: the API's own origin when the build names it, else this page's
/// (a development build, whose server passes `/api` on). None for a hash
/// that isn't a store hash, so nothing else is ever put into the address.
pub fn store_url(hash: &str, name: &str) -> Option<String> {
    if !is_store_hash(hash) {
        return None;
    }
    let base = crate::api::BASE.trim_end_matches('/');
    let base = if crate::api::origin(base).is_some() || !cfg!(target_arch = "wasm32") {
        base.to_string()
    } else {
        let here = crate::web::window()
            .and_then(|window| js_sys::Reflect::get(&window, &"location".into()).ok())
            .and_then(|location| js_sys::Reflect::get(&location, &"origin".into()).ok())
            .and_then(|origin| origin.as_string())
            .unwrap_or_default();
        format!("{here}{base}")
    };
    Some(format!("{base}/store/{hash}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_address_is_made_only_from_a_well_formed_store_hash() {
        let hash = format!("sha256:{}", "0123456789abcdef".repeat(4));
        let url = store_url(&hash, "source.tar").expect("a store hash");
        assert!(url.ends_with(&format!("/store/{hash}/source.tar")), "{url}");
        for bad in [
            String::new(),
            "0123456789abcdef".repeat(4),
            format!("sha256:{}", "0123456789ABCDEF".repeat(4)),
            format!("sha256:{}", "0123456789abcdef".repeat(3)),
            format!("sha256:{}0", "0123456789abcdef".repeat(4)),
            format!("sha256:{}/../x", "0123456789abcdef".repeat(4)),
            format!("sha256:../../{}", "0123456789abcdef".repeat(4)),
            format!("sha256:{}?a", &"0123456789abcdef".repeat(4)[1..]),
        ] {
            assert_eq!(store_url(&bad, "source.tar"), None, "{bad}");
        }
    }
}
