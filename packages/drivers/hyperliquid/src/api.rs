//! HyperCore's HTTP API: the reads (`/info`) and the signed actions
//! (`/exchange`) the platform needs.
//!
//! The venue answers a refused action with a status of `err`, or with an
//! `error` per order, never an HTTP failure, so every answer is read and a
//! refusal returned as an error.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use alloy::signers::{Signer, local::PrivateKeySigner};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::signing::{
    Field, Network, SIGNATURE_CHAIN_ID, l1_action_digest, user_action_digest, wire_decimal,
};

/// How an error from HyperCore's own refusal begins, so a caller can tell
/// it from a request that got no answer.
pub const REFUSED: &str = "HyperCore refused the action";

/// How an error begins for an action that reached HyperCore and whose answer
/// was lost: it may have done what it asked.
pub const ARRIVED: &str = "the action reached HyperCore and its answer was lost";

/// How many times in all one signed action is sent to the exchange endpoint
/// while no answer comes back: the same bytes each time, so at most one of
/// them is ever taken.
pub const EXCHANGE_SENDS: usize = 3;

/// What came of sending one signed action to the exchange endpoint.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    /// The venue's answer to it: `status` is `ok` or `err`.
    Answered(Value),
    /// A later send of the same bytes was refused as a duplicate nonce: the
    /// action arrived, and the answer to it was lost.
    Arrived,
    /// No answer came back to any send: it may or may not have arrived.
    Unanswered(String),
}

/// Whether the venue's answer refuses a nonce it has already taken.
fn duplicate_nonce(answer: &Value) -> bool {
    if answer["status"] != "err" {
        return false;
    }
    let text = match &answer["response"] {
        Value::String(text) => text.to_ascii_lowercase(),
        other => other.to_string().to_ascii_lowercase(),
    };
    text.contains("nonce") && (text.contains("duplicate") || text.contains("already used"))
}

/// Where HyperCore answers, and which network it is.
#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    pub network: Network,
}

/// A token on HyperCore's spot market, as the venue numbers it on this
/// network (testnet and mainnet number them differently).
#[derive(Clone, Debug)]
pub struct SpotToken {
    pub name: String,
    pub index: u64,
    /// The id a send names it by, `0x` and 32 hex digits.
    pub token_id: String,
    pub sz_decimals: u32,
    pub wei_decimals: u32,
    /// Its pair against USDC: the pair's index, and the asset number an
    /// order names (10,000 plus the index).
    pub usdc_pair: Option<u64>,
}

impl SpotToken {
    /// How a send names the token: `NAME:0x…`.
    pub fn wire_name(&self) -> String {
        format!("{}:{}", self.name, self.token_id)
    }
}

/// Where a send of a spot token to HyperEVM goes: HYPE's own system address,
/// and for other tokens `0x20` followed by the token's index.
pub fn system_address(token: &SpotToken) -> Address {
    if token.name == "HYPE" {
        return Address::repeat_byte(0x22);
    }
    let mut bytes = [0u8; 20];
    bytes[0] = 0x20;
    bytes[12..].copy_from_slice(&token.index.to_be_bytes());
    Address::from(bytes)
}

impl Api {
    pub fn new(network: Network) -> Self {
        let base = match network {
            Network::Mainnet => "https://api.hyperliquid.xyz",
            Network::Testnet => "https://api.hyperliquid-testnet.xyz",
        };
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("an HTTP client with a timeout"),
            base: base.to_string(),
            network,
        }
    }

    /// The API at another address, for a stand-in of the venue.
    pub fn at(base: &str, network: Network) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            ..Self::new(network)
        }
    }

    /// The network HyperCore is for an EVM chain id: HyperEVM's mainnet
    /// (999) or testnet (998).
    pub fn for_chain(chain_id: u64) -> Option<Self> {
        match chain_id {
            999 => Some(Self::new(Network::Mainnet)),
            998 => Some(Self::new(Network::Testnet)),
            _ => None,
        }
    }

    /// One read, retried with a growing pause when the venue limits the
    /// rate.
    pub async fn info(&self, request: Value) -> Result<Value> {
        self.read("info", &request).await
    }

    /// Send one signed action to the exchange endpoint, and find out whether
    /// it arrived when its answer is lost.
    ///
    /// The action is sent once. Only when no answer comes back (the
    /// connection failed, the answer was cut off or unreadable, a gateway
    /// answered for the venue, or the venue turned it away for its rate) are
    /// the very same bytes sent again, at most [`EXCHANGE_SENDS`] times in
    /// all. The venue takes an action's nonce once, so a second send can
    /// never act a second time: it is either the action's first arrival,
    /// answered as such, or refused as a duplicate nonce, which says the
    /// first arrived.
    pub async fn deliver(&self, body: &Value) -> Delivery {
        let url = format!("{}/exchange", self.base);
        let mut pause = Duration::from_millis(1_000);
        let mut why = String::new();
        for send in 0..EXCHANGE_SENDS {
            if send > 0 {
                tokio::time::sleep(pause).await;
                pause *= 2;
            }
            let response = match self.http.post(&url).json(body).send().await {
                Ok(response) => response,
                Err(error) => {
                    why = format!("asking HyperCore: {error}");
                    continue;
                }
            };
            let status = response.status();
            let text = match response.text().await {
                Ok(text) => text,
                Err(error) => {
                    why = format!("reading HyperCore's answer: {error}");
                    continue;
                }
            };
            if status.as_u16() == 429 || status.is_server_error() {
                why = format!("HyperCore answered {status}: {}", cut(&text));
                continue;
            }
            if !status.is_success() {
                // The venue turned the request itself away (a body it can't
                // read): nothing was done.
                return Delivery::Answered(json!({
                    "status": "err",
                    "response": format!("HyperCore answered {status}: {}", cut(&text)),
                }));
            }
            let Ok(answer) = serde_json::from_str::<Value>(&text) else {
                why = format!("HyperCore's answer can't be read: {}", cut(&text));
                continue;
            };
            if answer["status"] != "ok" && answer["status"] != "err" {
                why = format!("HyperCore's answer can't be read: {}", cut(&text));
                continue;
            }
            if send > 0 && duplicate_nonce(&answer) {
                return Delivery::Arrived;
            }
            return Delivery::Answered(answer);
        }
        Delivery::Unanswered(why)
    }

    /// A read: asked again on a failed connection or a rate limit, which no
    /// read can do harm by.
    async fn read(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/{path}", self.base);
        let mut pause = Duration::from_millis(500);
        for attempt in 0.. {
            let answer = self.http.post(&url).json(body).send().await;
            match answer {
                Ok(response) if response.status().as_u16() == 429 && attempt < 6 => {}
                Ok(response) => {
                    let status = response.status();
                    let text = response
                        .text()
                        .await
                        .context("reading HyperCore's answer")?;
                    if !status.is_success() {
                        bail!("HyperCore answered {status}: {}", cut(&text));
                    }
                    return serde_json::from_str(&text)
                        .with_context(|| format!("HyperCore's answer: {}", cut(&text)));
                }
                Err(_) if attempt < 3 => {}
                Err(error) => return Err(error).context("asking HyperCore"),
            }
            tokio::time::sleep(pause).await;
            pause *= 2;
        }
        unreachable!()
    }

    /// Every spot token, with its pair against USDC where it has one.
    pub async fn spot_tokens(&self) -> Result<Vec<SpotToken>> {
        let meta = self.info(json!({"type": "spotMeta"})).await?;
        let tokens = meta["tokens"]
            .as_array()
            .ok_or_else(|| anyhow!("spotMeta has no tokens"))?;
        let universe = meta["universe"]
            .as_array()
            .ok_or_else(|| anyhow!("spotMeta has no universe"))?;
        let usdc = tokens
            .iter()
            .find(|token| token["name"] == "USDC")
            .and_then(|token| token["index"].as_u64())
            .ok_or_else(|| anyhow!("no USDC on HyperCore's spot market"))?;
        Ok(tokens
            .iter()
            .filter_map(|token| {
                let index = token["index"].as_u64()?;
                let usdc_pair = universe
                    .iter()
                    .find(|pair| {
                        pair["tokens"][0].as_u64() == Some(index)
                            && pair["tokens"][1].as_u64() == Some(usdc)
                    })
                    .and_then(|pair| pair["index"].as_u64());
                Some(SpotToken {
                    name: token["name"].as_str()?.to_string(),
                    index,
                    token_id: token["tokenId"].as_str()?.to_string(),
                    sz_decimals: token["szDecimals"].as_u64()? as u32,
                    wei_decimals: token["weiDecimals"].as_u64()? as u32,
                    usdc_pair,
                })
            })
            .collect())
    }

    /// The spot token the venue lists under `name`, if exactly one is (the
    /// testnet has look-alikes, so a name that two tokens share is refused).
    pub async fn spot_token(&self, name: &str) -> Result<SpotToken> {
        let mut found: Vec<SpotToken> = self
            .spot_tokens()
            .await?
            .into_iter()
            .filter(|token| token.name == name)
            .collect();
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => bail!("HyperCore lists no spot token named {name}"),
            n => bail!("HyperCore lists {n} spot tokens named {name}"),
        }
    }

    /// The best bid and ask of a book, by the coin name the venue uses
    /// (`@<pair>` for a spot pair).
    pub async fn best_bid_ask(&self, coin: &str) -> Result<(Option<f64>, Option<f64>)> {
        let book = self.info(json!({"type": "l2Book", "coin": coin})).await?;
        let price = |side: usize| {
            book["levels"][side][0]["px"]
                .as_str()
                .and_then(|px| px.parse::<f64>().ok())
        };
        Ok((price(0), price(1)))
    }

    /// An account's spot balance of a token, in whole tokens (`total`, which
    /// includes anything held by open orders).
    pub async fn spot_balance(&self, user: Address, token: &str) -> Result<f64> {
        let state = self
            .info(json!({"type": "spotClearinghouseState", "user": user}))
            .await?;
        Ok(state["balances"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|balance| balance["coin"] == token)
            .and_then(|balance| balance["total"].as_str())
            .and_then(|total| total.parse().ok())
            .unwrap_or(0.0))
    }

    /// An account's spot balance of a token as the driver reads it: the
    /// total, and how much of it open orders hold. A token the account has
    /// never held is not in the answer, and is zero; a balance whose figures
    /// can't be read is an error, never zero.
    pub async fn spot_holding(&self, user: Address, token: &str) -> Result<SpotHolding> {
        let state = self
            .info(json!({"type": "spotClearinghouseState", "user": user}))
            .await?;
        spot_holding_of(&state, token)
    }

    /// The venue's own price of a spot pair (`markPx`, the price HyperEVM's
    /// spot price precompile gives the driver), by the pair's coin name.
    pub async fn spot_price(&self, coin: &str) -> Result<crate::payload::Decimal> {
        let answer = self.info(json!({"type": "spotMetaAndAssetCtxs"})).await?;
        spot_price_of(&answer, coin)
    }

    /// What the venue records of one of `user`'s orders, by the venue's id
    /// or the client id it was placed with (`0x` and 32 hex digits): the
    /// record's `order` and `status`, or `None` when it knows no such order.
    pub async fn order_status(&self, user: Address, id: Value) -> Result<Option<Value>> {
        let answer = self
            .info(json!({"type": "orderStatus", "user": user, "oid": id}))
            .await?;
        match answer["status"].as_str() {
            Some("order") if answer["order"].is_object() => Ok(Some(answer["order"].clone())),
            Some("unknownOid") => Ok(None),
            _ => bail!(
                "HyperCore's order status can't be read: {}",
                cut(&answer.to_string())
            ),
        }
    }

    /// Every fill of `user`'s since `since_ms` (the venue's clock), oldest
    /// first, read in the venue's pages; at most `pages` pages, past which
    /// it is an error rather than a list cut short.
    pub async fn fills_since(
        &self,
        user: Address,
        since_ms: u64,
        pages: usize,
    ) -> Result<Vec<Value>> {
        let mut fills: Vec<Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut start = since_ms;
        for _ in 0..pages {
            let page = self
                .info(json!({"type": "userFillsByTime", "user": user, "startTime": start}))
                .await?;
            let page = page.as_array().ok_or_else(|| {
                anyhow!(
                    "HyperCore's fills can't be read: {}",
                    cut(&page.to_string())
                )
            })?;
            let mut last = None;
            for fill in page {
                let time = fill["time"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("a fill without a time: {fill}"))?;
                last = Some(last.map_or(time, |at: u64| at.max(time)));
                // A fill on the page boundary is on both pages: kept once.
                if seen.insert((fill["tid"].to_string(), time)) {
                    fills.push(fill.clone());
                }
            }
            match last {
                Some(last) if page.len() >= FILLS_PAGE => start = last,
                _ => {
                    fills.sort_by_key(|fill| fill["time"].as_u64().unwrap_or_default());
                    return Ok(fills);
                }
            }
        }
        bail!("the account has more fills since {since_ms} than {pages} pages of them")
    }

    /// What an account's perp side could withdraw, in dollars.
    pub async fn perp_withdrawable(&self, user: Address) -> Result<f64> {
        let state = self
            .info(json!({"type": "clearinghouseState", "user": user}))
            .await?;
        Ok(state["withdrawable"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0))
    }

    /// How many dollar deposits of exactly `millionths` HyperCore's ledger
    /// shows credited to `user` since `since_ms` (its own clock).
    pub async fn deposits_of(
        &self,
        user: Address,
        since_ms: u64,
        millionths: u64,
    ) -> Result<usize> {
        let ledger = self
            .info(json!({
                "type": "userNonFundingLedgerUpdates",
                "user": user,
                "startTime": since_ms,
            }))
            .await
            .context("reading the account's ledger on HyperCore")?;
        Ok(deposits_matching(&ledger, user, millionths))
    }

    /// [`Api::deposits_of`], as the entries themselves (see
    /// [`deposit_entries`]).
    pub async fn deposit_entries_of(
        &self,
        user: Address,
        since_ms: u64,
        millionths: u64,
    ) -> Result<Vec<String>> {
        let ledger = self
            .info(json!({
                "type": "userNonFundingLedgerUpdates",
                "user": user,
                "startTime": since_ms,
            }))
            .await
            .context("reading the account's ledger on HyperCore")?;
        if !ledger.is_array() {
            bail!(
                "HyperCore's ledger can't be read: {}",
                cut(&ledger.to_string())
            );
        }
        Ok(deposit_entries(&ledger, user, millionths))
    }

    /// Whether HyperCore knows the account at all: an address it has never
    /// credited can't act there, and its actions are refused.
    pub async fn account_exists(&self, user: Address) -> Result<bool> {
        let role = self.info(json!({"type": "userRole", "user": user})).await?;
        Ok(role["role"].as_str().is_some_and(|role| role != "missing"))
    }
}

/// An account on HyperCore that signs its own actions.
pub struct HyperCore {
    pub api: Api,
    signer: PrivateKeySigner,
    last_nonce: AtomicU64,
}

#[derive(Serialize)]
struct OrderAction {
    #[serde(rename = "type")]
    kind: &'static str,
    orders: Vec<OrderWire>,
    grouping: &'static str,
}

#[derive(Serialize)]
struct OrderWire {
    a: u64,
    b: bool,
    p: String,
    s: String,
    r: bool,
    t: OrderType,
}

#[derive(Serialize)]
struct OrderType {
    limit: Limit,
}

#[derive(Serialize)]
struct Limit {
    tif: &'static str,
}

#[derive(Serialize)]
struct EvmUserModify {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "usingBigBlocks")]
    using_big_blocks: bool,
}

/// What an immediate-or-cancel order did.
#[derive(Debug, Clone, Copy)]
pub struct Fill {
    pub size: f64,
    pub average_price: f64,
}

impl HyperCore {
    pub fn new(api: Api, signer: PrivateKeySigner) -> Self {
        Self {
            api,
            signer,
            last_nonce: AtomicU64::new(0),
        }
    }

    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// A nonce: the time in milliseconds, which the venue requires to be
    /// close to its own clock and new for the signer.
    fn nonce() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn sign(&self, digest: B256) -> Result<Value> {
        let signature = self.signer.sign_hash(&digest).await?;
        Ok(json!({
            "r": format!("0x{:064x}", signature.r()),
            "s": format!("0x{:064x}", signature.s()),
            "v": 27 + signature.v() as u8,
        }))
    }

    /// Send a signed action; its answer, or an error that begins with
    /// [`REFUSED`] only when the venue said it refused it. Any other error is
    /// an action that may have been done.
    async fn submit(&self, action: Value, nonce: u64, signature: Value) -> Result<Value> {
        let body =
            json!({"action": action, "nonce": nonce, "signature": signature, "vaultAddress": null});
        match self.api.deliver(&body).await {
            Delivery::Answered(answer) if answer["status"] == "ok" => {
                Ok(answer["response"].clone())
            }
            Delivery::Answered(answer) => bail!("{REFUSED}: {}", answer["response"]),
            Delivery::Arrived => bail!("{ARRIVED}"),
            Delivery::Unanswered(why) => {
                bail!("HyperCore never answered the action, which may still have been done: {why}")
            }
        }
    }

    /// Sign an L1 action with `nonce` and send it; what came of it. An error
    /// means it couldn't be signed, and nothing was sent.
    pub async fn deliver_l1<A: Serialize>(&self, action: &A, nonce: u64) -> Result<Delivery> {
        let digest = l1_action_digest(self.api.network, action, nonce)?;
        let signature = self.sign(digest).await?;
        let body = json!({
            "action": serde_json::to_value(action)?,
            "nonce": nonce,
            "signature": signature,
            "vaultAddress": null,
        });
        Ok(self.api.deliver(&body).await)
    }

    /// A nonce for this signer: the time in milliseconds, and always more
    /// than the last one it used, so two actions in one millisecond both
    /// go through.
    pub fn next_nonce(&self) -> u64 {
        let now = Self::nonce();
        let mut last = self.last_nonce.load(Ordering::Relaxed);
        loop {
            let next = now.max(last + 1);
            match self
                .last_nonce
                .compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return next,
                Err(seen) => last = seen,
            }
        }
    }

    async fn l1<A: Serialize>(&self, action: &A) -> Result<Value> {
        self.l1_at(action, self.next_nonce()).await
    }

    /// Sign and send an L1 action with `nonce`, for an action that names its
    /// own nonce too.
    pub async fn l1_at<A: Serialize>(&self, action: &A, nonce: u64) -> Result<Value> {
        let digest = l1_action_digest(self.api.network, action, nonce)?;
        let signature = self.sign(digest).await?;
        self.submit(serde_json::to_value(action)?, nonce, signature)
            .await
    }

    /// An immediate-or-cancel limit order on a spot pair, buying or selling
    /// `size` at no worse than `limit`. A partial fill is a fill; nothing
    /// filled is an error.
    pub async fn spot_ioc(&self, pair: u64, buy: bool, size: f64, limit: f64) -> Result<Fill> {
        let action = OrderAction {
            kind: "order",
            orders: vec![OrderWire {
                a: 10_000 + pair,
                b: buy,
                p: wire_decimal(limit),
                s: wire_decimal(size),
                r: false,
                t: OrderType {
                    limit: Limit { tif: "Ioc" },
                },
            }],
            grouping: "na",
        };
        let response = self.l1(&action).await?;
        let status = &response["data"]["statuses"][0];
        if let Some(error) = status["error"].as_str() {
            bail!("the order was refused: {error}");
        }
        let filled = &status["filled"];
        let number = |field: &str| {
            filled[field]
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())
        };
        match (number("totalSz"), number("avgPx")) {
            (Some(size), Some(average_price)) if size > 0.0 => Ok(Fill {
                size,
                average_price,
            }),
            _ => bail!("the order filled nothing: {status}"),
        }
    }

    /// Put this account's HyperEVM transactions into big blocks (or back
    /// into small ones), for deploying contracts larger than a small block.
    pub async fn use_big_blocks(&self, on: bool) -> Result<()> {
        self.l1(&EvmUserModify {
            kind: "evmUserModify",
            using_big_blocks: on,
        })
        .await
        .map(|_| ())
    }

    /// Move dollars between the account's perp and spot balances.
    pub async fn usd_class_transfer(&self, amount: f64, to_perp: bool) -> Result<()> {
        let nonce = Self::nonce();
        let network = self.api.network.chain_name();
        let amount = wire_decimal(amount);
        let digest = user_action_digest(
            "HyperliquidTransaction:UsdClassTransfer",
            "string hyperliquidChain,string amount,bool toPerp,uint64 nonce",
            &[
                Field::String(network),
                Field::String(&amount),
                Field::Bool(to_perp),
                Field::U64(nonce),
            ],
        );
        let signature = self.sign(digest).await?;
        self.submit(
            json!({
                "type": "usdClassTransfer",
                "signatureChainId": format!("0x{SIGNATURE_CHAIN_ID:x}"),
                "hyperliquidChain": network,
                "amount": amount,
                "toPerp": to_perp,
                "nonce": nonce,
            }),
            nonce,
            signature,
        )
        .await
        .map(|_| ())
    }

    /// Send dollars from the perp balance to another account on HyperCore,
    /// which also brings an account HyperCore has never seen into being.
    pub async fn usd_send(&self, to: Address, amount: f64) -> Result<()> {
        let time = Self::nonce();
        let network = self.api.network.chain_name();
        let destination = format!("{to:#x}");
        let amount = wire_decimal(amount);
        let digest = user_action_digest(
            "HyperliquidTransaction:UsdSend",
            "string hyperliquidChain,string destination,string amount,uint64 time",
            &[
                Field::String(network),
                Field::String(&destination),
                Field::String(&amount),
                Field::U64(time),
            ],
        );
        let signature = self.sign(digest).await?;
        self.submit(
            json!({
                "type": "usdSend",
                "signatureChainId": format!("0x{SIGNATURE_CHAIN_ID:x}"),
                "hyperliquidChain": network,
                "destination": destination,
                "amount": amount,
                "time": time,
            }),
            time,
            signature,
        )
        .await
        .map(|_| ())
    }

    /// Send a spot token to another account, or to a token's system address
    /// to move it to this account on HyperEVM.
    pub async fn spot_send(&self, to: Address, token: &SpotToken, amount: f64) -> Result<()> {
        let time = Self::nonce();
        let network = self.api.network.chain_name();
        let destination = format!("{to:#x}");
        let amount = wire_decimal(amount);
        let token = token.wire_name();
        let digest = user_action_digest(
            "HyperliquidTransaction:SpotSend",
            "string hyperliquidChain,string destination,string token,string amount,uint64 time",
            &[
                Field::String(network),
                Field::String(&destination),
                Field::String(&token),
                Field::String(&amount),
                Field::U64(time),
            ],
        );
        let signature = self.sign(digest).await?;
        self.submit(
            json!({
                "type": "spotSend",
                "signatureChainId": format!("0x{SIGNATURE_CHAIN_ID:x}"),
                "hyperliquidChain": network,
                "destination": destination,
                "token": token,
                "amount": amount,
                "time": time,
            }),
            time,
            signature,
        )
        .await
        .map(|_| ())
    }
}

/// How many fills the venue gives in one page of `userFillsByTime`.
pub const FILLS_PAGE: usize = 2_000;

/// A spot balance: the total, and how much of it open orders hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpotHolding {
    pub total: crate::payload::Decimal,
    pub hold: crate::payload::Decimal,
}

impl SpotHolding {
    /// What open orders don't hold: what the account can send or sell now.
    pub fn free(&self) -> crate::payload::Decimal {
        self.total - self.hold
    }
}

/// [`Api::spot_holding`], from the venue's `spotClearinghouseState` answer.
pub fn spot_holding_of(state: &Value, token: &str) -> Result<SpotHolding> {
    let balances = state["balances"].as_array().ok_or_else(|| {
        anyhow!(
            "HyperCore's spot balances can't be read: {}",
            cut(&state.to_string())
        )
    })?;
    let mut found = balances.iter().filter(|balance| balance["coin"] == token);
    let Some(balance) = found.next() else {
        return Ok(SpotHolding {
            total: crate::payload::Decimal::ZERO,
            hold: crate::payload::Decimal::ZERO,
        });
    };
    if found.next().is_some() {
        bail!("HyperCore gives two spot balances of {token}");
    }
    let figure = |field: &str| -> Result<crate::payload::Decimal> {
        let value: crate::payload::Decimal = balance[field]
            .as_str()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| {
                anyhow!("the spot balance of {token} has no readable {field}: {balance}")
            })?;
        if value.is_sign_negative() {
            bail!("the spot balance of {token} has a negative {field}: {balance}");
        }
        Ok(value)
    };
    let (total, hold) = (figure("total")?, figure("hold")?);
    if hold > total {
        bail!("the spot balance of {token} holds more than it has: {balance}");
    }
    Ok(SpotHolding { total, hold })
}

/// [`Api::spot_price`], from the venue's `spotMetaAndAssetCtxs` answer.
pub fn spot_price_of(answer: &Value, coin: &str) -> Result<crate::payload::Decimal> {
    let contexts = answer[1]
        .as_array()
        .ok_or_else(|| anyhow!("HyperCore's spot prices can't be read"))?;
    let mut found = contexts.iter().filter(|context| context["coin"] == coin);
    let context = match (found.next(), found.next()) {
        (Some(context), None) => context,
        (None, _) => bail!("HyperCore gives no spot price for {coin}"),
        _ => bail!("HyperCore gives two spot prices for {coin}"),
    };
    let price: crate::payload::Decimal = context["markPx"]
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| anyhow!("the spot price of {coin} can't be read: {context}"))?;
    if price <= crate::payload::Decimal::ZERO {
        bail!("the spot price of {coin} is not positive: {context}");
    }
    Ok(price)
}

fn cut(text: &str) -> &str {
    let end = text
        .char_indices()
        .nth(300)
        .map(|(at, _)| at)
        .unwrap_or(text.len());
    &text[..end]
}

/// How many entries of an account's ledger are dollars of exactly
/// `millionths` coming into it from outside: a deposit (from HyperEVM or a
/// bridge, whatever the venue calls it, as long as its type says deposit),
/// or a send whose destination is the account and whose source isn't. Its
/// own moves between perp and spot, sends out, withdrawals and trades are
/// not.
pub fn deposits_matching(ledger: &Value, user: Address, millionths: u64) -> usize {
    deposit_entries(ledger, user, millionths).len()
}

/// The entries [`deposits_matching`] counts, each as one string that tells
/// it from every other entry of the ledger (its hash and time when the venue
/// gives them, else the whole entry): so one credit is taken for one deposit
/// only.
pub fn deposit_entries(ledger: &Value, user: Address, millionths: u64) -> Vec<String> {
    let me = format!("{user:#x}");
    ledger
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            let delta = &entry["delta"];
            let kind = delta["type"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let lower = |field: &str| delta[field].as_str().map(str::to_ascii_lowercase);
            let incoming = kind.contains("deposit")
                || (lower("destination").as_deref() == Some(me.as_str())
                    && lower("user").as_deref() != Some(me.as_str())
                    && lower("source").as_deref() != Some(me.as_str())
                    && !kind.contains("class"));
            if !incoming || kind.contains("withdraw") {
                return false;
            }
            let dollars = delta["usdc"].as_str().or_else(|| {
                let token = delta["token"].as_str().unwrap_or_default();
                (token == "USDC" || token.starts_with("USDC:"))
                    .then(|| delta["amount"].as_str())
                    .flatten()
            });
            dollars.and_then(millionths_of) == Some(millionths)
        })
        .map(
            |entry| match (entry["hash"].as_str(), entry["time"].as_u64()) {
                (Some(hash), Some(time)) => format!("{hash}@{time}"),
                _ => entry.to_string(),
            },
        )
        .collect()
}

/// A decimal number of dollars as millionths, when it is a whole number of
/// them.
fn millionths_of(text: &str) -> Option<u64> {
    let (whole, fraction) = text.trim().split_once('.').unwrap_or((text.trim(), ""));
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > 6 || whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction: u64 = format!("{fraction:0<6}").parse().ok()?;
    whole.checked_mul(1_000_000)?.checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dollars_coming_into_the_account_count_as_its_deposit() {
        let me: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let other = "0x2222222222222222222222222222222222222222";
        let mine = format!("{me:#x}");
        let ledger = json!([
            {"time": 1, "delta": {"type": "deposit", "usdc": "650.0"}},
            {"time": 2, "delta": {"type": "send", "user": other, "destination": mine, "token": "USDC", "amount": "650"}},
            // Not deposits: its own perp-to-spot move, a send out, a withdrawal.
            {"time": 3, "delta": {"type": "accountClassTransfer", "usdc": "650.0", "toPerp": true}},
            {"time": 4, "delta": {"type": "send", "user": mine, "destination": other, "token": "USDC", "amount": "650"}},
            {"time": 5, "delta": {"type": "withdraw", "usdc": "650.0"}},
            // Another amount.
            {"time": 6, "delta": {"type": "deposit", "usdc": "649.999999"}},
        ]);
        assert_eq!(deposits_matching(&ledger, me, 650_000_000), 2);
        assert_eq!(deposits_matching(&ledger, me, 649_999_999), 1);
        assert_eq!(deposits_matching(&json!([]), me, 650_000_000), 0);
        assert_eq!(deposits_matching(&json!({"error": "x"}), me, 1), 0);
    }

    #[test]
    fn dollar_amounts_read_as_exact_millionths() {
        assert_eq!(millionths_of("650"), Some(650_000_000));
        assert_eq!(millionths_of("650.0"), Some(650_000_000));
        assert_eq!(millionths_of("0.000001"), Some(1));
        assert_eq!(millionths_of("20.40378700"), Some(20_403_787));
        assert_eq!(millionths_of("0.0000001"), None);
        assert_eq!(millionths_of("-1"), None);
        assert_eq!(millionths_of("x"), None);
    }

    #[test]
    fn a_spot_balance_is_its_total_less_what_orders_hold_and_never_read_as_zero_by_default() {
        let state = json!({"balances": [
            {"coin": "USDC", "token": 0, "total": "14.6", "hold": "0.0"},
            {"coin": "HYPE", "token": 150, "total": "0.30", "hold": "0.30"}
        ]});
        let hype = spot_holding_of(&state, "HYPE").unwrap();
        assert_eq!(hype.free(), "0".parse().unwrap());
        assert_eq!(hype.total, "0.3".parse().unwrap());
        // Never held: none.
        assert_eq!(
            spot_holding_of(&state, "PURR").unwrap().total,
            "0".parse().unwrap()
        );
        // A figure that can't be read is an error, not zero.
        for broken in [
            json!({"balances": [{"coin": "HYPE", "total": 0.3, "hold": "0"}]}),
            json!({"balances": [{"coin": "HYPE", "total": "0.3"}]}),
            json!({"balances": [{"coin": "HYPE", "total": "0.3", "hold": "0.4"}]}),
            json!({"error": "x"}),
        ] {
            assert!(spot_holding_of(&broken, "HYPE").is_err(), "{broken}");
        }
    }

    #[test]
    fn a_spot_price_is_the_venue_s_own_for_the_pair_s_coin() {
        let answer = json!([{}, [
            {"coin": "PURR/USDC", "markPx": "4.5795", "midPx": "4.6"},
            {"coin": "@1035", "markPx": "40.705", "midPx": "43.5745"}
        ]]);
        assert_eq!(
            spot_price_of(&answer, "@1035").unwrap(),
            "40.705".parse().unwrap()
        );
        assert!(spot_price_of(&answer, "@1").is_err());
        assert!(spot_price_of(&json!([{}, [{"coin": "@1", "markPx": 3.0}]]), "@1").is_err());
        assert!(spot_price_of(&json!([{}, [{"coin": "@1", "markPx": "0"}]]), "@1").is_err());
    }

    #[test]
    fn only_a_duplicate_nonce_answer_says_an_earlier_send_arrived() {
        assert!(duplicate_nonce(
            &json!({"status": "err", "response": "Invalid nonce: duplicate nonce"})
        ));
        assert!(!duplicate_nonce(
            &json!({"status": "err", "response": "Insufficient margin"})
        ));
        assert!(!duplicate_nonce(
            &json!({"status": "ok", "response": "duplicate nonce"})
        ));
    }

    fn token(name: &str, index: u64) -> SpotToken {
        SpotToken {
            name: name.to_string(),
            index,
            token_id: "0x00".to_string(),
            sz_decimals: 2,
            wei_decimals: 8,
            usdc_pair: None,
        }
    }

    #[test]
    fn hype_goes_to_hyperevm_through_its_own_system_address() {
        assert_eq!(
            system_address(&token("HYPE", 1105)),
            Address::repeat_byte(0x22)
        );
    }

    #[test]
    fn another_token_goes_through_the_address_its_index_names() {
        let address = system_address(&token("PURR", 0x1b));
        assert_eq!(address.as_slice()[0], 0x20);
        assert_eq!(address.as_slice()[19], 0x1b);
        assert!(address.as_slice()[1..19].iter().all(|byte| *byte == 0));
    }
}
