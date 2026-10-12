//! For tests only: the stand-in venue's driver for the third copy of the
//! stand-in venue (`shonin-local-venue-3`).
//!
//! It is the stand-in driver's own logic pointed at that venue's address, so
//! a station can be given a driver per venue. Its accounts are laid out as
//! the stand-in driver's are.

use anchor_lang::prelude::*;
use shonin_local_driver::{answer, logic};
use shonin_local_venue_3 as venue;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

#[program]
pub mod shonin_local_driver_3 {
    use super::*;

    pub fn allowlist(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::allowlist(&venue::ID))
    }

    pub fn restricted(_ctx: Context<Empty>) -> Result<()> {
        answer(&Vec::<[u8; 32]>::new())
    }

    pub fn describe(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::description())
    }

    pub fn value<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::valuation(&book, band_bps)?)
    }

    pub fn close<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        fraction: u64,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::closing(&book, fraction, band_bps, 2)?)
    }

    /// A stopped station's unwind of everything: the stand-in sells it all
    /// in one transaction, so the same as a close of it all.
    pub fn unwind<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        close(ctx, shonin_driver::WAD, band_bps)
    }

    pub fn sweep<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        amount: u64,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::sweep(&book, amount, 2))
    }
}

#[derive(Accounts)]
pub struct Empty {}

#[derive(Accounts)]
pub struct Read<'info> {
    /// CHECK: the station's authority at the venue, whose account there is
    /// derived from its address.
    pub authority: UncheckedAccount<'info>,
    /// CHECK: the authority's dollar account, where a sweep sends the
    /// dollars; the venue itself checks the authority owns it.
    pub authority_dollars: UncheckedAccount<'info>,
}
