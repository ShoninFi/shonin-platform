//! The time, as every Shonin program reads it.
//!
//! Only the cluster's clock, `Clock.unix_timestamp`, never the slot: on the
//! local chain the clock is moved forward in jumps, and a jump can leave the
//! slot lower than it was.

use anchor_lang::prelude::*;

/// The cluster's time in seconds. Zero means "never" throughout the programs
/// (never proposed, never reviewed), so a clock that reads zero or less is
/// refused rather than trusted.
pub fn now() -> Result<u64> {
    let time = Clock::get()?.unix_timestamp;
    if time <= 0 {
        return Err(ProgramError::InvalidArgument.into());
    }
    Ok(time as u64)
}
