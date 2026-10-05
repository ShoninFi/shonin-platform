//! Creating a program-derived account whose size is only known once the
//! instruction runs.
//!
//! Anyone can send lamports to an address before it is created, and a plain
//! `create_account` then fails, so an attacker could keep a record from ever
//! being made. An address that already holds lamports is topped up to rent,
//! given its room and assigned instead, which nobody can prevent.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    system_instruction,
};

/// Creates `target`, the program-derived address `seeds` (with its bump)
/// derive for `owner`, with `space` bytes, `payer` paying its rent.
pub fn create_pda<'info>(
    payer: &AccountInfo<'info>,
    target: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    seeds: &[&[u8]],
    space: usize,
    owner: &Pubkey,
) -> Result<()> {
    let rent = Rent::get()?.minimum_balance(space);
    let held = target.lamports();
    if held == 0 {
        invoke_signed(
            &system_instruction::create_account(payer.key, target.key, rent, space as u64, owner),
            &[payer.clone(), target.clone(), system_program.clone()],
            &[seeds],
        )?;
        return Ok(());
    }
    if rent > held {
        invoke(
            &system_instruction::transfer(payer.key, target.key, rent - held),
            &[payer.clone(), target.clone(), system_program.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(target.key, space as u64),
        &[target.clone(), system_program.clone()],
        &[seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(target.key, owner),
        &[target.clone(), system_program.clone()],
        &[seeds],
    )?;
    Ok(())
}

/// Resizes `account`, which this program owns, to `needed` bytes: `payer`
/// pays for more room and is refunded what less room no longer needs.
pub fn fit<'info>(
    account: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    needed: usize,
) -> Result<()> {
    let rent = Rent::get()?.minimum_balance(needed);
    let held = account.lamports();
    if rent > held {
        invoke(
            &system_instruction::transfer(payer.key, account.key, rent - held),
            &[payer.clone(), account.clone(), system_program.clone()],
        )?;
    } else if held > rent {
        **account.try_borrow_mut_lamports()? -= held - rent;
        **payer.try_borrow_mut_lamports()? += held - rent;
    }
    account.resize(needed)?;
    Ok(())
}
