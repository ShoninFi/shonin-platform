//! An agent's network: `wasi:http` requests and WebSockets to any host,
//! outbound only, with TLS ended here.
//!
//! The agent holds no socket and no certificate. It names a URL; the
//! executor resolves it, connects, checks the host's certificate against the
//! roots compiled into the image, and hands the agent only what the host
//! sent. Nothing connects in, and nothing is reconnected on the agent's
//! behalf.
//!
//! An agent's manifest declares `network` to be given any of this, and then
//! may reach any host on the public internet. Addresses inside the machine
//! or its network (loopback, private ranges, link-local, the cloud's
//! metadata service among them) are refused, whatever name resolves to them:
//! they are the executor's, not the agent's.
//!
//! Each vault is held to its allowances:
//!
//! - **connections**: requests in flight, WebSockets open and shared streams
//!   subscribed together, at most the `streams` allowance;
//! - **rate**: requests made, WebSockets opened and messages sent, at most
//!   the `requests_per_min` allowance in any minute;
//! - **size**: each WebSocket message, and each request or response body,
//!   at most [`MESSAGE_MOST`];
//! - **bandwidth**: every byte sent and received (bodies and messages),
//!   at most the `bytes_per_day` allowance in a day.
//!
//! Past any of them the call is refused, or the connection or body ends with
//! an error, and the agent decides what to do. The day's bytes and the
//! minute's requests are the vault's, so starting the agent again doesn't
//! give it a fresh day.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use tokio::net::TcpStream;
use wasmtime_wasi_http::{WasiBody, WasiHttpHooks};

/// The most one WebSocket message, or one request or response body, may
/// hold.
pub const MESSAGE_MOST: usize = 4 * 1024 * 1024;

/// How long a connection, its TLS and its handshake are given.
const CONNECT_WITHIN: Duration = Duration::from_secs(20);

/// How long a request waits for the first byte of its answer, and between
/// bytes after it.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// How many messages from a host wait for the agent to read them before the
/// connection stops reading from the host.
const WAITING_MOST: usize = 256;

/// What an agent is held to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quotas {
    /// Requests, WebSockets opened and messages sent, in any minute.
    pub per_minute: u32,
    /// Bytes sent and received in a day.
    pub per_day: u64,
    /// Connections and shared streams open at once.
    pub open: u32,
    /// The most one message or body may hold.
    pub message_most: usize,
}

impl Quotas {
    pub fn of(allowances: &shared::Allowances) -> Self {
        Self {
            per_minute: allowances.requests_per_min,
            per_day: allowances.bytes_per_day,
            open: allowances.streams,
            message_most: MESSAGE_MOST,
        }
    }
}

/// Which of its quotas an agent ran into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Over {
    Rate(u32),
    Open(u32),
    Bandwidth(u64),
    Size(usize),
}

impl std::fmt::Display for Over {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Over::Rate(most) => write!(f, "over the allowance of {most} requests a minute"),
            Over::Open(most) => write!(f, "over the allowance of {most} open connections"),
            Over::Bandwidth(most) => write!(f, "over the allowance of {most} bytes a day"),
            Over::Size(most) => write!(f, "a message over {most} bytes"),
        }
    }
}

/// What one vault has used. Kept for as long as the executor runs, so an
/// agent started again carries on from its vault's day and minute.
#[derive(Debug)]
pub struct Usage {
    inner: Mutex<Used>,
}

#[derive(Debug)]
struct Used {
    /// When the current day of bandwidth began.
    day: Instant,
    bytes: u64,
    /// When each request in the last minute was made.
    recent: VecDeque<Instant>,
    open: u32,
}

const MINUTE: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

impl Default for Usage {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Used {
                day: Instant::now(),
                bytes: 0,
                recent: VecDeque::new(),
                open: 0,
            }),
        }
    }
}

/// Each vault's usage, by its chain and address.
static USAGE: OnceLock<Mutex<ByVault>> = OnceLock::new();

/// Usage by each vault's chain and address.
type ByVault = HashMap<(String, String), Arc<Usage>>;

/// The usage of the vault at `vault` on `chain`, the same for every agent it
/// runs while this executor does.
pub fn usage_of(chain: &str, vault: &str) -> Arc<Usage> {
    let all = USAGE.get_or_init(Default::default);
    let mut all = all.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    all.entry((chain.to_string(), vault.to_string()))
        .or_default()
        .clone()
}

impl Usage {
    fn used(&self) -> std::sync::MutexGuard<'_, Used> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Count one request (or connection opened, or message sent), refused
    /// if the last minute already holds the allowance's worth.
    pub fn request(&self, quotas: &Quotas, now: Instant) -> Result<(), Over> {
        let mut used = self.used();
        while used
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= MINUTE)
        {
            used.recent.pop_front();
        }
        if used.recent.len() >= quotas.per_minute as usize {
            return Err(Over::Rate(quotas.per_minute));
        }
        used.recent.push_back(now);
        Ok(())
    }

    /// Take one of the open slots, given back when the slot is dropped.
    pub fn open(self: &Arc<Self>, quotas: &Quotas) -> Result<Slot, Over> {
        let mut used = self.used();
        if used.open >= quotas.open {
            return Err(Over::Open(quotas.open));
        }
        used.open += 1;
        Ok(Slot(self.clone()))
    }

    /// Count `bytes` moved, refused (and not counted) if they would take the
    /// day past its allowance.
    pub fn bytes(&self, quotas: &Quotas, bytes: u64, now: Instant) -> Result<(), Over> {
        let mut used = self.used();
        if now.saturating_duration_since(used.day) >= DAY {
            used.day = now;
            used.bytes = 0;
        }
        let total = used.bytes.saturating_add(bytes);
        if total > quotas.per_day {
            return Err(Over::Bandwidth(quotas.per_day));
        }
        used.bytes = total;
        Ok(())
    }

    /// The bytes counted in the current day.
    #[cfg(test)]
    pub fn bytes_today(&self) -> u64 {
        self.used().bytes
    }

    #[cfg(test)]
    pub fn open_now(&self) -> u32 {
        self.used().open
    }
}

/// One open connection or stream, counted against the vault's allowance
/// until it is dropped.
#[derive(Debug)]
pub struct Slot(Arc<Usage>);

impl Drop for Slot {
    fn drop(&mut self) {
        let mut used = self.0.used();
        used.open = used.open.saturating_sub(1);
    }
}

/// What an agent's network may do, and what it has used.
#[derive(Clone)]
pub struct Net {
    /// Whether the manifest declares `network`.
    pub declared: bool,
    pub quotas: Quotas,
    pub usage: Arc<Usage>,
    pub reach: Reach,
}

/// Which addresses an agent may reach.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reach {
    /// The public internet only.
    Public,
    /// Anything, the machine's own addresses included. Only for tests
    /// against a server on this machine.
    #[cfg(test)]
    Anywhere,
}

impl Net {
    fn check_declared(&self) -> Result<(), String> {
        if self.declared {
            Ok(())
        } else {
            Err("this agent's manifest doesn't declare `network`".to_string())
        }
    }

    /// Admit one new connection: declared, within the rate and with a slot
    /// free.
    fn admit(&self) -> Result<Slot, String> {
        self.check_declared()?;
        self.usage
            .request(&self.quotas, Instant::now())
            .map_err(|over| over.to_string())?;
        self.usage
            .open(&self.quotas)
            .map_err(|over| over.to_string())
    }

    fn count(&self, bytes: usize) -> Result<(), Over> {
        if bytes > self.quotas.message_most {
            return Err(Over::Size(self.quotas.message_most));
        }
        self.usage.bytes(&self.quotas, bytes as u64, Instant::now())
    }
}

/// Whether an agent may reach `ip`: anything on the public internet, and
/// nothing that is this machine's or its network's.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let first = ip.segments()[0];
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                // Unique local, fc00::/7.
                || (first & 0xfe00) == 0xfc00
                // Link-local, fe80::/10.
                || (first & 0xffc0) == 0xfe80
                // Documentation, 2001:db8::/32.
                || (first == 0x2001 && ip.segments()[1] == 0x0db8)
                // The old IPv4-compatible and the NAT64 well-known prefix,
                // which reach IPv4 addresses that are checked above.
                || ip.segments()[..6] == [0; 6]
                || ip.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0])
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // This network, 0.0.0.0/8.
        || a == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (b & 0xc0) == 64)
        // Protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b & 0xfe) == 18)
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

/// Where `host` is, on `port`: one address the agent may reach. A host any
/// of whose addresses it may not reach is refused altogether, so a name
/// can't be made to answer one way when checked and another when used: the
/// connection is made to the address checked.
async fn resolve(host: &str, port: u16, reach: Reach) -> Result<SocketAddr, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::time::timeout(CONNECT_WITHIN, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| format!("looking up {host} took too long"))?
            .map_err(|error| format!("looking up {host}: {error}"))?
            .collect(),
    };
    let allowed = |address: &SocketAddr| match reach {
        Reach::Public => public(address.ip()),
        #[cfg(test)]
        Reach::Anywhere => true,
    };
    if let Some(refused) = addresses.iter().find(|address| !allowed(address)) {
        return Err(format!(
            "{host} is at {}, which is not on the public internet",
            refused.ip()
        ));
    }
    addresses
        .into_iter()
        .next()
        .ok_or_else(|| format!("{host} has no address"))
}

/// The roots every host's certificate is checked against: the ones compiled
/// into the image.
pub(crate) fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

/// A TCP connection, or a TLS one over it, as one type.
trait Wire: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static {}
impl<T> Wire for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static {}

/// Connect to `host` on `port`, with TLS ended here if `secure`.
async fn connect(
    host: &str,
    port: u16,
    secure: bool,
    reach: Reach,
) -> Result<Box<dyn Wire>, String> {
    let address = resolve(host, port, reach).await?;
    let tcp = tokio::time::timeout(CONNECT_WITHIN, TcpStream::connect(address))
        .await
        .map_err(|_| format!("connecting to {host} took too long"))?
        .map_err(|error| format!("connecting to {host}: {error}"))?;
    if !secure {
        return Ok(Box::new(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
    )
    .map_err(|error| format!("{host}: {error}"))?;
    let tls = tokio::time::timeout(
        CONNECT_WITHIN,
        tokio_rustls::TlsConnector::from(tls()).connect(name, tcp),
    )
    .await
    .map_err(|_| format!("TLS with {host} took too long"))?
    .map_err(|error| format!("TLS with {host}: {error}"))?;
    Ok(Box::new(tls))
}

// HTTP ------------------------------------------------------------------------

/// The executor's side of an agent's `wasi:http`: every request it sends is
/// checked and counted here, and sent from here.
pub struct Http {
    pub net: Net,
}

impl WasiHttpHooks for Http {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        options: Option<wasmtime_wasi_http::RequestOptions>,
        _fut: Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
    ) -> Box<
        dyn Future<
                Output = wasmtime_wasi_http::Result<(
                    http::Response<WasiBody>,
                    Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
                )>,
            > + Send,
    > {
        let net = self.net.clone();
        Box::new(send(net, request, options))
    }
}

type HttpError = wasmtime_wasi_http::Error;

async fn send(
    net: Net,
    request: http::Request<WasiBody>,
    options: Option<wasmtime_wasi_http::RequestOptions>,
) -> wasmtime_wasi_http::Result<(
    http::Response<WasiBody>,
    Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
)> {
    let slot = net.admit().map_err(|why| {
        tracing::debug!(why, "a request was refused");
        HttpError::HttpRequestDenied
    })?;
    let uri = request.uri().clone();
    let secure = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err(HttpError::HttpProtocolError),
    };
    let host = uri.host().ok_or(HttpError::HttpRequestUriInvalid)?;
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let wire = connect(host, port, secure, net.reach)
        .await
        .map_err(|why| {
            if why.contains("not on the public internet") {
                HttpError::DestinationIpProhibited
            } else if why.contains("looking up") || why.contains("has no address") {
                HttpError::DestinationNotFound
            } else if why.contains("TLS") {
                HttpError::TlsProtocolError
            } else {
                HttpError::ConnectionRefused
            }
        })?;
    let within = |chosen: Option<Duration>| chosen.unwrap_or(ANSWER_WITHIN).min(ANSWER_WITHIN);
    let first_byte = within(options.and_then(|options| options.first_byte_timeout));
    let between = within(options.and_then(|options| options.between_bytes_timeout));

    let (mut sender, connection) = tokio::time::timeout(
        CONNECT_WITHIN,
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(wire)),
    )
    .await
    .map_err(|_| HttpError::ConnectionTimeout)??;
    let driving = tokio::spawn(connection);

    // Sent as the path alone: the host is in the `Host` header, and a full
    // URI is only for proxies.
    let (mut parts, body) = request.into_parts();
    if !parts.headers.contains_key(http::header::HOST) {
        let authority = uri.authority().map(|a| a.as_str()).unwrap_or(host);
        if let Ok(value) = http::HeaderValue::from_str(authority) {
            parts.headers.insert(http::header::HOST, value);
        }
    }
    parts.uri = http::Uri::builder()
        .path_and_query(uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"))
        .build()
        .map_err(|_| HttpError::HttpRequestUriInvalid)?;
    let sent_body = Counted::new(body, net.clone(), Side::Sent);
    let request = http::Request::from_parts(parts, sent_body);

    let response = match tokio::time::timeout(first_byte, sender.send_request(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            driving.abort();
            return Err(error.into());
        }
        Err(_) => {
            driving.abort();
            return Err(HttpError::ConnectionReadTimeout);
        }
    };
    let response = response.map(|incoming| {
        let incoming = incoming.map_err(HttpError::from).boxed_unsync();
        Counted::new(incoming, net.clone(), Side::Received)
            .between(between)
            .holding(slot)
            .boxed_unsync()
    });
    let io = async move {
        match driving.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(HttpError::from(error)),
            Err(_) => Ok(()),
        }
    };
    Ok((response, Box::new(io)))
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Sent,
    Received,
}

/// A body, counted against the vault's bandwidth as it goes and cut off
/// past [`Quotas::message_most`].
struct Counted {
    body: WasiBody,
    net: Net,
    side: Side,
    so_far: usize,
    /// How long it may go quiet.
    between: Option<Duration>,
    quiet: Option<Pin<Box<tokio::time::Sleep>>>,
    /// The connection's slot, held until the body is done.
    _slot: Option<Slot>,
}

impl Counted {
    fn new(body: WasiBody, net: Net, side: Side) -> Self {
        Self {
            body,
            net,
            side,
            so_far: 0,
            between: None,
            quiet: None,
            _slot: None,
        }
    }

    fn between(mut self, between: Duration) -> Self {
        self.between = Some(between);
        self
    }

    fn holding(mut self, slot: Slot) -> Self {
        self._slot = Some(slot);
        self
    }

    fn too_big(&self) -> HttpError {
        let most = Some(self.net.quotas.message_most as u64);
        match self.side {
            Side::Sent => HttpError::HttpRequestBodySize(most),
            Side::Received => HttpError::HttpResponseBodySize(most),
        }
    }
}

impl http_body::Body for Counted {
    type Data = Bytes;
    type Error = HttpError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, HttpError>>> {
        let polled = Pin::new(&mut self.body).poll_frame(cx);
        match polled {
            Poll::Ready(Some(Ok(frame))) => {
                self.quiet = None;
                if let Some(data) = frame.data_ref() {
                    let size = data.len();
                    self.so_far = self.so_far.saturating_add(size);
                    if self.so_far > self.net.quotas.message_most {
                        return Poll::Ready(Some(Err(self.too_big())));
                    }
                    if let Err(over) = self.net.count(size) {
                        tracing::debug!(%over, "a body was cut off");
                        return Poll::Ready(Some(Err(match over {
                            Over::Size(_) => self.too_big(),
                            _ => HttpError::ConnectionLimitReached,
                        })));
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Pending => {
                if let Some(between) = self.between {
                    let quiet = self
                        .quiet
                        .get_or_insert_with(|| Box::pin(tokio::time::sleep(between)));
                    if quiet.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Some(Err(HttpError::ConnectionReadTimeout)));
                    }
                }
                Poll::Pending
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
}

// WebSockets ------------------------------------------------------------------

/// A message either way on a WebSocket.
pub use crate::host::shonin::agent::websocket::Message;

/// An agent's open WebSocket. Dropping it closes the connection.
pub struct Connection {
    outgoing: tokio::sync::mpsc::Sender<Message>,
    /// Taken by the first call to `messages`.
    incoming: Option<tokio::sync::mpsc::Receiver<Message>>,
    net: Net,
    _task: crate::AbortOnDrop,
}

impl Connection {
    /// Open a WebSocket to `url` (`ws://` or `wss://`), with `headers` added
    /// to the handshake.
    pub async fn open(
        net: &Net,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Connection, String> {
        let slot = net.admit()?;
        let parsed: http::Uri = url
            .parse()
            .map_err(|error| format!("{url} is not a URL: {error}"))?;
        let secure = match parsed.scheme_str() {
            Some("wss") => true,
            Some("ws") => false,
            _ => return Err(format!("{url} is not a ws:// or wss:// URL")),
        };
        let host = parsed
            .host()
            .ok_or_else(|| format!("{url} names no host"))?
            .to_string();
        let port = parsed.port_u16().unwrap_or(if secure { 443 } else { 80 });
        let wire = connect(&host, port, secure, net.reach).await?;

        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url
            .into_client_request()
            .map_err(|error| format!("{url}: {error}"))?;
        for (name, value) in headers {
            let name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("header {name}: {error}"))?;
            let value = http::HeaderValue::from_str(value)
                .map_err(|error| format!("header {name}: {error}"))?;
            request.headers_mut().append(name, value);
        }
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(net.quotas.message_most))
            .max_frame_size(Some(net.quotas.message_most));
        let (socket, _) = tokio::time::timeout(
            CONNECT_WITHIN,
            tokio_tungstenite::client_async_with_config(request, wire, Some(config)),
        )
        .await
        .map_err(|_| format!("the handshake with {host} took too long"))?
        .map_err(|error| format!("the handshake with {host}: {error}"))?;

        let (outgoing, to_send) = tokio::sync::mpsc::channel(WAITING_MOST);
        let (received, incoming) = tokio::sync::mpsc::channel(WAITING_MOST);
        let task = crate::AbortOnDrop(tokio::spawn(pump(
            socket,
            to_send,
            received,
            net.clone(),
            slot,
        )));
        Ok(Connection {
            outgoing,
            incoming: Some(incoming),
            net: net.clone(),
            _task: task,
        })
    }

    /// What sends on this connection, held apart from it so a send can
    /// wait without holding the connection.
    pub fn sender(&self) -> Outgoing {
        Outgoing {
            outgoing: self.outgoing.clone(),
            net: self.net.clone(),
        }
    }

    /// Send one message, counted against the rate and the bandwidth.
    #[cfg(test)]
    pub async fn send(&self, message: Message) -> Result<(), String> {
        self.sender().send(message).await
    }

    /// What the host sends, once: a second call is handed a stream that has
    /// already ended.
    pub fn messages(&mut self) -> Option<tokio::sync::mpsc::Receiver<Message>> {
        self.incoming.take()
    }
}

/// The sending half of a connection.
pub struct Outgoing {
    outgoing: tokio::sync::mpsc::Sender<Message>,
    net: Net,
}

impl Outgoing {
    /// Send one message, counted against the rate and the bandwidth.
    pub async fn send(&self, message: Message) -> Result<(), String> {
        let size = match &message {
            Message::Text(text) => text.len(),
            Message::Binary(bytes) => bytes.len(),
        };
        if size > self.net.quotas.message_most {
            return Err(Over::Size(self.net.quotas.message_most).to_string());
        }
        self.net
            .usage
            .request(&self.net.quotas, Instant::now())
            .map_err(|over| over.to_string())?;
        self.net.count(size).map_err(|over| over.to_string())?;
        self.outgoing
            .send(message)
            .await
            .map_err(|_| "the connection is closed".to_string())
    }
}

/// Carry messages both ways until either side closes, counting what comes
/// in. A host that sends faster than the agent reads waits: nothing is
/// read from it while [`WAITING_MOST`] messages wait.
async fn pump<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    mut to_send: tokio::sync::mpsc::Receiver<Message>,
    received: tokio::sync::mpsc::Sender<Message>,
    net: Net,
    _slot: Slot,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio_tungstenite::tungstenite::Message as Frame;
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            outgoing = to_send.recv() => {
                let Some(message) = outgoing else {
                    let _ = sink.send(Frame::Close(None)).await;
                    return;
                };
                let frame = match message {
                    Message::Text(text) => Frame::text(text),
                    Message::Binary(bytes) => Frame::binary(bytes),
                };
                if sink.send(frame).await.is_err() {
                    return;
                }
            }
            incoming = stream.next() => {
                let message = match incoming {
                    Some(Ok(Frame::Text(text))) => Message::Text(text.as_str().to_string()),
                    Some(Ok(Frame::Binary(bytes))) => Message::Binary(bytes.to_vec()),
                    // Answered by the library; nothing for the agent.
                    Some(Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_))) => continue,
                    Some(Ok(Frame::Close(_))) | Some(Err(_)) | None => return,
                };
                let size = match &message {
                    Message::Text(text) => text.len(),
                    Message::Binary(bytes) => bytes.len(),
                };
                if let Err(over) = net.count(size) {
                    tracing::debug!(%over, "a WebSocket was closed");
                    let _ = sink.send(Frame::Close(None)).await;
                    return;
                }
                if received.send(message).await.is_err() {
                    // The agent let go of the stream.
                    let _ = sink.send(Frame::Close(None)).await;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quotas() -> Quotas {
        Quotas {
            per_minute: 3,
            per_day: 100,
            open: 2,
            message_most: 40,
        }
    }

    fn net(declared: bool) -> Net {
        Net {
            declared,
            quotas: quotas(),
            usage: Arc::new(Usage::default()),
            reach: Reach::Anywhere,
        }
    }

    #[test]
    fn requests_past_the_rate_are_refused_until_a_minute_has_passed() {
        let usage = Usage::default();
        let start = Instant::now();
        for _ in 0..3 {
            usage.request(&quotas(), start).unwrap();
        }
        assert_eq!(usage.request(&quotas(), start), Err(Over::Rate(3)));
        assert_eq!(
            usage.request(&quotas(), start + Duration::from_secs(59)),
            Err(Over::Rate(3))
        );
        usage.request(&quotas(), start + MINUTE).unwrap();
    }

    #[test]
    fn connections_past_the_allowance_are_refused_until_one_closes() {
        let usage = Arc::new(Usage::default());
        let first = usage.open(&quotas()).unwrap();
        let _second = usage.open(&quotas()).unwrap();
        assert_eq!(usage.open(&quotas()).unwrap_err(), Over::Open(2));
        drop(first);
        assert_eq!(usage.open_now(), 1);
        usage.open(&quotas()).unwrap();
    }

    #[test]
    fn bytes_past_the_day_s_allowance_are_refused_and_not_counted() {
        let usage = Usage::default();
        let start = Instant::now();
        usage.bytes(&quotas(), 60, start).unwrap();
        assert_eq!(usage.bytes(&quotas(), 41, start), Err(Over::Bandwidth(100)));
        assert_eq!(usage.bytes_today(), 60);
        usage.bytes(&quotas(), 40, start).unwrap();
        // A new day starts the count again.
        usage.bytes(&quotas(), 100, start + DAY).unwrap();
    }

    #[test]
    fn a_message_over_the_size_limit_is_refused() {
        let net = net(true);
        assert_eq!(net.count(41), Err(Over::Size(40)));
        assert_eq!(net.usage.bytes_today(), 0);
        net.count(40).unwrap();
    }

    #[test]
    fn the_day_and_the_minute_are_the_vault_s_across_restarts() {
        let one = usage_of("base", "0xabc");
        one.bytes(&quotas(), 70, Instant::now()).unwrap();
        // The agent started again is handed the same usage.
        assert_eq!(usage_of("base", "0xabc").bytes_today(), 70);
        // The same address on another chain is another vault.
        assert_eq!(usage_of("hyperliquid", "0xabc").bytes_today(), 0);
    }

    #[test]
    fn only_the_public_internet_is_reachable() {
        for refused in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
        ] {
            assert!(!public(refused.parse().unwrap()), "{refused} was reachable");
        }
        for allowed in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(public(allowed.parse().unwrap()), "{allowed} was refused");
        }
    }

    #[tokio::test]
    async fn a_name_resolving_inside_the_machine_is_refused() {
        let refused = resolve("localhost", 80, Reach::Public).await.unwrap_err();
        assert!(refused.contains("not on the public internet"), "{refused}");
        let refused = resolve("169.254.169.254", 80, Reach::Public)
            .await
            .unwrap_err();
        assert!(refused.contains("not on the public internet"), "{refused}");
    }

    #[tokio::test]
    async fn an_agent_that_does_not_declare_network_opens_nothing() {
        let refused = Connection::open(&net(false), "ws://127.0.0.1:1", &[])
            .await
            .err()
            .unwrap();
        assert!(refused.contains("doesn't declare `network`"), "{refused}");
    }

    /// An HTTP server on this machine that answers every request with
    /// `body`.
    async fn answering(body: &'static str) -> SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let _ = tcp.read(&mut request).await;
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tcp.write_all(answer.as_bytes()).await;
                });
            }
        });
        address
    }

    fn get(url: &str) -> http::Request<WasiBody> {
        let empty = http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        http::Request::get(url).body(empty).unwrap()
    }

    #[tokio::test]
    async fn a_request_is_answered_and_its_bytes_counted() {
        let address = answering("hello").await;
        let net = net(true);
        let (response, _io) = send(net.clone(), get(&format!("http://{address}/")), None)
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        // In flight until its body is read.
        assert_eq!(net.usage.open_now(), 1);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"hello");
        assert_eq!(net.usage.bytes_today(), 5);
        assert_eq!(net.usage.open_now(), 0, "the slot was given back");
    }

    #[tokio::test]
    async fn a_response_body_over_the_size_limit_is_cut_off() {
        let address = answering("this answer is well over forty bytes long, by some way").await;
        let (response, _io) = send(net(true), get(&format!("http://{address}/")), None)
            .await
            .unwrap();
        let cut = response.into_body().collect().await.err().unwrap();
        assert!(
            matches!(cut, HttpError::HttpResponseBodySize(Some(40))),
            "{cut:?}"
        );
    }

    #[tokio::test]
    async fn requests_are_refused_undeclared_inside_the_machine_or_past_the_rate() {
        let address = answering("ok").await;
        let url = format!("http://{address}/");
        let refused = send(net(false), get(&url), None).await.err().unwrap();
        assert!(
            matches!(refused, HttpError::HttpRequestDenied),
            "{refused:?}"
        );

        let public_only = Net {
            reach: Reach::Public,
            ..net(true)
        };
        let refused = send(public_only, get(&url), None).await.err().unwrap();
        assert!(
            matches!(refused, HttpError::DestinationIpProhibited),
            "{refused:?}"
        );

        let net = net(true);
        for _ in 0..3 {
            let (response, _io) = send(net.clone(), get(&url), None).await.unwrap();
            response.into_body().collect().await.unwrap();
        }
        let refused = send(net, get(&url), None).await.err().unwrap();
        assert!(
            matches!(refused, HttpError::HttpRequestDenied),
            "{refused:?}"
        );
    }

    /// Against Hyperliquid's public testnet, so not run by default: TLS
    /// ended here, over HTTPS and a secure WebSocket.
    #[tokio::test]
    #[ignore = "reaches Hyperliquid's public testnet"]
    async fn tls_to_a_public_host_is_ended_here() {
        let net = Net {
            reach: Reach::Public,
            quotas: Quotas {
                per_minute: 10,
                per_day: 10_000_000,
                open: 4,
                message_most: MESSAGE_MOST,
            },
            ..net(true)
        };
        let body = http_body_util::Full::new(Bytes::from_static(br#"{"type":"meta"}"#))
            .map_err(|never| match never {})
            .boxed_unsync();
        let request = http::Request::post("https://api.hyperliquid-testnet.xyz/info")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();
        let (response, _io) = send(net.clone(), request, None).await.unwrap();
        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.starts_with(b"{\"universe\""), "{body:?}");

        let mut connection = Connection::open(&net, HYPERLIQUID_WS, &[]).await.unwrap();
        let mut messages = connection.messages().unwrap();
        connection
            .send(Message::Text(r#"{"method":"ping"}"#.to_string()))
            .await
            .unwrap();
        let pong = messages.recv().await.unwrap();
        assert!(matches!(&pong, Message::Text(text) if text.contains("pong")));
    }

    const HYPERLIQUID_WS: &str = "wss://api.hyperliquid-testnet.xyz/ws";

    /// A WebSocket server on this machine that echoes what it is sent.
    async fn echo() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    while let Some(Ok(frame)) = socket.next().await {
                        if frame.is_text() || frame.is_binary() {
                            let _ = socket.send(frame).await;
                        }
                    }
                });
            }
        });
        address
    }

    #[tokio::test]
    async fn a_websocket_carries_messages_both_ways_and_counts_them() {
        let address = echo().await;
        let net = net(true);
        let mut connection = Connection::open(&net, &format!("ws://{address}"), &[])
            .await
            .unwrap();
        let mut messages = connection.messages().unwrap();
        connection
            .send(Message::Text("hello".to_string()))
            .await
            .unwrap();
        assert!(matches!(messages.recv().await, Some(Message::Text(text)) if text == "hello"));
        // Five out and five back.
        assert_eq!(net.usage.bytes_today(), 10);
        assert_eq!(net.usage.open_now(), 1);
        // Too big to send.
        let refused = connection
            .send(Message::Binary(vec![0; 41]))
            .await
            .unwrap_err();
        assert!(refused.contains("over 40 bytes"), "{refused}");
        // The rate: the connection and one message so far, one more fits.
        connection
            .send(Message::Text("x".to_string()))
            .await
            .unwrap();
        let refused = connection
            .send(Message::Text("y".to_string()))
            .await
            .unwrap_err();
        assert!(refused.contains("requests a minute"), "{refused}");
        drop(messages);
        drop(connection);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(net.usage.open_now(), 0, "the slot was given back");
    }

    #[tokio::test]
    async fn a_websocket_past_the_open_allowance_is_refused() {
        let address = echo().await;
        let net = Net {
            quotas: Quotas {
                per_minute: 100,
                ..quotas()
            },
            ..net(true)
        };
        let url = format!("ws://{address}");
        let _one = Connection::open(&net, &url, &[]).await.unwrap();
        let _two = Connection::open(&net, &url, &[]).await.unwrap();
        let refused = Connection::open(&net, &url, &[]).await.err().unwrap();
        assert!(refused.contains("open connections"), "{refused}");
    }
}
