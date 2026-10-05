//! For tests only: a driver that can be told to misbehave.
//!
//! Honest, it is the stand-in venue's driver, so it can stand as a second
//! implementation of a driver name. Told otherwise (by its mode account, which
//! anyone may set: it holds nothing and is never deployed anywhere that
//! matters), it fails, burns its compute, answers garbage or nothing, or
//! prepares instructions a station must refuse, so the tests can show what a
//! station does when a pinned driver turns hostile or breaks.
//!
//! **Its accounts**, after the station's authority at the venue (0) and that
//! authority's dollar account (1): its mode account, then the stand-in
//! driver's own.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, set_return_data};
use anchor_lang::InstructionData;
use shonin_driver::{Close, Meta, Plan, Prepared, Valuation};
use shonin_local_driver::{answer, layout, logic, STATION, STATION_DOLLARS};
use shonin_local_venue as venue;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

pub const MODE_SEED: &[u8] = b"mode";

/// What the driver does; anything not named here is honest.
pub mod mode {
    pub const HONEST: u8 = 0;
    /// Every value, close and sweep fails.
    pub const FAIL: u8 = 1;
    /// Every value, close and sweep runs until its compute is gone.
    pub const BURN: u8 = 2;
    /// A close moves the authority's dollars through the token program.
    pub const FOREIGN_PROGRAM: u8 = 3;
    /// A close withdraws from the venue to an account that isn't the
    /// authority's.
    pub const FOREIGN_ACCOUNT: u8 = 4;
    /// A close names the authority's address where no rule puts it.
    pub const STATION_OUT_OF_PLACE: u8 = 5;
    /// A sweep leaves the proceeds at the venue.
    pub const KEEP_PROCEEDS: u8 = 6;
    /// Every answer is bytes that don't decode.
    pub const GARBAGE: u8 = 7;
    /// A close is an instruction the allowlist doesn't name.
    pub const WRONG_DISCRIMINATOR: u8 = 8;
    /// The allowlist names the station program as its venue.
    pub const VENUE_IS_STATION: u8 = 9;
    /// The allowlist names the registry as its venue.
    pub const VENUE_IS_REGISTRY: u8 = 10;
    /// The allowlist names the token program as its venue.
    pub const VENUE_IS_TOKEN: u8 = 11;
    /// The allowlist has a discriminator another starts with.
    pub const AMBIGUOUS: u8 = 12;
    /// A close deposits from the authority's dollar account into the venue.
    pub const DEPOSIT_IN_CLOSE: u8 = 13;
    /// Nothing is answered at all.
    pub const SILENT: u8 = 14;
    /// Every value, and the valuation a close carries, is twice what the
    /// venue says.
    pub const DOUBLE_VALUE: u8 = 15;
    /// A close names an account index it wasn't given.
    pub const OUT_OF_RANGE: u8 = 16;
    /// The allowlist names the system program as its venue.
    pub const VENUE_IS_SYSTEM: u8 = 17;
    /// A sweep withdraws all the account's cash, whatever it was asked for.
    pub const SWEEP_ALL: u8 = 18;
    /// A value tries to withdraw from the venue itself, signing as the
    /// station's authority, which it was never given.
    pub const SIGN_AS_STATION: u8 = 19;
    /// The allowlist names the vault's program as its venue.
    pub const VENUE_IS_VAULT: u8 = 20;
    /// The allowlist names this driver program as its venue.
    pub const VENUE_IS_ITSELF: u8 = 21;
    /// The allowlist names the stand-in venue's driver program as its venue.
    pub const VENUE_IS_LOCAL_DRIVER: u8 = 22;
    /// A close also buys a tenth of a unit of the first spot market the
    /// account holds at the ask, which a sale would fetch only the bid for:
    /// a loss no leaver's fraction owes.
    pub const TRADE_IN_CLOSE: u8 = 23;
    /// The last account it is given is not the stand-in's but one passed on:
    /// a close hands it to the venue among the accounts of its `place`, so
    /// the venue could call it, if it is a program, with the authority's
    /// signature.
    pub const PASS_ON_LAST: u8 = 24;
    /// A close's `place` names the authority where its rule puts it, but
    /// writable.
    pub const AUTHORITY_WRITABLE: u8 = 25;
}

/// Where the stand-in driver's accounts start among those this one is given.
const BASE: u8 = 3;

#[program]
pub mod shonin_test_driver {
    use super::*;

    pub fn set_mode(ctx: Context<SetMode>, mode: u8) -> Result<()> {
        ctx.accounts.mode.mode = mode;
        ctx.accounts.mode.bump = ctx.bumps.mode;
        Ok(())
    }

    pub fn allowlist(ctx: Context<Forwarded>) -> Result<()> {
        let mut list = logic::allowlist(&venue::ID);
        let mode = read_mode(ctx.remaining_accounts.first());
        match mode {
            mode::VENUE_IS_STATION => list.venue = shonin_station::ID,
            mode::VENUE_IS_REGISTRY => list.venue = shonin_registry::ID,
            mode::VENUE_IS_TOKEN => list.venue = anchor_spl::token::ID,
            mode::VENUE_IS_SYSTEM => list.venue = Pubkey::default(),
            mode::VENUE_IS_VAULT => list.venue = shonin_test_vault::ID,
            mode::VENUE_IS_ITSELF => list.venue = crate::ID,
            mode::VENUE_IS_LOCAL_DRIVER => list.venue = shonin_local_driver::ID,
            mode::AMBIGUOUS => {
                let mut prefix = list.rules[0].clone();
                prefix.discriminator.truncate(4);
                list.rules.push(prefix);
            }
            _ => {}
        }
        respond(mode, &list)
    }

    /// Its venue's terms exclude North Korea besides the sanctioned, so a
    /// vault that comes to read it has a longer list.
    pub fn restricted(_ctx: Context<Forwarded>) -> Result<()> {
        let code = |text: &[u8]| {
            let mut out = [0u8; 32];
            out[..text.len()].copy_from_slice(text);
            out
        };
        answer(&vec![code(b"KP"), code(b"SANCTIONED")])
    }

    pub fn value<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        let (mode, accounts) = split(ctx.remaining_accounts)?;
        misbehave(mode)?;
        let book = logic::read(&venue::ID, ctx.accounts.authority.key, accounts)?;
        if mode == mode::SIGN_AS_STATION {
            let plan = logic::sweep(&book, u64::MAX, BASE);
            let mut given = vec![
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.authority_dollars.to_account_info(),
                ctx.remaining_accounts[0].clone(),
            ];
            given.extend(accounts.iter().cloned());
            for prepared in &plan.instructions {
                let ix = Instruction {
                    program_id: *given[prepared.program as usize].key,
                    accounts: prepared
                        .accounts
                        .iter()
                        .map(|meta| AccountMeta {
                            pubkey: *given[meta.index as usize].key,
                            is_signer: meta.index == STATION,
                            is_writable: meta.writable,
                        })
                        .collect(),
                    data: prepared.data.clone(),
                };
                invoke(&ix, &given)?;
            }
        }
        respond(mode, &valuation(mode, &book, band_bps)?)
    }

    pub fn close<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        fraction: u64,
        band_bps: u16,
    ) -> Result<()> {
        let (mode, accounts) = split(ctx.remaining_accounts)?;
        misbehave(mode)?;
        let book = logic::read(&venue::ID, ctx.accounts.authority.key, accounts)?;
        let mut plan = logic::close(&book, fraction, band_bps, BASE)?;
        if mode == mode::TRADE_IN_CLOSE {
            let spot = book.markets.iter().find(|market| !market.is_perp);
            if let (Some(spot), Some(place)) = (spot, plan.instructions.first_mut()) {
                let mut orders = Vec::<venue::OrderRequest>::try_from_slice(&place.data[8..])?;
                orders.push(venue::OrderRequest {
                    market: spot.id,
                    is_buy: true,
                    size: (venue::SIZE_SCALE / 10) as u128,
                    limit_price: u64::MAX,
                    reduce_only: false,
                });
                place.data = venue::instruction::Place { orders }.data();
            }
        }
        if mode == mode::PASS_ON_LAST {
            // After the authority's two and the mode account, the stand-in's
            // accounts, then the one passed on.
            let last = BASE + accounts.len() as u8;
            if let Some(place) = plan.instructions.first_mut() {
                place.accounts.push(Meta {
                    index: last,
                    writable: false,
                });
            }
        }
        let at = |offset: usize| BASE + offset as u8;
        let meta = |index: u8, writable: bool| Meta { index, writable };
        let hostile = match mode {
            mode::FOREIGN_PROGRAM => Some(Prepared {
                program: at(layout::TOKEN_PROGRAM),
                accounts: vec![
                    meta(STATION_DOLLARS, true),
                    meta(at(layout::VAULT), true),
                    meta(STATION, false),
                ],
                // The token program's `Transfer` of one dollar-unit.
                data: [vec![3u8], 1u64.to_le_bytes().to_vec()].concat(),
            }),
            mode::FOREIGN_ACCOUNT => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![
                    meta(STATION, false),
                    meta(at(layout::ACCOUNT), true),
                    meta(at(layout::VAULT), true),
                    meta(at(layout::VENUE), false),
                    meta(at(layout::VAULT), true),
                    meta(at(layout::MINT), false),
                    meta(at(layout::TOKEN_PROGRAM), false),
                ],
                data: venue::instruction::Withdraw { amount: 1 }.data(),
            }),
            mode::STATION_OUT_OF_PLACE => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![
                    meta(STATION, false),
                    meta(at(layout::ACCOUNT), true),
                    meta(STATION, false),
                ],
                data: venue::instruction::Place { orders: vec![] }.data(),
            }),
            mode::WRONG_DISCRIMINATOR => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![meta(STATION, false)],
                data: vec![9; 8],
            }),
            mode::DEPOSIT_IN_CLOSE => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![
                    meta(STATION, false),
                    meta(at(layout::ACCOUNT), true),
                    meta(STATION_DOLLARS, true),
                    meta(at(layout::VENUE), false),
                    meta(at(layout::VAULT), true),
                    meta(at(layout::MINT), false),
                    meta(at(layout::TOKEN_PROGRAM), false),
                ],
                data: venue::instruction::Deposit { amount: 1 }.data(),
            }),
            mode::AUTHORITY_WRITABLE => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![
                    meta(STATION, true),
                    meta(at(layout::ACCOUNT), true),
                    meta(at(layout::VENUE), false),
                ],
                data: venue::instruction::Place { orders: vec![] }.data(),
            }),
            mode::OUT_OF_RANGE => Some(Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts: vec![meta(200, false)],
                data: venue::instruction::Place { orders: vec![] }.data(),
            }),
            _ => None,
        };
        if let Some(hostile) = hostile {
            plan.outside = false;
            plan.instructions.insert(0, hostile);
        }
        let value = valuation(mode, &book, band_bps)?;
        respond(mode, &Close { value, plan })
    }

    pub fn sweep<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        amount: u64,
    ) -> Result<()> {
        let (mode, accounts) = split(ctx.remaining_accounts)?;
        misbehave(mode)?;
        let book = logic::read(&venue::ID, ctx.accounts.authority.key, accounts)?;
        let plan = match mode {
            mode::KEEP_PROCEEDS => Plan::default(),
            mode::SWEEP_ALL => logic::sweep(&book, u64::MAX, BASE),
            _ => logic::sweep(&book, amount, BASE),
        };
        respond(mode, &plan)
    }
}

/// The stand-in's valuation, doubled when told to lie about it: a close
/// carries the same valuation a value would answer.
fn valuation(mode: u8, book: &logic::Book, band_bps: u16) -> Result<Valuation> {
    let mut valuation = logic::valuation(book, band_bps)?;
    if mode == mode::DOUBLE_VALUE {
        valuation.mark = valuation.mark.saturating_mul(2);
        valuation.exit = valuation.exit.saturating_mul(2);
    }
    Ok(valuation)
}

fn read_mode(info: Option<&AccountInfo>) -> u8 {
    let Some(info) = info else {
        return mode::HONEST;
    };
    let address = Pubkey::find_program_address(&[MODE_SEED], &crate::ID).0;
    if info.key != &address || info.owner != &crate::ID {
        return mode::HONEST;
    }
    let Ok(data) = info.try_borrow_data() else {
        return mode::HONEST;
    };
    Mode::try_deserialize(&mut &data[..]).map_or(mode::HONEST, |mode| mode.mode)
}

/// The mode, and the stand-in driver's accounts after the mode account (all
/// but the last, when that one is only passed on).
fn split<'a, 'info>(accounts: &'a [AccountInfo<'info>]) -> Result<(u8, &'a [AccountInfo<'info>])> {
    let (first, rest) = accounts
        .split_first()
        .ok_or(error!(TestDriverError::Failed))?;
    let mode = read_mode(Some(first));
    if mode == mode::PASS_ON_LAST {
        let (_, rest) = rest.split_last().ok_or(error!(TestDriverError::Failed))?;
        return Ok((mode, rest));
    }
    Ok((mode, rest))
}

fn misbehave(mode: u8) -> Result<()> {
    match mode {
        mode::FAIL => err!(TestDriverError::Failed),
        mode::BURN => {
            let mut x: u64 = 1;
            loop {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                if x == 0 {
                    return Ok(());
                }
            }
        }
        _ => Ok(()),
    }
}

fn respond(mode: u8, value: &impl AnchorSerialize) -> Result<()> {
    match mode {
        mode::GARBAGE => {
            set_return_data(&[0xff; 3]);
            Ok(())
        }
        mode::SILENT => Ok(()),
        _ => answer(value),
    }
}

#[account]
pub struct Mode {
    pub mode: u8,
    pub bump: u8,
}

#[derive(Accounts)]
pub struct SetMode<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + 2,
        seeds = [MODE_SEED],
        bump,
    )]
    pub mode: Account<'info, Mode>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Forwarded {}

#[derive(Accounts)]
pub struct Read<'info> {
    /// CHECK: the station's authority at the venue.
    pub authority: UncheckedAccount<'info>,
    /// CHECK: the authority's dollar account.
    pub authority_dollars: UncheckedAccount<'info>,
}

#[error_code]
pub enum TestDriverError {
    #[msg("Told to fail")]
    Failed,
}
