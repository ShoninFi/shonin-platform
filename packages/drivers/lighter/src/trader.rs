//! The driver's executor half: what the executor signs with the station's
//! Lighter key.
//!
//! The key is made inside the executor, and its public half is named on the
//! board by the executor's own blessed key; the station registers it at the
//! exchange. Lighter has one kind of key, and it can do more than trade: make
//! sub-accounts, move collateral among the owner's accounts, ask for
//! withdrawals and put collateral into public pools. So everything it may
//! sign is listed here ([`SIGNABLE`]: create, cancel and modify orders,
//! cancel all), each built only from a decoded [`Action`] for the station's
//! own master account, and a transaction of any other type is refused even
//! if something tried to build one.

use anyhow::{Result, anyhow, bail};
use lighters::constants::{
    GOOD_TILL_TIME, IMMEDIATE_OR_CANCEL, LIMIT_ORDER, NIL_ORDER_EXPIRY, POST_ONLY,
    TX_TYPE_L2_CANCEL_ALL_ORDERS, TX_TYPE_L2_CANCEL_ORDER, TX_TYPE_L2_CREATE_ORDER,
    TX_TYPE_L2_MODIFY_ORDER,
};
use lighters::transactions::to_signed;
use lighters::{KeyManager, OrderReq, SignedTransaction, TransactOpts, TxClient};

use crate::markets::{Markets, Round};
use crate::payload::{Action, OrderRef, Side, Tif};

/// Every transaction type the executor signs with a station's Lighter key.
pub const SIGNABLE: [u8; 4] = [
    TX_TYPE_L2_CREATE_ORDER,
    TX_TYPE_L2_CANCEL_ORDER,
    TX_TYPE_L2_CANCEL_ALL_ORDERS,
    TX_TYPE_L2_MODIFY_ORDER,
];

/// The key slot the station registers the executor's key in (as the driver
/// contract's `KEY_SLOT`).
pub const KEY_SLOT: u8 = 2;

/// How long a signed transaction stays valid, in milliseconds.
const VALID_MS: i64 = 10 * 60 * 1000;
/// Lighter's ceiling on a client order id.
const MAX_CLIENT_ID: u64 = (1 << 48) - 1;

/// A station's Lighter key, made in the executor.
pub struct Key {
    manager: KeyManager,
}

impl Key {
    pub fn generate() -> Self {
        Self {
            manager: KeyManager::generate(),
        }
    }

    /// The 40 bytes the executor's key names on the board.
    pub fn public(&self) -> [u8; 40] {
        self.manager.public_key_bytes()
    }

    fn secret_hex(&self) -> String {
        self.manager.private_key_hex()
    }
}

/// Signs a station's actions.
pub struct Trader {
    client: TxClient,
    markets: Markets,
}

impl Trader {
    /// For the station's master account `account`, on the Lighter instance
    /// whose signing chain id is `chain_id`.
    pub fn new(key: &Key, account: u64, chain_id: u32, markets: Markets) -> Result<Self> {
        let account = i64::try_from(account).map_err(|_| anyhow!("account index out of range"))?;
        let client = TxClient::new(&key.secret_hex(), account, KEY_SLOT, chain_id)
            .map_err(|error| anyhow!("{error}"))?;
        Ok(Self { client, markets })
    }

    /// Sign `action` with `nonce`, valid until `now_ms` plus ten minutes.
    pub fn sign(&self, action: &Action, nonce: i64, now_ms: i64) -> Result<SignedTransaction> {
        let opts = TransactOpts {
            nonce: Some(nonce),
            expired_at: Some(now_ms + VALID_MS),
            ..Default::default()
        };
        let signed = match action {
            Action::Place(order) => {
                let market = self.markets.get(&order.market)?;
                let round = match order.side {
                    Side::Buy => Round::Down,
                    Side::Sell => Round::Up,
                };
                let (time_in_force, order_expiry) = match order.tif {
                    Tif::PostOnly => (POST_ONLY, now_ms + 28 * 86_400_000),
                    Tif::GoodTill { expiry_ms } => (
                        GOOD_TILL_TIME,
                        i64::try_from(expiry_ms).map_err(|_| anyhow!("expiry out of range"))?,
                    ),
                    Tif::Ioc => (IMMEDIATE_OR_CANCEL, NIL_ORDER_EXPIRY),
                };
                let tx = self
                    .client
                    .create_order(
                        OrderReq {
                            market_index: market.index as i16,
                            client_order_index: client_index(order.client_id)?,
                            base_amount: market.base_units(order.size)? as i64,
                            price: market.price_ticks(order.limit_price, round)?,
                            is_ask: u8::from(order.side == Side::Sell),
                            order_type: LIMIT_ORDER,
                            time_in_force,
                            reduce_only: u8::from(order.reduce_only),
                            trigger_price: 0,
                            order_expiry,
                        },
                        &opts,
                    )
                    .map_err(|error| anyhow!("{error}"))?;
                to_signed(&tx, self.client.chain_id())
            }
            Action::Cancel { market, order } => {
                let market = self.markets.get(market)?;
                let tx = self
                    .client
                    .cancel_order(market.index as i16, order_index(order)?, &opts)
                    .map_err(|error| anyhow!("{error}"))?;
                to_signed(&tx, self.client.chain_id())
            }
            Action::Modify {
                market,
                order,
                size,
                limit_price,
            } => {
                let market = self.markets.get(market)?;
                // Rounded down: a modified order never asks more than said.
                let tx = self
                    .client
                    .modify_order(
                        market.index as i16,
                        order_index(order)?,
                        market.base_units(*size)? as i64,
                        market.price_ticks(*limit_price, Round::Down)?,
                        0,
                        &opts,
                    )
                    .map_err(|error| anyhow!("{error}"))?;
                to_signed(&tx, self.client.chain_id())
            }
            Action::CancelAll => {
                // Immediately, not scheduled.
                let tx = self
                    .client
                    .cancel_all_orders(0, 0, &opts)
                    .map_err(|error| anyhow!("{error}"))?;
                to_signed(&tx, self.client.chain_id())
            }
        }
        .map_err(|error| anyhow!("{error}"))?;
        if !SIGNABLE.contains(&signed.tx_type) {
            bail!(
                "transaction type {} is not one the executor signs",
                signed.tx_type
            );
        }
        Ok(signed)
    }
}

fn client_index(client_id: Option<u64>) -> Result<i64> {
    match client_id {
        None => Ok(0),
        Some(id) if (1..=MAX_CLIENT_ID).contains(&id) => Ok(id as i64),
        Some(id) => bail!("client id {id} is outside 1 to 2^48 - 1"),
    }
}

fn order_index(order: &OrderRef) -> Result<i64> {
    match *order {
        OrderRef::Venue(index) => {
            i64::try_from(index).map_err(|_| anyhow!("order index out of range"))
        }
        OrderRef::Client(id) => client_index(Some(id)),
    }
}

/// The transaction types a Lighter key can sign that the executor never
/// does, for tests and for reading a refusal.
pub fn never_signed() -> Vec<u8> {
    use lighters::constants::*;
    vec![
        TX_TYPE_L2_CHANGE_PUB_KEY,
        TX_TYPE_L2_CREATE_SUB_ACCOUNT,
        TX_TYPE_L2_CREATE_PUBLIC_POOL,
        TX_TYPE_L2_UPDATE_PUBLIC_POOL,
        TX_TYPE_L2_TRANSFER,
        TX_TYPE_L2_WITHDRAW,
        TX_TYPE_L2_MINT_SHARES,
        TX_TYPE_L2_BURN_SHARES,
        TX_TYPE_L2_APPROVE_INTEGRATOR,
        TX_TYPE_L2_UPDATE_MARGIN,
        TX_TYPE_L2_STAKE_ASSETS,
        TX_TYPE_L2_UNSTAKE_ASSETS,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::Order;

    fn markets() -> Markets {
        let answer = serde_json::json!({"order_books": [
            {"symbol": "SPY", "market_id": 26, "market_type": "perp", "status": "active",
             "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "0.0100"}
        ]});
        Markets::from_order_books(&answer).unwrap()
    }

    fn d(text: &str) -> crate::payload::Decimal {
        text.parse().unwrap()
    }

    fn order(side: Side, tif: Tif) -> Action {
        Action::Place(Order {
            market: "SPY".into(),
            side,
            size: d("0.5"),
            limit_price: d("775.555"),
            tif,
            reduce_only: false,
            client_id: Some(42),
        })
    }

    #[test]
    fn every_action_signs_to_one_of_the_listed_types_and_nothing_else_is_listed() {
        let trader = Trader::new(&Key::generate(), 40_518, 300, markets()).unwrap();
        let actions = [
            order(Side::Buy, Tif::Ioc),
            order(Side::Sell, Tif::PostOnly),
            Action::Cancel {
                market: "SPY".into(),
                order: OrderRef::Client(42),
            },
            Action::Modify {
                market: "SPY".into(),
                order: OrderRef::Venue(9),
                size: d("0.2"),
                limit_price: d("770"),
            },
            Action::CancelAll,
        ];
        for action in &actions {
            let signed = trader.sign(action, 1, 1_790_000_000_000).unwrap();
            assert!(SIGNABLE.contains(&signed.tx_type));
        }
        for refused in never_signed() {
            assert!(!SIGNABLE.contains(&refused));
        }
    }

    #[test]
    fn an_order_is_signed_for_the_stations_master_account_in_the_executors_slot() {
        let trader = Trader::new(&Key::generate(), 40_518, 300, markets()).unwrap();
        let signed = trader
            .sign(&order(Side::Sell, Tif::Ioc), 7, 1_790_000_000_000)
            .unwrap();
        let info: serde_json::Value = serde_json::from_str(&signed.tx_info).unwrap();
        assert_eq!(info["AccountIndex"], 40_518);
        assert_eq!(info["ApiKeyIndex"], KEY_SLOT);
        assert_eq!(info["MarketIndex"], 26);
        assert_eq!(info["BaseAmount"], 5_000);
        assert_eq!(info["Price"], 77_556, "a sell's limit rounds up");
        assert_eq!(info["IsAsk"], 1);
        assert_eq!(info["Nonce"], 7);
    }

    #[test]
    fn an_unknown_market_or_an_order_below_the_smallest_is_refused() {
        let trader = Trader::new(&Key::generate(), 1, 300, markets()).unwrap();
        let mut action = order(Side::Buy, Tif::Ioc);
        if let Action::Place(order) = &mut action {
            order.market = "TSLA".into();
        }
        assert!(trader.sign(&action, 1, 0).is_err());
        let mut tiny = order(Side::Buy, Tif::Ioc);
        if let Action::Place(order) = &mut tiny {
            order.size = d("0.001");
        }
        assert!(trader.sign(&tiny, 1, 0).is_err());
    }
}
