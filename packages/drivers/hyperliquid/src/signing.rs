//! The two ways HyperCore actions are signed, as EIP-712 digests.

use alloy::primitives::{Address, B256, U256, keccak256};
use serde::Serialize;

/// The chain id the `Exchange` domain of an L1 action names, whichever
/// network: the network is told apart by the phantom agent's `source`.
const L1_CHAIN_ID: u64 = 1337;

/// The chain id a user-signed action is signed under. HyperCore takes the
/// one the action names in `signatureChainId`; this is the one its own tools
/// use.
pub const SIGNATURE_CHAIN_ID: u64 = 0x66eee;

/// Which HyperCore an action is meant for: a signature for one is refused by
/// the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    /// The `hyperliquidChain` a user-signed action names.
    pub fn chain_name(self) -> &'static str {
        match self {
            Self::Mainnet => "Mainnet",
            Self::Testnet => "Testnet",
        }
    }

    fn phantom_source(self) -> &'static str {
        match self {
            Self::Mainnet => "a",
            Self::Testnet => "b",
        }
    }
}

/// The digest an L1 action is signed over: the action's MessagePack
/// encoding (maps with their fields in the order the venue's own tools write
/// them), the nonce, and the vault it acts for (none here), hashed into a
/// phantom agent under the `Exchange` domain.
pub fn l1_action_digest<A: Serialize>(
    network: Network,
    action: &A,
    nonce: u64,
) -> anyhow::Result<B256> {
    let mut data = rmp_serde::to_vec_named(action)?;
    data.extend_from_slice(&nonce.to_be_bytes());
    data.push(0);
    let connection_id = keccak256(&data);

    let type_hash = keccak256("Agent(string source,bytes32 connectionId)");
    let mut encoded = Vec::with_capacity(96);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(keccak256(network.phantom_source()).as_slice());
    encoded.extend_from_slice(connection_id.as_slice());
    let struct_hash = keccak256(&encoded);
    Ok(digest(
        &domain_separator("Exchange", L1_CHAIN_ID),
        &struct_hash,
    ))
}

/// One field of a user-signed action, in the order its type names them.
pub enum Field<'a> {
    String(&'a str),
    Bool(bool),
    U64(u64),
}

/// The digest a user-signed action is signed over: `type_name` such as
/// `HyperliquidTransaction:UsdClassTransfer`, its fields' declaration as the
/// venue writes it, and their values in the same order.
pub fn user_action_digest(type_name: &str, declaration: &str, fields: &[Field<'_>]) -> B256 {
    let type_hash = keccak256(format!("{type_name}({declaration})"));
    let mut encoded = Vec::with_capacity(32 * (fields.len() + 1));
    encoded.extend_from_slice(type_hash.as_slice());
    for field in fields {
        let word = match field {
            Field::String(text) => keccak256(text.as_bytes()),
            Field::Bool(value) => B256::from(U256::from(*value as u8)),
            Field::U64(value) => B256::from(U256::from(*value)),
        };
        encoded.extend_from_slice(word.as_slice());
    }
    let struct_hash = keccak256(&encoded);
    digest(
        &domain_separator("HyperliquidSignTransaction", SIGNATURE_CHAIN_ID),
        &struct_hash,
    )
}

fn domain_separator(name: &str, chain_id: u64) -> B256 {
    let type_hash = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut encoded = Vec::with_capacity(160);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(keccak256(name).as_slice());
    encoded.extend_from_slice(keccak256("1").as_slice());
    encoded.extend_from_slice(B256::from(U256::from(chain_id)).as_slice());
    encoded.extend_from_slice(Address::ZERO.into_word().as_slice());
    keccak256(&encoded)
}

fn digest(domain: &B256, struct_hash: &B256) -> B256 {
    let mut data = Vec::with_capacity(66);
    data.extend_from_slice(&[0x19, 0x01]);
    data.extend_from_slice(domain.as_slice());
    data.extend_from_slice(struct_hash.as_slice());
    keccak256(&data)
}

/// A decimal as HyperCore's wire format writes it: at most eight places,
/// trailing zeros and a trailing point dropped, never `-0`.
pub fn wire_decimal(value: f64) -> String {
    let text = format!("{value:.8}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" || text.is_empty() {
        "0".to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Modify {
        #[serde(rename = "type")]
        kind: &'static str,
        #[serde(rename = "usingBigBlocks")]
        using_big_blocks: bool,
    }

    #[test]
    fn an_l1_action_is_hashed_from_its_named_message_pack_encoding() {
        let action = Modify {
            kind: "evmUserModify",
            using_big_blocks: true,
        };
        let packed = rmp_serde::to_vec_named(&action).unwrap();
        // A two-entry map, keys as written, the bool as msgpack's true.
        assert_eq!(packed[0], 0x82);
        assert_eq!(&packed[1..6], b"\xa4type");
        assert_eq!(*packed.last().unwrap(), 0xc3);
        let mainnet = l1_action_digest(Network::Mainnet, &action, 1).unwrap();
        let testnet = l1_action_digest(Network::Testnet, &action, 1).unwrap();
        let later = l1_action_digest(Network::Testnet, &action, 2).unwrap();
        assert_ne!(
            mainnet, testnet,
            "a testnet signature must not serve mainnet"
        );
        assert_ne!(testnet, later, "the nonce is part of what is signed");
    }

    #[test]
    fn a_user_signed_action_binds_every_field() {
        let declaration = "string hyperliquidChain,string amount,bool toPerp,uint64 nonce";
        let a = user_action_digest(
            "HyperliquidTransaction:UsdClassTransfer",
            declaration,
            &[
                Field::String("Testnet"),
                Field::String("10"),
                Field::Bool(false),
                Field::U64(7),
            ],
        );
        let b = user_action_digest(
            "HyperliquidTransaction:UsdClassTransfer",
            declaration,
            &[
                Field::String("Testnet"),
                Field::String("10"),
                Field::Bool(true),
                Field::U64(7),
            ],
        );
        assert_ne!(a, b);
    }

    #[test]
    fn decimals_are_written_as_the_venue_reads_them() {
        assert_eq!(wire_decimal(22.51), "22.51");
        assert_eq!(wire_decimal(1.0), "1");
        assert_eq!(wire_decimal(0.000000001), "0");
        assert_eq!(wire_decimal(-0.0), "0");
        assert_eq!(wire_decimal(0.12345678), "0.12345678");
    }
}
