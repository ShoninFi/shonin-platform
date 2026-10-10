//! Only protocol refusals that prove the peer did not process the request.

/// Match the HTTP client's protocol-safe retry cases using typed causes,
/// before errors are scrubbed. Never retry an ambiguous connection failure.
pub(crate) fn unprocessed(mut error: &(dyn std::error::Error + 'static)) -> bool {
    loop {
        if let Some(error) = error.downcast_ref::<h2::Error>() {
            return error.is_remote()
                && ((error.is_go_away() && error.reason() == Some(h2::Reason::NO_ERROR))
                    || (error.is_reset() && error.reason() == Some(h2::Reason::REFUSED_STREAM)));
        }
        match error.source() {
            Some(cause) => error = cause,
            None => return false,
        }
    }
}

/// The HTTP client's original bound, above the counted request boundary.
pub(crate) const EXTRA_TRIES: usize = 2;

#[cfg(test)]
pub(crate) async fn refusing_server(
    refusals: usize,
    reason: h2::Reason,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(socket).await.unwrap();
        let mut count = 0;
        while let Some(request) = connection.accept().await {
            let (request, mut response) = request.unwrap();
            let refuse = count < refusals;
            count += 1;
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut body = request.into_body();
                let mut bytes = Vec::new();
                while let Some(chunk) = body.data().await {
                    let chunk = chunk.unwrap();
                    body.flow_control().release_capacity(chunk.len()).unwrap();
                    bytes.extend_from_slice(&chunk);
                }
                seen.lock().unwrap().push(bytes);
                if refuse {
                    response.send_reset(reason);
                } else {
                    response
                        .send_response(http::Response::new(()), false)
                        .unwrap()
                        .send_data(
                            bytes::Bytes::from_static(
                                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"accepted\"}",
                            ),
                            true,
                        )
                        .unwrap();
                }
            });
        }
    });
    (url, bodies, server)
}
