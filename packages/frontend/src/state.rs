//! What the app remembers between visits, in the browser and nowhere else.

use std::sync::OnceLock;

use futures_signals::signal::Mutable;

use crate::ui::overlay::Toasts;
use crate::ui::primitives::Tone;

/// Which theme the viewer has chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

/// The app's state. One instance, built on first use.
pub struct State {
    pub theme: Mutable<Theme>,
    /// What the app has to say about something that just happened.
    pub toasts: Toasts,
}

static STATE: OnceLock<State> = OnceLock::new();

pub fn get() -> &'static State {
    STATE.get_or_init(|| State {
        theme: Mutable::new(
            storage_get("theme")
                .as_deref()
                .and_then(Theme::from_key)
                .unwrap_or(Theme::Dark),
        ),
        toasts: Toasts::new(),
    })
}

/// Say something happened, in the corner of the screen.
pub fn notify(message: impl Into<String>, tone: Tone) {
    get().toasts.push(message, tone);
}

/// Switch themes, remember it, and apply it to the document.
pub fn toggle_theme() {
    let theme = get().theme.get().other();
    get().theme.set(theme);
    storage_set("theme", theme.key());
    apply_theme(theme);
}

/// Put the chosen theme on the document, where the stylesheet reads it.
pub fn apply_theme(theme: Theme) {
    let root = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element());
    if let Some(root) = root {
        let _ = root.set_attribute("data-theme", theme.key());
    }
}

// Storage is a convenience, and every access can fail: a private window, a
// blocked origin, a viewer who cleared their site data. Nothing the app shows
// depends on it working.

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn storage_get(key: &str) -> Option<String> {
    storage()?.get_item(&format!("shonin.{key}")).ok().flatten()
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(&format!("shonin.{key}"), value);
    }
}

pub fn storage_remove(key: &str) {
    if let Some(storage) = storage() {
        let _ = storage.remove_item(&format!("shonin.{key}"));
    }
}
