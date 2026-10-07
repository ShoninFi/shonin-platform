//! Reading an answer's body with a bound on its size, checked as it streams
//! in: an answer too long is dropped as soon as it passes the bound, never
//! read whole first.

use anyhow::{Context, Result, bail};

/// The body of `answer`, refused once it is longer than `most` bytes.
pub async fn read_at_most(mut answer: reqwest::Response, most: usize) -> Result<Vec<u8>> {
    if answer
        .content_length()
        .is_some_and(|length| length > most as u64)
    {
        bail!("the answer is longer than {most} bytes");
    }
    let mut body = Vec::new();
    while let Some(chunk) = answer.chunk().await.context("reading the answer")? {
        if body.len() + chunk.len() > most {
            bail!("the answer is longer than {most} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A server whose answer never ends: chunk after chunk, no length given.
    async fn endless() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut asked = [0u8; 1024];
                    let _ = socket.read(&mut asked).await;
                    let head = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n";
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let chunk = format!("400\r\n{}\r\n", "x".repeat(1024));
                    while socket.write_all(chunk.as_bytes()).await.is_ok() {}
                });
            }
        });
        url
    }

    /// A short answer, with its length given.
    async fn short(body: &'static str) -> String {
        let app = axum::Router::new().route("/", axum::routing::get(move || async move { body }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// An answer that never ends is refused as soon as it passes the bound:
    /// read whole first, it would never be refused at all.
    #[tokio::test]
    async fn an_answer_past_the_bound_is_refused_while_it_streams() {
        let answer = reqwest::get(endless().await).await.unwrap();
        let read = tokio::time::timeout(Duration::from_secs(20), read_at_most(answer, 64 * 1024))
            .await
            .expect("the endless answer was read on past its bound");
        assert!(read.is_err());
    }

    #[tokio::test]
    async fn an_answer_within_the_bound_is_read_whole() {
        let answer = reqwest::get(short("a short answer").await).await.unwrap();
        assert_eq!(
            read_at_most(answer, 64).await.unwrap(),
            b"a short answer".to_vec()
        );
        // One longer than the bound, its length given, is refused unread.
        let answer = reqwest::get(short("a short answer").await).await.unwrap();
        assert!(read_at_most(answer, 4).await.is_err());
    }
}
