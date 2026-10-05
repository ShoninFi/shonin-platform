//! For tests only: the stand-in venue after its upgrade authority has
//! replaced its code.
//!
//! The tests deploy it over the stand-in venue, at the venue's own address,
//! and send it the venue's own instructions through the station, every
//! account the station's allowlist constrains in its place. Whatever it does,
//! it holds one signature: the station's authority at this venue, which the
//! station lends it for the call.
//!
//! **`deposit`** and **`withdraw`** take the stand-in's own accounts in its
//! own order (the authority, its venue account, its dollar account, the
//! venue's settings, the venue's dollar account, the mint, the token program,
//! ...), so every position the station pins holds what it must, and turn the
//! authority's signature on the authority's own dollar account, moving
//! nothing, so that a check of the account's balance alone sees nothing
//! happen: `deposit` does as `how` says, in favour of the venue's settings,
//! which this program can sign for and so later spend from or close;
//! `withdraw` makes the settings a delegate over it, as a station's own close
//! would hand it. The station must refuse every one.
//!
//! **`place`** takes the stand-in's three (the authority, its account here,
//! the venue's settings) and then whatever accounts the executor added where
//! the stand-in reads markets, and uses the authority's signature against
//! them as [`attack`] says: the first order's limit price picks the attack,
//! and its market names the program called, which need not be among this
//! venue's accounts, only somewhere in the transaction. Each tries to reach
//! money that is not this venue's: the station's account at another venue,
//! or a token account this venue's authority doesn't own.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;
use anchor_lang::InstructionData;
use anchor_spl::token::{
    self, spl_token::instruction::AuthorityType, Approve, SetAuthority, Transfer,
};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// What `deposit` does to the authority's dollar account.
pub mod how {
    /// Approves a delegate for everything it holds.
    pub const DELEGATE: u64 = 0;
    /// Gives it a close authority.
    pub const CLOSE_AUTHORITY: u64 = 1;
    /// Hands it to another owner.
    pub const OWNER: u64 = 2;
}

/// What `place` does with the accounts after the stand-in's three, `a[3]`
/// on, while the station's authority at this venue (`a[0]`) signs.
pub mod attack {
    /// Calls the program the first order names (another venue) to withdraw
    /// one dollar-unit from an account there (`a[3]`) to `a[4]`, as that
    /// venue's own `withdraw` lays its accounts out (master, account, master's
    /// dollars, its settings `a[5]`, its dollar account `a[6]`, the mint
    /// `a[7]`, the token program `a[8]`), with this venue's authority as the
    /// master.
    pub const WITHDRAW_AS_ITSELF: u64 = 0;
    /// The same, naming `a[9]` as the master and the signer: the master of
    /// the account at the other venue.
    pub const WITHDRAW_AS_ITS_MASTER: u64 = 1;
    /// Moves one dollar-unit from `a[3]` to `a[4]` through the token program,
    /// signed by this venue's authority.
    pub const TAKE: u64 = 2;
    /// The same, naming `a[5]`, the owner of `a[3]`, as the signer.
    pub const TAKE_AS_ITS_OWNER: u64 = 3;
}

/// One order, laid out as the stand-in venue's.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct OrderRequest {
    pub market: [u8; 32],
    pub is_buy: bool,
    pub size: u128,
    pub limit_price: u64,
    pub reduce_only: bool,
}

/// Where the stand-in's own instructions put each account used here: the
/// station's authority at this venue, and its dollar account.
const AUTHORITY: usize = 0;
const DOLLARS: usize = 2;
const SETTINGS: usize = 3;
const TOKEN_PROGRAM: usize = 6;

#[program]
pub mod shonin_test_hostile_venue {
    use super::*;

    pub fn deposit<'info>(ctx: Context<'_, '_, 'info, 'info, Given>, how: u64) -> Result<()> {
        let a = ctx.remaining_accounts;
        let (authority, held, to, token_program) =
            (&a[AUTHORITY], &a[DOLLARS], &a[SETTINGS], &a[TOKEN_PROGRAM]);
        match how {
            how::DELEGATE => delegate(authority, held, to, token_program),
            how::CLOSE_AUTHORITY => hand_over(
                authority,
                held,
                to,
                token_program,
                AuthorityType::CloseAccount,
            ),
            _ => hand_over(
                authority,
                held,
                to,
                token_program,
                AuthorityType::AccountOwner,
            ),
        }
    }

    pub fn withdraw<'info>(ctx: Context<'_, '_, 'info, 'info, Given>, _amount: u64) -> Result<()> {
        let a = ctx.remaining_accounts;
        delegate(&a[AUTHORITY], &a[DOLLARS], &a[SETTINGS], &a[TOKEN_PROGRAM])
    }

    pub fn place<'info>(
        ctx: Context<'_, '_, 'info, 'info, Given>,
        orders: Vec<OrderRequest>,
    ) -> Result<()> {
        let a = ctx.remaining_accounts;
        let first = orders.first().ok_or(error!(HostileError::NoAttack))?;
        let target = Pubkey::new_from_array(first.market);
        match first.limit_price {
            attack::WITHDRAW_AS_ITSELF | attack::WITHDRAW_AS_ITS_MASTER => {
                let master = if first.limit_price == attack::WITHDRAW_AS_ITSELF {
                    &a[0]
                } else {
                    &a[9]
                };
                let meta = |info: &AccountInfo, writable: bool| AccountMeta {
                    pubkey: *info.key,
                    is_signer: false,
                    is_writable: writable,
                };
                let ix = Instruction {
                    program_id: target,
                    accounts: vec![
                        AccountMeta::new_readonly(*master.key, true),
                        meta(&a[3], true),
                        meta(&a[4], true),
                        meta(&a[5], false),
                        meta(&a[6], true),
                        meta(&a[7], false),
                        meta(&a[8], false),
                    ],
                    data: instruction::Withdraw { _amount: 1 }.data(),
                };
                invoke(&ix, a)?;
            }
            how => {
                let owner = if how == attack::TAKE { &a[0] } else { &a[5] };
                let program = a
                    .iter()
                    .find(|info| info.key == &token::ID)
                    .ok_or(error!(HostileError::NoAttack))?;
                token::transfer(
                    CpiContext::new(
                        program.clone(),
                        Transfer {
                            from: a[3].clone(),
                            to: a[4].clone(),
                            authority: owner.clone(),
                        },
                    ),
                    1,
                )?;
            }
        }
        Ok(())
    }
}

#[error_code]
pub enum HostileError {
    #[msg("No attack named")]
    NoAttack,
}

fn delegate<'info>(
    authority: &AccountInfo<'info>,
    held: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
) -> Result<()> {
    token::approve(
        CpiContext::new(
            token_program.clone(),
            Approve {
                to: held.clone(),
                delegate: to.clone(),
                authority: authority.clone(),
            },
        ),
        u64::MAX,
    )
}

fn hand_over<'info>(
    authority: &AccountInfo<'info>,
    held: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    kind: AuthorityType,
) -> Result<()> {
    token::set_authority(
        CpiContext::new(
            token_program.clone(),
            SetAuthority {
                current_authority: authority.clone(),
                account_or_mint: held.clone(),
            },
        ),
        kind,
        Some(*to.key),
    )
}

/// Everything comes as remaining accounts, laid out as the stand-in venue's
/// own instruction lays them out.
#[derive(Accounts)]
pub struct Given {}
