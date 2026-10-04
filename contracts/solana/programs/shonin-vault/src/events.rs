//! What a vault says happened, as the EVM vault's events say it.
//!
//! Solana keeps no past state, so the record of a vault is rebuilt from these;
//! everything a valuation, a settlement, a claim's close and the fallbacks
//! that ask the station say (the figures the record depends on among it) is
//! emitted through the program's own instruction data, which a node can't
//! truncate the way it truncates logs.

use anchor_lang::event::EVENT_IX_TAG_LE;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::Instruction, log::sol_log_data, program::invoke_signed,
};
use shonin_prices::Allowances;

use crate::state::{HaltReason, RequestKind, Terms, VaultState};

/// Where the events of the bookkeeping an instruction shares with others go:
/// to the logs as they happen, or kept and emitted once it is done through
/// calls of the program's own, as `emit_cpi!` emits them. A valuation's, a
/// settlement's, and those of every instruction that calls the station's
/// venues are kept: a full settlement says more than the logs hold, the
/// venues log their own first, and a node drops whatever comes after the
/// limit.
pub struct Said(Option<Vec<Vec<u8>>>);

impl Said {
    pub fn logs() -> Self {
        Self(None)
    }

    pub fn kept() -> Self {
        Self(Some(Vec::new()))
    }

    pub fn say<E: anchor_lang::Event>(&mut self, event: E) {
        match &mut self.0 {
            Some(kept) => kept.push(event.data()),
            None => sol_log_data(&[&event.data()]),
        }
    }

    /// Emits what was kept, in order, signed by the program's event
    /// authority.
    pub fn emit<'info>(self, authority: &AccountInfo<'info>, bump: u8) -> Result<()> {
        for data in self.0.unwrap_or_default() {
            let ix = Instruction::new_with_bytes(
                crate::ID,
                &[EVENT_IX_TAG_LE, &data].concat(),
                vec![AccountMeta::new_readonly(*authority.key, true)],
            );
            invoke_signed(
                &ix,
                std::slice::from_ref(authority),
                &[&[b"__event_authority", &[bump]]],
            )?;
        }
        Ok(())
    }
}

#[event]
pub struct Activated {
    pub strategy_id: u64,
    pub author: Pubkey,
    pub vault: Pubkey,
    pub station: Pubkey,
    pub tank_funding: u64,
}

#[event]
pub struct TermsSet {
    pub terms: Terms,
}

/// Shares moving, as an ERC-20 reports it: from the default key is a mint,
/// to it a burn; the vault's own key stands for shares the vault holds.
#[event]
pub struct Transfer {
    pub from: Pubkey,
    pub to: Pubkey,
    pub value: u128,
}

#[event]
pub struct Requested {
    pub request_id: u64,
    /// Where the request's account is: its maker's address, not its id's.
    pub address: Pubkey,
    pub owner: Pubkey,
    pub kind: RequestKind,
    pub earliest: u64,
    pub amount: u128,
}

#[event]
pub struct Deposited {
    pub request_id: u64,
    pub owner: Pubkey,
    pub assets: u64,
    pub shares: u128,
}

/// A deposit sent back, to whoever paid it.
#[event]
pub struct Refunded {
    pub request_id: u64,
    pub to: Pubkey,
    pub assets: u64,
}

#[event]
pub struct Withdrawn {
    pub request_id: u64,
    pub owner: Pubkey,
    pub shares: u128,
    pub assets: u64,
    pub fee_shares: u128,
}

/// One valuation's figures: what the record is rebuilt from.
#[event]
pub struct Valued {
    pub index: u64,
    pub time: u64,
    pub gross: u128,
    pub gross_exit: u128,
    pub supply: u128,
    pub fee: u128,
}

#[event]
pub struct ValuationVoided {
    pub index: u64,
    pub time: u64,
}

#[event]
pub struct HoldingSynced {
    pub shares: u128,
}

#[event]
pub struct FeesPaid {
    pub author_shares: u128,
    pub platform_shares: u128,
    pub holding_shares: u128,
    pub mark: u128,
}

#[event]
pub struct MarkRaised {
    pub mark: u128,
}

#[event]
pub struct ClaimBooked {
    pub claim_id: u64,
    pub owner: Pubkey,
    pub shares: u128,
}

#[event]
pub struct ClaimTried {
    pub claim_id: u64,
    pub blocked: bool,
}

#[event]
pub struct ClaimPaid {
    pub claim_id: u64,
    pub assets: u64,
}

#[event]
pub struct ProRataTaken {
    pub owner: Pubkey,
    pub shares: u128,
    pub assets: u64,
    pub units: u128,
}

#[event]
pub struct UnitsPaid {
    pub owner: Pubkey,
    pub assets: u64,
}

#[event]
pub struct TankFunded {
    pub from: Pubkey,
    pub assets: u64,
}

#[event]
pub struct TankCharged {
    pub billed: u64,
    pub charged: u64,
    pub total: u64,
}

#[event]
pub struct TankWithdrawn {
    pub assets: u64,
}

/// The author asked to withdraw the tank without a signed total; they can
/// from `ready_at`, for as long again.
#[event]
pub struct TankWithdrawalRequested {
    pub ready_at: u64,
}

#[event]
pub struct TankFilled {
    pub shares: u128,
    pub assets: u64,
}

#[event]
pub struct TankKeepSet {
    pub assets: u64,
}

#[event]
pub struct AllowancesSet {
    pub allowances: Allowances,
}

#[event]
pub struct IdleFloorSet {
    pub bps: u16,
    pub effective_at: u64,
}

#[event]
pub struct StateChanged {
    pub state: VaultState,
}

#[event]
pub struct Halted {
    pub reason: HaltReason,
    pub halted_at: u64,
}

#[event]
pub struct StatementRecorded {
    pub depositor: Pubkey,
    pub restricted_hash: [u8; 32],
}

#[event]
pub struct RestrictedAdded {
    pub code: [u8; 32],
}

#[event]
pub struct ToStation {
    pub assets: u64,
}

/// Dollars the station sent home, booked: `to_units` of them for the unit
/// holders.
#[event]
pub struct FromStation {
    pub assets: u64,
    pub to_units: u64,
}

/// A payout its receiver's dollar account couldn't take, held for them.
#[event]
pub struct Held {
    pub to: Pubkey,
    pub assets: u64,
}

#[event]
pub struct HeldCollected {
    pub to: Pubkey,
    pub assets: u64,
}
