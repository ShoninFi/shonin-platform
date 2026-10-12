//! Reading what an author's flows act on, from the chain, at the moment it
//! matters: the publishing fee, the factory's terms and its plan for a
//! strategy, the price of the allowances, and a vault's tank, allowances,
//! idle floor, owner and state.

use alloy_primitives::{Address, B256, U256};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::prices::IPrices;
use chain::bindings::publishing::IPublishing;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::IVault;

use super::form::Allowances;
use crate::tx::TxError;
use crate::tx::port::Reader;
use crate::tx::read::{State, read};

/// What publishing costs, and in which dollar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publishing {
    pub fee: U256,
    pub asset: Address,
}

pub async fn publishing<R: Reader>(reader: &R, at: Address) -> Result<Publishing, TxError> {
    Ok(Publishing {
        fee: read(reader, at, IPublishing::feeCall {}).await?,
        asset: read(reader, at, IPublishing::assetCall {}).await?,
    })
}

/// What the publishing contract's own record says of a payment for one
/// upload, judged against the wallet the upload named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentRecord {
    /// Paid by the wallet the upload named, under this id: the job starts
    /// once the build service has read it confirmed.
    ByUploader { strategy_id: U256 },
    /// Paid by another wallet: it starts nothing.
    ByAnother { strategy_id: U256, payer: Address },
    /// No payment for the upload is recorded where it was looked for.
    None,
}

/// The most payments looked through after a known count.
const MOST_SEARCHED: u64 = 256;

/// The payment recorded under `strategy_id`, if it is for `upload`.
pub async fn payment_at<R: Reader>(
    reader: &R,
    publishing: Address,
    strategy_id: U256,
    upload: B256,
    uploader: Address,
) -> Result<PaymentRecord, TxError> {
    let record = read(
        reader,
        publishing,
        IPublishing::submissionOfCall {
            strategyId: strategy_id,
        },
    )
    .await?;
    Ok(judge(
        strategy_id,
        record.author,
        record.upload,
        upload,
        uploader,
    ))
}

/// A payment for `upload` among those recorded after the first `since`
/// (or among the latest, when that count isn't known): the uploader's if
/// there is one.
pub async fn payment_after<R: Reader>(
    reader: &R,
    publishing: Address,
    upload: B256,
    uploader: Address,
    since: Option<U256>,
) -> Result<PaymentRecord, TxError> {
    let count = read(reader, publishing, IPublishing::countCall {}).await?;
    let since = since.unwrap_or_else(|| count.saturating_sub(U256::from(MOST_SEARCHED)));
    let last = count.min(since.saturating_add(U256::from(MOST_SEARCHED)));
    let mut found = PaymentRecord::None;
    let mut id = since.saturating_add(U256::from(1u8));
    while id <= last {
        match payment_at(reader, publishing, id, upload, uploader).await? {
            PaymentRecord::None => {}
            mine @ PaymentRecord::ByUploader { .. } => return Ok(mine),
            other => found = other,
        }
        id += U256::from(1u8);
    }
    Ok(found)
}

fn judge(
    strategy_id: U256,
    payer: Address,
    paid_for: B256,
    upload: B256,
    uploader: Address,
) -> PaymentRecord {
    if payer == Address::ZERO || paid_for != upload {
        PaymentRecord::None
    } else if payer == uploader {
        PaymentRecord::ByUploader { strategy_id }
    } else {
        PaymentRecord::ByAnother { strategy_id, payer }
    }
}

/// The factory's terms that an activation names and is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terms {
    /// The hash an activation passes: the terms in force.
    pub hash: B256,
    /// The least a vault may be activated with.
    pub minimum_tank: U256,
    pub minimum_deposit: U256,
    pub author_bps: u16,
    /// How long a vault with stations elsewhere has to open before it can
    /// be cancelled (seconds).
    pub activation_window: u64,
    /// How long a lowered idle floor waits (seconds).
    pub floor_delay: u64,
    /// How long a retirement winds down (seconds).
    pub wind_down: u64,
}

pub async fn terms<R: Reader>(reader: &R, factory: Address) -> Result<Terms, TxError> {
    let hash = read(reader, factory, IVaultFactory::termsCall {}).await?;
    let config = read(reader, factory, IVaultFactory::configCall {}).await?;
    Ok(Terms {
        hash,
        minimum_tank: config.minimumTank,
        minimum_deposit: config.minimumDeposit,
        author_bps: config.authorBps,
        activation_window: config.timers.activationWindow,
        floor_delay: config.timers.floorDelay,
        wind_down: config.timers.windDown,
    })
}

/// Where a strategy stands before it is activated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub author: Address,
    pub registered: bool,
    pub reviewed: bool,
    /// Its vault, once activated.
    pub vault: Option<Address>,
    /// Chain ids besides this one that it trades on.
    pub other_chains: Vec<u64>,
}

/// The platform's contracts an activation reads, on one chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registries {
    pub factory: Address,
    pub strategies: Address,
    pub reviews: Address,
    pub prices: Address,
}

pub async fn readiness<R: Reader>(
    reader: &R,
    at: &Registries,
    chain_id: u64,
    strategy_id: U256,
) -> Result<Readiness, TxError> {
    let record = read(
        reader,
        at.strategies,
        IStrategyRegistry::strategyOfCall {
            strategyId: strategy_id,
        },
    )
    .await?;
    let reviewed = read(
        reader,
        at.reviews,
        IReviewRegistry::hasReviewCall {
            strategyId: strategy_id,
        },
    )
    .await?;
    let vault = read(
        reader,
        at.factory,
        IVaultFactory::vaultOfCall {
            strategyId: strategy_id,
        },
    )
    .await?;
    let named = read(reader, at.factory, IVaultFactory::chainsCall {}).await?;
    let mut other_chains = Vec::new();
    for need in &record.chains {
        // A name the factory doesn't know is a chain it can't open either.
        let id = named
            .iter()
            .find(|known| known.name == need.chain)
            .map(|known| u64::try_from(known.chainId).unwrap_or(u64::MAX))
            .unwrap_or(u64::MAX);
        if id != chain_id {
            other_chains.push(id);
        }
    }
    Ok(Readiness {
        author: record.author,
        registered: record.registeredAt != 0,
        reviewed,
        vault: (vault != Address::ZERO).then_some(vault),
        other_chains,
    })
}

/// The most a day at these allowances can cost, at the prices in force now:
/// the contract's own arithmetic, in the dollar's base units.
pub async fn daily_cost<R: Reader>(
    reader: &R,
    prices: Address,
    allowances: &Allowances,
) -> Result<U256, TxError> {
    read(
        reader,
        prices,
        IPrices::dailyCostCall {
            allowances: allowances.to_sol(),
        },
    )
    .await
}

/// A vault as its author's controls need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorVault {
    pub state: State,
    pub author: Address,
    pub tank: Tank,
    pub allowances: Allowances,
    pub floor: Floor,
    /// The author's own shares: their earnings, paid as shares.
    pub shares: U256,
    pub share_price: U256,
    pub cash: U256,
    pub author_bps: u16,
    pub escape_delay: u64,
    pub usage_freshness: u64,
    pub wind_down: u64,
    pub ready_to_close: bool,
    /// The chain's clock when this was read.
    pub now: u64,
}

/// The tank, as the vault keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tank {
    pub balance: U256,
    /// What auto-fill tops it back up to; zero is off.
    pub keep_at: U256,
    pub usage_settled: U256,
    pub usage_as_of: u64,
    /// When the author asked to withdraw without a signed total; zero when
    /// they haven't.
    pub escape_requested_at: u64,
    pub fill_pending: bool,
}

/// The idle floor as the vault keeps it: the floor stored, and a lowering
/// waiting until `pending_at` (zero when none is waiting; the lowered floor
/// itself may be zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Floor {
    pub bps: u16,
    pub pending_bps: u16,
    pub pending_at: u64,
}

impl Floor {
    /// The floor in force at chain time `now`: a lowering counts once its
    /// time has come, as the vault counts it.
    pub fn now(&self, now: u64) -> u16 {
        if self.pending_at != 0 && now >= self.pending_at {
            self.pending_bps
        } else {
            self.bps
        }
    }

    /// The lowering still waiting at `now`, and when it takes effect.
    pub fn waiting(&self, now: u64) -> Option<(u16, u64)> {
        (self.pending_at != 0 && now < self.pending_at)
            .then_some((self.pending_bps, self.pending_at))
    }
}

/// Where the unsigned tank withdrawal stands at chain time `now`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// Not asked for, or used.
    NotAsked,
    /// Asked for; it opens at `ready_at`.
    Waiting { ready_at: u64 },
    /// Open until `until`.
    Open { until: u64 },
    /// Asked for too long ago: it lapsed.
    Lapsed,
    /// A closed or cancelled vault pays its tank back without asking.
    Free,
}

impl AuthorVault {
    pub fn escape(&self) -> Escape {
        if matches!(self.state, State::Closed | State::Cancelled) {
            return Escape::Free;
        }
        let asked = self.tank.escape_requested_at;
        if asked == 0 {
            return Escape::NotAsked;
        }
        let ready_at = asked.saturating_add(self.escape_delay);
        let until = asked.saturating_add(self.escape_delay.saturating_mul(2));
        if self.now < ready_at {
            Escape::Waiting { ready_at }
        } else if self.now <= until {
            Escape::Open { until }
        } else {
            Escape::Lapsed
        }
    }
}

pub async fn author_vault<R: Reader>(
    reader: &R,
    vault: Address,
    author_of: Address,
) -> Result<AuthorVault, TxError> {
    let state: State = read(reader, vault, IVault::stateCall {}).await?.into();
    let author = read(reader, vault, IVault::authorCall {}).await?;
    let tank = read(reader, vault, IVault::tankCall {}).await?;
    let allowances = read(reader, vault, IVault::allowancesCall {}).await?;
    let floor = read(reader, vault, IVault::idleFloorCall {}).await?;
    let shares = read(reader, vault, IVault::balanceOfCall { owner: author_of }).await?;
    let share_price = read(reader, vault, IVault::sharePriceCall {}).await?;
    let cash = read(reader, vault, IVault::cashCall {}).await?;
    let fees = read(reader, vault, IVault::feesCall {}).await?;
    let timers = read(reader, vault, IVault::timersCall {}).await?;
    let ready_to_close = read(reader, vault, IVault::readyToCloseCall {}).await?;
    let now = reader.chain_time().await?;
    Ok(AuthorVault {
        state,
        author,
        tank: Tank {
            balance: tank.balance,
            keep_at: tank.keepAt,
            usage_settled: tank.usageSettled,
            usage_as_of: tank.usageAsOf,
            escape_requested_at: tank.escapeRequestedAt,
            fill_pending: tank.fillPending,
        },
        allowances: Allowances::from_sol(&allowances),
        floor: Floor {
            bps: floor.bps,
            pending_bps: floor.pendingBps,
            pending_at: floor.pendingAt,
        },
        shares,
        share_price,
        cash,
        author_bps: fees.authorBps,
        escape_delay: timers.tankEscapeDelay,
        usage_freshness: timers.usageFreshness,
        wind_down: timers.windDown,
        ready_to_close,
        now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lowered_floor_counts_only_once_its_time_has_come() {
        let floor = Floor {
            bps: 500,
            pending_bps: 300,
            pending_at: 1_000,
        };
        assert_eq!(floor.now(999), 500);
        assert_eq!(floor.waiting(999), Some((300, 1_000)));
        assert_eq!(floor.now(1_000), 300);
        assert_eq!(floor.waiting(1_000), None);
    }

    #[test]
    fn a_floor_lowered_to_zero_is_still_a_waiting_change() {
        let floor = Floor {
            bps: 500,
            pending_bps: 0,
            pending_at: 1_000,
        };
        assert_eq!(floor.waiting(10), Some((0, 1_000)));
        assert_eq!(floor.now(2_000), 0);
        let raised = Floor {
            bps: 700,
            pending_bps: 0,
            pending_at: 0,
        };
        assert_eq!(raised.waiting(10), None);
        assert_eq!(raised.now(10), 700);
    }

    fn vault(state: State, asked: u64, now: u64) -> AuthorVault {
        AuthorVault {
            state,
            author: Address::ZERO,
            tank: Tank {
                escape_requested_at: asked,
                ..Tank::default()
            },
            allowances: Allowances::default(),
            floor: Floor::default(),
            shares: U256::ZERO,
            share_price: U256::ZERO,
            cash: U256::ZERO,
            author_bps: 1_000,
            escape_delay: 100,
            usage_freshness: 0,
            wind_down: 0,
            ready_to_close: false,
            now,
        }
    }

    #[test]
    fn the_unsigned_withdrawal_opens_after_its_delay_and_lapses_after_twice_it() {
        assert_eq!(vault(State::Open, 0, 50).escape(), Escape::NotAsked);
        assert_eq!(
            vault(State::Open, 1_000, 1_099).escape(),
            Escape::Waiting { ready_at: 1_100 }
        );
        assert_eq!(
            vault(State::Open, 1_000, 1_100).escape(),
            Escape::Open { until: 1_200 }
        );
        assert_eq!(
            vault(State::Open, 1_000, 1_200).escape(),
            Escape::Open { until: 1_200 }
        );
        assert_eq!(vault(State::Open, 1_000, 1_201).escape(), Escape::Lapsed);
        assert_eq!(vault(State::Closed, 0, 1).escape(), Escape::Free);
    }
}
