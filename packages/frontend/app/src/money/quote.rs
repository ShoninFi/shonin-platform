//! What a deposit or a withdrawal comes to, worked out exactly as the vault
//! contract works it out: the same integer steps, rounded the same way, on
//! the same figures. A request is priced at a later valuation, so a quote made
//! on the last priced one is an estimate of the price; the arithmetic on that
//! price is the contract's own.
//!
//! Amounts are base units: the dollar's (six decimals on every chain today)
//! and the share's (eighteen).

use alloy_primitives::U256;

use super::units::{mul_div, mul_div_up};

/// Basis points in one.
pub const BPS: u64 = 10_000;
/// The virtual shares a vault counts beside its real ones, so a share starts
/// at a dollar and the first deposit can't be rounded away.
pub const OFFSET: u64 = 1_000_000_000_000;
/// One whole share, in share units.
pub const WAD: u128 = 1_000_000_000_000_000_000;

fn bps(value: u16) -> U256 {
    U256::from(value)
}

fn base() -> U256 {
    U256::from(BPS)
}

/// The fee schedule a vault keeps for life, as `fees()` returns it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fees {
    pub holding_bps: u16,
    pub author_bps: u16,
    pub platform_bps: u16,
    /// On every deposit and withdrawal, kept in the pool.
    pub transaction_bps: u16,
}

/// One priced valuation's snapshot, as `valuationAt` returns it: what every
/// request it settles is priced at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Valuation {
    /// Cash and every station at mark, plus one.
    pub gross: U256,
    /// The same at exit-side prices, never above `gross`.
    pub gross_exit: U256,
    /// Every share, plus the virtual offset.
    pub supply: U256,
    /// The profit fee owed on `gross`, and on `gross_exit`.
    pub fee: U256,
    pub fee_exit: U256,
}

/// The pool a slot's deposits buy into: its value net of the profit fee, and
/// its supply. At settlement the contract fixes these once the slot's exits
/// are paid; before then the last priced valuation is the best estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositPool {
    pub net: U256,
    pub supply: U256,
}

impl DepositPool {
    /// The pool as the last priced valuation found it.
    pub fn at(valuation: &Valuation) -> Option<Self> {
        Some(Self {
            net: valuation.gross.checked_sub(valuation.fee)?,
            supply: valuation.supply,
        })
    }

    /// A vault no valuation has priced yet: its cash and its supply (the dead
    /// shares included) plus the virtual offset, a dollar a share.
    pub fn unpriced(cash: U256, total_supply: U256) -> Self {
        Self {
            net: cash.saturating_add(U256::from(1u8)),
            supply: total_supply.saturating_add(U256::from(OFFSET)),
        }
    }
}

/// What a deposit of `assets` buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositQuote {
    pub assets: U256,
    /// The transaction fee, which stays in the pool: rounded up, as the
    /// contract takes it.
    pub fee: U256,
    pub shares: U256,
}

/// The deposit as `_deposit` settles it:
/// `shares = (assets − ⌈assets × fee / 10000⌉) × supply ÷ net`, rounded
/// down. `None` if the pool can't price anything (no value).
pub fn deposit(assets: U256, transaction_bps: u16, pool: DepositPool) -> Option<DepositQuote> {
    let fee = mul_div_up(assets, bps(transaction_bps), base())?;
    let paid_in = assets.checked_sub(fee)?;
    let shares = mul_div(paid_in, pool.supply, pool.net)?;
    Some(DepositQuote {
        assets,
        fee,
        shares,
    })
}

/// What `shares` come to on the way out at a valuation's exit price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitQuote {
    pub shares: U256,
    /// The slice of them that pays the profit fee they owe.
    pub slice: U256,
    /// The rest, burned.
    pub burned: U256,
    /// What the burned shares are worth at exit-side marks.
    pub gross: U256,
    /// The transaction fee on that, which stays in the pool.
    pub fee: U256,
    /// What is paid.
    pub payout: U256,
}

/// A withdrawal as `exitOf` prices it:
/// `slice = ⌈shares × feeExit ÷ grossExit⌉` (none without a fee),
/// `gross = (shares − slice) × grossExit ÷ supply` rounded down, and the
/// payout `gross − ⌈gross × fee / 10000⌉`.
pub fn exit(shares: U256, transaction_bps: u16, valuation: &Valuation) -> Option<ExitQuote> {
    let slice = if valuation.fee_exit.is_zero() {
        U256::ZERO
    } else {
        mul_div_up(shares, valuation.fee_exit, valuation.gross_exit)?
    };
    let burned = shares.checked_sub(slice)?;
    let gross = mul_div(burned, valuation.gross_exit, valuation.supply)?;
    let fee = mul_div_up(gross, bps(transaction_bps), base())?;
    let payout = gross.checked_sub(fee)?;
    Some(ExitQuote {
        shares,
        slice,
        burned,
        gross,
        fee,
        payout,
    })
}

/// Whether a withdrawal of `shares` is paid from cash or booked as a claim:
/// it is paid when its payout is no more than the cash (`_redeem`: `payout <=
/// free()`, and the free cash is the cash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitPath {
    Cash,
    Booked,
}

pub fn exit_path(quote: &ExitQuote, cash: U256) -> ExitPath {
    if quote.payout <= cash {
        ExitPath::Cash
    } else {
        ExitPath::Booked
    }
}

/// The fewest shares whose payout at a valuation's exit price, after the fee
/// slice and the transaction fee, is at least `assets`: for a withdrawal
/// typed in dollars. It starts from the contract's own estimate for a tank
/// fill (`_sharesFor`: `⌈assets × supply × 10000 ÷ ((grossExit − feeExit) ×
/// (10000 − fee))⌉`), which the separate roundings can leave a unit short,
/// and searches from there with [`exit`] itself, whose payout never falls as
/// shares rise. `None` when no number of shares pays it.
pub fn shares_for(assets: U256, transaction_bps: u16, valuation: &Valuation) -> Option<U256> {
    let per_share = valuation.gross_exit.checked_sub(valuation.fee_exit)?;
    let keep = base().checked_sub(bps(transaction_bps))?;
    if per_share.is_zero() || keep.is_zero() {
        return None;
    }
    let numerator = valuation.supply.checked_mul(base())?;
    let denominator = per_share.checked_mul(keep)?;
    let estimate = mul_div_up(assets, numerator, denominator)?;
    let pays = |shares: U256| exit(shares, transaction_bps, valuation).map(|q| q.payout >= assets);

    // An upper bound that pays: step up from the estimate, doubling.
    let mut high = estimate;
    let mut step = (estimate / U256::from(1_000_000u64)).max(U256::from(1u8));
    let mut tries = 0;
    while !pays(high)? {
        high = high.checked_add(step)?;
        step = step.checked_mul(U256::from(2u8))?;
        tries += 1;
        if tries > 256 {
            return None;
        }
    }
    // The least that pays, between nothing and that bound.
    let mut low = U256::ZERO;
    while low < high {
        let middle = low + (high - low) / U256::from(2u8);
        if pays(middle)? {
            high = middle;
        } else {
            low = middle + U256::from(1u8);
        }
    }
    Some(high)
}

/// What `shares` fetch from a closed vault (`redeemClosed`, `claimClosed`):
/// their fraction of the cash, rounded down, no fee.
pub fn closed(shares: U256, cash: U256, total_supply: U256) -> Option<U256> {
    mul_div(cash, shares, total_supply)
}

/// The fewest shares a closed vault pays at least `assets` for: the inverse
/// of [`closed`], rounded so the payout never falls short. `None` when the
/// vault has no cash or the amount is more than it holds.
pub fn closed_shares_for(assets: U256, cash: U256, total_supply: U256) -> Option<U256> {
    if cash.is_zero() || assets > cash {
        return None;
    }
    let shares = mul_div_up(assets, total_supply, cash)?;
    (closed(shares, cash, total_supply)? >= assets).then_some(shares)
}

/// The share price the vault reports (`sharePrice()`): the value net of the
/// profit fee per whole share, in the dollar's base units. A dollar before
/// any valuation.
pub fn share_price(valuation: Option<&Valuation>) -> Option<U256> {
    match valuation {
        None => Some(U256::from(1_000_000u64)),
        Some(v) => mul_div(v.gross.checked_sub(v.fee)?, U256::from(WAD), v.supply),
    }
}

/// The exit-side price per whole share, as a withdrawal is valued: before the
/// transaction fee, net of the profit fee the shares owe.
pub fn exit_price(valuation: &Valuation) -> Option<U256> {
    mul_div(
        valuation.gross_exit.checked_sub(valuation.fee_exit)?,
        U256::from(WAD),
        valuation.supply,
    )
}

/// What `shares` are worth at a price per whole share, rounded down.
pub fn value_of(shares: U256, price: U256) -> Option<U256> {
    mul_div(shares, price, U256::from(WAD))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_at_a_closed_vault_ask_for_the_fewest_shares_whose_cash_fraction_pays_them() {
        // $131,870 of cash over 100,000 shares: no fee, and no exit price.
        let cash = U256::from(131_870u64 * 1_000_000);
        let supply = U256::from(100_000u128 * 1_000_000_000_000_000_000);
        for dollars in [1u64, 250, 1_000, 131_870] {
            let assets = U256::from(dollars * 1_000_000);
            let shares = closed_shares_for(assets, cash, supply).unwrap();
            assert!(closed(shares, cash, supply).unwrap() >= assets);
            assert!(closed(shares - U256::from(1u8), cash, supply).unwrap() < assets);
        }
        assert_eq!(
            closed_shares_for(cash + U256::from(1u8), cash, supply),
            None
        );
        assert_eq!(closed_shares_for(U256::from(1u8), U256::ZERO, supply), None);
    }

    fn usd(dollars: u64) -> U256 {
        U256::from(dollars) * U256::from(1_000_000u64)
    }

    fn shares(whole: u64) -> U256 {
        U256::from(whole) * U256::from(WAD)
    }

    /// A pool at exactly a dollar a share, no profit owed: `gross` is its
    /// value plus one, its supply the real shares plus the offset.
    fn at_a_dollar(value: u64) -> Valuation {
        let gross = usd(value) + U256::from(1u8);
        let supply = usd(value) * U256::from(OFFSET) + U256::from(OFFSET);
        Valuation {
            gross,
            gross_exit: gross,
            supply,
            fee: U256::ZERO,
            fee_exit: U256::ZERO,
        }
    }

    #[test]
    fn a_deposit_at_a_dollar_buys_its_amount_less_the_fee_in_shares() {
        let v = at_a_dollar(100_000);
        let quote = deposit(usd(1_000), 10, DepositPool::at(&v).unwrap()).unwrap();
        assert_eq!(quote.fee, usd(1));
        assert_eq!(quote.shares, shares(999));
    }

    #[test]
    fn the_smallest_deposit_of_ten_dollars_pays_one_cent_and_buys_nine_ninety_nine() {
        let v = at_a_dollar(1_000);
        let quote = deposit(usd(10), 10, DepositPool::at(&v).unwrap()).unwrap();
        assert_eq!(quote.fee, U256::from(10_000u64));
        assert_eq!(quote.shares, U256::from(9_990_000u64) * U256::from(OFFSET));
    }

    #[test]
    fn the_deposit_fee_is_rounded_up_so_the_pool_never_loses_a_unit() {
        // 1 base unit at 0.1%: the fee is 0.001 units, taken as a whole one.
        let v = at_a_dollar(1_000);
        let quote = deposit(U256::from(1u8), 10, DepositPool::at(&v).unwrap()).unwrap();
        assert_eq!(quote.fee, U256::from(1u8));
        assert_eq!(quote.shares, U256::ZERO);
        // 1,999 units: 1.999 rounds up to 2.
        let quote = deposit(U256::from(1_999u64), 10, DepositPool::at(&v).unwrap()).unwrap();
        assert_eq!(quote.fee, U256::from(2u8));
    }

    #[test]
    fn deposit_shares_are_rounded_down_at_an_awkward_price() {
        // $1.1402 a share: net 1,140,200 units for 1e18 share units.
        let pool = DepositPool {
            net: U256::from(1_140_200u64),
            supply: U256::from(WAD),
        };
        let quote = deposit(usd(1_000), 10, pool).unwrap();
        // (1e9 − 1e6) × 1e18 / 1,140,200, rounded down.
        let expected = U256::from(999_000_000u64) * U256::from(WAD) / U256::from(1_140_200u64);
        assert_eq!(quote.shares, expected);
        assert!(
            quote.shares * U256::from(1_140_200u64) <= U256::from(999_000_000u64) * U256::from(WAD)
        );
        // 876.16... shares: the design's "about 876.2".
        assert_eq!(quote.shares / U256::from(WAD), U256::from(876u64));
    }

    #[test]
    fn an_unpriced_vault_sells_a_share_for_a_dollar() {
        // Activated with one cent of dead assets: 10,000 units, 10,000 × 1e12
        // dead shares.
        let cash = U256::from(10_000u64);
        let supply = cash * U256::from(OFFSET);
        let quote = deposit(usd(100), 10, DepositPool::unpriced(cash, supply)).unwrap();
        assert_eq!(quote.shares, U256::from(99_900_000u64) * U256::from(OFFSET));
        assert_eq!(share_price(None), Some(usd(1)));
    }

    #[test]
    fn a_pool_with_no_value_prices_nothing_instead_of_dividing_by_zero() {
        let pool = DepositPool {
            net: U256::ZERO,
            supply: U256::from(1u8),
        };
        assert_eq!(deposit(usd(10), 10, pool), None);
        let v = Valuation::default();
        assert_eq!(exit(shares(1), 10, &v), None);
        assert_eq!(share_price(Some(&v)), None);
        assert_eq!(shares_for(usd(1), 10, &v), None);
        assert_eq!(closed(shares(1), usd(1), U256::ZERO), None);
    }

    #[test]
    fn a_withdrawal_with_no_profit_owed_pays_its_value_less_the_fee() {
        let v = at_a_dollar(100_000);
        let quote = exit(shares(2_000), 10, &v).unwrap();
        assert_eq!(quote.slice, U256::ZERO);
        assert_eq!(quote.burned, shares(2_000));
        // 2000e18 × (1e11 + 1) / (1e12 × (1e11 + 1)): exactly $2,000.
        assert_eq!(quote.gross, usd(2_000));
        assert_eq!(quote.fee, usd(2));
        assert_eq!(quote.payout, usd(1_998));
        // One share unit less is worth less than a dollar unit: rounded down.
        let quote = exit(shares(2_000) - U256::from(1u8), 10, &v).unwrap();
        assert_eq!(quote.gross, usd(2_000) - U256::from(1u8));
    }

    #[test]
    fn a_withdrawal_owing_profit_fee_gives_up_a_slice_rounded_up() {
        // grossExit 1,100, feeExit 10: a hundred-and-tenth of the shares,
        // rounded up, pays the fee.
        let v = Valuation {
            gross: usd(1_100),
            gross_exit: usd(1_100),
            supply: shares(1_000),
            fee: usd(10),
            fee_exit: usd(10),
        };
        let quote = exit(U256::from(1_000u64), 10, &v).unwrap();
        // ⌈1000 × 10 / 1100⌉ = ⌈9.09⌉ = 10.
        assert_eq!(quote.slice, U256::from(10u8));
        assert_eq!(quote.burned, U256::from(990u64));
        let quote = exit(shares(100), 10, &v).unwrap();
        // ⌈100e18 / 110⌉
        let slice = (shares(100) + U256::from(109u8)) / U256::from(110u8);
        assert_eq!(quote.slice, slice);
        assert_eq!(
            quote.gross,
            (shares(100) - slice) * usd(1_100) / shares(1_000)
        );
        assert_eq!(
            quote.payout,
            quote.gross - (quote.gross * U256::from(10u8)).div_ceil(U256::from(BPS))
        );
    }

    #[test]
    fn a_withdrawal_is_paid_from_cash_up_to_and_including_the_cash() {
        let v = at_a_dollar(100_000);
        let quote = exit(shares(2_000), 10, &v).unwrap();
        assert_eq!(exit_path(&quote, quote.payout), ExitPath::Cash);
        assert_eq!(
            exit_path(&quote, quote.payout - U256::from(1u8)),
            ExitPath::Booked
        );
        assert_eq!(exit_path(&quote, usd(131_870)), ExitPath::Cash);
        assert_eq!(exit_path(&quote, usd(1_940)), ExitPath::Booked);
    }

    #[test]
    fn shares_typed_as_dollars_pay_at_least_those_dollars() {
        let v = Valuation {
            gross: U256::from(1_140_200_000_001u64),
            gross_exit: U256::from(1_139_700_000_001u64),
            supply: shares(1_000_000) + U256::from(OFFSET),
            fee: U256::from(3_000_000u64),
            fee_exit: U256::from(2_900_000u64),
        };
        for dollars in [10u64, 2_277, 99_999] {
            let wanted = usd(dollars);
            let needed = shares_for(wanted, 10, &v).unwrap();
            let paid = exit(needed, 10, &v).unwrap().payout;
            assert!(paid >= wanted, "{dollars}: paid {paid} < {wanted}");
            // And it is the fewest that do.
            let fewer = exit(needed - U256::from(1u8), 10, &v).unwrap().payout;
            assert!(fewer < wanted, "{dollars}: one share unit less still pays");
        }
    }

    #[test]
    fn a_closed_vault_pays_its_fraction_of_the_cash_rounded_down() {
        let cash = U256::from(1_000_000_001u64);
        let supply = shares(3);
        assert_eq!(
            closed(shares(1), cash, supply),
            Some(U256::from(333_333_333u64))
        );
        assert_eq!(closed(supply, cash, supply), Some(cash));
    }

    #[test]
    fn the_share_price_matches_the_contracts_formula() {
        let v = Valuation {
            gross: U256::from(1_140_200u64) + U256::from(1u8),
            gross_exit: U256::from(1_139_700u64),
            supply: U256::from(WAD),
            fee: U256::from(1u8),
            fee_exit: U256::ZERO,
        };
        assert_eq!(share_price(Some(&v)), Some(U256::from(1_140_200u64)));
        assert_eq!(exit_price(&v), Some(U256::from(1_139_700u64)));
        assert_eq!(
            value_of(shares(2), U256::from(1_140_200u64)),
            Some(U256::from(2_280_400u64))
        );
    }

    #[test]
    fn huge_amounts_are_refused_rather_than_wrapped() {
        let v = at_a_dollar(100_000);
        // A deposit so large its share count overflows 256 bits.
        assert_eq!(deposit(U256::MAX, 10, DepositPool::at(&v).unwrap()), None);
    }
}
