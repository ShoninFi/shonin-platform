//! Talking to the platform's API.
//!
//! The app asks for a snapshot every 5 seconds while the tab is visible,
//! pauses while it is hidden, and backs off when the API doesn't answer. Each
//! snapshot is diffed into the store ([`crate::store`]), so only what changed
//! is redrawn. The build-time pre-render reads one snapshot the same way,
//! natively.

/// Where the API answers. A release build names the API's own address (the
/// build task sets `SHONIN_APP_API` from the public config); the dev server
/// proxies the relative path to the API it was started with.
pub const BASE: &str = match option_env!("SHONIN_APP_API") {
    Some(base) => base,
    None => "/api",
};

/// How often to ask for a newer snapshot while the page is visible.
pub const EVERY_MILLIS: u32 = 5_000;
/// The longest wait between asks while the API isn't answering.
pub const MAX_BACKOFF_MILLIS: u32 = 60_000;

/// How long to wait before the next ask, after `failures` failed asks in a
/// row: 5 s, then 10, 20, 40, and never more than a minute.
pub fn next_delay(failures: u32) -> u32 {
    if failures == 0 {
        return EVERY_MILLIS;
    }
    EVERY_MILLIS
        .saturating_mul(1u32 << failures.min(8))
        .min(MAX_BACKOFF_MILLIS)
}

/// The origin (scheme, host and port) of an absolute URL, or `None` for a
/// relative one.
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

#[cfg(target_arch = "wasm32")]
pub use browser::follow;

/// Nothing to follow without a browser: the pre-render reads one snapshot.
#[cfg(not(target_arch = "wasm32"))]
pub fn follow() {}

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::collections::BTreeSet;

    use futures_signals::signal::{Mutable, SignalExt};
    use gloo_timers::future::TimeoutFuture;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use super::{BASE, next_delay};
    use crate::store;

    /// Keep the store up to date for as long as the page is open.
    pub fn follow() {
        let visible = Mutable::new(is_visible());
        watch_visibility(visible.clone());
        let store = store::get();
        store.sync.started.set(js_sys::Date::now());

        wasm_bindgen_futures::spawn_local(async move {
            let mut failures = 0u32;
            let mut configured = BTreeSet::new();
            loop {
                // Nothing is fetched while nobody can see the page.
                visible.signal().wait_for(true).await;
                match fetch::<shared::Snapshot>("snapshot").await {
                    Ok(snapshot) => {
                        failures = 0;
                        store.apply(&snapshot);
                        crate::wallet::get().take_snapshot(&snapshot);
                        store.sync.last_ok.set(Some(js_sys::Date::now()));
                        store.sync.failing.set_neq(false);
                        store.sync.error.set_neq(None);
                        for info in &snapshot.chains {
                            if configured.insert(info.chain) {
                                let store = store.clone();
                                let chain = info.chain;
                                wasm_bindgen_futures::spawn_local(async move {
                                    if let Ok(config) =
                                        fetch::<shared::Config>(&format!("{chain}/config")).await
                                    {
                                        store.apply_config(&config);
                                    }
                                });
                            }
                        }
                    }
                    Err(why) => {
                        failures = failures.saturating_add(1);
                        store.sync.failing.set_neq(true);
                        store.sync.error.set_neq(Some(why));
                    }
                }
                // Wait out the delay, or less if a reader asks to try again.
                let asked = store.sync.retry.get();
                let mut waited = 0;
                let delay = next_delay(failures);
                while waited < delay && store.sync.retry.get() == asked {
                    TimeoutFuture::new(250).await;
                    waited += 250;
                }
            }
        });
    }

    fn document() -> Option<web_sys::Document> {
        web_sys::window()?.document()
    }

    fn is_visible() -> bool {
        document()
            .is_none_or(|document| document.visibility_state() == web_sys::VisibilityState::Visible)
    }

    fn watch_visibility(visible: Mutable<bool>) {
        let Some(document) = document() else {
            return;
        };
        let listener = Closure::<dyn FnMut()>::new(move || visible.set_neq(is_visible()));
        let _ = document.add_event_listener_with_callback(
            "visibilitychange",
            listener.as_ref().unchecked_ref(),
        );
        // The page listens for as long as it is open.
        listener.forget();
    }

    async fn fetch<T: serde::de::DeserializeOwned>(what: &str) -> Result<T, String> {
        let reply = gloo_net::http::Request::get(&format!("{BASE}/{what}"))
            .send()
            .await
            .map_err(|error| format!("the API did not answer: {error}"))?;
        if !reply.ok() {
            return Err(format!("the API answered {}", reply.status()));
        }
        reply
            .json()
            .await
            .map_err(|error| format!("the API said something unexpected: {error}"))
    }
}

/// Read a snapshot without a browser, for the build-time pre-render.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_snapshot(api: &str) -> Result<shared::Snapshot, String> {
    blocking(api, "snapshot")
}

/// A chain's configuration, for the same pre-render.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_config(api: &str, chain: shared::Chain) -> Result<shared::Config, String> {
    blocking(api, &format!("{chain}/config"))
}

#[cfg(not(target_arch = "wasm32"))]
fn blocking<T: serde::de::DeserializeOwned>(api: &str, path: &str) -> Result<T, String> {
    let url = format!("{}/{path}", api.trim_end_matches('/'));
    let reply =
        reqwest::blocking::get(&url).map_err(|error| format!("{url} did not answer: {error}"))?;
    if !reply.status().is_success() {
        return Err(format!("{url} answered {}", reply.status()));
    }
    reply
        .json()
        .map_err(|error| format!("{url} said something unexpected: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_backs_off_while_the_api_is_down_and_never_waits_more_than_a_minute() {
        assert_eq!(next_delay(0), 5_000);
        assert_eq!(next_delay(1), 10_000);
        assert_eq!(next_delay(2), 20_000);
        assert_eq!(next_delay(3), 40_000);
        assert_eq!(next_delay(4), 60_000);
        assert_eq!(next_delay(40), 60_000);
    }

    #[test]
    fn an_origin_is_the_scheme_and_host_only() {
        assert_eq!(
            origin("https://testnet-api.shonin.fi/api").as_deref(),
            Some("https://testnet-api.shonin.fi")
        );
        assert_eq!(
            origin("http://localhost:19201/api/").as_deref(),
            Some("http://localhost:19201")
        );
        assert_eq!(origin("/api"), None);
    }
}
