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
pub use args::{ChainArgs, Served, Target, connect_to, signing_for};
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
    /// Every interface under `contracts/evm/src/interfaces`, read as one,
    /// so the types they share are defined once. See `build.rs`.
    mod all {
        include!(concat!(env!("OUT_DIR"), "/interfaces.rs"));
    }
    pub mod usd {
        pub use super::all::*;
    }
    pub mod venue {
        pub use super::all::*;
    }
    pub mod attestation {
        pub use super::all::*;
    }
    pub mod publishing {
        pub use super::all::*;
    }
    pub mod prices {
        pub use super::all::*;
    }
    pub mod strategies {
        pub use super::all::*;
    }
    pub mod reviews {
        pub use super::all::*;
    }
    pub mod profiles {
        pub use super::all::*;
    }
    pub mod factory {
        pub use super::all::*;
    }
    pub mod vault {
        pub use super::all::*;
    }
    pub mod station {
        pub use super::all::*;
    }
    pub mod driver {
        pub use super::all::*;
    }
}

/// Allowances as the contracts take them: gas in the asset's units, six
/// decimals, rather than in dollars.
pub fn allowances(allowances: &shared::Allowances) -> bindings::prices::Allowances {
    bindings::prices::Allowances {
        fuelPerSec: allowances.fuel_per_sec,
        memoryMib: allowances.memory_mib,
        requestsPerMin: allowances.requests_per_min,
        bytesPerDay: allowances.bytes_per_day,
        streams: allowances.streams,
        inferenceMsPerMin: allowances.inference_ms_per_min,
        gasPerDay: (allowances.gas_per_day.max(0.0) * 1_000_000.0).round() as u64,
    }
}

/// What a depositor signs to be allowed to hold a vault's shares: EIP-712,
/// domain `("Shonin Vault", "1", chainId, vault)`, so a wallet shows the
/// person the words and the list they are agreeing to.
pub mod statement {
    use alloy_primitives::{Address, B256, U256, keccak256};
    use alloy_sol_types::SolValue;

    pub const TEXT: &str =
        "I am not a restricted person for this vault, and I am not acting for one.";

    /// The digest the vault checks for `depositor`, against the list as the
    /// vault writes it (`SANCTIONED,US`).
    pub fn digest(chain_id: u64, vault: Address, depositor: Address, restricted: &str) -> B256 {
        let domain = keccak256(
            (
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
                ),
                keccak256("Shonin Vault"),
                keccak256("1"),
                U256::from(chain_id),
                vault,
            )
                .abi_encode(),
        );
        let statement = keccak256(
            (
                keccak256("Statement(address depositor,string statement,string restricted)"),
                depositor,
                keccak256(TEXT),
                keccak256(restricted),
            )
                .abi_encode(),
        );
        let mut bytes = Vec::with_capacity(66);
        bytes.extend_from_slice(b"\x19\x01");
        bytes.extend_from_slice(domain.as_slice());
        bytes.extend_from_slice(statement.as_slice());
        keccak256(bytes)
    }
}

/// Names the contracts hold as `bytes32`: a driver, a bridge, a chain, a
/// market. A short ASCII name padded with zeros, as Solidity writes
/// `bytes32("local")`.
pub mod names {
    use alloy_primitives::{B256, FixedBytes};

    /// The stand-in venue's driver.
    pub const LOCAL_DRIVER: B256 = name(b"local");
    /// The stand-in bridge.
    pub const LOCAL_BRIDGE: B256 = name(b"local-bridge");

    pub const fn name(text: &[u8]) -> B256 {
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < text.len() && i < 32 {
            out[i] = text[i];
            i += 1;
        }
        FixedBytes(out)
    }

    /// A name read back from the contract: the bytes before the padding.
    pub fn text(name: B256) -> String {
        let end = name.0.iter().position(|byte| *byte == 0).unwrap_or(32);
        String::from_utf8_lossy(&name.0[..end]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolCall;

    #[test]
    fn a_call_encodes_to_its_selector() {
        let call = bindings::vault::IVault::requestRedeemCall {
            shares: alloy_primitives::U256::from(1_000_000u64),
            receiver: alloy_primitives::Address::ZERO,
        };
        let encoded = call.abi_encode();
        assert_eq!(
            &encoded[..4],
            &bindings::vault::IVault::requestRedeemCall::SELECTOR
        );
        assert_eq!(encoded.len(), 4 + 64);
    }

    #[test]
    fn a_name_is_what_solidity_would_write() {
        assert_eq!(&names::LOCAL_DRIVER.0[..5], b"local");
        assert_eq!(names::LOCAL_DRIVER.0[5], 0);
        assert_eq!(names::text(names::LOCAL_BRIDGE), "local-bridge");
    }
}
