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

#[derive(Clone, Debug)]
pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::new(),
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let answer: Value = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("asking Lighter {path}"))?
            .json()
            .await?;
        check(&answer)?;
        Ok(answer)
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

    /// An account by its index.
    pub async fn account(&self, index: u64) -> Result<Value> {
        let answer = self
            .get(&format!("/api/v1/account?by=index&value={index}"))
            .await?;
        answer["accounts"]
            .get(0)
            .cloned()
            .ok_or_else(|| anyhow!("no account {index}"))
    }

    /// Every account an address owns: its master and sub-accounts.
    pub async fn accounts_of(&self, owner: &str) -> Result<Value> {
        self.get(&format!("/api/v1/accountsByL1Address?l1_address={owner}"))
            .await
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

    /// Send a signed transaction; the exchange's hash for it.
    pub async fn send_tx(&self, tx_type: u8, tx_info: &str) -> Result<String> {
        let url = format!("{}/api/v1/sendTx", self.base);
        let form = reqwest::multipart::Form::new()
            .text("tx_type", tx_type.to_string())
            .text("tx_info", tx_info.to_string());
        let answer: Value = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .context("sending a transaction to Lighter")?
            .json()
            .await?;
        check(&answer)?;
        Ok(answer["tx_hash"].as_str().unwrap_or_default().to_string())
    }
}

/// Lighter answers 200 with a `code` of its own; anything but 200 is a
/// refusal.
fn check(answer: &Value) -> Result<()> {
    match answer["code"].as_u64() {
        Some(200) | None => Ok(()),
        Some(code) => bail!(
            "Lighter refused ({code}): {}",
            answer["message"].as_str().unwrap_or("no message")
        ),
    }
}
