//! The few things only a browser has: the clock, storage, the clipboard,
//! focus and navigation. Everything here is only ever called from the app's
//! event handlers and timers, never from the pre-render.

use wasm_bindgen::JsCast;

pub fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

pub fn document() -> Option<web_sys::Document> {
    window()?.document()
}

/// Milliseconds since the Unix epoch on the reader's clock.
pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

// Storage is a convenience, and every access can fail: a private window, a
// blocked origin, cleared site data. Nothing the app shows depends on it.

fn storage() -> Option<web_sys::Storage> {
    window()?.local_storage().ok().flatten()
}

pub fn storage_get(key: &str) -> Option<String> {
    storage()?.get_item(&format!("shonin.{key}")).ok().flatten()
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(&format!("shonin.{key}"), value);
    }
}

/// Put text on the clipboard.
pub fn copy(text: &str) {
    if let Some(window) = window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

/// The element that has focus now, to give it back later.
pub fn focused() -> Option<web_sys::HtmlElement> {
    document()?.active_element()?.dyn_into().ok()
}

pub fn focus(element: &web_sys::HtmlElement) {
    let _ = element.focus();
}

/// Every element inside `root` that a Tab reaches, in order.
pub fn focusables(root: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    const SELECTOR: &str = "a[href], button:not([disabled]), input:not([disabled]), \
        select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";
    let Ok(list) = root.query_selector_all(SELECTOR) else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|element| {
            element.offset_parent().is_some() || element.get_client_rects().length() > 0
        })
        .collect()
}

/// The window's width in CSS pixels.
pub fn width() -> f64 {
    window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(1440.0)
}

/// Mark the app's own content inert (or not) while a dialog is open.
pub fn set_inert(inert: bool) {
    let Some(document) = document() else {
        return;
    };
    if let Some(app) = document.get_element_by_id("app") {
        if inert {
            let _ = app.set_attribute("inert", "");
        } else {
            let _ = app.remove_attribute("inert");
        }
    }
    if let Some(body) = document.body() {
        let _ = if inert {
            body.class_list().add_1("sh-inert")
        } else {
            body.class_list().remove_1("sh-inert")
        };
    }
}

/// Follow a link inside the app without reloading, unless the reader asked
/// for a new tab or window.
pub fn internal_link(el: crate::view::El, href: String) -> crate::view::El {
    el.hook(move |dom| {
        dom.event_with_options(
            &dominator::EventOptions::preventable(),
            move |event: dominator::events::Click| {
                if event.button() == dominator::events::MouseButton::Left
                    && !event.ctrl_key()
                    && !event.shift_key()
                    && !event.alt_key()
                {
                    event.prevent_default();
                    dominator::routing::go_to_url(&href);
                }
            },
        )
    })
}

/// A link to a page of this app.
pub fn app_link(href: impl Into<String>) -> crate::view::El {
    let href = href.into();
    internal_link(crate::view::el("a").attr("href", href.clone()), href)
}
