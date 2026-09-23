//! What a strategy can reach, and what happens when it does.
//!
//! Every import of the `shonin:strategy` world is implemented here. The shape
//! of the thing is the point: a strategy asks, the host decides, and the only
//! request that can move money is a submission the venue's adapter has decoded
//! and the vault's own contract has agreed to. Nothing a strategy can say
//! reaches a key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::Address;
use anyhow::Result;
use chain::bindings::vault::IVault;
use chain::bindings::venue::{ILocalVenue, OrderRequest};
use chain::{Client, KeyClient};
use strategy_sdk::{Account, Action, MarketState, PositionState, Receipt, Side};
use tokio::sync::mpsc::UnboundedReceiver;
use wasmtime::AsContextMut;
use wasmtime::Result as HostResult;
use wasmtime::component::{Access, Accessor, HasSelf, Resource, ResourceTable, StreamReader};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::feed::ChannelProducer;
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal, to_units};

wasmtime::component::bindgen!({
    path: "../strategy-sdk/wit",
    world: "strategy",
    imports: { default: async | store | trappable },
    exports: { default: async },
});

use shonin::strategy::types::Verdict;
use shonin::strategy::vault::VaultEvent;
/// A socket to the outside world. This deployment opens none, so no value of
/// this type is ever made.
use shonin::strategy::websocket::Connection;

/// What the strategy is allowed to know about, and how to act for it.
pub struct Ctx {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Reading the chain.
    pub chain: Client,
    /// Acting on it, as the key blessed for this vault.
    pub key: KeyClient,
    pub vault: Address,
    pub venue: Address,
    /// The venue this strategy's manifest named.
    pub venue_id: String,
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
impl shonin::strategy::types::Host for Ctx {}
impl shonin::strategy::venues::Host for Ctx {}
impl shonin::strategy::vault::Host for Ctx {}
impl shonin::strategy::data::Host for Ctx {}
impl shonin::strategy::websocket::Host for Ctx {}
impl shonin::strategy::secrets::Host for Ctx {}
impl shonin::strategy::state::Host for Ctx {}
impl shonin::strategy::inference::Host for Ctx {}

// Venues ---------------------------------------------------------------------

impl shonin::strategy::venues::HostWithStore<Ctx> for HasSelf<Ctx> {
    /// The one way out. Each action is decoded here, inside the measured
    /// image; anything that does not decode, or names an account this vault
    /// does not own, is refused before it reaches a key.
    async fn submit(
        accessor: &Accessor<Ctx, Self>,
        venue: String,
        actions: Vec<Vec<u8>>,
    ) -> HostResult<Vec<Verdict>> {
        let (key, vault, venue_address, expected) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (ctx.key.clone(), ctx.vault, ctx.venue, ctx.venue_id.clone())
        });

        if venue != expected {
            let refusal = format!("this strategy trades {expected}, not {venue}");
            return Ok(actions
                .iter()
                .map(|_| Verdict::Rejected(refusal.clone()))
                .collect());
        }

        let mut verdicts = Vec::with_capacity(actions.len());
        let mut refused = 0;
        for bytes in &actions {
            let (verdict, counts) = match strategy_sdk::from_payload::<Action>(bytes) {
                Ok(action) => act(&key, vault, venue_address, action).await,
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
        venue: String,
        _query: Vec<u8>,
    ) -> HostResult<Result<Vec<u8>, String>> {
        let (chain, vault, venue_address, expected) = accessor.with(|mut access| {
            let ctx = access.get();
            (
                ctx.chain.clone(),
                ctx.vault,
                ctx.venue,
                ctx.venue_id.clone(),
            )
        });
        if venue != expected {
            return Ok(Err(format!("this strategy trades {expected}, not {venue}")));
        }
        match read_account_state(&chain, vault, venue_address).await {
            Ok(state) => Ok(Ok(strategy_sdk::to_payload(&state))),
            Err(error) => Ok(Err(error.to_string())),
        }
    }

    async fn account_events(
        mut access: Access<'_, Ctx, Self>,
        _venue: String,
    ) -> HostResult<StreamReader<Vec<u8>>> {
        refuel(&mut access);
        let events = access.get().account_events.take();
        stream_of(&mut access, events)
    }
}

// The vault ------------------------------------------------------------------

impl shonin::strategy::vault::HostWithStore<Ctx> for HasSelf<Ctx> {
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

impl shonin::strategy::data::HostWithStore<Ctx> for HasSelf<Ctx> {
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

impl shonin::strategy::websocket::HostWithStore<Ctx> for HasSelf<Ctx> {
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

impl shonin::strategy::websocket::HostConnectionWithStore<Ctx> for HasSelf<Ctx> {
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

impl shonin::strategy::websocket::HostConnection for Ctx {}

// Secrets, state and inference -----------------------------------------------

impl shonin::strategy::secrets::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn get(_access: Access<'_, Ctx, Self>, _name: String) -> HostResult<Option<String>> {
        // Secrets are sealed to a bundle's hash and unsealed inside the
        // enclave. This deployment holds none, and a strategy that reads one
        // gets nothing rather than something wrong.
        Ok(None)
    }
}

impl shonin::strategy::state::HostWithStore<Ctx> for HasSelf<Ctx> {
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

impl shonin::strategy::inference::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn infer(
        _accessor: &Accessor<Ctx, Self>,
        model: String,
        _inputs: Vec<shonin::strategy::inference::NamedTensor>,
    ) -> HostResult<
        Result<
            Vec<shonin::strategy::inference::NamedTensor>,
            shonin::strategy::inference::InferenceError,
        >,
    > {
        // Models are carried in a bundle and run by the executor's own
        // runtime. This deployment loads none, so every name is unknown, and
        // an unknown model is an error the strategy handles rather than a
        // stall.
        Ok(Err(
            shonin::strategy::inference::InferenceError::UnknownModel(model),
        ))
    }
}

// Doing what a strategy asked ------------------------------------------------

/// Do one action, and say whether the answer counts against the strategy's
/// health. An order that found nobody at its price is the market's answer, not
/// the venue's: a strategy whose orders are not crossing is trading, and one
/// whose orders are being refused is not.
async fn act(key: &KeyClient, vault: Address, venue: Address, action: Action) -> (Verdict, bool) {
    let done = match action {
        Action::Place(order) => place(key, vault, venue, &order).await,
        Action::MoveCapital { from, to, amount } => {
            move_capital(key, vault, from, to, amount).await
        }
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
    vault: Address,
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
                master: vault,
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

async fn move_capital(
    key: &KeyClient,
    vault: Address,
    from: Account,
    to: Account,
    amount: strategy_sdk::Decimal,
) -> std::result::Result<Receipt, Refusal> {
    key.send(
        vault,
        IVault::moveCapitalCall {
            from: account_id(from),
            to: account_id(to),
            amount: to_units(amount, ASSET_DECIMALS)?,
        },
    )
    .await?;
    Ok(Receipt::Moved { from, to, amount })
}

// Reading the chain ----------------------------------------------------------

pub async fn read_account_state(
    chain: &Client,
    vault: Address,
    venue: Address,
) -> Result<strategy_sdk::AccountState> {
    let cash = chain
        .read(venue, ILocalVenue::cashOfCall { master: vault })
        .await?;
    let value = chain
        .read(venue, ILocalVenue::accountValueCall { master: vault })
        .await?;
    let exit = chain
        .read(venue, ILocalVenue::exitValueCall { master: vault })
        .await?;
    let margin = chain.read(venue, ILocalVenue::marginBpsCall {}).await?;
    let held = chain
        .read(venue, ILocalVenue::marketsHeldByCall { master: vault })
        .await?;
    let vault_value = chain.read(vault, IVault::totalValueCall {}).await?;
    let idle = chain.read(vault, IVault::idleCall {}).await?;

    let mut positions = Vec::new();
    for market in held {
        let position = chain
            .read(
                venue,
                ILocalVenue::positionOfCall {
                    master: vault,
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

fn account_id(account: Account) -> alloy::primitives::FixedBytes<32> {
    match account {
        Account::Idle => chain::accounts::IDLE,
        Account::Venue => chain::accounts::VENUE,
    }
}
