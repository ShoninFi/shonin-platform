//! Money, exactly: typed text to base units and back, the contracts' own
//! quote arithmetic, and the design's money figures. No web dependencies, so
//! every rule here is tested natively.

pub mod format;
pub mod quote;
pub mod units;

pub use units::{
    AmountError, Rounding, format_exact, format_units, mul_div, mul_div_up, parse_units,
};
