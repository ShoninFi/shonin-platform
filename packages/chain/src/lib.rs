//! The chain the platform reads and writes.
//!
//! Every binding here is generated from the Solidity under
//! `contracts/src/interfaces`, which is the same file the contracts
//! implement, so a signature cannot drift between the chain and the code that
//! talks to it: change the interface and both sides stop compiling until they
//! agree.
//!
//! Nothing in this crate talks to a node. It encodes calls and decodes events
//! and logs, which every target can do, so the web app reaches the contracts
//! through exactly the bindings the services use.

#[cfg(feature = "rpc")]
pub mod client;
pub mod deployments;

#[cfg(feature = "rpc")]
pub use client::{Client, KeyClient, Role};
pub use deployments::{Deployments, Roles};

/// The generated bindings, one module per interface file.
///
/// Each module holds the file's structs and enums beside the interface's own
/// call and event types.
///
/// Everything inside is written by the macro from the Solidity, so the shape
/// of a call is the contract's decision rather than this crate's.
#[allow(clippy::too_many_arguments)]
pub mod bindings {
    pub mod usd {
        alloy_sol_types::sol!("../../contracts/src/interfaces/ILocalUsd.sol");
    }
    pub mod venue {
        alloy_sol_types::sol!("../../contracts/src/interfaces/ILocalVenue.sol");
    }
    pub mod attestation {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IAttestationRegistry.sol");
    }
    pub mod strategies {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IStrategyRegistry.sol");
    }
    pub mod reviews {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IReviewRegistry.sol");
    }
    pub mod profiles {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IAuthorProfiles.sol");
    }
    pub mod factory {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IVaultFactory.sol");
    }
    pub mod vault {
        alloy_sol_types::sol!("../../contracts/src/interfaces/IVault.sol");
    }
}

/// The two accounts a vault can move capital between, and the venue it names.
/// They are `bytes32` on the contract, which is a short ASCII name padded with
/// zeros.
pub mod accounts {
    use alloy_primitives::{B256, FixedBytes};

    pub const IDLE: B256 = name(b"idle");
    pub const VENUE: B256 = name(b"venue");
    pub const VENUE_ID: B256 = name(b"local");

    /// Solidity's `bytes32("idle")`: the bytes, then zeros.
    const fn name(text: &[u8]) -> B256 {
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < text.len() {
            out[i] = text[i];
            i += 1;
        }
        FixedBytes(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolCall;

    #[test]
    fn a_call_encodes_to_its_selector() {
        let call = bindings::vault::IVault::depositCall {
            assets: alloy_primitives::U256::from(1_000_000u64),
            receiver: alloy_primitives::Address::ZERO,
        };
        let encoded = call.abi_encode();
        assert_eq!(
            &encoded[..4],
            &bindings::vault::IVault::depositCall::SELECTOR
        );
        assert_eq!(encoded.len(), 4 + 64);
    }

    #[test]
    fn an_account_name_is_what_solidity_would_write() {
        assert_eq!(&accounts::IDLE.0[..4], b"idle");
        assert_eq!(accounts::IDLE.0[4], 0);
        assert_eq!(&accounts::VENUE.0[..5], b"venue");
        assert_eq!(&accounts::VENUE_ID.0[..5], b"local");
    }
}
