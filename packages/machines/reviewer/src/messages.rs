//! Asking the pinned model over the Messages API.
//!
//! In the attested image the reviewer holds the model's key itself: it is
//! sealed to the reviewer's purpose, opened in the enclave at start, and
//! kept in memory only. It is sent as one sensitive header to one fixed
//! host, never through a proxy and never after a redirect, and nothing the
//! reviewer says (its logs, its errors, its answers) carries it.
//!
//! The report's shape is forced: the model is given one tool, `report`, whose
//! input is the schema, and must call it. An answer that is not that call, or
//! is cut short, or is an error of the API's, is no answer at all: the
//! review goes unmade (503), and the build service asks again later.

use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use zeroize::Zeroizing;

/// The only host the key is ever sent to.
const HOST: &str = "https://api.anthropic.com";

/// The API version the request and the answer are read as.
const API_VERSION: &str = "2023-06-01";

/// The name of the one tool the model is made to call.
const TOOL: &str = "report";

/// The longest answer asked for, thinking included. A report is a summary,
/// at most fifty findings and one answer per battery question; this leaves
/// room for that many times over, and stays under the size at which an
/// answer that is not streamed is expected to take longer than ten minutes.
pub const MOST_ANSWER_TOKENS: u32 = 20_000;

/// The model's key, wiped from memory when dropped and never printed.
pub struct ModelKey(Zeroizing<String>);

impl ModelKey {
    /// The key from the plaintext it was sealed as. Surrounding whitespace
    /// (a trailing newline) is dropped; anything else a header can't carry
    /// is refused, without saying what it was.
    pub fn from_plaintext(plaintext: &[u8]) -> Result<Self, NotAKey> {
        let text = std::str::from_utf8(plaintext).map_err(|_| NotAKey)?.trim();
        if text.is_empty() || HeaderValue::from_str(text).is_err() {
            return Err(NotAKey);
        }
        Ok(Self(Zeroizing::new(text.to_string())))
    }

    fn header(&self) -> HeaderValue {
        let mut value =
            HeaderValue::from_str(&self.0).expect("checked to be a header value when made");
        value.set_sensitive(true);
        value
    }
}

impl std::fmt::Debug for ModelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModelKey(redacted)")
    }
}

/// The sealed model key opened to something that is not a key.
#[derive(Debug)]
pub struct NotAKey;

impl std::fmt::Display for NotAKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sealed model key did not open to a key")
    }
}

impl std::error::Error for NotAKey {}

/// Why the model gave no report. Each says what happened in the reviewer's
/// own words, with at most the HTTP status and the API's error type: never a
/// header, and never the body of an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// The API could not be reached, or did not answer in time.
    Unreached(&'static str),
    /// The API answered with a status other than success: a redirect is
    /// never followed, so it is one of these too.
    Refused {
        status: u16,
        error_type: Option<String>,
    },
    /// The model's answer was cut short at the most it was allowed.
    CutShort,
    /// The model answered, but not with the report.
    OffSchema(&'static str),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreached(why) => write!(f, "the model's API {why}"),
            Self::Refused {
                status,
                error_type: Some(kind),
            } => write!(f, "the model's API answered {status} ({kind})"),
            Self::Refused {
                status,
                error_type: None,
            } => write!(f, "the model's API answered {status}"),
            Self::CutShort => f.write_str("the model's report was cut short"),
            Self::OffSchema(why) => write!(f, "the model answered outside its schema: {why}"),
        }
    }
}

impl std::error::Error for Unanswered {}

/// A client for the pinned model.
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    key: ModelKey,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("key", &self.key)
            .finish()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ErrorAnswer {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: String,
}

impl Client {
    /// A client for `model`, holding `key`, that gives one answer at most
    /// `wait` to arrive whole.
    pub fn new(key: ModelKey, model: String, wait: Duration) -> Self {
        Self::at(format!("{HOST}/v1/messages"), key, model, wait)
    }

    /// A client asking a stand-in instead of the API.
    #[cfg(test)]
    fn at_stand_in(url: &str, key: ModelKey, model: &str, wait: Duration) -> Self {
        Self::at(format!("{url}/v1/messages"), key, model.to_string(), wait)
    }

    fn at(endpoint: String, key: ModelKey, model: String, wait: Duration) -> Self {
        let http = reqwest::Client::builder()
            // Never through a proxy, whatever the environment names, and
            // never on to wherever a redirect points: the key goes to the
            // API's host and nowhere else.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(wait)
            // A long answer is a long silence on the connection; keep-alives
            // stop a network in between from dropping it as idle.
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("an http client");
        Self {
            http,
            endpoint,
            model,
            key,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The request's body: `system` as the system prompt, `user` as the one
    /// user turn, and the report's `schema` as the input of the one tool the
    /// model must call.
    fn body(&self, system: &str, user: &str, schema: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "max_tokens": MOST_ANSWER_TOKENS,
            "system": system,
            "messages": [{"role": "user", "content": user}],
            "tools": [{
                "name": TOOL,
                "description": "Give the review: the summary, the findings, what the review \
                                could not see, and one answer for every battery question.",
                "input_schema": schema,
            }],
            "tool_choice": {"type": "tool", "name": TOOL},
        })
    }

    /// Ask for the report, and the input the model called `report` with.
    pub async fn ask(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value, Unanswered> {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", self.key.header());
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let body = serde_json::to_vec(&self.body(system, user, schema))
            .map_err(|_| Unanswered::Unreached("could not be asked"))?;

        let response = self
            .http
            .post(&self.endpoint)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|error| Unanswered::Unreached(unreached(&error)))?;
        let status = response.status();
        let text = response
            .bytes()
            .await
            .map_err(|error| Unanswered::Unreached(unreached(&error)))?;
        if !status.is_success() {
            return Err(Unanswered::Refused {
                status: status.as_u16(),
                error_type: error_type(&text),
            });
        }
        report_in(&text)
    }
}

/// What went wrong reaching the API, in words of the reviewer's own: a
/// transport error's text can name the URL and more, so it is not used.
fn unreached(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "did not answer in time"
    } else if error.is_connect() {
        "could not be reached"
    } else if error.is_body() || error.is_decode() {
        "broke off its answer"
    } else {
        "could not be asked"
    }
}

/// The error type of an error answer, if it is one the API documents the
/// shape of and the type is a plain word. Nothing else of the body is kept.
fn error_type(body: &[u8]) -> Option<String> {
    let kind = serde_json::from_slice::<ErrorAnswer>(body).ok()?.error.kind;
    (!kind.is_empty()
        && kind.len() <= 64
        && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    .then_some(kind)
}

/// The report the model gave in a successful answer: the input of its call
/// to `report`, and only if the answer ended in that call.
fn report_in(body: &[u8]) -> Result<serde_json::Value, Unanswered> {
    let answer: Answer = serde_json::from_slice(body)
        .map_err(|_| Unanswered::OffSchema("the answer is not a message"))?;
    match answer.stop_reason.as_deref() {
        Some("tool_use") => {}
        Some("max_tokens") | Some("model_context_window_exceeded") => {
            return Err(Unanswered::CutShort);
        }
        _ => return Err(Unanswered::OffSchema("it did not end by giving the report")),
    }
    let mut calls = answer
        .content
        .into_iter()
        .filter(|block| block.kind == "tool_use");
    match (calls.next(), calls.next()) {
        (Some(call), None) if call.name.as_deref() == Some(TOOL) => call
            .input
            .filter(serde_json::Value::is_object)
            .ok_or(Unanswered::OffSchema("the report is not an object")),
        (Some(_), None) => Err(Unanswered::OffSchema("it called another tool")),
        (None, _) => Err(Unanswered::OffSchema("it gave no report")),
        (Some(_), Some(_)) => Err(Unanswered::OffSchema("it gave more than one report")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::http::{HeaderMap as Headers, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{any, post};
    use std::sync::{Arc, Mutex};

    const KEY: &str = "sk-stand-in-key-that-must-never-be-seen-0123456789";

    fn key() -> ModelKey {
        ModelKey::from_plaintext(format!("{KEY}\n").as_bytes()).unwrap()
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// What a stand-in API was sent: each request's headers and body.
    type Seen = Arc<Mutex<Vec<(Headers, serde_json::Value)>>>;

    /// A stand-in API answering every request with `status` and `answer`,
    /// noting what it was sent.
    async fn api(status: StatusCode, answer: serde_json::Value) -> (String, Seen) {
        let seen: Seen = Default::default();
        let noted = seen.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            post(
                move |headers: Headers, Json(body): Json<serde_json::Value>| {
                    let noted = noted.clone();
                    let answer = answer.clone();
                    async move {
                        noted.lock().unwrap().push((headers, body));
                        (status, Json(answer)).into_response()
                    }
                },
            ),
        );
        (serve(app).await, seen)
    }

    fn reported(input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "stand-in",
            "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "report", "input": input}],
        })
    }

    fn schema() -> serde_json::Value {
        serde_json::from_str(include_str!("../review/schema.json")).unwrap()
    }

    fn client(url: &str) -> Client {
        Client::at_stand_in(url, key(), "stand-in-model", Duration::from_secs(10))
    }

    #[tokio::test]
    async fn the_request_forces_the_report_through_its_one_tool() {
        let (url, seen) = api(
            StatusCode::OK,
            reported(serde_json::json!({"summary": "s"})),
        )
        .await;
        let report = client(&url)
            .ask("the prompt", "the description and the source", &schema())
            .await
            .unwrap();
        assert_eq!(report["summary"], "s");

        let seen = seen.lock().unwrap();
        let (headers, body) = &seen[0];
        assert_eq!(
            headers["x-api-key"], KEY,
            "the key is sent trimmed, as the key"
        );
        assert_eq!(headers["anthropic-version"], API_VERSION);
        assert_eq!(body["model"], "stand-in-model");
        assert_eq!(body["max_tokens"], MOST_ANSWER_TOKENS);
        assert_eq!(body["system"], "the prompt");
        assert_eq!(
            body["messages"],
            serde_json::json!([{"role": "user", "content": "the description and the source"}])
        );
        assert_eq!(body["tools"][0]["name"], "report");
        assert_eq!(body["tools"][0]["input_schema"], schema());
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "report"})
        );
        assert!(body.get("stream").is_none());
    }

    #[tokio::test]
    async fn an_answer_that_is_not_the_report_is_no_answer() {
        let cases = [
            (
                "cut short",
                serde_json::json!({"stop_reason": "max_tokens", "content": [
                    {"type": "tool_use", "name": "report", "input": {"summary": "half"}}]}),
                Unanswered::CutShort,
            ),
            (
                "prose",
                serde_json::json!({"stop_reason": "end_turn", "content": [
                    {"type": "text", "text": "looks fine"}]}),
                Unanswered::OffSchema("it did not end by giving the report"),
            ),
            (
                "refused",
                serde_json::json!({"stop_reason": "refusal", "content": []}),
                Unanswered::OffSchema("it did not end by giving the report"),
            ),
            (
                "another tool",
                serde_json::json!({"stop_reason": "tool_use", "content": [
                    {"type": "tool_use", "name": "approve", "input": {}}]}),
                Unanswered::OffSchema("it called another tool"),
            ),
            (
                "two reports",
                serde_json::json!({"stop_reason": "tool_use", "content": [
                    {"type": "tool_use", "name": "report", "input": {}},
                    {"type": "tool_use", "name": "report", "input": {}}]}),
                Unanswered::OffSchema("it gave more than one report"),
            ),
            (
                "not an object",
                serde_json::json!({"stop_reason": "tool_use", "content": [
                    {"type": "tool_use", "name": "report", "input": "cleared"}]}),
                Unanswered::OffSchema("the report is not an object"),
            ),
        ];
        for (what, answer, expected) in cases {
            let (url, _) = api(StatusCode::OK, answer).await;
            let outcome = client(&url).ask("p", "u", &schema()).await;
            assert_eq!(outcome, Err(expected), "{what}");
        }
    }

    #[tokio::test]
    async fn an_error_answer_is_no_answer_and_names_only_its_status_and_type() {
        let (url, _) = api(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({"type": "error", "error": {
                "type": "rate_limit_error", "message": format!("slow down {KEY}")}}),
        )
        .await;
        let error = client(&url).ask("p", "u", &schema()).await.unwrap_err();
        assert_eq!(
            error,
            Unanswered::Refused {
                status: 429,
                error_type: Some("rate_limit_error".into())
            }
        );
        assert_eq!(
            error.to_string(),
            "the model's API answered 429 (rate_limit_error)"
        );

        // An error type that is not a plain word is not repeated.
        let (url, _) = api(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"type": format!("x {KEY}")}}),
        )
        .await;
        assert_eq!(
            client(&url).ask("p", "u", &schema()).await,
            Err(Unanswered::Refused {
                status: 400,
                error_type: None
            })
        );
    }

    /// The key is in no log line (at every level, the HTTP stack's own
    /// included), no error, and no `Debug` of the client or the key, even
    /// when the API echoes it back in its answer's body and headers.
    #[test]
    fn the_key_is_in_no_log_error_or_debug() {
        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let _logging = tracing::subscriber::set_default(subscriber);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let said = runtime.block_on(async {
            let app = axum::Router::new().route(
                "/v1/messages",
                post(|headers: Headers| async move {
                    let echoed = headers["x-api-key"].to_str().unwrap().to_string();
                    tracing::trace!("the stand-in got the request");
                    Response::builder()
                        .status(401)
                        .header("x-echo", &echoed)
                        .body(axum::body::Body::from(format!(
                            r#"{{"type":"error","error":{{"type":"authentication_error","message":"bad key {echoed}"}}}}"#
                        )))
                        .unwrap()
                }),
            );
            let url = serve(app).await;
            let client = client(&url);
            let error = client.ask("p", "u", &schema()).await.unwrap_err();
            let error = anyhow::Error::new(error).context("a review could not be made");
            tracing::warn!(error = format!("{error:#}"), "as the reviewer logs it");
            format!("{error}\n{error:#}\n{error:?}\n{client:?}\n{:?}", key())
        });
        drop(_logging);

        let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("the stand-in got the request"),
            "nothing was logged, so the test proves nothing: {logged}"
        );
        assert!(said.contains("401 (authentication_error)"), "{said}");
        assert!(
            !said.contains(KEY),
            "the key is in an error or a Debug: {said}"
        );
        assert!(!logged.contains(KEY), "the key was logged: {logged}");
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let followed = Arc::new(Mutex::new(0));
        let counted = followed.clone();
        let app = axum::Router::new()
            .route(
                "/v1/messages",
                post(|| async {
                    Response::builder()
                        .status(307)
                        .header("location", "/elsewhere")
                        .body(axum::body::Body::empty())
                        .unwrap()
                }),
            )
            .route(
                "/elsewhere",
                any(move || {
                    let counted = counted.clone();
                    async move {
                        *counted.lock().unwrap() += 1;
                        Json(reported(serde_json::json!({})))
                    }
                }),
            );
        let url = serve(app).await;
        let outcome = client(&url).ask("p", "u", &schema()).await;
        assert_eq!(
            outcome,
            Err(Unanswered::Refused {
                status: 307,
                error_type: None
            })
        );
        assert_eq!(*followed.lock().unwrap(), 0, "the redirect was followed");
    }

    /// The environment variable that tells the copy of this test binary run
    /// by [`a_proxy_named_in_the_environment_is_not_used`] to play its part.
    const PROXIED: &str = "SHONIN_REVIEWER_TEST_PROXIED";

    /// A proxy named in the environment is not used: this test binary is run
    /// again with every proxy variable naming a stand-in proxy that counts the
    /// connections it gets. There the client asks the stand-in API, and an
    /// ordinary client asks it too, to show the variables are honoured by
    /// default. The proxy must have seen only the ordinary one.
    #[test]
    fn a_proxy_named_in_the_environment_is_not_used() {
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        proxy.set_nonblocking(true).unwrap();

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "messages::tests::asked_with_a_proxy_in_the_environment",
                "--ignored",
                "--nocapture",
            ])
            .env(PROXIED, "1")
            .env("HTTP_PROXY", &proxy_url)
            .env("HTTPS_PROXY", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("https_proxy", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .unwrap();
        let mut connections = 0;
        while proxy.accept().is_ok() {
            connections += 1;
        }
        let said = String::from_utf8_lossy(&child.stdout);
        assert!(child.status.success(), "the proxied run failed:\n{said}");
        assert!(
            said.contains("1 passed"),
            "the proxied run ran nothing:\n{said}"
        );
        assert_eq!(
            connections, 1,
            "the proxy should have seen the ordinary client alone"
        );
    }

    #[tokio::test]
    #[ignore = "run by a_proxy_named_in_the_environment_is_not_used"]
    async fn asked_with_a_proxy_in_the_environment() {
        assert_eq!(std::env::var(PROXIED).as_deref(), Ok("1"));
        let (url, seen) = api(StatusCode::OK, reported(serde_json::json!({}))).await;
        client(&url)
            .ask("p", "u", &schema())
            .await
            .expect("the client went straight to the API");
        assert_eq!(seen.lock().unwrap().len(), 1);
        // The ordinary client goes to the proxy, which never answers.
        let ordinary = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .post(format!("{url}/v1/messages"))
            .send()
            .await;
        assert!(ordinary.is_err() || !ordinary.unwrap().status().is_success());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_key_is_plain_header_text() {
        assert!(ModelKey::from_plaintext(b"").is_err());
        assert!(ModelKey::from_plaintext(b" \n").is_err());
        assert!(ModelKey::from_plaintext(b"a\nb").is_err());
        assert!(ModelKey::from_plaintext(&[0xff, 0xfe]).is_err());
        assert_eq!(format!("{:?}", key()), "ModelKey(redacted)");
    }
}
