//! Solana: the platform's programs as the services reach them.
//!
//! There is one description of each program's interface, and it is the
//! program itself. Every account layout, instruction (its accounts, their
//! order and its arguments) and event used here comes from the programs'
//! own crates under `contracts/solana`, built for the host without their
//! entry points ([`programs`]), so a change to a program and the code that
//! talks to it stop compiling until they agree, as the Solidity interfaces
//! do for the EVM.
//!
//! A program's id is compiled in from the keypair it is deployed with
//! (`contracts/solana/target/deploy`), as the programs compile in each
//! other's. [`SolanaClient::check_deployments`] refuses a deployment whose
//! ids differ from the ones compiled here, which is a build against other
//! keypairs than the cluster's.
//!
//! - [`rpc`]: the node's JSON-RPC.
//! - [`client`]: connecting, reading, and sending until a transaction lands.
//! - [`tables`]: address lookup tables, for a transaction too large for the
//!   wire by address.
//! - [`events`]: a landed transaction, and the events in it.
//! - [`keys`]: the role keys, and keypair files.
//! - [`platform`]: every instruction the platform sends, built from the
//!   deployment's accounts.
//! - [`upkeep`]: what keeps a vault going (the bill, rent back, held
//!   payouts, a closed vault's doors), and finding every vault, request and
//!   claim.

pub mod attest;
pub mod client;
pub mod events;
pub mod executor;
pub mod keys;
pub mod platform;
pub mod pro_rata;
pub mod rpc;
pub mod scan;
pub mod tables;
pub mod upkeep;

pub use anchor_lang::prelude::{AccountMeta, Pubkey};
pub use anchor_lang::solana_program::instruction::Instruction;
pub use client::{Landed, MayStillLand, NeverLanded, SolanaClient, TransactionFailed};
pub use events::{Called, TransactionRecord};
pub use platform::Platform;
pub use rpc::{RateLimited, rate_limited};
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

/// The programs' own crates: their accounts, instructions, events, errors
/// and address helpers.
pub mod programs {
    pub use anchor_lang;
    pub use anchor_spl;
    pub use shonin_common as common;
    pub use shonin_cs_verifier as cs_verifier;
    pub use shonin_dev_verifier as dev_verifier;
    pub use shonin_driver as driver;
    pub use shonin_local_driver as local_driver;
    pub use shonin_local_venue as local_venue;
    pub use shonin_prices as prices;
    pub use shonin_profiles as profiles;
    pub use shonin_publishing as publishing;
    pub use shonin_registry as registry;
    pub use shonin_station as station;
    pub use shonin_vault as vault;

    use super::Pubkey;

    /// Every program the platform deploys, by its library name, at the id
    /// compiled into this build.
    pub fn compiled() -> Vec<(&'static str, Pubkey)> {
        vec![
            ("shonin_registry", shonin_registry::ID),
            ("shonin_dev_verifier", shonin_dev_verifier::ID),
            ("shonin_cs_verifier", shonin_cs_verifier::ID),
            ("shonin_publishing", shonin_publishing::ID),
            ("shonin_prices", shonin_prices::ID),
            ("shonin_profiles", shonin_profiles::ID),
            ("shonin_station", shonin_station::ID),
            ("shonin_vault", shonin_vault::ID),
            ("shonin_local_venue", shonin_local_venue::ID),
            ("shonin_local_driver", shonin_local_driver::ID),
        ]
    }
}

/// A 32-byte name as the programs hold one (`bytes32("local")`): the text,
/// then zeros.
pub fn name(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = text.as_bytes();
    let len = bytes.len().min(32);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}
