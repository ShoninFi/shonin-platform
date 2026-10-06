//! The vault's account as the venue reports it: its events as a stream, and
//! its state on request.
//!
//! Events come over the venue's WebSocket (`userEvents` for fills, funding
//! and liquidations; `orderUpdates` for every order's state), one connection
//! per account, reconnecting after a failure. A strategy reads them as
//! [`AccountEvent`] payloads.

use std::time::Duration;

use alloy::primitives::Address;
use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::api::Api;
use crate::markets::Markets;
use crate::payload::{AccountEvent, AccountState, Decimal, OpenOrder, Position, Side, SpotBalance};
use crate::signing::Network;

pub const TESTNET_SOCKET: &str = "wss://api.hyperliquid-testnet.xyz/ws";
pub const MAINNET_SOCKET: &str = "wss://api.hyperliquid.xyz/ws";

/// How many events wait for a slow reader before the newest are dropped.
const WAITING_MOST: usize = 1024;

fn decimal(value: &Value) -> Option<Decimal> {
    value.as_str().and_then(|text| text.parse().ok())
}

fn side(value: &Value) -> Option<Side> {
    match value.as_str()? {
        "B" => Some(Side::Buy),
        "A" => Some(Side::Sell),
        _ => None,
    }
}

/// The market an event names, by the agent's name where the venue's coin is
/// one it knows.
fn market(markets: &Markets, coin: &Value) -> String {
    let coin = coin.as_str().unwrap_or_default();
    markets
        .by_coin(coin)
        .map(|market| market.name.clone())
        .unwrap_or_else(|| coin.to_string())
}

/// The events one WebSocket message carries.
pub fn parse(markets: &Markets, message: &Value) -> Vec<AccountEvent> {
    let mut events = Vec::new();
    let data = &message["data"];
    match message["channel"].as_str() {
        Some("user") => {
            for fill in data["fills"].as_array().into_iter().flatten() {
                if let (Some(side), Some(size), Some(price)) = (
                    side(&fill["side"]),
                    decimal(&fill["sz"]),
                    decimal(&fill["px"]),
                ) {
                    events.push(AccountEvent::Filled {
                        market: market(markets, &fill["coin"]),
                        side,
                        size,
                        price,
                        fee: decimal(&fill["fee"]).unwrap_or(Decimal::ZERO),
                        order_id: fill["oid"].as_u64().unwrap_or(0),
                        time_ms: fill["time"].as_u64().unwrap_or(0),
                    });
                }
            }
            let funding = &data["funding"];
            if let Some(amount) = decimal(&funding["usdc"]) {
                events.push(AccountEvent::Funding {
                    market: market(markets, &funding["coin"]),
                    amount,
                    time_ms: funding["time"].as_u64().unwrap_or(0),
                });
            }
            if data.get("liquidation").is_some() {
                events.push(AccountEvent::Liquidated {
                    time_ms: data["liquidation"]["time"].as_u64().unwrap_or(0),
                });
            }
        }
        Some("orderUpdates") => {
            for update in data.as_array().into_iter().flatten() {
                let order = &update["order"];
                events.push(AccountEvent::Order {
                    market: market(markets, &order["coin"]),
                    order_id: order["oid"].as_u64().unwrap_or(0),
                    client_id: order["cloid"].as_str().and_then(|text| {
                        u128::from_str_radix(text.trim_start_matches("0x"), 16).ok()
                    }),
                    status: update["status"].as_str().unwrap_or("unknown").to_string(),
                    time_ms: update["statusTimestamp"].as_u64().unwrap_or(0),
                });
            }
        }
        _ => {}
    }
    events
}

/// The account's events, for as long as the receiver is held.
pub fn account_events(
    network: Network,
    account: Address,
    markets: Markets,
) -> mpsc::Receiver<AccountEvent> {
    let (sender, receiver) = mpsc::channel(WAITING_MOST);
    let url = match network {
        Network::Mainnet => MAINNET_SOCKET,
        Network::Testnet => TESTNET_SOCKET,
    };
    tokio::spawn(async move {
        let mut pause = Duration::from_secs(1);
        while !sender.is_closed() {
            if let Err(error) = follow(url, account, &markets, &sender).await {
                tracing::warn!(%error, "the venue's account stream dropped; reconnecting");
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(30));
        }
    });
    receiver
}

async fn follow(
    url: &str,
    account: Address,
    markets: &Markets,
    sender: &mpsc::Sender<AccountEvent>,
) -> Result<()> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    let user = format!("{account:#x}");
    for kind in ["userEvents", "orderUpdates"] {
        let subscribe =
            json!({"method": "subscribe", "subscription": {"type": kind, "user": user}});
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                subscribe.to_string().into(),
            ))
            .await?;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                let ping = json!({"method": "ping"}).to_string();
                socket.send(tokio_tungstenite::tungstenite::Message::Text(ping.into())).await?;
            }
            message = socket.next() => {
                let message = message.ok_or_else(|| anyhow!("the venue closed the stream"))??;
                let Ok(text) = message.to_text() else { continue };
                let Ok(value) = serde_json::from_str::<Value>(text) else { continue };
                for event in parse(markets, &value) {
                    // A full queue drops the event rather than stall the stream.
                    if sender.try_send(event).is_err() && sender.is_closed() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// The account now: value, positions, spot balances and open orders. The
/// vault's own figures are left at zero for the caller, which reads the
/// chain, to fill in.
pub async fn account_state(api: &Api, markets: &Markets, account: Address) -> Result<AccountState> {
    let perp = api
        .info(json!({"type": "clearinghouseState", "user": account}))
        .await?;
    let spot = api
        .info(json!({"type": "spotClearinghouseState", "user": account}))
        .await?;
    let orders = api
        .info(json!({"type": "openOrders", "user": account}))
        .await?;
    state_from(markets, &perp, &spot, &orders)
}

/// The same, from the venue's three answers. An answer that isn't the list
/// or the figure it should be, or an entry in it that can't be read, is an
/// error: never an account with less in it than the venue holds.
pub fn state_from(
    markets: &Markets,
    perp: &Value,
    spot: &Value,
    orders: &Value,
) -> Result<AccountState> {
    let list = |answer: &Value, what: &str| -> Result<Vec<Value>> {
        answer.as_array().cloned().ok_or_else(|| {
            anyhow!("the venue's answer for the account's {what} can't be read: {answer}")
        })
    };
    let unreadable =
        |what: &str, entry: &Value| anyhow!("one of the account's {what} can't be read: {entry}");
    let mut positions = Vec::new();
    for entry in list(&perp["assetPositions"], "positions")? {
        let position = &entry["position"];
        positions.push(Position {
            market: market(markets, &position["coin"]),
            size: decimal(&position["szi"]).ok_or_else(|| unreadable("positions", &entry))?,
            entry_price: decimal(&position["entryPx"]),
            leverage: position["leverage"]["value"].as_u64().unwrap_or(0) as u32,
        });
    }
    let mut balances = Vec::new();
    for balance in list(&spot["balances"], "spot balances")? {
        balances.push(SpotBalance {
            token: balance["coin"]
                .as_str()
                .ok_or_else(|| unreadable("spot balances", &balance))?
                .to_string(),
            total: decimal(&balance["total"])
                .ok_or_else(|| unreadable("spot balances", &balance))?,
            hold: decimal(&balance["hold"]).unwrap_or(Decimal::ZERO),
        });
    }
    let mut open_orders = Vec::new();
    for order in list(orders, "open orders")? {
        let read = || {
            Some(OpenOrder {
                market: market(markets, &order["coin"]),
                order_id: order["oid"].as_u64()?,
                side: side(&order["side"])?,
                size: decimal(&order["sz"])?,
                limit_price: decimal(&order["limitPx"])?,
            })
        };
        open_orders.push(read().ok_or_else(|| unreadable("open orders", &order))?);
    }
    let figure = |value: &Value, what: &str| {
        decimal(value).ok_or_else(|| anyhow!("the account's {what} can't be read: {value}"))
    };
    Ok(AccountState {
        vault_value: Decimal::ZERO,
        idle: Decimal::ZERO,
        account_value: figure(&perp["marginSummary"]["accountValue"], "value")?,
        withdrawable: figure(&perp["withdrawable"], "withdrawable dollars")?,
        positions,
        spot: balances,
        open_orders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markets::tests::sample;

    #[test]
    fn fills_funding_and_liquidations_are_read_from_user_events() {
        let markets = sample();
        let message = json!({"channel": "user", "data": {"fills": [
            {"coin": "@1035", "px": "22.5", "sz": "1.5", "side": "B", "time": 5, "fee": "0.01", "oid": 9}
        ]}});
        let events = parse(&markets, &message);
        assert_eq!(
            events,
            vec![AccountEvent::Filled {
                market: "HYPE/USDC".into(),
                side: Side::Buy,
                size: "1.5".parse().unwrap(),
                price: "22.5".parse().unwrap(),
                fee: "0.01".parse().unwrap(),
                order_id: 9,
                time_ms: 5,
            }]
        );
        let funding = json!({"channel": "user", "data": {"funding": {"coin": "ETH", "usdc": "-0.25", "time": 7}}});
        assert!(matches!(
            parse(&markets, &funding)[0],
            AccountEvent::Funding { ref market, .. } if market == "ETH"
        ));
        let liquidation = json!({"channel": "user", "data": {"liquidation": {"time": 8}}});
        assert_eq!(
            parse(&markets, &liquidation),
            vec![AccountEvent::Liquidated { time_ms: 8 }]
        );
    }

    #[test]
    fn order_updates_carry_the_agents_own_id_back() {
        let markets = sample();
        let message = json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "ETH", "oid": 3, "cloid": "0x0000000000000000000000000000000a"},
             "status": "canceled", "statusTimestamp": 11}
        ]});
        assert_eq!(
            parse(&markets, &message),
            vec![AccountEvent::Order {
                market: "ETH".into(),
                order_id: 3,
                client_id: Some(10),
                status: "canceled".into(),
                time_ms: 11,
            }]
        );
    }

    #[test]
    fn the_account_state_is_read_from_the_venues_answers() {
        let markets = sample();
        let perp = json!({
            "marginSummary": {"accountValue": "100.5"},
            "withdrawable": "80",
            "assetPositions": [{"position": {"coin": "ETH", "szi": "-0.1", "entryPx": "2700", "leverage": {"type": "cross", "value": 3}}}]
        });
        let spot = json!({"balances": [{"coin": "USDC", "total": "5", "hold": "1"}]});
        let orders =
            json!([{"coin": "ETH", "oid": 1, "side": "A", "sz": "0.1", "limitPx": "2800"}]);
        let state = state_from(&markets, &perp, &spot, &orders).unwrap();
        assert_eq!(state.positions[0].size, "-0.1".parse().unwrap());
        assert_eq!(state.positions[0].leverage, 3);
        assert_eq!(state.spot[0].hold, "1".parse().unwrap());
        assert_eq!(state.open_orders[0].side, Side::Sell);
        assert_eq!(state.withdrawable, "80".parse().unwrap());
    }
}
