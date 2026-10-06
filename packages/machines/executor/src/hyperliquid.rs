//! The Hyperliquid driver's executor half, as this executor runs it.
//!
//! The vault's station on HyperEVM is the master of its HyperCore account
//! and has approved this vault's own key as its named agent (the key is made
//! per vault, and the venue lets one agent address serve one account only).
//! Everything a strategy submits through the `hyperliquid` driver is decoded
//! and checked against the allowlist in `hyperliquid::agent`, for the markets
//! the station's pinned driver lists, before that key signs it.

use std::sync::Arc;

use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow};
use chain::Client;
use hyperliquid::agent::{Allowlist, Outcome, Trader};
use hyperliquid::{Api, Markets};
use tokio::sync::mpsc::UnboundedSender;

/// The name `shonin.toml` writes for it.
pub const DRIVER: &str = "hyperliquid";

alloy::sol! {
    interface IPinnedStation {
        function pinnedDriver(bytes32 driver) external view returns (address);
        function value() external view returns (uint256 mark, uint256 exit);
    }
    interface IVaultCash {
        function cash() external view returns (uint256);
    }
    interface IHyperliquidDriver {
        struct Perp { uint32 index; uint8 szDecimals; }
        struct Spot { uint32 pair; uint64 token; uint8 szDecimals; uint8 weiDecimals; }
        function perps() external view returns (Perp[] memory);
        function spots() external view returns (Spot[] memory);
    }
}

/// The driver's name as a station keys it: the name, zero-padded.
pub fn driver_name() -> B256 {
    let mut name = [0u8; 32];
    name[..DRIVER.len()].copy_from_slice(DRIVER.as_bytes());
    B256::from(name)
}

/// One vault's Hyperliquid account, and the key that trades it.
pub struct Half {
    pub trader: Trader,
    api: Api,
    chain: Client,
    vault: Address,
    station: Address,
}

impl Half {
    /// Read what the station's pinned driver lists, and the venue's numbers
    /// for it, and hold `agent` to them.
    pub async fn connect(
        chain: &Client,
        vault: Address,
        station: Address,
        agent: PrivateKeySigner,
    ) -> Result<Arc<Self>> {
        let api = Api::for_chain(chain.chain_id)
            .ok_or_else(|| anyhow!("chain {} is not HyperEVM", chain.chain_id))?;
        let driver = chain
            .read(
                station,
                IPinnedStation::pinnedDriverCall {
                    driver: driver_name(),
                },
            )
            .await
            .context("reading the station's pinned Hyperliquid driver")?;
        let perps: Vec<u32> = chain
            .read(driver, IHyperliquidDriver::perpsCall {})
            .await?
            .into_iter()
            .map(|perp| perp.index)
            .collect();
        let spots: Vec<u32> = chain
            .read(driver, IHyperliquidDriver::spotsCall {})
            .await?
            .into_iter()
            .map(|spot| spot.pair)
            .collect();
        let markets = Markets::read(&api).await?;
        let allowlist = Allowlist::from_driver(station, markets, &perps, &spots)?;
        Ok(Arc::new(Self {
            trader: Trader::new(api.clone(), agent, allowlist),
            api,
            chain: chain.clone(),
            vault,
            station,
        }))
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        self.trader.submit_payload(payload).await
    }

    /// The account now, as a payload.
    pub async fn state(&self) -> Result<Vec<u8>> {
        let mut state = hyperliquid::events::account_state(
            &self.api,
            &self.trader.allowlist.markets,
            self.station,
        )
        .await?;
        let cash = self.chain.read(self.vault, IVaultCash::cashCall {}).await?;
        let held = self
            .chain
            .read(self.station, IPinnedStation::valueCall {})
            .await?
            .mark;
        state.idle = dollars(cash);
        state.vault_value = dollars(cash + held);
        Ok(hyperliquid::agent::encode(&state))
    }

    /// HyperCore's count of deposits of exactly `millionths` into the
    /// account lately, read before a deposit is sent: the deposit is
    /// confirmed once the count is past this.
    pub async fn deposit_watch(&self, millionths: u64) -> Result<DepositWatch> {
        DepositWatch::start(&self.api, self.station, millionths).await
    }

    /// Whether HyperCore has credited the deposit `watch` was started for,
    /// looking for at most [`DEPOSIT_CONFIRM_WAIT`].
    pub async fn deposit_credited(&self, watch: &DepositWatch) -> bool {
        watch
            .credited(&self.api, DEPOSIT_CONFIRM_WAIT, DEPOSIT_CONFIRM_EVERY)
            .await
    }

    /// Hand the account's events to the vault's feed, as payloads, until
    /// the task is dropped.
    pub fn forward_events(&self, sender: UnboundedSender<Vec<u8>>) -> tokio::task::JoinHandle<()> {
        let mut events = hyperliquid::events::account_events(
            self.api.network,
            self.station,
            self.trader.allowlist.markets.clone(),
        );
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if sender.send(hyperliquid::agent::encode(&event)).is_err() {
                    return;
                }
            }
        })
    }
}

// HYPE for sends home ------------------------------------------------------

/// The HYPE the account keeps on HyperCore at the least. Every send of
/// dollars from HyperCore to the station on HyperEVM (a sweep, a claim's
/// delivery, a stopped station's unwind) costs the account a little HYPE,
/// and the on-chain driver refuses to start a close without enough for one;
/// this keeps several sends' worth, above the driver's own threshold.
pub const HYPE_FLOOR: &str = "0.05";
/// What a top-up aims for: the floor twice over, or the venue's smallest
/// order, whichever buys more.
pub const HYPE_TARGET: &str = "0.1";
/// The most HYPE a top-up may leave the account holding: the reserve the
/// on-chain driver treats as the cost of sending home rather than a holding
/// a close must sell. A top-up that would leave more is not made (it says
/// so in the log); at a HYPE price under $12 / (this − the floor) the
/// venue's smallest order no longer fits under it.
pub const HYPE_RESERVE: &str = "1";
/// The venue refuses an order under ten dollars: a top-up is at least this.
pub const HYPE_BUY_LEAST_USD: &str = "12";
/// And never more than this, at its limit price: a top-up that would cost
/// more (a book far from where HYPE trades) is not made.
pub const HYPE_BUY_MOST_USD: &str = "25";
/// How far above the best ask the immediate-or-cancel buy may fill.
pub const HYPE_BUY_REACH: &str = "0.02";
/// How often the account's HYPE is looked at.
const HYPE_LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
/// At most one top-up in this long, and at most [`HYPE_BUYS_A_DAY`] a day:
/// a bound on what the upkeep can spend (four buys of at most $25).
const HYPE_BUY_SPACING: std::time::Duration = std::time::Duration::from_secs(600);
const HYPE_BUYS_A_DAY: usize = 4;

/// A top-up of the account's HYPE: an immediate-or-cancel buy of `size` at
/// no more than `limit`, after moving `from_perp` dollars to spot to pay
/// for it.
#[derive(Clone, Debug, PartialEq)]
pub struct HypeBuy {
    pub size: hyperliquid::payload::Decimal,
    pub limit: hyperliquid::payload::Decimal,
    pub from_perp: hyperliquid::payload::Decimal,
}

/// What to buy, if the account's HYPE (`held`) is under the floor: enough to
/// reach the target, and at least the venue's smallest order; limited to
/// [`HYPE_BUY_REACH`] above the best ask and to [`HYPE_BUY_MOST_USD`]; paid
/// from spot dollars first, then from what the perp side can spare. `None`
/// when nothing is needed, there is no ask, or the account can't pay.
pub fn plan_hype_buy(
    held: hyperliquid::payload::Decimal,
    ask: Option<hyperliquid::payload::Decimal>,
    spot_usdc: hyperliquid::payload::Decimal,
    perp_free: hyperliquid::payload::Decimal,
    sz_decimals: u32,
) -> Option<HypeBuy> {
    use hyperliquid::payload::Decimal;
    let decimal = |text: &str| -> Decimal { text.parse().unwrap_or(Decimal::ZERO) };
    let zero = Decimal::ZERO;
    if held >= decimal(HYPE_FLOOR) {
        return None;
    }
    let ask = ask.filter(|ask| *ask > zero)?;
    let limit = five_figures_up(ask * (Decimal::from(1) + decimal(HYPE_BUY_REACH)));
    let wanted = decimal(HYPE_TARGET) - held;
    let least = decimal(HYPE_BUY_LEAST_USD) / ask;
    let scale = Decimal::from(10u64.pow(sz_decimals));
    let size = ((if wanted > least { wanted } else { least }) * scale).ceil() / scale;
    let cost = size * limit;
    if size <= zero || cost > decimal(HYPE_BUY_MOST_USD) || held + size > decimal(HYPE_RESERVE) {
        return None;
    }
    // A little over the cost, for the venue's fee.
    let needed = cost * decimal("1.01");
    let from_perp = if spot_usdc >= needed {
        zero
    } else {
        // Whole cents, rounded up.
        ((needed - spot_usdc) * Decimal::from(100)).ceil() / Decimal::from(100)
    };
    if from_perp > perp_free {
        return None;
    }
    Some(HypeBuy {
        size,
        limit,
        from_perp,
    })
}

/// A number the venue's API gave, as a decimal (zero if it can't be read).
fn decimal_of(value: f64) -> hyperliquid::payload::Decimal {
    format!("{value}")
        .parse()
        .unwrap_or(hyperliquid::payload::Decimal::ZERO)
}

/// A price to five significant figures (the most the venue takes), rounded
/// up, so a buy's limit is never under the price asked for.
fn five_figures_up(price: hyperliquid::payload::Decimal) -> hyperliquid::payload::Decimal {
    use hyperliquid::payload::Decimal;
    if price <= Decimal::ZERO {
        return price;
    }
    let ten = Decimal::from(10);
    let mut scale = Decimal::from(1);
    while price * scale < Decimal::from(10_000) {
        scale *= ten;
    }
    while price * scale >= Decimal::from(100_000) {
        scale /= ten;
    }
    (price * scale).ceil() / scale
}

impl Half {
    /// The pinned HYPE/USDC market, if the station's driver lists it: the
    /// only market a top-up may trade.
    fn hype_market(&self) -> Option<hyperliquid::Market> {
        self.trader
            .allowlist
            .markets
            .get("HYPE/USDC")
            .filter(|market| market.is_spot())
            .cloned()
    }

    /// Look at the account's HYPE every minute and top it up when it falls
    /// under the floor, until the task is dropped. Says what it did, or why
    /// it couldn't, in the log.
    pub fn hype_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let half = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(HYPE_LOOK_EVERY);
            let mut bought: std::collections::VecDeque<tokio::time::Instant> = Default::default();
            let mut warned = false;
            loop {
                tick.tick().await;
                let now = tokio::time::Instant::now();
                while bought.front().is_some_and(|at| {
                    now.duration_since(*at) > std::time::Duration::from_secs(86_400)
                }) {
                    bought.pop_front();
                }
                if bought.len() >= HYPE_BUYS_A_DAY
                    || bought
                        .back()
                        .is_some_and(|at| now.duration_since(*at) < HYPE_BUY_SPACING)
                {
                    continue;
                }
                match half.top_up_hype().await {
                    Ok(Some(buy)) => {
                        bought.push_back(now);
                        tracing::info!(
                            station = %half.station,
                            size = %buy.size,
                            limit = %buy.limit,
                            from_perp = %buy.from_perp,
                            "bought HYPE on HyperCore for the account's sends home"
                        );
                    }
                    Ok(None) => warned = false,
                    Err(error) => {
                        if !warned {
                            tracing::warn!(
                                station = %half.station,
                                error = %format!("{error:#}"),
                                "the account's HYPE for sends home is low and wasn't topped up"
                            );
                        }
                        warned = true;
                    }
                }
            }
        })
    }

    /// One look at the account's HYPE, and a top-up if it is low: the buy
    /// made, `None` if none was needed, or why one was needed and not made.
    pub async fn top_up_hype(&self) -> Result<Option<HypeBuy>> {
        use hyperliquid::payload::{Action, Balance, Decimal, Order, Side, Tif};
        let held = decimal_of(self.api.spot_balance(self.station, "HYPE").await?);
        if held >= HYPE_FLOOR.parse().unwrap_or(Decimal::ZERO) {
            return Ok(None);
        }
        let market = self.hype_market().ok_or_else(|| {
            anyhow!("the station's driver doesn't list the HYPE/USDC pair, the only market a top-up may use")
        })?;
        let (_, ask) = self.api.best_bid_ask(&market.coin).await?;
        let spot_usdc = decimal_of(self.api.spot_balance(self.station, "USDC").await?);
        let perp_free = decimal_of(self.api.perp_withdrawable(self.station).await?);
        let buy = plan_hype_buy(
            held,
            ask.map(decimal_of),
            spot_usdc,
            perp_free,
            market.sz_decimals,
        )
        .ok_or_else(|| {
            anyhow!(
                "HYPE is {held}, under {HYPE_FLOOR}, and no buy fits (best ask {ask:?}, spot dollars {spot_usdc}, perp free {perp_free})"
            )
        })?;
        if buy.from_perp > Decimal::ZERO {
            self.trader
                .submit(&Action::MoveDollars {
                    from: Balance::Perp,
                    amount: buy.from_perp,
                })
                .await
                .context("moving dollars to spot for HYPE")?;
        }
        self.trader
            .submit(&Action::Place(Order {
                market: market.name.clone(),
                side: Side::Buy,
                size: buy.size,
                limit_price: buy.limit,
                tif: Tif::Ioc,
                reduce_only: false,
                client_id: None,
            }))
            .await
            .context("buying HYPE")?;
        Ok(Some(buy))
    }
}

/// How long a deposit into HyperCore is looked for before the move is
/// reported as not confirmed. HyperCore credits a deposit from HyperEVM in
/// the next few blocks, seconds; a minute leaves room for a slow API.
pub const DEPOSIT_CONFIRM_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const DEPOSIT_CONFIRM_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
/// How far back the ledger is read, on the venue's clock, so a clock a
/// little off doesn't hide the deposit: what counts is the rise in the
/// number of matching deposits, not their times.
const DEPOSIT_LOOKBACK_MS: u64 = 10 * 60 * 1000;

/// A deposit into the station's HyperCore account, watched for in the
/// account's ledger: deposits of exactly its amount, counted before it was
/// sent and looked for again after.
#[derive(Clone, Debug)]
pub struct DepositWatch {
    account: Address,
    millionths: u64,
    since_ms: u64,
    before: usize,
}

impl DepositWatch {
    pub async fn start(api: &Api, account: Address, millionths: u64) -> Result<Self> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();
        let since_ms = now_ms.saturating_sub(DEPOSIT_LOOKBACK_MS);
        let before = api.deposits_of(account, since_ms, millionths).await?;
        Ok(Self {
            account,
            millionths,
            since_ms,
            before,
        })
    }

    /// Whether the ledger shows one more such deposit than it did before,
    /// looking every `every` for at most `within`. A read that fails is
    /// looked at again, never taken for a deposit.
    pub async fn credited(
        &self,
        api: &Api,
        within: std::time::Duration,
        every: std::time::Duration,
    ) -> bool {
        let until = tokio::time::Instant::now() + within;
        loop {
            if let Ok(now) = api
                .deposits_of(self.account, self.since_ms, self.millionths)
                .await
                && now > self.before
            {
                return true;
            }
            if tokio::time::Instant::now() + every > until {
                return false;
            }
            tokio::time::sleep(every).await;
        }
    }
}

/// Until when, in seconds, the venue takes `agent` as the station's named
/// agent: zero if it doesn't (never approved, replaced, or an approval the
/// venue dropped because the account didn't exist yet). What the station's
/// approval is compared with, as the stand-in venue's own record is.
pub async fn agent_valid_until(chain_id: u64, station: Address, agent: Address) -> Result<u64> {
    let api =
        Api::for_chain(chain_id).ok_or_else(|| anyhow!("chain {chain_id} is not HyperEVM"))?;
    let agents = api
        .info(serde_json::json!({"type": "extraAgents", "user": format!("{station:#x}")}))
        .await
        .context("reading the station's agents from the venue")?;
    Ok(valid_until_of(&agents, agent))
}

/// [`agent_valid_until`], from the venue's answer: milliseconds there.
fn valid_until_of(agents: &serde_json::Value, agent: Address) -> u64 {
    agents
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["address"]
                .as_str()
                .and_then(|address| address.parse::<Address>().ok())
                == Some(agent)
        })
        .filter_map(|entry| entry["validUntil"].as_u64())
        .map(|ms| ms / 1000)
        .max()
        .unwrap_or(0)
}

/// Millionths of a dollar, as a decimal number of dollars.
fn dollars(millionths: alloy::primitives::U256) -> hyperliquid::payload::Decimal {
    let units = u64::try_from(millionths).unwrap_or(u64::MAX);
    hyperliquid::payload::Decimal::from(units) / hyperliquid::payload::Decimal::from(1_000_000u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_is_named_as_the_station_keys_it() {
        let name = driver_name();
        assert_eq!(&name[..11], b"hyperliquid");
        assert!(name[11..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn an_agent_s_validity_is_read_from_the_venue_in_seconds_and_zero_when_absent() {
        let agent: Address = "0x369f7e7819454c8a5c62e8ee1f73f1b2f94844dc"
            .parse()
            .unwrap();
        let answer = serde_json::json!([
            {"name": "shonin", "address": "0x369f7e7819454c8a5c62e8ee1f73f1b2f94844dc", "validUntil": 1791498294000u64},
            {"name": "other", "address": "0x1111111111111111111111111111111111111111", "validUntil": 1u64},
        ]);
        assert_eq!(valid_until_of(&answer, agent), 1_791_498_294);
        assert_eq!(valid_until_of(&answer, Address::repeat_byte(2)), 0);
        assert_eq!(valid_until_of(&serde_json::json!([]), agent), 0);
    }

    /// A stand-in for HyperCore's `/info`: the account's ledger holds one
    /// deposit of `amount` once it has been asked `credit_after` times (never,
    /// for `None`), and answers anything else with an empty list.
    async fn stand_in_venue(amount: &'static str, credit_after: Option<usize>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let asked = asked.clone();
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 8192];
                    let mut read = 0;
                    loop {
                        let Ok(n) = socket.read(&mut buffer[read..]).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        read += n;
                        let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let length: usize = head
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().ok())?
                                })
                                .unwrap_or(0);
                            if body.len() >= length {
                                break;
                            }
                        }
                    }
                    let seen = asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let ledger = if credit_after.is_some_and(|after| seen >= after) {
                        serde_json::json!([{"time": 1, "delta": {"type": "deposit", "usdc": amount}}])
                    } else {
                        serde_json::json!([])
                    };
                    let body = ledger.to_string();
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_deposit_is_confirmed_once_hypercore_s_ledger_shows_it() {
        let url = stand_in_venue("650.0", Some(3)).await;
        let api = Api::at(&url, hyperliquid::Network::Testnet);
        let station = Address::repeat_byte(0x51);
        let watch = DepositWatch::start(&api, station, 650_000_000)
            .await
            .unwrap();
        assert!(
            watch
                .credited(
                    &api,
                    std::time::Duration::from_secs(5),
                    std::time::Duration::from_millis(10)
                )
                .await
        );
    }

    #[tokio::test]
    async fn a_deposit_hypercore_never_credits_is_not_confirmed() {
        let url = stand_in_venue("650.0", None).await;
        let api = Api::at(&url, hyperliquid::Network::Testnet);
        let station = Address::repeat_byte(0x51);
        let watch = DepositWatch::start(&api, station, 650_000_000)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        assert!(
            !watch
                .credited(
                    &api,
                    std::time::Duration::from_millis(300),
                    std::time::Duration::from_millis(50)
                )
                .await
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait is bounded"
        );
    }

    #[tokio::test]
    async fn an_earlier_deposit_of_the_same_amount_is_not_taken_for_this_one() {
        // The ledger already shows one such deposit when the watch starts,
        // and never a second.
        let url = stand_in_venue("650.0", Some(0)).await;
        let api = Api::at(&url, hyperliquid::Network::Testnet);
        let watch = DepositWatch::start(&api, Address::repeat_byte(0x51), 650_000_000)
            .await
            .unwrap();
        assert!(
            !watch
                .credited(
                    &api,
                    std::time::Duration::from_millis(200),
                    std::time::Duration::from_millis(50)
                )
                .await
        );
    }

    #[tokio::test]
    async fn a_deposit_of_another_amount_does_not_confirm_this_one() {
        let url = stand_in_venue("649.0", Some(1)).await;
        let api = Api::at(&url, hyperliquid::Network::Testnet);
        let watch = DepositWatch::start(&api, Address::repeat_byte(0x51), 650_000_000)
            .await
            .unwrap();
        assert!(
            !watch
                .credited(
                    &api,
                    std::time::Duration::from_millis(200),
                    std::time::Duration::from_millis(50)
                )
                .await
        );
    }

    fn d(text: &str) -> hyperliquid::payload::Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn a_top_up_never_leaves_more_than_the_reserve() {
        // At $34, 0.36 HYPE: under the reserve.
        let buy = plan_hype_buy(d("0.04"), Some(d("34")), d("100"), d("100"), 2).unwrap();
        assert!(d("0.04") + buy.size <= d(HYPE_RESERVE));
        // At $5, the venue's smallest order is 2.4 HYPE: over it, so none.
        assert_eq!(
            plan_hype_buy(d("0.04"), Some(d("5")), d("100"), d("100"), 2),
            None
        );
        // The floor sits under the reserve.
        assert!(d(HYPE_FLOOR) < d(HYPE_RESERVE));
    }

    #[test]
    fn hype_over_the_floor_is_left_alone() {
        assert_eq!(
            plan_hype_buy(d("0.05"), Some(d("34")), d("100"), d("100"), 2),
            None
        );
        assert_eq!(
            plan_hype_buy(d("3"), Some(d("34")), d("100"), d("100"), 2),
            None
        );
    }

    #[test]
    fn a_low_balance_buys_at_least_the_venue_s_smallest_order_within_the_bounds() {
        // 0.01 HYPE at an ask of 34: the target needs 0.09, the venue's
        // smallest order 12 / 34 = 0.353 HYPE, rounded up to 0.36.
        let buy = plan_hype_buy(d("0.01"), Some(d("34")), d("100"), d("0"), 2).unwrap();
        assert_eq!(buy.size, d("0.36"));
        // Two per cent over the ask, to five figures, rounded up.
        assert_eq!(buy.limit, d("34.68"));
        assert_eq!(buy.from_perp, d("0"));
        assert!(buy.size * buy.limit <= d(HYPE_BUY_MOST_USD));
        assert!(buy.size * d("34") >= d("10"), "under the venue's minimum");
    }

    #[test]
    fn a_top_up_is_paid_from_perp_only_for_what_spot_lacks() {
        let buy = plan_hype_buy(d("0"), Some(d("34")), d("5"), d("100"), 2).unwrap();
        // 0.36 at 34.68 is 12.4848; with 1% for the fee 12.609648; less the
        // 5 on spot, 7.61 in whole cents, rounded up.
        assert_eq!(buy.from_perp, d("7.61"));
        // An account that can't pay buys nothing.
        assert_eq!(
            plan_hype_buy(d("0"), Some(d("34")), d("5"), d("1"), 2),
            None
        );
    }

    #[test]
    fn no_top_up_without_an_ask_or_past_the_most_it_may_cost() {
        assert_eq!(plan_hype_buy(d("0"), None, d("100"), d("100"), 2), None);
        assert_eq!(
            plan_hype_buy(d("0"), Some(d("0")), d("100"), d("100"), 2),
            None
        );
        // A HYPE so dear that one whole size step costs more than the cap.
        assert_eq!(
            plan_hype_buy(d("0"), Some(d("400")), d("1000"), d("1000"), 0),
            None
        );
    }

    #[test]
    fn millionths_read_as_dollars() {
        let value = dollars(alloy::primitives::U256::from(20_403_787u64));
        assert_eq!(value, "20.403787".parse().unwrap());
    }
}
