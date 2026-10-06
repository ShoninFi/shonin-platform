//! A request the exchange's API never answers must give up by itself. The
//! post that values a station is made in a loop that waits for each read in
//! turn, and an agent's orders wait for the nonce read and the send while
//! holding the station's trader: one connection that is taken and never
//! answered would otherwise stop every later post and every later order,
//! for as long as the executor runs.
//!
//! The clock is paused, so time passes at once when nothing else can
//! happen: the client gives up, with an error, within its own timeout.

use std::time::Duration;

/// Long enough for any request to have given up by itself.
const GIVEN_UP: Duration = Duration::from_secs(2 * lighter::api::REQUEST_TIMEOUT.as_secs());

#[tokio::test(start_paused = true)]
async fn a_read_the_exchange_never_answers_gives_up_within_the_request_timeout() {
    // Connections are taken by the system and never answered.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = lighter::Api::new(format!("http://{}", listener.local_addr().unwrap()));

    let read = tokio::time::timeout(GIVEN_UP, api.order_books()).await;

    assert!(
        matches!(read, Ok(Err(_))),
        "the read was still waiting for an answer, or took one that never came"
    );
    let read = tokio::time::timeout(GIVEN_UP, api.account(1)).await;
    assert!(matches!(read, Ok(Err(_))));
    drop(listener);
}

/// The same holds for the executor's other venue client, Hyperliquid's.
#[tokio::test(start_paused = true)]
async fn a_hyperliquid_read_never_answered_gives_up_too() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = hyperliquid::Api::at(
        &format!("http://{}", listener.local_addr().unwrap()),
        hyperliquid::Network::Testnet,
    );

    let read =
        tokio::time::timeout(GIVEN_UP * 10, api.info(serde_json::json!({"type": "meta"}))).await;

    assert!(matches!(read, Ok(Err(_))), "it gave up with an error");
    drop(listener);
}

#[tokio::test(start_paused = true)]
async fn a_send_the_exchange_never_answers_gives_up_within_the_request_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = lighter::Api::new(format!("http://{}", listener.local_addr().unwrap()));

    let sent = tokio::time::timeout(GIVEN_UP, api.send_tx(14, "{}")).await;

    let Ok(Err(error)) = sent else {
        panic!("the send was still waiting for an answer, or was reported sent");
    };
    assert!(
        !lighter::api::not_taken(&error),
        "a send that got no answer may have been taken, and is never reported refused"
    );
    drop(listener);
}
