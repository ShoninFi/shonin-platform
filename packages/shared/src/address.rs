//! An account on any vault chain: a 20-byte EVM address or a 32-byte Solana
//! key, written the way its own chain writes it.
//!
//! Anything keyed by an account across chains (a vault, an author, a key a
//! report was signed by) is keyed by this, so a Solana key is never squeezed
//! into an EVM address or the other way round. It travels as a string: `0x`
//! and forty hex digits for an EVM address, base58 for a Solana key.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An account on one vault chain, in the form its chain uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChainAddress {
    /// An EVM address. Compared by its bytes, so its case never matters.
    Evm([u8; 20]),
    /// A Solana key, base58. Its spelling is its identity: case matters.
    Solana([u8; 32]),
}

impl ChainAddress {
    /// Read an address in either form: `0x` and forty hex digits is an EVM
    /// address; anything else must be base58 for exactly 32 bytes.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if let Some(hex_digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            let bytes =
                hex::decode(hex_digits).map_err(|_| format!("{text:?} is not an EVM address"))?;
            let bytes: [u8; 20] = bytes
                .try_into()
                .map_err(|_| format!("{text:?} is not 20 bytes long"))?;
            return Ok(Self::Evm(bytes));
        }
        let bytes = bs58::decode(text)
            .into_vec()
            .map_err(|_| format!("{text:?} is neither an EVM address nor a Solana key"))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| format!("{text:?} is not a 32-byte Solana key"))?;
        Ok(Self::Solana(bytes))
    }

    pub fn evm(&self) -> Option<[u8; 20]> {
        match self {
            Self::Evm(bytes) => Some(*bytes),
            Self::Solana(_) => None,
        }
    }

    pub fn solana(&self) -> Option<[u8; 32]> {
        match self {
            Self::Solana(bytes) => Some(*bytes),
            Self::Evm(_) => None,
        }
    }

    pub fn is_solana(&self) -> bool {
        matches!(self, Self::Solana(_))
    }
}

impl std::fmt::Display for ChainAddress {
    /// An EVM address in lower case (`0x…`); a Solana key in base58.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evm(bytes) => write!(f, "0x{}", hex::encode(bytes)),
            Self::Solana(bytes) => f.write_str(&bs58::encode(bytes).into_string()),
        }
    }
}

impl std::str::FromStr for ChainAddress {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for ChainAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ChainAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_evm_address_reads_in_any_case_and_a_solana_key_only_as_written() {
        let lower: ChainAddress = "0xa11ce00000000000000000000000000000000001"
            .parse()
            .unwrap();
        let mixed: ChainAddress = "0xA11cE00000000000000000000000000000000001"
            .parse()
            .unwrap();
        assert_eq!(lower, mixed);
        assert_eq!(
            lower.to_string(),
            "0xa11ce00000000000000000000000000000000001"
        );

        let key = "Vote111111111111111111111111111111111111111";
        let solana: ChainAddress = key.parse().unwrap();
        assert!(solana.is_solana());
        assert_eq!(solana.to_string(), key);
        assert_ne!(
            ChainAddress::parse(&key.to_lowercase()).ok(),
            Some(solana),
            "base58 is case-sensitive"
        );
    }

    #[test]
    fn an_address_travels_as_its_chain_writes_it() {
        let key = ChainAddress::Solana([7; 32]);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(
            json,
            format!("\"{}\"", bs58::encode([7u8; 32]).into_string())
        );
        assert_eq!(serde_json::from_str::<ChainAddress>(&json).unwrap(), key);
        assert!(serde_json::from_str::<ChainAddress>("\"0x1234\"").is_err());
        assert!(serde_json::from_str::<ChainAddress>("\"not an address\"").is_err());
    }
}
