//! `Review::recorded_tx` leaves the API's JSON as it was when it is
//! `None`, and a record written before it still reads.

use shared::model::review::{Rating, Review};

fn review(recorded_tx: Option<String>) -> Review {
    Review {
        ratings: [Rating::LooksFine; 6],
        findings: vec![],
        summary: "s".to_string(),
        reviewed: time::Date::from_calendar_date(2026, time::Month::October, 1).unwrap(),
        refused: false,
        sealed: false,
        reviewer_image: "img".to_string(),
        model_id_hash: "m".to_string(),
        prompt_hash: "p".to_string(),
        report_hash: "r".to_string(),
        limits: vec![],
        recorded_tx,
    }
}

#[test]
fn a_review_without_a_recorded_tx_serializes_as_before_and_old_json_still_reads() {
    let json = serde_json::to_value(review(None)).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert!(!keys.iter().any(|key| *key == "recorded_tx"), "{json}");
    assert_eq!(keys.len(), 11, "{keys:?}");
    // JSON as the API wrote it before the field existed reads back as None.
    let back: Review = serde_json::from_value(json).unwrap();
    assert_eq!(back, review(None));
    // With one, it is written and read back.
    let tx = format!("0x{}", "ab".repeat(32));
    let json = serde_json::to_value(review(Some(tx.clone()))).unwrap();
    assert_eq!(json["recorded_tx"], serde_json::json!(tx));
    let back: Review = serde_json::from_value(json).unwrap();
    assert_eq!(back.recorded_tx, Some(tx));
}
