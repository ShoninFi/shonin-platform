//! The Shonin registry, on Solana.
//!
//! A skeleton: it proves the toolchain, the build and the local validator are
//! wired in. The registries the EVM contracts hold arrive here later.

use anchor_lang::prelude::*;

declare_id!("Bn5t8rYJnsd86vE1DRA7xwfG6Qiz7sw3AoGaM4WrpAs6");

#[program]
pub mod shonin_registry {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        msg!("shonin-registry {:?}", ctx.program_id);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
