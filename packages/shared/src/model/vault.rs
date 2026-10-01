//! The vault: shares, value, the tank, claims and kills.

use serde::{Deserialize, Serialize};
use time::Date;

use super::common::Chain;
use super::venue::ExitClass;

/// What a vault is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VaultState {
    /// Taking deposits, trading.
    Open,
    /// Frozen after a claim went unserved: no new deposits.
    Frozen,
    /// Stopped by the admin key; positions unwound, everyone paid out.
    Killed,
    /// Nothing left to do: the strategy is retired and the tank reclaimed.
    Closed,
}

impl VaultState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Frozen => "frozen",
            Self::Killed => "killed",
            Self::Closed => "closed",
        }
    }

    pub fn takes_deposits(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// The author's balance for the strategy's running costs. Never depositors'
/// money: it is excluded from the value shares are worth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tank {
    pub balance: f64,
    /// What the last day's usage statement charged, or, before there is one,
    /// the most a day may charge at the allowances the author declared.
    pub daily_charge: f64,
    /// What auto-fill tops it back up to.
    pub target: f64,
    /// The share of each author fee that refills the tank, in basis points.
    pub autofill_bps: u32,
    /// Unpaid charges, repaid from future author fees before anything reaches
    /// the author.
    pub debt: f64,
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
    /// Closed, valued again, burned at the marks, and paid.
    Paid,
}

impl ClaimState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Waiting => "waiting for the market",
            Self::Paid => "paid",
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
    /// Everything the vault is worth, less the tank.
    pub value: f64,
    /// The part sitting in the contract, which pays withdrawals at once.
    pub idle: f64,
    pub shares: f64,
    /// Value divided by shares.
    pub share_price: f64,
    /// The highest share price fees have been crystallised at.
    pub high_water_mark: f64,
    pub last_crystallised: Date,
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
