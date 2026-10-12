//! A Solana vault's figures, as the EVM pages read them: the vault's own
//! account turned into the same [`VaultFigures`] and [`Holdings`], so a
//! quote and a form work the same on either kind of chain. The share maths
//! is the same on both (the program's is a port of the contract's); the one
//! difference a quote must follow is the deposit's: on Solana a deposit also
//! pays what the station's exit side was worth above its mark, so it buys
//! into a pool worth that much more ([`deposit_quote`]).

use alloy_primitives::U256;
use chain::solana_ix::accounts::{Holder, Vault, VaultState};

use crate::money::quote::{self, DepositPool, DepositQuote, Fees, Valuation};
use crate::money::units::{mul_div, mul_div_up};
use crate::tx::read::{Holdings, Priced, State, Timers, VaultFigures};

/// `s` share units at mark `m` are worth `s * m / PRICE` dollar units.
const PRICE: u128 = 1_000_000_000_000_000_000_000_000_000_000;
const BPS: u64 = 10_000;

fn state(state: VaultState) -> State {
    match state {
        VaultState::None => State::None,
        VaultState::Open => State::Open,
        VaultState::WindingDown => State::WindingDown,
        VaultState::Frozen => State::Frozen,
        VaultState::Killed => State::Killed,
        VaultState::Closed => State::Closed,
    }
}

/// The vault's figures, from its account.
pub fn figures(vault: &Vault) -> VaultFigures {
    let latest = &vault.latest;
    VaultFigures {
        state: state(vault.state),
        fees: Fees {
            holding_bps: vault.fees.holding_bps,
            author_bps: vault.fees.author_bps,
            platform_bps: vault.fees.platform_bps,
            transaction_bps: vault.fees.transaction_bps,
        },
        timers: Timers {
            slot_interval: vault.timers.slot_interval,
            slot_lead: vault.timers.slot_lead,
            slot_window: vault.timers.slot_window,
            fallback_after: vault.timers.fallback_after,
        },
        cash: U256::from(vault.cash),
        total_supply: U256::from(vault.total_supply),
        minimum_deposit: U256::from(vault.minimum_deposit),
        last: vault.priced.then(|| Priced {
            index: U256::from(latest.index),
            time: latest.time,
            valuation: Valuation {
                gross: U256::from(latest.gross),
                gross_exit: U256::from(latest.gross_exit),
                supply: U256::from(latest.supply),
                fee: U256::from(latest.fee),
                fee_exit: U256::from(latest.fee_exit),
            },
        }),
        ready_to_close: vault.ready_to_close(),
    }
}

/// The program's `fee_of`: the profit fee owed on `gross` with `supply`
/// shares at high-water mark `mark`, never below zero.
pub fn fee_of(mark: u128, fees: &Fees, gross: U256, supply: U256) -> Option<U256> {
    let basis = mul_div_up(U256::from(mark), supply, U256::from(PRICE))?;
    if basis >= gross {
        return Some(U256::ZERO);
    }
    mul_div(
        gross - basis,
        U256::from(u32::from(fees.author_bps) + u32::from(fees.platform_bps)),
        U256::from(BPS),
    )
}

/// What depositing `assets` would buy at the last price, as the program
/// settles a deposit: into the pool the last valuation found, its exit
/// side's worth above the mark included, net of the profit fee owed on it.
/// The valuation a request is priced at is a later one, so this is an
/// estimate of the price; the arithmetic on it is the program's.
pub fn deposit_quote(vault: &Vault, assets: U256) -> Option<DepositQuote> {
    let figures = figures(vault);
    let pool = if vault.priced {
        let gross = U256::from(vault.latest.gross).checked_add(U256::from(vault.latest.extra))?;
        let supply = U256::from(vault.latest.supply);
        let fee = fee_of(vault.mark, &figures.fees, gross, supply)?;
        DepositPool {
            net: gross.checked_sub(fee)?,
            supply,
        }
    } else {
        DepositPool::unpriced(figures.cash, figures.total_supply)
    };
    quote::deposit(assets, figures.fees.transaction_bps, pool)
}

/// What one account holds of a vault and its dollar.
pub fn holdings(vault: &Vault, holder: Option<&Holder>, balance: u64) -> Holdings {
    Holdings {
        balance: U256::from(balance),
        shares: U256::from(holder.map_or(0, |h| h.shares)),
        units: U256::from(holder.map_or(0, |h| h.units)),
        units_claimable: U256::from(
            holder
                .and_then(|h| h.units_claimable(vault.per_unit))
                .unwrap_or(0),
        ),
        held: U256::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fee_is_owed_at_or_below_the_mark() {
        let fees = Fees {
            author_bps: 1000,
            platform_bps: 500,
            ..Fees::default()
        };
        // A dollar a share: mark 1e30 per share unit... the basis of 1e18
        // share units at a mark of 1e30 is 1e18 * 1e30 / 1e30.
        let supply = U256::from(1_000_000_000_000_000_000u128);
        let at_mark = U256::from(1_000_000_000_000_000_000u128);
        assert_eq!(fee_of(PRICE, &fees, at_mark, supply), Some(U256::ZERO));
        // 10% above it: 15% of the gain.
        let above = at_mark + at_mark / U256::from(10u8);
        assert_eq!(
            fee_of(PRICE, &fees, above, supply),
            Some(at_mark / U256::from(10u8) * U256::from(15u8) / U256::from(100u8))
        );
    }
}
