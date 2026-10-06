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
        self.post("info", &request).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
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

    async fn submit(&self, action: Value, nonce: u64, signature: Value) -> Result<Value> {
        let answer = self
            .api
            .post(
                "exchange",
                &json!({"action": action, "nonce": nonce, "signature": signature, "vaultAddress": null}),
            )
            .await?;
        if answer["status"] != "ok" {
            bail!("{REFUSED}: {}", answer["response"]);
        }
        Ok(answer["response"].clone())
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
        .count()
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
