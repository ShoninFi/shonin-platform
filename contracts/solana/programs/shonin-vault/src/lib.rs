//! Pooled capital behind one strategy, on Solana, and the factory that
//! creates a vault for each: the counterpart of the EVM `VaultFactory` and
//! `Vault` for a vault that trades on its own chain only.
//!
//! A vault holds shares, cash, the tank, requests and claims, and never
//! trades: its station does. What it is worth comes from its own ledger
//! (cash, escrow, the tank, the unit holders' reserve, payouts held) and its
//! station's valuation, never from its dollar account's balance, so money
//! sent to it by anyone else counts for nothing. Deposits and withdrawals are
//! requests priced at a later valuation, so nobody who moves can pick the
//! price they move at. Fees are measured at every valuation and paid on the
//! first one on or after Monday.
//!
//! Where the EVM vault keeps mappings and arrays, this program keeps one
//! account per entry: a holder's shares (`["holder", vault, owner]`), each
//! request (`["request", vault, maker, nonce]`, its id kept in it) and each
//! claim (`["claim", vault, id]`). Whoever makes one pays its rent and is refunded it when it is
//! closed, the refund address written in the account. The work is split into
//! modules as the EVM vault is split into libraries: `valuations` (requests,
//! valuations, settlement, the fallback, units, moving money to and from the
//! station), `claims`, `upkeep` (statements, the tank, freezes, the
//! lifecycle, the closed vault's doors) and `factory`; `ledger` is the
//! bookkeeping they share.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;

pub mod claims;
pub mod codes;
pub mod contexts;
pub mod error;
pub mod events;
pub mod factory;
pub mod ledger;
pub mod maths;
pub mod state;
pub mod station;
pub mod upkeep;
pub mod valuations;

pub use contexts::*;

/// The station's allocator: it grows as far as the transaction's heap frame
/// reaches and gives back the most recent allocation, so a settlement of
/// many requests, each paying out through a call of the token program, never
/// runs out of the default 32 KiB that never frees. The vault's callers ask
/// for the heap the station's do.
#[cfg(all(
    target_os = "solana",
    feature = "custom-heap",
    not(feature = "no-entrypoint")
))]
#[global_allocator]
static HEAP: shonin_station::heap::Heap = shonin_station::heap::Heap;
pub use factory::{ActivateArgs, InitializeArgs};
pub use upkeep::{HeldIn, Usage};

use shonin_station::state::vault::VaultView;
use state::{HaltReason, Terms};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

#[program]
pub mod shonin_vault {
    use super::*;

    // The factory -------------------------------------------------------------

    /// Sets a fresh deployment up, once, signed by the key that deployed it:
    /// the admin, the platform, the dollar, this chain's name, the terms, and
    /// the program this one replaces.
    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        factory::initialize(ctx, args)
    }

    /// The admin changes the terms new vaults are created with.
    pub fn set_terms(ctx: Context<SetTerms>, terms: Terms) -> Result<()> {
        factory::set_terms(ctx, terms)
    }

    /// The author activates a registered, reviewed strategy: its vault, its
    /// station and every account they keep, in one transaction.
    pub fn activate<'info>(
        ctx: Context<'_, '_, 'info, 'info, Activate<'info>>,
        args: ActivateArgs,
    ) -> Result<()> {
        factory::activate(ctx, args)
    }

    // What the station asks -----------------------------------------------------

    /// When the vault halted, how long its executor has to wind down, and
    /// whether a priced valuation still has requests to settle: answered from
    /// the vault's own account, with nothing written.
    pub fn station_view(ctx: Context<StationView>) -> Result<()> {
        let vault = &ctx.accounts.vault;
        let view = VaultView {
            halted_at: vault.halted_at,
            halt_grace: vault.timers.halt_grace,
            unsettled: vault.owed(),
        };
        set_return_data(&view.try_to_vec()?);
        Ok(())
    }

    // Depositors ----------------------------------------------------------------

    pub fn record_statement(ctx: Context<RecordStatement>, statement: [u8; 32]) -> Result<()> {
        upkeep::record_statement(ctx, statement)
    }

    pub fn refresh_restricted<'info>(
        ctx: Context<'_, '_, '_, 'info, RefreshRestricted<'info>>,
        drivers: Vec<u8>,
    ) -> Result<()> {
        upkeep::refresh_restricted(ctx, drivers)
    }

    pub fn request_deposit<'info>(
        ctx: Context<'_, '_, '_, 'info, RequestDeposit<'info>>,
        assets: u64,
        receiver: Pubkey,
        statement: Option<[u8; 32]>,
        nonce: u64,
    ) -> Result<()> {
        valuations::request_deposit(ctx, assets, receiver, statement, nonce)
    }

    pub fn request_redeem<'info>(
        ctx: Context<'_, '_, '_, 'info, RequestRedeem<'info>>,
        shares: u128,
        receiver: Pubkey,
        nonce: u64,
    ) -> Result<()> {
        valuations::request_redeem(ctx, shares, receiver, nonce)
    }

    pub fn record_valuation<'info>(
        ctx: Context<'_, '_, '_, 'info, RecordValuation<'info>>,
        groups: Vec<u8>,
    ) -> Result<()> {
        valuations::record_valuation(ctx, groups)
    }

    pub fn settle<'info>(
        ctx: Context<'_, '_, 'info, 'info, Settle<'info>>,
        from: u64,
    ) -> Result<()> {
        valuations::settle(ctx, from)
    }

    pub fn settle_claim<'info>(
        ctx: Context<'_, '_, 'info, 'info, SettleClaim<'info>>,
        claim_id: u64,
        groups: Vec<u8>,
    ) -> Result<()> {
        claims::settle_claim(ctx, claim_id, groups)
    }

    pub fn claim_instead<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimInstead<'info>>,
        request_id: u64,
    ) -> Result<()> {
        claims::claim_instead(ctx, request_id)
    }

    pub fn freeze(ctx: Context<Freeze>, claim_id: u64) -> Result<()> {
        upkeep::freeze(ctx, claim_id)
    }

    pub fn freeze_units(ctx: Context<FreezeUnits>) -> Result<()> {
        upkeep::freeze_units(ctx)
    }

    pub fn take_pro_rata<'info>(
        ctx: Context<'_, '_, '_, 'info, TakeProRata<'info>>,
        request_id: u64,
        groups: Vec<u8>,
    ) -> Result<()> {
        valuations::take_pro_rata(ctx, request_id, groups)
    }

    pub fn take_pro_rata_claim<'info>(
        ctx: Context<'_, '_, '_, 'info, TakeProRataClaim<'info>>,
        claim_id: u64,
    ) -> Result<()> {
        claims::take_pro_rata_claim(ctx, claim_id)
    }

    pub fn serve_pro_rata<'info>(
        ctx: Context<'_, '_, 'info, 'info, ServeProRata<'info>>,
        owner: Pubkey,
        groups: Vec<u8>,
    ) -> Result<()> {
        valuations::serve_pro_rata(ctx, owner, groups)
    }

    pub fn collect_units<'info>(
        ctx: Context<'_, '_, '_, 'info, CollectUnits<'info>>,
    ) -> Result<()> {
        valuations::collect_units(ctx)
    }

    pub fn redeem_closed<'info>(
        ctx: Context<'_, '_, '_, 'info, RedeemClosed<'info>>,
        shares: u128,
        receiver: Pubkey,
    ) -> Result<()> {
        upkeep::redeem_closed(ctx, shares, receiver)
    }

    pub fn claim_closed<'info>(
        ctx: Context<'_, '_, '_, 'info, ClosedDoor<'info>>,
        claim_id: u64,
    ) -> Result<()> {
        upkeep::claim_closed(ctx, claim_id)
    }

    pub fn settle_closed<'info>(
        ctx: Context<'_, '_, '_, 'info, ClosedDoor<'info>>,
        request_id: u64,
    ) -> Result<()> {
        upkeep::settle_closed(ctx, request_id)
    }

    pub fn collect_held<'info>(
        ctx: Context<'_, '_, '_, 'info, CollectHeld<'info>>,
        held: HeldIn,
    ) -> Result<()> {
        upkeep::collect_held(ctx, held)
    }

    pub fn close_request<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseRecord<'info>>,
        request_id: u64,
    ) -> Result<()> {
        upkeep::close_request(ctx, request_id)
    }

    pub fn close_claim<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseRecord<'info>>,
        claim_id: u64,
    ) -> Result<()> {
        upkeep::close_claim(ctx, claim_id)
    }

    pub fn close_holder(ctx: Context<CloseHolder>) -> Result<()> {
        upkeep::close_holder(ctx)
    }

    // The station ---------------------------------------------------------------

    pub fn to_station<'info>(
        ctx: Context<'_, '_, '_, 'info, Move<'info>>,
        amount: u64,
    ) -> Result<()> {
        valuations::to_station(ctx, amount)
    }

    pub fn from_station<'info>(
        ctx: Context<'_, '_, '_, 'info, Move<'info>>,
        amount: u64,
    ) -> Result<()> {
        valuations::from_station(ctx, amount)
    }

    // The tank ------------------------------------------------------------------

    pub fn fund_tank(ctx: Context<FundTank>, assets: u64) -> Result<()> {
        upkeep::fund_tank(ctx, assets)
    }

    pub fn keep<'info>(
        ctx: Context<'_, '_, '_, 'info, Keep<'info>>,
        usage: Option<Usage>,
    ) -> Result<()> {
        upkeep::keep(ctx, usage)
    }

    pub fn withdraw_tank<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawTank<'info>>,
        assets: u64,
        usage: Usage,
    ) -> Result<()> {
        upkeep::withdraw_tank(ctx, assets, usage)
    }

    pub fn request_tank_withdrawal(ctx: Context<Author>) -> Result<()> {
        upkeep::request_tank_withdrawal(ctx)
    }

    pub fn withdraw_tank_unsigned<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawTankUnsigned<'info>>,
        assets: u64,
    ) -> Result<()> {
        upkeep::withdraw_tank_unsigned(ctx, assets)
    }

    pub fn top_up_from_shares<'info>(
        ctx: Context<'_, '_, '_, 'info, TopUpFromShares<'info>>,
        shares: u128,
        nonce: u64,
    ) -> Result<()> {
        upkeep::top_up_from_shares(ctx, shares, nonce)
    }

    pub fn set_tank_keep(ctx: Context<Author>, assets: u64) -> Result<()> {
        upkeep::set_tank_keep(ctx, assets)
    }

    pub fn set_allowances(
        ctx: Context<SetAllowances>,
        allowances: shonin_prices::Allowances,
    ) -> Result<()> {
        upkeep::set_allowances(ctx, allowances)
    }

    // The author and the admin ----------------------------------------------------

    pub fn set_idle_floor(ctx: Context<Author>, bps: u16) -> Result<()> {
        upkeep::set_idle_floor(ctx, bps)
    }

    pub fn retire(ctx: Context<Author>) -> Result<()> {
        upkeep::retire(ctx)
    }

    pub fn kill(ctx: Context<Admin>) -> Result<()> {
        upkeep::kill(ctx, HaltReason::Killed)
    }

    /// The root key is lost: stop, so positions are unwound and everyone is
    /// paid while the machines that hold the keys still run.
    pub fn wind_down_sealed(ctx: Context<Admin>) -> Result<()> {
        upkeep::kill(ctx, HaltReason::Sealed)
    }

    // The lifecycle, open to anyone -------------------------------------------------

    pub fn evict(ctx: Context<Anyone>) -> Result<()> {
        upkeep::evict(ctx)
    }

    pub fn close(ctx: Context<CloseVault>) -> Result<()> {
        upkeep::close(ctx)
    }
}
