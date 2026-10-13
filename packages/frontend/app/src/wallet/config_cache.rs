//! The chains' configs (`/api/{chain}/config`), remembered between visits.
//!
//! The API can take a long while to answer for a chain's config (the Solana
//! cluster's especially), and nothing on the portfolio can be read before
//! it. So the last config that read well is kept in the browser's storage,
//! keyed by the API's origin and this build's deployment, and used at once
//! on the next load *to read*; the API is asked again in the background.
//!
//! A remembered config is never used to sign. Before a flow sends anything
//! it takes [`ConfigSlot::for_signing`]: the config fetched from the API in
//! this page's life, fetching it now if the page holds only a remembered
//! copy. A fresh answer that differs from the remembered copy the page was
//! showing stops that send, so the person sees the new figures first. The
//! flows' own checks (the cluster's genesis, the programs' ids, the chain id
//! the endpoint answers for) run on whatever is used, as before.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use shared::Chain;

/// Where the page's copy of a config came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Kept from an earlier visit: good for reading only.
    Remembered,
    /// Fetched from the API during this page's life.
    Fetched,
}

/// A config as the API wrote it, byte for byte, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub raw: Rc<str>,
    pub source: Source,
}

/// What a send may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForSigning {
    /// A config fetched from the API in this page's life.
    Use(Rc<str>),
    /// The API's fresh answer differs from the remembered copy the page was
    /// showing: the page takes the fresh one, and this send stops so the
    /// person sees what changed.
    Changed(Rc<str>),
    /// No fresh config could be read: nothing is sent.
    Unavailable(String),
}

/// One chain's config as the page holds it, with at most one fetch of it in
/// flight at a time: a second asker waits for the first fetch's answer.
#[derive(Default)]
pub struct ConfigSlot {
    held: RefCell<Option<Held>>,
    in_flight: Mutable<bool>,
    last_error: RefCell<Option<String>>,
}

impl ConfigSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a copy remembered from an earlier visit, unless the page has
    /// already fetched one.
    pub fn remember(&self, raw: &str) {
        let mut held = self.held.borrow_mut();
        if held.is_none() {
            *held = Some(Held {
                raw: raw.into(),
                source: Source::Remembered,
            });
        }
    }

    pub fn held(&self) -> Option<Held> {
        self.held.borrow().clone()
    }

    /// Whether the page holds a config fetched in its own life.
    pub fn is_fresh(&self) -> bool {
        self.held
            .borrow()
            .as_ref()
            .is_some_and(|held| held.source == Source::Fetched)
    }

    /// Ask the API (or wait for the ask already in flight). The config it
    /// answered, which the slot now holds as fetched.
    pub async fn refresh<F, Fut>(&self, fetch: F) -> Result<Rc<str>, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        if self.in_flight.get() {
            self.in_flight.signal().wait_for(false).await;
            return match self.held() {
                Some(Held {
                    raw,
                    source: Source::Fetched,
                }) => Ok(raw),
                _ => Err(self
                    .last_error
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "the API did not answer".to_string())),
            };
        }
        self.in_flight.set(true);
        let answer = fetch().await;
        let result = match answer {
            Ok(raw) => {
                let raw: Rc<str> = raw.into();
                *self.held.borrow_mut() = Some(Held {
                    raw: raw.clone(),
                    source: Source::Fetched,
                });
                Ok(raw)
            }
            Err(why) => {
                *self.last_error.borrow_mut() = Some(why.clone());
                Err(why)
            }
        };
        self.in_flight.set(false);
        result
    }

    /// The config a send may use: one fetched in this page's life, else a
    /// fresh fetch now. A remembered copy is used only as the fresh fetch
    /// itself: identical byte for byte, or the send stops.
    pub async fn for_signing<F, Fut>(&self, fetch: F) -> ForSigning
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        let before = self.held();
        if let Some(Held {
            raw,
            source: Source::Fetched,
        }) = &before
        {
            return ForSigning::Use(raw.clone());
        }
        match self.refresh(fetch).await {
            Ok(fresh) => match before {
                Some(remembered) if *remembered.raw != *fresh => ForSigning::Changed(fresh),
                _ => ForSigning::Use(fresh),
            },
            Err(why) => ForSigning::Unavailable(why),
        }
    }
}

/// A config as the page holds, keeps and compares it: as the API wrote it,
/// less its reading of the chain's head (`chain.block` is zeroed and
/// `chain.as_of` left out), which moves with every answer and says nothing
/// about where to send. Keys come out in one order, so two answers that
/// agree on everything else are the same bytes. `None` for an answer that
/// isn't a JSON object.
pub fn canonical(raw: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let config = value.as_object_mut()?;
    if let Some(chain) = config
        .get_mut("chain")
        .and_then(|chain| chain.as_object_mut())
    {
        if chain.contains_key("block") {
            chain.insert("block".to_string(), serde_json::Value::from(0u8));
        }
        chain.remove("as_of");
    }
    serde_json::to_string(&value).ok()
}

/// This build's deployment: the network it was built for and the Solana
/// vault program it names, so a copy kept for one deployment is never read
/// by a build for another.
pub fn deployment() -> String {
    format!(
        "{}:{}",
        option_env!("SHONIN_APP_NETWORK").unwrap_or("dev"),
        chain::solana_ix::ids::VAULT
    )
}

/// Where a chain's config is kept: by the API's origin (the page's own when
/// the API is reached through it), this build's deployment and the chain.
pub fn storage_key(api_origin: &str, chain: Chain) -> String {
    format!("config.v1.{api_origin}.{}.{}", deployment(), chain.name())
}

/// The API's origin as the key names it.
#[cfg(target_arch = "wasm32")]
fn api_origin() -> String {
    crate::api::origin(crate::api::BASE)
        .or_else(|| crate::web::window()?.location().origin().ok())
        .unwrap_or_default()
}

/// The copy of `chain`'s config kept from an earlier visit. Storage may be
/// missing or refuse; then there is none.
#[cfg(target_arch = "wasm32")]
pub fn recall(chain: Chain) -> Option<String> {
    canonical(&crate::web::storage_get(&storage_key(
        &api_origin(),
        chain,
    ))?)
}

/// Keep `raw` as `chain`'s config for the next visit (only a config that
/// read well is kept). Storage may refuse; nothing depends on it.
#[cfg(target_arch = "wasm32")]
pub fn keep(chain: Chain, raw: &str) {
    crate::web::storage_set(&storage_key(&api_origin(), chain), raw);
}

/// Ask the API for `chain`'s config, in its [`canonical`] form.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_raw(chain: Chain) -> Result<String, String> {
    let reply = gloo_net::http::Request::get(&format!("{}/{chain}/config", crate::api::BASE))
        .send()
        .await
        .map_err(|error| format!("the API did not answer: {error}"))?;
    if !reply.ok() {
        return Err(format!("the API answered {}", reply.status()));
    }
    let text = reply
        .text()
        .await
        .map_err(|error| format!("the API said something unexpected: {error}"))?;
    canonical(&text).ok_or_else(|| "the API said something unexpected".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    async fn answer(raw: &str) -> Result<String, String> {
        Ok(raw.to_string())
    }

    async fn down() -> Result<String, String> {
        Err("the API answered 503".to_string())
    }

    #[test]
    fn a_remembered_config_is_read_at_once_but_never_signed_with_while_the_api_is_down() {
        let slot = ConfigSlot::new();
        slot.remember("{\"v\":1}");
        // Readers have it at once.
        assert_eq!(
            slot.held(),
            Some(Held {
                raw: "{\"v\":1}".into(),
                source: Source::Remembered
            })
        );
        assert!(!slot.is_fresh());
        // A send asks the API; with no answer, nothing is signed.
        assert_eq!(
            block_on(slot.for_signing(down)),
            ForSigning::Unavailable("the API answered 503".to_string())
        );
        assert!(!slot.is_fresh(), "still only the remembered copy");
    }

    #[test]
    fn a_remembered_config_is_signed_with_only_when_a_fresh_fetch_matches_it_byte_for_byte() {
        let slot = ConfigSlot::new();
        slot.remember("{\"v\":1}");
        assert_eq!(
            block_on(slot.for_signing(|| answer("{\"v\":1}"))),
            ForSigning::Use("{\"v\":1}".into())
        );
        assert!(slot.is_fresh());

        // A fresh answer that differs, even by a byte, stops the send and
        // replaces what the page reads.
        let slot = ConfigSlot::new();
        slot.remember("{\"v\":1}");
        assert_eq!(
            block_on(slot.for_signing(|| answer("{\"v\": 1}"))),
            ForSigning::Changed("{\"v\": 1}".into())
        );
        assert_eq!(slot.held().unwrap().raw, "{\"v\": 1}".into());
        assert_eq!(slot.held().unwrap().source, Source::Fetched);
        // The next send uses the fetched copy.
        assert_eq!(
            block_on(slot.for_signing(down)),
            ForSigning::Use("{\"v\": 1}".into())
        );
    }

    #[test]
    fn a_config_fetched_in_this_pages_life_is_signed_with_and_never_replaced_by_a_remembered_one() {
        let slot = ConfigSlot::new();
        assert!(block_on(slot.refresh(|| answer("{\"v\":2}"))).is_ok());
        slot.remember("{\"v\":1}");
        assert_eq!(slot.held().unwrap().raw, "{\"v\":2}".into());
        assert_eq!(
            block_on(slot.for_signing(down)),
            ForSigning::Use("{\"v\":2}".into())
        );
        // Nothing held at all: a fresh fetch is used as it is.
        let empty = ConfigSlot::new();
        assert_eq!(
            block_on(empty.for_signing(|| answer("{\"v\":3}"))),
            ForSigning::Use("{\"v\":3}".into())
        );
    }

    #[test]
    fn two_answers_that_differ_only_in_the_chains_head_are_the_same_bytes() {
        let one =
            r#"{"rpc_url":"https://a","chain":{"chain":"solana","block":1,"as_of":[2026,1]}}"#;
        let two =
            r#"{"chain":{"as_of":[2026,2],"block":9,"chain":"solana"},"rpc_url":"https://a"}"#;
        assert_eq!(canonical(one), canonical(two));
        assert!(!canonical(one).unwrap().contains("as_of"));
        assert!(canonical(one).unwrap().contains("\"block\":0"));
        let moved = r#"{"chain":{"chain":"solana","block":9},"rpc_url":"https://b"}"#;
        assert_ne!(canonical(one), canonical(moved));
        assert_eq!(canonical("not json"), None);
        assert_eq!(canonical("[1]"), None);
        // So a remembered copy is signed with once the API agrees on all
        // but the head.
        let slot = ConfigSlot::new();
        slot.remember(&canonical(one).unwrap());
        let fresh = canonical(two).unwrap();
        assert!(matches!(
            block_on(slot.for_signing(|| async move { Ok(fresh) })),
            ForSigning::Use(_)
        ));
    }

    #[test]
    fn a_kept_config_is_keyed_by_the_api_origin_the_deployment_and_the_chain() {
        let one = storage_key("https://testnet-api.shonin.fi", Chain::Solana);
        let other = storage_key("http://localhost:19201", Chain::Solana);
        assert_ne!(one, other);
        assert!(one.contains("https://testnet-api.shonin.fi"), "{one}");
        assert!(one.contains(&deployment()), "{one}");
        assert!(one.ends_with(".solana"), "{one}");
        assert_ne!(
            one,
            storage_key("https://testnet-api.shonin.fi", Chain::Base)
        );
    }
}
