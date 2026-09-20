//! What the viewer holds, and what happens when they deposit or withdraw.
//!
//! Everything here lives in the browser: a wallet the viewer connects, the
//! shares they hold in each vault, and any claim they are waiting on. It
//! follows the rules the contracts enforce — deposits at mark, withdrawals at
//! exit-side marks, the transaction fee kept in the pool, and a claim when the
//! idle balance can't cover a withdrawal — so the flow is the real one even
//! though nothing is settled anywhere.

use futures_signals::signal::Mutable;
use serde::{Deserialize, Serialize};
use shared::{ClaimState, Strategy, Vault};

use crate::data;
use crate::state;

/// A fraction of the spread a leaver pays by being valued at exit-side marks:
/// a long at the best bid, a short at the best ask.
const EXIT_SPREAD: f64 = 0.0008;

/// Shares held in one vault, and what they cost.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Position {
    pub slug: String,
    pub shares: f64,
    /// What was paid for them, for the profit and loss figure.
    pub cost: f64,
}

/// A withdrawal the idle balance couldn't pay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub id: u32,
    pub slug: String,
    /// Shares marked pending until the claim settles.
    pub shares: f64,
    /// The fraction of the vault closed to serve it.
    pub fraction: f64,
    pub state: ClaimState,
    /// What it eventually paid, once the cash was home.
    pub paid: Option<f64>,
}

/// Everything the viewer has done, saved in their browser and nowhere else.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Holdings {
    pub address: Option<String>,
    pub balance: f64,
    pub positions: Vec<Position>,
    pub claims: Vec<Claim>,
    next_claim: u32,
}

impl Holdings {
    pub fn position(&self, slug: &str) -> Option<&Position> {
        self.positions.iter().find(|held| held.slug == slug)
    }

    pub fn claims_for(&self, slug: &str) -> Vec<&Claim> {
        self.claims
            .iter()
            .filter(|claim| claim.slug == slug)
            .collect()
    }

    pub fn is_connected(&self) -> bool {
        self.address.is_some()
    }

    /// What every position is worth at today's value per share.
    pub fn value(&self) -> f64 {
        self.positions
            .iter()
            .map(|held| {
                data::strategy(&held.slug)
                    .and_then(Strategy::vault)
                    .map_or(0.0, |vault| held.shares * vault.share_price)
            })
            .sum()
    }

    /// What those positions cost.
    pub fn cost(&self) -> f64 {
        self.positions.iter().map(|held| held.cost).sum()
    }
}

/// The viewer's holdings. Read it with `holdings().signal_cloned()`.
pub fn holdings() -> &'static Mutable<Holdings> {
    state::get().holdings()
}

fn save() {
    if let Ok(json) = serde_json::to_string(&holdings().get_cloned()) {
        state::storage_set("holdings", &json);
    }
}

/// What was saved last time, if anything was.
pub fn restore() -> Holdings {
    state::storage_get("holdings")
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Connect a wallet. Nothing is signed and nothing leaves the browser: it is
/// the flow, with an address made up on the spot.
pub fn connect(label: &str) {
    let address = data::address(&format!("viewer-{label}"));
    holdings().replace_with(|holdings| {
        holdings.address = Some(address.clone());
        if holdings.balance <= 0.0 {
            // Enough to reach past a smaller vault's idle balance, so the
            // claim path can be walked through rather than only described.
            holdings.balance = 250_000.0;
        }
        holdings.clone()
    });
    save();
}

pub fn disconnect() {
    holdings().replace_with(|holdings| {
        holdings.address = None;
        holdings.clone()
    });
    save();
}

/// Start again: no wallet, no positions, no claims.
pub fn reset() {
    holdings().set(Holdings::default());
    state::storage_remove("holdings");
}

/// What a deposit of this much would do.
pub struct DepositQuote {
    pub assets: f64,
    pub fee: f64,
    pub shares: f64,
    pub share_price: f64,
}

pub fn quote_deposit(vault: &Vault, assets: f64) -> DepositQuote {
    let fee = assets * fee_fraction(vault);
    DepositQuote {
        assets,
        fee,
        // Deposits are valued at mark, in the same transaction.
        shares: if vault.share_price > 0.0 {
            (assets - fee) / vault.share_price
        } else {
            0.0
        },
        share_price: vault.share_price,
    }
}

/// What a withdrawal of this many shares would do.
pub struct WithdrawQuote {
    pub shares: f64,
    /// The price a leaver is valued at: the exit side of the spread.
    pub exit_price: f64,
    pub gross: f64,
    pub fee: f64,
    pub assets: f64,
    /// True when the idle balance can't cover it, so a claim is booked.
    pub books_claim: bool,
}

pub fn quote_withdraw(vault: &Vault, shares: f64) -> WithdrawQuote {
    let exit_price = vault.share_price * (1.0 - EXIT_SPREAD);
    let gross = shares * exit_price;
    let fee = gross * fee_fraction(vault);
    WithdrawQuote {
        shares,
        exit_price,
        gross,
        fee,
        assets: gross - fee,
        books_claim: gross > vault.idle,
    }
}

fn fee_fraction(vault: &Vault) -> f64 {
    f64::from(vault.fees.transaction_bps) / 10_000.0
}

/// Deposit into a vault: the fee stays in the pool, and shares are minted at
/// the value the contract computes in the same transaction.
pub fn deposit(slug: &str, assets: f64) -> Result<DepositQuote, String> {
    let Some(vault) = data::strategy(slug).and_then(Strategy::vault) else {
        return Err("That vault doesn't exist.".to_string());
    };
    if !vault.state.takes_deposits() {
        return Err(format!(
            "This vault is {} and isn't taking deposits.",
            vault.state.label()
        ));
    }
    if assets <= 0.0 {
        return Err("Enter an amount.".to_string());
    }
    if assets > holdings().lock_ref().balance {
        return Err("That's more than your balance.".to_string());
    }

    let quote = quote_deposit(vault, assets);
    holdings().replace_with(|holdings| {
        holdings.balance -= assets;
        match holdings.positions.iter_mut().find(|held| held.slug == slug) {
            Some(held) => {
                held.shares += quote.shares;
                held.cost += assets;
            }
            None => holdings.positions.push(Position {
                slug: slug.to_string(),
                shares: quote.shares,
                cost: assets,
            }),
        }
        holdings.clone()
    });
    save();
    Ok(quote)
}

/// Withdraw from a vault. If the idle balance covers it the shares are burned
/// and paid in the same transaction; if not, they are marked pending and a
/// claim is booked.
pub fn withdraw(slug: &str, shares: f64) -> Result<WithdrawQuote, String> {
    let Some(strategy) = data::strategy(slug) else {
        return Err("That vault doesn't exist.".to_string());
    };
    let Some(vault) = strategy.vault() else {
        return Err("That vault doesn't exist.".to_string());
    };
    if shares <= 0.0 {
        return Err("Enter an amount.".to_string());
    }
    let held = holdings()
        .lock_ref()
        .position(slug)
        .map_or(0.0, |held| held.shares);
    if shares > held + 1e-9 {
        return Err("You don't hold that many shares.".to_string());
    }

    let quote = quote_withdraw(vault, shares);
    let fraction = if vault.shares > 0.0 {
        shares / vault.shares
    } else {
        0.0
    };

    holdings().replace_with(|holdings| {
        if let Some(position) = holdings
            .positions
            .iter_mut()
            .find(|position| position.slug == slug)
        {
            let share_of_cost = if position.shares > 0.0 {
                position.cost * (shares / position.shares)
            } else {
                0.0
            };
            position.shares -= shares;
            position.cost -= share_of_cost;
        }
        holdings.positions.retain(|held| held.shares > 1e-9);

        if quote.books_claim {
            let id = holdings.next_claim.max(1);
            holdings.next_claim = id + 1;
            holdings.claims.push(Claim {
                id,
                slug: slug.to_string(),
                shares,
                fraction,
                state: ClaimState::Pending,
                paid: None,
            });
        } else {
            holdings.balance += quote.assets;
        }
        holdings.clone()
    });
    save();
    Ok(quote)
}

/// Drive a claim's settlement one step. Anyone can send these transactions:
/// the contract closes the leaver's fraction of every position, brings the
/// cash home, values the vault again and pays.
pub fn settle(id: u32) -> Option<ClaimState> {
    let mut reached = None;
    let mut paid_to_balance = 0.0;

    holdings().replace_with(|holdings| {
        if let Some(claim) = holdings.claims.iter_mut().find(|claim| claim.id == id) {
            let next = match claim.state {
                ClaimState::Pending => ClaimState::Closing,
                ClaimState::Closing => ClaimState::CashHome,
                ClaimState::CashHome | ClaimState::Waiting => ClaimState::Paid,
                ClaimState::Paid => ClaimState::Paid,
            };
            claim.state = next;
            reached = Some(next);

            if next == ClaimState::Paid && claim.paid.is_none() {
                // Valued again once the cash is home, at exit-side marks then.
                let assets = data::strategy(&claim.slug)
                    .and_then(Strategy::vault)
                    .map(|vault| {
                        let quote = quote_withdraw(vault, claim.shares);
                        quote.assets
                    })
                    .unwrap_or_default();
                claim.paid = Some(assets);
                paid_to_balance = assets;
            }
        }
        holdings.balance += paid_to_balance;
        holdings.clone()
    });

    save();
    reached
}

/// Forget a settled claim.
pub fn clear_claim(id: u32) {
    holdings().replace_with(|holdings| {
        holdings.claims.retain(|claim| claim.id != id);
        holdings.clone()
    });
    save();
}

/// Move a position from one version of a strategy to its successor: a
/// withdrawal from one vault and a deposit into the other, which is all a
/// move can ever be.
pub fn move_to(from: &str, to: &str) -> Result<f64, String> {
    let shares = holdings()
        .lock_ref()
        .position(from)
        .map_or(0.0, |held| held.shares);
    if shares <= 0.0 {
        return Err("You hold nothing in this vault.".to_string());
    }

    let quote = withdraw(from, shares)?;
    if quote.books_claim {
        return Err(
            "The idle balance couldn't cover it, so a claim was booked. The move finishes when \
             the claim settles."
                .to_string(),
        );
    }
    deposit(to, quote.assets)?;
    Ok(quote.assets)
}

/// What the transaction fee comes to, as a percentage, for showing in a quote.
pub fn transaction_fee_label(vault: &Vault) -> String {
    crate::format::from_bps(vault.fees.transaction_bps)
}
