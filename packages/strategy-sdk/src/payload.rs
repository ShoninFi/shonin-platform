//! Turning typed values into the bytes a venue's adapter reads.
//!
//! Every payload is canonical CBOR, so the same action encodes to the same
//! bytes wherever it is built, and decimals travel as decimals rather than as
//! floats that two machines might round differently.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The number a strategy and an adapter agree on: 128 bits of decimal, which
/// holds every size and price either of them will ever exchange without
/// rounding one of them into a different trade.
pub type Decimal = fastnum::D128;

/// A decimal from text, for the constants a strategy writes down.
///
/// ```ignore
/// let cap = strategy_sdk::decimal("0.05");
/// ```
pub fn decimal(text: &str) -> Decimal {
    text.parse().expect("a decimal the strategy wrote down")
}

/// Encode a value as a payload.
pub fn to_payload<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).expect("a payload encodes");
    bytes
}

/// Read a payload back, saying what went wrong rather than trapping: a
/// strategy that cannot read a reply should handle it, not die.
pub fn from_payload<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    ciborium::from_reader(bytes).map_err(|error| error.to_string())
}

/// Dollars as the millionths `transfers` moves, rounded down, so an agent
/// never asks to move more than it meant to. Zero for a negative amount.
pub fn millionths(dollars: Decimal) -> u64 {
    let units = (dollars * Decimal::from(1_000_000u32)).floor();
    u64::try_from(units).unwrap_or(0)
}
