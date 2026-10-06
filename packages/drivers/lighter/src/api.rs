//! Lighter's REST API on Robinhood Chain.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::markets::Markets;

/// Lighter's instance on Robinhood Chain mainnet.
pub const MAINNET: &str = "https://api.rh.lighter.xyz";
/// Its testnet instance, whose dollar lives on Ethereum Sepolia rather than
/// on Robinhood Chain's testnet.
pub const TESTNET: &str = "https://api.rh-testnet.lighter.xyz";
/// The WebSocket each serves.
pub const MAINNET_STREAM: &str = "wss://api.rh.lighter.xyz/stream";
pub const TESTNET_STREAM: &str = "wss://api.rh-testnet.lighter.xyz/stream";

/// The chain id Lighter signs transactions for on each instance (its own
/// numbering, not the EVM chain's), as Lighter's integration docs give them.
pub const MAINNET_CHAIN_ID: u32 = 466_324;
pub const TESTNET_CHAIN_ID: u32 = 300;

/// The most any one request to the exchange's API may take, connection,
/// answer and all: a request the exchange never answers gives up, so neither
/// the next post nor the next order waits on it.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// The most making the connection may take, within that.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A transaction the exchange did not take: its own refusal, or a gateway
/// turning the request away before it reached the exchange. Nothing was
/// done. Any other failure of a send may have been taken.
#[derive(Debug)]
pub struct NotTaken(pub String);

impl std::fmt::Display for NotTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lighter did not take the transaction: {}", self.0)
    }
}

impl std::error::Error for NotTaken {}

/// Whether a failed send is one the exchange did not take.
pub fn not_taken(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NotTaken>().is_some()
}

#[derive(Clone, Debug)]
pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("an HTTP client with a timeout"),
        }
    }

    /// One read: an answer only if the HTTP status is a success and the
    /// exchange's own code in it is 200.
    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("asking Lighter {path}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .with_context(|| format!("reading Lighter's answer to {path}"))?;
        if !status.is_success() {
            bail!("Lighter answered {path} with {status}: {}", cut(&text));
        }
        let answer: Value = serde_json::from_str(&text)
            .with_context(|| format!("Lighter's answer to {path}: {}", cut(&text)))?;
        match answer["code"].as_u64() {
            Some(200) => Ok(answer),
            Some(code) => bail!(
                "Lighter refused {path} ({code}): {}",
                answer["message"].as_str().unwrap_or("no message")
            ),
            None => bail!("Lighter's answer to {path} has no code: {}", cut(&text)),
        }
    }

    pub async fn order_books(&self) -> Result<Value> {
        self.get("/api/v1/orderBooks").await
    }

    pub async fn markets(&self) -> Result<Markets> {
        Markets::from_order_books(&self.order_books().await?)
    }

    /// One market's details: its mark and index prices among them.
    pub async fn book_details(&self, market: u16) -> Result<Value> {
        self.get(&format!("/api/v1/orderBookDetails?market_id={market}"))
            .await
    }

    /// The best `limit` orders each side of a market's book.
    pub async fn book_orders(&self, market: u16, limit: u32) -> Result<Value> {
        self.get(&format!(
            "/api/v1/orderBookOrders?market_id={market}&limit={limit}"
        ))
        .await
    }

    /// An account by its index: exactly the one asked for.
    pub async fn account(&self, index: u64) -> Result<Value> {
        let answer = self
            .get(&format!("/api/v1/account?by=index&value={index}"))
            .await?;
        let account = answer["accounts"]
            .get(0)
            .cloned()
            .ok_or_else(|| anyhow!("no account {index}"))?;
        if account["index"].as_u64() != Some(index) {
            bail!(
                "asked for account {index}, Lighter answered with {}",
                account["index"]
            );
        }
        Ok(account)
    }

    /// Every account an address owns: its master and sub-accounts.
    pub async fn accounts_of(&self, owner: &str) -> Result<Value> {
        self.get(&format!("/api/v1/accountsByL1Address?l1_address={owner}"))
            .await
    }

    /// One transaction by its hash: what the exchange carried out, in which
    /// of its blocks, and when.
    pub async fn tx(&self, hash: &str) -> Result<Value> {
        if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("{hash:?} is not a transaction hash");
        }
        self.get(&format!("/api/v1/tx?by=hash&value={hash}")).await
    }

    pub async fn next_nonce(&self, account: u64, api_key: u8) -> Result<i64> {
        let answer = self
            .get(&format!(
                "/api/v1/nextNonce?account_index={account}&api_key_index={api_key}"
            ))
            .await?;
        answer["nonce"]
            .as_i64()
            .ok_or_else(|| anyhow!("no nonce in {answer}"))
    }

    /// Send a signed transaction; the exchange's hash for it, only when the
    /// exchange said it took it (a success, its own code 200, and a hash).
    /// Its refusal, or a gateway's 429, is [`NotTaken`]: nothing was done.
    /// Anything else (no answer, a server error, an answer that can't be
    /// read) may have been taken.
    pub async fn send_tx(&self, tx_type: u8, tx_info: &str) -> Result<String> {
        let url = format!("{}/api/v1/sendTx", self.base);
        let form = reqwest::multipart::Form::new()
            .text("tx_type", tx_type.to_string())
            .text("tx_info", tx_info.to_string());
        let response = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .context("sending a transaction to Lighter, which may have taken it")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("reading Lighter's answer to a transaction, which it may have taken")?;
        sent(status.as_u16(), &text)
    }
}

/// Lighter's public explorer API for each instance: the exchange's batches
/// and blocks, and each account's logs.
pub const MAINNET_EXPLORER: &str = "https://explorerapi.rh.lighter.xyz/api";
pub const TESTNET_EXPLORER: &str = "https://explorerapi.rh-testnet.lighter.xyz/api";

/// The most withdrawals asked of the explorer at once (its own limit).
pub const EXPLORER_PAGE: u64 = 100;

/// Lighter's explorer API. Its answers pass through a cache in front of it,
/// so every read carries a mark of its own and asks for no cached answer:
/// what is read is what the explorer knows now.
#[derive(Clone, Debug)]
pub struct Explorer {
    base: String,
    http: reqwest::Client,
}

impl Explorer {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("an HTTP client with a timeout"),
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let fresh = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let joiner = if path.contains('?') { '&' } else { '?' };
        let url = format!("{}{path}{joiner}fresh={fresh}", self.base);
        let response = self
            .http
            .get(&url)
            .header("cache-control", "no-cache")
            .send()
            .await
            .with_context(|| format!("asking Lighter's explorer {path}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .with_context(|| format!("reading Lighter's explorer's answer to {path}"))?;
        if !status.is_success() {
            bail!(
                "Lighter's explorer answered {path} with {status}: {}",
                cut(&text)
            );
        }
        serde_json::from_str(&text)
            .with_context(|| format!("Lighter's explorer's answer to {path}: {}", cut(&text)))
    }

    /// An account's withdrawal logs, newest first, from the `offset`th:
    /// at most [`EXPLORER_PAGE`].
    pub async fn withdrawals(&self, account: u64, offset: u64) -> Result<Vec<Value>> {
        let kinds = crate::withdrawals::WITHDRAW_LOGS.join(",");
        let answer = self
            .get(&format!(
                "/accounts/{account}/logs?pub_data_type={kinds}&limit={EXPLORER_PAGE}&offset={offset}"
            ))
            .await?;
        match answer {
            Value::Array(logs) => Ok(logs),
            other => bail!(
                "Lighter's explorer's list of account {account}'s withdrawals can't be read: {}",
                cut(&other.to_string())
            ),
        }
    }

    /// One batch, with every block in it.
    pub async fn batch(&self, number: u64) -> Result<Value> {
        self.get(&format!("/batches/{number}")).await
    }

    /// The newest blocks the explorer has.
    pub async fn newest_blocks(&self) -> Result<Value> {
        self.get("/blocks?limit=1").await
    }
}

/// What an answer to a sent transaction says: see [`Api::send_tx`].
pub fn sent(status: u16, text: &str) -> Result<String> {
    let answer: Option<Value> = serde_json::from_str(text).ok();
    let code = answer.as_ref().and_then(|answer| answer["code"].as_u64());
    let message = answer
        .as_ref()
        .and_then(|answer| answer["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| cut(text).to_string());
    match status {
        429 => return Err(NotTaken(format!("turned away for its rate (429): {message}")).into()),
        200..=299 => {}
        // The exchange read the request and refused it.
        400..=499 if code.is_some_and(|code| code != 200) => {
            return Err(
                NotTaken(format!("{status}, code {}: {message}", code.unwrap_or(0))).into(),
            );
        }
        _ => bail!("Lighter answered {status}, and may have taken the transaction: {message}"),
    }
    match (
        code,
        answer
            .as_ref()
            .and_then(|answer| answer["tx_hash"].as_str()),
    ) {
        (Some(200), Some(hash)) if !hash.is_empty() => Ok(hash.to_string()),
        (Some(200), _) => {
            bail!(
                "Lighter took the transaction and gave no hash for it: {}",
                cut(text)
            )
        }
        (Some(code), _) => Err(NotTaken(format!("code {code}: {message}")).into()),
        (None, _) => bail!(
            "Lighter's answer can't be read, and it may have taken the transaction: {}",
            cut(text)
        ),
    }
}

fn cut(text: &str) -> &str {
    let end = text
        .char_indices()
        .nth(300)
        .map(|(at, _)| at)
        .unwrap_or(text.len());
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transaction_is_sent_only_when_the_exchange_said_it_took_it() {
        assert_eq!(
            sent(200, r#"{"code":200,"tx_hash":"0xabc"}"#).unwrap(),
            "0xabc"
        );
        // Turned away by a gateway, or refused by the exchange: not taken.
        for (status, body) in [
            (429, r#"{"message":"Too Many Requests"}"#),
            (429, "slow down"),
            (200, r#"{"code":21120,"message":"invalid nonce"}"#),
            (400, r#"{"code":21120,"message":"invalid nonce"}"#),
        ] {
            let error = sent(status, body).unwrap_err();
            assert!(not_taken(&error), "{status} {body}");
        }
        // Not sent, and it may have been taken.
        for (status, body) in [
            (502, r#"{"error":"upstream unavailable"}"#),
            (504, ""),
            (200, "not json"),
            (200, r#"{"code":200}"#),
            (200, r#"{"code":200,"tx_hash":""}"#),
            (400, "bad request"),
        ] {
            let error = sent(status, body).unwrap_err();
            assert!(!not_taken(&error), "{status} {body}");
        }
    }
}
