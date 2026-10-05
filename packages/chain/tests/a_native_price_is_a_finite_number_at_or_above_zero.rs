//! The chain config's `native_usd` is the price the executor's meter turns
//! gas into the author's bill at. A price that isn't a finite number at or
//! above zero is refused when the file is read: a NaN or an infinity would
//! bill the author's whole gas allowance whatever was spent, and a negative
//! price would take from the rest of the bill.

use chain::ChainConfigs;

fn with_price(price: &str) -> String {
    format!(
        "[chains.\"evm:31337\"]\nlabel = \"Base (local)\"\nrpc = \"http://127.0.0.1:19100\"\ndollar = \"USDC\"\nnative_usd = {price}\n"
    )
}

#[test]
fn a_native_price_that_is_not_a_finite_number_at_or_above_zero_is_refused() {
    assert!(ChainConfigs::from_toml(&with_price("2500.0"), |_| None).is_ok());
    assert!(ChainConfigs::from_toml(&with_price("0.0"), |_| None).is_ok());

    for bad in ["nan", "inf", "-inf", "-2500.0"] {
        assert!(
            ChainConfigs::from_toml(&with_price(bad), |_| None).is_err(),
            "native_usd = {bad} was accepted"
        );
    }
}
