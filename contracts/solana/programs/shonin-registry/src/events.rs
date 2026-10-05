use anchor_lang::prelude::*;

use crate::state::{ApprovalKind, ImageKind};

#[event]
pub struct Initialized {
    pub admin: Pubkey,
    pub cluster_name: String,
    pub cluster: [u8; 32],
    pub timelock: u64,
    pub min_timelock: u64,
    pub blessing_ttl: u64,
}

#[event]
pub struct ImageProposed {
    pub image_digest: [u8; 32],
    pub kind: ImageKind,
    pub ready_at: u64,
}

#[event]
pub struct ImageRevoked {
    pub image_digest: [u8; 32],
}

#[event]
pub struct ProjectProposed {
    pub project_id: [u8; 32],
    pub ready_at: u64,
}

#[event]
pub struct ProjectRevoked {
    pub project_id: [u8; 32],
}

#[event]
pub struct VerifierProposed {
    pub program: Pubkey,
    pub state: Pubkey,
    pub ready_at: u64,
}

#[event]
pub struct VerifierRetired {
    pub state: Pubkey,
}

#[event]
pub struct Proposed {
    pub kind: ApprovalKind,
    pub name: [u8; 32],
    pub implementation: Pubkey,
    pub ready_at: u64,
}

#[event]
pub struct ProposalWithdrawn {
    pub kind: ApprovalKind,
    pub name: [u8; 32],
    pub implementation: Pubkey,
}

#[event]
pub struct Revoked {
    pub kind: ApprovalKind,
    pub name: [u8; 32],
    pub implementation: Pubkey,
}

/// The timelock in force changes at `effective_at`: at once when raised,
/// after the current timelock when lowered.
#[event]
pub struct TimelockSet {
    pub timelock: u64,
    pub effective_at: u64,
}

/// What a blessing recorded: `blessed_until` is the expiry now on record,
/// which a replay that changes nothing leaves as it was.
#[event]
pub struct Blessed {
    pub signer: Pubkey,
    pub vault_chain: [u8; 32],
    pub vault: [u8; 32],
    pub strategy_hash: [u8; 32],
    pub image_digest: [u8; 32],
    pub blessed_until: u64,
}
