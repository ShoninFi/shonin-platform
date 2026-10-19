//! Asking Confidential Space's launcher for an attestation token, over the
//! unix socket only the workload's container reaches. The same request the
//! executor makes for its blessings, with the exchange's nonce.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

/// Where Confidential Space's launcher answers a workload's token requests.
pub const LAUNCHER_SOCKET: &str = "/run/container_launcher/teeserver.sock";

/// The audience the token is asked for, and the only one the checks accept.
pub const AUDIENCE: &str = shonin_attestation_core::AUDIENCE;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The kind of token every request asks for: a PKI token, whose header
/// carries its signing key's certificate chain up to Google's Confidential
/// Space Root CA. A blessing's proof and the root key exchange both trust it
/// by that root, so neither depends on the keys Google rotates.
pub const TOKEN_TYPE: &str = "PKI";

/// Ask the launcher at `socket` for a PKI token of our audience whose nonce
/// is `nonce`.
pub async fn request_token(socket: &Path, nonce: &str) -> Result<String> {
    use http_body_util::BodyExt;

    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to the launcher at {}", socket.display()))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .context("speaking HTTP to the launcher")?;
    let _connection = AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let body = serde_json::json!({
        "audience": AUDIENCE,
        "token_type": TOKEN_TYPE,
        "nonces": [nonce],
    })
    .to_string();
    let request = http::Request::post("/v1/token")
        .header(http::header::HOST, "localhost")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from(body)))?;
    let response = tokio::time::timeout(Duration::from_secs(30), sender.send_request(request))
        .await
        .context("the launcher did not answer")?
        .context("asking the launcher for a token")?;
    let status = response.status();
    let body = tokio::time::timeout(Duration::from_secs(30), response.into_body().collect())
        .await
        .context("the launcher did not finish its answer")?
        .context("reading the launcher's answer")?
        .to_bytes();
    anyhow::ensure!(body.len() < 64 * 1024, "the launcher's answer is too long");
    let text = String::from_utf8(body.to_vec()).context("the launcher's answer is not text")?;
    anyhow::ensure!(
        status.is_success(),
        "the launcher refused the token request ({status})"
    );
    Ok(text.trim().to_string())
}

/// Where Google publishes Confidential Space's signing keys: the `jwks_uri`
/// of the issuer's OpenID configuration.
pub const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/service_accounts/v1/metadata/jwk/signer@confidentialspace-sign.iam.gserviceaccount.com";

/// Google's signing keys as published now. Nothing fetched here is trusted
/// by itself: a key signs a token for an exchange only if an allowed checker
/// on the canonical chain trusts its hash.
pub async fn fetch_google_keys(
    client: &reqwest::Client,
) -> Result<Vec<shonin_attestation_core::Jwk>> {
    let text = client
        .get(GOOGLE_JWKS_URL)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("fetching Google's signing keys")?
        .error_for_status()
        .context("fetching Google's signing keys")?
        .text()
        .await
        .context("reading Google's signing keys")?;
    shonin_attestation_core::Jwk::from_jwks(&text)
        .map_err(|error| anyhow::anyhow!("Google's signing keys: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test]
    async fn every_token_is_asked_for_as_a_pki_token_of_our_audience_and_nonce() {
        let dir = std::env::temp_dir().join(format!("shonin-token-type-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("teeserver.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let asked: Arc<Mutex<Vec<serde_json::Value>>> = Default::default();
        let noted = asked.clone();
        let app = axum::Router::new().route(
            "/v1/token",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let noted = noted.clone();
                async move {
                    noted.lock().unwrap().push(body);
                    "a.b.c"
                }
            }),
        );
        let _server = AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));

        assert_eq!(request_token(&socket, "n1").await.unwrap(), "a.b.c");
        let asked = asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            [serde_json::json!({"audience": AUDIENCE, "token_type": "PKI", "nonces": ["n1"]})]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
