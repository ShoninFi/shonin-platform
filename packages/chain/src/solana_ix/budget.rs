//! The instructions around the platform's own: the compute budget, the
//! Ed25519 check a signature made elsewhere travels in, and opening a dollar
//! account.

use super::{Instruction, Pubkey, address, ids, r, w, ws};

/// The compute a transaction asks for: the most one may use, as the services
/// ask. A vault's calls through its station need much of it (a claim's close
/// at three venues is over a million units), and asking costs nothing at a
/// zero price.
pub const COMPUTE_UNITS: u32 = 1_400_000;

/// The heap a transaction calling the vault or the station asks for: the
/// station's allocator, which the vault uses too, grows into it.
pub const HEAP: u32 = 256 * 1024;

/// The compute budget's `set_compute_unit_limit`.
pub fn compute_unit_limit(units: u32) -> Instruction {
    budget(2, units)
}

/// The compute budget's `request_heap_frame`.
pub fn heap_frame(bytes: u32) -> Instruction {
    budget(1, bytes)
}

fn budget(tag: u8, value: u32) -> Instruction {
    let mut data = vec![tag];
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: ids::COMPUTE_BUDGET,
        accounts: vec![],
        data,
    }
}

/// `ixs` with the compute budget the services send with them: the most
/// compute, and the heap frame when any calls the vault or the station.
pub fn with_budget(ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut all = vec![compute_unit_limit(COMPUTE_UNITS)];
    if ixs
        .iter()
        .any(|ix| ix.program_id == ids::VAULT || ix.program_id == ids::STATION)
    {
        all.push(heap_frame(HEAP));
    }
    all.extend(ixs);
    all
}

/// An Ed25519 instruction checking `signer`'s `signature` over `message`,
/// all carried in its own data: how a total the vault's executor signed
/// reaches the vault, which finds it through the instructions sysvar.
pub fn ed25519(signer: &Pubkey, signature: &[u8; 64], message: &[u8]) -> Instruction {
    // One signature, a padding byte, then the offsets of the signature, the
    // key and the message, each in this same instruction (index u16::MAX).
    const HEADER: u16 = 2 + 14;
    const KEY_AT: u16 = HEADER;
    const SIGNATURE_AT: u16 = KEY_AT + 32;
    const MESSAGE_AT: u16 = SIGNATURE_AT + 64;
    let this = u16::MAX;
    let mut data = vec![1u8, 0];
    for field in [
        SIGNATURE_AT,
        this,
        KEY_AT,
        this,
        MESSAGE_AT,
        message.len() as u16,
        this,
    ] {
        data.extend_from_slice(&field.to_le_bytes());
    }
    data.extend_from_slice(signer.as_ref());
    data.extend_from_slice(signature);
    data.extend_from_slice(message);
    Instruction {
        program_id: ids::ED25519,
        accounts: vec![],
        data,
    }
}

/// Open `owner`'s dollar account for `mint` if it isn't open, `payer` paying
/// its rent: the associated token program's `create_idempotent`. The vault
/// pays a receiver only into this account, so a payout to someone who never
/// held the dollar goes first.
pub fn open_dollars(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ids::ASSOCIATED_TOKEN,
        accounts: vec![
            ws(*payer),
            w(address::dollars(owner, mint)),
            r(*owner),
            r(*mint),
            r(ids::SYSTEM),
            r(ids::TOKEN),
        ],
        data: vec![1],
    }
}
