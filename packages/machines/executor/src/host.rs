//! What a strategy can reach, and what happens when it does.
//!
//! Every import of the `shonin:agent` world is implemented here. The shape of
//! the thing is the point: an agent asks, the host decides, and the only
//! requests that can move money are a submission the driver has decoded and a
//! transfer the vault's station has checked. Nothing an agent can say reaches
//! a key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Result, anyhow};
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStation;
use chain::bindings::vault::IVault;
use chain::bindings::venue::{ILocalVenue, OrderRequest};
use chain::{Client, KeyClient};
use strategy_sdk::{Action, MarketState, PositionState, Receipt, Side};
use tokio::sync::mpsc::UnboundedReceiver;
use wasmtime::AsContextMut;
use wasmtime::Result as HostResult;
use wasmtime::component::{Access, Accessor, HasSelf, Resource, ResourceTable, StreamReader};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::feed::ChannelProducer;
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal, to_units};

wasmtime::component::bindgen!({
    path: "../../strategy-sdk/wit",
    world: "agent",
    imports: { default: async | store | trappable },
    exports: { default: async },
});

use shonin::agent::transfers::{Account, Balance, VenueAccount};
use shonin::agent::types::Verdict;
use shonin::agent::vault::VaultEvent;
/// A socket to the outside world. This deployment opens none, so no value of
/// this type is ever made.
use shonin::agent::websocket::Connection;

/// What the strategy is allowed to know about, and how to act for it.
pub struct Ctx {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Reading the chain.
    pub chain: Client,
    /// Acting on it, as the key blessed for this vault.
    pub key: KeyClient,
    pub vault: Address,
    /// The vault's station on this chain: the master of its venue account,
    /// and the only thing that moves its money.
    pub station: Address,
    pub venue: Address,
    pub usd: Address,
    pub factory: Address,
    /// The vault's own chain, as `shonin.toml` names it.
    pub home_chain: String,
    /// The driver this agent's manifest named. The executor refuses a
    /// manifest naming one this image doesn't carry.
    pub driver: String,
    /// The per-strategy cache. It is promised to nobody: a restart on another
    /// machine starts with none of it, and a strategy rebuilds what it needs
    /// from the venue.
    pub cache: HashMap<String, Vec<u8>>,
    /// Filled by the watcher, drained by the strategy. Taken when the strategy
    /// first asks for the stream.
    pub account_events: Option<UnboundedReceiver<Vec<u8>>>,
    pub vault_events: Option<UnboundedReceiver<VaultEvent>>,
    /// What the health report is made of. Shared, so a report can be sent
    /// while the strategy is still running: a strategy's `run` is meant never
    /// to return.
    pub counters: Arc<Mutex<Counters>>,
    /// How much fuel a second of running is worth, and when it was last
    /// topped up.
    /// The most memory the strategy's instance may hold, from its manifest.
    pub memory_bytes: usize,
}

impl Ctx {
    fn count(&self, change: impl FnOnce(&mut Counters)) {
        if let Ok(mut counters) = self.counters.lock() {
            change(&mut counters);
        }
    }
}

/// Top the strategy back up for the time that has passed.
///
/// Fuel is kept topped up, because it is not what bounds a strategy here: the
/// engine's epoch is, and it takes the machine back every few milliseconds
/// whatever the strategy is doing. What the declared fuel rate decides is how
/// often the strategy yields, and what its vault's tank is charged for. Until
/// the mapping from fuel to processor time is measured, a rate that stopped a
/// strategy mid-thought would be an arbitrary number deciding whether a
/// strategy works at all.
fn refuel<S>(access: &mut Access<'_, Ctx, S>)
where
    S: wasmtime::component::HasData,
    for<'a> S::Data<'a>: std::ops::DerefMut<Target = Ctx>,
{
    let mut store = access.as_context_mut();
    if store.get_fuel().unwrap_or(0) < crate::runner::BANK / 2 {
        let _ = store.set_fuel(crate::runner::BANK);
    }
}

/// What the executor counts while a strategy runs, and signs into its health
/// report. Operational, never a record of what the strategy did: that is on
/// the chain.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub submissions: u64,
    pub rejections: u64,
    pub venue_errors: u64,
    pub faults: u64,
    pub restarts: u64,
}

/// The memory allowance, enforced. A strategy that asks for more than it
/// declared is refused the growth, which it sees as an allocation failure and
/// the host counts as a fault if it cannot carry on.
impl wasmtime::ResourceLimiter for Ctx {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.memory_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// The interfaces with no host state of their own still need their marker.
impl shonin::agent::types::Host for Ctx {}
impl shonin::agent::drivers::Host for Ctx {}
impl shonin::agent::transfers::Host for Ctx {}
impl shonin::agent::vault::Host for Ctx {}
impl shonin::agent::data::Host for Ctx {}
impl shonin::agent::websocket::Host for Ctx {}
impl shonin::agent::secrets::Host for Ctx {}
impl shonin::agent::state::Host for Ctx {}
impl shonin::agent::inference::Host for Ctx {}

// Drivers ---------------------------------------------------------------------

impl shonin::agent::drivers::HostWithStore<Ctx> for HasSelf<Ctx> {
    /// The one way out. Each action is decoded here, inside the measured
    /// image; anything that does not decode, or names an account this vault
    /// does not own, is refused before it reaches a key.
    async fn submit(
        accessor: &Accessor<Ctx, Self>,
        driver: String,
        actions: Vec<Vec<u8>>,
    ) -> HostResult<Vec<Verdict>> {
        let (key, station, venue_address, expected) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (ctx.key.clone(), ctx.station, ctx.venue, ctx.driver.clone())
        });

        if driver != expected {
            let refusal = format!("this agent trades through {expected}, not {driver}");
            return Ok(actions
                .iter()
                .map(|_| Verdict::Rejected(refusal.clone()))
                .collect());
        }

        let mut verdicts = Vec::with_capacity(actions.len());
        let mut refused = 0;
        for bytes in &actions {
            let (verdict, counts) = match strategy_sdk::from_payload::<Action>(bytes) {
                Ok(action) => act(&key, station, venue_address, action).await,
                Err(error) => (
                    Verdict::Rejected(format!("that is not an action: {error}")),
                    true,
                ),
            };
            if counts {
                refused += 1;
            }
            verdicts.push(verdict);
        }
        accessor.with(|mut access| {
            refuel(&mut access);
            access.get().count(|counters| {
                counters.submissions += actions.len() as u64;
                counters.rejections += refused;
            });
        });
        Ok(verdicts)
    }

    /// The vault's own account, as the venue reports it, with the vault beside
    /// it because a strategy sizes against the vault rather than the venue.
    async fn account_state(
        accessor: &Accessor<Ctx, Self>,
        driver: String,
        _query: Vec<u8>,
    ) -> HostResult<Result<Vec<u8>, String>> {
        let (chain, vault, station, venue_address, usd, expected) = accessor.with(|mut access| {
            let ctx = access.get();
            (
                ctx.chain.clone(),
                ctx.vault,
                ctx.station,
                ctx.venue,
                ctx.usd,
                ctx.driver.clone(),
            )
        });
        if driver != expected {
            return Ok(Err(format!(
                "this agent trades through {expected}, not {driver}"
            )));
        }
        match read_account_state(&chain, vault, station, venue_address, usd).await {
            Ok(state) => Ok(Ok(strategy_sdk::to_payload(&state))),
            Err(error) => Ok(Err(error.to_string())),
        }
    }

    async fn account_events(
        mut access: Access<'_, Ctx, Self>,
        _driver: String,
    ) -> HostResult<StreamReader<Vec<u8>>> {
        refuel(&mut access);
        let events = access.get().account_events.take();
        stream_of(&mut access, events)
    }
}

// Transfers --------------------------------------------------------------------

/// One call a move is made of, sent to the station as the vault's key.
enum Step {
    Pull,
    Push,
    Deposit,
    Withdraw,
}

/// The calls that take money from one account to another on the vault's own
/// chain. Anything else is refused here, and the station would refuse it too.
fn route(
    home: &str,
    driver: &str,
    source: &Account,
    destination: &Account,
) -> Result<Vec<Step>, String> {
    let here = |chain: &str| {
        if chain == home {
            Ok(())
        } else {
            Err(format!(
                "this executor serves the vault's station on {home} only, not on {chain}"
            ))
        }
    };
    let venue = |account: &VenueAccount| {
        here(&account.chain)?;
        if account.driver == driver {
            Ok(())
        } else {
            Err(format!(
                "this agent trades through {driver}, not {}",
                account.driver
            ))
        }
    };
    match (source, destination) {
        (Account::Vault, Account::Station(chain)) => here(chain).map(|_| vec![Step::Pull]),
        (Account::Station(chain), Account::Vault) => here(chain).map(|_| vec![Step::Push]),
        (Account::Station(chain), Account::Venue(to)) => {
            here(chain)?;
            venue(to).map(|_| vec![Step::Deposit])
        }
        (Account::Venue(from), Account::Station(chain)) => {
            here(chain)?;
            venue(from).map(|_| vec![Step::Withdraw])
        }
        (Account::Vault, Account::Venue(to)) => venue(to).map(|_| vec![Step::Pull, Step::Deposit]),
        (Account::Venue(from), Account::Vault) => {
            venue(from).map(|_| vec![Step::Withdraw, Step::Push])
        }
        _ => Err("money only moves between different accounts of the vault's own".to_string()),
    }
}

impl shonin::agent::transfers::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn home_chain(mut access: Access<'_, Ctx, Self>) -> HostResult<String> {
        refuel(&mut access);
        Ok(access.get().home_chain.clone())
    }

    async fn move_(
        accessor: &Accessor<Ctx, Self>,
        source: Account,
        destination: Account,
        amount: u64,
    ) -> HostResult<Result<(), String>> {
        let (key, station, home, driver) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (
                ctx.key.clone(),
                ctx.station,
                ctx.home_chain.clone(),
                ctx.driver.clone(),
            )
        });
        let steps = match route(&home, &driver, &source, &destination) {
            Ok(steps) => steps,
            Err(why) => return Ok(Err(why)),
        };
        let amount = U256::from(amount);
        let name = chain::names::name(driver.as_bytes());
        for step in steps {
            let sent = match step {
                Step::Pull => key.send(station, IStation::pullCall { amount }).await,
                Step::Push => key.send(station, IStation::pushCall { amount }).await,
                Step::Deposit => {
                    key.send(
                        station,
                        IStation::depositCall {
                            driver: name,
                            amount,
                        },
                    )
                    .await
                }
                Step::Withdraw => {
                    key.send(
                        station,
                        IStation::withdrawCall {
                            driver: name,
                            amount,
                        },
                    )
                    .await
                }
            };
            if let Err(error) = sent {
                accessor.with(|mut access| access.get().count(|c| c.rejections += 1));
                return Ok(Err(format!("{error:#}")));
            }
        }
        Ok(Ok(()))
    }

    async fn bridge(
        accessor: &Accessor<Ctx, Self>,
        name: String,
        source_chain: String,
        destination_chain: String,
        amount: u64,
    ) -> HostResult<Result<(), String>> {
        let (chain, key, station, factory, home) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (
                ctx.chain.clone(),
                ctx.key.clone(),
                ctx.station,
                ctx.factory,
                ctx.home_chain.clone(),
            )
        });
        if source_chain != home {
            return Ok(Err(format!(
                "this executor serves the vault's station on {home} only, not on {source_chain}"
            )));
        }
        let to_chain = match chain_id_of(&chain, factory, &destination_chain).await {
            Ok(id) => id,
            Err(error) => return Ok(Err(format!("{error:#}"))),
        };
        let sent = key
            .send(
                station,
                IStation::bridgeCall {
                    name: chain::names::name(name.as_bytes()),
                    toChain: to_chain,
                    amount: U256::from(amount),
                    extra: Bytes::new(),
                },
            )
            .await;
        Ok(sent.map(|_| ()).map_err(|error| format!("{error:#}")))
    }

    async fn balances(accessor: &Accessor<Ctx, Self>) -> HostResult<Vec<Balance>> {
        let (chain, vault, station, venue, usd, home, driver) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (
                ctx.chain.clone(),
                ctx.vault,
                ctx.station,
                ctx.venue,
                ctx.usd,
                ctx.home_chain.clone(),
                ctx.driver.clone(),
            )
        });
        let units = |value: U256| u64::try_from(value).unwrap_or(u64::MAX);
        let mut out = Vec::new();
        if let Ok(cash) = chain.read(vault, IVault::cashCall {}).await {
            out.push(Balance {
                account: Account::Vault,
                amount: units(cash),
            });
        }
        if let Ok(idle) = chain
            .read(usd, IERC20::balanceOfCall { account: station })
            .await
        {
            out.push(Balance {
                account: Account::Station(home.clone()),
                amount: units(idle),
            });
        }
        if let Ok(value) = chain
            .read(venue, ILocalVenue::accountValueCall { master: station })
            .await
        {
            out.push(Balance {
                account: Account::Venue(VenueAccount {
                    chain: home,
                    driver,
                }),
                amount: units(value),
            });
        }
        Ok(out)
    }
}

/// A chain's id from its name, as the vault factory on this chain knows it.
async fn chain_id_of(chain: &Client, factory: Address, name: &str) -> Result<U256> {
    let wanted = chain::names::name(name.as_bytes());
    let chains = chain.read(factory, IVaultFactory::chainsCall {}).await?;
    chains
        .into_iter()
        .find(|known| known.name == wanted)
        .map(|known| known.chainId)
        .ok_or_else(|| anyhow!("no chain called {name} here"))
}

// The vault ------------------------------------------------------------------

impl shonin::agent::vault::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn events(mut access: Access<'_, Ctx, Self>) -> HostResult<StreamReader<VaultEvent>> {
        refuel(&mut access);
        let events = access.get().vault_events.take();
        stream_of(&mut access, events)
    }
}

/// Hand the strategy a stream fed by the watcher, or an empty one if it has
/// already taken it. Asking twice is a mistake in the strategy, not a reason
/// to stop it.
fn stream_of<T, S>(
    access: &mut Access<'_, Ctx, S>,
    events: Option<UnboundedReceiver<T>>,
) -> HostResult<StreamReader<T>>
where
    T: wasmtime::component::Lower + wasmtime::component::Lift + Send + Sync + Unpin + 'static,
    S: wasmtime::component::HasData,
{
    match events {
        Some(events) => Ok(StreamReader::new(access, ChannelProducer::new(events))?),
        None => Ok(StreamReader::new(access, Vec::<T>::new())?),
    }
}

// The shared data layer ------------------------------------------------------

impl shonin::agent::data::HostWithStore<Ctx> for HasSelf<Ctx> {
    /// One source: the venue's own markets, cached and shared, so ten
    /// strategies watching the same book cost the executor one read.
    async fn query(
        accessor: &Accessor<Ctx, Self>,
        source: String,
        _request: Vec<u8>,
    ) -> HostResult<Result<Vec<u8>, String>> {
        if source != "venue-markets" {
            return Ok(Err(format!(
                "this executor knows no source called {source}"
            )));
        }
        let (chain, venue) = accessor.with(|mut access| {
            refuel(&mut access);
            (access.get().chain.clone(), access.get().venue)
        });
        match read_markets(&chain, venue).await {
            Ok(markets) => Ok(Ok(strategy_sdk::to_payload(&markets))),
            Err(error) => Ok(Err(error.to_string())),
        }
    }

    async fn subscribe(
        mut access: Access<'_, Ctx, Self>,
        _venue: String,
        _feed: Vec<u8>,
    ) -> HostResult<Result<StreamReader<Vec<u8>>, String>> {
        // The account stream already carries every price the venue posts, so
        // there is nothing a second subscription would add. Saying so is
        // better than handing back a stream that never speaks.
        let _ = &mut access;
        Ok(Err(
            "prices arrive on the account stream; this executor has no second feed".to_string(),
        ))
    }
}

// Sockets --------------------------------------------------------------------

impl shonin::agent::websocket::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn connect(
        _accessor: &Accessor<Ctx, Self>,
        _url: String,
        _headers: Vec<(String, String)>,
    ) -> HostResult<Result<Resource<Connection>, String>> {
        // A strategy that reaches the open internet needs the executor to
        // terminate the connection for it. This one runs against a chain and
        // a venue on the same machine and opens nothing outward, so it says
        // so rather than pretending to connect.
        Ok(Err("this executor opens no outward sockets".to_string()))
    }
}

impl shonin::agent::websocket::HostConnectionWithStore<Ctx> for HasSelf<Ctx> {
    /// `Connection` has no values, because `connect` never returns one, so
    /// there is never anything here to let go of.
    async fn drop(_accessor: &Accessor<Ctx, Self>, _rep: Resource<Connection>) -> HostResult<()> {
        Ok(())
    }

    async fn send(
        _accessor: &Accessor<Ctx, Self>,
        _self_: Resource<Connection>,
        _message: Vec<u8>,
    ) -> HostResult<Result<(), String>> {
        Ok(Err("this executor opens no outward sockets".to_string()))
    }

    async fn messages(
        mut access: Access<'_, Ctx, Self>,
        _self_: Resource<Connection>,
    ) -> HostResult<StreamReader<Vec<u8>>> {
        StreamReader::new(&mut access, Vec::<Vec<u8>>::new())
    }
}

impl shonin::agent::websocket::HostConnection for Ctx {}

// Secrets, state and inference -----------------------------------------------

impl shonin::agent::secrets::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn get(_access: Access<'_, Ctx, Self>, _name: String) -> HostResult<Option<String>> {
        // Secrets are sealed to a bundle's hash and unsealed inside the
        // enclave. This deployment holds none, and a strategy that reads one
        // gets nothing rather than something wrong.
        Ok(None)
    }
}

impl shonin::agent::state::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn get(mut access: Access<'_, Ctx, Self>, key: String) -> HostResult<Option<Vec<u8>>> {
        refuel(&mut access);
        Ok(access.get().cache.get(&key).cloned())
    }

    async fn set(mut access: Access<'_, Ctx, Self>, key: String, value: Vec<u8>) -> HostResult<()> {
        access.get().cache.insert(key, value);
        Ok(())
    }

    async fn delete(mut access: Access<'_, Ctx, Self>, key: String) -> HostResult<()> {
        access.get().cache.remove(&key);
        Ok(())
    }
}

impl shonin::agent::inference::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn infer(
        _accessor: &Accessor<Ctx, Self>,
        model: String,
        _inputs: Vec<shonin::agent::inference::NamedTensor>,
    ) -> HostResult<
        Result<
            Vec<shonin::agent::inference::NamedTensor>,
            shonin::agent::inference::InferenceError,
        >,
    > {
        // Models are carried in a bundle and run by the executor's own
        // runtime. This deployment loads none, so every name is unknown, and
        // an unknown model is an error the strategy handles rather than a
        // stall.
        Ok(Err(shonin::agent::inference::InferenceError::UnknownModel(
            model,
        )))
    }
}

// Doing what a strategy asked ------------------------------------------------

/// Do one action, and say whether the answer counts against the strategy's
/// health. An order that found nobody at its price is the market's answer, not
/// the venue's: a strategy whose orders are not crossing is trading, and one
/// whose orders are being refused is not.
async fn act(key: &KeyClient, station: Address, venue: Address, action: Action) -> (Verdict, bool) {
    let done = match action {
        Action::Place(order) => place(key, station, venue, &order).await,
    };
    match done {
        Ok(receipt) => (Verdict::Accepted(strategy_sdk::to_payload(&receipt)), false),
        Err(Refusal::NoFill) => (Verdict::Rejected(NO_FILL.to_string()), false),
        // A refusal is the venue's answer, not a fault: the strategy is told
        // why and decides what to do about it.
        Err(Refusal::Refused(why)) => (Verdict::Rejected(why), true),
    }
}

/// What the venue says when an order found no fill.
const NO_FILL: &str = "no fill at that price";

/// Why an action produced no receipt.
enum Refusal {
    /// The venue would have taken the order; nobody was there at that price.
    NoFill,
    /// The venue would not take it, and said why.
    Refused(String),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        Refusal::Refused(format!("{error}"))
    }
}

async fn place(
    key: &KeyClient,
    station: Address,
    venue: Address,
    order: &strategy_sdk::Order,
) -> std::result::Result<Receipt, Refusal> {
    let request = OrderRequest {
        market: market_id(&order.market),
        isBuy: matches!(order.side, Side::Buy),
        size: to_units(order.size, SIZE_DECIMALS)?,
        limitPrice: to_units(order.limit_price, ASSET_DECIMALS)?,
        reduceOnly: order.reduce_only,
    };
    let receipt = key
        .send(
            venue,
            ILocalVenue::placeCall {
                master: station,
                orders: vec![request],
            },
        )
        .await?;

    // What the venue did is in its own log, where anyone reading the chain
    // sees the same thing the strategy is told.
    for log in receipt.inner.logs() {
        if let Ok(filled) = log.log_decode::<ILocalVenue::Filled>() {
            let filled = filled.inner.data;
            return Ok(Receipt::Filled {
                market: order.market.clone(),
                side: if filled.isBuy { Side::Buy } else { Side::Sell },
                size: to_decimal(filled.size, SIZE_DECIMALS)?,
                price: to_decimal(filled.price, ASSET_DECIMALS)?,
                notional: to_decimal(filled.notional, ASSET_DECIMALS)?,
            });
        }
        if log.log_decode::<ILocalVenue::Unfilled>().is_ok() {
            return Err(Refusal::NoFill);
        }
        if let Ok(rejected) = log.log_decode::<ILocalVenue::Rejected>() {
            return Err(Refusal::Refused(rejected.inner.data.reason));
        }
    }
    Err(Refusal::Refused(
        "the venue said nothing about that order".to_string(),
    ))
}

// Reading the chain ----------------------------------------------------------

pub async fn read_account_state(
    chain: &Client,
    vault: Address,
    station: Address,
    venue: Address,
    usd: Address,
) -> Result<strategy_sdk::AccountState> {
    let cash = chain
        .read(venue, ILocalVenue::cashOfCall { master: station })
        .await?;
    let value = chain
        .read(venue, ILocalVenue::accountValueCall { master: station })
        .await?;
    let exit = chain
        .read(venue, ILocalVenue::exitValueCall { master: station })
        .await?;
    let margin = chain.read(venue, ILocalVenue::marginBpsCall {}).await?;
    let held = chain
        .read(venue, ILocalVenue::marketsHeldByCall { master: station })
        .await?;
    let idle = chain.read(vault, IVault::cashCall {}).await?;
    let station_idle = chain
        .read(usd, IERC20::balanceOfCall { account: station })
        .await?;
    // What the agent sizes against: everything the vault holds now, at mark.
    let vault_value = idle + station_idle + value;

    let mut positions = Vec::new();
    for market in held {
        let position = chain
            .read(
                venue,
                ILocalVenue::positionOfCall {
                    master: station,
                    market,
                },
            )
            .await?;
        positions.push(PositionState {
            market: market_name(market),
            size: signed_to_decimal(position.size, SIZE_DECIMALS)?,
        });
    }

    Ok(strategy_sdk::AccountState {
        cash: signed_to_decimal(cash, ASSET_DECIMALS)?,
        value: to_decimal(value, ASSET_DECIMALS)?,
        exit_value: to_decimal(exit, ASSET_DECIMALS)?,
        vault_value: to_decimal(vault_value, ASSET_DECIMALS)?,
        idle: to_decimal(idle, ASSET_DECIMALS)?,
        station_idle: to_decimal(station_idle, ASSET_DECIMALS)?,
        margin_bps: margin.try_into().unwrap_or(u32::MAX),
        positions,
        markets: read_markets(chain, venue).await?,
    })
}

pub async fn read_markets(chain: &Client, venue: Address) -> Result<Vec<MarketState>> {
    let ids = chain.read(venue, ILocalVenue::marketsCall {}).await?;
    let mut markets = Vec::with_capacity(ids.len());
    for id in ids {
        let market = chain
            .read(venue, ILocalVenue::marketOfCall { market: id })
            .await?;
        markets.push(MarketState {
            market: market_name(id),
            is_perp: market.isPerp,
            mark: to_decimal(market.mark, ASSET_DECIMALS)?,
            oracle: to_decimal(market.oracle, ASSET_DECIMALS)?,
            bid: to_decimal(market.bid, ASSET_DECIMALS)?,
            ask: to_decimal(market.ask, ASSET_DECIMALS)?,
            funding_index: signed_to_decimal(market.fundingIndex, ASSET_DECIMALS)?,
            funding_rate: signed_to_decimal(market.fundingRate, ASSET_DECIMALS)?,
        });
    }
    Ok(markets)
}

/// A market's name, as the contract stores it: the text, then zeros.
pub fn market_id(name: &str) -> alloy::primitives::FixedBytes<32> {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    let take = bytes.len().min(32);
    out[..take].copy_from_slice(&bytes[..take]);
    alloy::primitives::FixedBytes(out)
}

pub fn market_name(id: alloy::primitives::FixedBytes<32>) -> String {
    let end = id.0.iter().position(|byte| *byte == 0).unwrap_or(32);
    String::from_utf8_lossy(&id.0[..end]).to_string()
}
