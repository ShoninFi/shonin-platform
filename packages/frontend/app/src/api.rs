//! Talking to the platform's API.
//!
//! The app asks for a snapshot, renders it, and asks again. It never holds a
//! second copy of anything. Chain figures carry their observation dates and
//! follow the indexer's configured active or idle cadence; authenticated executor
//! reports update independently.

#[cfg(target_arch = "wasm32")]
use crate::data;

/// Where the API answers. The dev server proxies this path to it, and a
/// deployed app is served beside it, so the same relative path works in both.
pub const BASE: &str = "/api";

/// How often to ask whether anything has changed.
#[cfg(target_arch = "wasm32")]
const EVERY_MILLIS: u32 = 2_000;

/// Keep fetching snapshots, and fetch each vault chain's configuration the
/// first time the snapshot lists it.
#[cfg(target_arch = "wasm32")]
pub fn follow() {
    wasm_bindgen_futures::spawn_local(async {
        let mut resumed = false;
        loop {
            match fetch::<shared::Snapshot>("snapshot").await {
                Ok(snapshot) => {
                    for info in &snapshot.chains {
                        if data::config(info.chain).is_none() {
                            match fetch::<shared::Config>(&format!("{}/config", info.chain)).await {
                                Ok(config) => data::set_config(config),
                                Err(why) => data::set_failed(why),
                            }
                        }
                    }
                    // Only a newer snapshot is worth re-rendering for.
                    let held = data::current().map(|dataset| dataset.version).unwrap_or(0);
                    if snapshot.version != held {
                        data::set(snapshot);
                    }
                    // Following a link should not disconnect anyone.
                    if !resumed {
                        resumed = true;
                        crate::wallet::resume().await;
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

/// A vault's running usage total, as its executor signed it.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct UsageTotal {
    pub total: String,
    pub as_of: u64,
    pub signature: String,
}

/// The executor's latest signed total for a vault: what an author settles
/// before taking money out of the tank.
#[cfg(target_arch = "wasm32")]
pub async fn usage(chain: shared::Chain, vault: &str) -> Result<UsageTotal, String> {
    let reply = gloo_net::http::Request::get(&format!("{BASE}/{chain}/usage/{vault}"))
        .send()
        .await
        .map_err(|error| format!("the API did not answer: {error}"))?;
    if !reply.ok() {
        return Err(no_usage_total(reply.status()));
    }
    reply
        .json()
        .await
        .map_err(|error| format!("the API said something unexpected: {error}"))
}

/// Why there is no signed total to settle with, from the API's status: the
/// executor serves none for the vault, or it can't be reached just now.
pub fn no_usage_total(status: u16) -> String {
    match status {
        404 => "the executor has no total for this vault yet".to_string(),
        502..=504 => "the executor is unavailable just now; try again in a minute".to_string(),
        other => format!("the API answered {other}"),
    }
}

/// What one address holds on one vault chain, read when a page needs it.
#[cfg(target_arch = "wasm32")]
pub async fn portfolio(chain: shared::Chain, address: &str) -> Result<shared::Portfolio, String> {
    fetch(&format!("{chain}/portfolio/{address}")).await
}

/// Upload an agent's source to the build service, for a vault on this chain.
#[cfg(target_arch = "wasm32")]
pub async fn upload(
    chain: shared::Chain,
    upload: &shared::Upload,
) -> Result<shared::UploadAccepted, String> {
    let reply = gloo_net::http::Request::post(&format!("{BASE}/{chain}/uploads"))
        .json(upload)
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
pub fn fetch_config(_api: &str, _chain: shared::Chain) -> Result<shared::Config, String> {
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
    fn an_executor_that_is_unavailable_is_not_said_to_have_no_total() {
        for status in [502, 503] {
            let said = no_usage_total(status);
            assert!(said.contains("unavailable"), "{status}: {said}");
        }
        assert!(no_usage_total(404).contains("no total"));
    }
}
