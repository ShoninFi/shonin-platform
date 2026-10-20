//! The vault: shares, value, requests, the tank, claims and stopping.

use serde::{Deserialize, Serialize};
use time::{Date, PrimitiveDateTime};

use super::common::Chain;
use super::venue::ExitClass;
use crate::manifest::Allowances;

/// What a vault is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VaultState {
    /// Created, waiting for its stations on other chains.
    Activating,
    /// Taking deposits, trading.
    Open,
    /// Retired by its author: no deposits; the agent runs out its wind-down.
    WindingDown,
    /// Frozen after a claim went untried: no new deposits.
    Frozen,
    /// Stopped by the admin key.
    Killed,
    /// Nothing at the stations: the value is cash, and fees have stopped.
    Closed,
    /// Never opened; the tank went back to its author.
    Cancelled,
}

impl VaultState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Activating => "activating",
            Self::Open => "open",
            Self::WindingDown => "winding down",
            Self::Frozen => "frozen",
            Self::Killed => "killed",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn takes_deposits(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// The author's balance for the agent's running costs. Never depositors'
/// money: it is excluded from the value shares are worth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tank {
    pub balance: f64,
    /// What a day costs at the allowances the author set and the published
    /// prices: the most a day can be charged.
    pub daily_charge: f64,
    /// "Keep my tank at": what it is topped back up to from the author's
    /// shares. Zero is off.
    pub keep_at: f64,
    /// The running usage total collected so far.
    pub usage_settled: f64,
}

impl Tank {
    /// Days of charges the balance covers.
    pub fn runway_days(&self) -> f64 {
        if self.daily_charge <= 0.0 {
            return f64::INFINITY;
        }
        self.balance / self.daily_charge
    }
}

/// A request waiting for a valuation to price it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub owner: String,
    pub kind: RequestKind,
    /// Dollars for a deposit or a tank fill asked in dollars; shares
    /// otherwise.
    pub amount: f64,
    /// The first valuation it can be priced at.
    pub earliest: PrimitiveDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestKind {
    Deposit,
    Withdrawal,
    /// A claim, priced again after its fraction was closed.
    Claim,
    /// The author's shares, into the tank.
    TankFill,
}

impl RequestKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Withdrawal => "withdrawal",
            Self::Claim => "claim",
            Self::TankFill => "tank top-up",
        }
    }
}

/// Why a vault stopped, and when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Halt {
    pub at: PrimitiveDateTime,
    /// `killed`, `frozen`, `retired` or `sealed`.
    pub reason: String,
}

/// A lowering of the idle floor that applies later.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingFloor {
    pub bps: u32,
    pub from: PrimitiveDateTime,
}

/// A withdrawal the idle balance could not pay: the leaver's shares are
/// pending while their fraction of the vault is closed and sold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub id: u32,
    pub owner: String,
    pub shares: f64,
    /// The fraction of every position that was closed for it.
    pub fraction_closed: f64,
    pub booked: Date,
    pub state: ClaimState,
    /// Paid out once the cash is home, valued at exit-side marks then.
    pub settled_for: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimState {
    /// Booked, and the close has not been attempted yet.
    Pending,
    /// A settlement was tried and the market was outside the exit band, so
    /// nothing was sold badly. It is retried.
    Waiting,
    /// Its fraction was closed; it is priced at the next valuation.
    Queued,
    /// Priced, burned and paid.
    Paid,
    /// Taken through the fallback: the cash's share now, the rest as the
    /// stations bring it home.
    Fallback,
}

impl ClaimState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Waiting => "waiting for the market",
            Self::Queued => "closed, being priced",
            Self::Paid => "paid",
            Self::Fallback => "taken pro rata",
        }
    }
}

/// Capital moving between two of the vault's own accounts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InTransit {
    pub amount: f64,
    pub from: String,
    pub to: String,
    pub route: String,
    pub elapsed_minutes: u32,
    pub timeout_minutes: u32,
}

/// The fee schedule a vault was created with. It never changes under a
/// running vault.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Fees {
    /// The holding fee: charged on value, per year, to the platform.
    pub holding_bps: u32,
    /// Charged on gains above the high-water mark, to the author.
    pub author_bps: u32,
    /// Charged on the same gains, to the platform.
    pub platform_bps: u32,
    /// Charged on every deposit and withdrawal, and kept in the pool.
    pub transaction_bps: u32,
}

/// Pooled capital behind one agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vault {
    pub address: String,
    pub chain: Chain,
    /// What deposits and withdrawals are paid in.
    pub asset: String,
    pub state: VaultState,
    pub created: Date,
    /// The vault's station on its own chain: the master of its venue
    /// accounts.
    pub station: String,
    /// Every share at the price of the last valuation (the price every
    /// request is priced at): a deposit priced since is in it, as the shares
    /// it made. Dollars in escrow are not. A closed vault is worth its cash.
    pub value: f64,
    /// When that valuation was made, on the chain's own clock; None before
    /// the first.
    #[serde(default)]
    pub valued_at: Option<PrimitiveDateTime>,
    /// Its cash, which pays withdrawals without closing anything.
    pub idle: f64,
    /// Deposits waiting for a valuation, held apart.
    pub escrow: f64,
    pub shares: f64,
    /// Value per share after the fees owed, at the last valuation.
    pub share_price: f64,
    /// The high-water mark: no profit fee is owed below it.
    pub high_water_mark: f64,
    /// When fees were last paid; they are paid on the first valuation on or
    /// after Monday.
    pub last_crystallised: Date,
    /// The share of value kept as cash, which the station can't take.
    pub idle_floor_bps: u32,
    pub pending_floor: Option<PendingFloor>,
    /// What the agent may use, which caps what the tank is charged.
    pub allowances: Allowances,
    /// Requests waiting to be priced.
    pub requests: Vec<Request>,
    /// Who can't deposit, as the vault holds the list.
    pub restricted: Vec<String>,
    pub halt: Option<Halt>,
    pub fees: Fees,
    /// How far either side of the venue's price a claim may close. Fixed when
    /// the vault was created, like its fees.
    pub exit_band_bps: u32,
    pub tank: Tank,
    pub exit_class: ExitClass,
    /// Days an unserved claim freezes the vault after.
    pub freeze_after_days: u32,
    pub claims: Vec<Claim>,
    pub in_transit: Option<InTransit>,
}

impl Vault {
    /// What the idle balance can pay out without booking a claim.
    pub fn idle_share(&self) -> f64 {
        if self.value <= 0.0 {
            return 0.0;
        }
        self.idle / self.value
    }
}
