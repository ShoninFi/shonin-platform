//! For tests only: a caller standing where the vault will.
//!
//! A vault reaches its station by calling the station program, signing with
//! the vault's own program-derived address. This program does exactly that
//! and nothing else, so the tests can drive a station through the interface
//! the vault uses (create, pull, push, value, unwind) at the call depth the
//! vault will use it at, and can say, as the vault will be asked, whether it
//! has halted and whether it has a valuation to settle.
//!
//! `call` makes one station instruction, given as its data and its accounts,
//! with the vault signing; `depth` more calls of this program sit in between
//! first, to show how much room is left under the runtime's limit on nested
//! calls. It measures the vault's own dollar account across the call, as the
//! vault will measure what a station sends it, and answers with what the
//! station answered.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke_signed, set_return_data},
};
use anchor_lang::InstructionData;
use shonin_station::state::vault::VaultView;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

pub const VAULT_SEED: &[u8] = b"vault";

#[program]
pub mod shonin_test_vault {
    use super::*;

    pub fn open(ctx: Context<Open>, label: [u8; 32]) -> Result<()> {
        ctx.accounts.vault.set_inner(VaultState {
            label,
            bump: ctx.bumps.vault,
            halted_at: 0,
            halt_grace: 0,
            unsettled: false,
            received: 0,
        });
        Ok(())
    }

    /// What the station is told when it asks.
    pub fn set_view(
        ctx: Context<SetView>,
        halted_at: u64,
        halt_grace: u64,
        unsettled: bool,
    ) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        vault.halted_at = halted_at;
        vault.halt_grace = halt_grace;
        vault.unsettled = unsettled;
        Ok(())
    }

    /// The station's question, answered from the vault's account.
    pub fn station_view(ctx: Context<View>) -> Result<()> {
        let vault = &ctx.accounts.vault;
        let view = VaultView {
            halted_at: vault.halted_at,
            halt_grace: vault.halt_grace,
            unsettled: vault.unsettled,
        };
        set_return_data(&view.try_to_vec()?);
        Ok(())
    }

    /// Makes the station instruction `data`, whose accounts are the remaining
    /// ones, signing as the vault, after `depth` calls of this program in
    /// between.
    pub fn call<'info>(
        ctx: Context<'_, '_, 'info, 'info, Call<'info>>,
        data: Vec<u8>,
        depth: u8,
    ) -> Result<()> {
        let vault_info = ctx.accounts.vault.to_account_info();
        let (label, bump) = {
            let data = vault_info.try_borrow_data()?;
            let state = VaultState::try_deserialize(&mut &data[..])?;
            (state.label, state.bump)
        };
        let seeds: &[&[u8]] = &[VAULT_SEED, &label, &[bump]];
        let vault = vault_info.key;
        let metas = |accounts: &[AccountInfo]| -> Vec<AccountMeta> {
            accounts
                .iter()
                .map(|info| AccountMeta {
                    pubkey: *info.key,
                    is_signer: info.key == vault || info.is_signer,
                    is_writable: info.is_writable,
                })
                .collect()
        };
        let program = ctx.accounts.program.to_account_info();
        let dollars = ctx.accounts.dollars.to_account_info();
        let before = balance(&dollars);
        let ix = if depth == 0 {
            Instruction {
                program_id: *program.key,
                accounts: metas(ctx.remaining_accounts),
                data,
            }
        } else {
            let mut accounts = vec![
                AccountMeta {
                    pubkey: *vault,
                    is_signer: false,
                    is_writable: vault_info.is_writable,
                },
                AccountMeta::new_readonly(*dollars.key, false),
                AccountMeta::new_readonly(*program.key, false),
            ];
            accounts.extend(metas(ctx.remaining_accounts));
            Instruction {
                program_id: crate::ID,
                accounts,
                data: instruction::Call {
                    data,
                    depth: depth - 1,
                }
                .data(),
            }
        };
        let mut infos = vec![vault_info.clone(), dollars.clone(), program.clone()];
        infos.extend(ctx.remaining_accounts.iter().cloned());
        set_return_data(&[]);
        invoke_signed(&ix, &infos, &[seeds])?;
        let answer = get_return_data().map(|(_, data)| data).unwrap_or_default();
        if depth == 0 && vault_info.is_writable {
            let received = balance(&dollars).saturating_sub(before);
            let mut data = vault_info.try_borrow_mut_data()?;
            let mut state = VaultState::try_deserialize(&mut &data[..])?;
            state.received = received;
            state.try_serialize(&mut &mut data[..])?;
        }
        set_return_data(&answer);
        Ok(())
    }
}

fn balance(info: &AccountInfo) -> u64 {
    info.try_borrow_data()
        .ok()
        .and_then(|data| {
            data.get(64..72)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        })
        .unwrap_or(0)
}

pub fn vault_address(label: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[VAULT_SEED, label], &crate::ID).0
}

#[account]
#[derive(InitSpace)]
pub struct VaultState {
    pub label: [u8; 32],
    pub bump: u8,
    pub halted_at: u64,
    pub halt_grace: u64,
    pub unsettled: bool,
    /// What the vault's dollar account gained across the last call.
    pub received: u64,
}

#[derive(Accounts)]
#[instruction(label: [u8; 32])]
pub struct Open<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + VaultState::INIT_SPACE,
        seeds = [VAULT_SEED, label.as_ref()],
        bump,
    )]
    pub vault: Account<'info, VaultState>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetView<'info> {
    #[account(mut, seeds = [VAULT_SEED, vault.label.as_ref()], bump = vault.bump)]
    pub vault: Account<'info, VaultState>,
}

#[derive(Accounts)]
pub struct View<'info> {
    #[account(seeds = [VAULT_SEED, vault.label.as_ref()], bump = vault.bump)]
    pub vault: Account<'info, VaultState>,
}

#[derive(Accounts)]
pub struct Call<'info> {
    /// CHECK: this program's vault account, at its own address; read and
    /// written by hand, so a call nested in another never writes over what
    /// the inner one recorded.
    #[account(mut, owner = crate::ID)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, measured across the call.
    pub dollars: UncheckedAccount<'info>,
    /// CHECK: the program called: the station, or this one again.
    pub program: UncheckedAccount<'info>,
}
