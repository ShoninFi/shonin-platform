//! Robinhood Chain's two drivers' executor halves, as this executor runs
//! them.
//!
//! - **`robinhood`**: stock tokens in the station's Safe, traded on Uniswap
//!   by this vault's key as a member of the Safe's Roles v2 role. A swap is
//!   checked against the token's Chainlink price and the station's band here
//!   (`robinhood::trader`) before the key signs, and again by the role
//!   on-chain.
//! - **`lighter`**: the station's Lighter account, traded with a Lighter key
//!   made here at every start. The vault's key names the new key's public
//!   half on the board and has the station register it, so a restarted (or
//!   replaced) executor's old key stops working. Only orders, cancels and
//!   modifications are signed (`lighter::trader`). The same half posts the
//!   attested read of the station's accounts on the board, which values the
//!   station and proves its staged closes.

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, anyhow, bail};
use chain::{Client, KeyClient};
use hyperliquid::agent::Outcome;
use tokio::sync::Mutex;

/// The names `shonin.toml` writes for them.
pub const ROBINHOOD: &str = "robinhood";
pub const LIGHTER: &str = "lighter";

/// How often the attested read of a station's Lighter accounts is posted.
const REPORT_EVERY: Duration = Duration::from_secs(120);

alloy::sol! {
    interface IStationRh {
        function pinnedDriver(bytes32 driver) external view returns (address);
        function approveKey(bytes32 driver, address key) external;
        function approvedKey(bytes32 driver) external view returns (address);
        function arrivals() external view returns (uint256);
        function venueFlows(bytes32 driver) external view returns (uint256 sent, uint64 depositedAt);
    }
    interface ILighterDriver {
        function exchange() external view returns (address);
        function board() external view returns (address);
    }
    interface IExchange {
        function addressToAccountIndex(address owner) external view returns (uint48);
        function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
    }
}

/// A driver's name as a station keys it: the name, zero-padded.
fn name(driver: &str) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[..driver.len()].copy_from_slice(driver.as_bytes());
    B256::from(bytes)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether a send's failure may still land, as the agent is told.
fn outcome_of(error: anyhow::Error) -> Outcome<Vec<u8>> {
    if chain::may_still_land(&error) {
        Outcome::Unknown(format!("{error:#}"))
    } else {
        Outcome::Refused(format!("{error:#}"))
    }
}

// Stock tokens ------------------------------------------------------------------

/// One vault's Safe and the role its key trades through.
pub struct StockHalf {
    trader: robinhood::Trader,
    chain: Client,
    key: KeyClient,
}

impl StockHalf {
    pub async fn connect(chain: &Client, key: KeyClient, station: Address) -> Result<Arc<Self>> {
        let driver = chain
            .read(
                station,
                IStationRh::pinnedDriverCall {
                    driver: name(ROBINHOOD),
                },
            )
            .await
            .context("reading the station's pinned Robinhood driver")?;
        let trader = robinhood::Trader::load(chain.provider(), driver, station).await?;
        Ok(Arc::new(Self {
            trader,
            chain: chain.clone(),
            key,
        }))
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        let action: robinhood::payload::Action = match strategy_sdk::from_payload(payload) {
            Ok(action) => action,
            Err(error) => {
                return Outcome::Refused(format!("that is not a Robinhood action: {error}"));
            }
        };
        let token = match &action {
            robinhood::payload::Action::Buy { token, .. }
            | robinhood::payload::Action::Sell { token, .. } => token.clone(),
        };
        let bounds = match self.trader.bounds(self.chain.provider(), &token).await {
            Ok(bounds) => bounds,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let calls = match self.trader.prepare(&action, &bounds) {
            Ok(calls) => calls,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let mut last = None;
        for call in calls {
            match self.key.send(call.to, call.call).await {
                Ok(receipt) => last = Some(receipt.transaction_hash),
                Err(error) => return outcome_of(error),
            }
        }
        let (spent, received) = match &action {
            robinhood::payload::Action::Buy {
                amount, min_tokens, ..
            } => (*amount, *min_tokens),
            robinhood::payload::Action::Sell {
                tokens, min_amount, ..
            } => (*tokens, *min_amount),
        };
        Outcome::Accepted(strategy_sdk::to_payload(
            &robinhood::payload::Receipt::Swapped {
                token,
                spent,
                // At least this much: the swap reverts below it.
                received,
                transaction: last.map(|hash| hash.to_string()).unwrap_or_default(),
            },
        ))
    }

    /// What the agent asked: the Safe's holdings priced at their feeds, or
    /// every listed token's feed and pool prices.
    pub async fn state(&self, query: &[u8]) -> Result<Vec<u8>> {
        use robinhood::contracts::{IAggregatorV3, IRobinhoodDriver, IStockToken};
        use robinhood::payload::{AccountState, Decimal, Holding, Query, TokenPrice};
        let query = if query.is_empty() {
            Query::Holdings
        } else {
            strategy_sdk::from_payload(query)
                .map_err(|error| anyhow!("not a Robinhood query: {error}"))?
        };
        let provider = self.chain.provider();
        let units = |value: U256, decimals: u8| -> Decimal {
            let raw: Decimal = value.to_string().parse().unwrap_or_default();
            raw / Decimal::from(10u64).powi(i32::from(decimals))
        };
        let whole = U256::from(10u64).pow(U256::from(18u8));
        match query {
            Query::Holdings => {
                let dollars = IStockToken::new(self.trader.dollar, provider)
                    .balanceOf(self.trader.safe)
                    .call()
                    .await?;
                let mut holdings = Vec::new();
                for listed in self.trader.listed.values() {
                    let token = IStockToken::new(listed.token, provider);
                    let balance = token.balanceOf(self.trader.safe).call().await?;
                    if balance.is_zero() {
                        continue;
                    }
                    let round = IAggregatorV3::new(listed.feed, provider)
                        .latestRoundData()
                        .call()
                        .await?;
                    let multiplier = token.uiMultiplier().call().await.unwrap_or(whole);
                    holdings.push(Holding {
                        token: listed.symbol.clone(),
                        tokens: units(balance, listed.token_decimals),
                        feed_price: units(round.answer.into_raw(), listed.feed_decimals),
                        feed_updated_at: u64::try_from(round.updatedAt).unwrap_or(0),
                        multiplier: units(multiplier, 18),
                    });
                }
                Ok(strategy_sdk::to_payload(&AccountState {
                    safe: self.trader.safe.to_string(),
                    dollars: units(dollars, self.trader.dollar_decimals),
                    holdings,
                }))
            }
            Query::Prices => {
                let driver = IRobinhoodDriver::new(self.trader.driver, provider);
                let mut prices = Vec::new();
                for listed in self.trader.listed.values() {
                    let token = IStockToken::new(listed.token, provider);
                    let round = IAggregatorV3::new(listed.feed, provider)
                        .latestRoundData()
                        .call()
                        .await?;
                    let one = U256::from(10u64).pow(U256::from(listed.token_decimals));
                    let market = driver
                        .marketQuote(listed.pool, listed.token, one)
                        .call()
                        .await
                        .unwrap_or_default();
                    prices.push(TokenPrice {
                        token: listed.symbol.clone(),
                        feed_price: units(round.answer.into_raw(), listed.feed_decimals),
                        feed_updated_at: u64::try_from(round.updatedAt).unwrap_or(0),
                        market_price: units(market, self.trader.dollar_decimals),
                        multiplier: units(token.uiMultiplier().call().await.unwrap_or(whole), 18),
                        paused: token.oraclePaused().call().await.unwrap_or(false),
                    });
                }
                Ok(strategy_sdk::to_payload(&prices))
            }
        }
    }
}

// Lighter -------------------------------------------------------------------------

/// One vault's Lighter account, and the Lighter key made for it at this start.
pub struct LighterHalf {
    chain: Client,
    key: KeyClient,
    station: Address,
    exchange: Address,
    board: Address,
    api: lighter::Api,
    chain_id: u32,
    lighter_key: lighter::Key,
    /// Built once the station has an account and has registered this key.
    trader: Mutex<Option<(u64, lighter::Trader)>>,
}

impl LighterHalf {
    /// Make a Lighter key, name it on the board as this vault's key, and
    /// have the station register it if the account already exists.
    pub async fn connect(chain: &Client, key: KeyClient, station: Address) -> Result<Arc<Self>> {
        let (api, chain_id) = match chain.chain_id {
            robinhood::listings::MAINNET => (
                lighter::Api::new(lighter::api::MAINNET),
                lighter::api::MAINNET_CHAIN_ID,
            ),
            other => bail!(
                "Lighter has no exchange on chain {other} (its testnet settles on Ethereum Sepolia)"
            ),
        };
        let driver = chain
            .read(
                station,
                IStationRh::pinnedDriverCall {
                    driver: name(LIGHTER),
                },
            )
            .await
            .context("reading the station's pinned Lighter driver")?;
        let exchange = chain.read(driver, ILighterDriver::exchangeCall {}).await?;
        let board = chain.read(driver, ILighterDriver::boardCall {}).await?;
        let lighter_key = lighter::Key::generate();
        key.send(
            board,
            lighter::report::ILighterBoard::setPubKeyCall {
                pubKey: lighter_key.public().to_vec().into(),
            },
        )
        .await
        .context("naming this start's Lighter key on the board")?;
        let half = Arc::new(Self {
            chain: chain.clone(),
            key,
            station,
            exchange,
            board,
            api,
            chain_id,
            lighter_key,
            trader: Mutex::new(None),
        });
        // Best effort now; tried again before the first order.
        if let Err(error) = half.trader().await {
            tracing::info!(%station, error = %format!("{error:#}"), "no Lighter account to trade yet");
        }
        Ok(half)
    }

    /// The trader, once the station has an account: the first time, the
    /// station registers this start's key in it.
    async fn trader(&self) -> Result<tokio::sync::MappedMutexGuard<'_, (u64, lighter::Trader)>> {
        let mut held = self.trader.lock().await;
        if held.is_none() {
            let account = self
                .chain
                .read(
                    self.exchange,
                    IExchange::addressToAccountIndexCall {
                        owner: self.station,
                    },
                )
                .await?
                .to::<u64>();
            if account == 0 {
                bail!("the station has no Lighter account yet: move dollars to it first");
            }
            self.key
                .send(
                    self.station,
                    IStationRh::approveKeyCall {
                        driver: name(LIGHTER),
                        key: self.key.address,
                    },
                )
                .await
                .context("having the station register this start's Lighter key")?;
            let markets = self.api.markets().await?;
            let trader = lighter::Trader::new(&self.lighter_key, account, self.chain_id, markets)?;
            *held = Some((account, trader));
        }
        Ok(tokio::sync::MutexGuard::map(held, |held| {
            held.as_mut().expect("just made")
        }))
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        let action: lighter::payload::Action = match strategy_sdk::from_payload(payload) {
            Ok(action) => action,
            Err(error) => {
                return Outcome::Refused(format!("that is not a Lighter action: {error}"));
            }
        };
        let held = match self.trader().await {
            Ok(held) => held,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let (account, trader) = (&held.0, &held.1);
        let nonce = match self
            .api
            .next_nonce(*account, lighter::trader::KEY_SLOT)
            .await
        {
            Ok(nonce) => nonce,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let signed = match trader.sign(&action, nonce, now_ms()) {
            Ok(signed) => signed,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        match self.api.send_tx(signed.tx_type, &signed.tx_info).await {
            Ok(tx_hash) => {
                Outcome::Accepted(strategy_sdk::to_payload(&lighter::payload::Receipt::Sent {
                    tx_hash,
                }))
            }
            // The exchange may have taken it before the answer was lost.
            Err(error) if error.to_string().contains("refused") => {
                Outcome::Refused(format!("{error:#}"))
            }
            Err(error) => Outcome::Unknown(format!("{error:#}")),
        }
    }

    pub async fn state(&self) -> Result<Vec<u8>> {
        let account = {
            let held = self.trader().await?;
            held.0
        };
        let raw = self.api.account(account).await?;
        let decimal = |value: &serde_json::Value| -> lighter::payload::Decimal {
            value.as_str().unwrap_or("0").parse().unwrap_or_default()
        };
        let positions = raw["positions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|position| {
                let size = decimal(&position["position"]);
                if size.is_zero() {
                    return None;
                }
                let signed = if position["sign"].as_i64() == Some(-1) {
                    -size
                } else {
                    size
                };
                Some(lighter::payload::Position {
                    market: position["symbol"].as_str().unwrap_or_default().to_string(),
                    size: signed,
                    entry_price: decimal(&position["avg_entry_price"]),
                    unrealized_pnl: decimal(&position["unrealized_pnl"]),
                })
            })
            .collect();
        Ok(strategy_sdk::to_payload(&lighter::payload::AccountState {
            account_index: account,
            equity: decimal(&raw["total_asset_value"]),
            available: decimal(&raw["available_balance"]),
            positions,
        }))
    }

    /// What the post reads on-chain, all at the chain's newest block: that
    /// block's time, which stamps the post, the station's pending balance at
    /// the exchange, and its own counters, so the driver can tell what moved
    /// after the reading.
    async fn on_chain(&self) -> Result<lighter::report::OnChain> {
        let (head, as_of) = self.chain.head_and_time().await?;
        let at = alloy::eips::BlockId::number(head);
        let pending = self
            .chain
            .read_at(
                self.exchange,
                IExchange::getPendingBalanceCall {
                    owner: self.station,
                    assetIndex: 3,
                },
                at,
            )
            .await?;
        let arrived = self
            .chain
            .read_at(self.station, IStationRh::arrivalsCall {}, at)
            .await?;
        let flows = self
            .chain
            .read_at(
                self.station,
                IStationRh::venueFlowsCall {
                    driver: name(LIGHTER),
                },
                at,
            )
            .await?;
        Ok(lighter::report::OnChain {
            as_of,
            pending: U256::from(pending),
            arrived,
            sent: flows.sent,
        })
    }

    /// Read the station's accounts and post them on the board, every
    /// `REPORT_EVERY`, until the task is dropped.
    pub fn report_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let half = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(REPORT_EVERY);
            loop {
                tick.tick().await;
                if let Err(error) = half.post().await {
                    tracing::warn!(station = %half.station, error = %format!("{error:#}"), "the Lighter read wasn't posted");
                }
            }
        })
    }

    /// One attested read of every account the station owns, posted.
    pub async fn post(&self) -> Result<()> {
        let account = self
            .chain
            .read(
                self.exchange,
                IExchange::addressToAccountIndexCall {
                    owner: self.station,
                },
            )
            .await?
            .to::<u64>();
        if account == 0 {
            return Ok(());
        }
        let markets = self.api.markets().await?;
        let owned = self.api.accounts_of(&self.station.to_string()).await?;
        let mut accounts: Vec<serde_json::Value> = Vec::new();
        for summary in owned["sub_accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(std::iter::once(&serde_json::json!({ "index": account })))
        {
            let Some(index) = summary["index"].as_u64() else {
                continue;
            };
            if accounts
                .iter()
                .any(|known| known["index"].as_u64() == Some(index))
            {
                continue;
            }
            accounts.push(self.api.account(index).await?);
        }
        let mut held = std::collections::BTreeSet::new();
        for account in &accounts {
            for position in account["positions"].as_array().into_iter().flatten() {
                let open = position["position"]
                    .as_str()
                    .is_some_and(|size| size.parse::<f64>().unwrap_or(0.0) != 0.0);
                if let (true, Some(market)) = (open, position["market_id"].as_u64()) {
                    held.insert(market as u16);
                }
            }
        }
        let mut prices = Vec::new();
        for market in held {
            let details = self.api.book_details(market).await?;
            let book = self.api.book_orders(market, 1).await?;
            prices.push(lighter::report::prices(market, &details, &book, &markets)?);
        }
        let on_chain = self.on_chain().await?;
        let report = lighter::report::build(on_chain, &accounts, &prices, &markets)?;
        tracing::info!(report = %lighter::report::describe(self.station, &report), "posting the Lighter read");
        self.key
            .send(
                self.board,
                lighter::report::ILighterBoard::postCall {
                    station: self.station,
                    report,
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| anyhow!("{error:#}"))
    }
}
