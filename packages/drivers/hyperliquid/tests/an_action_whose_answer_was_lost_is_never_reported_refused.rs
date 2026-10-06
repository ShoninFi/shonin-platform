//! An action the venue took, whose answer never came back, may have landed:
//! the agent must hear "unknown", or what the venue's own record says it
//! did, never "refused" (which says nothing happened).
//!
//! The stand-in venue here answers the exchange endpoint as each test says
//! (dropping the connection without an answer, as a venue does when its
//! answer is lost on the way), and refuses any later send of a nonce it took
//! as a duplicate, as a venue that numbers actions by nonce must. Its `/info`
//! answers an order's status and the account's fills from what it took.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use serde_json::{Value, json};
use shonin_hyperliquid::agent::{Allowlist, Outcome, Trader};
use shonin_hyperliquid::payload::{Action, Order, Receipt, Side, Tif};
use shonin_hyperliquid::{Api, Markets, Network};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One whole HTTP request: its path and its body, as JSON.
async fn request(socket: &mut tokio::net::TcpStream) -> Option<(String, Value)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buffer).to_string();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                return Some((path, serde_json::from_str(&body[..length]).ok()?));
            }
        }
    }
}

/// What the stand-in does with a send to the exchange endpoint.
#[derive(Clone, Copy)]
enum Exchange {
    /// Take the first send of a nonce and lose its answer; refuse any later
    /// send of it as a duplicate.
    TakeAndLoseTheAnswer,
    /// Lose the first send on the way (it never arrives), and answer the
    /// next as a venue does.
    LoseTheFirstSend,
    /// Take the first send of a nonce and lose its answer; answer any later
    /// send of it with an error in words other than a duplicate nonce's.
    TakeAndErrOnTheNextInOtherWords,
    /// Take the order and answer with no status for it.
    AnswerWithNoStatus,
}

/// What the stand-in's record says of an order it took.
#[derive(Clone)]
enum Record {
    /// Nothing: it has no order under that client id.
    None,
    /// The order, with this status, and these fills (size, price).
    Order(&'static str, Vec<(&'static str, &'static str)>),
}

const OID: u64 = 77;

#[derive(Default)]
struct Seen {
    /// Each nonce the exchange endpoint took.
    taken: HashSet<u64>,
    /// Bodies the exchange endpoint was sent, in order.
    sent: Vec<Value>,
}

async fn venue(exchange: Exchange, record: Record) -> (String, Arc<Mutex<Seen>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let shared = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = shared.clone();
            let record = record.clone();
            tokio::spawn(async move {
                let Some((path, body)) = request(&mut socket).await else {
                    return;
                };
                let answer = if path.ends_with("/exchange") {
                    let nonce = body["nonce"].as_u64().unwrap_or(0);
                    let mut seen = seen.lock().unwrap();
                    seen.sent.push(body.clone());
                    if matches!(exchange, Exchange::LoseTheFirstSend) && seen.sent.len() == 1 {
                        return;
                    }
                    if !seen.taken.insert(nonce) {
                        if matches!(exchange, Exchange::TakeAndErrOnTheNextInOtherWords) {
                            json!({"status": "err", "response": "Insufficient margin to place order."})
                        } else {
                            json!({"status": "err", "response": "Invalid nonce: duplicate nonce"})
                        }
                    } else if matches!(exchange, Exchange::AnswerWithNoStatus) {
                        json!({"status": "ok", "response": {"type": "order",
                            "data": {"statuses": []}}})
                    } else if matches!(
                        exchange,
                        Exchange::TakeAndLoseTheAnswer | Exchange::TakeAndErrOnTheNextInOtherWords
                    ) {
                        // Taken, and the answer lost.
                        return;
                    } else {
                        json!({"status": "ok", "response": {"type": "order",
                            "data": {"statuses": [{"resting": {"oid": OID}}]}}})
                    }
                } else {
                    let first = seen.lock().unwrap().sent.first().cloned();
                    let Some(first) = first else { return };
                    let order = first["action"]["orders"][0].clone();
                    let at = first["nonce"].clone();
                    match (body["type"].as_str(), &record) {
                        (Some("orderStatus"), Record::Order(status, _))
                            if body["oid"] == order["c"] =>
                        {
                            json!({"status": "order", "order": {
                                "order": {
                                    "coin": "ETH",
                                    "side": if order["b"] == true { "B" } else { "A" },
                                    "limitPx": order["p"],
                                    "origSz": order["s"],
                                    "sz": "0.0",
                                    "oid": OID,
                                    "timestamp": at,
                                    "cloid": order["c"],
                                },
                                "status": status,
                                "statusTimestamp": 0,
                            }})
                        }
                        (Some("orderStatus"), _) => json!({"status": "unknownOid"}),
                        (Some("userFillsByTime"), Record::Order(_, fills)) => Value::Array(
                            fills
                                .iter()
                                .enumerate()
                                .map(|(tid, (sz, px))| {
                                    json!({"coin": "ETH", "sz": sz, "px": px, "oid": OID,
                                        "tid": tid, "time": at})
                                })
                                .collect(),
                        ),
                        (Some("userFillsByTime"), _) => json!([]),
                        _ => json!({}),
                    }
                };
                let answer = answer.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (format!("http://{address}"), seen)
}

fn markets() -> Markets {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let spot = json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"}
        ],
        "universe": []
    });
    Markets::from_meta(&meta, &spot).unwrap()
}

fn trader(url: &str) -> Trader {
    let allowlist =
        Allowlist::from_driver(Address::repeat_byte(0x55), markets(), &[0], &[]).unwrap();
    Trader::new(
        Api::at(url, Network::Testnet),
        PrivateKeySigner::random(),
        allowlist,
    )
}

fn order(tif: Tif) -> Action {
    Action::Place(Order {
        market: "ETH".to_string(),
        side: Side::Buy,
        size: "0.5".parse().unwrap(),
        limit_price: "2700".parse().unwrap(),
        tif,
        reduce_only: false,
        client_id: None,
    })
}

#[tokio::test]
async fn an_order_the_venue_took_before_its_answer_was_lost_is_not_reported_refused() {
    let (url, seen) = venue(
        Exchange::TakeAndLoseTheAnswer,
        Record::Order("open", vec![]),
    )
    .await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    let seen = seen.lock().unwrap();
    assert_eq!(seen.taken.len(), 1, "the venue took the order once");
    assert!(
        seen.sent.windows(2).all(|pair| pair[0] == pair[1]),
        "a send whose answer was lost is sent again only as the very same bytes"
    );
    assert!(
        !matches!(outcome, Outcome::Refused(_)),
        "the venue took the order and its answer was lost, yet the agent is told it was refused (nothing happened): {outcome:?}"
    );
    assert_eq!(
        outcome,
        Outcome::Accepted(Receipt::Resting { order_id: OID }),
        "the venue's own record says the order rests"
    );
}

#[tokio::test]
async fn an_order_whose_answer_was_lost_and_which_filled_is_reported_filled_from_its_fills() {
    let (url, _) = venue(
        Exchange::TakeAndLoseTheAnswer,
        Record::Order("filled", vec![("0.2", "2690"), ("0.3", "2700")]),
    )
    .await;

    let outcome = trader(&url).outcome(&order(Tif::Ioc)).await;

    let Outcome::Accepted(Receipt::Filled {
        order_id,
        size,
        average_price,
    }) = outcome
    else {
        panic!("a filled order is reported filled: {outcome:?}");
    };
    assert_eq!(order_id, OID);
    assert_eq!(size, "0.5".parse().unwrap());
    assert_eq!(average_price, "2696".parse().unwrap());
}

#[tokio::test]
async fn an_order_that_arrived_and_that_the_venue_cannot_find_is_unknown_never_refused() {
    let (url, _) = venue(Exchange::TakeAndLoseTheAnswer, Record::None).await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    assert!(
        matches!(outcome, Outcome::Unknown(_)),
        "the venue took the nonce, and nothing says what the order did: {outcome:?}"
    );
}

#[tokio::test]
async fn an_order_the_venue_rejected_is_refused_once_its_record_says_so() {
    let (url, _) = venue(
        Exchange::TakeAndLoseTheAnswer,
        Record::Order("perpMarginRejected", vec![]),
    )
    .await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    assert!(
        matches!(&outcome, Outcome::Refused(why) if why.contains("perpMarginRejected")),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn an_order_whose_first_send_never_arrived_is_answered_by_its_second() {
    let (url, seen) = venue(Exchange::LoseTheFirstSend, Record::None).await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    assert_eq!(
        outcome,
        Outcome::Accepted(Receipt::Resting { order_id: OID })
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.sent.len(), 2);
    assert_eq!(
        seen.sent[0], seen.sent[1],
        "the very same bytes, nonce and all"
    );
    assert_eq!(seen.taken.len(), 1);
}

#[tokio::test]
async fn an_action_with_no_record_to_read_back_whose_answer_was_lost_is_unknown() {
    let (url, _) = venue(Exchange::TakeAndLoseTheAnswer, Record::None).await;

    let outcome = trader(&url)
        .outcome(&Action::Leverage {
            market: "ETH".into(),
            leverage: 3,
        })
        .await;

    assert!(matches!(outcome, Outcome::Unknown(_)), "{outcome:?}");
}

#[tokio::test]
async fn every_order_is_sent_with_a_client_id_to_look_it_up_by() {
    let (url, seen) = venue(
        Exchange::TakeAndLoseTheAnswer,
        Record::Order("open", vec![]),
    )
    .await;

    trader(&url).outcome(&order(Tif::Gtc)).await;

    let seen = seen.lock().unwrap();
    let client = seen.sent[0]["action"]["orders"][0]["c"].as_str().unwrap();
    assert_eq!(client.len(), 34, "0x and 32 hex digits: {client}");
}

#[tokio::test]
async fn any_error_answering_a_later_send_leads_to_the_venue_s_record_never_to_refused() {
    let (url, seen) = venue(
        Exchange::TakeAndErrOnTheNextInOtherWords,
        Record::Order("open", vec![]),
    )
    .await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    assert!(seen.lock().unwrap().sent.len() >= 2, "it was sent again");
    assert_eq!(
        outcome,
        Outcome::Accepted(Receipt::Resting { order_id: OID }),
        "the first send rests, whatever the later one was answered"
    );
}

#[tokio::test]
async fn an_error_answering_a_later_send_with_no_record_to_read_is_unknown_never_refused() {
    let (url, _) = venue(Exchange::TakeAndErrOnTheNextInOtherWords, Record::None).await;

    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;

    assert!(
        matches!(outcome, Outcome::Unknown(_)),
        "an error answering a later send says nothing of the first: {outcome:?}"
    );
}

#[tokio::test]
async fn an_order_answered_with_no_status_is_looked_up_never_taken_for_done() {
    let (url, _) = venue(Exchange::AnswerWithNoStatus, Record::Order("open", vec![])).await;
    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;
    assert_eq!(
        outcome,
        Outcome::Accepted(Receipt::Resting { order_id: OID }),
        "what the order did is read from the venue's record"
    );

    let (url, _) = venue(Exchange::AnswerWithNoStatus, Record::None).await;
    let outcome = trader(&url).outcome(&order(Tif::Gtc)).await;
    assert!(
        matches!(outcome, Outcome::Unknown(_)),
        "no status and no record: unknown, never done: {outcome:?}"
    );
}
