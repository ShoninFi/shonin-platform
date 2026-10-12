//! The Solana programs' instructions, built on every target, the browser's
//! included.
//!
//! The [`solana`](crate) feature reaches the programs through their own
//! crates, which build only for the host. This module builds the same
//! instructions from the Solana SDK's smallest pieces (a key, an instruction,
//! a message) and Borsh, so the web app can make a depositor's or an author's
//! transaction and hand it to the wallet. It never talks to a node: whatever
//! an instruction needs from the cluster (a vault's author, a claim's owner,
//! the markets a station holds) is an input.
//!
//! The programs stay the one description of their interface. Every builder
//! here is checked by this crate's tests to make exactly the instruction the
//! program's own `accounts::` and `instruction::` types make for the same
//! inputs (program, accounts and their order, signer and writable flags, and
//! data), so a change to a program that this module doesn't follow fails the
//! tests.
//!
//! - [`ids`]: the programs' ids, compiled in as the programs compile theirs
//!   (the committed devnet ids with `devnet`, mainnet's with `mainnet`,
//!   otherwise the local deploy keypairs').
//! - [`address`]: every program-derived address and dollar account.
//! - [`accounts`]: the accounts and events a flow reads, from their bytes.
//! - [`vault`]: the depositor's and the author's vault instructions.
//! - [`publishing`]: paying to publish.
//! - [`budget`]: the compute budget every vault transaction asks for, and the
//!   Ed25519 instruction a signed usage total travels in.
//! - [`read`]: the few account fields a builder's inputs come from and the
//!   restricted list's hash a deposit states.
//! - [`transaction`]: the transaction a wallet signs.

pub mod accounts;
pub mod address;
pub mod budget;
mod discriminator;
pub mod publishing;
pub mod read;
pub mod transaction;
pub mod vault;

pub use solana_hash::Hash;
pub use solana_instruction::{AccountMeta, Instruction};
pub use solana_pubkey::Pubkey;

/// The programs' ids for this build's network, and the Solana programs the
/// platform's instructions name.
pub mod ids {
    use super::Pubkey;

    include!(concat!(env!("OUT_DIR"), "/solana_program_ids.rs"));

    pub const SYSTEM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");
    /// The classic token program: the only one a vault's dollar may be under.
    pub const TOKEN: Pubkey = solana_pubkey::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
    pub const ASSOCIATED_TOKEN: Pubkey =
        solana_pubkey::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    pub const COMPUTE_BUDGET: Pubkey =
        solana_pubkey::pubkey!("ComputeBudget111111111111111111111111111111");
    pub const ED25519: Pubkey =
        solana_pubkey::pubkey!("Ed25519SigVerify111111111111111111111111111");
    /// The instructions sysvar, where a program finds the Ed25519 instruction.
    pub const INSTRUCTIONS: Pubkey =
        solana_pubkey::pubkey!("Sysvar1nstructions1111111111111111111111111");
    pub const UPGRADEABLE_LOADER: Pubkey =
        solana_pubkey::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
}

/// A program's instruction: its 8-byte discriminator, then its arguments in
/// Borsh, in the order the handler takes them.
fn data(discriminator: [u8; 8], args: &impl borsh::BorshSerialize) -> Vec<u8> {
    let mut out = discriminator.to_vec();
    args.serialize(&mut out)
        .expect("writing to a vector never fails");
    out
}

/// An account the instruction writes.
fn w(key: Pubkey) -> AccountMeta {
    AccountMeta::new(key, false)
}

/// An account the instruction only reads.
fn r(key: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(key, false)
}

/// A signer whose account the instruction writes (it pays, or is paid).
fn ws(key: Pubkey) -> AccountMeta {
    AccountMeta::new(key, true)
}

/// A signer whose account the instruction only reads.
fn rs(key: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(key, true)
}

#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod tests;
