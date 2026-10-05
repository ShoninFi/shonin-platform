//! 256-bit unsigned integers, for products that outgrow 128 bits.
//!
//! A cost multiplies a 64-bit allowance by a 128-bit price by a 64-bit span
//! of seconds: up to 256 bits, which this type holds exactly, so a division
//! after the product rounds once, down, as the EVM's `mulDiv` does.

#![allow(clippy::manual_div_ceil, clippy::assign_op_pattern)]

uint::construct_uint! {
    /// An unsigned 256-bit integer.
    pub struct U256(4);
}

impl U256 {
    /// Little-endian bytes.
    pub fn to_le_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        self.to_little_endian(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_largest_product_of_a_cost_fits() {
        let product = U256::from(u64::MAX) * U256::from(u128::MAX) * U256::from(u64::MAX);
        assert!(product > U256::from(u128::MAX));
        assert_eq!(
            product / U256::from(u64::MAX) / U256::from(u64::MAX),
            U256::from(u128::MAX)
        );
    }
}
