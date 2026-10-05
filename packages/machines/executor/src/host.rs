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
use wasmtime::AsContextMut;
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
    /// Where the vault is, and how to act for it there.
    pub place: Place,
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
    /// first asks for the stream. They are the vault's, so whatever an agent
    /// before this one left unread is read here first.
    pub account_events: Option<Reader<Vec<u8>>>,
    pub vault_events: Option<Reader<VaultEvent>>,
    /// What the health report is made of. Shared, so a report can be sent
    /// while the strategy is still running: a strategy's `run` is meant never
    /// to return.
    pub counters: Arc<Mutex<Counters>>,
    /// Raised once the vault's halting notice has been handed to the
    /// strategy, so an agent that then ends is known to have been told.
    pub halting_heard: Arc<std::sync::atomic::AtomicBool>,
    /// How much fuel a second of running is worth, and when it was last
    /// topped up.
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
    /// How many times the component in the store was found not to be the
    /// vault's bundle, and was not run.
    pub not_the_bundle: u64,
}

impl Counters {
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
        let (place, expected) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (ctx.place.clone(), ctx.driver.clone())
        });

        if driver != expected {
            let refusal = format!("this agent trades through {expected}, not {driver}");
            return Ok(actions
                .iter()
                .map(|_| Verdict::Rejected(refusal.clone()))
                .collect());
        }

        let mut verdicts = Vec::with_capacity(actions.len());
        let mut tallies = Vec::with_capacity(actions.len());
        for bytes in &actions {
            let (verdict, tally) = match strategy_sdk::from_payload::<Action>(bytes) {
                Ok(action) => match &place {
                    Place::Evm(evm) => act(&evm.key, evm.station, evm.venue, action).await,
                    Place::Solana(trading) => answer(trading.act(action).await),
                },
                Err(error) => (
                    Verdict::Rejected(format!("that is not an action: {error}")),
                    Tally::Refused,
                ),
            };
            tallies.push(tally);
            verdicts.push(verdict);
        }
        accessor.with(|mut access| {
            refuel(&mut access);
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
        _query: Vec<u8>,
    ) -> HostResult<Result<Vec<u8>, String>> {
        let (place, expected) = accessor.with(|mut access| {
            let ctx = access.get();
            (ctx.place.clone(), ctx.driver.clone())
        });
        if driver != expected {
            return Ok(Err(format!(
                "this agent trades through {expected}, not {driver}"
            )));
        }
        let state = match &place {
            Place::Evm(evm) => {
                read_account_state(&evm.chain, evm.vault, evm.station, evm.venue, evm.usd).await
            }
            Place::Solana(trading) => trading.account_state().await,
        };
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
        refuel(&mut access);
        let events = access.get().account_events.take();
        stream_of(&mut access, events)
    }
}

// Transfers --------------------------------------------------------------------

/// One call a move is made of, sent to the station as the vault's key.
pub enum Step {
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
        let (place, home, driver) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (
                ctx.place.clone(),
                ctx.home_chain.clone(),
                ctx.driver.clone(),
            )
        });
        let steps = match route(&home, &driver, &source, &destination) {
            Ok(steps) => steps,
            Err(why) => return Ok(Err(why)),
        };
        let (key, station) = match place {
            Place::Evm(evm) => (evm.key, evm.station),
            Place::Solana(trading) => {
                // Counted once however many sends it takes, as below.
                let moved = trading.carry(&steps, amount).await;
                let tally = match &moved {
                    Ok(()) => Tally::Done,
                    Err(error) => crate::solana::tally_of(error),
                };
                accessor.with(|mut access| access.get().count(|c| c.submitted(tally)));
                return Ok(moved.map_err(|error| format!("{error:#}")));
            }
        };
        let amount = U256::from(amount);
        let name = chain::names::name(driver.as_bytes());
        // A move the station is asked to make is a submission like an order,
        // and counted once however many calls it takes.
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
                let tally = Tally::of_failure(&error);
                accessor.with(|mut access| access.get().count(|c| c.submitted(tally)));
                return Ok(Err(format!("{error:#}")));
            }
        }
        accessor.with(|mut access| access.get().count(|c| c.submitted(Tally::Done)));
        Ok(Ok(()))
    }

    async fn bridge(
        accessor: &Accessor<Ctx, Self>,
        name: String,
        source_chain: String,
        destination_chain: String,
        amount: u64,
    ) -> HostResult<Result<(), String>> {
        let (place, home) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (ctx.place.clone(), ctx.home_chain.clone())
        });
        if source_chain != home {
            return Ok(Err(format!(
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
            return Ok(Err(format!("no bridge is offered from {home}")));
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
                return Ok(Err(format!("{error:#}")));
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
        Ok(sent.map(|_| ()).map_err(|error| format!("{error:#}")))
    }

    async fn balances(accessor: &Accessor<Ctx, Self>) -> HostResult<Vec<Balance>> {
        let (place, home, driver) = accessor.with(|mut access| {
            refuel(&mut access);
            let ctx = access.get();
            (
                ctx.place.clone(),
                ctx.home_chain.clone(),
                ctx.driver.clone(),
            )
        });
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
        let place = accessor.with(|mut access| {
            refuel(&mut access);
            access.get().place.clone()
        });
        let markets = match &place {
            Place::Evm(evm) => read_markets(&evm.chain, evm.venue).await,
            Place::Solana(trading) => trading.markets().await,
        };
        match markets {
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

/// Do one action, and say whether the answer counts against the strategy's
/// health. An order that found nobody at its price is the market's answer, not
/// the venue's: a strategy whose orders are not crossing is trading, and one
/// whose orders are being refused is not.
async fn act(
    key: &KeyClient,
    station: Address,
    venue: Address,
    action: Action,
) -> (Verdict, Tally) {
    let done = match action {
        Action::Place(order) => place(key, station, venue, &order).await,
    };
    answer(done)
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
        Err(Refusal::Unanswered(why)) => (Verdict::Rejected(why), Tally::Unanswered),
    }
}

/// What the venue says when an order found no fill.
pub const NO_FILL: &str = "no fill at that price";

/// Why an action produced no receipt.
pub enum Refusal {
    /// The venue would have taken the order; nobody was there at that price.
    NoFill,
    /// The venue would not take it, and said why.
    Refused(String),
    /// The venue could not be reached, or never answered.
    Unanswered(String),
}

impl From<anyhow::Error> for Refusal {
    fn from(error: anyhow::Error) -> Self {
        if chain::unanswered(&error) {
            Refusal::Unanswered(format!("{error}"))
        } else {
            Refusal::Refused(format!("{error}"))
        }
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
}
