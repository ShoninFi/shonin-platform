//! A signed order is "sent" only when the exchange said it took it. An
//! answer that is an HTTP failure (a gateway turning requests away, a
//! server error) is not the exchange taking the order, whatever its body
//! looks like, and must never reach the agent as an accepted send with an
//! empty hash. A gateway turning the request away for its rate (429) never
//! passed it on, so nothing was done; a server error may have come after
//! the exchange took it, so it is never reported refused either.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A stand-in for the exchange's API that answers every request with
/// `status` and `body`.
async fn answering(status: &'static str, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // The request is small; one read after a short pause holds
                // all of it.
                let mut buffer = vec![0u8; 65_536];
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let _ = socket.read(&mut buffer).await;
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn an_order_a_gateway_turned_away_is_not_reported_sent() {
    let url = answering(
        "429 Too Many Requests",
        r#"{"message":"Too Many Requests"}"#,
    )
    .await;
    let sent = lighter::Api::new(url).send_tx(14, "{}").await;
    assert!(
        sent.is_err(),
        "the exchange never took the order, and it was reported sent with the hash {sent:?}"
    );
    assert!(
        lighter::api::not_taken(&sent.unwrap_err()),
        "turned away: nothing was done"
    );
}

#[tokio::test]
async fn an_order_answered_with_a_server_error_is_not_reported_sent() {
    let url = answering("502 Bad Gateway", r#"{"error":"upstream unavailable"}"#).await;
    let sent = lighter::Api::new(url).send_tx(14, "{}").await;
    assert!(
        sent.is_err(),
        "the exchange never took the order, and it was reported sent with the hash {sent:?}"
    );
    assert!(
        !lighter::api::not_taken(&sent.unwrap_err()),
        "a server error may have come after the exchange took it: never reported refused"
    );
}

#[tokio::test]
async fn an_answer_with_no_hash_is_not_reported_sent() {
    let url = answering("200 OK", r#"{"code":200}"#).await;
    let sent = lighter::Api::new(url).send_tx(14, "{}").await;
    assert!(sent.is_err(), "{sent:?}");
}

#[tokio::test]
async fn the_exchange_s_own_refusal_is_reported_refused() {
    let url = answering(
        "400 Bad Request",
        r#"{"code":21120,"message":"invalid nonce"}"#,
    )
    .await;
    let sent = lighter::Api::new(url).send_tx(14, "{}").await;
    assert!(lighter::api::not_taken(&sent.unwrap_err()));
}

#[tokio::test]
async fn an_order_the_exchange_took_is_reported_sent_with_its_hash() {
    let url = answering("200 OK", r#"{"code":200,"tx_hash":"0xfeed"}"#).await;
    let sent = lighter::Api::new(url).send_tx(14, "{}").await;
    assert_eq!(sent.unwrap(), "0xfeed");
}

#[tokio::test]
async fn a_read_turned_away_is_an_error_never_an_empty_answer() {
    // A station's list of accounts read from a 429 would otherwise name
    // none of its sub-accounts, and the post would leave them out.
    let url = answering(
        "429 Too Many Requests",
        r#"{"message":"Too Many Requests"}"#,
    )
    .await;
    assert!(lighter::Api::new(url).accounts_of("0x01").await.is_err());
    let url = answering("200 OK", r#"{"sub_accounts":[]}"#).await;
    assert!(
        lighter::Api::new(url).accounts_of("0x01").await.is_err(),
        "an answer without the exchange's own code 200 is not read"
    );
}
