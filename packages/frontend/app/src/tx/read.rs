//! Reading a vault and an account from the chain, through the contracts' own
//! bindings. Money is read here at the moment it matters (the vault's state,
//! its list, balances, allowances, the last price and the valuation
//! schedule), never taken from the snapshot.

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, SolValue};
use chain::bindings::erc20::IERC20;
use chain::bindings::vault::{self as bindings, IVault};

use super::error::TxError;
use super::port::Reader;
use crate::money::quote::{self, Fees, Valuation};

/// Ask a contract something.
pub async fn read<R: Reader, C: SolCall>(
    reader: &R,
    to: Address,
    call: C,
) -> Result<C::Return, TxError> {
    let answer = reader.call(to, Bytes::from(call.abi_encode())).await?;
    C::abi_decode_returns(&answer)
        .map_err(|_| TxError::Network("the chain answered in an unexpected shape".to_string()))
}

/// Where a vault is in its life, as the contract says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    None,
    Activating,
    Open,
    WindingDown,
    Frozen,
    Killed,
    Closed,
    Cancelled,
}

impl From<bindings::VaultState> for State {
    fn from(state: bindings::VaultState) -> Self {
        use bindings::VaultState as S;
        match state {
            S::None => Self::None,
            S::Activating => Self::Activating,
            S::Open => Self::Open,
            S::WindingDown => Self::WindingDown,
            S::Frozen => Self::Frozen,
            S::Killed => Self::Killed,
            S::Closed => Self::Closed,
            S::Cancelled => Self::Cancelled,
            _ => Self::None,
        }
    }
}

/// The timers a flow waits on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timers {
    /// Valuation slots fall on multiples of this (ten seconds).
    pub slot_interval: u64,
    /// A request is priced at the first slot at least this far ahead.
    pub slot_lead: u64,
    /// A slot's valuation counts only if recorded this soon after it.
    pub slot_window: u64,
    /// A request nothing priced for this long may take the fallback.
    pub fallback_after: u64,
}

/// The newest priced valuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Priced {
    pub index: U256,
    pub time: u64,
    pub valuation: Valuation,
}

/// A vault's figures that a quote and a flow read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultFigures {
    pub state: State,
    pub fees: Fees,
    pub timers: Timers,
    pub cash: U256,
    pub total_supply: U256,
    pub minimum_deposit: U256,
    pub last: Option<Priced>,
    /// Halted, and a valuation since found the stations empty: no new
    /// request is taken, and the vault only waits to close.
    pub ready_to_close: bool,
}

impl VaultFigures {
    /// The valuation a quote is made on: the newest priced one, or before
    /// any, the vault as it stands at a dollar a share.
    pub fn basis(&self) -> Valuation {
        match &self.last {
            Some(priced) => priced.valuation,
            None => {
                let gross = self.cash.saturating_add(U256::from(1u8));
                Valuation {
                    gross,
                    gross_exit: gross,
                    supply: self.total_supply.saturating_add(U256::from(quote::OFFSET)),
                    fee: U256::ZERO,
                    fee_exit: U256::ZERO,
                }
            }
        }
    }

    /// What depositing `assets` would buy, at the last price.
    pub fn deposit_quote(&self, assets: U256) -> Option<quote::DepositQuote> {
        let pool = match &self.last {
            Some(priced) => quote::DepositPool::at(&priced.valuation)?,
            None => quote::DepositPool::unpriced(self.cash, self.total_supply),
        };
        quote::deposit(assets, self.fees.transaction_bps, pool)
    }

    /// What withdrawing `shares` would pay, at the last exit price, and
    /// whether the cash covers it. A closed vault pays its fraction of the
    /// cash, with no fee.
    pub fn exit_quote(&self, shares: U256) -> Option<(quote::ExitQuote, quote::ExitPath)> {
        if self.state == State::Closed {
            let payout = quote::closed(shares, self.cash, self.total_supply)?;
            let exit = quote::ExitQuote {
                shares,
                slice: U256::ZERO,
                burned: shares,
                gross: payout,
                fee: U256::ZERO,
                payout,
            };
            return Some((exit, quote::ExitPath::Cash));
        }
        let exit = quote::exit(shares, self.fees.transaction_bps, &self.basis())?;
        let path = quote::exit_path(&exit, self.cash);
        Some((exit, path))
    }

    /// The fewest shares that pay at least `assets` on the way out.
    pub fn shares_for(&self, assets: U256) -> Option<U256> {
        quote::shares_for(assets, self.fees.transaction_bps, &self.basis())
    }

    /// The share price the vault reports.
    pub fn share_price(&self) -> Option<U256> {
        quote::share_price(self.last.as_ref().map(|priced| &priced.valuation))
    }
}

pub async fn state<R: Reader>(reader: &R, vault: Address) -> Result<State, TxError> {
    Ok(read(reader, vault, IVault::stateCall {}).await?.into())
}

pub async fn last_priced<R: Reader>(reader: &R, vault: Address) -> Result<Option<Priced>, TxError> {
    let last = read(reader, vault, IVault::lastPricedCall {}).await?;
    if !last.priced {
        return Ok(None);
    }
    let v = read(reader, vault, IVault::valuationAtCall { index: last.index }).await?;
    Ok(Some(Priced {
        index: last.index,
        time: last.time,
        valuation: Valuation {
            gross: v.gross,
            gross_exit: v.grossExit,
            supply: v.supply,
            fee: v.fee,
            fee_exit: v.feeExit,
        },
    }))
}

pub async fn figures<R: Reader>(reader: &R, vault: Address) -> Result<VaultFigures, TxError> {
    let state = state(reader, vault).await?;
    let fees = read(reader, vault, IVault::feesCall {}).await?;
    let timers = read(reader, vault, IVault::timersCall {}).await?;
    let cash = read(reader, vault, IVault::cashCall {}).await?;
    let total_supply = read(reader, vault, IVault::totalSupplyCall {}).await?;
    let minimum_deposit = read(reader, vault, IVault::minimumDepositCall {}).await?;
    let ready_to_close = read(reader, vault, IVault::readyToCloseCall {}).await?;
    let last = last_priced(reader, vault).await?;
    Ok(VaultFigures {
        state,
        fees: Fees {
            holding_bps: fees.holdingBps,
            author_bps: fees.authorBps,
            platform_bps: fees.platformBps,
            transaction_bps: fees.transactionBps,
        },
        timers: Timers {
            slot_interval: timers.slotInterval,
            slot_lead: timers.slotLead,
            slot_window: timers.slotWindow,
            fallback_after: timers.fallbackAfter,
        },
        cash,
        total_supply,
        minimum_deposit,
        last,
        ready_to_close,
    })
}

/// What one account holds of a vault and its dollar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holdings {
    /// The dollar, in the wallet, on the vault's chain.
    pub balance: U256,
    pub shares: U256,
    /// The fallback's units: the fraction of the stations held (scaled by
    /// 1e18), and the dollars they have earned and can be collected.
    pub units: U256,
    pub units_claimable: U256,
    /// Payouts the dollar refused to deliver, held for collecting.
    pub held: U256,
}

pub async fn holdings<R: Reader>(
    reader: &R,
    vault: Address,
    asset: Address,
    who: Address,
) -> Result<Holdings, TxError> {
    let balance = read(reader, asset, IERC20::balanceOfCall { account: who }).await?;
    let shares = read(reader, vault, IVault::balanceOfCall { owner: who }).await?;
    let units = read(reader, vault, IVault::unitsOfCall { holder: who }).await?;
    let held = read(reader, vault, IVault::heldForCall { owner: who }).await?;
    Ok(Holdings {
        balance,
        shares,
        units: units.units,
        units_claimable: units.claimable,
        held,
    })
}

pub async fn asset_balance<R: Reader>(
    reader: &R,
    asset: Address,
    who: Address,
) -> Result<U256, TxError> {
    read(reader, asset, IERC20::balanceOfCall { account: who }).await
}

pub async fn allowance<R: Reader>(
    reader: &R,
    asset: Address,
    owner: Address,
    spender: Address,
) -> Result<U256, TxError> {
    read(reader, asset, IERC20::allowanceCall { owner, spender }).await
}

/// Whether `depositor` has signed for the vault's list as it stands now: the
/// vault records the hash of the list each statement was signed for, and
/// takes a deposit only when that is the list's hash today.
pub async fn has_statement<R: Reader>(
    reader: &R,
    vault: Address,
    depositor: Address,
) -> Result<bool, TxError> {
    let recorded = read(reader, vault, IVault::statementOfCall { depositor }).await?;
    let list = read(reader, vault, IVault::restrictedCall {}).await?;
    Ok(recorded == restricted_hash(&list))
}

/// The list's hash as the vault keeps it: `keccak256(abi.encode(list))`.
pub fn restricted_hash(list: &[B256]) -> B256 {
    keccak256(list.to_vec().abi_encode())
}

/// One request, as the vault holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub owner: Address,
    pub earliest: u64,
    pub done: bool,
    pub amount: U256,
}

pub async fn request<R: Reader>(reader: &R, vault: Address, id: U256) -> Result<Request, TxError> {
    let request = read(reader, vault, IVault::requestAtCall { requestId: id }).await?;
    Ok(Request {
        owner: request.owner,
        earliest: request.earliest,
        done: request.done,
        amount: request.amount,
    })
}

/// Where a claim is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimState {
    None,
    Pending,
    Waiting,
    Queued,
    Paid,
    Fallback,
    Closing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    pub owner: Address,
    pub shares: U256,
    pub booked_at: u64,
    pub last_attempt_at: u64,
    pub state: ClaimState,
    pub paid: U256,
}

pub async fn claim<R: Reader>(reader: &R, vault: Address, id: U256) -> Result<Claim, TxError> {
    use bindings::ClaimState as S;
    let claim = read(reader, vault, IVault::claimAtCall { claimId: id }).await?;
    Ok(Claim {
        owner: claim.owner,
        shares: claim.shares,
        booked_at: claim.bookedAt,
        last_attempt_at: claim.lastAttemptAt,
        state: match claim.state {
            S::Pending => ClaimState::Pending,
            S::Waiting => ClaimState::Waiting,
            S::Queued => ClaimState::Queued,
            S::Paid => ClaimState::Paid,
            S::Fallback => ClaimState::Fallback,
            S::Closing => ClaimState::Closing,
            _ => ClaimState::None,
        },
        paid: claim.paid,
    })
}

/// The slot a request made at chain time `now` is priced at, as the vault
/// queues it (`nextSlot`): the first multiple of the interval at least
/// `slot_lead` ahead.
pub fn next_slot(now: u64, timers: &Timers) -> u64 {
    let interval = timers.slot_interval.max(1);
    (now + timers.slot_lead).div_ceil(interval) * interval
}

/// The slot a request with this `earliest` can still be priced at, at chain
/// time `now`: its own slot while that slot's window is open, and after a
/// slot passes unrecorded, the next one whose window hasn't closed.
pub fn live_slot(earliest: u64, now: u64, timers: &Timers) -> u64 {
    let interval = timers.slot_interval.max(1);
    if now <= earliest + timers.slot_window {
        return earliest;
    }
    // The first slot `earliest + k × interval` whose window is still open.
    let late = now - timers.slot_window - earliest;
    earliest + late.div_ceil(interval) * interval
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timers() -> Timers {
        Timers {
            slot_interval: 10,
            slot_lead: 5,
            slot_window: 3,
            fallback_after: 86_400,
        }
    }

    #[test]
    fn a_request_is_priced_at_the_first_ten_second_mark_at_least_five_seconds_away() {
        // 12:00:03 → 12:00:10; 12:00:06 → 12:00:20; 12:00:05 → 12:00:10.
        assert_eq!(next_slot(3, &timers()), 10);
        assert_eq!(next_slot(6, &timers()), 20);
        assert_eq!(next_slot(5, &timers()), 10);
        assert_eq!(next_slot(14_32_33, &timers()), 14_32_40);
    }

    #[test]
    fn a_missed_slot_moves_the_request_to_the_next_open_one() {
        // Its own slot, while the window is open.
        assert_eq!(live_slot(40, 30, &timers()), 40);
        assert_eq!(live_slot(40, 43, &timers()), 40);
        // Past the window: the next slot.
        assert_eq!(live_slot(40, 44, &timers()), 50);
        assert_eq!(live_slot(40, 53, &timers()), 50);
        assert_eq!(live_slot(40, 54, &timers()), 60);
    }

    fn figures(state: State, last: Option<Priced>) -> VaultFigures {
        VaultFigures {
            state,
            fees: Fees {
                transaction_bps: 10,
                ..Fees::default()
            },
            timers: timers(),
            cash: U256::from(1_940_000_000u64),
            total_supply: U256::from(10_000u128 * quote::WAD),
            minimum_deposit: U256::from(10_000_000u64),
            last,
            ready_to_close: false,
        }
    }

    #[test]
    fn before_any_valuation_a_quote_is_at_a_dollar_a_share() {
        // Activated with a cent of dead assets, and their dead shares.
        let mut figures = figures(State::Open, None);
        figures.cash = U256::from(10_000u64);
        figures.total_supply = U256::from(10_000u64) * U256::from(quote::OFFSET);
        assert_eq!(figures.share_price(), Some(U256::from(1_000_000u64)));
        let quote = figures.deposit_quote(U256::from(100_000_000u64)).unwrap();
        assert_eq!(quote.fee, U256::from(100_000u64));
        // 99.9 dollars' worth of shares, give or take the dead shares' cent.
        assert!(quote.shares > U256::from(99u128 * quote::WAD));
        assert!(quote.shares < U256::from(100u128 * quote::WAD));
    }

    #[test]
    fn a_withdrawal_is_booked_once_its_payout_passes_the_cash_at_home() {
        let valuation = Valuation {
            gross: U256::from(10_000_000_001u64),
            gross_exit: U256::from(10_000_000_001u64),
            supply: U256::from(10_000u128 * quote::WAD) + U256::from(quote::OFFSET),
            fee: U256::ZERO,
            fee_exit: U256::ZERO,
        };
        let figures = figures(
            State::Open,
            Some(Priced {
                index: U256::ZERO,
                time: 10,
                valuation,
            }),
        );
        let (_, path) = figures
            .exit_quote(U256::from(1_000u128 * quote::WAD))
            .unwrap();
        assert_eq!(path, quote::ExitPath::Cash);
        let (_, path) = figures
            .exit_quote(U256::from(2_400u128 * quote::WAD))
            .unwrap();
        assert_eq!(path, quote::ExitPath::Booked);
    }

    #[test]
    fn a_closed_vault_quotes_its_fraction_of_the_cash_with_no_fee() {
        let figures = figures(State::Closed, None);
        let (exit, path) = figures
            .exit_quote(U256::from(5_000u128 * quote::WAD))
            .unwrap();
        assert_eq!(path, quote::ExitPath::Cash);
        assert_eq!(exit.fee, U256::ZERO);
        assert_eq!(exit.payout, U256::from(970_000_000u64));
    }

    #[test]
    fn the_lists_hash_is_the_vaults_abi_encoding_of_it() {
        // The empty list is keccak256(abi.encode(bytes32[]())): an offset and
        // a zero length.
        let mut encoded = vec![0u8; 64];
        encoded[31] = 0x20;
        assert_eq!(restricted_hash(&[]), keccak256(&encoded));
        let one = B256::repeat_byte(1);
        let mut encoded = vec![0u8; 96];
        encoded[31] = 0x20;
        encoded[63] = 1;
        encoded[64..].copy_from_slice(one.as_slice());
        assert_eq!(restricted_hash(&[one]), keccak256(&encoded));
    }
}
