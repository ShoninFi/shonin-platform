//! A strategy: one frozen bundle, its vault or its connected accounts, and
//! everything shown beside it. A new version is a new strategy, never an
//! edit of this one.

use serde::{Deserialize, Serialize};
use time::Date;

use super::activity::{Kill, Trade, VaultEvent};
use super::attestation::Attestation;
use super::bundle::Bundle;
use super::common::Point;
use super::health::Health;
use super::review::Review;
use super::vault::Vault;
use super::venue::Leg;

/// Where a strategy stands on the leaderboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// Live, with enough history to be ranked.
    Ranked,
    /// Live, but the record is still short: listed, labelled, not ranked.
    New,
    /// The author published a successor. This one carries on as it was.
    Superseded,
    /// Stopped by its author; the record stays.
    Retired,
    /// Stopped by the admin key, with the reason on the page.
    Killed,
    /// The build failed. There is no vault and no record, and the log is public.
    BuildFailed,
}

impl Standing {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ranked => "ranked",
            Self::New => "new",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
            Self::Killed => "killed",
            Self::BuildFailed => "build failed",
        }
    }

    /// Whether it appears on the leaderboard's default view.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Ranked | Self::New)
    }
}

/// Which product a strategy was published for.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Product {
    /// Pooled capital in a vault contract.
    Vault(Box<Vault>),
    /// Owners' own accounts at a broker, each an entry and a tank on-chain.
    Connected(Connected),
}

/// A strategy that runs on owners' own broker accounts. There is no pool and
/// no share price: value is an attested read of each account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connected {
    pub broker: String,
    /// What the broker's rules stop the strategy doing.
    pub broker_limits: Vec<String>,
    pub accounts: u32,
    /// The total value of every account running this strategy.
    pub total_value: f64,
    /// How often the executor reads each account and posts the read on-chain.
    pub read_cadence: String,
    /// The registry the entries and tanks live on.
    pub registry: String,
    /// What an owner needs before they can connect.
    pub requirements: Vec<String>,
    /// A typical tank: what a month of running costs comes to.
    pub monthly_cost: f64,
}

/// The daily series a record is read from, and everything derived from it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Value per share, or an account's value, day by day.
    pub value_per_share: Vec<Point>,
    /// The same series rescaled to the platform's volatility target.
    pub normalised: Vec<Point>,
    /// Peak-to-trough fall on each day, as a negative fraction.
    pub drawdown: Vec<Point>,
    /// Capital behind the strategy, day by day.
    pub aum: Vec<Point>,
    /// Return in each calendar month covered by the series.
    pub monthly: Vec<Point>,
    pub trades: u32,
    /// What the vault held on average, and at its peak, over the record.
    pub average_aum: f64,
    pub peak_aum: f64,
}

/// One published strategy version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strategy {
    /// Stable across versions.
    pub id: String,
    /// `id` and version together: what the URL uses.
    pub slug: String,
    pub version: u32,
    /// The author's title and description, shown as the author's words.
    pub title: String,
    pub description: String,
    /// The author's handle.
    pub author: String,
    pub standing: Standing,
    pub registered: Date,
    pub days_live: u32,
    /// An earlier version this one replaces: a claim, not a mechanism.
    pub supersedes: Option<String>,
    /// Set on the earlier version when a successor declares it.
    pub superseded_by: Option<String>,
    pub bundle: Bundle,
    pub product: Product,
    pub legs: Vec<Leg>,
    pub review: Option<Review>,
    /// Earlier reviews, newest first. Nothing is re-reviewed automatically.
    pub past_reviews: Vec<Review>,
    pub health: Option<Health>,
    pub attestation: Option<Attestation>,
    pub record: Option<Record>,
    pub trades: Vec<Trade>,
    pub events: Vec<VaultEvent>,
    pub kill: Option<Kill>,
    /// The limits the strategy sets in its own code, in the author's words.
    pub limits: Vec<String>,
    /// The hosts and feeds it depends on, named in its description.
    pub dependencies: Vec<String>,
}

impl Strategy {
    pub fn name(&self) -> String {
        format!("{} v{}", self.id, self.version)
    }

    pub fn vault(&self) -> Option<&Vault> {
        match &self.product {
            Product::Vault(vault) => Some(vault),
            Product::Connected(_) => None,
        }
    }

    pub fn connected(&self) -> Option<&Connected> {
        match &self.product {
            Product::Connected(connected) => Some(connected),
            Product::Vault(_) => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.product, Product::Connected(_))
    }

    /// What the strategy is worth today, whichever product it is.
    pub fn capital(&self) -> f64 {
        match &self.product {
            Product::Vault(vault) => vault.value,
            Product::Connected(connected) => connected.total_value,
        }
    }

    /// Days a strategy has to run live before it is ranked.
    pub const DAYS_TO_RANK: u32 = 90;

    /// The volatility every record is rescaled to for comparison.
    pub const VOLATILITY_TARGET: f64 = 0.15;
}
