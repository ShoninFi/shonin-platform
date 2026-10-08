//! What a strategy can reach, and what happens when it does.
//!
//! Every import of the `shonin:agent` world is implemented here. The shape of
//! the thing is the point: an agent asks, the host decides, and the only
//! requests that can move money are a submission the driver has decoded and a
//! transfer the vault's station has checked. Nothing an agent can say reaches
//! a key.

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Result, anyhow};
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStation;
use chain::bindings::vault::IVault;
use chain::bindings::venue::{ILocalVenue, OrderRequest};
use chain::{Client, KeyClient};
use hyperliquid::agent::Outcome;
use strategy_sdk::{Action, MarketState, PositionState, Receipt, Side};
use wasmtime::Result as HostResult;
use wasmtime::component::{Access, Accessor, HasSelf, Resource, ResourceTable, StreamReader};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::feed::{ChannelProducer, Reader};
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal, to_units};

wasmtime::component::bindgen!({
    path: "../../strategy-sdk/wit",
    world: "agent",
    imports: { default: async | store | trappable },
    exports: { default: async },
    with: {
        // The standard WASI packages are wasmtime's own.
        "wasi:io": wasmtime_wasi::p2::bindings::io,
        "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
        "wasi:http": wasmtime_wasi_http::p2::bindings::http,
        "wasi:keyvalue/store.bucket": crate::state::Bucket,
        "shonin:agent/websocket.connection": crate::net::Connection,
    },
});

use shonin::agent::transfers::{Account, Balance, Moved, PartialMove, VenueAccount};
use shonin::agent::types::Verdict;
use shonin::agent::vault::VaultEvent;
use wasi::keyvalue::store::{Error as StoreError, KeyResponse};

/// Everything an agent can import, added to `linker`: the standard WASI
/// packages (no filesystem, environment or sockets are given, so those
/// imports find nothing), `wasi:http`, and the world's own interfaces.
pub fn link(linker: &mut wasmtime::component::Linker<Ctx>) -> Result<()> {
    wasmtime_wasi::p2::add_to_linker_async(linker)
        .map_err(|error| anyhow!("the WASI imports: {error}"))?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(linker)
        .map_err(|error| anyhow!("the wasi:http imports: {error}"))?;
    let ours = |error: wasmtime::Error| anyhow!("the platform's imports: {error}");
    shonin::agent::types::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::drivers::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::transfers::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::vault::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::data::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::websocket::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::inference::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    shonin::agent::secrets::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    wasi::keyvalue::store::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    wasi::keyvalue::atomics::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    wasi::keyvalue::batch::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx).map_err(ours)?;
    link_random(linker).map_err(|error| anyhow!("the WASI randomness imports: {error}"))?;
    Ok(())
}

/// The most random bytes one call may ask for. A call is the host's work
/// from start to end, charged once it is done: held to this, no call runs
/// long past the strategy's budget.
pub const RANDOM_AT_MOST: u64 = 1024 * 1024;

/// `wasi:random`'s calls for bytes, in place of WASI's own: at most
/// [`RANDOM_AT_MOST`] a call, made by the operating system's generator in
/// one go rather than a byte at a time. The insecure call is served the
/// same way: secure bytes are never worse.
fn link_random(linker: &mut wasmtime::component::Linker<Ctx>) -> wasmtime::Result<()> {
    linker.allow_shadowing(true);
    let linked = [
        ("wasi:random/random@0.2.12", "get-random-bytes"),
        ("wasi:random/insecure@0.2.12", "get-insecure-random-bytes"),
    ]
    .into_iter()
    .try_for_each(|(interface, function)| {
        linker.instance(interface)?.func_wrap(
            function,
            |_, (len,): (u64,)| -> wasmtime::Result<(Vec<u8>,)> { Ok((random_bytes(len)?,)) },
        )
    });
    linker.allow_shadowing(false);
    linked
}

/// `len` random bytes, or a trap past [`RANDOM_AT_MOST`].
fn random_bytes(len: u64) -> wasmtime::Result<Vec<u8>> {
    if len > RANDOM_AT_MOST {
        wasmtime::bail!("requested len {len} exceeds limit {RANDOM_AT_MOST}");
    }
    let mut bytes = vec![0; usize::try_from(len)?];
    getrandom::fill(&mut bytes)
        .map_err(|error| wasmtime::format_err!("the system's randomness: {error}"))?;
    Ok(bytes)
}

/// The stand-in venue's driver, whose half is this module's own.
pub const LOCAL: &str = "local";

/// What the strategy is allowed to know about, and how to act for it.
pub struct Ctx {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Where the vault is, and how to act for it there.
    pub place: Place,
    /// The vault's own chain, as `shonin.toml` names it.
    pub home_chain: String,
    /// The drivers this agent's manifest named. The executor refuses a
    /// manifest naming one this image doesn't carry.
    pub drivers: Vec<String>,
    /// The bundle's models, if it carries any.
    pub inference: Option<Arc<crate::inference::Inference>>,
    /// The agent's state, served as `wasi:keyvalue`. It lives as long as
    /// this run of the agent, so every start begins with it empty.
    pub state: crate::state::Store,
    /// What the agent's network may do, and what its vault has used.
    pub net: crate::net::Net,
    pub http: wasmtime_wasi_http::WasiHttpCtx,
    pub http_hooks: crate::net::Http,
    /// Filled by the watcher, drained by the strategy. Taken when the strategy
    /// first asks for the stream. They are the vault's, so whatever an agent
    /// before this one left unread is read here first.
    pub account_events: Option<Reader<Vec<u8>>>,
    pub vault_events: Option<Reader<VaultEvent>>,
    /// The stand-in venue's markets the agent follows, which its watcher
    /// wakes it for: noted here as it asks about them and trades them.
    pub following: crate::watch::Following,
    /// What the health report is made of. Shared, so a report can be sent
    /// while the strategy is still running: a strategy's `run` is meant never
    /// to return.
    pub counters: Arc<Mutex<Counters>>,
    /// Raised once the vault's halting notice has been handed to the
    /// strategy, so an agent that then ends is known to have been told.
    pub halting_heard: Arc<std::sync::atomic::AtomicBool>,
    /// The most memory the strategy's instance may hold, from its manifest.
    pub memory: Allowance,
}

/// Where a vault is, and how its agent's requests reach it. One is held for
/// each agent running, so its size doesn't matter.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Place {
    Evm(Evm),
    Solana(Arc<crate::solana::Trading>),
}

/// A vault on an EVM chain.
#[derive(Clone)]
pub struct Evm {
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
    /// The Hyperliquid driver's half, for a vault whose agent trades there.
    pub hyperliquid: Option<Arc<crate::hyperliquid::Half>>,
    /// The Robinhood driver's half: the station's Safe of stock tokens.
    pub stocks: Option<Arc<crate::robinhood::StockHalf>>,
    /// The Lighter driver's half: the station's Lighter account.
    pub lighter: Option<Arc<crate::robinhood::LighterHalf>>,
}

impl Place {
    /// The vault, as its chain writes its address.
    pub fn vault(&self) -> String {
        match self {
            Place::Evm(evm) => evm.vault.to_string(),
            Place::Solana(trading) => trading.vault.to_string(),
        }
    }
}

impl Ctx {
    pub(crate) fn count(&self, change: impl FnOnce(&mut Counters)) {
        if let Ok(mut counters) = self.counters.lock() {
            change(&mut counters);
        }
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
    /// How many times the component in the store was found not to be the
    /// vault's bundle, and was not run.
    pub not_the_bundle: u64,
    /// How many times the agent was parked for want of fuel, having burnt
    /// what its rate gives it, and when it last was (Unix seconds).
    pub held_back: u64,
    pub last_held_back: Option<u64>,
}

impl Counters {
    /// Count the agent parked for want of fuel, now.
    pub fn held_back(&mut self) {
        self.held_back = self.held_back.saturating_add(1);
        self.last_held_back = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs());
    }

    /// Count one thing the agent asked to have done, and how it was
    /// answered. A refusal or an unanswered request is always one of the
    /// submissions too, so the share refused can never pass the whole.
    fn submitted(&mut self, tally: Tally) {
        self.submissions += 1;
        match tally {
            Tally::Done => {}
            Tally::Refused => self.rejections += 1,
            Tally::Unanswered => self.venue_errors += 1,
        }
    }

    /// The share of submissions refused, as a percentage.
    pub fn rejection_rate(&self) -> f64 {
        if self.submissions == 0 {
            0.0
        } else {
            self.rejections as f64 / self.submissions as f64 * 100.0
        }
    }
}

/// The memory allowance, enforced. A strategy that asks for more than it
/// declared is refused the growth, which it sees as an allocation failure and
/// the host counts as a fault if it cannot carry on.
///
/// Its tables are held to the same allowance as its linear memory: each entry
/// is kept in the host's memory, so a table of millions of entries is memory
/// the strategy holds whether or not it is linear. Every memory and table the
/// instance makes, at instantiation and after, is counted together.
///
/// Only growth that happens is counted. Wasmtime asks before it checks the
/// memory's or table's own maximum, so a growth past that maximum is refused
/// here, uncounted, as wasmtime would refuse it after. What is left to fail
/// once a growth is let through is the host's own allocation, which is
/// counted as held: wasmtime also reports failures it never asked about, so
/// an undo could take back a growth that did happen.
///
/// A strategy is also held to a few instances, memories and tables, so it
/// can't reserve address space by making many.
#[derive(Debug)]
pub struct Allowance {
    bytes: usize,
    held: usize,
}

impl Allowance {
    /// What one table entry is counted as holding: a reference, the size of
    /// a pointer.
    pub const TABLE_ENTRY_BYTES: usize = std::mem::size_of::<usize>();

    /// The most core instances a strategy's component may make. A built
    /// strategy makes three or so.
    pub const INSTANCES: usize = 16;

    /// The most linear memories a strategy may make. Each one reserves
    /// gigabytes of address space whatever it holds.
    pub const MEMORIES: usize = 4;

    /// The most tables a strategy may make.
    pub const TABLES: usize = 16;

    pub fn new(bytes: usize) -> Self {
        Self { bytes, held: 0 }
    }

    /// What one call may hand back to the strategy: an answer is lowered
    /// into the strategy's own memory, so one bigger than its whole
    /// allowance could never be taken in, and the host doesn't build it.
    pub fn answer_room(&self) -> Room {
        Room::new(self.bytes)
    }

    fn grow(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> bool {
        if maximum.is_some_and(|maximum| desired > maximum) {
            return false;
        }
        let more = desired.saturating_sub(current);
        match self.held.checked_add(more) {
            Some(held) if held <= self.bytes => {
                self.held = held;
                true
            }
            _ => false,
        }
    }
}

/// What is left of the most one call may hand back to the strategy,
/// counted as the host would hold the answer before handing it over, and
/// taken before any of it is built. Data the strategy hands the host is held
/// to the same allowance by the store's hostcall fuel.
#[derive(Clone, Copy, Debug)]
pub struct Room {
    left: usize,
    most: usize,
}

impl Room {
    pub fn new(most: usize) -> Self {
        Self { left: most, most }
    }

    /// Take `bytes` more of the answer, or say the answer is too big.
    pub fn take(&mut self, bytes: usize) -> Result<(), TooMuch> {
        match self.left.checked_sub(bytes) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => Err(TooMuch { most: self.most }),
        }
    }

    /// What a string of the answer takes: its bytes and what holds them.
    pub fn string(text: &str) -> usize {
        std::mem::size_of::<String>().saturating_add(text.len())
    }

    /// What a list of bytes of the answer takes.
    pub fn bytes(bytes: &[u8]) -> usize {
        std::mem::size_of::<Vec<u8>>().saturating_add(bytes.len())
    }
}

/// An answer bigger than the strategy's memory allowance, refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooMuch {
    pub most: usize,
}

impl std::fmt::Display for TooMuch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the answer would take more than the agent's memory allowance of {} bytes",
            self.most
        )
    }
}

impl From<TooMuch> for StoreError {
    fn from(too_much: TooMuch) -> Self {
        StoreError::Other(too_much.to_string())
    }
}

impl wasmtime::ResourceLimiter for Allowance {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(self.grow(current, desired, maximum))
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(self.grow(
            current.saturating_mul(Self::TABLE_ENTRY_BYTES),
            desired.saturating_mul(Self::TABLE_ENTRY_BYTES),
            maximum.map(|entries| entries.saturating_mul(Self::TABLE_ENTRY_BYTES)),
        ))
    }

    fn instances(&self) -> usize {
        Self::INSTANCES
    }

    fn memories(&self) -> usize {
        Self::MEMORIES
    }

    fn tables(&self) -> usize {
        Self::TABLES
    }
}

impl wasmtime::ResourceLimiter for Ctx {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.memory.memory_growing(current, desired, maximum)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.memory.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize {
        self.memory.instances()
    }

    fn memories(&self) -> usize {
        self.memory.memories()
    }

    fn tables(&self) -> usize {
        self.memory.tables()
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

impl wasmtime_wasi_http::WasiHttpView for Ctx {
    fn http(&mut self) -> wasmtime_wasi_http::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.http_hooks,
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
impl wasi::keyvalue::store::Host for Ctx {}
impl wasi::keyvalue::store::HostBucket for Ctx {}
impl wasi::keyvalue::atomics::Host for Ctx {}
impl wasi::keyvalue::batch::Host for Ctx {}
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
        let (place, drivers, room) = accessor.with(|mut access| {
            let ctx = access.get();
            (
                ctx.place.clone(),
                ctx.drivers.clone(),
                ctx.memory.answer_room(),
            )
        });

        if let Some(refused) = refused_whole(actions.len(), room)? {
            return Ok(refused);
        }

        if !drivers.contains(&driver) {
            let refusal = format!(
                "this agent trades through {drivers:?}, not {}",
                crate::runner::at_most(&driver, NAMED_MOST)
            );
            return Ok(actions
                .iter()
                .map(|_| Verdict::Rejected(refusal.clone()))
                .collect());
        }

        let mut verdicts = Vec::with_capacity(actions.len());
        let mut tallies = Vec::with_capacity(actions.len());
        if driver != LOCAL {
            for bytes in &actions {
                let outcome = match (&place, driver.as_str()) {
                    (
                        Place::Evm(Evm {
                            hyperliquid: Some(half),
                            ..
                        }),
                        crate::hyperliquid::DRIVER,
                    ) => Some(half.submit(bytes).await),
                    (
                        Place::Evm(Evm {
                            stocks: Some(half), ..
                        }),
                        crate::robinhood::ROBINHOOD,
                    ) => Some(half.submit(bytes).await),
                    (
                        Place::Evm(Evm {
                            lighter: Some(half),
                            ..
                        }),
                        crate::robinhood::LIGHTER,
                    ) => Some(half.submit(bytes).await),
                    _ => None,
                };
                let (verdict, tally) = match outcome {
                    Some(Outcome::Accepted(receipt)) => (Verdict::Accepted(receipt), Tally::Done),
                    Some(Outcome::Refused(why)) => (Verdict::Rejected(why), Tally::Refused),
                    Some(Outcome::Unknown(why)) => (Verdict::Unknown(why), Tally::Unanswered),
                    None => (
                        Verdict::Rejected(format!(
                            "this vault has no {} account here",
                            crate::runner::at_most(&driver, NAMED_MOST)
                        )),
                        Tally::Refused,
                    ),
                };
                tallies.push(tally);
                verdicts.push(within(verdict));
            }
            accessor.with(|mut access| {
                access.get().count(|counters| {
                    for tally in tallies {
                        counters.submitted(tally);
                    }
                });
            });
            return Ok(verdicts);
        }
        // Every action that decodes is an order; those that don't are
        // refused here, and the orders beside them still go.
        let mut answers: Vec<Option<(Verdict, Tally)>> = Vec::with_capacity(actions.len());
        let mut orders = Vec::new();
        for (at, bytes) in actions.iter().enumerate() {
            match strategy_sdk::from_payload::<Action>(bytes) {
                Ok(Action::Place(order)) => {
                    orders.push((at, order));
                    answers.push(None);
                }
                Err(error) => answers.push(Some((
                    Verdict::Rejected(format!("that is not an action: {error}")),
                    Tally::Refused,
                ))),
            }
        }
        // A market the agent sends an order for is one it follows.
        accessor.with(|mut access| {
            let ctx = access.get();
            for (_, order) in &orders {
                ctx.following.market(&order.market);
            }
        });
        let (at, orders): (Vec<usize>, Vec<strategy_sdk::Order>) = orders.into_iter().unzip();
        let done = match &place {
            Place::Evm(evm) => place_all(&evm.key, evm.station, evm.venue, &orders).await,
            Place::Solana(trading) => {
                let mut done = Vec::with_capacity(orders.len());
                for order in orders {
                    done.push(trading.act(Action::Place(order)).await);
                }
                done
            }
        };
        for (at, done) in at.into_iter().zip(done) {
            answers[at] = Some(answer(done));
        }
        for (verdict, tally) in answers.into_iter().flatten() {
            tallies.push(tally);
            verdicts.push(within(verdict));
        }
        accessor.with(|mut access| {
            access.get().count(|counters| {
                for tally in tallies {
                    counters.submitted(tally);
                }
            });
        });
        Ok(verdicts)
    }

    /// The vault's own account, as the venue reports it, with the vault beside
    /// it because a strategy sizes against the vault rather than the venue.
    async fn account_state(
        accessor: &Accessor<Ctx, Self>,
        driver: String,
        query: Vec<u8>,
    ) -> HostResult<Result<Vec<u8>, String>> {
        let (place, drivers) = accessor.with(|mut access| {
            let ctx = access.get();
            (ctx.place.clone(), ctx.drivers.clone())
        });
        if !drivers.contains(&driver) {
            return Ok(Err(format!(
                "this agent trades through {drivers:?}, not {driver}"
            )));
        }
        let state = match (&place, driver.as_str()) {
            (
                Place::Evm(Evm {
                    hyperliquid: Some(half),
                    ..
                }),
                crate::hyperliquid::DRIVER,
            ) => Some(half.state().await),
            (
                Place::Evm(Evm {
                    stocks: Some(half), ..
                }),
                crate::robinhood::ROBINHOOD,
            ) => Some(half.state(&query).await),
            (
                Place::Evm(Evm {
                    lighter: Some(half),
                    ..
                }),
                crate::robinhood::LIGHTER,
            ) => Some(half.state().await),
            _ => None,
        };
        if let Some(state) = state {
            return Ok(match state {
                Ok(state) => Ok(state),
                Err(error) => {
                    accessor.with(|mut access| access.get().count(|c| c.venue_errors += 1));
                    Err(error.to_string())
                }
            });
        }
        // The markets the agent asks about are the ones it follows from now
        // on, noted before the account is read: an event the watcher passes
        // over before then is in what this read finds.
        let asked = asked_about(&query);
        accessor.with(|mut access| {
            let ctx = access.get();
            match &asked {
                Some(markets) => markets
                    .iter()
                    .for_each(|market| ctx.following.market(market)),
                None => ctx.following.every(),
            }
        });
        let state = match &place {
            Place::Evm(evm) => {
                read_account_state(
                    &evm.chain,
                    evm.vault,
                    evm.station,
                    evm.venue,
                    evm.usd,
                    asked.as_deref(),
                )
                .await
            }
            Place::Solana(trading) => trading.account_state().await,
        };
        // And the markets it holds, which it reads of here.
        if let Ok(state) = &state {
            accessor.with(|mut access| {
                let ctx = access.get();
                for position in &state.positions {
                    ctx.following.market(&position.market);
                }
            });
        }
        match state {
            Ok(state) => Ok(Ok(strategy_sdk::to_payload(&state))),
            Err(error) => {
                accessor.with(|mut access| access.get().count(|c| c.venue_errors += 1));
                Ok(Err(error.to_string()))
            }
        }
    }

    async fn account_events(
        mut access: Access<'_, Ctx, Self>,
        _driver: String,
    ) -> HostResult<StreamReader<Vec<u8>>> {
        let events = access.get().account_events.take();
        stream_of(&mut access, events)
    }
}

/// The most of a name the agent gave that a refusal repeats.
const NAMED_MOST: usize = 128;

/// The most a verdict's reason may say: a longer one is cut.
const REASON_MOST: usize = 1024;

/// The most one verdict is counted as taking of the answer to a submission:
/// a reason cut to [`REASON_MOST`] (and the few bytes cutting adds), or a
/// driver's receipt, which is a short record of its own making.
const VERDICT_MOST: usize = std::mem::size_of::<Verdict>() + REASON_MOST + 64;

/// One verdict is handed back for each action, whatever becomes of it, so a
/// submission of `actions` is weighed by the most its verdicts could take of
/// `room` before anything is sent. One whose verdicts could be too big for
/// the agent to take in is refused whole: nothing of it is sent, and each
/// action is told so in a few words. Only a submission too big for even those
/// words to be handed back is a fault in the agent.
fn refused_whole(actions: usize, room: Room) -> wasmtime::Result<Option<Vec<Verdict>>> {
    let mut most = room;
    if (0..actions)
        .try_for_each(|_| most.take(VERDICT_MOST))
        .is_ok()
    {
        return Ok(None);
    }
    let mut refusals = room;
    let refusal = std::mem::size_of::<Verdict>().saturating_add(TOO_MANY.len());
    if (0..actions)
        .try_for_each(|_| refusals.take(refusal))
        .is_err()
    {
        wasmtime::bail!(
            "a submission of {actions} actions could not be answered within the agent's memory allowance"
        );
    }
    Ok(Some(
        (0..actions)
            .map(|_| Verdict::Rejected(TOO_MANY.to_string()))
            .collect(),
    ))
}

/// What each action of a submission whose verdicts could be bigger than the
/// agent's memory allowance is told.
const TOO_MANY: &str = "too many actions in one submission; nothing was sent";

/// `verdict` with its reason cut to [`REASON_MOST`], and a receipt longer
/// than a verdict is counted as taking (which no driver makes) handed back
/// as a verdict saying the action was taken but its receipt isn't given.
fn within(verdict: Verdict) -> Verdict {
    let cut = |why: String| match crate::runner::at_most(&why, REASON_MOST) {
        std::borrow::Cow::Borrowed(_) => why,
        std::borrow::Cow::Owned(cut) => cut,
    };
    match verdict {
        Verdict::Rejected(why) => Verdict::Rejected(cut(why)),
        Verdict::Unknown(why) => Verdict::Unknown(cut(why)),
        Verdict::Accepted(receipt) if receipt.len() > REASON_MOST => Verdict::Unknown(format!(
            "taken, but its receipt of {} bytes is too long to hand back",
            receipt.len()
        )),
        accepted => accepted,
    }
}

// Transfers --------------------------------------------------------------------

/// One call a move is made of, sent to the station as the vault's key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Pull,
    Push,
    Deposit,
    Withdraw,
}

impl Step {
    /// The vault's own account the money is in once this step has landed.
    fn lands_in(self, home: &str, driver: &str) -> Account {
        match self {
            Step::Pull | Step::Withdraw => Account::Station(home.to_string()),
            Step::Push => Account::Vault,
            Step::Deposit => Account::Venue(VenueAccount {
                chain: home.to_string(),
                driver: driver.to_string(),
            }),
        }
    }
}

/// What the agent is told of a move that stopped at a step: nothing moved
/// if it was the first, otherwise as far as the steps before it took the
/// money; and whether that step may still land.
fn stopped(reached: Option<Account>, why: String, may_still_land: bool) -> Moved {
    match reached {
        None if may_still_land => Moved::Unknown(why),
        None => Moved::Refused(why),
        Some(reached) => Moved::Partial(PartialMove {
            reached,
            reason: why,
            may_still_land,
        }),
    }
}

/// Why the station may be holding money back from moving through driver
/// `name`, if one of its rules does now: a close staged there (none of the
/// executor's moves until it ends), or, for a move through the venue
/// (`through_venue`), a move through that driver lately (they are spaced, so
/// a valuation can be priced between two). Nothing when neither can be read
/// or neither holds.
async fn held_back(
    chain: &chain::Client,
    station: Address,
    name: alloy::primitives::FixedBytes<32>,
    through_venue: bool,
) -> Option<String> {
    if let Ok(stage) = chain.read(station, IStation::stageCall {}).await
        && stage.stagedAt != 0
    {
        return Some(format!(
            "refused: a close is staged at the station (since {}), and the executor's money moves wait until it ends",
            stage.stagedAt
        ));
    }
    if !through_venue {
        return None;
    }
    let moved = chain
        .read(station, IStation::movedAtCall { driver: name })
        .await
        .ok()?;
    (moved != 0).then(|| {
        format!(
            "refused, maybe for being too soon: the station spaces money moves through a venue, and the last through this one was at {moved} (chain time), so wait for a valuation before moving again"
        )
    })
}

/// What the agent is told of a deposit that left the station and that the
/// venue hasn't credited: whatever steps came before it, the money is in
/// none of the vault's accounts now but on its way to the venue, where it
/// may still arrive. Read the balances before moving it again.
fn left_for_the_venue(why: String) -> Moved {
    Moved::Unknown(format!(
        "the money left the station for the venue and hasn't been credited there: {why}"
    ))
}

/// The station's idle dollars, if they can be read.
async fn station_holds(chain: &Client, usd: Address, station: Address) -> Option<U256> {
    chain
        .read(usd, IERC20::balanceOfCall { account: station })
        .await
        .ok()
}

/// How long a withdrawal through a venue other than Hyperliquid is looked
/// for in the station's balance after its receipt: enough for an endpoint a
/// block or two behind to catch up.
const WITHDRAWAL_SEEN_WAIT: std::time::Duration = std::time::Duration::from_secs(6);

/// How far the station's idle dollars have risen over `before`, looking
/// every two seconds for at most `wait` until they have risen by `units`;
/// `None` when that can't be read. A fall (the station paying out meanwhile)
/// counts as no rise, so a rise is never more than what came in.
async fn risen_by(
    chain: &Client,
    usd: Address,
    station: Address,
    before: Option<U256>,
    units: U256,
    wait: std::time::Duration,
) -> Option<U256> {
    let before = before?;
    let until = tokio::time::Instant::now() + wait;
    loop {
        let risen = station_holds(chain, usd, station)
            .await
            .map(|held| held.saturating_sub(before));
        if risen.is_some_and(|risen| risen >= units) || tokio::time::Instant::now() >= until {
            return risen;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// The calls that take money from one account to another on the vault's own
/// chain. Anything else is refused here, and the station would refuse it too.
fn route(
    home: &str,
    drivers: &[String],
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
        if drivers.contains(&account.driver) {
            Ok(())
        } else {
            Err(format!(
                "this agent trades through {drivers:?}, not {}",
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
        Ok(access.get().home_chain.clone())
    }

    async fn move_(
        accessor: &Accessor<Ctx, Self>,
        source: Account,
        destination: Account,
        amount: u64,
    ) -> HostResult<Moved> {
        let (place, home, drivers) = accessor.with(|mut access| {
            let ctx = access.get();
            (
                ctx.place.clone(),
                ctx.home_chain.clone(),
                ctx.drivers.clone(),
            )
        });
        let steps = match route(&home, &drivers, &source, &destination) {
            Ok(steps) => steps,
            Err(why) => return Ok(Moved::Refused(why)),
        };
        // The venue account the move reaches or leaves, if any: the driver
        // the station is asked to deposit or withdraw through.
        let driver = match (&source, &destination) {
            (Account::Venue(account), _) | (_, Account::Venue(account)) => account.driver.clone(),
            _ => drivers.first().cloned().unwrap_or_default(),
        };
        let name = chain::names::name(driver.as_bytes());
        let units = U256::from(amount);
        // A move the station is asked to make is a submission like an order,
        // and counted once however many calls it takes. Each step is sent
        // only once the one before it has landed, so a move that stops is
        // known to have got as far as the last step that landed.
        let mut reached = None;
        for step in steps {
            // Set when a deposit left the station and the venue hasn't
            // credited it, or a withdrawal was asked of the venue and hasn't
            // all reached the station: the money is not where it was going,
            // so the agent hears it may be on its way.
            let mut in_flight = false;
            let (failed, may_still_land) = match &place {
                Place::Evm(evm) => {
                    let (key, station) = (&evm.key, evm.station);
                    let sent = match step {
                        Step::Pull => {
                            key.send(station, IStation::pullCall { amount: units })
                                .await
                        }
                        Step::Push => {
                            key.send(station, IStation::pushCall { amount: units })
                                .await
                        }
                        Step::Deposit => {
                            // A deposit into HyperCore lands on HyperEVM
                            // whether or not HyperCore credits it (on
                            // testnet it silently doesn't for an address
                            // without mainnet history), so it is done only
                            // once HyperCore's ledger shows it.
                            let half = evm
                                .hyperliquid
                                .as_ref()
                                .filter(|_| driver == crate::hyperliquid::DRIVER);
                            let watch = match half {
                                Some(half) => match half.deposit_watch(amount).await {
                                    Ok(watch) => Some((half, watch)),
                                    Err(error) => {
                                        let error = error.context(
                                            "HyperCore's ledger can't be read, so the deposit couldn't be confirmed; nothing was sent",
                                        );
                                        let tally = Tally::of_lookup_failure(&error);
                                        accessor.with(|mut access| {
                                            access.get().count(|c| {
                                                c.submitted(tally.unwrap_or(Tally::Unanswered))
                                            })
                                        });
                                        return Ok(stopped(reached, format!("{error:#}"), false));
                                    }
                                },
                                None => None,
                            };
                            let sent = key
                                .send(
                                    station,
                                    IStation::depositCall {
                                        driver: name,
                                        amount: units,
                                    },
                                )
                                .await;
                            match (sent, watch) {
                                (Ok(receipt), Some((half, watch))) => {
                                    if half.deposit_credited(&watch).await {
                                        Ok(receipt)
                                    } else {
                                        in_flight = true;
                                        tracing::warn!(
                                            station = %station,
                                            amount,
                                            "a deposit left the station for HyperCore and HyperCore hasn't credited it"
                                        );
                                        Err(anyhow::Error::new(chain::NeverLanded).context(format!(
                                            "the deposit of {amount} millionths left the station for HyperCore, which hasn't credited it within {} s: it may still arrive, or be held by the deposit contract",
                                            crate::hyperliquid::DEPOSIT_CONFIRM_WAIT.as_secs()
                                        )))
                                    }
                                }
                                (sent, _) => sent,
                            }
                        }
                        Step::Withdraw => {
                            // A delayed venue pays a withdrawal out after the
                            // call that asks for it, and may pay less or
                            // nothing; so it is done only once the station's
                            // own balance shows it.
                            let before = station_holds(&evm.chain, evm.usd, station).await;
                            let sent = key
                                .send(
                                    station,
                                    IStation::withdrawCall {
                                        driver: name,
                                        amount: units,
                                    },
                                )
                                .await;
                            match sent {
                                Ok(receipt) => {
                                    // HyperCore pays out a block or so later;
                                    // any other venue's payout is read again
                                    // for a few seconds, in case the first
                                    // read comes from an endpoint a block
                                    // behind the receipt.
                                    let wait = if driver == crate::hyperliquid::DRIVER {
                                        crate::hyperliquid::DEPOSIT_CONFIRM_WAIT
                                    } else {
                                        WITHDRAWAL_SEEN_WAIT
                                    };
                                    let risen =
                                        risen_by(&evm.chain, evm.usd, station, before, units, wait)
                                            .await;
                                    if risen.is_some_and(|risen| risen >= units) {
                                        Ok(receipt)
                                    } else {
                                        in_flight = true;
                                        let found = match risen {
                                            Some(risen) => format!("the station's balance rose by {risen} millionths"),
                                            None => "the station's balance can't be read, so what reached it can't be told".to_string(),
                                        };
                                        tracing::warn!(
                                            station = %station,
                                            amount,
                                            "a withdrawal was asked of the venue and hasn't all reached the station"
                                        );
                                        Err(anyhow::Error::new(chain::NeverLanded).context(format!(
                                            "the withdrawal of {amount} millionths was asked of the venue, and {found} within {} s: the rest may still arrive, or still be at the venue",
                                            wait.as_secs()
                                        )))
                                    }
                                }
                                Err(error) => Err(error),
                            }
                        }
                    };
                    match sent {
                        Ok(_) => (None, false),
                        Err(error) => {
                            let may = chain::may_still_land(&error);
                            let tally = Tally::of_failure(&error);
                            // A refusal the station's own rules explain is
                            // told with them, so the agent waits rather than
                            // asking again into the same refusal.
                            let held = if may || in_flight {
                                None
                            } else {
                                let venue = matches!(step, Step::Deposit | Step::Withdraw);
                                held_back(&evm.chain, station, name, venue).await
                            };
                            let error = match held {
                                Some(why) => error.context(why),
                                None => error,
                            };
                            (Some((tally, error)), may)
                        }
                    }
                }
                Place::Solana(trading) => {
                    match trading.carry(std::slice::from_ref(&step), amount).await {
                        Ok(()) => (None, false),
                        Err(error) => {
                            let tally = crate::solana::tally_of(&error);
                            // The cluster can't say whether an unanswered send
                            // was ever broadcast, so it may still land.
                            let may = matches!(tally, Tally::Unanswered);
                            (Some((tally, error)), may)
                        }
                    }
                }
            };
            if let Some((tally, error)) = failed {
                accessor.with(|mut access| access.get().count(|c| c.submitted(tally)));
                if in_flight {
                    return Ok(match step {
                        Step::Withdraw => Moved::Unknown(format!("{error:#}")),
                        _ => left_for_the_venue(format!("{error:#}")),
                    });
                }
                return Ok(stopped(reached, format!("{error:#}"), may_still_land));
            }
            reached = Some(step.lands_in(&home, &driver));
        }
        accessor.with(|mut access| access.get().count(|c| c.submitted(Tally::Done)));
        Ok(Moved::Done)
    }

    async fn bridge(
        accessor: &Accessor<Ctx, Self>,
        name: String,
        source_chain: String,
        destination_chain: String,
        amount: u64,
    ) -> HostResult<Moved> {
        let (place, home) = accessor.with(|mut access| {
            let ctx = access.get();
            (ctx.place.clone(), ctx.home_chain.clone())
        });
        if source_chain != home {
            return Ok(Moved::Refused(format!(
                "this executor serves the vault's station on {home} only, not on {source_chain}"
            )));
        }
        let Place::Evm(Evm {
            chain,
            key,
            station,
            factory,
            ..
        }) = place
        else {
            // No bridge is offered from a Solana vault yet: the agent's
            // mistake, refused here and counted as nothing.
            return Ok(Moved::Refused(format!("no bridge is offered from {home}")));
        };
        let to_chain = match chain_id_of(&chain, factory, &destination_chain).await {
            Ok(id) => id,
            Err(error) => {
                // A chain that never answered the lookup is a venue that
                // never answered; a name nobody knows is the agent's mistake
                // and counts as nothing.
                if let Some(tally) = Tally::of_lookup_failure(&error) {
                    accessor.with(|mut access| access.get().count(|c| c.submitted(tally)));
                }
                return Ok(Moved::Refused(format!("{error:#}")));
            }
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
        // A bridge the station is asked to make is a submission like a move,
        // and its refusal or a venue that never answered is counted the same.
        let tally = match &sent {
            Ok(_) => Tally::Done,
            Err(error) => Tally::of_failure(error),
        };
        accessor.with(|mut access| access.get().count(|c| c.submitted(tally)));
        Ok(match sent {
            Ok(_) => Moved::Done,
            Err(error) => stopped(None, format!("{error:#}"), chain::may_still_land(&error)),
        })
    }

    async fn balances(accessor: &Accessor<Ctx, Self>) -> HostResult<Vec<Balance>> {
        let (place, home, drivers) = accessor.with(|mut access| {
            let ctx = access.get();
            (
                ctx.place.clone(),
                ctx.home_chain.clone(),
                ctx.drivers.clone(),
            )
        });
        let driver = drivers.first().cloned().unwrap_or_default();
        let Evm {
            chain,
            vault,
            station,
            venue,
            usd,
            ..
        } = match place {
            Place::Evm(evm) => evm,
            Place::Solana(trading) => {
                let (vault, station, venue) = trading.balances().await;
                let mut out = Vec::new();
                if let Some(amount) = vault {
                    out.push(Balance {
                        account: Account::Vault,
                        amount,
                    });
                }
                if let Some(amount) = station {
                    out.push(Balance {
                        account: Account::Station(home.clone()),
                        amount,
                    });
                }
                if let Some(amount) = venue {
                    out.push(Balance {
                        account: Account::Venue(VenueAccount {
                            chain: home,
                            driver,
                        }),
                        amount,
                    });
                }
                return Ok(out);
            }
        };
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
        for driver in drivers {
            // The stand-in venue reads its own account; every other driver
            // values the station's account through the driver it pinned.
            let value = if driver == LOCAL {
                chain
                    .read(venue, ILocalVenue::accountValueCall { master: station })
                    .await
            } else {
                pinned_value(&chain, station, &driver).await
            };
            if let Ok(value) = value {
                out.push(Balance {
                    account: Account::Venue(VenueAccount {
                        chain: home.clone(),
                        driver,
                    }),
                    amount: units(value),
                });
            }
        }
        Ok(out)
    }
}

alloy::sol! {
    interface IPinnedDriverValue {
        function pinnedDriver(bytes32 driver) external view returns (address);
        function valueOf(address station) external view returns (uint256 mark, uint256 exit);
    }
}

/// What a station holds through `driver`, at the mark, by its pinned driver.
async fn pinned_value(chain: &Client, station: Address, driver: &str) -> Result<U256> {
    let pinned = chain
        .read(
            station,
            IPinnedDriverValue::pinnedDriverCall {
                driver: chain::names::name(driver.as_bytes()),
            },
        )
        .await?;
    Ok(chain
        .read(pinned, IPinnedDriverValue::valueOfCall { station })
        .await?
        .mark)
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
        let events = access.get().vault_events.take();
        let heard = access.get().halting_heard.clone();
        match events {
            Some(events) => {
                let producer = ChannelProducer::new(events).noting(move |event| {
                    if matches!(event, VaultEvent::Halting(_)) {
                        heard.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                });
                Ok(StreamReader::new(&mut access, producer)?)
            }
            None => stream_of(&mut access, None),
        }
    }
}

/// Hand the strategy a stream fed by the watcher, or an empty one if it has
/// already taken it. Asking twice is a mistake in the strategy, not a reason
/// to stop it.
fn stream_of<T, S>(
    access: &mut Access<'_, Ctx, S>,
    events: Option<Reader<T>>,
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
    /// One source: the venue's own markets, read from the chain's RPC on
    /// every call. Nothing is cached or shared between strategies yet, and
    /// no quota holds a strategy's reads down: the shared data layer and its
    /// quotas are still to be built.
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
        let place = accessor.with(|mut access| access.get().place.clone());
        let markets = match &place {
            Place::Evm(evm) => read_markets(&evm.chain, evm.venue).await,
            Place::Solana(trading) => trading.markets().await,
        };
        match markets {
            Ok(markets) => Ok(Ok(strategy_sdk::to_payload(&markets))),
            Err(error) => Ok(Err(error.to_string())),
        }
    }

    /// One of Shonin's shared venue streams. Each subscription holds one of
    /// the vault's open streams until the agent lets go of it.
    async fn subscribe(
        mut access: Access<'_, Ctx, Self>,
        source: String,
        feed: Vec<u8>,
    ) -> HostResult<Result<StreamReader<shonin::agent::data::Stamped>, String>> {
        let net = access.get().net.clone();
        let slot = match net.usage.open(&net.quotas) {
            Ok(slot) => slot,
            Err(over) => return Ok(Err(over.to_string())),
        };
        let values = match crate::data::hub().subscribe(&source, &feed) {
            Ok(values) => values,
            Err(why) => return Ok(Err(why)),
        };
        let values = futures::StreamExt::map(values, move |value| {
            let _held = &slot;
            shonin::agent::data::Stamped {
                received_ms: value.received_ms,
                source_ms: value.source_ms,
                value: value.value,
            }
        });
        Ok(Ok(StreamReader::new(
            &mut access,
            crate::feed::FromStream(values),
        )?))
    }
}

// Sockets --------------------------------------------------------------------

impl shonin::agent::websocket::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn connect(
        accessor: &Accessor<Ctx, Self>,
        url: String,
        headers: Vec<(String, String)>,
    ) -> HostResult<Result<Resource<crate::net::Connection>, String>> {
        let net = accessor.with(|mut access| access.get().net.clone());
        match crate::net::Connection::open(&net, &url, &headers).await {
            Ok(connection) => {
                let held = accessor.with(|mut access| access.get().table.push(connection))?;
                Ok(Ok(held))
            }
            Err(why) => Ok(Err(why)),
        }
    }
}

impl shonin::agent::websocket::HostConnectionWithStore<Ctx> for HasSelf<Ctx> {
    /// Letting go of a connection closes it.
    async fn drop(
        accessor: &Accessor<Ctx, Self>,
        rep: Resource<crate::net::Connection>,
    ) -> HostResult<()> {
        accessor.with(|mut access| access.get().table.delete(rep))?;
        Ok(())
    }

    async fn send(
        accessor: &Accessor<Ctx, Self>,
        connection: Resource<crate::net::Connection>,
        message: shonin::agent::websocket::Message,
    ) -> HostResult<Result<(), String>> {
        let sender = accessor.with(|mut access| {
            access
                .get()
                .table
                .get(&connection)
                .map(|connection| connection.sender())
        })?;
        Ok(sender.send(message).await)
    }

    async fn messages(
        mut access: Access<'_, Ctx, Self>,
        connection: Resource<crate::net::Connection>,
    ) -> HostResult<StreamReader<shonin::agent::websocket::Message>> {
        let incoming = access.get().table.get_mut(&connection)?.messages();
        match incoming {
            Some(incoming) => StreamReader::new(&mut access, crate::feed::FromStream(incoming)),
            // Asked for twice: the second is a stream that has ended.
            None => StreamReader::new(&mut access, Vec::<shonin::agent::websocket::Message>::new()),
        }
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

/// The bucket a handle names, or the agent's mistake.
fn bucket_of(ctx: &Ctx, bucket: &Resource<crate::state::Bucket>) -> Result<String, StoreError> {
    ctx.table
        .get(bucket)
        .map(|bucket| bucket.name.clone())
        .map_err(|error| StoreError::Other(error.to_string()))
}

impl wasi::keyvalue::store::HostWithStore<Ctx> for HasSelf<Ctx> {
    /// Any name opens a bucket of the agent's own: buckets are only names,
    /// and every one of them shares the agent's one allowance.
    ///
    /// A handle keeps its name on the host while it is open, so the names
    /// of the handles open at once are held to the memory allowance.
    async fn open(
        mut access: Access<'_, Ctx, Self>,
        identifier: String,
    ) -> HostResult<Result<Resource<crate::state::Bucket>, StoreError>> {
        let ctx = access.get();
        let named = identifier.len();
        if let Err(too_much) = ctx.state.open(named, ctx.memory.bytes) {
            return Ok(Err(too_much.into()));
        }
        match ctx.table.push(crate::state::Bucket { name: identifier }) {
            Ok(bucket) => Ok(Ok(bucket)),
            Err(error) => {
                // Never opened, so its name is not held.
                ctx.state.close(named);
                Err(error.into())
            }
        }
    }
}

impl wasi::keyvalue::store::HostBucketWithStore<Ctx> for HasSelf<Ctx> {
    async fn get(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        key: String,
    ) -> HostResult<Result<Option<Vec<u8>>, StoreError>> {
        let ctx = access.get();
        let mut room = ctx.memory.answer_room();
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            let Some(value) = ctx.state.get(&name, &key) else {
                return Ok(None);
            };
            room.take(Room::bytes(value))?;
            Ok(Some(value.clone()))
        }))
    }

    async fn set(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        key: String,
        value: Vec<u8>,
    ) -> HostResult<Result<(), StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            ctx.state
                .set(&name, key, value)
                .map_err(|refused| StoreError::Other(refused.to_string()))
        }))
    }

    async fn delete(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        key: String,
    ) -> HostResult<Result<(), StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).map(|name| ctx.state.delete(&name, &key)))
    }

    async fn exists(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        key: String,
    ) -> HostResult<Result<bool, StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).map(|name| ctx.state.get(&name, &key).is_some()))
    }

    async fn list_keys(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        cursor: Option<u64>,
    ) -> HostResult<Result<KeyResponse, StoreError>> {
        let ctx = access.get();
        let most = ctx.memory.bytes;
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            let (keys, cursor) = ctx.state.keys(&name, cursor, most)?;
            Ok(KeyResponse { keys, cursor })
        }))
    }

    async fn drop(
        accessor: &Accessor<Ctx, Self>,
        rep: Resource<crate::state::Bucket>,
    ) -> HostResult<()> {
        accessor.with(|mut access| {
            let ctx = access.get();
            let bucket = ctx.table.delete(rep)?;
            ctx.state.close(bucket.name.len());
            HostResult::Ok(())
        })
    }
}

impl wasi::keyvalue::atomics::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn increment(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        key: String,
        delta: u64,
    ) -> HostResult<Result<u64, StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            ctx.state
                .increment(&name, &key, delta)
                .map_err(StoreError::Other)
        }))
    }
}

impl wasi::keyvalue::batch::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn get_many(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        keys: Vec<String>,
    ) -> HostResult<Result<Vec<Option<(String, Vec<u8>)>>, StoreError>> {
        let ctx = access.get();
        let mut room = ctx.memory.answer_room();
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            // The whole answer is weighed before any of it is copied: every
            // key named is a value handed back, however often it is named.
            for key in &keys {
                room.take(std::mem::size_of::<Option<(String, Vec<u8>)>>())?;
                if let Some(value) = ctx.state.get(&name, key) {
                    room.take(Room::string(key).saturating_add(Room::bytes(value)))?;
                }
            }
            Ok(keys
                .into_iter()
                .map(|key| {
                    let value = ctx.state.get(&name, &key).cloned();
                    value.map(|value| (key, value))
                })
                .collect())
        }))
    }

    /// Each pair is set in turn; one that would pass the allowance stops
    /// the batch there, those before it set.
    async fn set_many(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        entries: Vec<(String, Vec<u8>)>,
    ) -> HostResult<Result<(), StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).and_then(|name| {
            entries.into_iter().try_for_each(|(key, value)| {
                ctx.state
                    .set(&name, key, value)
                    .map_err(|refused| StoreError::Other(refused.to_string()))
            })
        }))
    }

    async fn delete_many(
        mut access: Access<'_, Ctx, Self>,
        bucket: Resource<crate::state::Bucket>,
        keys: Vec<String>,
    ) -> HostResult<Result<(), StoreError>> {
        let ctx = access.get();
        Ok(bucket_of(ctx, &bucket).map(|name| {
            for key in keys {
                ctx.state.delete(&name, &key);
            }
        }))
    }
}

impl shonin::agent::inference::HostWithStore<Ctx> for HasSelf<Ctx> {
    async fn infer(
        accessor: &Accessor<Ctx, Self>,
        model: String,
        inputs: Vec<shonin::agent::inference::NamedTensor>,
    ) -> HostResult<
        Result<
            Vec<shonin::agent::inference::NamedTensor>,
            shonin::agent::inference::InferenceError,
        >,
    > {
        use crate::inference::{Data, Named, Refusal};
        use shonin::agent::inference::{InferenceError, NamedTensor, Tensor, TensorData};
        let (inference, most) = accessor.with(|mut access| {
            let ctx = access.get();
            (ctx.inference.clone(), ctx.memory.bytes)
        });
        // A bundle without models: every name is unknown, an error the
        // strategy handles rather than a stall.
        let Some(inference) = inference else {
            return Ok(Err(InferenceError::UnknownModel(model)));
        };
        let inputs = inputs
            .into_iter()
            .map(|named| Named {
                name: named.name,
                shape: named.tensor.shape,
                data: match named.tensor.data {
                    TensorData::Float32(v) => Data::F32(v),
                    TensorData::Float64(v) => Data::F64(v),
                    TensorData::Int32(v) => Data::I32(v),
                    TensorData::Int64(v) => Data::I64(v),
                    TensorData::Uint8(v) => Data::U8(v),
                },
            })
            .collect();
        Ok(match inference.infer(&model, inputs, most).await {
            Ok(outputs) => Ok(outputs
                .into_iter()
                .map(|named| NamedTensor {
                    name: named.name,
                    tensor: Tensor {
                        shape: named.shape,
                        data: match named.data {
                            Data::F32(v) => TensorData::Float32(v),
                            Data::F64(v) => TensorData::Float64(v),
                            Data::I32(v) => TensorData::Int32(v),
                            Data::I64(v) => TensorData::Int64(v),
                            Data::U8(v) => TensorData::Uint8(v),
                        },
                    },
                })
                .collect()),
            Err(Refusal::UnknownModel(why)) => Err(InferenceError::UnknownModel(why)),
            Err(Refusal::InvalidInput(why)) => Err(InferenceError::InvalidInput(why)),
            Err(Refusal::QuotaExceeded(why)) => Err(InferenceError::QuotaExceeded(why)),
            Err(Refusal::Backend(why)) => Err(InferenceError::BackendError(why)),
        })
    }
}

// Doing what a strategy asked ------------------------------------------------

/// How an action counts in the health report.
#[derive(Clone, Copy)]
pub enum Tally {
    Done,
    /// The venue looked at it and said no.
    Refused,
    /// The venue never answered.
    Unanswered,
}

impl Tally {
    /// How a send that failed counts: a refusal if the chain looked at it and
    /// said no, otherwise a venue that never answered.
    pub fn of_failure(error: &anyhow::Error) -> Self {
        if chain::unanswered(error) {
            Tally::Unanswered
        } else {
            Tally::Refused
        }
    }

    /// How a lookup the host makes before sending counts when it fails: as a
    /// venue that never answered if the chain didn't, and not at all if it
    /// answered and the agent asked for something that isn't there.
    fn of_lookup_failure(error: &anyhow::Error) -> Option<Self> {
        chain::unanswered(error).then_some(Tally::Unanswered)
    }
}

/// What the agent is told of an action, and how it counts.
pub fn answer(done: std::result::Result<Receipt, Refusal>) -> (Verdict, Tally) {
    match done {
        Ok(receipt) => (
            Verdict::Accepted(strategy_sdk::to_payload(&receipt)),
            Tally::Done,
        ),
        Err(Refusal::NoFill) => (Verdict::Rejected(NO_FILL.to_string()), Tally::Done),
        // A refusal is the venue's answer, not a fault: the strategy is told
        // why and decides what to do about it.
        Err(Refusal::Refused(why)) => (Verdict::Rejected(why), Tally::Refused),
        // Sent, and nobody said what became of it: it may still land.
        Err(Refusal::Unanswered(why)) => (Verdict::Unknown(why), Tally::Unanswered),
        // The venue was out of reach before anything was sent: nothing
        // happened, and the venue counts as not having answered.
        Err(Refusal::NotSent(why)) => (Verdict::Rejected(why), Tally::Unanswered),
    }
}

/// What the venue says when an order found no fill.
pub const NO_FILL: &str = "no fill at that price";

/// Why an action produced no receipt.
#[derive(Clone, Debug)]
pub enum Refusal {
    /// The venue would have taken the order; nobody was there at that price.
    NoFill,
    /// The venue would not take it, and said why.
    Refused(String),
    /// It was sent, or may have been, and no answer came: it may still land.
    Unanswered(String),
    /// The venue could not be reached, and nothing was sent.
    NotSent(String),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        if chain::may_still_land(&error) {
            Refusal::Unanswered(format!("{error}"))
        } else if chain::unanswered(&error) {
            Refusal::NotSent(format!("{error}"))
        } else {
            Refusal::Refused(format!("{error}"))
        }
    }
}

/// The order as the venue takes it, or why it can't be put that way.
fn request_of(order: &strategy_sdk::Order) -> std::result::Result<OrderRequest, Refusal> {
    Ok(OrderRequest {
        market: market_id(&order.market),
        isBuy: matches!(order.side, Side::Buy),
        size: to_units(order.size, SIZE_DECIMALS)?,
        limitPrice: to_units(order.limit_price, ASSET_DECIMALS)?,
        reduceOnly: order.reduce_only,
    })
}

/// Place every order of one submission in one transaction, and say what
/// became of each, in order. An order that can't be put as the venue takes
/// it is refused on its own, and the rest still go.
///
/// The venue decides each order in a batch on its own: one it refuses or
/// that finds no fill leaves the others standing, and it writes one log for
/// each, in order. Only a batch the chain refuses as a whole (one order
/// overflowing the venue's arithmetic, or a key the venue doesn't take)
/// would tie the orders together, so such a batch is sent again an order at
/// a time and each is decided on its own, as it would have been sent alone.
/// A batch that was sent and never answered is not sent again: it may still
/// land.
async fn place_all(
    key: &KeyClient,
    station: Address,
    venue: Address,
    orders: &[strategy_sdk::Order],
) -> Vec<std::result::Result<Receipt, Refusal>> {
    let mut done: Vec<Option<std::result::Result<Receipt, Refusal>>> =
        orders.iter().map(|_| None).collect();
    let mut going = Vec::new();
    let mut requests = Vec::new();
    for (at, order) in orders.iter().enumerate() {
        match request_of(order) {
            Ok(request) => {
                going.push(at);
                requests.push(request);
            }
            Err(refusal) => done[at] = Some(Err(refusal)),
        }
    }
    if !requests.is_empty() {
        let sent: Vec<&strategy_sdk::Order> = going.iter().map(|at| &orders[*at]).collect();
        let outcomes = match send_orders(key, station, venue, requests.clone(), &sent).await {
            Err(Refusal::Refused(_)) if requests.len() > 1 => {
                let mut each = Vec::with_capacity(requests.len());
                for (request, order) in requests.into_iter().zip(&sent) {
                    each.push(
                        match send_orders(key, station, venue, vec![request], &[*order]).await {
                            Ok(mut one) => one.remove(0),
                            Err(refusal) => Err(refusal),
                        },
                    );
                }
                each
            }
            Ok(outcomes) => outcomes,
            Err(refusal) => sent.iter().map(|_| Err(refusal.clone())).collect(),
        };
        for (at, outcome) in going.into_iter().zip(outcomes) {
            done[at] = Some(outcome);
        }
    }
    done.into_iter()
        .map(|done| {
            done.unwrap_or_else(|| {
                Err(Refusal::Refused(
                    "the venue said nothing about that order".to_string(),
                ))
            })
        })
        .collect()
}

/// Send `requests` in one `place`, and read what became of each of
/// `orders` (the same orders, as the agent put them) from the receipt; or
/// why the transaction as a whole came to nothing.
async fn send_orders(
    key: &KeyClient,
    station: Address,
    venue: Address,
    requests: Vec<OrderRequest>,
    orders: &[&strategy_sdk::Order],
) -> std::result::Result<Vec<std::result::Result<Receipt, Refusal>>, Refusal> {
    let receipt = key
        .send(
            venue,
            ILocalVenue::placeCall {
                master: station,
                orders: requests,
            },
        )
        .await?;
    Ok(outcomes_of(receipt.inner.logs(), venue, station, orders))
}

/// What the venue did with each of `orders`, read from the logs of the one
/// transaction that placed them. What the venue did is in its own log, where
/// anyone reading the chain sees the same thing the strategy is told: one
/// `Filled`, `Unfilled` or `Rejected` for each order, in the order they were
/// placed. An order the venue wrote nothing for is said to be refused.
pub fn outcomes_of(
    logs: &[alloy::rpc::types::Log],
    venue: Address,
    station: Address,
    orders: &[&strategy_sdk::Order],
) -> Vec<std::result::Result<Receipt, Refusal>> {
    let mut outcomes = Vec::with_capacity(orders.len());
    let mut next = orders.iter();
    for log in logs.iter().filter(|log| log.address() == venue) {
        let said = if let Ok(filled) = log.log_decode::<ILocalVenue::Filled>() {
            let filled = filled.inner.data;
            (filled.master, filled.market, Ok(Some(filled)))
        } else if let Ok(unfilled) = log.log_decode::<ILocalVenue::Unfilled>() {
            let unfilled = unfilled.inner.data;
            (unfilled.master, unfilled.market, Ok(None))
        } else if let Ok(rejected) = log.log_decode::<ILocalVenue::Rejected>() {
            let rejected = rejected.inner.data;
            (rejected.master, rejected.market, Err(rejected.reason))
        } else {
            continue;
        };
        let (master, market, said) = said;
        if master != station {
            continue;
        }
        let Some(order) = next.next() else {
            break;
        };
        outcomes.push(if market != market_id(&order.market) {
            Err(Refusal::Refused(
                "the venue's answer for this order named another market".to_string(),
            ))
        } else {
            match said {
                Ok(Some(filled)) => filled_receipt(&filled),
                Ok(None) => Err(Refusal::NoFill),
                Err(why) => Err(Refusal::Refused(why)),
            }
        });
    }
    for _ in next {
        outcomes.push(Err(Refusal::Refused(
            "the venue said nothing about that order".to_string(),
        )));
    }
    outcomes
}

/// What the agent is told of an order the venue filled. The market is named
/// as the venue knows it, so a receipt is no longer than the venue's name
/// for it whatever the agent sent.
fn filled_receipt(filled: &ILocalVenue::Filled) -> std::result::Result<Receipt, Refusal> {
    Ok(Receipt::Filled {
        market: market_name(filled.market),
        side: if filled.isBuy { Side::Buy } else { Side::Sell },
        size: to_decimal(filled.size, SIZE_DECIMALS)?,
        price: to_decimal(filled.price, ASSET_DECIMALS)?,
        notional: to_decimal(filled.notional, ASSET_DECIMALS)?,
    })
}

/// The markets a `local` account read names, or none if it names none: an
/// empty query, or anything that isn't one, asks about every market.
fn asked_about(query: &[u8]) -> Option<Vec<String>> {
    strategy_sdk::from_payload::<strategy_sdk::Query>(query)
        .ok()
        .map(|query| query.markets)
        .filter(|markets| !markets.is_empty())
}

// Reading the chain ----------------------------------------------------------

pub async fn read_account_state(
    chain: &Client,
    vault: Address,
    station: Address,
    venue: Address,
    usd: Address,
    asked: Option<&[String]>,
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
        // Every market, or those asked about and those held.
        markets: read_some_markets(chain, venue, |name| {
            asked.is_none_or(|asked| asked.iter().any(|market| market == name))
                || positions.iter().any(|held| held.market == name)
        })
        .await?,
        positions,
    })
}

pub async fn read_markets(chain: &Client, venue: Address) -> Result<Vec<MarketState>> {
    read_some_markets(chain, venue, |_| true).await
}

/// The venue's markets `keep` keeps, by name, in the venue's order: only
/// those are read.
pub async fn read_some_markets(
    chain: &Client,
    venue: Address,
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<MarketState>> {
    let ids = chain.read(venue, ILocalVenue::marketsCall {}).await?;
    let mut markets = Vec::with_capacity(ids.len());
    for id in ids.into_iter().filter(|id| keep(&market_name(*id))) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A core module's instance in a store held to `mib` of allowance, as the
    /// runner holds a strategy's.
    fn instantiate(
        mib: usize,
        wat: &str,
    ) -> (wasmtime::Store<Allowance>, HostResult<wasmtime::Instance>) {
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, Allowance::new(mib * 1024 * 1024));
        store.limiter(|allowance| allowance);
        let module = wasmtime::Module::new(&engine, wat).unwrap();
        let instance = wasmtime::Instance::new(&mut store, &module, &[]);
        (store, instance)
    }

    #[test]
    fn a_table_declared_past_the_memory_allowance_is_refused_at_instantiation() {
        // 2^27 entries: a GiB of host memory against a 64 MiB allowance.
        let (_, refused) = instantiate(64, "(module (table 134217728 funcref))");
        assert!(refused.is_err(), "a table past the allowance was made");
        // A table that fits is made.
        let (_, made) = instantiate(64, "(module (table 1024 funcref))");
        made.unwrap();
    }

    #[test]
    fn a_table_cannot_grow_past_the_memory_allowance() {
        let wat = r#"(module
            (table $t 0 funcref)
            (func (export "grow") (param i32) (result i32)
                (table.grow $t (ref.null func) (local.get 0))))"#;
        let (mut store, instance) = instantiate(64, wat);
        let instance = instance.unwrap();
        let grow = instance
            .get_typed_func::<i32, i32>(&mut store, "grow")
            .unwrap();
        // 2^28 entries is two GiB at 8 bytes an entry: refused, which the
        // strategy sees as -1.
        assert_eq!(grow.call(&mut store, 1 << 28).unwrap(), -1);
        // What fits in the allowance is still given.
        assert_eq!(grow.call(&mut store, 1024).unwrap(), 0);
    }

    #[test]
    fn tables_and_linear_memory_share_one_allowance() {
        // 32 MiB of linear memory (512 pages) against a 64 MiB allowance
        // leaves room for 32 MiB of table, and no more.
        let wat = r#"(module
            (memory (export "memory") 512)
            (table $t 0 funcref)
            (func (export "grow") (param i32) (result i32)
                (table.grow $t (ref.null func) (local.get 0))))"#;
        let (mut store, instance) = instantiate(64, wat);
        let instance = instance.unwrap();
        let grow = instance
            .get_typed_func::<i32, i32>(&mut store, "grow")
            .unwrap();
        let fits = (32 * 1024 * 1024 / Allowance::TABLE_ENTRY_BYTES) as i32;
        assert_eq!(grow.call(&mut store, fits + 1).unwrap(), -1);
        assert_eq!(grow.call(&mut store, fits).unwrap(), 0);
        // Nothing is left for the memory to grow into.
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        assert!(memory.grow(&mut store, 1).is_err());
        assert_eq!(grow.call(&mut store, 1).unwrap(), -1);
    }

    /// A growth past a table's own maximum never happens, so it uses up none
    /// of the allowance: a memory growth well inside it is still given.
    #[test]
    fn a_table_growth_past_its_own_maximum_does_not_use_up_the_allowance() {
        let wat = r#"(module
            (memory (export "memory") 1)
            (table $t 0 1 funcref)
            (func (export "grow_table") (param i32) (result i32)
                (table.grow $t (ref.null func) (local.get 0)))
            (func (export "grow_memory") (param i32) (result i32)
                (memory.grow (local.get 0))))"#;
        let (mut store, instance) = instantiate(2, wat);
        let instance = instance.unwrap();
        let grow_table = instance
            .get_typed_func::<i32, i32>(&mut store, "grow_table")
            .unwrap();
        let grow_memory = instance
            .get_typed_func::<i32, i32>(&mut store, "grow_memory")
            .unwrap();
        // 1.92 MB of entries fits the allowance, but not the table.
        assert_eq!(grow_table.call(&mut store, 240_000).unwrap(), -1);
        // 64 KiB held; two more pages make 192 KiB of 2 MiB.
        assert_eq!(grow_memory.call(&mut store, 2).unwrap(), 1);
    }

    /// The same for a memory grown past its own maximum, however often.
    #[test]
    fn a_memory_growth_past_its_own_maximum_does_not_use_up_the_allowance() {
        let wat = r#"(module
            (memory $capped 1 2)
            (memory $free (export "memory") 1)
            (func (export "grow_capped") (param i32) (result i32)
                (memory.grow $capped (local.get 0)))
            (func (export "grow_free") (param i32) (result i32)
                (memory.grow $free (local.get 0))))"#;
        let (mut store, instance) = instantiate(4, wat);
        let instance = instance.unwrap();
        let grow_capped = instance
            .get_typed_func::<i32, i32>(&mut store, "grow_capped")
            .unwrap();
        let grow_free = instance
            .get_typed_func::<i32, i32>(&mut store, "grow_free")
            .unwrap();
        for _ in 0..12 {
            assert_eq!(grow_capped.call(&mut store, 4).unwrap(), -1);
        }
        // 128 KiB held; another MiB fits 4 MiB easily.
        assert_eq!(grow_free.call(&mut store, 16).unwrap(), 1);
        // A growth inside the capped memory's maximum is still given.
        assert_eq!(grow_capped.call(&mut store, 1).unwrap(), 1);
    }

    /// Each memory reserves address space whatever it holds, so a strategy
    /// may make only a few, and only a few tables and instances.
    #[test]
    fn a_strategy_may_make_only_a_few_memories_tables_and_instances() {
        let memories = |count: usize| format!("(module {})", "(memory 0)".repeat(count));
        instantiate(64, &memories(Allowance::MEMORIES)).1.unwrap();
        assert!(
            instantiate(64, &memories(Allowance::MEMORIES + 1))
                .1
                .is_err(),
            "more memories than allowed were made"
        );

        let tables = |count: usize| format!("(module {})", "(table 0 funcref)".repeat(count));
        instantiate(64, &tables(Allowance::TABLES)).1.unwrap();
        assert!(
            instantiate(64, &tables(Allowance::TABLES + 1)).1.is_err(),
            "more tables than allowed were made"
        );

        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, Allowance::new(64 * 1024 * 1024));
        store.limiter(|allowance| allowance);
        let module = wasmtime::Module::new(&engine, "(module)").unwrap();
        for _ in 0..Allowance::INSTANCES {
            wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        }
        assert!(
            wasmtime::Instance::new(&mut store, &module, &[]).is_err(),
            "more instances than allowed were made"
        );
    }

    #[test]
    fn a_send_that_may_still_land_is_unknown_and_one_never_sent_is_rejected() {
        // Broadcast and never seen again: it may still land.
        let lost = anyhow::Error::new(chain::NeverLanded).context("placing an order");
        let (verdict, tally) = answer(Err(Refusal::from(lost)));
        assert!(matches!(verdict, Verdict::Unknown(_)), "{verdict:?}");
        assert!(matches!(tally, Tally::Unanswered));
        // Refused by the chain: nothing happened, and the agent is told why.
        let (verdict, tally) = answer(Err(Refusal::from(anyhow!("execution reverted"))));
        assert!(matches!(verdict, Verdict::Rejected(why) if why.contains("reverted")));
        assert!(matches!(tally, Tally::Refused));
        // Out of reach before anything was sent: nothing happened.
        let (verdict, tally) = answer(Err(Refusal::NotSent("no node".to_string())));
        assert!(matches!(verdict, Verdict::Rejected(_)));
        assert!(matches!(tally, Tally::Unanswered));
        // No fill is the market's answer.
        let (verdict, tally) = answer(Err(Refusal::NoFill));
        assert!(matches!(verdict, Verdict::Rejected(why) if why == NO_FILL));
        assert!(matches!(tally, Tally::Done));
    }

    #[test]
    fn a_move_reaches_any_declared_drivers_account_and_no_other() {
        let drivers = vec!["robinhood".to_string(), "lighter".to_string()];
        let venue = |driver: &str| {
            Account::Venue(VenueAccount {
                chain: "robinhood".to_string(),
                driver: driver.to_string(),
            })
        };
        assert_eq!(
            route("robinhood", &drivers, &Account::Vault, &venue("lighter")).unwrap(),
            vec![Step::Pull, Step::Deposit]
        );
        assert_eq!(
            route("robinhood", &drivers, &venue("robinhood"), &Account::Vault).unwrap(),
            vec![Step::Withdraw, Step::Push]
        );
        assert!(
            route(
                "robinhood",
                &drivers,
                &Account::Vault,
                &venue("hyperliquid")
            )
            .is_err()
        );
    }

    #[test]
    fn a_move_that_stops_on_the_way_says_where_the_money_is() {
        let (home, driver) = ("base", "local");
        // From the vault to the venue: pulled to the station, then deposited.
        let steps = route(
            home,
            &[driver.to_string()],
            &Account::Vault,
            &Account::Venue(VenueAccount {
                chain: home.to_string(),
                driver: driver.to_string(),
            }),
        )
        .unwrap();
        assert_eq!(steps, vec![Step::Pull, Step::Deposit]);
        // The first step refused: nothing moved.
        assert!(matches!(
            stopped(None, "no".to_string(), false),
            Moved::Refused(_)
        ));
        // The first step unanswered: it may still land.
        assert!(matches!(
            stopped(None, "silence".to_string(), true),
            Moved::Unknown(_)
        ));
        // The pull landed and the deposit was refused: the money is in the
        // station, and stays there.
        let reached = Some(steps[0].lands_in(home, driver));
        match stopped(reached.clone(), "the venue is shut".to_string(), false) {
            Moved::Partial(partial) => {
                assert!(matches!(&partial.reached, Account::Station(chain) if chain == home));
                assert!(!partial.may_still_land);
                assert_eq!(partial.reason, "the venue is shut");
            }
            other => panic!("{other:?}"),
        }
        // The deposit unanswered: the money may yet reach the venue.
        assert!(matches!(
            stopped(reached, "silence".to_string(), true),
            Moved::Partial(PartialMove {
                may_still_land: true,
                ..
            })
        ));
        // Each step lands where it says.
        assert!(matches!(Step::Push.lands_in(home, driver), Account::Vault));
        assert!(matches!(
            Step::Withdraw.lands_in(home, driver),
            Account::Station(_)
        ));
        assert!(matches!(
            Step::Deposit.lands_in(home, driver),
            Account::Venue(VenueAccount { chain, driver: d }) if chain == home && d == driver
        ));
    }

    #[test]
    fn a_deposit_the_venue_has_not_credited_is_never_said_to_be_at_the_station() {
        // Whether it was the move's first step or came after a pull from the
        // vault, the money left the station: it may still arrive.
        let moved = left_for_the_venue("not credited within 60 s".to_string());
        assert!(
            matches!(&moved, Moved::Unknown(why) if why.contains("left the station")),
            "{moved:?}"
        );
    }

    #[test]
    fn a_bridge_whose_chain_lookup_goes_unanswered_counts_as_a_venue_error() {
        let unanswered = anyhow::Error::new(chain::NeverLanded).context("reading the chains");
        let mut counters = Counters::default();
        if let Some(tally) = Tally::of_lookup_failure(&unanswered) {
            counters.submitted(tally);
        }
        assert_eq!(counters.venue_errors, 1);
        assert_eq!(counters.submissions, 1);
        // A chain nobody here knows is the agent's mistake, not the venue's.
        let unknown = anyhow!("no chain called nowhere here");
        assert!(Tally::of_lookup_failure(&unknown).is_none());
    }

    #[test]
    fn the_rejection_rate_counts_refused_transfers_as_submissions() {
        // One order the venue took, and three transfers the station refused.
        let mut counters = Counters::default();
        counters.submitted(Tally::Done);
        for _ in 0..3 {
            counters.submitted(Tally::Refused);
        }
        assert_eq!(counters.submissions, 4);
        assert_eq!(counters.rejection_rate(), 75.0);

        // Whatever is refused, the share never passes the whole.
        let mut counters = Counters::default();
        for _ in 0..5 {
            counters.submitted(Tally::Refused);
            counters.submitted(Tally::Unanswered);
            assert!(counters.rejection_rate() <= 100.0);
        }
    }

    fn order(market: &str, side: Side, limit: &str) -> strategy_sdk::Order {
        strategy_sdk::Order {
            market: market.to_string(),
            side,
            size: strategy_sdk::decimal("2"),
            limit_price: strategy_sdk::decimal(limit),
            reduce_only: false,
        }
    }

    /// Orders placed at the stand-in venue, and the node they went to.
    async fn placed(
        orders: &[strategy_sdk::Order],
    ) -> (
        crate::stand_in::venue::Shared,
        Vec<std::result::Result<Receipt, Refusal>>,
    ) {
        let node = crate::stand_in::venue::Shared::default();
        let client = crate::stand_in::venue::start(node.clone()).await;
        let key = crate::stand_in::key(&client);
        let done = place_all(
            &key,
            Address::repeat_byte(0x55),
            Address::repeat_byte(0x33),
            orders,
        )
        .await;
        (node, done)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn orders_in_one_submit_go_out_in_one_transaction() {
        let orders = [
            order("BTC", Side::Buy, "200"),
            order("BTC-PERP", Side::Sell, "50"),
            order("ETH", Side::Buy, "150"),
        ];
        let (node, done) = placed(&orders).await;
        let node = node.lock().unwrap();
        assert_eq!(node.placed.len(), 1, "one transaction for the three orders");
        let markets: Vec<String> = node.placed[0]
            .iter()
            .map(|request| market_name(request.market))
            .collect();
        assert_eq!(markets, ["BTC", "BTC-PERP", "ETH"], "in the order given");
        assert!(done.iter().all(Result::is_ok));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_order_in_a_batch_gets_its_own_verdict() {
        let orders = [
            order("BTC", Side::Buy, "200"),
            order("NOPE", Side::Buy, "200"),
            order("BTC", Side::Buy, "50"),
            order("BTC-PERP", Side::Sell, "90"),
        ];
        let (node, done) = placed(&orders).await;
        assert_eq!(node.lock().unwrap().placed.len(), 1);
        let answers: Vec<(Verdict, Tally)> = done.into_iter().map(answer).collect();
        match &answers[0] {
            (Verdict::Accepted(receipt), Tally::Done) => assert_eq!(
                strategy_sdk::from_payload::<Receipt>(receipt).unwrap(),
                Receipt::Filled {
                    market: "BTC".to_string(),
                    side: Side::Buy,
                    size: strategy_sdk::decimal("2"),
                    price: strategy_sdk::decimal("100"),
                    notional: strategy_sdk::decimal("200"),
                }
            ),
            _ => panic!("the first order's fill was not said"),
        }
        assert!(
            matches!(&answers[1], (Verdict::Rejected(why), Tally::Refused) if why == "no such market"),
            "the refused order was not told why"
        );
        assert!(
            matches!(&answers[2], (Verdict::Rejected(why), Tally::Done) if why == NO_FILL),
            "the order that found no fill was not said to"
        );
        match &answers[3] {
            (Verdict::Accepted(receipt), Tally::Done) => assert!(matches!(
                strategy_sdk::from_payload::<Receipt>(receipt).unwrap(),
                Receipt::Filled { market, side: Side::Sell, .. } if market == "BTC-PERP"
            )),
            _ => panic!("the order after a refused one lost its fill"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_filled_order_s_verdict_is_held_to_what_a_submission_counts_it_as() {
        // The venue knows a market by its first 32 bytes; an agent can send
        // a name of any length that comes to the same market.
        let long = format!("BTC{}", "\0".repeat(1 << 20));
        let (_node, done) = placed(&[order(&long, Side::Buy, "200")]).await;
        let (verdict, tally) = done.into_iter().map(answer).next().unwrap();
        let verdict = within(verdict);
        let held = std::mem::size_of::<Verdict>()
            + match &verdict {
                Verdict::Accepted(receipt) => receipt.len(),
                Verdict::Rejected(why) | Verdict::Unknown(why) => why.len(),
            };
        assert!(
            held <= VERDICT_MOST,
            "one verdict holds {held} bytes, counted as {VERDICT_MOST}"
        );
        assert!(matches!(tally, Tally::Done));
        match verdict {
            Verdict::Accepted(receipt) => assert!(matches!(
                strategy_sdk::from_payload::<Receipt>(&receipt).unwrap(),
                Receipt::Filled { market, .. } if market == "BTC"
            )),
            other => panic!("the fill was not said: {other:?}"),
        }
    }

    #[test]
    fn a_receipt_longer_than_a_verdict_is_counted_as_is_not_handed_back() {
        let verdict = within(Verdict::Accepted(vec![0; REASON_MOST + 1]));
        assert!(
            matches!(&verdict, Verdict::Unknown(why) if why.len() <= REASON_MOST),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_submission_whose_verdicts_could_outgrow_the_allowance_is_refused_action_by_action() {
        let fits = 10_000 / VERDICT_MOST;
        assert!(refused_whole(fits, Room::new(10_000)).unwrap().is_none());

        let refused = refused_whole(fits + 1, Room::new(10_000)).unwrap().unwrap();
        assert_eq!(refused.len(), fits + 1, "one verdict for each action");
        assert!(
            refused
                .iter()
                .all(|verdict| matches!(verdict, Verdict::Rejected(why) if why == TOO_MANY))
        );

        // Not even the refusals could be handed back: a fault in the agent.
        let one = std::mem::size_of::<Verdict>() + TOO_MANY.len();
        assert!(refused_whole(10_000 / one + 1, Room::new(10_000)).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_order_that_reverts_its_batch_leaves_the_others_decided_on_their_own() {
        let orders = [
            order("BTC", Side::Buy, "200"),
            order("SINK", Side::Buy, "200"),
            order("ETH", Side::Buy, "200"),
        ];
        let (node, done) = placed(&orders).await;
        // The batch was never sent; the two orders that go alone were.
        assert_eq!(node.lock().unwrap().placed.len(), 2);
        assert!(
            done[0].is_ok() && done[2].is_ok(),
            "the others lost their fills"
        );
        assert!(matches!(done[1], Err(Refusal::Refused(_))), "{:?}", done[1]);
    }

    #[test]
    fn an_order_the_venue_said_nothing_about_is_refused_and_the_others_keep_their_answers() {
        let station = Address::repeat_byte(0x55);
        let venue = Address::repeat_byte(0x33);
        let log = |data: alloy::primitives::LogData, address: Address| alloy::rpc::types::Log {
            inner: alloy::primitives::Log { address, data },
            ..Default::default()
        };
        use alloy::sol_types::SolEvent;
        let unfilled = |market: &str| {
            ILocalVenue::Unfilled {
                master: station,
                market: market_id(market),
                by: Address::ZERO,
            }
            .encode_log_data()
        };
        let logs = [
            // Another contract's log, and another account's, are not the
            // venue's answer to these orders.
            log(unfilled("BTC"), Address::repeat_byte(0x99)),
            log(
                ILocalVenue::Unfilled {
                    master: Address::repeat_byte(0x66),
                    market: market_id("BTC"),
                    by: Address::ZERO,
                }
                .encode_log_data(),
                venue,
            ),
            log(unfilled("BTC"), venue),
        ];
        let (btc, eth) = (order("BTC", Side::Buy, "1"), order("ETH", Side::Buy, "1"));
        let outcomes = outcomes_of(&logs, venue, station, &[&btc, &eth]);
        assert!(matches!(outcomes[0], Err(Refusal::NoFill)));
        assert!(matches!(&outcomes[1], Err(Refusal::Refused(why)) if why.contains("said nothing")));
    }

    #[test]
    fn a_local_account_read_naming_no_markets_asks_about_every_market() {
        assert_eq!(asked_about(&[]), None);
        assert_eq!(asked_about(b"not a query"), None);
        assert_eq!(
            asked_about(&strategy_sdk::to_payload(&strategy_sdk::Query::default())),
            None
        );
        let query = strategy_sdk::Query {
            markets: vec!["BTC".to_string(), "BTC-PERP".to_string()],
        };
        assert_eq!(
            asked_about(&strategy_sdk::to_payload(&query)),
            Some(query.markets)
        );
    }

    #[test]
    fn a_call_for_random_bytes_gets_them_up_to_a_mib_and_traps_past_it() {
        let bytes = random_bytes(RANDOM_AT_MOST).unwrap();
        assert_eq!(bytes.len() as u64, RANDOM_AT_MOST);
        assert!(bytes.iter().any(|byte| *byte != 0));
        assert!(random_bytes(0).unwrap().is_empty());
        assert!(random_bytes(RANDOM_AT_MOST + 1).is_err());
    }

    #[test]
    fn the_executor_s_randomness_is_linked_in_place_of_wasi_s() {
        let engine = crate::runner::engine().unwrap();
        let mut linker = wasmtime::component::Linker::new(&engine);
        link(&mut linker).unwrap();
        // Linking again over it is refused: shadowing is not left allowed.
        assert!(link(&mut linker).is_err());
    }
}
