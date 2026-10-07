//! The work the fuel benchmark times, compiled to a component.
//!
//! Each workload is the kind of thing an agent spends its processor on: plain
//! integer arithmetic, a hash, the 128-bit decimals every size and price is
//! written in, decoding the CBOR a venue answers with and the JSON a data
//! source does, and a whole decision step put together from those. Each
//! returns a checksum, so the compiler can't drop the work.

use std::hint::black_box;
use std::sync::OnceLock;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use strategy_sdk::{
    AccountState, Action, Decimal, Limits, MarketState, Order, PositionState, Side, decimal,
    from_payload, size_for, to_payload,
};

/// The workloads, in the order `run` numbers them.
const WORKLOADS: [&str; 6] = [
    "integer-loop",
    "sha256",
    "decimal",
    "cbor-decode",
    "json-decode",
    "decision-step",
];

/// The component's exports. The glue `wit-bindgen` writes for them is unsafe
/// code of its own, allowed here and nowhere else.
#[allow(unsafe_code)]
mod exports {
    use super::*;

    wit_bindgen::generate!({
        path: "../wit",
        world: "bench",
    });

    struct Bench;

    impl Guest for Bench {
        fn workloads() -> Vec<String> {
            WORKLOADS.iter().map(|name| name.to_string()).collect()
        }

        fn run(workload: u32, iterations: u32) -> u64 {
            match workload {
                0 => integer_loop(iterations),
                1 => sha256(iterations),
                2 => decimals(iterations),
                3 => cbor_decode(iterations),
                4 => json_decode(iterations),
                5 => decision_step(iterations),
                _ => 0,
            }
        }
    }

    export!(Bench);
}

/// Shifts, multiplies and adds: the cheapest instructions there are, so the
/// most fuel a second the machine will show.
pub fn integer_loop(iterations: u32) -> u64 {
    let mut x = black_box(0x9e37_79b9_7f4a_7c15u64);
    let mut sum = 0u64;
    for _ in 0..iterations {
        for _ in 0..1_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            sum = sum.wrapping_add(x.wrapping_mul(0x2545_f491_4f6c_dd1d));
        }
    }
    sum
}

/// A kibibyte hashed each iteration, each hash feeding the next.
pub fn sha256(iterations: u32) -> u64 {
    let mut block = [0u8; 1_024];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut sum = 0u64;
    for _ in 0..iterations {
        let digest = Sha256::digest(black_box(&block));
        block[..32].copy_from_slice(&digest);
        sum = sum.wrapping_add(u64::from_le_bytes(
            digest[..8].try_into().unwrap_or_default(),
        ));
    }
    sum
}

/// What sizing an order takes: products, quotients and comparisons of
/// 128-bit decimals, as every agent works out what to hold.
pub fn decimals(iterations: u32) -> u64 {
    let limits = limits();
    let minimum = decimal("100");
    let deployment = decimal("0.80");
    let mut sum = Decimal::from(0);
    for i in 0..iterations {
        let vault = Decimal::from(100_000 + (i % 1_000)) + decimal("0.125");
        let mark = decimal("64000.5") + Decimal::from(i % 97);
        let rate = decimal("0.0003") * mark / Decimal::from(24);
        let fraction = rate / mark;
        let wanted = vault * deployment;
        let gross = wanted / Decimal::from(2) + fraction;
        let notional = size_for(black_box(wanted), vault, gross, &limits, minimum);
        sum += (notional / mark).round(8);
    }
    sum.to_string().len() as u64
}

/// A venue's answer about an account read back from its payload.
pub fn cbor_decode(iterations: u32) -> u64 {
    let payload = account_payload();
    let mut sum = 0u64;
    for _ in 0..iterations {
        let state: AccountState = from_payload(black_box(payload)).unwrap_or_else(|_| empty());
        sum = sum.wrapping_add(state.markets.len() as u64 + state.margin_bps as u64);
    }
    sum
}

/// A level of an order book, as a JSON data source writes one: prices and
/// sizes as text.
#[derive(Deserialize)]
struct Level {
    px: String,
    sz: String,
    n: u32,
}

#[derive(Deserialize)]
struct Book {
    coin: String,
    time: u64,
    levels: [Vec<Level>; 2],
}

/// An order book of twenty levels a side read from JSON, its prices and
/// sizes made decimals.
pub fn json_decode(iterations: u32) -> u64 {
    let text = book_json();
    let mut sum = 0u64;
    for _ in 0..iterations {
        let Ok(book) = serde_json::from_str::<Book>(black_box(text)) else {
            continue;
        };
        let mut depth = Decimal::from(0);
        for level in book.levels.iter().flatten() {
            let (Ok(px), Ok(sz)) = (level.px.parse::<Decimal>(), level.sz.parse::<Decimal>())
            else {
                continue;
            };
            depth += px * sz;
            sum = sum.wrapping_add(level.n as u64);
        }
        sum = sum
            .wrapping_add(book.time + book.coin.len() as u64 + (depth > Decimal::from(0)) as u64);
    }
    sum
}

/// One step of a carry agent: read the account from its payload, work out
/// the funding and the size it wants, and write the order it would send.
pub fn decision_step(iterations: u32) -> u64 {
    let payload = account_payload();
    let limits = limits();
    let minimum = decimal("100");
    let entry = decimal("0.0002");
    let mut sum = 0u64;
    for _ in 0..iterations {
        let Ok(state) = from_payload::<AccountState>(black_box(payload)) else {
            continue;
        };
        let Some(perp) = state.market("BTC-PERP") else {
            continue;
        };
        let funding = perp.funding_fraction() * Decimal::from(24);
        if funding < entry {
            continue;
        }
        let held =
            state.position("BTC").abs() * perp.mark + state.position("BTC-PERP").abs() * perp.mark;
        let wanted = state.vault_value * decimal("0.80") - held;
        let notional = size_for(wanted, state.vault_value, held, &limits, minimum);
        let order = Order {
            market: "BTC".to_string(),
            side: Side::Buy,
            size: (notional / perp.ask).round(6),
            limit_price: (perp.ask * decimal("1.002")).round(2),
            reduce_only: false,
        };
        sum = sum.wrapping_add(to_payload(&Action::Place(order)).len() as u64);
    }
    sum
}

fn limits() -> Limits {
    Limits {
        max_gross_leverage: decimal("2"),
        max_order_fraction: decimal("0.05"),
        idle_buffer: decimal("0.06"),
        drawdown_stop: decimal("0.10"),
    }
}

fn empty() -> AccountState {
    AccountState {
        cash: Decimal::from(0),
        value: Decimal::from(0),
        exit_value: Decimal::from(0),
        vault_value: Decimal::from(0),
        idle: Decimal::from(0),
        station_idle: Decimal::from(0),
        margin_bps: 0,
        positions: Vec::new(),
        markets: Vec::new(),
    }
}

/// An account holding a carry on one market, as the venue would report it.
fn account_payload() -> &'static [u8] {
    static PAYLOAD: OnceLock<Vec<u8>> = OnceLock::new();
    PAYLOAD.get_or_init(|| {
        let market = |name: &str, is_perp: bool, rate: &str| MarketState {
            market: name.to_string(),
            is_perp,
            mark: decimal("64000.5"),
            oracle: decimal("63990.25"),
            bid: decimal("63995.0"),
            ask: decimal("64005.0"),
            funding_index: decimal("1234.567890"),
            funding_rate: decimal(rate),
        };
        to_payload(&AccountState {
            cash: decimal("41234.567891"),
            value: decimal("98765.432101"),
            exit_value: decimal("98700.000001"),
            vault_value: decimal("104321.123456"),
            idle: decimal("6250.5"),
            station_idle: decimal("125.25"),
            margin_bps: 1_000,
            positions: vec![
                PositionState {
                    market: "BTC".to_string(),
                    size: decimal("0.4321"),
                },
                PositionState {
                    market: "BTC-PERP".to_string(),
                    size: decimal("-0.4321"),
                },
            ],
            markets: vec![market("BTC", false, "0"), market("BTC-PERP", true, "1.25")],
        })
    })
}

fn book_json() -> &'static str {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        let side = |sign: i64| {
            (0..20)
                .map(|i| {
                    let px = 64_000 + sign * (i + 1);
                    format!(
                        r#"{{"px":"{px}.5","sz":"0.{:04}","n":{}}}"#,
                        1_000 + i * 37,
                        i % 5 + 1
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            r#"{{"coin":"BTC","time":1759800000000,"levels":[[{}],[{}]]}}"#,
            side(-1),
            side(1)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_workload_does_its_work() {
        assert_ne!(integer_loop(2), 0);
        assert_ne!(sha256(2), 0);
        assert_ne!(decimals(2), 0);
        assert_eq!(cbor_decode(2), 2 * (2 + 1_000));
        assert_ne!(json_decode(2), 0);
        assert_ne!(decision_step(2), 0, "the account is worth a step");
    }
}
