//! The Shonin registry, on Solana.
//!
//! A skeleton: it proves the toolchain, the build and the local validator are
//! wired in. The registries the EVM contracts hold arrive here later.

use anchor_lang::prelude::*;

// The address comes from build.rs: the committed mainnet id, or the keypair
// this machine deploys with. No other address is ever written down.
include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

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
