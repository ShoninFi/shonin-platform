use anchor_lang::prelude::*;

#[event]
pub struct StationCreated {
    pub vault: Pubkey,
    pub station: Pubkey,
    pub drivers: Vec<[u8; 32]>,
    /// The implementation pinned for each, in the same order.
    pub programs: Vec<Pubkey>,
}

#[event]
pub struct KeyApproved {
    pub driver: [u8; 32],
    pub key: Pubkey,
    pub valid_until: u64,
}

#[event]
pub struct KeyRevoked {
    pub driver: [u8; 32],
    pub key: Pubkey,
}

/// A venue instruction a key sent through the guard: the counted dollars the
/// station moved to the venue's authority for it, and what it took back from
/// there afterwards and counted.
#[event]
pub struct Executed {
    pub driver: [u8; 32],
    pub key: Pubkey,
    pub program: Pubkey,
    pub funded: u64,
    pub returned: u64,
}

#[event]
pub struct Pulled {
    pub amount: u64,
}

#[event]
pub struct Pushed {
    pub amount: u64,
}

/// Dollars the station sent to the vault's dollar account, and its running
/// total of everything it has sent there.
#[event]
pub struct SentHome {
    pub amount: u64,
    pub total: u128,
}

#[event]
pub struct Unwound {
    pub fraction: u64,
    pub sent_home: u64,
    pub blocked: bool,
}

#[event]
pub struct Stopped {
    pub halted_at: u64,
}
