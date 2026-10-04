use anchor_lang::prelude::*;

#[event]
pub struct Submitted {
    pub strategy_id: u64,
    pub author: Pubkey,
    pub upload: [u8; 32],
    pub fee: u64,
}

#[event]
pub struct Registered {
    pub strategy_id: u64,
    pub author: Pubkey,
    pub bundle_hash: [u8; 32],
    pub source_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
}

#[event]
pub struct ReviewSubmitted {
    pub strategy_id: u64,
    pub reviewer: Pubkey,
    pub ratings: [u8; 6],
    pub report_hash: [u8; 32],
}

#[event]
pub struct Refused {
    pub strategy_id: u64,
    pub reviewer: Pubkey,
    pub source_hash: [u8; 32],
    pub report_hash: [u8; 32],
}
