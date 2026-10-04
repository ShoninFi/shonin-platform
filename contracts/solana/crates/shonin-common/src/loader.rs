//! Whether a program can still be changed, and by whom.
//!
//! A program owned by the upgradeable loader names a program-data account,
//! whose header holds its upgrade authority; with none, nobody can ever change
//! it again. A program owned by the original loaders can never be changed at
//! all. Anything else (another loader, or an account that isn't a program) is
//! not something this code can vouch for, and reads as unknown.

use anchor_lang::prelude::*;
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable};

/// What can still be done to a deployed program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// Nobody can ever change it.
    Final,
    /// This key can replace it.
    Upgradeable(Pubkey),
}

/// The upgradeable loader's account tags.
const PROGRAM_TAG: u32 = 2;
const PROGRAM_DATA_TAG: u32 = 3;
/// A program-data header: the tag, the slot it was deployed at, then an
/// optional authority.
const SLOT_END: usize = 4 + 8;

/// What can be done to the program at `program`, given its program-data
/// account if it is an upgradeable one. `None` when `program` is not a
/// deployed program this code understands, or `program_data` is not its own.
pub fn program_state(program: &AccountInfo, program_data: Option<&AccountInfo>) -> Option<Program> {
    if !program.executable {
        return None;
    }
    let owner = program.owner;
    if owner == &bpf_loader::ID || owner == &bpf_loader_deprecated::ID {
        return Some(Program::Final);
    }
    if owner != &bpf_loader_upgradeable::ID {
        return None;
    }
    let program_data = program_data?;
    let named = {
        let data = program.try_borrow_data().ok()?;
        if tag(&data)? != PROGRAM_TAG {
            return None;
        }
        Pubkey::try_from(data.get(4..36)?).ok()?
    };
    if program_data.key != &named || program_data.owner != &bpf_loader_upgradeable::ID {
        return None;
    }
    let data = program_data.try_borrow_data().ok()?;
    if tag(&data)? != PROGRAM_DATA_TAG {
        return None;
    }
    match *data.get(SLOT_END)? {
        0 => Some(Program::Final),
        1 => Some(Program::Upgradeable(
            Pubkey::try_from(data.get(SLOT_END + 1..SLOT_END + 33)?).ok()?,
        )),
        _ => None,
    }
}

/// Whether `program` is deployed and can never be changed.
pub fn is_final(program: &AccountInfo, program_data: Option<&AccountInfo>) -> bool {
    program_state(program, program_data) == Some(Program::Final)
}

/// Whether `signer` is the key that can still replace `program`: the one key
/// trusted to set a fresh deployment up, before it is made final.
pub fn is_upgrade_authority(
    program: &AccountInfo,
    program_data: Option<&AccountInfo>,
    signer: &Pubkey,
) -> bool {
    program_state(program, program_data) == Some(Program::Upgradeable(*signer))
}

fn tag(data: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(0..4)?.try_into().ok()?))
}
