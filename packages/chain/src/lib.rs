//! The chain the platform reads and writes.
//!
//! Every binding here is generated from the Solidity under
//! `contracts/evm/src/interfaces`, which is the same file the contracts
//! implement, so a signature cannot drift between the chain and the code that
//! talks to it: change the interface and both sides stop compiling until they
//! agree.
//!
//! Nothing in this crate talks to a node. It encodes calls and decodes events
//! and logs, which every target can do, so the web app reaches the contracts
//! through exactly the bindings the services use.

#[cfg(feature = "args")]
pub mod args;
#[cfg(feature = "rpc")]
pub mod attest;
#[cfg(feature = "rpc")]
pub mod client;
pub mod config;
pub mod deployments;

#[cfg(feature = "args")]
pub use args::{ChainArgs, Target, connect_to, signing_for};
#[cfg(feature = "rpc")]
pub use client::{Client, KeyClient, ROLE_COUNT, Role, Signing, keys_from_mnemonic};
pub use config::{ChainConfig, ChainConfigs, ChainKey, ChainKind, Network, NetworkName};
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
    /// The token standard, which is nobody's invention and so is written
    /// here rather than read from a contract of ours. A vault's shares and the
    /// asset it settles in are both read through it.
    pub mod erc20 {
        alloy_sol_types::sol! {
            interface IERC20 {
                event Transfer(address indexed from, address indexed to, uint256 value);
                event Approval(address indexed owner, address indexed spender, uint256 value);

                function name() external view returns (string memory);
                function symbol() external view returns (string memory);
                function decimals() external view returns (uint8);
                function totalSupply() external view returns (uint256);
                function balanceOf(address account) external view returns (uint256);
                function allowance(address owner, address spender) external view returns (uint256);
                function transfer(address to, uint256 value) external returns (bool);
                function approve(address spender, uint256 value) external returns (bool);
                function transferFrom(address from, address to, uint256 value) external returns (bool);
            }
        }
    }
    pub mod usd {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/ILocalUsd.sol");
    }
    pub mod venue {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/ILocalVenue.sol");
    }
    pub mod attestation {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IAttestationRegistry.sol");
    }
    pub mod strategies {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IStrategyRegistry.sol");
    }
    pub mod reviews {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IReviewRegistry.sol");
    }
    pub mod profiles {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IAuthorProfiles.sol");
    }
    pub mod factory {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IVaultFactory.sol");
    }
    pub mod vault {
        alloy_sol_types::sol!("../../contracts/evm/src/interfaces/IVault.sol");
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
