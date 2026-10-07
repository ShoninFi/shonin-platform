//! The root key service's public reads, relayed from the executor's status
//! port, which only the platform's own machines reach:
//!
//! - `GET /api/rootkey`: whether the executor holds and serves the root, and
//!   the root signer's address;
//! - `GET /api/rootkey/published/{purpose}`: a purpose's public key, signed
//!   by the root signer.
//!
//! The relay is trusted with nothing: a published key is checked by whoever
//! seals to it, against the signer the canonical chain records
//! (`shonin-seal` does). It only passes the answer on, bounded in size and
//! time, and asks the executor for nothing but these two paths.
//!
//! The executor reads the canonical chain to answer each published key, so
//! the public route can't be let spend its chain reads: each purpose's
//! answer is kept for [`KEEP`] and passed on again from here (a published
//! key is public, and checked against the chain by whoever seals to it), and
//! no more than [`MOST_AT_ONCE`] are asked of the executor at a time; one
//! more is answered busy.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::Service;

/// The most of an answer passed on: a published key or the status is a few
/// hundred bytes.
pub const MOST_BYTES: usize = 16 * 1024;
/// How long the executor has to answer.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The longest purpose asked for: a venue's, with its chain, vault and name.
const MOST_PURPOSE: usize = 160;
/// How long the executor's answer for a purpose's published key is passed
/// on again without asking it.
pub const KEEP: Duration = Duration::from_secs(30);
/// The most published keys asked of the executor at once.
pub const MOST_AT_ONCE: usize = 4;
/// The most purposes whose answers are kept at once.
const MOST_KEPT: usize = 1024;

/// An answer of the executor's, as it is passed on.
#[derive(Clone)]
struct Answer {
    status: StatusCode,
    body: axum::body::Bytes,
}

/// The published keys' relays: each purpose's answer as last asked, and the
/// relays to the executor in flight.
pub struct Relays {
    keep: Duration,
    kept: Mutex<HashMap<String, (Instant, Answer)>>,
    at_once: tokio::sync::Semaphore,
}

impl Default for Relays {
    fn default() -> Relays {
        Relays::new(KEEP, MOST_AT_ONCE)
    }
}

impl Relays {
    /// Answers kept for `keep`, and at most `at_once` asked at a time.
    pub fn new(keep: Duration, at_once: usize) -> Relays {
        Relays {
            keep,
            kept: Mutex::new(HashMap::new()),
            at_once: tokio::sync::Semaphore::new(at_once),
        }
    }

    fn kept(&self, purpose: &str, now: Instant) -> Option<Answer> {
        let kept = self
            .kept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        kept.get(purpose)
            .filter(|(at, _)| now.duration_since(*at) < self.keep)
            .map(|(_, answer)| answer.clone())
    }

    fn keep(&self, purpose: &str, now: Instant, answer: Answer) {
        let mut kept = self
            .kept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if kept.len() >= MOST_KEPT {
            kept.retain(|_, (at, _)| now.duration_since(*at) < self.keep);
        }
        if kept.len() < MOST_KEPT || kept.contains_key(purpose) {
            kept.insert(purpose.to_string(), (now, answer));
        }
    }

    /// The executor's answer for `purpose`'s published key at
    /// `{executor}/rootkey/published/{purpose}`: as kept, if it was asked in
    /// the last [`KEEP`]; otherwise asked now, unless as many are in flight
    /// as may be (503, to ask again in a moment).
    pub async fn published(
        &self,
        http: &reqwest::Client,
        executor: &str,
        purpose: &str,
    ) -> Response {
        if let Some(answer) = self.kept(purpose, Instant::now()) {
            return pass_on(answer);
        }
        let Ok(_asking) = self.at_once.try_acquire() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                "the root key service is busy: ask again in a moment",
            )
                .into_response();
        };
        let url = format!("{executor}/rootkey/published/{purpose}");
        let Some(answer) = ask(http, &url, TIMEOUT).await else {
            return unavailable();
        };
        self.keep(purpose, Instant::now(), answer.clone());
        pass_on(answer)
    }
}

/// `GET /api/rootkey`.
pub async fn status(State(service): State<Arc<Service>>) -> Response {
    relay(
        &service.http,
        &format!("{}/rootkey", service.executor),
        TIMEOUT,
    )
    .await
}

/// `GET /api/rootkey/published/{purpose}`.
pub async fn published(
    State(service): State<Arc<Service>>,
    UrlPath(purpose): UrlPath<String>,
) -> Response {
    if !is_purpose(&purpose) {
        return (StatusCode::BAD_REQUEST, "that is not a purpose").into_response();
    }
    service
        .rootkey
        .published(&service.http, &service.executor, &purpose)
        .await
}

/// Whether `text` could be a purpose's string form (`platform:dummy`,
/// `bundle:0x…`, `venue:<chain>:0x<vault>:<venue>`): lowercase letters,
/// digits, `:`, `.`, `_` and `-`, starting with a letter, and no `..`. The
/// executor decides whether it is one; this only keeps what is asked of the
/// executor a single path segment of its own route.
pub fn is_purpose(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MOST_PURPOSE
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && !text.contains("..")
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b":._-".contains(&b))
}

/// Ask the executor at `url` and pass its answer on: a success as JSON, a
/// refusal (400, 403, 404, 503) with its status and reason, anything else,
/// an answer too long or too slow, or no answer, as 502.
pub async fn relay(http: &reqwest::Client, url: &str, timeout: Duration) -> Response {
    match ask(http, url, timeout).await {
        Some(answer) => pass_on(answer),
        None => unavailable(),
    }
}

fn unavailable() -> Response {
    (StatusCode::BAD_GATEWAY, "the executor is unavailable").into_response()
}

/// The executor's answer at `url`, if it is one to pass on: a success, or a
/// refusal (400, 403, 404, 503), within [`MOST_BYTES`] and `timeout`.
async fn ask(http: &reqwest::Client, url: &str, timeout: Duration) -> Option<Answer> {
    let asked = async {
        let mut reply = http.get(url).send().await.ok()?;
        if reply
            .content_length()
            .is_some_and(|length| length > MOST_BYTES as u64)
        {
            return None;
        }
        let status = reply.status();
        let mut body = Vec::new();
        while let Some(chunk) = reply.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MOST_BYTES {
                return None;
            }
        }
        Some((status, body))
    };
    let (status, body) = tokio::time::timeout(timeout, asked).await.ok()??;
    matches!(status.as_u16(), 200 | 400 | 403 | 404 | 503).then(|| Answer {
        status,
        body: body.into(),
    })
}

fn pass_on(answer: Answer) -> Response {
    let kind = if answer.status == StatusCode::OK {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    (answer.status, [(header::CONTENT_TYPE, kind)], answer.body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    /// An executor whose root key routes answer `status` with `body`, after
    /// `delay`, noting each path asked.
    async fn an_executor(
        status: StatusCode,
        body: String,
        delay: Duration,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let noted = asked.clone();
        let app = axum::Router::new().fallback(get(move |uri: axum::http::Uri| {
            let body = body.clone();
            let noted = noted.clone();
            async move {
                noted.lock().unwrap().push(uri.path().to_string());
                tokio::time::sleep(delay).await;
                (status, body)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, asked)
    }

    async fn body_of(response: Response) -> (StatusCode, String) {
        use http_body_util::BodyExt;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    const KEY: &str =
        r#"{"chain_id":31337,"purpose":"platform:dummy","public_key":"0x00","signature":"0x00"}"#;

    #[tokio::test]
    async fn the_executor_s_answer_is_relayed_as_it_is() {
        let (executor, asked) = an_executor(StatusCode::OK, KEY.into(), Duration::ZERO).await;
        let http = reqwest::Client::new();
        let url = format!("{executor}/rootkey/published/platform:dummy");
        let (status, body) = body_of(relay(&http, &url, TIMEOUT).await).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, KEY));
        assert_eq!(
            *asked.lock().unwrap(),
            ["/rootkey/published/platform:dummy"]
        );

        // A refusal keeps its status and its reason.
        let why = "the canonical chain does not record this root as the live root key";
        for refused in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_FOUND,
        ] {
            let (executor, _) = an_executor(refused, why.into(), Duration::ZERO).await;
            let url = format!("{executor}/rootkey");
            let (status, body) = body_of(relay(&http, &url, TIMEOUT).await).await;
            assert_eq!((status, body.as_str()), (refused, why));
        }
    }

    #[tokio::test]
    async fn an_answer_too_long_too_slow_or_not_at_all_is_unavailable() {
        let http = reqwest::Client::new();
        let (executor, _) =
            an_executor(StatusCode::OK, "x".repeat(MOST_BYTES + 1), Duration::ZERO).await;
        let answer = relay(&http, &format!("{executor}/rootkey"), TIMEOUT).await;
        assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);

        let (executor, _) = an_executor(StatusCode::OK, KEY.into(), Duration::from_secs(5)).await;
        let started = std::time::Instant::now();
        let answer = relay(
            &http,
            &format!("{executor}/rootkey"),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);
        assert!(started.elapsed() < Duration::from_secs(2));

        let answer = relay(&http, "http://127.0.0.1:1/rootkey", TIMEOUT).await;
        assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);

        // An executor failing in some other way is unavailable too.
        let (executor, _) = an_executor(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no".into(),
            Duration::ZERO,
        )
        .await;
        let answer = relay(&http, &format!("{executor}/rootkey"), TIMEOUT).await;
        assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);
    }

    /// The public route never has the executor read the chain more than
    /// once per purpose per [`KEEP`]: a purpose's answer is passed on again
    /// from here while it is kept, and asked again once it has run out.
    #[tokio::test]
    async fn a_purpose_s_published_key_is_asked_of_the_executor_once_while_kept() {
        let (executor, asked) = an_executor(StatusCode::OK, KEY.into(), Duration::ZERO).await;
        let http = reqwest::Client::new();
        let relays = Relays::new(Duration::from_millis(300), MOST_AT_ONCE);
        for _ in 0..5 {
            let (status, body) =
                body_of(relays.published(&http, &executor, "platform:dummy").await).await;
            assert_eq!((status, body.as_str()), (StatusCode::OK, KEY));
        }
        assert_eq!(asked.lock().unwrap().len(), 1);
        // Another purpose is its own answer.
        relays.published(&http, &executor, "builder").await;
        assert_eq!(asked.lock().unwrap().len(), 2);
        // Once it has run out, asked again.
        tokio::time::sleep(Duration::from_millis(400)).await;
        relays.published(&http, &executor, "platform:dummy").await;
        assert_eq!(
            *asked.lock().unwrap(),
            [
                "/rootkey/published/platform:dummy",
                "/rootkey/published/builder",
                "/rootkey/published/platform:dummy"
            ]
        );

        // A refusal is kept the same way; no answer at all is not kept.
        let (refusing, asked) = an_executor(
            StatusCode::SERVICE_UNAVAILABLE,
            "not serving".into(),
            Duration::ZERO,
        )
        .await;
        for _ in 0..3 {
            let answer = relays.published(&http, &refusing, "platform:dummy-2").await;
            assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        assert_eq!(asked.lock().unwrap().len(), 1);
        for _ in 0..2 {
            let answer = relays
                .published(&http, "http://127.0.0.1:1", "platform:dummy-3")
                .await;
            assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);
        }
    }

    /// However many purposes are asked at once, no more than
    /// [`MOST_AT_ONCE`] reach the executor together; the rest are answered
    /// busy at once.
    #[tokio::test]
    async fn no_more_published_keys_than_the_bound_are_asked_of_the_executor_at_once() {
        let (executor, asked) =
            an_executor(StatusCode::OK, KEY.into(), Duration::from_millis(500)).await;
        let http = reqwest::Client::new();
        let relays = Arc::new(Relays::default());
        let mut asking = tokio::task::JoinSet::new();
        for each in 0..(MOST_AT_ONCE * 3) {
            let (relays, http, executor) = (relays.clone(), http.clone(), executor.clone());
            asking.spawn(async move {
                relays
                    .published(&http, &executor, &format!("platform:p{each}"))
                    .await
                    .status()
            });
        }
        let mut answered = Vec::new();
        while let Some(status) = asking.join_next().await {
            answered.push(status.unwrap());
        }
        let served = answered.iter().filter(|s| **s == StatusCode::OK).count();
        let busy = answered
            .iter()
            .filter(|s| **s == StatusCode::SERVICE_UNAVAILABLE)
            .count();
        assert_eq!(served, MOST_AT_ONCE);
        assert_eq!(busy, MOST_AT_ONCE * 2);
        assert_eq!(asked.lock().unwrap().len(), MOST_AT_ONCE);
    }

    #[test]
    fn only_a_purpose_s_shape_is_asked_for() {
        for purpose in [
            "platform:dummy",
            "builder",
            "root",
            "bundle:0x5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b",
            "venue:998:0x1111111111111111111111111111111111111111:hyperliquid",
        ] {
            assert!(is_purpose(purpose), "{purpose}");
        }
        for not in [
            "",
            "..",
            "platform:..",
            "Platform:dummy",
            ":dummy",
            "1builder",
            "platform:dummy?x=1",
            "platform:dummy#x",
            "platform/dummy",
            "platform:%2e%2e",
            "platform: dummy",
        ] {
            assert!(!is_purpose(not), "{not:?}");
        }
        assert!(!is_purpose(&"a".repeat(MOST_PURPOSE + 1)));
    }
}
