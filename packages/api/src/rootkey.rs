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

use std::time::Duration;

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
    relay(
        &service.http,
        &format!("{}/rootkey/published/{purpose}", service.executor),
        TIMEOUT,
    )
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
    let unavailable = || (StatusCode::BAD_GATEWAY, "the executor is unavailable").into_response();
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
    let Ok(Some((status, body))) = tokio::time::timeout(timeout, asked).await else {
        return unavailable();
    };
    match status.as_u16() {
        200 => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        code @ (400 | 403 | 404 | 503) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            body,
        )
            .into_response(),
        _ => unavailable(),
    }
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
