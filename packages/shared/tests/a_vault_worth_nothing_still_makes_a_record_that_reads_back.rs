//! A vault whose value per share reads 0 at a month's start (everything lost,
//! holders still in it) has no return to show for that month, not NaN or
//! infinity: serde_json writes a non-finite f64 as `null`, which does not
//! read back as an f64, so one such vault would leave the snapshot the API
//! serves unreadable by every client.
//!
//! The indexer samples the vault at $1 in January, then at $0 on the first
//! of February and the first of March.

use shared::model::common::Point;
use shared::model::strategy::Record;
use time::{Date, Month};

fn on(month: Month, value: f64) -> Point {
    Point {
        date: Date::from_calendar_date(2026, month, 1).unwrap(),
        value,
    }
}

#[test]
fn a_vault_worth_nothing_still_makes_a_record_that_reads_back() {
    let series = vec![
        on(Month::January, 1.0),
        on(Month::February, 0.0),
        on(Month::March, 0.0),
    ];
    let record = Record::from_series(series.clone(), series, 0);
    let non_finite: Vec<f64> = record
        .monthly
        .iter()
        .map(|point| point.value)
        .filter(|value| !value.is_finite())
        .collect();
    let json = serde_json::to_string(&record).unwrap();
    let read_back = serde_json::from_str::<Record>(&json);
    assert!(
        non_finite.is_empty() && read_back.is_ok(),
        "monthly returns that are not numbers: {non_finite:?}; reading the record back: {:?}",
        read_back.err()
    );
}
