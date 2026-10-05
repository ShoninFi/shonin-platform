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

use crate::api::{Api, HyperCore};
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

    fn order_wire(&self, order: &Order) -> Result<OrderWire> {
        let market = self.market(&order.market)?;
        if order.reduce_only && market.is_spot() {
            bail!("a spot order can't be reduce-only");
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
            c: order.client_id.map(cloid),
        })
    }

    /// Check `action` and turn it into the one wire action it allows.
    /// `nonce` is the nonce it will be signed with, which a move names.
    pub fn check(&self, action: &Action, nonce: u64) -> Result<Wire> {
        Ok(match action {
            Action::Place(order) => Wire::Order(OrderAction {
                kind: "order",
                orders: vec![self.order_wire(order)?],
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
                order: self.order_wire(order)?,
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
        let nonce = self.core.next_nonce();
        let wire = match self.allowlist.check(action, nonce) {
            Ok(wire) => wire,
            Err(why) => return Outcome::Refused(why.to_string()),
        };
        debug_assert!(SIGNABLE.contains(&wire.kind()));
        match self.core.l1_at(&wire, nonce).await {
            Ok(response) => match receipt(&wire, &response) {
                Ok(receipt) => Outcome::Accepted(receipt),
                Err(why) => Outcome::Refused(why.to_string()),
            },
            Err(why) => {
                let why = format!("{why:#}");
                if why.starts_with(crate::api::REFUSED) {
                    Outcome::Refused(why)
                } else {
                    Outcome::Unknown(why)
                }
            }
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

/// What the venue's answer to `wire` means.
fn receipt(wire: &Wire, response: &Value) -> Result<Receipt> {
    match wire {
        Wire::Order(_) | Wire::Modify(_) => {
            let status = &response["data"]["statuses"][0];
            if let Some(error) = status["error"].as_str() {
                bail!("the venue refused the order: {error}");
            }
            if let Some(oid) = status["resting"]["oid"].as_u64() {
                return Ok(Receipt::Resting { order_id: oid });
            }
            let filled = &status["filled"];
            if let Some(oid) = filled["oid"].as_u64() {
                let number = |field: &str| -> Result<Decimal> {
                    filled[field]
                        .as_str()
                        .and_then(|text| text.parse().ok())
                        .ok_or_else(|| anyhow!("the venue's fill has no {field}"))
                };
                return Ok(Receipt::Filled {
                    order_id: oid,
                    size: number("totalSz")?,
                    average_price: number("avgPx")?,
                });
            }
            if status == "success" || status.is_null() {
                return Ok(Receipt::Done);
            }
            bail!("the venue's answer is not one this driver reads: {status}")
        }
        Wire::Cancel(_) | Wire::CancelByCloid(_) => {
            let status = &response["data"]["statuses"][0];
            if let Some(error) = status["error"].as_str() {
                bail!("the venue refused the cancel: {error}");
            }
            Ok(Receipt::Done)
        }
        _ => Ok(Receipt::Done),
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
