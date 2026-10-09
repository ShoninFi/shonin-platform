//! What an agent's requests come to on Solana: every one is sent by the
//! vault's key through the vault or its station, and the station's guard
//! checks each venue instruction against the driver's pinned allowlist
//! before it signs for it. The key holds nothing the guard would let it
//! take.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use alloy::primitives::{I256, U256};
use anyhow::{Context, Result, anyhow};
use chain::solana::executor::{self as on_chain, VenueOutcome};
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::local_venue;
use chain::solana::{Instruction, Keypair, Landed, Platform, Pubkey, Signer, SolanaClient, name};
use strategy_sdk::{AccountEvent, Action, MarketState, PositionState, Receipt, Side};
use tokio::sync::mpsc::UnboundedSender;

use crate::host::{Refusal, Step};
use crate::scale::{ASSET_DECIMALS, SIZE_DECIMALS, signed_to_decimal, to_decimal, to_units};
use crate::watch::VaultEvent;

/// The station's one driver on a vault the platform activates: the stand-in
/// venue's.
const DRIVER: u8 = 0;

/// One vault's trading, as its agent's host reaches it.
pub struct Trading {
    pub client: SolanaClient,
    pub platform: Platform,
    pub key: Arc<Keypair>,
    pub accounts: VaultAccounts,
    pub vault: Pubkey,
    pub bundle: [u8; 32],
    /// What the agent's own sends have cost the key, in lamports: fees, and
    /// rent put into accounts less rent taken back. Billed to the tank.
    pub spent: Arc<AtomicI64>,
    /// One send at a time, so the agent's sends are counted one by one.
    sending: tokio::sync::Mutex<()>,
    /// The agent's sends that may still land, and when the last settled.
    pub flights: Flights,
    /// The Orca driver's half, for a vault whose agent trades there: taken
    /// up once the manifest the builder recorded names it.
    pub(super) orca: std::sync::OnceLock<super::orca::Orca>,
}

/// The agent's sends whose cost may still be added: started, and not yet
/// landed or given up on. A send is watched in a task of its own, so an
/// agent stopped mid-send (its vault closed or taken over) leaves the send
/// watched, and its cost counted, until it lands or is given up on.
#[derive(Clone, Default)]
pub struct Flights(Arc<std::sync::Mutex<FlightLog>>);

#[derive(Default)]
struct FlightLog {
    in_flight: usize,
    last_settled: Option<std::time::Instant>,
}

/// One send in flight, from its start until it is dropped, however its
/// watch ends.
pub struct InFlight(Flights);

impl Flights {
    pub fn start(&self) -> InFlight {
        if let Ok(mut log) = self.0.lock() {
            log.in_flight += 1;
        }
        InFlight(self.clone())
    }

    /// Run `send` (a send, watched until it lands or is given up on, and
    /// its cost counted) in a task of its own, in flight until it ends: an
    /// agent stopped while it waits stops waiting, and the send carries on.
    pub fn watched<T: Send + 'static>(
        &self,
        send: impl std::future::Future<Output = T> + Send + 'static,
    ) -> tokio::task::JoinHandle<T> {
        let flight = self.start();
        tokio::spawn(async move {
            let ended = send.await;
            drop(flight);
            ended
        })
    }

    /// Whether any send may still land. A log that can't be read says yes.
    pub fn any(&self) -> bool {
        self.0.lock().map_or(true, |log| log.in_flight > 0)
    }

    /// When the last send landed or was given up on.
    pub fn last_settled(&self) -> Option<std::time::Instant> {
        self.0.lock().ok().and_then(|log| log.last_settled)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut log) = (self.0).0.lock() {
            log.in_flight = log.in_flight.saturating_sub(1);
            log.last_settled = Some(std::time::Instant::now());
        }
    }
}

impl Trading {
    pub fn new(
        client: SolanaClient,
        platform: Platform,
        key: Arc<Keypair>,
        accounts: VaultAccounts,
        bundle: [u8; 32],
        spent: Arc<AtomicI64>,
    ) -> Self {
        Self {
            client,
            platform,
            key,
            vault: accounts.vault,
            accounts,
            bundle,
            spent,
            sending: Default::default(),
            flights: Default::default(),
            orca: Default::default(),
        }
    }

    /// The station's authority at the stand-in venue: the master of its
    /// account there.
    async fn authority(&self) -> Result<Pubkey> {
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?;
        station
            .drivers
            .get(DRIVER as usize)
            .map(|driver| driver.authority)
            .ok_or_else(|| anyhow!("the vault's station declares no driver"))
    }

    /// Send for the agent, the key paying, and bill what it cost: a send
    /// that landed and failed was paid for too. The send is watched, and
    /// its cost counted, in a task of its own, which carries on if the agent
    /// is stopped while it waits. One the node stopped answering about is
    /// watched on in a task of its own, still in flight, and billed if it
    /// lands (see [`landed_late`]); the agent is answered meanwhile.
    pub(super) async fn send(&self, ixs: Vec<Instruction>) -> Result<Landed> {
        let _turn = self.sending.lock().await;
        let (client, key, spent) = (self.client.clone(), self.key.clone(), self.spent.clone());
        let flights = self.flights.clone();
        let watched = self.flights.watched(async move {
            let sent = client.send(ixs, &[&key]).await;
            let cost = match &sent {
                Ok(landed) => landed.payer_spent(),
                Err(error) => match to_bill(error) {
                    ToBill::Failed(signature) => client
                        .transaction(&signature)
                        .await
                        .ok()
                        .flatten()
                        .map_or(0, |record| record.spent_by(&key.pubkey())),
                    ToBill::IfItLands(signature) => {
                        let (client, key, spent) = (client.clone(), key.clone(), spent.clone());
                        drop(flights.watched(async move {
                            let cost = landed_late(&client, &signature, &key.pubkey()).await;
                            add_spent(&spent, cost);
                        }));
                        0
                    }
                    ToBill::Nothing => 0,
                },
            };
            add_spent(&spent, cost);
            sent
        });
        watched
            .await
            .map_err(|error| anyhow!("the send's watch ended: {error}"))?
    }

    /// A venue instruction sent through the station's guard, with `fund` of
    /// the station's counted dollars lent to it first.
    async fn through_the_guard(&self, venue_ix: Instruction, fund: u64) -> Result<Landed> {
        let ix = self
            .platform
            .ix_execute(
                &self.client,
                &self.accounts,
                &self.bundle,
                &self.key.pubkey(),
                DRIVER,
                venue_ix,
                fund,
            )
            .await?;
        self.send(vec![ix]).await
    }

    /// The station's account at the venue, opened through the guard if it
    /// has none yet: the key pays its rent.
    async fn opened(&self, authority: &Pubkey) -> Result<()> {
        if self
            .platform
            .margin_account(&self.client, authority)
            .await?
            .is_some()
        {
            return Ok(());
        }
        let open = self.platform.venue_open(&self.key.pubkey(), authority);
        self.through_the_guard(open, 0)
            .await
            .context("opening the station's account at the venue")?;
        Ok(())
    }

    /// Place one order at the venue through the guard.
    pub async fn act(&self, action: Action) -> std::result::Result<Receipt, Refusal> {
        let Action::Place(order) = action;
        let request = local_venue::OrderRequest {
            market: name(&order.market),
            is_buy: matches!(order.side, Side::Buy),
            size: units(to_units(order.size, SIZE_DECIMALS))?,
            limit_price: units(to_units(order.limit_price, ASSET_DECIMALS))?,
            reduce_only: order.reduce_only,
        };
        // The receipt names the market as the venue knows it, so it is no
        // longer than the venue's name for it whatever the agent sent.
        let market = market_name(&request.market);
        let authority = self.authority().await.map_err(refusal)?;
        self.opened(&authority).await.map_err(refusal)?;
        let place = self
            .platform
            .venue_place(&self.client, &authority, vec![request])
            .await
            .map_err(refusal)?;
        let landed = self.through_the_guard(place, 0).await.map_err(refusal)?;
        match on_chain::venue_outcome(&landed.record, &authority) {
            VenueOutcome::Filled(filled) => Ok(Receipt::Filled {
                market,
                side: if filled.is_buy { Side::Buy } else { Side::Sell },
                size: to_decimal(U256::from(filled.size), SIZE_DECIMALS)?,
                price: to_decimal(U256::from(filled.price), ASSET_DECIMALS)?,
                notional: to_decimal(U256::from(filled.notional), ASSET_DECIMALS)?,
            }),
            VenueOutcome::Unfilled => Err(Refusal::NoFill),
            VenueOutcome::Rejected(why) => Err(Refusal::Refused(why)),
            VenueOutcome::Silent => Err(Refusal::Refused(
                "the venue said nothing about that order".to_string(),
            )),
        }
    }

    /// Carry `amount` along `steps`: the vault's cash to its station or
    /// back, and the station's to the venue or back, each through the
    /// vault's or the station's own instruction.
    pub async fn carry(&self, steps: &[Step], amount: u64) -> Result<()> {
        for step in steps {
            match step {
                Step::Pull | Step::Push => {
                    let ix = self
                        .platform
                        .ix_move(
                            &self.client,
                            &self.accounts,
                            &self.bundle,
                            &self.key.pubkey(),
                            amount,
                            matches!(step, Step::Pull),
                        )
                        .await?;
                    self.send(vec![ix]).await?;
                }
                // Orca's driver lends each call the dollars it needs and
                // takes back what it didn't use: nothing waits at Orca.
                Step::Deposit | Step::Withdraw if self.has_orca() => {
                    return Err(anyhow!(
                        "the {} driver holds no dollars at the venue between calls: an open, an add or a buy draws on the station's own",
                        super::orca::DRIVER
                    ));
                }
                Step::Deposit => {
                    let authority = self.authority().await?;
                    self.opened(&authority).await?;
                    let deposit = self.platform.venue_deposit(&authority, amount);
                    self.through_the_guard(deposit, amount).await?;
                }
                Step::Withdraw => {
                    let authority = self.authority().await?;
                    let withdraw =
                        on_chain::venue_withdraw(&self.platform, &self.client, &authority, amount)
                            .await?;
                    self.through_the_guard(withdraw, 0).await?;
                }
            }
        }
        Ok(())
    }

    /// The vault's cash, the station's counted dollars, and the venue
    /// account's value at the mark, each where it could be read.
    pub async fn balances(&self) -> (Option<u64>, Option<u64>, Option<u64>) {
        let vault = self
            .platform
            .vault(&self.client, &self.vault)
            .await
            .ok()
            .map(|vault| vault.cash);
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await
            .ok()
            .map(|station| station.idle);
        let venue = if self.has_orca() {
            self.orca_mark().await
        } else {
            match self.venue_value().await {
                Ok((value, _)) => Some(value.clamp(0, u64::MAX as i128) as u64),
                Err(_) => None,
            }
        };
        (vault, station, venue)
    }

    /// The station's account at the venue, valued at the mark and at exit.
    async fn venue_value(&self) -> Result<(i128, i128)> {
        let authority = self.authority().await?;
        let Some(account) = self
            .platform
            .margin_account(&self.client, &authority)
            .await?
        else {
            return Ok((0, 0));
        };
        let markets = on_chain::venue_markets(&self.client).await?;
        let held = held_in_order(&account, &markets)?;
        let mark = account
            .value_at(&held, local_venue::PriceKind::Mark)
            .map_err(|error| anyhow!("valuing the venue account: {error}"))?;
        let exit = account
            .value_at(&held, local_venue::PriceKind::Exit)
            .map_err(|error| anyhow!("valuing the venue account: {error}"))?;
        Ok((mark, exit))
    }

    /// Every market the venue lists.
    pub async fn markets(&self) -> Result<Vec<MarketState>> {
        on_chain::venue_markets(&self.client)
            .await?
            .iter()
            .map(market_state)
            .collect()
    }

    /// The vault's account at the venue as the agent sizes against it, the
    /// vault beside it.
    pub async fn account_state(&self) -> Result<strategy_sdk::AccountState> {
        let authority = self.authority().await?;
        let account = self
            .platform
            .margin_account(&self.client, &authority)
            .await?;
        let markets = on_chain::venue_markets(&self.client).await?;
        let venue: local_venue::Venue = self
            .client
            .load(&local_venue::venue_address())
            .await?
            .ok_or_else(|| anyhow!("the venue is not set up"))?;
        let (cash, value, exit, positions) = match &account {
            Some(account) => {
                let held = held_in_order(account, &markets)?;
                let value = |kind| {
                    account
                        .value_at(&held, kind)
                        .map_err(|error| anyhow!("valuing the venue account: {error}"))
                };
                let positions = account
                    .positions
                    .iter()
                    .map(|position| {
                        Ok(PositionState {
                            market: market_name(&position.market),
                            size: signed(position.size, SIZE_DECIMALS)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                (
                    account.cash,
                    value(local_venue::PriceKind::Mark)?,
                    value(local_venue::PriceKind::Exit)?,
                    positions,
                )
            }
            None => (0, 0, 0, Vec::new()),
        };
        let idle = self.platform.vault(&self.client, &self.vault).await?.cash;
        let station_idle = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?
            .idle;
        let vault_value = idle as i128 + station_idle as i128 + value;
        Ok(strategy_sdk::AccountState {
            cash: signed(cash, ASSET_DECIMALS)?,
            value: signed(value, ASSET_DECIMALS)?,
            exit_value: signed(exit, ASSET_DECIMALS)?,
            vault_value: signed(vault_value, ASSET_DECIMALS)?,
            idle: to_decimal(U256::from(idle), ASSET_DECIMALS)?,
            station_idle: to_decimal(U256::from(station_idle), ASSET_DECIMALS)?,
            margin_bps: venue.margin_bps.try_into().unwrap_or(u32::MAX),
            positions,
            markets: markets.iter().map(market_state).collect::<Result<_>>()?,
        })
    }
}

/// How long a send the node stopped answering about is watched on for, and
/// how often it is looked for: well past its blockhash's life.
const LATE_WATCH: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const LATE_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// What a send that ended in `error` may have cost the key.
#[derive(Debug, PartialEq)]
enum ToBill {
    /// It landed and failed: its fee was paid.
    Failed(String),
    /// The node stopped answering about it: it is paid for if it lands.
    IfItLands(String),
    /// It never landed, or never went out.
    Nothing,
}

fn to_bill(error: &anyhow::Error) -> ToBill {
    let failed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<chain::solana::TransactionFailed>())
        .and_then(|failed| failed.signature.clone());
    if let Some(signature) = failed {
        return ToBill::Failed(signature);
    }
    match error.downcast_ref::<chain::solana::MayStillLand>() {
        Some(late) => ToBill::IfItLands(late.signature.clone()),
        None => ToBill::Nothing,
    }
}

/// What the send `signature` cost `payer` once it is found landed, looked for
/// every [`LATE_POLL`] for [`LATE_WATCH`]; nothing if it never is.
async fn landed_late(client: &SolanaClient, signature: &str, payer: &Pubkey) -> i128 {
    let started = std::time::Instant::now();
    loop {
        if let Ok(Some(record)) = client.transaction(signature).await {
            tracing::info!(%signature, "a send the node stopped answering about landed: billed");
            return record.spent_by(payer);
        }
        if started.elapsed() > LATE_WATCH {
            tracing::warn!(%signature, "a send the node stopped answering about was never found landed");
            return 0;
        }
        tokio::time::sleep(LATE_POLL).await;
    }
}

/// `cost` added to what the agent's sends have cost the key.
fn add_spent(spent: &AtomicI64, cost: i128) {
    spent.fetch_add(
        cost.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        Ordering::SeqCst,
    );
}

/// How a send that failed counts against the agent's health: refused if the
/// cluster ran it and said no, a venue that never answered otherwise.
pub fn tally_of(error: &anyhow::Error) -> crate::host::Tally {
    let refused = error
        .chain()
        .any(|cause| cause.is::<chain::solana::TransactionFailed>());
    if refused {
        crate::host::Tally::Refused
    } else {
        crate::host::Tally::Unanswered
    }
}

fn refusal(error: anyhow::Error) -> Refusal {
    match tally_of(&error) {
        crate::host::Tally::Refused => Refusal::Refused(format!("{error:#}")),
        _ => Refusal::Unanswered(format!("{error:#}")),
    }
}

/// An amount in units, as the venue's narrower integer.
fn units<T: TryFrom<U256>>(value: Result<U256>) -> std::result::Result<T, Refusal> {
    let value = value.map_err(|error| Refusal::Refused(format!("{error:#}")))?;
    T::try_from(value).map_err(|_| Refusal::Refused("that amount is too large".to_string()))
}

fn signed(value: i128, decimals: u32) -> Result<strategy_sdk::Decimal> {
    signed_to_decimal(
        I256::try_from(value).map_err(|_| anyhow!("{value} is out of range"))?,
        decimals,
    )
}

/// The markets an account holds, in its order, as the venue values them.
fn held_in_order(
    account: &local_venue::MarginAccount,
    markets: &[local_venue::Market],
) -> Result<Vec<local_venue::Market>> {
    account
        .positions
        .iter()
        .map(|position| {
            markets
                .iter()
                .find(|market| market.id == position.market)
                .cloned()
                .ok_or_else(|| anyhow!("the venue no longer lists a market the account holds"))
        })
        .collect()
}

fn market_name(id: &[u8; 32]) -> String {
    let end = id.iter().position(|byte| *byte == 0).unwrap_or(32);
    String::from_utf8_lossy(&id[..end]).to_string()
}

fn market_state(market: &local_venue::Market) -> Result<MarketState> {
    Ok(MarketState {
        market: market_name(&market.id),
        is_perp: market.is_perp,
        mark: to_decimal(U256::from(market.mark), ASSET_DECIMALS)?,
        oracle: to_decimal(U256::from(market.oracle), ASSET_DECIMALS)?,
        bid: to_decimal(U256::from(market.bid), ASSET_DECIMALS)?,
        ask: to_decimal(U256::from(market.ask), ASSET_DECIMALS)?,
        funding_index: signed(market.funding_index, ASSET_DECIMALS)?,
        funding_rate: signed(market.funding_rate, ASSET_DECIMALS)?,
    })
}

/// What an agent is told when its vault halts, for the reason the vault
/// gave.
fn halting(reason: chain::solana::programs::vault::state::HaltReason) -> VaultEvent {
    use chain::solana::programs::vault::state::HaltReason;
    VaultEvent::Halting(
        match reason {
            HaltReason::Killed => "the vault was stopped",
            HaltReason::Frozen => "a claim went unserved and the vault froze",
            HaltReason::Retired => "the author retired the vault and its wind-down is over",
            HaltReason::Sealed => "the vault is being wound down",
            HaltReason::None => "the vault has halted",
        }
        .to_string(),
    )
}

/// How often the cluster is looked at on an agent's behalf.
const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Follow one vault for its agent: the prices of each market it follows as
/// the stand-in venue posts them, on the account stream (unless `venue` is
/// false: an agent trading elsewhere hears its own venue's events), and the
/// vault's halt, on the vault's. A market it doesn't follow is passed over,
/// so its posts don't wake the agent. The halt is said once; a watcher
/// started after it has passed says it first.
pub async fn watch(
    client: SolanaClient,
    platform: Platform,
    vault: Pubkey,
    venue: bool,
    following: crate::watch::Following,
    account_events: UnboundedSender<Vec<u8>>,
    vault_events: UnboundedSender<VaultEvent>,
) {
    let mut seen: HashMap<[u8; 32], local_venue::Market> = HashMap::new();
    let mut halt_said = false;
    let mut unread = crate::Throttle::default();
    loop {
        let markets = if venue {
            on_chain::venue_markets(&client).await
        } else {
            Ok(Vec::new())
        };
        match markets {
            Ok(markets) => {
                for market in markets {
                    let changed = seen.get(&market.id).is_none_or(|before| {
                        (before.mark, before.bid, before.ask, before.funding_index)
                            != (market.mark, market.bid, market.ask, market.funding_index)
                    });
                    if changed {
                        if let Ok(state) = market_state(&market)
                            && following.follows(&state.market)
                            && account_events
                                .send(strategy_sdk::to_payload(&AccountEvent::Prices(state)))
                                .is_err()
                        {
                            return;
                        }
                        seen.insert(market.id, market);
                    }
                }
            }
            Err(error) => {
                if unread.due(std::time::Instant::now()) {
                    tracing::warn!(%vault, error = format!("{error:#}"), "could not read the venue's markets; the agent hears nothing new until they can be");
                }
            }
        }
        if !halt_said
            && let Ok(account) = platform.vault(&client, &vault).await
            && account.halted_at != 0
        {
            halt_said = true;
            if vault_events.send(halting(account.halt_reason)).is_err() {
                return;
            }
        }
        tokio::time::sleep(LOOK_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_market_name_is_read_up_to_its_padding() {
        assert_eq!(market_name(&name("BTC-PERP")), "BTC-PERP");
        assert_eq!(market_name(&[b'x'; 32]), "x".repeat(32));
    }

    #[test]
    fn a_send_the_cluster_ran_and_refused_counts_as_refused_and_any_other_as_unanswered() {
        let refused: anyhow::Error = chain::solana::TransactionFailed {
            signature: Some("sig".into()),
            err: serde_json::json!({"InstructionError": [0, {"Custom": 6000}]}),
            custom: Some(6000),
            logs: vec![],
        }
        .into();
        assert!(matches!(
            tally_of(&refused.context("buying spot")),
            crate::host::Tally::Refused
        ));
        assert!(matches!(
            tally_of(&anyhow!("the node did not answer")),
            crate::host::Tally::Unanswered
        ));
    }

    #[test]
    fn a_send_that_landed_and_failed_is_billed_and_one_that_may_still_land_is_billed_if_it_lands() {
        let failed: anyhow::Error = chain::solana::TransactionFailed {
            signature: Some("failed".into()),
            err: serde_json::json!({"InstructionError": [0, {"Custom": 6000}]}),
            custom: Some(6000),
            logs: vec![],
        }
        .into();
        assert_eq!(
            to_bill(&failed.context("buying spot")),
            ToBill::Failed("failed".into())
        );
        // However deep the client's own context put it.
        let late = anyhow!("the node did not answer")
            .context(chain::solana::MayStillLand {
                signature: "late".into(),
            })
            .context("setting up a lookup table");
        assert_eq!(to_bill(&late), ToBill::IfItLands("late".into()));
        let never: anyhow::Error = chain::solana::NeverLanded {
            signature: "never".into(),
        }
        .into();
        assert_eq!(to_bill(&never), ToBill::Nothing);
        assert_eq!(
            to_bill(&anyhow!("refused before it went out")),
            ToBill::Nothing
        );
    }

    #[test]
    fn an_amount_too_large_for_the_venue_is_refused_rather_than_cut() {
        let huge: std::result::Result<u64, Refusal> = units(Ok(U256::from(u128::MAX)));
        assert!(matches!(huge, Err(Refusal::Refused(_))));
        let fits: std::result::Result<u64, Refusal> = units(Ok(U256::from(5u64)));
        assert!(matches!(fits, Ok(5)));
    }
}
