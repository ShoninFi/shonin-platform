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
    name_of(DRIVER)
}

/// A driver's name as a station keys it: the name, zero-padded.
fn name_of(driver: &str) -> B256 {
    let mut name = [0u8; 32];
    name[..driver.len()].copy_from_slice(driver.as_bytes());
    B256::from(name)
}

/// The dollar on HyperCore, by the venue's number for it, as the driver
/// contract numbers it.
const USDC_TOKEN: u64 = 0;
/// The name of the token sends home are paid in.
const GAS_TOKEN_NAME: &str = "HYPE";
/// How often the venue's markets are read again to find which of the
/// pinned driver's markets it doesn't list for trading.
const MISSING_LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

/// One vault's Hyperliquid account, and the key that trades it.
pub struct Half {
    pub trader: Trader,
    api: Api,
    chain: Client,
    vault: Address,
    station: Address,
    /// The dollar on HyperEVM, which the station holds idle.
    usd: Address,
    /// The venue's numbers for the perps and spot pairs the pinned driver
    /// lists.
    perps: Vec<u32>,
    spots: Vec<u32>,
    /// The pinned spot pair of the token sends home are paid in, and the
    /// venue's number for that token, as the driver contract picks it: the
    /// last of its pairs whose token the venue names HYPE.
    gas: Option<(u32, u64)>,
}

impl Half {
    /// Read what the driver the station pinned under `pinned_as` (the name
    /// the vault's recorded manifest gives it) lists, and the venue's
    /// numbers for it, and hold `agent` to them.
    pub async fn connect(
        chain: &Client,
        vault: Address,
        station: Address,
        usd: Address,
        agent: PrivateKeySigner,
        pinned_as: &str,
    ) -> Result<Arc<Self>> {
        let api = Api::for_chain(chain.chain_id)
            .ok_or_else(|| anyhow!("chain {} is not HyperEVM", chain.chain_id))?;
        let spec = chain
            .read(station, chain::bindings::station::IStation::specCall {})
            .await
            .context("reading the station's drivers")?;
        if !spec.drivers.contains(&name_of(pinned_as)) {
            anyhow::bail!(
                "the station doesn't list the {pinned_as} driver the vault's manifest names"
            );
        }
        let driver = chain
            .read(
                station,
                IPinnedStation::pinnedDriverCall {
                    driver: name_of(pinned_as),
                },
            )
            .await
            .context("reading the station's pinned Hyperliquid driver")?;
        if driver == Address::ZERO {
            anyhow::bail!("the station has no driver pinned as {pinned_as}");
        }
        let perps: Vec<u32> = chain
            .read(driver, IHyperliquidDriver::perpsCall {})
            .await?
            .into_iter()
            .map(|perp| perp.index)
            .collect();
        let pinned = chain.read(driver, IHyperliquidDriver::spotsCall {}).await?;
        let spots: Vec<u32> = pinned.iter().map(|spot| spot.pair).collect();
        let markets = Markets::read(&api).await?;
        let gas = pinned
            .iter()
            .rev()
            .find(|spot| markets.token_name(spot.token) == Some(GAS_TOKEN_NAME))
            .map(|spot| (spot.pair, spot.token));
        let allowlist = Allowlist::from_driver(station, markets, &perps, &spots)?;
        for (market, why) in &allowlist.missing {
            tracing::warn!(
                %station,
                market,
                why,
                "a market the station's driver lists is not traded: the venue doesn't list it for trading now, and it is looked for again"
            );
        }
        Ok(Arc::new(Self {
            trader: Trader::new(api.clone(), agent, allowlist),
            api,
            chain: chain.clone(),
            vault,
            station,
            usd,
            perps,
            spots,
            gas,
        }))
    }

    /// For the life of the half, read the venue's markets again every
    /// [`MISSING_LOOK_EVERY`], and hold the agent key to the venue's markets
    /// as they are listed then whenever the set of the pinned driver's
    /// markets missing from them has changed, either way.
    pub fn missing_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let half = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(MISSING_LOOK_EVERY).await;
                let markets = match Markets::read(&half.api).await {
                    Ok(markets) => markets,
                    Err(error) => {
                        tracing::warn!(
                            station = %half.station,
                            error = %format!("{error:#}"),
                            "the venue's markets couldn't be read to look for the missing ones"
                        );
                        continue;
                    }
                };
                let Ok(allowlist) =
                    Allowlist::from_driver(half.station, markets, &half.perps, &half.spots)
                else {
                    continue;
                };
                let was = half.trader.allowlist();
                if allowlist.missing != was.missing {
                    for market in was.missing.keys() {
                        if !allowlist.missing.contains_key(market) {
                            tracing::info!(
                                station = %half.station,
                                market,
                                "a market the station's driver lists is listed for trading again, and traded"
                            );
                        }
                    }
                    for (market, why) in &allowlist.missing {
                        if !was.missing.contains_key(market) {
                            tracing::warn!(
                                station = %half.station,
                                market,
                                why,
                                "a market the station's driver lists is no longer listed for trading, and is not traded"
                            );
                        }
                    }
                    half.trader.hold_to(allowlist);
                }
            }
        })
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        self.trader.submit_payload(payload).await
    }

    /// The account now, as a payload.
    pub async fn state(&self) -> Result<Vec<u8>> {
        let mut state = hyperliquid::events::account_state(
            &self.api,
            &self.trader.allowlist().markets,
            self.station,
        )
        .await?;
        let cash = self.chain.read(self.vault, IVaultCash::cashCall {}).await?;
        let held = self
            .chain
            .read(self.station, IPinnedStation::valueCall {})
            .await?
            .mark;
        let station_idle = self
            .chain
            .read(
                self.usd,
                chain::bindings::erc20::IERC20::balanceOfCall {
                    account: self.station,
                },
            )
            .await?;
        with_the_vault(&mut state, cash, held, station_idle);
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
            self.trader.allowlist().markets.clone(),
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

/// The free HYPE (what open orders don't hold) the account keeps on
/// HyperCore at the least. Every send of dollars from HyperCore to the
/// station on HyperEVM (a sweep, a claim's delivery, a stopped station's
/// unwind) costs the account a little HYPE, and the on-chain driver refuses
/// to start a claim's close with less than [`DRIVER_GAS_FLOOR`] free; this
/// keeps several sends' worth above that.
pub const HYPE_FLOOR: &str = "0.05";
/// The driver's own line: with less free HYPE than this, every claim's close
/// is blocked. Said loudly whenever the account is under it.
pub const DRIVER_GAS_FLOOR: &str = "0.01";
/// What a top-up aims for, in free HYPE: the floor twice over, or the
/// venue's smallest order, whichever buys more.
pub const HYPE_TARGET: &str = "0.1";
/// The most HYPE in all a top-up may leave the account holding: the reserve
/// the on-chain driver treats as the cost of sending home rather than a
/// holding a close must sell. A top-up that would leave more is not made (it
/// says so in the log).
pub const HYPE_RESERVE: &str = "1";
/// The venue refuses an order under ten dollars: a top-up is at least this.
pub const HYPE_BUY_LEAST_USD: &str = "12";
/// The most a top-up may cost at its limit, unless twice the target's HYPE
/// costs more at that limit: then that, so a dear HYPE is still bought, and
/// never more than twice the target of it.
pub const HYPE_BUY_MOST_USD: &str = "25";
/// How far above the best ask the immediate-or-cancel buy may fill.
pub const HYPE_BUY_REACH: &str = "0.02";
/// How far above the venue's own spot price (the price the driver values
/// HYPE at) a top-up may pay: a book whose ask is further off is not bought
/// from, and the log says HYPE costs more than the top-up may pay.
pub const HYPE_BUY_OVER_SPOT: &str = "0.05";
/// How often the account's HYPE is looked at.
const HYPE_LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
/// At most one top-up in this long, and at most [`HYPE_BUYS_A_DAY`] a day:
/// a bound on what the upkeep can spend. Counted from the venue's own record
/// of the account's fills (the upkeep's orders carry their own tag), so a
/// restart doesn't reset it, and from the executor's own record of buys
/// whose outcome it couldn't learn.
const HYPE_BUY_SPACING_MS: u64 = 600_000;
const HYPE_BUYS_A_DAY: usize = 4;
const DAY_MS: u64 = 86_400_000;

/// A top-up of the account's HYPE: an immediate-or-cancel buy of `size` at
/// no more than `limit`, after moving `from_perp` dollars to spot to pay
/// for it.
#[derive(Clone, Debug, PartialEq)]
pub struct HypeBuy {
    pub size: hyperliquid::payload::Decimal,
    pub limit: hyperliquid::payload::Decimal,
    pub from_perp: hyperliquid::payload::Decimal,
}

/// Why no top-up is planned though the account's free HYPE is under the
/// floor.
#[derive(Clone, Debug, PartialEq)]
pub struct NoBuy {
    pub why: String,
    /// HYPE costs more than a top-up may pay.
    pub too_dear: bool,
}

/// What the account holds and what the venue asks, for a top-up's plan.
#[derive(Clone, Copy, Debug)]
pub struct HypeBook {
    /// HYPE not held by open orders, and HYPE in all.
    pub free: hyperliquid::payload::Decimal,
    pub total: hyperliquid::payload::Decimal,
    /// The best ask on the pair, and the venue's own spot price of it.
    pub ask: Option<hyperliquid::payload::Decimal>,
    pub spot: hyperliquid::payload::Decimal,
    /// Spot dollars not held by open orders, and what the perp side can
    /// spare.
    pub spot_usdc: hyperliquid::payload::Decimal,
    pub perp_free: hyperliquid::payload::Decimal,
    pub sz_decimals: u32,
}

/// What to buy, if the account's free HYPE is under the floor: enough to
/// reach the target, and at least the venue's smallest order; limited to
/// [`HYPE_BUY_REACH`] above the best ask and [`HYPE_BUY_OVER_SPOT`] above
/// the venue's spot price; never leaving more than [`HYPE_RESERVE`] in all;
/// paid from spot dollars first, then from what the perp side can spare.
/// `Ok(None)` when nothing is needed.
pub fn plan_hype_buy(book: &HypeBook) -> std::result::Result<Option<HypeBuy>, NoBuy> {
    use hyperliquid::payload::Decimal;
    let decimal = |text: &str| -> Decimal { text.parse().unwrap_or(Decimal::ZERO) };
    let zero = Decimal::ZERO;
    let no = |why: String| NoBuy {
        why,
        too_dear: false,
    };
    if book.free >= decimal(HYPE_FLOOR) {
        return Ok(None);
    }
    let Some(ask) = book.ask.filter(|ask| *ask > zero) else {
        return Err(no("the HYPE/USDC book has no ask".to_string()));
    };
    if book.spot <= zero {
        return Err(no("the venue gives no spot price for HYPE".to_string()));
    }
    let cap = book.spot * (Decimal::from(1) + decimal(HYPE_BUY_OVER_SPOT));
    let mut limit = five_figures_up(ask * (Decimal::from(1) + decimal(HYPE_BUY_REACH)));
    if limit > cap {
        limit = five_figures_down(cap);
    }
    if limit < ask {
        return Err(NoBuy {
            why: format!(
                "HYPE costs more than a top-up may pay: the best ask is {ask}, the venue's spot price {}, and a top-up pays at most {HYPE_BUY_OVER_SPOT} over it ({limit})",
                book.spot
            ),
            too_dear: true,
        });
    }
    let scale = Decimal::from(10u64.pow(book.sz_decimals));
    let wanted = decimal(HYPE_TARGET) - book.free;
    let least = decimal(HYPE_BUY_LEAST_USD) / ask;
    let size = ((if wanted > least { wanted } else { least }) * scale).ceil() / scale;
    let cost = size * limit;
    let most = {
        let target = Decimal::from(2) * decimal(HYPE_TARGET) * limit;
        let fixed = decimal(HYPE_BUY_MOST_USD);
        if target > fixed { target } else { fixed }
    };
    if size <= zero || cost > most {
        return Err(NoBuy {
            why: format!("a top-up of {size} HYPE at {limit} would cost {cost}, more than {most}"),
            too_dear: true,
        });
    }
    if book.total + size > decimal(HYPE_RESERVE) {
        return Err(no(format!(
            "a top-up of {size} HYPE would leave {} HYPE, more than the {HYPE_RESERVE} the driver keeps for sends home",
            book.total + size
        )));
    }
    // A little over the cost, for the venue's fee.
    let needed = cost * decimal("1.01");
    let from_perp = if book.spot_usdc >= needed {
        zero
    } else {
        // Whole cents, rounded up.
        ((needed - book.spot_usdc) * Decimal::from(100)).ceil() / Decimal::from(100)
    };
    if from_perp > book.perp_free {
        return Err(no(format!(
            "the account can't pay {needed} for HYPE: {} spot dollars free and {} to spare on the perp side",
            book.spot_usdc, book.perp_free
        )));
    }
    Ok(Some(HypeBuy {
        size,
        limit,
        from_perp,
    }))
}

/// A number the venue's API gave, as a decimal (zero if it can't be read).
fn decimal_of(value: f64) -> hyperliquid::payload::Decimal {
    format!("{value}")
        .parse()
        .unwrap_or(hyperliquid::payload::Decimal::ZERO)
}

/// The power of ten that puts `price` between 10,000 and 100,000.
fn five_figure_scale(price: hyperliquid::payload::Decimal) -> hyperliquid::payload::Decimal {
    use hyperliquid::payload::Decimal;
    let ten = Decimal::from(10);
    let mut scale = Decimal::from(1);
    while price * scale < Decimal::from(10_000) {
        scale *= ten;
    }
    while price * scale >= Decimal::from(100_000) {
        scale /= ten;
    }
    scale
}

/// A price to five significant figures (the most the venue takes), rounded
/// up, so a buy's limit is never under the price asked for.
fn five_figures_up(price: hyperliquid::payload::Decimal) -> hyperliquid::payload::Decimal {
    if price <= hyperliquid::payload::Decimal::ZERO {
        return price;
    }
    let scale = five_figure_scale(price);
    (price * scale).ceil() / scale
}

/// The same, rounded down, so a cap is never passed.
fn five_figures_down(price: hyperliquid::payload::Decimal) -> hyperliquid::payload::Decimal {
    if price <= hyperliquid::payload::Decimal::ZERO {
        return price;
    }
    let scale = five_figure_scale(price);
    (price * scale).floor() / scale
}

/// The upkeep's HYPE buys of the last day, by client id, with when each was
/// made (milliseconds): from the venue's record of the account's fills on
/// the pair (only the upkeep's orders carry its tag), and from `attempts`,
/// the buys this executor sent whose outcome it couldn't learn or that the
/// venue took.
pub fn upkeep_buys(
    fills: &[serde_json::Value],
    coin: &str,
    attempts: &[(u128, u64)],
    now_ms: u64,
) -> std::collections::BTreeMap<u128, u64> {
    let since = now_ms.saturating_sub(DAY_MS);
    let mut buys = std::collections::BTreeMap::new();
    let mut add = |id: u128, at: u64| {
        if at >= since {
            let earliest = buys.entry(id).or_insert(at);
            *earliest = (*earliest).min(at);
        }
    };
    for fill in fills {
        let tagged = fill["cloid"]
            .as_str()
            .filter(|cloid| hyperliquid::agent::is_upkeep_cloid(cloid))
            .and_then(|cloid| u128::from_str_radix(cloid.trim_start_matches("0x"), 16).ok());
        if let (Some(id), Some(at), true, true) = (
            tagged,
            fill["time"].as_u64(),
            fill["coin"] == coin,
            fill["side"] == "B",
        ) {
            add(id, at);
        }
    }
    for (id, at) in attempts {
        add(*id, *at);
    }
    buys
}

/// Whether a top-up may be made now, given the day's buys: none in the last
/// [`HYPE_BUY_SPACING_MS`], and fewer than [`HYPE_BUYS_A_DAY`] in the day.
pub fn may_buy(buys: &std::collections::BTreeMap<u128, u64>, now_ms: u64) -> bool {
    buys.len() < HYPE_BUYS_A_DAY
        && buys
            .values()
            .all(|at| now_ms.saturating_sub(*at) >= HYPE_BUY_SPACING_MS)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// What one look at the account's HYPE found and did.
#[derive(Clone, Debug, PartialEq)]
pub enum TopUp {
    /// Free HYPE at or over the floor.
    Enough,
    /// A close is staged at the station: nothing is bought until it ends
    /// (a buy would be charged to the leaver), and the close was staged
    /// with enough HYPE free.
    Staged,
    /// The day's buys are spent, or the last was too recent.
    Spaced,
    Bought(HypeBuy),
}

impl Half {
    /// The pinned spot pair of the token sends home are paid in, if the
    /// venue lists it for trading now: the only market a top-up may trade.
    fn hype_market(&self) -> Option<hyperliquid::Market> {
        let (pair, token) = self.gas?;
        self.trader
            .allowlist()
            .markets
            .spot_pair(pair)
            .filter(|market| {
                market.kind
                    == hyperliquid::markets::Kind::Spot {
                        pair,
                        base_token: token,
                    }
            })
            .cloned()
    }

    /// The account's holding of the token sends home are paid in.
    async fn hype_holding(&self) -> Result<hyperliquid::api::SpotHolding> {
        let (_, token) = self.gas.ok_or_else(|| {
            anyhow!("the station's driver lists no pair of the token sends home are paid in")
        })?;
        self.api.spot_holding(self.station, token).await
    }

    /// Look at the account's HYPE every minute and top it up when its free
    /// HYPE falls under the floor, until the task is dropped. Says what it
    /// did, or why it couldn't, in the log; and says loudly, at every look
    /// while it lasts, when the account has less free HYPE than the driver
    /// needs to start a claim's close, and whether only the strategy's own
    /// orders can end it. (The agent's event stream has no event for it: the
    /// log is where it is said.)
    pub fn hype_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let half = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(HYPE_LOOK_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut attempts: Vec<(u128, u64)> = Vec::new();
            let mut warned = false;
            loop {
                tick.tick().await;
                attempts.retain(|(_, at)| now_ms().saturating_sub(*at) < DAY_MS);
                let look = half.top_up_hype(&mut attempts).await;
                match &look {
                    Ok(TopUp::Bought(buy)) => {
                        warned = false;
                        tracing::info!(
                            station = %half.station,
                            size = %buy.size,
                            limit = %buy.limit,
                            from_perp = %buy.from_perp,
                            "bought HYPE on HyperCore for the account's sends home"
                        );
                    }
                    Ok(_) => warned = false,
                    Err(error) => {
                        let dear = error
                            .downcast_ref::<NoBuyError>()
                            .is_some_and(|no| no.0.too_dear);
                        // HYPE dearer than a top-up may pay is said at every
                        // look; anything else once until it changes.
                        if dear || !warned {
                            tracing::warn!(
                                station = %half.station,
                                error = %format!("{error:#}"),
                                "the account's HYPE for sends home is low and wasn't topped up"
                            );
                        }
                        warned = true;
                    }
                }
                // Said at every look (once a minute) while it lasts.
                if let Ok(hype) = half.hype_holding().await
                    && blocked(&hype)
                {
                    tracing::error!(
                        station = %half.station,
                        free = %hype.free(),
                        held = %hype.hold,
                        total = %hype.total,
                        floor = DRIVER_GAS_FLOOR,
                        "{}",
                        blocked_why(&hype)
                    );
                }
            }
        })
    }

    /// One look at the account's HYPE, and a top-up if its free HYPE is
    /// low: what it found or did, or why a top-up was needed and not made.
    /// `attempts` holds the buys this executor sent and could not see in
    /// the venue's fills; a buy sent now is added to it.
    pub async fn top_up_hype(&self, attempts: &mut Vec<(u128, u64)>) -> Result<TopUp> {
        use hyperliquid::payload::{Action, Balance, Decimal, Order, Side, Tif};
        let hype = self.hype_holding().await?;
        if hype.free() >= HYPE_FLOOR.parse().unwrap_or(Decimal::ZERO) {
            return Ok(TopUp::Enough);
        }
        if self.close_staged().await? {
            return Ok(TopUp::Staged);
        }
        let market = self.hype_market().ok_or_else(|| {
            anyhow!("the venue doesn't list the HYPE pair the station's driver lists for trading now, the only market a top-up may use")
        })?;
        let now = now_ms();
        let fills = self
            .api
            .fills_since(self.station, now.saturating_sub(DAY_MS), 10)
            .await
            .context("reading the account's fills, which count the day's top-ups")?;
        let buys = upkeep_buys(&fills, &market.coin, attempts, now);
        if !may_buy(&buys, now) {
            return Ok(TopUp::Spaced);
        }
        let (_, ask) = self.api.best_bid_ask(&market.coin).await?;
        let spot = self.api.spot_price(&market.coin).await?;
        let usdc = self.api.spot_holding(self.station, USDC_TOKEN).await?;
        let perp_free = decimal_of(self.api.perp_withdrawable(self.station).await?);
        let buy = plan_hype_buy(&HypeBook {
            free: hype.free(),
            total: hype.total,
            ask: ask.map(decimal_of),
            spot,
            spot_usdc: usdc.free(),
            perp_free,
            sz_decimals: market.sz_decimals,
        })
        .map_err(|no| {
            anyhow::Error::new(NoBuyError(no))
                .context(format!("free HYPE is {}, under {HYPE_FLOOR}", hype.free()))
        })?;
        let Some(buy) = buy else {
            return Ok(TopUp::Enough);
        };
        if buy.from_perp > Decimal::ZERO {
            self.trader
                .submit(&Action::MoveDollars {
                    from: Balance::Perp,
                    amount: buy.from_perp,
                })
                .await
                .context("moving dollars to spot for HYPE")?;
        }
        // A close may have been staged since the stage was first read: the
        // buy is sent only if the stage, read again now, is still empty (the
        // dollars moved to spot stay the account's, worth the same).
        if self.close_staged().await? {
            return Ok(TopUp::Staged);
        }
        let (outcome, client) = self
            .trader
            .upkeep(&Order {
                market: market.name.clone(),
                side: Side::Buy,
                size: buy.size,
                limit_price: buy.limit,
                tif: Tif::Ioc,
                reduce_only: false,
                client_id: None,
            })
            .await;
        match outcome {
            hyperliquid::agent::Outcome::Accepted(_) => {
                attempts.push((client, now));
                Ok(TopUp::Bought(buy))
            }
            hyperliquid::agent::Outcome::Unknown(why) => {
                // It may have bought: counted as a buy all the same.
                attempts.push((client, now));
                Err(anyhow!(
                    "a HYPE buy got no answer, and may have been made: {why}"
                ))
            }
            hyperliquid::agent::Outcome::Refused(why) => Err(anyhow!("buying HYPE: {why}")),
        }
    }
}

impl Half {
    /// Whether a close is staged at the station now.
    async fn close_staged(&self) -> Result<bool> {
        let stage = self
            .chain
            .read(
                self.station,
                chain::bindings::station::IStation::stageCall {},
            )
            .await
            .context("reading whether a close is staged at the station")?;
        Ok(stage.stagedAt != 0)
    }
}

/// Whether the account has less free HYPE than the driver needs to start a
/// claim's close.
pub fn blocked(hype: &hyperliquid::api::SpotHolding) -> bool {
    hype.free() < DRIVER_GAS_FLOOR.parse().unwrap_or_default()
}

/// Why the account's closes are blocked, as said in the log: when open
/// orders hold so much of the account's HYPE that no top-up can be bought
/// under the reserve, only the strategy that rests them can free it.
pub fn blocked_why(hype: &hyperliquid::api::SpotHolding) -> String {
    use hyperliquid::payload::Decimal;
    let reserve: Decimal = HYPE_RESERVE.parse().unwrap_or_default();
    let floor: Decimal = HYPE_FLOOR.parse().unwrap_or_default();
    if hype.total + (floor - hype.free()) > reserve {
        format!(
            "every claim's close at this station is blocked: the account's open orders hold {} of its {} HYPE, leaving {} free, under the {DRIVER_GAS_FLOOR} the driver needs to send dollars home; no top-up can be bought without leaving more than the {HYPE_RESERVE} HYPE reserve, so it lasts until the strategy cancels or fills some of its HYPE orders",
            hype.hold,
            hype.total,
            hype.free()
        )
    } else {
        format!(
            "every claim's close at this station is blocked: the account has {} free HYPE ({} held by open orders), under the {DRIVER_GAS_FLOOR} the driver needs to send dollars home",
            hype.free(),
            hype.hold
        )
    }
}

/// [`NoBuy`] as an error, so the loop can tell a HYPE too dear to buy.
#[derive(Debug)]
pub struct NoBuyError(pub NoBuy);

impl std::fmt::Display for NoBuyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.why)
    }
}

impl std::error::Error for NoBuyError {}

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
/// account's ledger: the deposits of exactly its amount there before it was
/// sent, and, after, one more that no other watch has taken.
#[derive(Clone, Debug)]
pub struct DepositWatch {
    account: Address,
    millionths: u64,
    since_ms: u64,
    before: std::collections::HashSet<String>,
}

/// How long a credit taken for one deposit is kept from every other: far
/// longer than any watch lasts, or than an entry stays in the window read.
const CREDIT_TAKEN_FOR: std::time::Duration = std::time::Duration::from_secs(3_600);

/// The ledger entries already taken as some deposit's credit, by account:
/// one credit confirms one deposit, however many of the same amount are
/// watched for at once.
static CREDITS_TAKEN: std::sync::LazyLock<std::sync::Mutex<CreditsTaken>> =
    std::sync::LazyLock::new(Default::default);

/// Each account's credits taken, with when.
type CreditsTaken = std::collections::HashMap<Address, Vec<(String, std::time::Instant)>>;

/// The ledger's matching entries, each named once even when two read alike
/// (the `n`th of a kind is `entry#n`).
async fn credits(
    api: &Api,
    account: Address,
    since_ms: u64,
    millionths: u64,
) -> Result<Vec<String>> {
    let entries = api
        .deposit_entries_of(account, since_ms, millionths)
        .await?;
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    Ok(entries
        .into_iter()
        .map(|entry| {
            let n = seen.entry(entry.clone()).or_default();
            *n += 1;
            format!("{entry}#{n}")
        })
        .collect())
}

impl DepositWatch {
    pub async fn start(api: &Api, account: Address, millionths: u64) -> Result<Self> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();
        let since_ms = now_ms.saturating_sub(DEPOSIT_LOOKBACK_MS);
        let before = credits(api, account, since_ms, millionths)
            .await?
            .into_iter()
            .collect();
        Ok(Self {
            account,
            millionths,
            since_ms,
            before,
        })
    }

    /// Whether the ledger shows a credit of this amount that wasn't there
    /// when the watch started and that no other watch has taken; if so it is
    /// taken for this deposit. Looked for every `every`, for at most
    /// `within`. A read that fails is looked at again, never taken for a
    /// deposit.
    pub async fn credited(
        &self,
        api: &Api,
        within: std::time::Duration,
        every: std::time::Duration,
    ) -> bool {
        let until = tokio::time::Instant::now() + within;
        loop {
            if let Ok(now) = credits(api, self.account, self.since_ms, self.millionths).await
                && self.take(now)
            {
                return true;
            }
            if tokio::time::Instant::now() + every > until {
                return false;
            }
            tokio::time::sleep(every).await;
        }
    }

    /// Take the first new, untaken credit among `now` for this deposit.
    fn take(&self, now: Vec<String>) -> bool {
        let Ok(mut taken) = CREDITS_TAKEN.lock() else {
            return false;
        };
        let clock = std::time::Instant::now();
        let mine = taken.entry(self.account).or_default();
        mine.retain(|(_, at)| clock.duration_since(*at) < CREDIT_TAKEN_FOR);
        let fresh = now.into_iter().find(|credit| {
            !self.before.contains(credit) && !mine.iter().any(|(known, _)| known == credit)
        });
        match fresh {
            Some(credit) => {
                mine.push((credit, clock));
                true
            }
            None => false,
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
/// Fill in the vault's own figures beside the account, as the local driver
/// reports them: the vault's cash, the station's idle dollars, and the whole
/// vault (its cash and everything the station holds, at mark).
fn with_the_vault(
    state: &mut hyperliquid::payload::AccountState,
    cash: alloy::primitives::U256,
    held: alloy::primitives::U256,
    station_idle: alloy::primitives::U256,
) {
    state.idle = dollars(cash);
    state.station_idle = dollars(station_idle);
    state.vault_value = dollars(cash + held);
}

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

    /// An account with `free` HYPE free (all of it, unless `total` says
    /// more), a book asking `ask` at a spot price of 34, and dollars to pay.
    fn book(free: &str, ask: Option<&str>) -> HypeBook {
        HypeBook {
            free: d(free),
            total: d(free),
            ask: ask.map(d),
            spot: d("34"),
            spot_usdc: d("100"),
            perp_free: d("100"),
            sz_decimals: 2,
        }
    }

    #[test]
    fn a_top_up_never_leaves_more_than_the_reserve() {
        // At $34, 0.36 HYPE: under the reserve.
        let buy = plan_hype_buy(&book("0.04", Some("34"))).unwrap().unwrap();
        assert!(d("0.04") + buy.size <= d(HYPE_RESERVE));
        // At $5, the venue's smallest order is 2.4 HYPE: over it, so none.
        let cheap = HypeBook {
            spot: d("5"),
            ..book("0.04", Some("5"))
        };
        assert!(plan_hype_buy(&cheap).is_err());
        // HYPE the strategy holds counts toward the reserve too.
        let held = HypeBook {
            total: d("0.8"),
            ..book("0", Some("34"))
        };
        assert!(plan_hype_buy(&held).is_err());
        // The floor sits under the reserve, and over the driver's own line.
        assert!(d(HYPE_FLOOR) < d(HYPE_RESERVE));
        assert!(d(DRIVER_GAS_FLOOR) < d(HYPE_FLOOR));
    }

    #[test]
    fn hype_held_by_the_strategy_s_orders_over_the_reserve_is_said_to_need_the_strategy() {
        let holding = |total: &str, hold: &str| hyperliquid::api::SpotHolding {
            total: d(total),
            hold: d(hold),
        };
        // All of 1.2 HYPE on hold: blocked, and no top-up fits under the
        // reserve, so only the strategy can free it.
        let held = holding("1.2", "1.2");
        assert!(blocked(&held));
        assert!(blocked_why(&held).contains("until the strategy cancels"));
        // The same with a buy the reserve still has room for: blocked, and a
        // top-up can end it.
        let low = holding("0.3", "0.295");
        assert!(blocked(&low));
        assert!(!blocked_why(&low).contains("until the strategy cancels"));
        assert!(!blocked(&holding("1.2", "1.0")));
    }

    #[test]
    fn hype_over_the_floor_is_left_alone() {
        assert_eq!(plan_hype_buy(&book("0.05", Some("34"))), Ok(None));
        assert_eq!(plan_hype_buy(&book("3", Some("34"))), Ok(None));
    }

    #[test]
    fn hype_an_open_order_holds_is_not_counted_and_is_topped_up() {
        // A strategy resting a sell of its 0.30 HYPE: none of it is free.
        let resting = HypeBook {
            total: d("0.3"),
            ..book("0", Some("34"))
        };
        let buy = plan_hype_buy(&resting).unwrap().unwrap();
        assert!(buy.size >= d("0.1"));
        assert!(d("0.3") + buy.size <= d(HYPE_RESERVE));
    }

    #[test]
    fn a_low_balance_buys_at_least_the_venue_s_smallest_order_within_the_bounds() {
        // 0.01 HYPE at an ask of 34: the target needs 0.09, the venue's
        // smallest order 12 / 34 = 0.353 HYPE, rounded up to 0.36.
        let buy = plan_hype_buy(&HypeBook {
            perp_free: d("0"),
            ..book("0.01", Some("34"))
        })
        .unwrap()
        .unwrap();
        assert_eq!(buy.size, d("0.36"));
        // Two per cent over the ask, to five figures, rounded up.
        assert_eq!(buy.limit, d("34.68"));
        assert_eq!(buy.from_perp, d("0"));
        assert!(buy.size * buy.limit <= d(HYPE_BUY_MOST_USD));
        assert!(buy.size * d("34") >= d("10"), "under the venue's minimum");
    }

    #[test]
    fn a_top_up_is_paid_from_perp_only_for_what_spot_lacks() {
        let buy = plan_hype_buy(&HypeBook {
            spot_usdc: d("5"),
            ..book("0", Some("34"))
        })
        .unwrap()
        .unwrap();
        // 0.36 at 34.68 is 12.4848; with 1% for the fee 12.609648; less the
        // 5 on spot, 7.61 in whole cents, rounded up.
        assert_eq!(buy.from_perp, d("7.61"));
        // An account that can't pay buys nothing.
        assert!(
            plan_hype_buy(&HypeBook {
                spot_usdc: d("5"),
                perp_free: d("1"),
                ..book("0", Some("34"))
            })
            .is_err()
        );
    }

    #[test]
    fn no_top_up_without_an_ask() {
        assert!(plan_hype_buy(&book("0", None)).is_err());
        assert!(plan_hype_buy(&book("0", Some("0"))).is_err());
    }

    #[test]
    fn a_top_up_never_pays_more_than_its_bound_over_the_venue_s_spot_price() {
        // The ask is 4% over the spot price: the limit is held at 5% over it.
        let buy = plan_hype_buy(&book("0", Some("35.36"))).unwrap().unwrap();
        assert!(buy.limit <= d("34") * d("1.05"), "{}", buy.limit);
        assert!(buy.limit >= d("35.36"));
        // The ask is 6% over: too dear, and said so.
        let dear = plan_hype_buy(&book("0", Some("36.04"))).unwrap_err();
        assert!(dear.too_dear, "{}", dear.why);
        assert!(dear.why.contains("costs more"), "{}", dear.why);
    }

    #[test]
    fn a_dear_hype_is_still_bought_up_to_the_target() {
        // At $400 the target's 0.1 HYPE alone costs more than the fixed cap.
        let dear = HypeBook {
            spot: d("400"),
            spot_usdc: d("1000"),
            perp_free: d("1000"),
            ..book("0", Some("400"))
        };
        let buy = plan_hype_buy(&dear).unwrap().unwrap();
        assert_eq!(buy.size, d("0.1"));
        // A HYPE whose smallest size step is a whole HYPE: more than twice
        // the target, so none.
        let whole = HypeBook {
            sz_decimals: 0,
            ..dear
        };
        assert!(plan_hype_buy(&whole).is_err());
    }

    #[test]
    fn the_day_s_top_ups_are_counted_from_the_venue_s_fills_and_the_unanswered_buys() {
        let now = 10 * DAY_MS;
        let tagged = |nonce: u64| format!("0x{:032x}", hyperliquid::agent::upkeep_client_id(nonce));
        let fills = vec![
            // Two fills of one top-up: one buy.
            serde_json::json!({"coin": "@1035", "side": "B", "time": now - 3_600_000, "cloid": tagged(1)}),
            serde_json::json!({"coin": "@1035", "side": "B", "time": now - 3_599_000, "cloid": tagged(1)}),
            // The strategy's own buy, a sell, another coin, and a top-up
            // over a day ago: none.
            serde_json::json!({"coin": "@1035", "side": "B", "time": now - 1_000, "cloid": "0x01"}),
            serde_json::json!({"coin": "@1035", "side": "A", "time": now - 1_000, "cloid": tagged(2)}),
            serde_json::json!({"coin": "ETH", "side": "B", "time": now - 1_000, "cloid": tagged(3)}),
            serde_json::json!({"coin": "@1035", "side": "B", "time": now - DAY_MS - 1, "cloid": tagged(4)}),
        ];
        let buys = upkeep_buys(&fills, "@1035", &[], now);
        assert_eq!(buys.len(), 1);
        assert!(may_buy(&buys, now));
        // A buy sent a minute ago whose outcome is unknown: too recent.
        let attempts = [(hyperliquid::agent::upkeep_client_id(5), now - 60_000)];
        let buys = upkeep_buys(&fills, "@1035", &attempts, now);
        assert_eq!(buys.len(), 2);
        assert!(!may_buy(&buys, now));
        // Four in the day, however far apart: no more.
        let attempts: Vec<_> = (0..3)
            .map(|n| {
                (
                    hyperliquid::agent::upkeep_client_id(10 + n),
                    now - 7_200_000 * (n + 1),
                )
            })
            .collect();
        let buys = upkeep_buys(&fills, "@1035", &attempts, now);
        assert_eq!(buys.len(), 4);
        assert!(!may_buy(&buys, now));
    }

    #[test]
    fn hyperliquid_account_state_reports_station_idle() {
        use alloy::primitives::U256;
        let mut state = hyperliquid::payload::AccountState {
            vault_value: d("0"),
            idle: d("0"),
            station_idle: d("0"),
            account_value: d("40"),
            withdrawable: d("40"),
            positions: Vec::new(),
            spot: Vec::new(),
            open_orders: Vec::new(),
        };
        with_the_vault(
            &mut state,
            U256::from(100_000_000u64),
            U256::from(65_000_000u64),
            U256::from(25_000_000u64),
        );
        assert_eq!(state.idle, d("100"));
        assert_eq!(state.station_idle, d("25"));
        assert_eq!(state.vault_value, d("165"));
        // As the agent reads it.
        let read: strategy_sdk::hyperliquid::AccountState =
            strategy_sdk::from_payload(&hyperliquid::agent::encode(&state)).unwrap();
        assert_eq!(read.station_idle, d("25"));
    }

    #[test]
    fn millionths_read_as_dollars() {
        let value = dollars(alloy::primitives::U256::from(20_403_787u64));
        assert_eq!(value, "20.403787".parse().unwrap());
    }
}
