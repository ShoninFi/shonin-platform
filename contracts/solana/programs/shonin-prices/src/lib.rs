//! The published price list, and its history.
//!
//! A vault caps what its tank can be charged at the author's allowances times
//! these prices, over the time that passed. Because the cap is worked out over
//! the history rather than at today's prices, a price change never reaches
//! back, and because a change waits out its notice, nobody's costs move
//! without warning. The admin's one power is to schedule a change, which
//! takes effect only after the notice; the notice itself is fixed.
//!
//! The history is one account that only grows, its periods fixed-size and in
//! order, so a cost over a recent interval reads only the periods it touches
//! (see `read`).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, program::set_return_data, system_instruction};
use shonin_common::{loader, math::U256, time::now, DAY};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

pub mod read;

/// The price list: one per deployment.
pub const PRICES_SEED: &[u8] = b"prices";

/// What each unit costs, in dollars scaled by 1e18, so that a fraction of a
/// cent per second of inference survives.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnitPrices {
    pub vcpu_hour: u128,
    pub gib_hour: u128,
    pub gb_egress: u128,
    pub inference_second: u128,
}

/// One entry in the price history: these prices, from this time on.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PricePeriod {
    pub effective_from: u64,
    pub prices: UnitPrices,
}

/// The size of one period, Borsh-encoded.
pub const PERIOD_SIZE: usize = 8 + 4 * 16;

/// The most an agent may use, set by its author on the live vault. The vault
/// only believes a usage bill up to these allowances at the published prices.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowances {
    /// Wasmtime fuel a second; a million a second stands for one core.
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    /// Enforced by the executor, not priced.
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    /// Enforced by the executor, not priced.
    pub streams: u32,
    pub inference_ms_per_min: u32,
    /// Gas, in the dollar's units (6 decimals) a day, charged at face value.
    pub gas_per_day: u64,
}

/// The price list. Laid out by Borsh: the discriminator, the admin, the
/// notice, the bump, then the history as a length and fixed-size periods in
/// order of `effective_from`.
#[account]
pub struct PriceList {
    pub admin: Pubkey,
    pub notice: u64,
    pub bump: u8,
    pub history: Vec<PricePeriod>,
}

impl PriceList {
    /// Where the history's length sits.
    pub const LENGTH_AT: usize = 8 + 32 + 8 + 1;
    /// Where the first period starts.
    pub const PERIODS_AT: usize = Self::LENGTH_AT + 4;

    /// The account size holding `periods` periods.
    pub fn space(periods: usize) -> usize {
        Self::PERIODS_AT + periods * PERIOD_SIZE
    }
}

#[program]
pub mod shonin_prices {
    use super::*;

    /// Sets a fresh deployment up, once, signed by the key that deployed it:
    /// the admin, the notice every change waits out, and the first prices,
    /// in force from the beginning.
    pub fn initialize(
        ctx: Context<Initialize>,
        admin: Pubkey,
        notice: u64,
        initial: UnitPrices,
    ) -> Result<()> {
        require!(
            loader::is_upgrade_authority(
                &ctx.accounts.program,
                Some(&ctx.accounts.program_data),
                ctx.accounts.deployer.key,
            ),
            PricesError::NotTheDeployer
        );
        ctx.accounts.prices.set_inner(PriceList {
            admin,
            notice,
            bump: ctx.bumps.prices,
            history: vec![PricePeriod {
                effective_from: 0,
                prices: initial,
            }],
        });
        emit!(PricesScheduled {
            effective_from: 0,
            prices: initial,
        });
        Ok(())
    }

    /// Admin only. Takes effect after the notice period. With a fixed
    /// notice, every later schedule starts no earlier than an earlier one, so
    /// the history stays in order.
    pub fn schedule(ctx: Context<Schedule>, prices: UnitPrices) -> Result<()> {
        let list = &ctx.accounts.prices;
        let (admin, notice, length, last) = {
            let data = list.try_borrow_data()?;
            let history = read::History::new(list.key, list.owner, &data)?;
            (
                history.admin(),
                history.notice(),
                history.len(),
                history.effective_from(history.len() - 1),
            )
        };
        require_keys_eq!(admin, ctx.accounts.admin.key(), PricesError::NotAdmin);
        let effective_from = now()?.checked_add(notice).ok_or(PricesError::Overflow)?;
        require!(effective_from >= last, PricesError::Backwards);

        // One more period, paid for by the admin.
        let needed = PriceList::space(length + 1);
        let rent = Rent::get()?.minimum_balance(needed);
        let short = rent.saturating_sub(list.lamports());
        if short > 0 {
            invoke(
                &system_instruction::transfer(ctx.accounts.admin.key, list.key, short),
                &[
                    ctx.accounts.admin.to_account_info(),
                    list.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
        }
        list.resize(needed)?;
        let period = PricePeriod {
            effective_from,
            prices,
        };
        let mut data = list.try_borrow_mut_data()?;
        let at = PriceList::space(length);
        period.serialize(&mut &mut data[at..at + PERIOD_SIZE])?;
        let count = u32::try_from(length + 1).map_err(|_| PricesError::Overflow)?;
        data[PriceList::LENGTH_AT..PriceList::PERIODS_AT].copy_from_slice(&count.to_le_bytes());
        emit!(PricesScheduled {
            effective_from,
            prices,
        });
        Ok(())
    }

    /// The most the allowances could cost between two times, in the dollar's
    /// units, as a 256-bit little-endian number in the return data. A view,
    /// for callers that would rather ask than read the account.
    pub fn cost_of(ctx: Context<View>, allowances: Allowances, from: u64, to: u64) -> Result<()> {
        let list = &ctx.accounts.prices;
        let data = list.try_borrow_data()?;
        let history = read::History::new(list.key, list.owner, &data)?;
        set_return_data(&history.cost_of(&allowances, from, to)?.to_le_bytes());
        Ok(())
    }

    /// A day at the allowances, at the prices in force now, in the dollar's
    /// units, as a 256-bit little-endian number in the return data.
    pub fn daily_cost(ctx: Context<View>, allowances: Allowances) -> Result<()> {
        let list = &ctx.accounts.prices;
        let data = list.try_borrow_data()?;
        let history = read::History::new(list.key, list.owner, &data)?;
        set_return_data(&history.daily_cost(&allowances, now()?)?.to_le_bytes());
        Ok(())
    }
}

/// What the allowances cost over `seconds` at `prices`, in dollars scaled by
/// 1e18. Each term rounds down.
pub fn cost(a: &Allowances, p: &UnitPrices, seconds: u64) -> U256 {
    let s = U256::from(seconds);
    let hour = U256::from(3600u64);
    // A million units of fuel a second is one core, until the fuel-to-
    // processor mapping is measured.
    let cores = U256::from(a.fuel_per_sec) * U256::from(p.vcpu_hour) * s
        / (U256::from(1_000_000u64) * hour);
    let memory =
        U256::from(a.memory_mib) * U256::from(p.gib_hour) * s / (U256::from(1024u64) * hour);
    let egress = U256::from(a.bytes_per_day) * U256::from(p.gb_egress) * s
        / (U256::from(1_000_000_000u64) * U256::from(DAY));
    // Milliseconds of inference a minute, as seconds of inference a second.
    let inference = U256::from(a.inference_ms_per_min) * U256::from(p.inference_second) * s
        / U256::from(60_000u64);
    let gas = U256::from(a.gas_per_day) * U256::from(read::TO_ASSET) * s / U256::from(DAY);
    cores + memory + egress + inference + gas
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,
    #[account(
        init,
        payer = deployer,
        space = PriceList::space(1),
        seeds = [PRICES_SEED],
        bump,
    )]
    pub prices: Account<'info, PriceList>,
    /// CHECK: this program's own account, whose upgrade authority is read.
    #[account(address = crate::ID)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: checked against `program` by `loader::is_upgrade_authority`.
    pub program_data: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Schedule<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: the price list, read and appended to by `read::History`.
    #[account(mut, seeds = [PRICES_SEED], bump)]
    pub prices: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct View<'info> {
    /// CHECK: the price list, read by `read::History`.
    #[account(seeds = [PRICES_SEED], bump)]
    pub prices: UncheckedAccount<'info>,
}

#[event]
pub struct PricesScheduled {
    pub effective_from: u64,
    pub prices: UnitPrices,
}

#[error_code]
pub enum PricesError {
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("Only the admin may schedule prices")]
    NotAdmin,
    #[msg("A change can't start before one already scheduled")]
    Backwards,
    #[msg("Not the price list")]
    NotThePriceList,
    #[msg("Arithmetic overflow")]
    Overflow,
}
