//! The driver's executor half: what the agent key signs for a vault's
//! account, and nothing else.
//!
//! The station on HyperEVM is the account's master and has approved this
//! key as its named agent. An agent can trade, and the venue stops it
//! withdrawing or sending anywhere; but it could also park the account's
//! money in a venue vault, or change the account's balance mode, and the
//! venue would accept both. So this half signs only what an [`Action`] can
//! express: orders, cancels and modifications on markets the vault's driver
//! lists, cross-margin leverage, dollars between the account's own perp and
//! spot balances, and a scheduled cancel. Every wire action it can produce is
//! one of [`SIGNABLE`]; there is no way to ask it for anything else.

use std::collections::BTreeSet;

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

use crate::api::{Api, Delivery, HyperCore};
use crate::markets::{Kind, Market, Markets};
use crate::payload::{Action, Balance, Decimal, Order, OrderRef, Receipt, Side, Tif};

/// Every type of action the agent key ever signs.
pub const SIGNABLE: [&str; 7] = [
    "order",
    "cancel",
    "cancelByCloid",
    "modify",
    "updateLeverage",
    "agentSendAsset",
    "scheduleCancel",
];

/// A decoded, checked action, ready to sign: exactly one of the venue's wire
/// formats, with the fields in the order the venue hashes them.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Wire {
    Order(OrderAction),
    Cancel(CancelAction),
    CancelByCloid(CancelByCloidAction),
    Modify(ModifyAction),
    Leverage(LeverageAction),
    Move(MoveAction),
    Schedule(ScheduleAction),
}

impl Wire {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Order(_) => "order",
            Self::Cancel(_) => "cancel",
            Self::CancelByCloid(_) => "cancelByCloid",
            Self::Modify(_) => "modify",
            Self::Leverage(_) => "updateLeverage",
            Self::Move(_) => "agentSendAsset",
            Self::Schedule(_) => "scheduleCancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderAction {
    #[serde(rename = "type")]
    kind: &'static str,
    orders: Vec<OrderWire>,
    grouping: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderWire {
    a: u32,
    b: bool,
    p: String,
    s: String,
    r: bool,
    t: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    c: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OrderType {
    limit: Limit,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Limit {
    tif: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelAction {
    #[serde(rename = "type")]
    kind: &'static str,
    cancels: Vec<CancelWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CancelWire {
    a: u32,
    o: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelByCloidAction {
    #[serde(rename = "type")]
    kind: &'static str,
    cancels: Vec<CancelByCloidWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CancelByCloidWire {
    asset: u32,
    cloid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
enum Oid {
    Venue(u64),
    Client(String),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModifyAction {
    #[serde(rename = "type")]
    kind: &'static str,
    oid: Oid,
    order: OrderWire,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeverageAction {
    #[serde(rename = "type")]
    kind: &'static str,
    asset: u32,
    #[serde(rename = "isCross")]
    is_cross: bool,
    leverage: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoveAction {
    #[serde(rename = "type")]
    kind: &'static str,
    destination: String,
    #[serde(rename = "sourceDex")]
    source_dex: String,
    #[serde(rename = "destinationDex")]
    destination_dex: String,
    token: String,
    amount: String,
    #[serde(rename = "fromSubAccount")]
    from_sub_account: String,
    nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScheduleAction {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<u64>,
}

/// The perp dex's name on the wire, and the spot balance's.
const PERP_DEX: &str = "";
const SPOT_DEX: &str = "spot";

/// What the agent key may sign for one vault's account.
pub struct Allowlist {
    /// The account: the station.
    pub master: Address,
    pub markets: Markets,
    /// The markets the vault's driver lists, by the agent's names.
    pub allowed: BTreeSet<String>,
}

impl Allowlist {
    /// The markets a deployed driver lists, by the venue's numbers (its
    /// `perps()` and `spots()`), named as an agent names them. A number the
    /// venue doesn't list is refused.
    pub fn from_driver(
        master: Address,
        markets: Markets,
        perps: &[u32],
        spot_pairs: &[u32],
    ) -> Result<Self> {
        let mut allowed = BTreeSet::new();
        for index in perps {
            let market = markets
                .perp(*index)
                .ok_or_else(|| anyhow!("the venue lists no tradable perp {index}"))?;
            allowed.insert(market.name.clone());
        }
        for pair in spot_pairs {
            let market = markets
                .spot_pair(*pair)
                .ok_or_else(|| anyhow!("the venue lists no spot pair {pair} against the dollar"))?;
            allowed.insert(market.name.clone());
        }
        Ok(Self {
            master,
            markets,
            allowed,
        })
    }

    fn market(&self, name: &str) -> Result<&Market> {
        if !self.allowed.contains(name) {
            bail!("{name} is not a market this vault's driver lists");
        }
        self.markets
            .get(name)
            .ok_or_else(|| anyhow!("the venue doesn't list {name}"))
    }

    /// `order` as the venue's wire writes it. Every order carries a client
    /// id, so a placement whose answer is lost can be looked up: the agent's
    /// own, or else one made from the account and the nonce, which no other
    /// order of the account's shares. An agent's id may not carry the tag
    /// the executor marks its own upkeep orders with.
    fn order_wire(&self, order: &Order, nonce: u64, upkeep: bool) -> Result<OrderWire> {
        let client_id = match (order.client_id, upkeep) {
            (_, true) => upkeep_client_id(nonce),
            (Some(id), false) => id,
            (None, false) => self.client_id_for(nonce),
        };
        self.wire_of(order, Some(client_id))
    }

    /// The order a modification puts in place of another: with the agent's
    /// client id, if it gave one.
    fn modified_wire(&self, order: &Order) -> Result<OrderWire> {
        self.wire_of(order, order.client_id)
    }

    fn wire_of(&self, order: &Order, client_id: Option<u128>) -> Result<OrderWire> {
        let market = self.market(&order.market)?;
        if order.reduce_only && market.is_spot() {
            bail!("a spot order can't be reduce-only");
        }
        if order.client_id.is_some_and(is_upkeep) {
            bail!("client ids beginning {UPKEEP_TAG:#018x} are the executor's own");
        }
        Ok(OrderWire {
            a: market.asset,
            b: order.side == Side::Buy,
            p: market.wire_price(order.limit_price)?,
            s: market.wire_size(order.size)?,
            r: order.reduce_only,
            t: OrderType {
                limit: Limit {
                    tif: match order.tif {
                        Tif::PostOnly => "Alo",
                        Tif::Gtc => "Gtc",
                        Tif::Ioc => "Ioc",
                    },
                },
            },
            c: client_id.map(cloid),
        })
    }

    /// A client id for an order the agent gave none: the account and the
    /// nonce, hashed, never one with the upkeep tag.
    fn client_id_for(&self, nonce: u64) -> u128 {
        let mut seed = self.master.to_vec();
        seed.extend_from_slice(&nonce.to_be_bytes());
        let hash = alloy::primitives::keccak256(&seed);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash[..16]);
        let id = u128::from_be_bytes(bytes);
        if is_upkeep(id) { id ^ (1 << 127) } else { id }
    }

    /// Check `action` and turn it into the one wire action it allows.
    /// `nonce` is the nonce it will be signed with, which a move names.
    pub fn check(&self, action: &Action, nonce: u64) -> Result<Wire> {
        self.check_as(action, nonce, false)
    }

    /// The same, for the executor's own upkeep (`upkeep`): its orders carry
    /// the upkeep tag in their client id, which an agent's may not.
    fn check_as(&self, action: &Action, nonce: u64, upkeep: bool) -> Result<Wire> {
        Ok(match action {
            Action::Place(order) => Wire::Order(OrderAction {
                kind: "order",
                orders: vec![self.order_wire(order, nonce, upkeep)?],
                grouping: "na",
            }),
            Action::Cancel {
                market,
                order: OrderRef::Venue(oid),
            } => Wire::Cancel(CancelAction {
                kind: "cancel",
                cancels: vec![CancelWire {
                    a: self.market(market)?.asset,
                    o: *oid,
                }],
            }),
            Action::Cancel {
                market,
                order: OrderRef::Client(id),
            } => Wire::CancelByCloid(CancelByCloidAction {
                kind: "cancelByCloid",
                cancels: vec![CancelByCloidWire {
                    asset: self.market(market)?.asset,
                    cloid: cloid(*id),
                }],
            }),
            Action::Modify { order_ref, order } => Wire::Modify(ModifyAction {
                kind: "modify",
                oid: match order_ref {
                    OrderRef::Venue(oid) => Oid::Venue(*oid),
                    OrderRef::Client(id) => Oid::Client(cloid(*id)),
                },
                order: self.modified_wire(order)?,
            }),
            Action::Leverage { market, leverage } => {
                let market = self.market(market)?;
                let Kind::Perp { max_leverage } = market.kind else {
                    bail!("{} is spot: it has no leverage", market.name);
                };
                if *leverage == 0 || *leverage > max_leverage {
                    bail!("{} allows leverage 1 to {max_leverage}", market.name);
                }
                Wire::Leverage(LeverageAction {
                    kind: "updateLeverage",
                    asset: market.asset,
                    is_cross: true,
                    leverage: *leverage,
                })
            }
            Action::MoveDollars { from, amount } => {
                let (source, destination) = match from {
                    Balance::Perp => (PERP_DEX, SPOT_DEX),
                    Balance::Spot => (SPOT_DEX, PERP_DEX),
                };
                let amount = crate::markets::plain(*amount)?;
                if amount.split_once('.').is_some_and(|(_, f)| f.len() > 6) {
                    bail!("dollars move in millionths at most, not {amount}");
                }
                Wire::Move(MoveAction {
                    kind: "agentSendAsset",
                    // Only ever the account itself: the venue also refuses
                    // any other destination for an agent.
                    destination: format!("{:#x}", self.master),
                    source_dex: source.to_string(),
                    destination_dex: destination.to_string(),
                    token: self.markets.usdc_wire.clone(),
                    amount,
                    from_sub_account: String::new(),
                    nonce,
                })
            }
            Action::CancelAllAt { at_ms } => Wire::Schedule(ScheduleAction {
                kind: "scheduleCancel",
                time: *at_ms,
            }),
        })
    }
}

/// An agent's client order id as the venue writes it.
fn cloid(id: u128) -> String {
    format!("0x{id:032x}")
}

/// The top 64 bits of the client id of every order the executor places for
/// its own upkeep (HYPE for sends home), and of no agent's: so the venue's
/// own record of the account's fills tells the upkeep's buys apart, after a
/// restart too.
pub const UPKEEP_TAG: u64 = u64::from_be_bytes(*b"shnnUPKP");

/// The client id of an upkeep order signed with `nonce`.
pub fn upkeep_client_id(nonce: u64) -> u128 {
    (u128::from(UPKEEP_TAG) << 64) | u128::from(nonce)
}

/// Whether a client id carries the upkeep tag.
pub fn is_upkeep(id: u128) -> bool {
    (id >> 64) as u64 == UPKEEP_TAG
}

/// Whether a client id as the venue writes it (`0x` and 32 hex digits)
/// carries the upkeep tag.
pub fn is_upkeep_cloid(text: &str) -> bool {
    text.strip_prefix("0x")
        .and_then(|hex| u128::from_str_radix(hex, 16).ok())
        .is_some_and(is_upkeep)
}

/// The agent key at work for one vault's account.
pub struct Trader {
    core: HyperCore,
    pub allowlist: Allowlist,
}

impl Trader {
    pub fn new(api: Api, agent: PrivateKeySigner, allowlist: Allowlist) -> Self {
        Self {
            core: HyperCore::new(api, agent),
            allowlist,
        }
    }

    /// The agent key's address: what the station approves.
    pub fn agent(&self) -> Address {
        self.core.address()
    }

    /// Decode one payload a strategy submitted.
    pub fn decode(payload: &[u8]) -> Result<Action> {
        ciborium::from_reader(payload).map_err(|error| anyhow!("not a hyperliquid action: {error}"))
    }

    /// Check, sign and send one action; what the venue said.
    pub async fn submit(&self, action: &Action) -> Result<Receipt> {
        match self.outcome(action).await {
            Outcome::Accepted(receipt) => Ok(receipt),
            Outcome::Refused(why) | Outcome::Unknown(why) => Err(anyhow!(why)),
        }
    }

    /// The same, told apart as the agent must hear it: refused (nothing
    /// happened), or sent with no answer (it may still land).
    pub async fn outcome(&self, action: &Action) -> Outcome {
        self.outcome_as(action, false).await
    }

    /// One of the executor's own upkeep orders (never an agent's): placed
    /// with the upkeep tag in its client id, and told apart as an agent's
    /// order is. The client id it was placed with comes back beside it.
    pub async fn upkeep(&self, order: &Order) -> (Outcome, u128) {
        let (outcome, nonce) = self
            .outcome_nonce(&Action::Place(order.clone()), true)
            .await;
        (outcome, upkeep_client_id(nonce))
    }

    async fn outcome_as(&self, action: &Action, upkeep: bool) -> Outcome {
        self.outcome_nonce(action, upkeep).await.0
    }

    async fn outcome_nonce(&self, action: &Action, upkeep: bool) -> (Outcome, u64) {
        let nonce = self.core.next_nonce();
        let wire = match self.allowlist.check_as(action, nonce, upkeep) {
            Ok(wire) => wire,
            Err(why) => return (Outcome::Refused(why.to_string()), nonce),
        };
        debug_assert!(SIGNABLE.contains(&wire.kind()));
        let delivery = match self.core.deliver_l1(&wire, nonce).await {
            Ok(delivery) => delivery,
            // It couldn't be signed: nothing was sent.
            Err(why) => return (Outcome::Refused(format!("{why:#}")), nonce),
        };
        let outcome = match delivery {
            Delivery::Answered(answer) if answer["status"] == "ok" => {
                match receipt(&wire, &answer["response"]) {
                    Read::Receipt(receipt) => Outcome::Accepted(receipt),
                    Read::Refused(why) => Outcome::Refused(why),
                    // The venue took the action and its answer can't be
                    // read: what it did is looked up like a lost answer.
                    Read::Unreadable(why) => self.look_up(action, &wire, nonce, false, why).await,
                }
            }
            Delivery::Answered(answer) => {
                Outcome::Refused(format!("{}: {}", crate::api::REFUSED, answer["response"]))
            }
            Delivery::Arrived => {
                self.look_up(action, &wire, nonce, true, crate::api::ARRIVED.to_string())
                    .await
            }
            Delivery::Unanswered(why) => {
                let why =
                    format!("HyperCore never answered, so the action may still be done: {why}");
                self.look_up(action, &wire, nonce, false, why).await
            }
        };
        (outcome, nonce)
    }

    /// What became of an action whose answer was lost (`arrived`: the venue
    /// is known to have taken its nonce), from the venue's own record: an
    /// order by its client id, and its fills; a cancel by the order it
    /// cancels. Refused only when the record says nothing happened; unknown
    /// when the record can't tell.
    async fn look_up(
        &self,
        action: &Action,
        wire: &Wire,
        nonce: u64,
        arrived: bool,
        why: String,
    ) -> Outcome {
        let unknown = |found: &str| Outcome::Unknown(format!("{why}; {found}"));
        let master = self.allowlist.master;
        let api = &self.core.api;
        match (action, wire) {
            (Action::Place(order), Wire::Order(placed)) => {
                let Some(sent) = placed.orders.first() else {
                    return unknown("no order was in it");
                };
                let Some(client) = sent.c.clone() else {
                    return unknown("it had no client id to look it up by");
                };
                let record = match api
                    .order_status(master, Value::String(client.clone()))
                    .await
                {
                    Ok(Some(record)) => record,
                    Ok(None) => {
                        return unknown(if arrived {
                            "the venue has no order under its client id, and can't say what it did"
                        } else {
                            "the venue has no order under its client id yet"
                        });
                    }
                    Err(error) => {
                        return unknown(&format!("its order couldn't be looked up: {error:#}"));
                    }
                };
                let market = match self.allowlist.markets.get(&order.market) {
                    Some(market) => market,
                    None => return unknown("its market is no longer listed"),
                };
                if !same_order(&record["order"], sent, market, nonce) {
                    return unknown("the order under its client id is not this one");
                }
                let Some(oid) = record["order"]["oid"].as_u64() else {
                    return unknown("the venue's record of the order has no id");
                };
                let status = record["status"].as_str().unwrap_or_default();
                let filled = || async { filled_of(api, master, oid, nonce).await };
                match status {
                    "open" => Outcome::Accepted(Receipt::Resting { order_id: oid }),
                    "filled" => match filled().await {
                        Ok(Some((size, average_price))) => Outcome::Accepted(Receipt::Filled {
                            order_id: oid,
                            size,
                            average_price,
                        }),
                        Ok(None) => {
                            unknown("the venue says it filled, and shows no fill of it yet")
                        }
                        Err(error) => unknown(&format!(
                            "it filled, and its fills couldn't be read: {error:#}"
                        )),
                    },
                    status if status.ends_with("Rejected") || status == "rejected" => {
                        Outcome::Refused(format!("the venue refused the order: {status}"))
                    }
                    status
                        if status.ends_with("Canceled")
                            || status == "canceled"
                            || status == "scheduledCancel" =>
                    {
                        match filled().await {
                            Ok(Some((size, average_price))) => Outcome::Accepted(Receipt::Filled {
                                order_id: oid,
                                size,
                                average_price,
                            }),
                            // Nothing of it filled and it is off the book:
                            // an order that never rested did nothing; one
                            // that rested was placed, and its cancel is the
                            // account's news.
                            Ok(None) if order.tif == Tif::Ioc => Outcome::Refused(format!(
                                "the venue took the order and cancelled it with nothing filled ({status})"
                            )),
                            Ok(None) => Outcome::Accepted(Receipt::Resting { order_id: oid }),
                            Err(error) => unknown(&format!(
                                "it was cancelled, and its fills couldn't be read: {error:#}"
                            )),
                        }
                    }
                    other => unknown(&format!("the venue says the order is {other}")),
                }
            }
            (Action::Cancel { order, .. }, _) => {
                let id = match order {
                    OrderRef::Venue(oid) => Value::from(*oid),
                    OrderRef::Client(id) => Value::String(cloid(*id)),
                };
                match api.order_status(master, id).await {
                    Ok(Some(record)) => {
                        let status = record["status"].as_str().unwrap_or_default();
                        if status.ends_with("Canceled") || status == "canceled" {
                            Outcome::Accepted(Receipt::Done)
                        } else {
                            unknown(&format!("the order it cancels is {status}"))
                        }
                    }
                    Ok(None) => unknown("the venue has no record of the order it cancels"),
                    Err(error) => unknown(&format!(
                        "the order it cancels couldn't be looked up: {error:#}"
                    )),
                }
            }
            _ => unknown(if arrived {
                "the venue took it, and what it did can't be read back"
            } else {
                "what it did can't be read back"
            }),
        }
    }

    /// Decode and submit one payload a strategy sent; an accepted receipt
    /// comes back encoded as a payload.
    pub async fn submit_payload(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        match Self::decode(payload) {
            Ok(action) => match self.outcome(&action).await {
                Outcome::Accepted(receipt) => Outcome::Accepted(encode(&receipt)),
                Outcome::Refused(why) => Outcome::Refused(why),
                Outcome::Unknown(why) => Outcome::Unknown(why),
            },
            Err(why) => Outcome::Refused(why.to_string()),
        }
    }
}

/// How far before an action's nonce (its signing time) the venue's record of
/// it may be dated and still be taken for it: clocks a little apart.
const RECORD_SLACK_MS: u64 = 60_000;

/// Whether the venue's record of an order (`orderStatus`'s `order.order`) is
/// the one sent: its market, side, size and price, placed no earlier than a
/// little before it was signed.
fn same_order(record: &Value, sent: &OrderWire, market: &Market, nonce: u64) -> bool {
    let decimal = |field: &str| -> Option<Decimal> { record[field].as_str()?.parse().ok() };
    let wire = |text: &str| -> Option<Decimal> { text.parse().ok() };
    let side = if sent.b { "B" } else { "A" };
    record["coin"].as_str() == Some(market.coin.as_str())
        && record["side"].as_str() == Some(side)
        && decimal("origSz").is_some()
        && decimal("origSz") == wire(&sent.s)
        && decimal("limitPx").is_some()
        && decimal("limitPx") == wire(&sent.p)
        && record["timestamp"]
            .as_u64()
            .is_some_and(|at| at + RECORD_SLACK_MS >= nonce)
}

/// What filled of order `oid`: its size and average price, from the
/// account's fills since a little before it was signed; `None` if nothing.
async fn filled_of(
    api: &Api,
    user: Address,
    oid: u64,
    nonce: u64,
) -> Result<Option<(Decimal, Decimal)>> {
    let fills = api
        .fills_since(user, nonce.saturating_sub(RECORD_SLACK_MS), 10)
        .await?;
    let mut size = Decimal::ZERO;
    let mut notional = Decimal::ZERO;
    for fill in fills
        .iter()
        .filter(|fill| fill["oid"].as_u64() == Some(oid))
    {
        let number = |field: &str| -> Result<Decimal> {
            fill[field]
                .as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| anyhow!("a fill without a readable {field}: {fill}"))
        };
        let (sz, px) = (number("sz")?, number("px")?);
        size += sz;
        notional += sz * px;
    }
    if size <= Decimal::ZERO {
        return Ok(None);
    }
    Ok(Some((size, notional / size)))
}

/// What became of one submitted action.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T = Receipt> {
    Accepted(T),
    /// Refused here or by the venue: nothing happened.
    Refused(String),
    /// Sent, and no answer came back: it may still land.
    Unknown(String),
}

/// A payload, as canonical CBOR.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).expect("a payload encodes");
    bytes
}

/// What the venue's answer to an action it took says.
enum Read {
    Receipt(Receipt),
    /// It refused the order or cancel inside it: nothing happened.
    Refused(String),
    /// It can't be read: the action was taken, and what it did is unknown.
    Unreadable(String),
}

/// What the venue's answer to `wire` means.
fn receipt(wire: &Wire, response: &Value) -> Read {
    match wire {
        Wire::Order(_) | Wire::Modify(_) => {
            let status = &response["data"]["statuses"][0];
            if let Some(error) = status["error"].as_str() {
                return Read::Refused(format!("the venue refused the order: {error}"));
            }
            if let Some(oid) = status["resting"]["oid"].as_u64() {
                return Read::Receipt(Receipt::Resting { order_id: oid });
            }
            let filled = &status["filled"];
            if let Some(oid) = filled["oid"].as_u64() {
                let number = |field: &str| -> Option<Decimal> {
                    filled[field].as_str().and_then(|text| text.parse().ok())
                };
                return match (number("totalSz"), number("avgPx")) {
                    (Some(size), Some(average_price)) => Read::Receipt(Receipt::Filled {
                        order_id: oid,
                        size,
                        average_price,
                    }),
                    _ => Read::Unreadable(format!("the venue's fill can't be read: {status}")),
                };
            }
            if status == "success" || status.is_null() {
                return Read::Receipt(Receipt::Done);
            }
            Read::Unreadable(format!(
                "the venue's answer is not one this driver reads: {status}"
            ))
        }
        Wire::Cancel(_) | Wire::CancelByCloid(_) => {
            let status = &response["data"]["statuses"][0];
            if let Some(error) = status["error"].as_str() {
                return Read::Refused(format!("the venue refused the cancel: {error}"));
            }
            Read::Receipt(Receipt::Done)
        }
        _ => Read::Receipt(Receipt::Done),
    }
}

/// The JSON a wire action is sent as (the same fields it is hashed from).
pub fn to_json(wire: &Wire) -> Value {
    serde_json::to_value(wire).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markets::tests::sample;
    use crate::signing::{Network, l1_action_digest};

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    fn allowlist() -> Allowlist {
        Allowlist::from_driver(Address::repeat_byte(0x55), sample(), &[4, 2], &[1035]).unwrap()
    }

    fn order(market: &str) -> Order {
        Order {
            market: market.to_string(),
            side: Side::Sell,
            size: d("0.5"),
            limit_price: d("2700"),
            tif: Tif::Gtc,
            reduce_only: false,
            client_id: Some(7),
        }
    }

    #[test]
    fn every_action_becomes_one_of_the_signable_wire_types() {
        let list = allowlist();
        let actions = vec![
            Action::Place(order("ETH")),
            Action::Cancel {
                market: "ETH".into(),
                order: OrderRef::Venue(9),
            },
            Action::Cancel {
                market: "ETH".into(),
                order: OrderRef::Client(9),
            },
            Action::Modify {
                order_ref: OrderRef::Venue(9),
                order: order("ETH"),
            },
            Action::Leverage {
                market: "ETH".into(),
                leverage: 3,
            },
            Action::MoveDollars {
                from: Balance::Perp,
                amount: d("12.5"),
            },
            Action::CancelAllAt { at_ms: Some(1) },
        ];
        let mut kinds = BTreeSet::new();
        for action in &actions {
            let wire = list.check(action, 1).unwrap();
            let json = to_json(&wire);
            assert_eq!(json["type"], wire.kind());
            kinds.insert(wire.kind());
        }
        assert_eq!(kinds, SIGNABLE.into_iter().collect());
    }

    #[test]
    fn a_market_the_driver_does_not_list_is_refused() {
        let list = allowlist();
        let error = list.check(&Action::Place(order("SOL")), 1).unwrap_err();
        assert!(error.to_string().contains("not a market"));
    }

    #[test]
    fn leverage_is_always_cross_and_within_the_markets_maximum() {
        let list = allowlist();
        let wire = list
            .check(
                &Action::Leverage {
                    market: "ETH".into(),
                    leverage: 25,
                },
                1,
            )
            .unwrap();
        assert_eq!(to_json(&wire)["isCross"], true);
        assert!(
            list.check(
                &Action::Leverage {
                    market: "ETH".into(),
                    leverage: 26
                },
                1
            )
            .is_err()
        );
        assert!(
            list.check(
                &Action::Leverage {
                    market: "HYPE/USDC".into(),
                    leverage: 2
                },
                1
            )
            .is_err()
        );
    }

    #[test]
    fn moving_dollars_names_only_the_account_itself_and_the_dollar() {
        let list = allowlist();
        let wire = list
            .check(
                &Action::MoveDollars {
                    from: Balance::Spot,
                    amount: d("3"),
                },
                42,
            )
            .unwrap();
        let json = to_json(&wire);
        assert_eq!(
            json["destination"],
            format!("{:#x}", Address::repeat_byte(0x55))
        );
        assert_eq!(json["sourceDex"], "spot");
        assert_eq!(json["destinationDex"], "");
        assert_eq!(json["token"], "USDC:0xaa");
        assert_eq!(json["fromSubAccount"], "");
        assert_eq!(json["nonce"], 42);
    }

    #[test]
    fn a_payload_naming_a_vault_transfer_or_a_mode_change_does_not_decode() {
        #[derive(Serialize)]
        #[serde(rename_all = "kebab-case")]
        enum Hostile {
            VaultTransfer { vault: String, usd: u64 },
            SetAbstraction { mode: u8 },
        }
        for hostile in [
            Hostile::VaultTransfer {
                vault: "0xdf13098394e1832014b0df3f91285497".into(),
                usd: 5,
            },
            Hostile::SetAbstraction { mode: 2 },
        ] {
            assert!(Trader::decode(&encode(&hostile)).is_err());
        }
    }

    #[test]
    fn an_order_is_hashed_with_its_fields_in_the_venues_order() {
        let list = allowlist();
        let wire = list.check(&Action::Place(order("ETH")), 1).unwrap();
        let packed = rmp_serde::to_vec_named(&wire).unwrap();
        // {type, orders, grouping}, and an order's keys a b p s r t c.
        assert_eq!(packed[0], 0x83);
        let keys: Vec<usize> = b"abpsrtc"
            .iter()
            .map(|key| {
                packed
                    .windows(2)
                    .position(|pair| pair == [0xa1, *key])
                    .unwrap()
            })
            .collect();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(l1_action_digest(Network::Testnet, &wire, 1).is_ok());
        assert_eq!(to_json(&wire)["orders"][0]["c"], format!("0x{:032x}", 7));
    }

    #[test]
    fn an_agent_may_not_use_the_client_ids_the_executor_marks_its_own_upkeep_with() {
        let list = allowlist();
        let mut tagged = order("ETH");
        tagged.client_id = Some(upkeep_client_id(5));
        assert!(list.check(&Action::Place(tagged.clone()), 1).is_err());
        assert!(
            list.check(
                &Action::Modify {
                    order_ref: OrderRef::Venue(9),
                    order: tagged,
                },
                1
            )
            .is_err()
        );
        // The executor's own order carries the tag, with its nonce.
        let mut own = order("HYPE/USDC");
        own.limit_price = d("22.5");
        own.client_id = None;
        let wire = list.check_as(&Action::Place(own), 42, true).unwrap();
        let client = to_json(&wire)["orders"][0]["c"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(is_upkeep_cloid(&client));
        assert_eq!(client, cloid(upkeep_client_id(42)));
    }

    #[test]
    fn an_order_the_agent_gave_no_client_id_is_sent_with_one_never_the_upkeep_s() {
        let list = allowlist();
        let mut plain = order("ETH");
        plain.client_id = None;
        let mut seen = BTreeSet::new();
        for nonce in 0..2_000 {
            let wire = list.check(&Action::Place(plain.clone()), nonce).unwrap();
            let client = to_json(&wire)["orders"][0]["c"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(!is_upkeep_cloid(&client));
            assert!(seen.insert(client), "one client id per nonce");
        }
    }

    #[test]
    fn a_spot_order_is_never_reduce_only() {
        let list = allowlist();
        let mut spot = order("HYPE/USDC");
        spot.limit_price = d("22.5");
        spot.reduce_only = true;
        assert!(list.check(&Action::Place(spot.clone()), 1).is_err());
        spot.reduce_only = false;
        let wire = list.check(&Action::Place(spot), 1).unwrap();
        assert_eq!(to_json(&wire)["orders"][0]["a"], 11_035);
    }
}
