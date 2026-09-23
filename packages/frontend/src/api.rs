//! Talking to the platform's API.
//!
//! The app asks for a snapshot, renders it, and asks again. It never holds a
//! second copy of anything: whatever the chain says is what the screen shows,
//! a couple of seconds later at worst.

#[cfg(target_arch = "wasm32")]
use crate::data;

/// Where the API answers. The dev server proxies this path to it, and a
/// deployed app is served beside it, so the same relative path works in both.
pub const BASE: &str = "/api";

/// How often to ask whether anything has changed.
#[cfg(target_arch = "wasm32")]
const EVERY_MILLIS: u32 = 2_000;

/// Fetch the configuration once, then keep fetching snapshots.
#[cfg(target_arch = "wasm32")]
pub fn follow() {
    wasm_bindgen_futures::spawn_local(async {
        match fetch::<shared::Config>("config").await {
            Ok(config) => data::set_config(config),
            Err(why) => data::set_failed(why),
        }
        // Following a link should not disconnect anyone.
        crate::wallet::resume().await;

        loop {
            match fetch::<shared::Snapshot>("snapshot").await {
                Ok(snapshot) => {
                    // Only a newer snapshot is worth re-rendering for.
                    let held = data::current().map(|dataset| dataset.version).unwrap_or(0);
                    if snapshot.version != held {
                        data::set(snapshot);
                    }
                }
                Err(why) => data::set_failed(why),
            }
            gloo_timers::future::TimeoutFuture::new(EVERY_MILLIS).await;
        }
    });
}

#[cfg(target_arch = "wasm32")]
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

/// What one address holds, read from the chain when a page needs it.
#[cfg(target_arch = "wasm32")]
pub async fn portfolio(address: &str) -> Result<shared::Portfolio, String> {
    fetch(&format!("portfolio/{address}")).await
}

/// Offer a strategy to the build service.
#[cfg(target_arch = "wasm32")]
pub async fn submit(submission: &shared::Submission) -> Result<shared::SubmissionAccepted, String> {
    let reply = gloo_net::http::Request::post(&format!("{BASE}/submissions"))
        .json(submission)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| format!("the API did not answer: {error}"))?;
    if !reply.ok() {
        let said: shared::ApiError = reply.json().await.unwrap_or(shared::ApiError {
            message: "the build service refused it".to_string(),
        });
        return Err(said.message);
    }
    reply.json().await.map_err(|error| error.to_string())
}

/// Nothing to follow without a browser: the pre-render reads one snapshot and
/// writes its pages from that.
#[cfg(not(target_arch = "wasm32"))]
pub fn follow() {}

/// The pre-render is a native tool, and the browser has its own way to fetch,
/// so these are only ever called from the one that has it.
#[cfg(target_arch = "wasm32")]
pub fn fetch_snapshot(_api: &str) -> Result<shared::Snapshot, String> {
    Err("the pre-render runs natively".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn fetch_config(_api: &str) -> Result<shared::Config, String> {
    Err("the pre-render runs natively".to_string())
}

/// Read a snapshot without a browser, for the build-time pre-render.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_snapshot(api: &str) -> Result<shared::Snapshot, String> {
    blocking(api, "snapshot")
}

/// The platform's own terms, for the same pre-render: a written page states
/// the fees and the bands the chain is running under, not the ones this app
/// was built with.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_config(api: &str) -> Result<shared::Config, String> {
    blocking(api, "config")
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
