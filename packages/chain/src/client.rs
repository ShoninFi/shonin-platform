//! Talking to the chain.
//!
//! Every service reaches the contracts through this one client, so they all
//! read the same addresses out of the same deployment file and encode calls
//! with the same bindings.
//!
//! How a client signs depends on the chain ([`Signing`]). A local chain
//! unlocks the accounts it publishes, so a service says which account it is
//! and the node signs for it: transactions through `eth_sendTransaction`,
//! messages through `eth_sign`. That is what makes it a development chain,
//! and it means no key is written down anywhere. On a testnet the same roles
//! are accounts derived from a mnemonic the developer holds in `.env`, signed
//! for here. Mainnet has no role keys at all: a client there only reads.

use std::time::Duration;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log, TransactionReceipt, TransactionRequest};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, anyhow, bail};

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::ChainConfig;
use crate::deployments::Deployments;

/// One of the platform's roles, as an account on the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    Platform,
    Attestor,
    Builder,
    Reviewer,
    Executor,
    Keeper,
    Operator,
    Author,
    Viewer,
}

impl Role {
    /// Which of the accounts the chain publishes plays this part.
    pub fn account_index(self) -> usize {
        match self {
            Self::Admin => 0,
            Self::Builder => 1,
            Self::Reviewer => 2,
            Self::Executor => 3,
            Self::Keeper => 4,
            Self::Author => 5,
            Self::Viewer => 6,
            Self::Operator => 7,
            Self::Attestor => 8,
            Self::Platform => 9,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Platform => "platform",
            Self::Attestor => "attestor",
            Self::Builder => "builder",
            Self::Reviewer => "reviewer",
            Self::Executor => "executor",
            Self::Keeper => "keeper",
            Self::Operator => "operator",
            Self::Author => "author",
            Self::Viewer => "viewer",
        }
    }
}

/// Who signs a client's transactions and messages.
pub enum Signing {
    /// The node, for the accounts it unlocked: a development chain.
    Unlocked,
    /// These keys, in role order, signed for here.
    Keys(Vec<PrivateKeySigner>),
    /// The role accounts as a deployment names them, in role order, and the
    /// keys of the few this client signs for here (possibly none). Sending
    /// or signing as any other account is refused at the call.
    Roles {
        accounts: Vec<Address>,
        keys: Vec<PrivateKeySigner>,
    },
    /// Nobody: the client only reads.
    ReadOnly,
}

/// The first accounts a mnemonic derives (`m/44'/60'/0'/0/i`), one per role
/// index, the same way a development chain derives its own.
///
/// The library's own error for a phrase it can't use quotes the phrase, and
/// a service prints the error it fails with, so it is never passed on: the
/// error says only that the phrase is not a valid one.
pub fn keys_from_mnemonic(phrase: &str, count: usize) -> Result<Vec<PrivateKeySigner>> {
    use alloy::signers::local::MnemonicBuilder;
    use alloy::signers::local::coins_bip39::English;
    (0..count as u32)
        .map(|index| {
            MnemonicBuilder::<English>::default()
                .phrase(phrase.trim())
                .index(index)
                .ok()
                .and_then(|builder| builder.build().ok())
                .ok_or_else(|| {
                    anyhow!(
                        "the mnemonic is not a valid BIP-39 phrase (its words, or its checksum)"
                    )
                })
        })
        .collect()
}

/// How many role accounts there are ([`Role::account_index`]).
pub const ROLE_COUNT: usize = 10;

/// Every role, in no particular order.
pub const ROLES: [Role; ROLE_COUNT] = [
    Role::Admin,
    Role::Platform,
    Role::Attestor,
    Role::Builder,
    Role::Reviewer,
    Role::Executor,
    Role::Keeper,
    Role::Operator,
    Role::Author,
    Role::Viewer,
];

/// One canonical header observation, including its event-address bloom.
#[derive(Clone, Debug)]
pub struct HeadObservation {
    pub number: u64,
    pub timestamp: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub logs_bloom: alloy::primitives::Bloom,
}

impl HeadObservation {
    /// A negative address bloom proves that address emitted no log in this
    /// block. Positive results may be false positives and require a log read.
    pub fn may_have_logs(&self, addresses: &[Address]) -> bool {
        addresses.iter().any(|address| {
            self.logs_bloom
                .contains_input(alloy::primitives::BloomInput::Raw(address.as_slice()))
        })
    }
}

/// A connection to the chain, and the addresses of everything on it.
#[derive(Clone)]
pub struct Client {
    policy: crate::rpc_policy::RpcPolicy,
    log_snapshot: Option<Arc<LogSnapshot>>,
    log_verification: Option<Arc<LogVerification>>,
    provider: DynProvider,
    pub rpc_url: String,
    pub chain_id: u64,
    pub accounts: Vec<Address>,
    /// The keys this client signs with itself, when the node doesn't.
    keys: Option<Arc<HashMap<Address, PrivateKeySigner>>>,
    /// One send at a time from each account this client signs for, shared by
    /// every clone. The nonce is asked of the node when a transaction is
    /// filled in, so two sends from one account at once would both take the
    /// same one and the node would keep only one of them.
    turns: Arc<std::sync::Mutex<HashMap<Address, Arc<tokio::sync::Mutex<()>>>>>,
    /// The nonce after each account's last send whose receipt was seen,
    /// shared by every clone and every [`KeyClient`] made from it.
    nonces: NonceFloor,
    /// Where a log query with no numbered range goes: the endpoints
    /// `logs_rpc` names, or the same connection as everything else when it
    /// names none.
    logs: DynProvider,
    /// The same endpoints, each on its own, for log queries over a numbered
    /// range, each checked against its own head.
    log_endpoints: Endpoints,
    /// The most blocks one log query may span, both ends included.
    log_range: Option<u64>,
    /// The `rpc` endpoints, each on its own: for sends, and for reads pinned
    /// to a past block, which only an endpoint that honours the block
    /// answers.
    endpoints: Endpoints,
    /// The provider that fills transactions in and signs them here, when
    /// the client holds the role accounts' keys.
    signing: Option<SigningProvider>,
    /// The chain's name, for errors and logs that can't name an endpoint.
    label: String,
}

struct LogVerification {
    head: u64,
    identities: tokio::sync::Mutex<HashMap<u64, (B256, Option<u64>)>>,
}

/// Only exact numbered ranges and explicitly covered addresses may be read
/// from this observation; no current contract state is stored here.
struct LogSnapshot {
    from: u64,
    to: u64,
    addresses: Vec<Address>,
    logs: Vec<Log>,
}

impl LogSnapshot {
    fn read(&self, filter: &Filter) -> Option<Vec<Log>> {
        let (from, to) = filter.get_from_block().zip(filter.get_to_block())?;
        if from > to
            || from < self.from
            || to > self.to
            || filter.address.is_empty()
            || !filter
                .address
                .iter()
                .all(|address| self.addresses.contains(address))
        {
            return None;
        }
        Some(
            self.logs
                .iter()
                .filter(|log| filter.rpc_matches(log))
                .cloned()
                .collect(),
        )
    }
}

/// The nonce after each account's last send this client saw land.
///
/// A public RPC is often several nodes behind one name, and one a block
/// behind counts the account's sends without the last one: numbered from its
/// count alone, the next send would take the last one's nonce again, and be
/// refused or, worse, replace a send still waiting in the pool. So a send is
/// numbered at the node's count of the account's sends (pending ones
/// included) but never below this. It starts empty: a client that starts
/// again takes the node's count until it has seen a send of its own land.
#[derive(Clone, Default)]
struct NonceFloor(Arc<std::sync::Mutex<HashMap<Address, u64>>>);

impl NonceFloor {
    /// The nonce for `from`'s next send, given the node's count of its sends.
    fn number(&self, from: Address, counted: u64) -> u64 {
        let floor = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&from)
            .copied()
            .unwrap_or(0);
        counted.max(floor)
    }

    /// A send of `from`'s numbered `nonce` landed (reverted or not: either
    /// way the nonce is used).
    fn landed(&self, from: Address, nonce: u64) {
        let mut floors = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = floors.entry(from).or_default();
        *next = (*next).max(nonce.saturating_add(1));
    }

    /// Ask the node for `from`'s count of sends, pending ones included, and
    /// number the next send from it and the floor. A count the node won't
    /// give is a send that can't be numbered safely, so nothing is sent.
    async fn next(&self, provider: &DynProvider, from: Address) -> Result<u64> {
        let counted = provider
            .get_transaction_count(from)
            .pending()
            .await
            .context("counting the account's sends")
            .map_err(NeverSent::wrap)?;
        Ok(self.number(from, counted))
    }
}

/// The fillers a signing client runs: gas, then the nonce, then the chain id.
type SigningFillers = alloy::providers::fillers::JoinFill<
    alloy::providers::fillers::JoinFill<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::GasFiller,
        >,
        alloy::providers::fillers::NonceFiller<alloy::providers::fillers::SimpleNonceManager>,
    >,
    alloy::providers::fillers::ChainIdFiller,
>;

/// A provider that fills a transaction in and signs it with one key.
type SigningProvider = alloy::providers::fillers::FillProvider<
    alloy::providers::fillers::JoinFill<
        SigningFillers,
        alloy::providers::fillers::WalletFiller<EthereumWallet>,
    >,
    alloy::providers::RootProvider,
>;

/// How a client that signs for itself fills a transaction in. The nonce is
/// asked of the chain each time rather than counted here, so a send that
/// fails before it is broadcast leaves no gap that strands every later one.
/// The chain id is `chain_id`, the one checked against the chain config and
/// the deployment file when the client connected, never one a node names at
/// send time: a key signs for the chain it was checked on, and a node that
/// answers another id afterwards gets nothing valid on that other chain.
fn signing_builder(
    chain_id: u64,
) -> ProviderBuilder<alloy::providers::Identity, SigningFillers, alloy::network::Ethereum> {
    ProviderBuilder::default()
        .with_gas_estimation()
        .with_simple_nonce_management()
        .with_chain_id(chain_id)
}

/// A paid RPC URL carries its key, in its path or its query, and an error
/// can name it: the HTTP client's own errors name the URL it was sending to,
/// and a gateway's error page or refusal often names the path it was asked
/// for. Every request goes through this layer, which takes the URL, and any
/// part after the host that could be a key, out of whatever comes back, so no
/// error a service logs or returns can hand the key on.
#[derive(Clone)]
struct KeepUrlOut {
    /// The URL as configured and as the HTTP client writes it, everything
    /// after its host, and each part of that long enough to be a key.
    spellings: Arc<[String]>,
    counted_chain: Option<u64>,
}

/// How long a path segment or query value must be before it is taken for a
/// key. Shorter ones are words such as `v2` or `rpc`, and replacing those
/// would only garble the errors they appear in.
const KEY_LENGTH: usize = 8;

impl KeepUrlOut {
    fn new(configured: &str, parsed: &alloy::transports::http::reqwest::Url) -> Self {
        let mut spellings = vec![configured.to_string(), parsed.as_str().to_string()];
        // Everything after the host, as it was asked for.
        let mut after_host = parsed.path().to_string();
        if let Some(query) = parsed.query() {
            after_host.push('?');
            after_host.push_str(query);
        }
        spellings.push(after_host);
        // A key can ride in the user part as well as the password.
        if !parsed.username().is_empty() {
            spellings.push(parsed.username().to_string());
        }
        if let Some(password) = parsed.password() {
            spellings.push(password.to_string());
        }
        for segment in parsed.path().split('/') {
            spellings.push(segment.to_string());
        }
        for part in parsed.query().unwrap_or_default().split('&') {
            spellings.push(part.to_string());
            if let Some((_, value)) = part.split_once('=') {
                spellings.push(value.to_string());
            }
        }
        if let Some(fragment) = parsed.fragment() {
            spellings.push(fragment.to_string());
        }
        // Each spelling also as the HTTP client may have escaped it, or a
        // gateway unescaped it.
        let unescaped: Vec<String> = spellings
            .iter()
            .filter_map(|spelling| unescape(spelling))
            .collect();
        spellings.extend(unescaped);
        spellings.retain(|spelling| spelling.len() >= KEY_LENGTH);
        // Longest first, so a spelling that contains another goes whole.
        spellings.sort_by_key(|spelling| std::cmp::Reverse(spelling.len()));
        spellings.dedup();
        Self {
            spellings: spellings.into(),
            counted_chain: None,
        }
    }

    fn counted_on(mut self, chain: Option<u64>) -> Self {
        self.counted_chain = chain;
        self
    }

    fn scrub(&self, text: &str) -> String {
        self.spellings.iter().fold(text.to_string(), |text, url| {
            text.replace(url.as_str(), "<rpc>")
        })
    }

    /// An error from the transport, with the URL taken out of whatever text
    /// it carries. It stays the same kind of error, so a caller tells an
    /// unanswered send from a refused one the same way.
    fn scrub_error(
        &self,
        error: alloy::transports::TransportError,
    ) -> alloy::transports::TransportError {
        use alloy::transports::{RpcError, TransportErrorKind};
        match error {
            RpcError::Transport(TransportErrorKind::Custom(inner)) => {
                TransportErrorKind::custom_str(&self.scrub(&inner.to_string()))
            }
            RpcError::Transport(TransportErrorKind::HttpError(mut http)) => {
                http.body = self.scrub(&http.body);
                RpcError::Transport(TransportErrorKind::HttpError(http))
            }
            RpcError::Transport(TransportErrorKind::HttpErrorWithRetryAfter {
                mut error,
                retry_after,
            }) => {
                error.body = self.scrub(&error.body);
                RpcError::Transport(TransportErrorKind::HttpErrorWithRetryAfter {
                    error,
                    retry_after,
                })
            }
            // A reply that didn't parse: both the text and the parser's
            // message, which quotes what it could not read.
            RpcError::DeserError { err, text } => {
                let message = err.to_string();
                let scrubbed = self.scrub(&message);
                RpcError::DeserError {
                    err: if scrubbed == message {
                        err
                    } else {
                        <serde_json::Error as serde::de::Error>::custom(scrubbed)
                    },
                    text: self.scrub(&text),
                }
            }
            // A refusal the transport read out of a reply that wasn't a
            // JSON-RPC response, as a gateway's bare error object is.
            RpcError::ErrorResp(mut refusal) => {
                self.scrub_refusal(&mut refusal);
                RpcError::ErrorResp(refusal)
            }
            other => other,
        }
    }

    /// A node's refusal, with the URL taken out of its message and its data.
    fn scrub_refusal(&self, refusal: &mut alloy::rpc::json_rpc::ErrorPayload) {
        let message = self.scrub(&refusal.message);
        if message != refusal.message {
            refusal.message = message.into();
        }
        if let Some(data) = &refusal.data {
            let scrubbed = self.scrub(data.get());
            if scrubbed != data.get() {
                // Data that is no longer JSON once scrubbed is carried as a
                // string.
                refusal.data = serde_json::value::RawValue::from_string(scrubbed.clone())
                    .or_else(|_| serde_json::value::to_raw_value(&scrubbed))
                    .ok();
            }
        }
    }

    /// A node's answer, with the URL taken out of any refusal in it.
    fn scrub_answer(
        &self,
        mut answer: alloy::rpc::json_rpc::ResponsePacket,
    ) -> alloy::rpc::json_rpc::ResponsePacket {
        use alloy::rpc::json_rpc::{ResponsePacket, ResponsePayload};
        let responses = match &mut answer {
            ResponsePacket::Single(one) => std::slice::from_mut(one),
            ResponsePacket::Batch(all) => all.as_mut_slice(),
        };
        for response in responses {
            if let ResponsePayload::Failure(refusal) = &mut response.payload {
                self.scrub_refusal(refusal);
            }
        }
        answer
    }
}

/// A URL part with its percent-escapes decoded, when it has any.
fn unescape(text: &str) -> Option<String> {
    if !text.contains('%') {
        return None;
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(hex) = text.get(at + 1..at + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

impl<S> tower::Layer<S> for KeepUrlOut {
    type Service = UrlKeptOut<S>;

    fn layer(&self, inner: S) -> Self::Service {
        UrlKeptOut {
            inner,
            keep: self.clone(),
        }
    }
}

#[derive(Clone)]
struct UrlKeptOut<S> {
    inner: S,
    keep: KeepUrlOut,
}

impl<S> tower::Service<alloy::rpc::json_rpc::RequestPacket> for UrlKeptOut<S>
where
    S: tower::Service<
            alloy::rpc::json_rpc::RequestPacket,
            Response = alloy::rpc::json_rpc::ResponsePacket,
            Error = alloy::transports::TransportError,
            Future = alloy::transports::TransportFut<'static>,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type Response = alloy::rpc::json_rpc::ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = alloy::transports::TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
        let methods: Vec<String> = request.method_names().map(str::to_owned).collect();
        let mut inner = self.inner.clone();
        let keep = self.keep.clone();
        Box::pin(async move {
            tokio::time::timeout(REQUEST_TIMEOUT, async move {
                for attempt in 0..=crate::http_retry::EXTRA_TRIES {
                    for method in &methods {
                        crate::rpc_counts::evm(keep.counted_chain, method);
                    }
                    let answer = inner.call(request.clone()).await;
                    if let Err(error) = &answer
                        && attempt < crate::http_retry::EXTRA_TRIES
                        && crate::http_retry::unprocessed(error)
                    {
                        continue;
                    }
                    return answer
                        .map(|answer| keep.scrub_answer(answer))
                        .map_err(|error| keep.scrub_error(error));
                }
                unreachable!("the final attempt always returns")
            })
            .await
            .unwrap_or_else(|_| {
                Err(alloy::transports::TransportErrorKind::custom_str(
                    "the HTTP request timed out",
                ))
            })
        })
    }
}

/// How long any one request to a node may take. A node or gateway that
/// takes a request and never answers would otherwise hold whatever waits on
/// it for good: a key's turn to send, a round of the keeper, a vault being
/// taken on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The connection to a node, with the URL kept out of its errors and every
/// request bounded in time.
///
/// `rpc` may name several endpoints, separated by spaces: free public
/// endpoints each limit how fast one address may ask, and a whole stack on
/// one machine passes any one of their limits. Each request then goes to the
/// endpoint ranked best by how often and how fast it has answered lately, and
/// to the next when one refuses (a rate limit included, however it is
/// worded, and an endpoint saying it no longer has the past state a read
/// asked for), which also falls in the ranking.
///
/// `setting` names the setting the list came from, for an error that can't
/// name the URL.
fn rpc_client(
    rpc: &str,
    setting: &str,
    chain_id: Option<u64>,
) -> Result<alloy::rpc::client::RpcClient> {
    let http = http_client()?;
    let urls: Vec<&str> = rpc.split_whitespace().collect();
    if urls.len() <= 1 {
        let url = parse_endpoint(rpc.trim(), setting)?;
        let is_local = alloy::transports::utils::guess_local_url(&url);
        let keep = KeepUrlOut::new(rpc.trim(), &url).counted_on(chain_id);
        let http = alloy::transports::http::Http::with_client(http, url);
        return Ok(alloy::rpc::client::ClientBuilder::default().transport(
            AskAgainWhenRateLimited {
                inner: tower::Layer::layer(&keep, http),
            },
            is_local,
        ));
    }
    let mut transports = Vec::with_capacity(urls.len());
    for configured in urls {
        transports.push(endpoint_transport(
            configured, setting, &http, chain_id, chain_id,
        )?);
    }
    // Every method the services use goes to one endpoint at a time, best
    // first, and on to the next when one refuses, so a request fails only
    // when every endpoint has. A method not listed goes to them all at once
    // and takes the first answer.
    let count =
        std::num::NonZeroUsize::new(transports.len()).unwrap_or(std::num::NonZeroUsize::MIN);
    let fallback = tower::Layer::layer(
        &alloy::transports::layers::FallbackLayer::default()
            .with_active_transport_count(count)
            .with_sequential_methods(
                ONE_ENDPOINT_AT_A_TIME
                    .iter()
                    .map(|method| method.to_string())
                    .collect(),
            ),
        transports,
    );
    Ok(alloy::rpc::client::ClientBuilder::default()
        .transport(AskAgainWhenRateLimited { inner: fallback }, false))
}

/// The HTTP client every endpoint is asked through, each request bounded in
/// time.
fn http_client() -> Result<alloy::transports::http::reqwest::Client> {
    alloy::transports::http::reqwest::Client::builder()
        // Retry and failover live above the counted transport. Hidden HTTP
        // protocol retries or redirects would evade both its accounting and the
        // endpoint's chain check.
        .retry(alloy::transports::http::reqwest::retry::never())
        .redirect(alloy::transports::http::reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("making the HTTP client")
}

fn parse_endpoint(url: &str, setting: &str) -> Result<alloy::transports::http::reqwest::Url> {
    url.parse()
        .map_err(|_| anyhow!("an endpoint in the chain's {setting} is not a URL"))
}

/// The connection to one endpoint of a list: its URL kept out of its
/// errors, its own trouble (a rate limit, missing history) an error rather
/// than an answer, and, once the chain's id is known, held to that chain.
fn endpoint_transport(
    configured: &str,
    setting: &str,
    http: &alloy::transports::http::reqwest::Client,
    chain_id: Option<u64>,
    counted_chain: Option<u64>,
) -> Result<
    HeldToChain<
        UrlKeptOut<
            TroubleIsAFailure<
                alloy::transports::http::Http<alloy::transports::http::reqwest::Client>,
            >,
        >,
    >,
> {
    let url = parse_endpoint(configured, setting)?;
    let keep = KeepUrlOut::new(configured, &url).counted_on(counted_chain);
    let inner = alloy::transports::http::Http::with_client(http.clone(), url);
    Ok(HeldToChain {
        inner: tower::Layer::layer(&keep, TroubleIsAFailure { inner }),
        chain_id,
        on_it: Default::default(),
        failed_at: Default::default(),
    })
}

/// How long an endpoint is given to say its chain id before a request.
const CHAIN_ID_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long after an endpoint failed to say its chain id its requests fail
/// at once, so a list moves each of them on rather than each asking again.
const CHAIN_ID_PROBE_REST: Duration = Duration::from_secs(10);

/// One endpoint of a list, held to chain `chain_id`: before it answers
/// anything, it is asked its chain id, once, and an endpoint on another
/// chain is refused every request from then on. An endpoint that doesn't
/// answer that within [`CHAIN_ID_PROBE_TIMEOUT`] fails this request, and
/// every request waiting on or arriving within [`CHAIN_ID_PROBE_REST`] of
/// that answer, at once, so a list moves each on to the next endpoint; it is
/// asked again after that. A list's endpoints are each asked
/// when the client connects, but one that didn't answer then (limiting the
/// rate while a whole stack starts) would otherwise answer for another
/// chain as this one, and stations share one address on every chain.
/// Without `chain_id` (the connection the id itself is first asked over)
/// requests pass as they are, and so does a request for the chain id.
#[derive(Clone)]
struct HeldToChain<S> {
    inner: S,
    chain_id: Option<u64>,
    /// Whether the endpoint said it is on the chain, once it has answered.
    on_it: Arc<tokio::sync::OnceCell<bool>>,
    /// When the endpoint last failed to say its chain id.
    failed_at: Arc<std::sync::Mutex<Option<tokio::time::Instant>>>,
}

impl<S> tower::Service<alloy::rpc::json_rpc::RequestPacket> for HeldToChain<S>
where
    S: tower::Service<
            alloy::rpc::json_rpc::RequestPacket,
            Response = alloy::rpc::json_rpc::ResponsePacket,
            Error = alloy::transports::TransportError,
            Future = alloy::transports::TransportFut<'static>,
        > + Send
        + Clone
        + 'static,
{
    type Response = alloy::rpc::json_rpc::ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = alloy::transports::TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
        let Some(chain_id) = self.chain_id else {
            return self.inner.call(request);
        };
        if self.on_it.get() == Some(&true)
            || request.method_names().all(|method| method == "eth_chainId")
        {
            return self.inner.call(request);
        }
        let mut inner = self.inner.clone();
        let on_it = self.on_it.clone();
        let failed_at = self.failed_at.clone();
        Box::pin(async move {
            use alloy::transports::TransportErrorKind;
            let answered = on_it
                .get_or_try_init(|| async {
                    let failed = || failed_at.lock().unwrap_or_else(|p| p.into_inner());
                    if failed().is_some_and(|at| at.elapsed() < CHAIN_ID_PROBE_REST) {
                        return Err(TransportErrorKind::custom_str(
                            "the endpoint did not give its chain id",
                        ));
                    }
                    let asking = alloy::rpc::json_rpc::Request::new(
                        "eth_chainId",
                        alloy::rpc::json_rpc::Id::Number(0),
                        (),
                    )
                    .serialize()
                    .map_err(TransportErrorKind::custom)?;
                    let answer = tokio::time::timeout(
                        CHAIN_ID_PROBE_TIMEOUT,
                        inner.call(alloy::rpc::json_rpc::RequestPacket::Single(asking)),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(TransportErrorKind::custom_str(
                            "the endpoint did not give its chain id in time",
                        ))
                    })
                    .inspect_err(|_| *failed() = Some(tokio::time::Instant::now()))?;
                    let id = answer
                        .responses()
                        .first()
                        .and_then(|response| response.payload.as_success())
                        .and_then(|raw| {
                            serde_json::from_str::<alloy::primitives::U64>(raw.get()).ok()
                        })
                        .ok_or_else(|| {
                            *failed() = Some(tokio::time::Instant::now());
                            TransportErrorKind::custom_str("the endpoint did not give its chain id")
                        })?;
                    Ok::<_, alloy::transports::TransportError>(id.to::<u64>() == chain_id)
                })
                .await?;
            if !*answered {
                return Err(TransportErrorKind::custom_str(&format!(
                    "the endpoint is not on chain {chain_id}"
                )));
            }
            inner.call(request).await
        })
    }
}

/// One endpoint of a list, asked on its own rather than through the
/// failover, for the requests whose answer depends on which endpoint gave
/// it: a send, a read pinned to a past block, a log query and the head it is
/// checked against.
#[derive(Clone)]
struct Endpoint {
    provider: DynProvider,
    /// Whether it answers a read pinned to a past block as of that block:
    /// `Some(false)` if it answers with the head's state whatever block is
    /// asked, `None` if that could not be found out ([`honours_blocks`]);
    /// and when it was last asked.
    honours_blocks: Arc<std::sync::Mutex<(Option<bool>, tokio::time::Instant)>>,
    /// How wide a log query it has been seen to take and to refuse.
    log_width: Arc<std::sync::Mutex<LogWidth>>,
}

/// What one endpoint has shown of the widest log query it takes: the
/// widest range it answered whole, and the narrowest it refused as too
/// wide. Free endpoints cap the width at anything from a hundred blocks to
/// ten thousand, and say so only by refusing.
#[derive(Clone, Copy, Debug, Default)]
struct LogWidth {
    taken: u64,
    refused: Option<u64>,
}

impl LogWidth {
    /// Whether a range `width` blocks wide may be taken whole: it is not
    /// known to be refused.
    fn may_take(self, width: u64) -> bool {
        self.refused.is_none_or(|refused| width < refused)
    }

    /// The widest range it may take: one block short of the narrowest it
    /// refused.
    fn widest_possible(self) -> u64 {
        self.refused.map_or(u64::MAX, |refused| refused - 1)
    }

    /// The width to read a range `width` blocks wide in, when it is known
    /// to refuse it whole: the widest it has taken, or half, if it has
    /// taken none yet.
    fn part(self, width: u64) -> u64 {
        if self.taken > 0 {
            self.taken
        } else {
            width.div_ceil(2)
        }
    }

    fn took(&mut self, width: u64) {
        self.taken = self.taken.max(width);
        // An endpoint whose nodes differ may take what it once refused:
        // what it refused is then no longer known.
        if self.refused.is_some_and(|refused| refused <= self.taken) {
            self.refused = None;
        }
    }

    fn refused(&mut self, width: u64) {
        self.refused = Some(self.refused.map_or(width, |refused| refused.min(width)));
        if self.taken >= width {
            self.taken = 0;
        }
    }
}

/// How long an endpoint not known to honour a past block goes before it
/// is asked again, when a past read would use it.
const PROBE_AGAIN_AFTER: Duration = Duration::from_secs(60);

impl Endpoint {
    fn log_width(&self) -> LogWidth {
        *self
            .log_width
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn saw_log_width(&self, width: u64, taken: bool) {
        let mut known = self
            .log_width
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if taken {
            known.took(width);
        } else {
            known.refused(width);
        }
    }

    fn honours(&self) -> Option<bool> {
        self.honours_blocks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0
    }

    /// Whether it honours a past block, asking it again first ([`honours_blocks`])
    /// if that isn't known and it wasn't asked in the last
    /// [`PROBE_AGAIN_AFTER`].
    async fn honours_now(&self) -> Option<bool> {
        {
            let mut probed = self
                .honours_blocks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if probed.0.is_some() || probed.1.elapsed() < PROBE_AGAIN_AFTER {
                return probed.0;
            }
            // Marked now, so reads at once don't all ask it.
            probed.1 = tokio::time::Instant::now();
        }
        let found = tokio::time::timeout(CHECK_TIMEOUT, honours_blocks(&self.provider))
            .await
            .ok()
            .flatten();
        let mut probed = self
            .honours_blocks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        probed.0 = probed.0.or(found);
        probed.0
    }
}

/// The endpoints of one list, each on its own, and which one answered last.
#[derive(Clone)]
struct Endpoints {
    each: Arc<[Endpoint]>,
    preferred: Arc<std::sync::atomic::AtomicUsize>,
}

impl Endpoints {
    /// Each endpoint held to chain `chain_id` when there is one
    /// ([`HeldToChain`]).
    fn new(list: &str, setting: &str, chain_id: Option<u64>, counted_chain: u64) -> Result<Self> {
        let http = http_client()?;
        let each = list
            .split_whitespace()
            .map(|configured| {
                let client = alloy::rpc::client::ClientBuilder::default().transport(
                    endpoint_transport(configured, setting, &http, chain_id, Some(counted_chain))?,
                    false,
                );
                Ok(Endpoint {
                    provider: ProviderBuilder::default().connect_client(client).erased(),
                    honours_blocks: Arc::new(std::sync::Mutex::new((
                        None,
                        tokio::time::Instant::now(),
                    ))),
                    log_width: Default::default(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if each.is_empty() {
            bail!("the chain's {setting} names no endpoint");
        }
        Ok(Self {
            each: each.into(),
            preferred: Default::default(),
        })
    }

    /// The order to ask them in: the one that answered last, then the rest
    /// in the list's order.
    fn order(&self) -> Vec<usize> {
        let count = self.each.len();
        let start = self.preferred.load(std::sync::atomic::Ordering::Relaxed) % count;
        (0..count).map(|step| (start + step) % count).collect()
    }

    fn answered(&self, at: usize) {
        self.preferred
            .store(at, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check that every endpoint that answers is on chain `chain_id` (one
    /// that doesn't answer now is held to it when next asked anything:
    /// [`HeldToChain`]), unless `id_known` says the list is the one endpoint the id was
    /// just asked of; and, with `probe`, find out which of them answer a
    /// read pinned to a past block as of that block. The endpoints are asked at once,
    /// each for at most [`CHECK_TIMEOUT`].
    async fn checked(
        self,
        chain_id: u64,
        label: &str,
        setting: &str,
        id_known: bool,
        probe: bool,
    ) -> Result<Self> {
        let ask_ids = !(id_known && self.each.len() == 1);
        let mut asked = tokio::task::JoinSet::new();
        for (at, endpoint) in self.each.iter().enumerate() {
            let provider = endpoint.provider.clone();
            asked.spawn(async move {
                let (id, honours) = tokio::join!(
                    async {
                        if ask_ids {
                            tokio::time::timeout(CHECK_TIMEOUT, provider.get_chain_id())
                                .await
                                .ok()
                                .and_then(Result::ok)
                        } else {
                            None
                        }
                    },
                    async {
                        if probe {
                            tokio::time::timeout(CHECK_TIMEOUT, honours_blocks(&provider))
                                .await
                                .ok()
                                .flatten()
                        } else {
                            None
                        }
                    }
                );
                (at, id, honours)
            });
        }
        while let Some(joined) = asked.join_next().await {
            let (at, id, honours) =
                joined.map_err(|_| anyhow!("checking an endpoint stopped before it ended"))?;
            // Named by its place in the list: the URL may carry a key.
            if let Some(id) = id
                && id != chain_id
            {
                bail!(
                    "endpoint {} of {label}'s {setting} is on chain {id}, not {chain_id}",
                    at + 1
                );
            }
            *self.each[at]
                .honours_blocks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                (honours, tokio::time::Instant::now());
            match honours {
                Some(false) => tracing::warn!(
                    chain = %label,
                    endpoint = at + 1,
                    "this endpoint answers a read at a past block with the head's state; \
                     past reads are never sent to it"
                ),
                None if probe => tracing::info!(
                    chain = %label,
                    endpoint = at + 1,
                    "could not find out whether this endpoint answers a read at a past block \
                     as of that block; it is asked again before a past read is sent to it"
                ),
                _ => {}
            }
        }
        Ok(self)
    }
}

/// How long each check of an endpoint at connect may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// How far below its head an endpoint is asked about in [`honours_blocks`]:
/// deep enough that a node a few blocks behind the others behind one name
/// has the block, shallow enough that one keeping little history still does.
const PROBE_DEPTH: u64 = 16;

/// How many blocks below [`PROBE_DEPTH`] [`honours_blocks`] looks through
/// for a transaction to ask about, on a quiet chain.
const PROBE_SCAN: u64 = 32;

/// Whether an endpoint answers a read pinned to a past block as of that
/// block. Some answer it with the head's state whatever block is asked, with
/// no error, so no wording can catch it.
///
/// It is asked about state, which is what a pinned read reads (the block's
/// own number or environment is not: an Arbitrum-stack chain's `NUMBER` is
/// the parent chain's, and some nodes run a call in the next block's
/// environment while reading the asked block's state). It finds a
/// transaction of an ordinary type in a block `b` from [`PROBE_DEPTH`] to
/// [`PROBE_DEPTH`] + [`PROBE_SCAN`] below its head whose sender `S` has a
/// nonce at the head above `n`, the lowest nonce `S` used in `b`, and asks
/// for `S`'s nonce as of block `b - 1`: exactly `n` honours the block, more
/// than `n` ignores it, and a refusal for missing history counts as
/// honouring (the read then moves on from it, as from any endpoint that
/// lacks the state). Past those blocks it looks, on a quiet chain, at one
/// block every power of two further down (2 × [`PROBE_SCAN`], 4 ×, …, to
/// the first block). `None` when it couldn't be found out: the endpoint
/// didn't answer or answered something else, or the chain is too young or
/// too quiet to have such a transaction in reach.
async fn honours_blocks(provider: &DynProvider) -> Option<bool> {
    let head = provider.get_block_number().await.ok()?;
    let newest = head.checked_sub(PROBE_DEPTH).filter(|block| *block >= 1)?;
    let oldest = newest.saturating_sub(PROBE_SCAN).max(1);
    let deeper =
        std::iter::successors(Some(PROBE_SCAN.saturating_mul(2)), |gap| gap.checked_mul(2))
            .map_while(|gap| newest.checked_sub(gap).filter(|block| *block >= 1));
    for block in (oldest..=newest).rev().chain(deeper) {
        let Some((sender, nonce)) = probe_transaction(provider, block).await? else {
            continue;
        };
        let now = provider.get_transaction_count(sender).await.ok()?;
        // A sender whose nonce the transaction didn't move (a chain's own
        // system transaction) says nothing either way.
        if now <= nonce {
            continue;
        }
        return match provider
            .get_transaction_count(sender)
            .block_id(alloy::eips::BlockId::number(block - 1))
            .await
        {
            Ok(before) if before == nonce => Some(true),
            Ok(before) if before > nonce => Some(false),
            Ok(_) => None,
            Err(error) if error.to_string().contains(MISSING_STATE) => Some(true),
            Err(_) => None,
        };
    }
    None
}

/// A transaction of an ordinary type (0 to 4) in `block`, as its sender and
/// the lowest nonce that sender used in the block; `Some(None)` when the
/// block has none, `None` when the block couldn't be read. The block is read
/// as plain JSON: a chain's own transaction types are not Ethereum's.
async fn probe_transaction(provider: &DynProvider, block: u64) -> Option<Option<(Address, u64)>> {
    let read: serde_json::Value = provider
        .raw_request(
            "eth_getBlockByNumber".into(),
            (format!("0x{block:x}"), true),
        )
        .await
        .ok()?;
    let quantity = |value: &serde_json::Value| {
        u64::from_str_radix(value.as_str()?.strip_prefix("0x")?, 16).ok()
    };
    let ordinary: Vec<(Address, u64)> = read
        .get("transactions")?
        .as_array()?
        .iter()
        .filter(|tx| quantity(&tx["type"]).unwrap_or(0) <= 4)
        .filter_map(|tx| Some((tx["from"].as_str()?.parse().ok()?, quantity(&tx["nonce"])?)))
        .collect();
    let Some((sender, _)) = ordinary.first().copied() else {
        return Some(None);
    };
    let lowest = ordinary
        .iter()
        .filter(|(from, _)| *from == sender)
        .map(|(_, nonce)| *nonce)
        .min()?;
    Some(Some((sender, lowest)))
}

/// How many times a read is asked again when every endpoint asked limited
/// the rate of requests (and nothing else went wrong).
const RATE_LIMIT_RETRIES: u32 = 3;
/// The pause before the first time it is asked again; each next pause is
/// twice the last.
const RATE_LIMIT_PAUSE: Duration = Duration::from_millis(500);
/// The longest pause, whatever an endpoint's Retry-After asks.
const RATE_LIMIT_PAUSE_MOST: Duration = Duration::from_secs(10);

/// The pause before asking again for the `retry`th time (from 0): 0.5 s,
/// 1 s, 2 s, or longer if an endpoint's Retry-After asked for longer, and never
/// over [`RATE_LIMIT_PAUSE_MOST`].
fn rate_limit_pause(retry: u32, retry_after: Option<Duration>) -> Duration {
    let grown = RATE_LIMIT_PAUSE.saturating_mul(2u32.saturating_pow(retry));
    retry_after
        .map_or(grown, |asked| asked.max(grown))
        .min(RATE_LIMIT_PAUSE_MOST)
}

/// The wait an endpoint asked for with a Retry-After, if it did.
fn retry_after(error: &alloy::transports::TransportError) -> Option<Duration> {
    match error {
        alloy::transports::RpcError::Transport(
            alloy::transports::TransportErrorKind::HttpErrorWithRetryAfter { retry_after, .. },
        ) => Some(*retry_after),
        _ => None,
    }
}

/// A read's rounds over a list's endpoints, for asking again when every
/// endpoint asked limited the rate. A read whose endpoints all limited the
/// rate once is not a read the chain refused: the endpoints are asked again
/// a few times, with growing pauses, before it fails. A send is never asked
/// again this way.
#[derive(Default)]
struct RateLimits {
    retries: u32,
}

/// What the endpoints of one round said that was not an answer.
struct Round {
    limited: bool,
    other: bool,
    retry_after: Option<Duration>,
}

impl Round {
    /// An endpoint's failure.
    fn saw(&mut self, error: &alloy::transports::TransportError) {
        if rate_limited(error) {
            self.limited = true;
            self.retry_after = self.retry_after.max(retry_after(error));
        } else {
            self.other = true;
        }
    }

    /// Anything else that went wrong.
    fn other(&mut self) {
        self.other = true;
    }
}

impl RateLimits {
    fn round() -> Round {
        Round {
            limited: false,
            other: false,
            retry_after: None,
        }
    }

    /// After a round with no answer: if every endpoint asked limited the
    /// rate and the read may be asked again, wait and say so.
    async fn wait_after(&mut self, round: Round) -> bool {
        if !round.limited || round.other || self.retries >= RATE_LIMIT_RETRIES {
            return false;
        }
        tokio::time::sleep(rate_limit_pause(self.retries, round.retry_after)).await;
        self.retries += 1;
        true
    }
}

/// Whether a request is made of reads alone, which may be asked again when
/// rate limited: no send, no signature, nothing a list doesn't name.
fn retried_when_rate_limited(request: &alloy::rpc::json_rpc::RequestPacket) -> bool {
    request.method_names().all(|method| {
        ONE_ENDPOINT_AT_A_TIME.contains(&method)
            && !method.starts_with("eth_send")
            && !method.starts_with("eth_sign")
    })
}

/// Asks a read again, as [`RateLimits`] says, when its whole list of
/// endpoints refused it for the rate: the outermost layer of a client.
#[derive(Clone)]
struct AskAgainWhenRateLimited<S> {
    inner: S,
}

impl<S> tower::Service<alloy::rpc::json_rpc::RequestPacket> for AskAgainWhenRateLimited<S>
where
    S: tower::Service<
            alloy::rpc::json_rpc::RequestPacket,
            Response = alloy::rpc::json_rpc::ResponsePacket,
            Error = alloy::transports::TransportError,
            Future = alloy::transports::TransportFut<'static>,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type Response = alloy::rpc::json_rpc::ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = alloy::transports::TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
        if !retried_when_rate_limited(&request) {
            return self.inner.call(request);
        }
        let mut inner = self.inner.clone();
        let first = self.inner.call(request.clone());
        Box::pin(async move {
            let mut limits = RateLimits::default();
            let mut answer = first.await;
            loop {
                let mut round = RateLimits::round();
                match &answer {
                    Err(error) => round.saw(error),
                    // A rate limit inside a JSON-RPC answer, from a lone
                    // endpoint (a list's endpoints turn it into a failure).
                    Ok(packet)
                        if packet
                            .responses()
                            .iter()
                            .filter_map(|response| response.payload.as_error())
                            .any(|error| is_rate_limit(error.code, &error.message)) =>
                    {
                        round.limited = true;
                    }
                    Ok(_) => return answer,
                }
                if !limits.wait_after(round).await {
                    return answer;
                }
                answer = inner.call(request.clone()).await;
            }
        })
    }
}

/// What [`TroubleIsAFailure`] says of an endpoint limiting the rate of
/// requests.
const RATE_LIMITED: &str = "the endpoint is limiting the rate of requests";

/// What [`TroubleIsAFailure`] says of an endpoint that no longer has the
/// state a read asked for.
const MISSING_STATE: &str = "the endpoint does not have the state asked for";

/// Whether a request's failure is an endpoint limiting the rate of
/// requests, however it arrived: an HTTP 429, a rate limit inside a
/// JSON-RPC error, or one an endpoint's transport already turned into a
/// failure. An endpoint that limited the rate did nothing with the request.
fn rate_limited(error: &alloy::transports::TransportError) -> bool {
    use alloy::transports::{RpcError, TransportErrorKind};
    match error {
        RpcError::Transport(TransportErrorKind::HttpError(http))
        | RpcError::Transport(TransportErrorKind::HttpErrorWithRetryAfter {
            error: http, ..
        }) => http.status == 429 || is_rate_limit(0, &http.body),
        RpcError::ErrorResp(refusal) => is_rate_limit(refusal.code, &refusal.message),
        other => {
            let text = other.to_string();
            text.contains(RATE_LIMITED) || is_rate_limit(0, &text)
        }
    }
}

/// What became of a transaction handed to a list's endpoints.
enum Handed {
    /// Taken, or it may have been: watched for by its hash.
    MayLand,
    /// Refused, and no endpoint can have taken it.
    Refused(alloy::transports::TransportError),
    /// Every endpoint limited the rate of requests: none took it.
    NotTaken(alloy::transports::TransportError),
}

/// Hand a signed transaction's bytes to a list's endpoints, one at a time,
/// until one takes it or refuses it.
///
/// An endpoint whose answer is lost or unreadable (a gateway error, a
/// timeout, a dropped connection) may have taken it, and on a chain with
/// fast blocks it may be mined before the next endpoint is asked. That one
/// then says its nonce is too low or the transaction is already known,
/// because this very transaction used it. So once an endpoint may have taken
/// it, no later refusal is taken as one: whether it landed is found out by
/// its hash. An error that is no judgement of the transaction itself
/// ([`judges_the_transaction`]) is no answer either, however it arrives. A
/// clear refusal from the first endpoint that answers at all stays a
/// refusal, except a "nonce too low" from an endpoint that holds this very
/// transaction's receipt (a gateway that retried it behind one name) or
/// can't be asked for it. A rate limit is no answer either way and moves on. The bytes are
/// the same at every endpoint, so the transaction is never sent under a
/// second nonce.
async fn hand_over(endpoints: &Endpoints, raw: &[u8], deadline: tokio::time::Instant) -> Handed {
    use alloy::rpc::json_rpc::RpcError;
    let mut may_have_taken = false;
    let mut limited = None;
    for at in endpoints.order() {
        let send = endpoints.each[at].provider.send_raw_transaction(raw);
        let Ok(reply) = tokio::time::timeout_at(deadline, send).await else {
            // Out of time with the bytes on their way: it may be taken.
            return Handed::MayLand;
        };
        match reply {
            Ok(_) => {
                endpoints.answered(at);
                return Handed::MayLand;
            }
            Err(error) if rate_limited(&error) => limited = Some(error),
            Err(refusal @ RpcError::ErrorResp(_)) => {
                if may_have_taken || already_held(&refusal) {
                    return Handed::MayLand;
                }
                // An error of the endpoint's own (an internal error, a
                // timeout upstream) says nothing of the transaction, which
                // its node may have: like a gateway's 502.
                if !judges_the_transaction(&refusal) {
                    may_have_taken = true;
                    continue;
                }
                // A gateway that retries a send behind one name can take it
                // and then call its own retry's nonce too low: this very
                // transaction is then mined. Asked by its hash, the endpoint
                // says so; one that can't be asked is no proof of a refusal.
                if nonce_too_low(&refusal) {
                    let hash = alloy::primitives::keccak256(raw);
                    let provider = &endpoints.each[at].provider;
                    match tokio::time::timeout_at(deadline, provider.get_transaction_receipt(hash))
                        .await
                    {
                        Ok(Ok(None)) => {}
                        _ => return Handed::MayLand,
                    }
                }
                return Handed::Refused(refusal);
            }
            Err(_) => may_have_taken = true,
        }
    }
    match limited {
        Some(error) if !may_have_taken => Handed::NotTaken(error),
        // No endpoint said yes or no, and one may have taken it.
        _ => Handed::MayLand,
    }
}

/// The methods sent to one endpoint at a time: every one the services use.
/// A send among them must never go to two endpoints at once.
const ONE_ENDPOINT_AT_A_TIME: [&str; 22] = [
    "eth_blockNumber",
    "eth_chainId",
    "net_version",
    "eth_call",
    "eth_estimateGas",
    "eth_getLogs",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getTransactionCount",
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_feeHistory",
    "eth_sendRawTransaction",
    "eth_sendTransaction",
    "eth_sendRawTransactionSync",
    "eth_sendTransactionSync",
    "eth_signTypedData_v4",
];

/// An answer that is the endpoint's own trouble rather than an answer to
/// what was asked, taken as the endpoint failing, so the request goes on to
/// the next endpoint and the ranking moves away from this one:
///
/// - a rate limit, whether as an HTTP status or inside a JSON-RPC error;
/// - to a read of the chain's state or logs, an endpoint saying it no longer
///   has (or never kept) the state asked for. Another endpoint in the list
///   may keep the past, so a list mixing endpoints that do and don't is
///   safe: the read fails only when none has it. A send is never moved on
///   for this, and a refusal about the request itself (a revert, a bad
///   argument) stays the answer it is.
#[derive(Clone)]
struct TroubleIsAFailure<S> {
    inner: S,
}

/// The reads an endpoint may answer by saying it no longer has the state
/// they ask about. No send is among them.
const READS_OF_THE_PAST: [&str; 8] = [
    "eth_call",
    "eth_estimateGas",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getTransactionCount",
    "eth_getProof",
    "eth_getLogs",
];

impl<S> tower::Service<alloy::rpc::json_rpc::RequestPacket> for TroubleIsAFailure<S>
where
    S: tower::Service<
            alloy::rpc::json_rpc::RequestPacket,
            Response = alloy::rpc::json_rpc::ResponsePacket,
            Error = alloy::transports::TransportError,
            Future = alloy::transports::TransportFut<'static>,
        > + Send
        + Clone
        + 'static,
{
    type Response = alloy::rpc::json_rpc::ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = alloy::transports::TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
        // Only a request made of reads alone may be moved on for missing
        // history: a batch carrying a send is answered as it is.
        let reads_only = request
            .method_names()
            .all(|method| READS_OF_THE_PAST.contains(&method));
        let answer = self.inner.call(request);
        Box::pin(async move {
            let answer = answer.await?;
            let refusals = || {
                answer
                    .responses()
                    .iter()
                    .filter_map(|response| response.payload.as_error())
            };
            if refusals().any(|error| is_rate_limit(error.code, &error.message)) {
                return Err(alloy::transports::TransportErrorKind::custom_str(
                    RATE_LIMITED,
                ));
            }
            if reads_only
                && let Some(error) =
                    refusals().find(|error| is_missing_history(error.code, &error.message))
            {
                return Err(alloy::transports::TransportErrorKind::custom_str(&format!(
                    "{MISSING_STATE}: {}",
                    error.message
                )));
            }
            Ok(answer)
        })
    }
}

/// Whether a JSON-RPC error is an endpoint saying it doesn't have the past
/// state (or the past blocks' logs) asked for: pruned, never kept, or behind
/// what an upstream it relies on keeps. Each wording here is one a public
/// endpoint gives. A revert, whatever its reason says, is the chain's
/// answer, never this.
fn is_missing_history(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if code == 3 || message.contains("revert") {
        return false;
    }
    [
        // Nitro (Robinhood Chain): "historical state <root> is not available",
        // "missing trie node <hash> (path ) state <root> is not available".
        "historical state",
        "missing trie node",
        // "state at block #N is pruned"; geth's "pruned history unavailable".
        "pruned",
        // "Unknown state. First available state is N".
        "unknown state",
        "first available state",
        // A gateway with no upstream keeping that block's state: "Upstream
        // lower height N of type STATE is greater than M".
        "upstream lower height",
        "state not available",
        "state is not available",
        "state unavailable",
        "state history",
        "archive",
        // "no state found for block 0x…".
        "no state found",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// Whether a JSON-RPC error is an endpoint limiting the rate of requests. A
/// revert, whatever its reason says, is the chain's answer, never this.
fn is_rate_limit(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if code == 3 || message.contains("revert") {
        return false;
    }
    code == 429
        || message.contains("rate limit")
        || message.contains("too many requests")
        || message.contains("request limit")
        || (code == -32005 && !message.contains("range") && !message.contains("results"))
}

/// Wait for a sent transaction's receipt, for at most `within`. A receipt
/// that never comes (the transaction dropped, the node restarted) is an
/// unanswered send rather than a wait that never ends.
async fn receipt_within(
    provider: &DynProvider,
    hash: B256,
    within: Duration,
    what: &str,
) -> Result<TransactionReceipt> {
    receipt_by(provider, hash, tokio::time::Instant::now() + within)
        .await
        .with_context(|| what.to_string())
}

/// Look for a transaction's receipt by its hash until `deadline`. A read that
/// fails (a gateway's rate limit, a node restarting) is the node's trouble,
/// not the transaction's: it says nothing about whether the transaction will
/// land, so it is asked again rather than taken for a failed send, which its
/// sender would send again.
async fn receipt_by(
    provider: &DynProvider,
    hash: B256,
    deadline: tokio::time::Instant,
) -> Result<TransactionReceipt> {
    let every = provider.client().poll_interval();
    loop {
        if let Ok(Ok(Some(receipt))) =
            tokio::time::timeout(POLL_TIMEOUT, provider.get_transaction_receipt(hash)).await
        {
            return Ok(receipt);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow::Error::new(NeverLanded));
        }
        tokio::time::sleep(every).await;
    }
}

impl Client {
    /// Reader settings selected for this connection's chain.
    pub fn rpc_policy(&self) -> &crate::rpc_policy::RpcPolicy {
        &self.policy
    }

    /// Apply already validated chain configuration before handing out clones.
    pub(crate) fn with_rpc_policy(mut self, policy: crate::rpc_policy::RpcPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Connect, waiting for the chain to answer: `task dev` starts everything
    /// at once, so a service usually arrives before the chain does.
    pub async fn connect(config: &ChainConfig, signing: Signing) -> Result<Self> {
        let rpc_url = config.rpc.as_str();
        // The chain id is asked for first, on a provider that signs nothing,
        // so a provider that signs is made for that id alone.
        let asking: DynProvider = ProviderBuilder::default()
            .connect_client(rpc_client(rpc_url, "rpc", None)?)
            .erased();

        // Measured by the clock, not by the pauses between tries: a node
        // that takes each request and never answers spends a request's whole
        // timeout on every try.
        let started = tokio::time::Instant::now();
        let chain_id = loop {
            match asking.get_chain_id().await {
                Ok(id) => break id,
                Err(error) => {
                    // The URL is left out: a paid one carries its key.
                    if started.elapsed() > Duration::from_secs(120) {
                        return Err(anyhow!("{} never answered: {error}", config.label));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        };

        // Every endpoint of the list is on this chain: one on another would
        // answer for that chain as this one, and stations share one address
        // on every chain.
        // A lone endpoint is the one that just answered the chain id.
        let several = rpc_url.split_whitespace().count() > 1;
        let endpoints = Endpoints::new(rpc_url, "rpc", several.then_some(chain_id), chain_id)?
            .checked(chain_id, &config.label, "rpc", true, true)
            .await?;

        // From here every endpoint of the list is held to this chain,
        // whether or not it answered just now.
        let rpc = rpc_client(rpc_url, "rpc", Some(chain_id))?;
        let asking: DynProvider = ProviderBuilder::default()
            .connect_client(rpc.clone())
            .erased();
        let reading = asking.clone();
        let (provider, signing_provider) = match &signing {
            Signing::Keys(keys) | Signing::Roles { keys, .. } if !keys.is_empty() => {
                let mut wallet = EthereumWallet::from(keys[0].clone());
                for key in &keys[1..] {
                    wallet.register_signer(key.clone());
                }
                let signs = signing_builder(chain_id).wallet(wallet).connect_client(rpc);
                (signs.clone().erased(), Some(signs))
            }
            // The node holds unlocked accounts' keys and numbers their
            // transactions itself.
            _ => (asking, None),
        };

        let (accounts, keys) = match signing {
            Signing::Unlocked => (
                provider
                    .get_accounts()
                    .await
                    .context("the chain's accounts")?,
                None,
            ),
            Signing::Keys(keys) => (
                keys.iter().map(|key| key.address()).collect(),
                Some(Arc::new(
                    keys.into_iter().map(|key| (key.address(), key)).collect(),
                )),
            ),
            Signing::Roles { accounts, keys } => (
                accounts,
                Some(Arc::new(
                    keys.into_iter().map(|key| (key.address(), key)).collect(),
                )),
            ),
            Signing::ReadOnly => (Vec::new(), None),
        };
        let (logs, log_endpoints) = match config
            .logs_rpc
            .as_deref()
            .filter(|list| !list.trim().is_empty())
        {
            Some(list) => (
                ProviderBuilder::default()
                    .connect_client(rpc_client(list, "logs_rpc", Some(chain_id))?)
                    .erased(),
                Endpoints::new(list, "logs_rpc", Some(chain_id), chain_id)?
                    .checked(chain_id, &config.label, "logs_rpc", false, false)
                    .await?,
            ),
            None => (reading, endpoints.clone()),
        };
        Ok(Self {
            provider,
            policy: crate::rpc_policy::RpcPolicy::default(),
            log_snapshot: None,
            log_verification: None,
            rpc_url: rpc_url.to_string(),
            chain_id,
            accounts,
            keys,
            turns: Default::default(),
            nonces: Default::default(),
            logs,
            log_endpoints,
            log_range: config.log_range,
            endpoints,
            signing: signing_provider,
            label: config.label.clone(),
        })
    }

    /// The most blocks one of this client's log queries spans, if capped.
    pub fn log_range(&self) -> Option<u64> {
        self.log_range
    }

    /// Whether the node itself holds the accounts' keys, which is what makes
    /// it a development chain.
    pub fn is_development(&self) -> bool {
        self.keys.is_none() && !self.accounts.is_empty()
    }

    /// Send the chain's own coin, for gas.
    pub async fn transfer(
        &self,
        from: Address,
        to: Address,
        value: alloy::primitives::U256,
    ) -> Result<TransactionReceipt> {
        self.transfer_noting(from, to, value, |_| {}).await
    }

    /// Send the chain's own coin, and hand the transfer's hash to `sent_as`
    /// as soon as the node has taken it, before waiting for it to land. A
    /// caller whose wait runs out can still look for it by that hash rather
    /// than send it again.
    pub async fn transfer_noting(
        &self,
        from: Address,
        to: Address,
        value: alloy::primitives::U256,
        sent_as: impl FnOnce(B256) + Send,
    ) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_value(value);
        let receipt = self
            .sent(
                request,
                RECEIPT_TIMEOUT,
                "sending gas money",
                "a transfer",
                sent_as,
            )
            .await?;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted)
                .context(format!("the transfer to {to} was reverted by the chain")));
        }
        Ok(receipt)
    }

    /// The chain's own coin an address holds.
    pub async fn balance(&self, of: Address) -> Result<alloy::primitives::U256> {
        self.provider
            .get_balance(of)
            .await
            .context("reading a balance")
    }

    /// Whether this client can sign as `who`. Where it signs with keys of
    /// its own, only with the account's key here: asking it of any other is
    /// an error that says which role the account plays, before anything is
    /// sent. Where it holds no keys, the node is asked to sign (a
    /// development chain's unlocked accounts), as before, and the node
    /// decides.
    pub fn holds_key_for(&self, who: Address) -> Result<()> {
        let Some(keys) = self.keys.as_ref() else {
            return Ok(());
        };
        if keys.contains_key(&who) {
            return Ok(());
        }
        let role = self
            .accounts
            .iter()
            .position(|account| *account == who)
            .and_then(|index| ROLES.iter().find(|role| role.account_index() == index))
            .map(|role| format!("the {} role's account ", role.label()))
            .unwrap_or_default();
        bail!(
            "this client holds no key for {role}{who}: on a testnet a role signs with SHONIN_TESTNET_EVM_MNEMONIC, or the keeper alone with SHONIN_TESTNET_KEEPER_KEY"
        )
    }

    /// The account that plays a part on this chain.
    pub fn account(&self, role: Role) -> Result<Address> {
        self.accounts
            .get(role.account_index())
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "no account plays the {} role here: on a testnet, set SHONIN_TESTNET_EVM_MNEMONIC or connect beside the chain's deployment file; on mainnet, clients only read",
                    role.label()
                )
            })
    }

    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    /// The `rpc` endpoints, each on its own, in the list's order: for a call
    /// whose answer depends on which endpoint runs it, asked of one known to
    /// run it as asked. Each is held to the chain, with its URL kept out of
    /// its errors, as the client's own connection is.
    pub fn each_endpoint(&self) -> Vec<DynProvider> {
        self.endpoints
            .each
            .iter()
            .map(|endpoint| endpoint.provider.clone())
            .collect()
    }

    /// Read a contract, without sending anything.
    pub async fn read<C: SolCall>(&self, to: Address, call: C) -> Result<C::Return> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let returned = self
            .provider
            .call(request)
            .await
            .with_context(|| format!("reading {} at {to}", C::SIGNATURE))?;
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// Read a contract as it was at a past block. A chain that keeps its
    /// history is a record nobody has to be trusted to have kept.
    ///
    /// A read pinned to a block (any but the head's tags `latest` and
    /// `pending`) goes only to the endpoints that answer as of the block
    /// asked: some answer with the head's state whatever block is asked,
    /// without an error. Which do was found out when the client connected
    /// ([`honours_blocks`]). Those known to honour the block are asked
    /// first; one that couldn't be found out is checked again (at most once
    /// a [`PROBE_AGAIN_AFTER`]) before it is asked, and asked unchecked only
    /// while no endpoint is known to honour the block, which is logged. The read moves on from an endpoint that
    /// doesn't answer, limits the rate or no longer has the state asked for,
    /// and fails, naming the chain, when only endpoints that ignore the block
    /// are left.
    pub async fn read_at<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: alloy::eips::BlockId,
    ) -> Result<C::Return> {
        use alloy::eips::{BlockId, BlockNumberOrTag};
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let at_the_head = matches!(
            block,
            BlockId::Number(BlockNumberOrTag::Latest | BlockNumberOrTag::Pending)
        );
        let returned = if at_the_head {
            self.provider
                .call(request)
                .block(block)
                .await
                .map_err(anyhow::Error::new)
        } else {
            self.call_pinned(request, block).await
        }
        .with_context(|| format!("reading {} at block {block:?}", C::SIGNATURE))?;
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// A call pinned to `block`, through the endpoints that honour it.
    ///
    /// An endpoint that says it hasn't reached the block ("header not
    /// found", "unknown block" and their kin) is behind, which is its own
    /// state, not the chain's answer: the read goes on to the next, and only
    /// an endpoint that answered becomes the one asked first next time. When
    /// no endpoint has reached the block, they are all asked again a few
    /// times, a moment apart, as for a log window no endpoint has reached;
    /// and when every endpoint asked limited the rate, as for any read
    /// ([`RateLimits`]). A revert, or any other refusal of the call itself,
    /// is the answer.
    async fn call_pinned(
        &self,
        request: TransactionRequest,
        block: alloy::eips::BlockId,
    ) -> Result<Bytes> {
        let mut last: Option<anyhow::Error> = None;
        let mut asked_any = false;
        let mut behind_tries = 0;
        let mut limits = RateLimits::default();
        loop {
            let mut behind = false;
            let mut round = RateLimits::round();
            // Those known to honour the block first. One not known is asked
            // again first, and asked unchecked only while none is known to.
            let any_known = self
                .endpoints
                .each
                .iter()
                .any(|endpoint| endpoint.honours() == Some(true));
            let mut order = self.endpoints.order();
            order.sort_by_key(|at| self.endpoints.each[*at].honours() != Some(true));
            for at in order {
                let endpoint = &self.endpoints.each[at];
                match endpoint.honours_now().await {
                    Some(false) => continue,
                    Some(true) => {}
                    None if any_known => continue,
                    None => tracing::warn!(
                        chain = %self.label,
                        endpoint = at + 1,
                        "no endpoint is known to answer a read at a past block as of that block, \
                         and this one could not be checked again: the read is sent to it unchecked"
                    ),
                }
                asked_any = true;
                match endpoint.provider.call(request.clone()).block(block).await {
                    Ok(returned) => {
                        self.endpoints.answered(at);
                        return Ok(returned);
                    }
                    Err(alloy::rpc::json_rpc::RpcError::ErrorResp(refusal))
                        if not_reached_yet(refusal.code, &refusal.message) =>
                    {
                        behind = true;
                        round.other();
                        last = Some(anyhow!(
                            "endpoint {} of {}'s rpc has not reached the block yet: {}",
                            at + 1,
                            self.label,
                            refusal.message
                        ));
                    }
                    // The chain's answer about the call itself (a revert, a
                    // bad argument) is the answer.
                    Err(refusal @ alloy::rpc::json_rpc::RpcError::ErrorResp(_)) => {
                        return Err(refusal.into());
                    }
                    // The endpoint's own trouble: on to the next.
                    Err(error) => {
                        round.saw(&error);
                        last = Some(error.into());
                    }
                }
            }
            if behind && behind_tries < PINNED_BEHIND_TRIES {
                behind_tries += 1;
                tokio::time::sleep(PINNED_BEHIND_PAUSE).await;
                continue;
            }
            if limits.wait_after(round).await {
                continue;
            }
            break;
        }
        match last {
            Some(error) => Err(error),
            None if !asked_any => bail!(
                "no endpoint of {}'s rpc answers a read at a past block as of that block \
                 (each answers with the head's state), so it is not read",
                self.label
            ),
            None => bail!("no endpoint of {}'s rpc answered", self.label),
        }
    }

    /// A numbered block's canonical identity, checked afresh on the node.
    pub async fn block_identity(&self, number: u64) -> Result<B256> {
        Ok(self.numbered_header(number).await?.0)
    }

    async fn numbered_header(&self, number: u64) -> Result<(B256, u64)> {
        let header = self.header_at(number).await?;
        Ok((header.hash, header.timestamp))
    }

    /// Fresh details of exactly the numbered canonical block requested.
    pub async fn header_at(&self, number: u64) -> Result<HeadObservation> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::number(number))
            .await
            .context("a numbered block")?
            .ok_or_else(|| anyhow!("no block {number}"))?;
        if block.header.number != number {
            bail!("the node answered another block instead of {number}");
        }
        Ok(HeadObservation {
            number: block.header.number,
            timestamp: block.header.timestamp,
            hash: block.header.hash,
            parent_hash: block.header.parent_hash,
            logs_bloom: block.header.logs_bloom,
        })
    }

    /// A clone sharing only this bounded, numbered observation of logs.
    /// Uncovered ranges, addresses and dynamic block tags use ordinary reads.
    /// The caller must recheck the end block before publishing derived state:
    /// a reorg invalidates its accumulated history as well as this observation.
    pub async fn capture_logs(
        &self,
        addresses: Vec<Address>,
        from: u64,
        to: u64,
        expected: B256,
    ) -> Result<Self> {
        if from > to
            || to - from >= self.policy.log_batch_blocks
            || addresses.is_empty()
            || addresses.len() > self.policy.log_batch_addresses as usize
        {
            bail!("the log observation exceeds its configured range or address bounds");
        }
        if self.block_identity(to).await? != expected {
            bail!("the log observation's end block changed");
        }
        let filter = Filter::new()
            .address(addresses.clone())
            .from_block(from)
            .to_block(to);
        // Never build another observation from an earlier one.
        let mut live = self.clone();
        live.log_snapshot = None;
        let logs = live.logs(&filter).await?;
        if logs.len() > self.policy.log_batch_logs as usize {
            bail!("the log observation exceeds its configured log bound");
        }
        let mut identities = HashMap::new();
        for log in &logs {
            if !filter.rpc_matches(log) || log.removed {
                bail!("the node returned a removed or unmatched log");
            }
            let number = log.block_number.context("a log has no block number")?;
            let hash = log.block_hash.context("a log has no block identity")?;
            if let Some(previous) = identities.insert(number, hash)
                && previous != hash
            {
                bail!("logs disagree on their block's identity");
            }
        }
        for (number, hash) in identities {
            if self.block_identity(number).await? != hash {
                bail!("a log belongs to a replaced block");
            }
        }
        if self.block_identity(to).await? != expected {
            bail!("the log observation's end block changed while reading it");
        }
        live.log_snapshot = Some(Arc::new(LogSnapshot {
            from,
            to,
            addresses,
            logs,
        }));
        Ok(live)
    }

    /// When a past block was mined, in the chain's own time.
    pub async fn block_timestamp_at(&self, number: u64) -> Result<u64> {
        let Some(scope) = &self.log_verification else {
            return Ok(self.numbered_header(number).await?.1);
        };
        if number > scope.head {
            bail!("a block time is newer than the observed head");
        }
        let mut identities = scope.identities.lock().await;
        if let Some((_, Some(time))) = identities.get(&number) {
            return Ok(*time);
        }
        let (hash, time) = self.numbered_header(number).await?;
        if let Some((expected, _)) = identities.get(&number)
            && hash != *expected
        {
            bail!("a block time belongs to a replaced block");
        }
        if identities.contains_key(&number)
            || identities.len() < self.policy.log_batch_blocks as usize
        {
            identities.insert(number, (hash, Some(time)));
        }
        Ok(time)
    }

    /// Send a call as `from`, and wait for it to land.
    pub async fn send<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
    ) -> Result<TransactionReceipt> {
        self.send_with(from, to, call, None).await
    }

    /// Send a call as `from` with the gas limit given rather than the node's
    /// estimate, and wait for it to land: for a call whose estimate leaves
    /// too little for what it tries inside and may let fail.
    pub async fn send_with_gas_limit<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
        gas_limit: u64,
    ) -> Result<TransactionReceipt> {
        self.send_with(from, to, call, Some(gas_limit)).await
    }

    async fn send_with<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
        gas_limit: Option<u64>,
    ) -> Result<TransactionReceipt> {
        let mut request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(call.abi_encode());
        if let Some(gas_limit) = gas_limit {
            request = request.with_gas_limit(gas_limit);
        }
        let receipt = self
            .sent(
                request,
                RECEIPT_TIMEOUT,
                &format!("sending {}", C::SIGNATURE),
                C::SIGNATURE,
                |_| {},
            )
            .await?;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted)
                .context(format!("{} was reverted by the chain", C::SIGNATURE)));
        }
        Ok(receipt)
    }

    /// Send bytes that are not a call of ours, such as a creation through the
    /// deterministic deployer.
    pub async fn send_raw(
        &self,
        from: Address,
        to: Address,
        data: alloy_primitives::Bytes,
    ) -> Result<TransactionReceipt> {
        self.send_raw_with_gas_limit(from, to, data, None).await
    }

    /// The same, with the gas limit given (when there is one) rather than
    /// the node's estimate: some endpoints estimate against a smaller block
    /// than the sender's transactions go into.
    pub async fn send_raw_with_gas_limit(
        &self,
        from: Address,
        to: Address,
        data: alloy_primitives::Bytes,
        gas_limit: Option<u64>,
    ) -> Result<TransactionReceipt> {
        let mut request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(data);
        if let Some(gas_limit) = gas_limit {
            request = request.with_gas_limit(gas_limit);
        }
        let receipt = self
            .sent(request, RECEIPT_TIMEOUT, "sending", "a transaction", |_| {})
            .await?;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted).context("the transaction reverted"));
        }
        Ok(receipt)
    }

    /// Send bytes as [`Client::send_raw`] does, and hand the transaction's
    /// hash to `sent_as` as soon as the node has it, before waiting for it
    /// to land: a caller whose wait runs out can look for it by that hash
    /// rather than send it again.
    pub async fn send_raw_noting(
        &self,
        from: Address,
        to: Address,
        data: alloy_primitives::Bytes,
        sent_as: impl FnOnce(B256) + Send,
    ) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(data);
        let receipt = self
            .sent(
                request,
                RECEIPT_TIMEOUT,
                "sending",
                "a transaction",
                sent_as,
            )
            .await?;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted).context("the transaction reverted"));
        }
        Ok(receipt)
    }

    /// A provider that signs as `signer` itself, over this client's
    /// endpoints, each held to the chain and with its URL kept out of every
    /// error: for a tool that sends from a key that plays no role. The
    /// transaction is signed once, so whichever endpoint takes it, the same
    /// bytes go to each.
    pub fn signing_provider(&self, signer: PrivateKeySigner) -> Result<DynProvider> {
        Ok(ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer))
            .connect_client(rpc_client(&self.rpc_url, "rpc", Some(self.chain_id))?)
            .erased())
    }

    /// Deploy a contract from its bytecode and constructor arguments. A
    /// creation the chain reverted is an error: its receipt still names the
    /// address the contract would have had, with nothing there.
    pub async fn deploy(&self, from: Address, bytecode: Vec<u8>) -> Result<Address> {
        self.deploy_with_gas_limit(from, bytecode, None).await
    }

    /// The same, with the gas limit given (when there is one) rather than
    /// the node's estimate.
    pub async fn deploy_with_gas_limit(
        &self,
        from: Address,
        bytecode: Vec<u8>,
        gas_limit: Option<u64>,
    ) -> Result<Address> {
        let mut request = TransactionRequest::default()
            .with_from(from)
            .with_deploy_code(bytecode);
        if let Some(gas_limit) = gas_limit {
            request = request.with_gas_limit(gas_limit);
        }
        let receipt = self
            .sent(request, DEPLOY_TIMEOUT, "deploying", "a deployment", |_| {})
            .await?;
        if !receipt.status() {
            return Err(
                anyhow::Error::new(Reverted).context("the deployment was reverted by the chain")
            );
        }
        receipt
            .contract_address
            .ok_or_else(|| anyhow!("the chain accepted a deployment without giving it an address"))
    }

    /// Send a transaction and wait, for at most `within`, for its receipt,
    /// telling `sent_as` its hash once the node has it.
    ///
    /// A send from an account this client signs for waits for that
    /// account's turn and holds it until the receipt comes or the wait runs
    /// out. A node that signs for the account numbers its transactions
    /// itself, so those need no turn.
    async fn sent(
        &self,
        request: TransactionRequest,
        within: Duration,
        sending: &str,
        what: &str,
        sent_as: impl FnOnce(B256) + Send,
    ) -> Result<TransactionReceipt> {
        if let Some(from) = request.from {
            self.holds_key_for(from)
                .with_context(|| sending.to_string())
                .map_err(NeverSent::wrap)?;
        }
        let _turn = match request.from {
            Some(from)
                if self
                    .keys
                    .as_ref()
                    .is_some_and(|keys| keys.contains_key(&from)) =>
            {
                let turn = self
                    .turns
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .entry(from)
                    .or_default()
                    .clone();
                Some(turn.lock_owned().await)
            }
            _ => None,
        };
        // Numbered here, while the account's turn is held: the node's count
        // of the account's sends, pending ones included, but never below the
        // one after the last send this client saw land ([`NonceFloor`]).
        let mut request = request;
        let mut numbered = None;
        if let (Some(_), Some(from), None) = (&_turn, request.from, request.nonce) {
            let nonce = self
                .nonces
                .next(&self.provider, from)
                .await
                .with_context(|| sending.to_string())?;
            request = request.with_nonce(nonce);
            numbered = Some((from, nonce));
        }
        let hash = match (&_turn, &self.signing) {
            // Signed here: handed to the endpoints one at a time, so a send
            // one endpoint may have taken is never reported refused by the
            // next ([`hand_over`]).
            (Some(_), Some(signing)) => {
                use alloy::eips::eip2718::Encodable2718;
                let filled = signing
                    .fill(request)
                    .await
                    .with_context(|| sending.to_string())
                    .map_err(NeverSent::wrap)?;
                let Some(signed) = filled.as_envelope() else {
                    return Err(NeverSent::wrap(anyhow!(
                        "{what} was filled in but not signed"
                    )));
                };
                let hash = *signed.tx_hash();
                let deadline = tokio::time::Instant::now() + within;
                match hand_over(&self.endpoints, &signed.encoded_2718(), deadline).await {
                    Handed::MayLand => hash,
                    Handed::Refused(refusal) => {
                        return Err(anyhow::Error::new(refusal).context(sending.to_string()));
                    }
                    Handed::NotTaken(limited) => {
                        return Err(NeverSent::wrap(
                            anyhow::Error::new(limited).context(sending.to_string()),
                        ));
                    }
                }
            }
            _ => *self
                .provider
                .send_transaction(request)
                .await
                .with_context(|| sending.to_string())?
                .tx_hash(),
        };
        sent_as(hash);
        let receipt = receipt_within(&self.provider, hash, within, what).await?;
        if let Some((from, nonce)) = numbered {
            self.nonces.landed(from, nonce);
        }
        Ok(receipt)
    }

    /// Have the node sign a digest as `who`, the way a wallet would: the
    /// signature a contract recovers with `toEthSignedMessageHash`.
    pub async fn sign(&self, who: Address, digest: B256) -> Result<Bytes> {
        self.holds_key_for(who)?;
        if let Some(key) = self.keys.as_ref().and_then(|keys| keys.get(&who)) {
            let signature = key
                .sign_message_sync(digest.as_slice())
                .context("signing")?;
            return Ok(Bytes::from(signature.as_bytes().to_vec()));
        }
        let signature: Bytes = self
            .provider
            .raw_request("eth_sign".into(), (who, Bytes::from(digest.to_vec())))
            .await
            .with_context(|| format!("asking the chain to sign as {who}"))?;
        Ok(signature)
    }

    /// Sign a depositor's statement for a vault: EIP-712, so a wallet would
    /// show the words. With the account's key here it signs the digest
    /// itself; otherwise the node signs, as a development chain does for its
    /// own accounts.
    pub async fn sign_statement(
        &self,
        who: Address,
        vault: Address,
        restricted: &str,
    ) -> Result<Bytes> {
        self.holds_key_for(who)?;
        if let Some(key) = self.keys.as_ref().and_then(|keys| keys.get(&who)) {
            let digest = crate::statement::digest(self.chain_id, vault, who, restricted);
            let signature = key.sign_hash_sync(&digest).context("signing")?;
            return Ok(Bytes::from(signature.as_bytes().to_vec()));
        }
        let typed = crate::statement::typed_data(self.chain_id, vault, who, restricted);
        let signature: Bytes = self
            .provider
            .raw_request("eth_signTypedData_v4".into(), (who, typed.to_string()))
            .await
            .with_context(|| format!("asking the chain to sign a statement as {who}"))?;
        Ok(signature)
    }

    /// Ask the node something the contracts do not cover, such as how fast a
    /// development chain should run its clock.
    pub async fn raw_request(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.provider
            .raw_request(method.into(), params)
            .await
            .with_context(|| format!("asking the chain for {method}"))
    }

    /// The logs a filter matches, in block order, through whatever the
    /// endpoints allow.
    ///
    /// Log queries go to the chain's `logs_rpc` endpoints when it names
    /// any, and to its `rpc` endpoints otherwise. With a width cap
    /// (`log_range`), a numbered range is read in consecutive windows of at
    /// most that many blocks, each block in exactly one, a few at a time
    /// ([`LOG_WINDOWS_IN_FLIGHT`]), and a range from a numbered block to the
    /// head is read up to the head's number, as the first of those
    /// endpoints to answer gives it. Without one, a numbered range is one
    /// window, and a range to the head is asked once, its end being
    /// whatever the endpoint answering has reached.
    ///
    /// A read is returned only when every block of the range was read by an
    /// endpoint that had reached it. A node behind the range's newest block
    /// may answer with the logs it has and no error, so each window goes to
    /// one endpoint at a time, and only to one whose own head, asked just
    /// before (once per endpoint and read, and again only when a window ends
    /// past what it last said), is at or past the window's end. An endpoint
    /// that is behind, says it hasn't reached the window, limits the rate,
    /// doesn't answer, or no longer has the logs, is passed over for the
    /// next; when every endpoint has been passed over and one of them was
    /// behind, the window is asked again a few times
    /// ([`LOGS_BEHIND_TRIES`]), a second and a half apart, and then the read
    /// fails. A window an endpoint refuses as too wide is read there in
    /// halves (and those in halves, as far as one block). A filter with no
    /// numbered range is asked once, through the failover.
    ///
    /// Behind one endpoint's name several nodes may answer, and its head
    /// and its logs may come from different ones: the check is exact per
    /// endpoint, not per node behind it.
    pub async fn logs(&self, filter: &Filter) -> Result<Vec<Log>> {
        let logs = match self
            .log_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.read(filter))
        {
            Some(logs) => logs,
            None => self.logs_uncached(filter).await?,
        };
        if let Some(scope) = &self.log_verification {
            let mut identities = scope.identities.lock().await;
            for log in &logs {
                if !filter.rpc_matches(log) || log.removed {
                    bail!("the node returned a removed or unmatched log");
                }
                let number = log.block_number.context("a log has no block number")?;
                let hash = log.block_hash.context("a log has no block identity")?;
                if number > scope.head {
                    bail!("a log is newer than the observed head");
                }
                let canonical = match identities.get(&number) {
                    Some((hash, _)) => *hash,
                    None => {
                        let (hash, time) = self.numbered_header(number).await?;
                        if identities.len() < self.policy.log_batch_blocks as usize {
                            identities.insert(number, (hash, Some(time)));
                        }
                        hash
                    }
                };
                if hash != canonical {
                    bail!("a log belongs to a replaced block");
                }
            }
        }
        Ok(logs)
    }

    /// A refresh-local log verifier. Every direct, fallback and shared log
    /// read checks its returned blocks against this canonical branch. The
    /// caller must check this head again before publishing, then discard the
    /// clone; the header cache must never survive that observation's scope.
    pub fn verifying_logs(&self, head: u64, identity: B256) -> Self {
        let mut client = self.clone();
        client.log_snapshot = None;
        client.log_verification = Some(Arc::new(LogVerification {
            head,
            identities: tokio::sync::Mutex::new(HashMap::from([(head, (identity, None))])),
        }));
        client
    }

    async fn logs_uncached(&self, filter: &Filter) -> Result<Vec<Log>> {
        use alloy::eips::BlockNumberOrTag;
        use alloy::rpc::types::FilterBlockOption;
        let from = filter.get_from_block();
        let mut to = filter.get_to_block();
        let to_the_head = matches!(
            filter.block_option,
            FilterBlockOption::Range {
                to_block: None | Some(BlockNumberOrTag::Latest),
                ..
            }
        );
        let heads = Heads::new(self.log_endpoints.each.len());
        if self.log_range.is_some() && from.is_some() && to_the_head {
            to = Some(
                logs_head(&self.log_endpoints, &heads)
                    .await
                    .context("the head, to read logs up to")?,
            );
        }
        let Some((from, to)) = from.zip(to).filter(|(from, to)| from <= to) else {
            return self.logs.get_logs(filter).await.context("reading logs");
        };

        let mut windows = LogWindows::new(from, to, self.log_range);
        let mut running = tokio::task::JoinSet::new();
        // Windows read but not yet handed on, because an earlier one hasn't
        // come back yet: at most the number in flight.
        let mut read: std::collections::BTreeMap<usize, Vec<Log>> = Default::default();
        let (mut started, mut handed_on) = (0usize, 0usize);
        let mut found = Vec::new();
        loop {
            while started - handed_on < LOG_WINDOWS_IN_FLIGHT
                && let Some((from, to)) = windows.next()
            {
                let endpoints = self.log_endpoints.clone();
                let heads = heads.clone();
                let window = filter.clone();
                let index = started;
                running.spawn(async move {
                    (
                        index,
                        read_window(&endpoints, &heads, &window, from, to).await,
                    )
                });
                started += 1;
            }
            let Some(joined) = running.join_next().await else {
                break;
            };
            let (index, logs) =
                joined.map_err(|_| anyhow!("a log read stopped before it ended"))?;
            // A window that failed fails the whole read; the rest are
            // stopped as the set is dropped.
            read.insert(index, logs?);
            while let Some(logs) = read.remove(&handed_on) {
                found.extend(logs);
                handed_on += 1;
            }
        }
        Ok(found)
    }

    pub async fn block_number(&self) -> Result<u64> {
        self.provider.get_block_number().await.context("the head")
    }

    /// The chain's own clock. Everything the platform dates is relative to
    /// this, never to the reader's watch.
    pub async fn block_timestamp(&self) -> Result<u64> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::latest())
            .await
            .context("the head block")?
            .ok_or_else(|| anyhow!("the chain has no blocks yet"))?;
        Ok(block.header.timestamp)
    }

    /// The head and its time, from one read of the head block: two reads
    /// could land either side of a new block.
    pub async fn head_and_time(&self) -> Result<(u64, u64)> {
        let (number, timestamp, _) = self.head_observation().await?;
        Ok((number, timestamp))
    }

    /// The head's number, time and identity from the same header.
    pub async fn head_observation(&self) -> Result<(u64, u64, B256)> {
        let head = self.head_details().await?;
        Ok((head.number, head.timestamp, head.hash))
    }

    /// Fresh header details for numbered event observation. A bloom alone
    /// never covers missing intermediate blocks or a replaced parent.
    pub async fn head_details(&self) -> Result<HeadObservation> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::latest())
            .await
            .context("the head block")?
            .ok_or_else(|| anyhow!("the chain has no blocks yet"))?;
        Ok(HeadObservation {
            number: block.header.number,
            timestamp: block.header.timestamp,
            hash: block.header.hash,
            parent_hash: block.header.parent_hash,
            logs_bloom: block.header.logs_bloom,
        })
    }

    /// Whether anything is deployed at an address: how a deploy knows the
    /// file it found is describing a chain that still exists.
    pub async fn has_code(&self, at: Address) -> Result<bool> {
        let code = self
            .provider
            .get_code_at(at)
            .await
            .context("reading a contract's code")?;
        Ok(!code.is_empty())
    }

    /// A client that acts as a key the service generated itself, rather than
    /// as one of the accounts the chain published.
    ///
    /// An executor's trading keys are like this: they are made where the
    /// strategy runs, they are blessed for one vault, and nothing outside that
    /// enclave ever holds them.
    pub fn with_key(&self, signer: PrivateKeySigner) -> Result<KeyClient> {
        let address = signer.address();
        let signing = signing_builder(self.chain_id)
            .wallet(EthereumWallet::from(signer.clone()))
            .connect_client(rpc_client(&self.rpc_url, "rpc", Some(self.chain_id))?);
        // The key's turn to send is the one every client made from this
        // one takes for this key, as its nonce floor is.
        let sending = self
            .turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(address)
            .or_default()
            .clone();
        Ok(KeyClient {
            provider: signing.clone().erased(),
            signing,
            signer,
            address,
            sending,
            spent: Default::default(),
            noting: None,
            nonces: self.nonces.clone(),
            endpoints: self.endpoints.clone(),
        })
    }

    /// Read the deployment file, and check it describes this chain.
    pub async fn deployments(&self, path: &std::path::Path) -> Result<Deployments> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let deployments = Deployments::from_json(&text).context("the deployment file")?;
        if deployments.chain_id != self.chain_id {
            bail!(
                "the deployment file is for chain {} and this chain is {}",
                deployments.chain_id,
                self.chain_id
            );
        }
        Ok(deployments)
    }

    /// Wait until a deploy has written the file and its contracts are there.
    pub async fn wait_for_deployments(&self, path: &std::path::Path) -> Result<Deployments> {
        // By the clock: each look can itself take a request's timeout.
        let started = tokio::time::Instant::now();
        loop {
            if path.exists()
                && let Ok(deployments) = self.deployments(path).await
                && self.has_code(deployments.factory).await.unwrap_or(false)
            {
                return Ok(deployments);
            }
            if started.elapsed() > Duration::from_secs(180) {
                bail!("no deployment appeared at {}", path.display());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// The chain, as seen by a key that signs for itself.
#[derive(Clone)]
pub struct KeyClient {
    provider: DynProvider,
    /// The same connection, filling transactions in and signing them here,
    /// so a transaction's hash is known before it is sent.
    signing: SigningProvider,
    signer: PrivateKeySigner,
    pub address: Address,
    /// One transaction at a time from this key. Several tasks act with the
    /// same key, and two sends at once would take the same nonce, leaving one
    /// of them waiting for a receipt that never comes. The turn is held by
    /// the task that sends, not by the caller, so it ends when the send does.
    sending: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// What this key has paid for gas, counted from the receipt of every
    /// transaction it sent, reverted ones included, whenever the receipt
    /// arrives.
    spent: Arc<std::sync::Mutex<Spent>>,
    /// Where each of this client's broadcasts is noted as one that may still
    /// land, from the moment it is broadcast until its receipt is found or
    /// no longer looked for (see [`KeyClient::noting`]).
    noting: Option<Unanswered>,
    /// The nonce after this key's last send seen to land, shared with the
    /// client it was made from and every other key client made from it.
    nonces: NonceFloor,
    /// The chain's endpoints, each on its own, that a send is handed to.
    endpoints: Endpoints,
}

/// What a key has paid for gas, in the native coin's smallest unit, and the
/// transactions already counted in it.
///
/// A key signs deterministically, so a send filled in exactly as an earlier
/// one was is the same transaction, with the same hash, and both sends watch
/// for its one receipt. Its gas is counted by whichever watch finds it first
/// and never again. A hash is remembered for twice the landing window, past
/// the end of any watch that could still find it, and then forgotten, so the
/// set stays small.
#[derive(Default)]
struct Spent {
    total: u128,
    counted: HashMap<B256, std::time::Instant>,
    /// Sends whose gas may still be added: started, and not yet landed or
    /// given up on.
    in_flight: usize,
    /// When the last of them landed or was given up on.
    last_settled: Option<std::time::Instant>,
}

/// Counts a send as in flight from when it starts until its watch ends,
/// however it ends.
struct InFlight(Arc<std::sync::Mutex<Spent>>);

impl InFlight {
    fn start(spent: Arc<std::sync::Mutex<Spent>>) -> Self {
        if let Ok(mut spent) = spent.lock() {
            spent.in_flight += 1;
        }
        Self(spent)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut spent) = self.0.lock() {
            spent.in_flight = spent.in_flight.saturating_sub(1);
            spent.last_settled = Some(std::time::Instant::now());
        }
    }
}

impl Spent {
    const REMEMBERED_FOR: Duration = Duration::from_secs(2 * LANDING_TIMEOUT.as_secs());

    fn count(&mut self, hash: B256, cost: u128, now: std::time::Instant) {
        self.counted
            .retain(|_, at| now.duration_since(*at) < Self::REMEMBERED_FOR);
        if self.counted.insert(hash, now).is_none() {
            self.total = self.total.saturating_add(cost);
        }
    }
}

/// A transaction that was sent and never came back with a receipt.
#[derive(Debug)]
pub struct NeverLanded;

impl std::fmt::Display for NeverLanded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("it never landed")
    }
}

impl std::error::Error for NeverLanded {}

/// A transaction the node never said it had taken, within the time a key's
/// turn to send is held. It may or may not have been broadcast.
#[derive(Debug)]
pub struct NeverAcknowledged;

impl std::fmt::Display for NeverAcknowledged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the node never said it had it")
    }
}

impl std::error::Error for NeverAcknowledged {}

/// A send that failed before anything was broadcast: filling it in (the gas
/// estimate, the fee or the nonce), or the check it was sent on, failed. The
/// node never had it, so it can't land, however unanswered the failure was.
#[derive(Debug)]
pub struct NeverSent(anyhow::Error);

impl NeverSent {
    pub(crate) fn wrap(error: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(NeverSent(error))
    }
}

impl std::fmt::Display for NeverSent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing was sent")
    }
}

impl std::error::Error for NeverSent {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// A transaction that was mined and reverted: the chain looked at it and
/// refused it, after it had been paid for.
#[derive(Debug)]
pub struct Reverted;

impl std::fmt::Display for Reverted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("it was reverted by the chain")
    }
}

impl std::error::Error for Reverted {}

/// Whether a send failed because the chain could not be reached, or never
/// answered, rather than because it looked at the transaction and refused
/// it. A refusal is an answer; this is the absence of one.
pub fn unanswered(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<NeverLanded>()
            || cause.is::<NeverAcknowledged>()
            || cause
                .downcast_ref::<alloy::transports::TransportError>()
                .is_some_and(|error| error.is_transport_error())
            || cause.is::<alloy::providers::PendingTransactionError>()
    })
}

/// Whether a failure is the node's trouble rather than an answer about what
/// was asked: it went unanswered, or the endpoint is limiting the rate of
/// requests, however it words that. Work that failed this way is nobody's
/// doing and can be tried again later.
pub fn node_trouble(error: &anyhow::Error) -> bool {
    unanswered(error)
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<alloy::transports::TransportError>()
                .and_then(|error| error.as_error_resp())
                .is_some_and(|response| is_rate_limit(response.code, &response.message))
                || is_rate_limit(0, &cause.to_string())
        })
}

/// Whether a failed send may still land: it went unanswered after it was
/// broadcast, or may have been. One that failed before anything was sent
/// can't, so it is no reason to hold the next send back.
pub fn may_still_land(error: &anyhow::Error) -> bool {
    unanswered(error) && !error.chain().any(|cause| cause.is::<NeverSent>())
}

/// A key's last send of one kind (an approval, a blessing) that may still
/// land, kept where every sender of that kind for the key sees it, however
/// often the sender itself is started again. While it may, another sent
/// meanwhile could land as well and both be paid for.
///
/// A send made through a [`KeyClient::noting`] this is noted by the send
/// itself, at the moment it is broadcast, and the note is taken off when its
/// receipt is found or no longer looked for. The send carries on whatever
/// becomes of its caller, so a caller stopped halfway still leaves the note.
/// A send made any other way is noted by its caller, with
/// [`Unanswered::failed`].
#[derive(Clone, Default)]
pub struct Unanswered(Arc<std::sync::Mutex<Option<Note>>>);

/// One send that may still land: whose, which one, and when it no longer may.
#[derive(Clone, Copy)]
struct Note {
    key: Address,
    id: u64,
    until: tokio::time::Instant,
}

impl Unanswered {
    /// Take note of a failed send of `key`'s that its caller saw fail: one
    /// that may still land holds the next back until it no longer may. A
    /// send already noted when it was broadcast is left as noted while that
    /// note holds; a note that has run out is replaced, or this send would
    /// hold nothing back.
    pub fn failed(&self, key: Address, error: &anyhow::Error) {
        if !may_still_land(error) {
            return;
        }
        let now = tokio::time::Instant::now();
        if let Ok(mut note) = self.0.lock()
            && !note.is_some_and(|note| note.key == key && now < note.until)
        {
            *note = Some(Note {
                key,
                id: next_note(),
                until: now + LANDING_TIMEOUT,
            });
        }
    }

    /// What the send was for is seen done: nothing is waited on any more.
    pub fn settled(&self) {
        if let Ok(mut note) = self.0.lock() {
            *note = None;
        }
    }

    /// Whether a send of `key`'s may still land.
    pub fn may_still_land(&self, key: Address) -> bool {
        self.0.lock().map_or(true, |note| {
            note.is_some_and(|note| note.key == key && tokio::time::Instant::now() < note.until)
        })
    }

    /// Note a send of `key`'s as broadcast. It may land for as long as it is
    /// watched for, which is never longer than this.
    fn broadcast(&self, key: Address) -> u64 {
        let id = next_note();
        if let Ok(mut note) = self.0.lock() {
            *note = Some(Note {
                key,
                id,
                until: tokio::time::Instant::now() + RECEIPT_TIMEOUT + LANDING_TIMEOUT,
            });
        }
        id
    }

    /// The send noted as `id` is no longer watched for: take its note off,
    /// unless a later send has been noted since.
    fn watched(&self, id: u64) {
        if let Ok(mut note) = self.0.lock()
            && note.is_some_and(|note| note.id == id)
        {
            *note = None;
        }
    }
}

fn next_note() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Keeps a broadcast noted in an [`Unanswered`] for as long as it is
/// watched for, and takes the note off when the watch ends, however it ends.
struct Noted {
    unanswered: Unanswered,
    id: u64,
}

impl Noted {
    fn start(unanswered: Unanswered, key: Address) -> Self {
        let id = unanswered.broadcast(key);
        Self { unanswered, id }
    }
}

impl Drop for Noted {
    fn drop(&mut self) {
        self.unanswered.watched(self.id);
    }
}

/// How long to wait for a transaction to land before giving up on it, so a
/// dropped one is an error rather than a task that never wakes.
const RECEIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long to wait for a deployment to land. A contract's creation is
/// larger than a call and can sit longer before a block takes it.
const DEPLOY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How often a [`KeyClient::send_if`] held back by a noted send that may
/// still land looks again.
const NOTED_LOOK: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a transaction's receipt is still looked for after the sender has
/// stopped waiting, so that its gas is counted when it lands late.
pub const LANDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// The most one look for a receipt may take, so a node that stops answering
/// can't stretch the landing past its deadline.
const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl KeyClient {
    /// Send a call, and wait for it to land.
    ///
    /// Once a transaction has been written to the node its gas may be spent
    /// whatever becomes of this call, and the node can take its time to say
    /// it has it. So everything from the broadcast on runs on a task of its
    /// own: a caller that stops waiting, or is itself stopped, even halfway
    /// through the broadcast, doesn't lose the count. The caller is free to
    /// stop while it waits for its turn, since nothing has been sent then.
    pub async fn send<C: SolCall>(&self, to: Address, call: C) -> Result<TransactionReceipt> {
        let turn = self.sending.clone().lock_owned().await;
        self.send_holding(turn, to, call).await
    }

    /// Send a call only if `check`, run once this key's turn to send is
    /// taken and before anything is signed, says to; `None` if it said not
    /// to. A check made before the turn may be stale by the time the turn
    /// comes (another of this key's sends may have changed what it read, or
    /// anyone else meanwhile). A check that fails sends nothing.
    ///
    /// Inside the turn, no send of this key's is being filled in or waited
    /// on, but one given up on may still be in the node's pool: its turn
    /// ends at its deadline, and it can land long after. The chain can't
    /// show the check that one, so a client [`KeyClient::noting`] its sends
    /// doesn't check while a send noted there may still land: it gives the
    /// turn back and waits, and checks once that one has landed or no longer
    /// can. A client that notes nothing can't know of such a send, and its
    /// check is all there is.
    pub async fn send_if<C: SolCall>(
        &self,
        check: impl std::future::Future<Output = Result<bool>>,
        to: Address,
        call: C,
    ) -> Result<Option<TransactionReceipt>> {
        let turn = loop {
            let turn = self.sending.clone().lock_owned().await;
            match &self.noting {
                Some(noted) if noted.may_still_land(self.address) => {
                    drop(turn);
                    tokio::time::sleep(NOTED_LOOK).await;
                }
                _ => break turn,
            }
        };
        if !check.await.map_err(NeverSent::wrap)? {
            return Ok(None);
        }
        self.send_holding(turn, to, call).await.map(Some)
    }

    async fn send_holding<C: SolCall>(
        &self,
        turn: tokio::sync::OwnedMutexGuard<()>,
        to: Address,
        call: C,
    ) -> Result<TransactionReceipt> {
        let request = TransactionRequest::default()
            .with_from(self.address)
            .with_to(to)
            .with_input(call.abi_encode());
        let in_flight = InFlight::start(self.spent.clone());
        // One deadline for the caller and the broadcast alike, measured from
        // here rather than from whenever the broadcast task first runs.
        let deadline = tokio::time::Instant::now() + RECEIPT_TIMEOUT;
        let stage = Arc::new(Stage::default());
        let sending = tokio::spawn(broadcast(
            self.signing.clone(),
            self.provider.clone(),
            request,
            turn,
            self.spent.clone(),
            in_flight,
            stage.clone(),
            deadline,
            self.noting.clone().map(|noting| (noting, self.address)),
            (self.nonces.clone(), self.address),
            self.endpoints.clone(),
            C::SIGNATURE,
        ));
        // Giving up claims the send's stage: a broadcast that hasn't begun by
        // then never will, so the send was never sent. One that has begun may
        // still land.
        let never_landed = || {
            let error = anyhow::Error::new(NeverLanded).context(C::SIGNATURE);
            if stage.give_up() {
                NeverSent::wrap(error)
            } else {
                error
            }
        };
        let receipt = tokio::time::timeout_at(deadline, sending)
            .await
            .map_err(|_| never_landed())?
            .map_err(|_| never_landed())??;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted)
                .context(format!("{} was reverted by the chain", C::SIGNATURE)));
        }
        Ok(receipt)
    }

    /// The same key with a gas counter of its own, for transactions sent on
    /// the platform's behalf rather than the agent's, such as its blessing:
    /// those are not the author's to pay. It shares the key's turn to send,
    /// so the two never reach for the same nonce.
    pub fn unmetered(&self) -> KeyClient {
        KeyClient {
            spent: Default::default(),
            ..self.clone()
        }
    }

    /// The same key, noting each of its broadcasts in `unanswered` as one
    /// that may still land, from the moment it is broadcast until its
    /// receipt is found or no longer looked for. The note is made and taken
    /// off by the send itself, which runs on whatever becomes of its caller.
    pub fn noting(&self, unanswered: &Unanswered) -> KeyClient {
        KeyClient {
            noting: Some(unanswered.clone()),
            ..self.clone()
        }
    }

    /// What this key has paid for gas so far, in the native coin's smallest
    /// unit.
    pub fn gas_spent(&self) -> u128 {
        self.spent
            .lock()
            .map(|spent| spent.total)
            .unwrap_or_default()
    }

    /// Whether a send this key's gas count covers may still land, so its gas
    /// may still be added to [`KeyClient::gas_spent`]: it has started and
    /// its watch for the receipt hasn't ended.
    pub fn sends_in_flight(&self) -> bool {
        self.spent.lock().map_or(true, |spent| spent.in_flight > 0)
    }

    /// When the last of this key's sends landed or was given up on, if any
    /// has.
    pub fn last_settled(&self) -> Option<std::time::Instant> {
        self.spent.lock().ok().and_then(|spent| spent.last_settled)
    }

    /// Sign a digest the way a contract expects to recover it.
    pub fn sign(&self, digest: B256) -> Result<Bytes> {
        let signature = self
            .signer
            .sign_message_sync(digest.as_slice())
            .context("signing")?;
        Ok(Bytes::from(signature.as_bytes().to_vec()))
    }
}

/// How far a send has got, claimed by whichever of its broadcast and its
/// caller giving up on it comes first: a send is either broadcast before it
/// is given up on, so it may still land, or never broadcast at all.
#[derive(Default)]
struct Stage(std::sync::atomic::AtomicU8);

impl Stage {
    const FILLING: u8 = 0;
    const BROADCAST: u8 = 1;
    const GIVEN_UP: u8 = 2;

    /// Claim the broadcast; false if the send was given up on first.
    fn broadcast(&self) -> bool {
        self.claim(Self::BROADCAST)
    }

    /// Claim giving up; false if the broadcast began first.
    fn give_up(&self) -> bool {
        self.claim(Self::GIVEN_UP)
    }

    fn claim(&self, to: u8) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        match self.0.compare_exchange(Self::FILLING, to, SeqCst, SeqCst) {
            Ok(_) => true,
            Err(now) => now == to,
        }
    }
}

/// Send a transaction and wait for it to land, holding the key's turn until
/// it does or until its sender would have stopped waiting, whichever is
/// first.
///
/// The transaction is filled in (nonce, gas, fees) and signed here, so its
/// hash is known before it is sent. Once it has been sent its receipt is
/// looked for by that hash whatever the node's reply: a reply lost or too
/// slow says nothing about whether the node took the transaction, and if it
/// lands its gas is counted all the same. Only a refusal from an endpoint
/// asked before any could have taken it, or every endpoint limiting the
/// rate, means it was not taken ([`hand_over`]). Everything up to the
/// receipt runs
/// under one deadline, so a node that never answers can't keep the turn:
/// past it the turn is given up and the send is unanswered, while the
/// receipt is still looked for afterwards without holding the key's next
/// transaction up.
#[allow(clippy::too_many_arguments)]
async fn broadcast(
    signing: SigningProvider,
    provider: DynProvider,
    request: TransactionRequest,
    turn: tokio::sync::OwnedMutexGuard<()>,
    spent: Arc<std::sync::Mutex<Spent>>,
    _in_flight: InFlight,
    stage: Arc<Stage>,
    deadline: tokio::time::Instant,
    noting: Option<(Unanswered, Address)>,
    (nonces, from): (NonceFloor, Address),
    endpoints: Endpoints,
    signature: &'static str,
) -> Result<TransactionReceipt> {
    use alloy::eips::eip2718::Encodable2718;

    // Nothing has been sent while this runs: only the node's reads are
    // asked for. So whatever fails here, nothing can land. The nonce is the
    // node's count of the key's sends, but never below the one after the
    // last send of the key's this client saw land ([`NonceFloor`]): a node
    // lagging behind the one that took that send would number this one the
    // same.
    let nonce = match tokio::time::timeout_at(deadline, nonces.next(&provider, from)).await {
        Ok(nonce) => nonce.with_context(|| format!("sending {signature}"))?,
        Err(_) => {
            return Err(NeverSent::wrap(
                anyhow::Error::new(NeverAcknowledged).context(signature),
            ));
        }
    };
    let request = request.with_nonce(nonce);
    let filled = match tokio::time::timeout_at(deadline, signing.fill(request)).await {
        Ok(filled) => filled
            .with_context(|| format!("sending {signature}"))
            .map_err(NeverSent::wrap)?,
        Err(_) => {
            return Err(NeverSent::wrap(
                anyhow::Error::new(NeverAcknowledged).context(signature),
            ));
        }
    };
    let Some(signed) = filled.as_envelope() else {
        return Err(NeverSent::wrap(anyhow!(
            "{signature} was filled in but not signed"
        )));
    };
    let hash = *signed.tx_hash();
    let raw = signed.encoded_2718();

    // The caller may have given up while this was filled in, and been told
    // nothing was sent: then nothing is. Nor is anything once the deadline
    // has passed: the send couldn't go out before the caller gives up, and
    // must not be taken as one that may still land.
    if tokio::time::Instant::now() >= deadline || !stage.broadcast() {
        return Err(NeverSent::wrap(
            anyhow::Error::new(NeverAcknowledged).context(signature),
        ));
    }
    // From here it may land: noted as such, in the send itself, until the
    // watch for its receipt ends.
    let _noted = noting.map(|(unanswered, key)| Noted::start(unanswered, key));
    match hand_over(&endpoints, &raw, deadline).await {
        Handed::MayLand => {}
        Handed::Refused(refusal) => {
            drop(turn);
            return Err(anyhow::Error::new(refusal).context(format!("sending {signature}")));
        }
        Handed::NotTaken(limited) => {
            drop(turn);
            return Err(NeverSent::wrap(
                anyhow::Error::new(limited).context(format!("sending {signature}")),
            ));
        }
    }
    // Taken, already held, or no reply came back: whichever, it may land.
    let landing = land(provider, hash, spent, (nonces, from, nonce));
    tokio::pin!(landing);
    let landed = match tokio::time::timeout_at(deadline, &mut landing).await {
        Ok(landed) => landed,
        Err(_) => {
            drop(turn);
            landing.await
        }
    };
    landed.with_context(|| format!("waiting for {signature}"))
}

/// Whether a node's refusal of a send says it already holds that very
/// transaction. That is no refusal: the node has it, and it may land, so it
/// is watched for like any other send. Nodes word it differently ("already
/// known", "known transaction", "transaction already imported"); an "unknown
/// transaction" is not one of them.
fn nonce_too_low(refusal: &alloy::transports::TransportError) -> bool {
    let alloy::rpc::json_rpc::RpcError::ErrorResp(payload) = refusal else {
        return false;
    };
    let message = payload.message.to_lowercase();
    message.contains("nonce too low") || message.contains("nonce has already been used")
}

/// Whether an endpoint's error to a send is a clear refusal of the
/// transaction: "this transaction is invalid, and here is why". Only these
/// count as clear:
///
/// - its nonce: "nonce too low", "nonce too high", "invalid nonce", "nonce
///   has already been used";
/// - its sender's funds: "insufficient funds";
/// - its gas and fees: "intrinsic gas too low", "exceeds block gas limit",
///   "gas limit", "underpriced", "fee cap", "base fee", "gas price",
///   "configured cap";
/// - its bytes: "invalid sender", "invalid signature", "chain id",
///   "transaction type not supported", "tx type not supported", "oversized
///   data", "initcode", "rlp", "invalid transaction";
/// - a revert the node found running it (code 3, "execution reverted");
/// - the request itself refused as malformed or unknown (codes -32601,
///   -32602).
///
/// Anything else (an internal error, a server error, a timeout, a gateway's
/// own failure) is unclear: the endpoint may have taken it.
fn judges_the_transaction(refusal: &alloy::transports::TransportError) -> bool {
    let alloy::rpc::json_rpc::RpcError::ErrorResp(payload) = refusal else {
        return false;
    };
    if matches!(payload.code, 3 | -32601 | -32602) {
        return true;
    }
    let message = payload.message.to_lowercase();
    [
        "nonce too low",
        "nonce too high",
        "invalid nonce",
        "nonce has already been used",
        "insufficient funds",
        "intrinsic gas too low",
        "exceeds block gas limit",
        "gas limit",
        "underpriced",
        "fee cap",
        "base fee",
        "gas price",
        "configured cap",
        "invalid sender",
        "invalid signature",
        "chain id",
        "transaction type not supported",
        "tx type not supported",
        "oversized data",
        "initcode",
        "rlp",
        "invalid transaction",
        "execution reverted",
    ]
    .iter()
    .any(|clear| message.contains(clear))
}

fn already_held(refusal: &alloy::transports::TransportError) -> bool {
    let alloy::rpc::json_rpc::RpcError::ErrorResp(payload) = refusal else {
        return false;
    };
    let message = payload.message.to_lowercase();
    message.contains("already known")
        || message.contains("alreadyknown")
        || message.contains("already imported")
        || message.starts_with("known transaction")
}

/// Wait for a sent transaction's receipt, and add what it cost to the key's
/// count when it arrives, unless another watch of the same transaction
/// already has; and raise the key's nonce floor past it.
async fn land(
    provider: DynProvider,
    hash: B256,
    spent: Arc<std::sync::Mutex<Spent>>,
    (nonces, from, nonce): (NonceFloor, Address, u64),
) -> Result<TransactionReceipt> {
    let receipt = receipt_by(
        &provider,
        hash,
        tokio::time::Instant::now() + LANDING_TIMEOUT,
    )
    .await?;
    // Landed, reverted or not: its nonce is used, and the next send of the
    // key's is never numbered at or below it.
    nonces.landed(from, nonce);
    if let Ok(mut spent) = spent.lock() {
        spent.count(
            hash,
            (receipt.gas_used as u128).saturating_mul(receipt.effective_gas_price),
            std::time::Instant::now(),
        );
    }
    Ok(receipt)
}

/// How many windows of one log read may be asked for at once. Their logs
/// are still handed back in block order. Four keeps one read well inside a
/// free endpoint's per-second limit (Base's own allows 25 requests a second)
/// while the rest of the services ask too, and covers the indexer's
/// 2,000-block page at the narrowest cap the testnets use (500 blocks) in
/// one round trip. A rate limit on any window still moves that window to
/// the next endpoint, as every read does.
pub const LOG_WINDOWS_IN_FLIGHT: usize = 4;

/// Consecutive block windows covering `from..=to`, each at most `cap`
/// blocks wide, each block in exactly one; the whole range at once without
/// a cap.
#[derive(Clone, Debug)]
struct LogWindows {
    next: Option<u64>,
    to: u64,
    cap: Option<u64>,
}

impl LogWindows {
    fn new(from: u64, to: u64, cap: Option<u64>) -> Self {
        Self {
            next: (from <= to).then_some(from),
            to,
            // A zero cap never passes the config; taken as one block if it
            // somehow did, rather than reading nothing.
            cap: cap.map(|cap| cap.max(1)),
        }
    }
}

impl Iterator for LogWindows {
    type Item = (u64, u64);

    fn next(&mut self) -> Option<(u64, u64)> {
        let from = self.next?;
        let to = match self.cap {
            Some(cap) => from.saturating_add(cap - 1).min(self.to),
            None => self.to,
        };
        self.next = if to >= self.to { None } else { Some(to + 1) };
        Some((from, to))
    }
}

/// The highest head each endpoint of a list said it had reached, during
/// one log read.
#[derive(Clone)]
struct Heads(Arc<std::sync::Mutex<Vec<Option<u64>>>>);

impl Heads {
    fn new(count: usize) -> Self {
        Self(Arc::new(std::sync::Mutex::new(vec![None; count])))
    }

    fn of(&self, at: usize) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(at)
            .copied()
            .flatten()
    }

    fn saw(&self, at: usize, head: u64) {
        if let Some(known) = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(at)
        {
            *known = Some(known.map_or(head, |known| known.max(head)));
        }
    }
}

/// The head, as the first endpoint of the list to answer gives it.
async fn logs_head(endpoints: &Endpoints, heads: &Heads) -> Result<u64> {
    let mut limits = RateLimits::default();
    loop {
        let mut last = None;
        let mut round = RateLimits::round();
        for at in endpoints.order() {
            match endpoints.each[at].provider.get_block_number().await {
                Ok(head) => {
                    heads.saw(at, head);
                    return Ok(head);
                }
                Err(error) => {
                    round.saw(&error);
                    last = Some(error);
                }
            }
        }
        if !limits.wait_after(round).await {
            return Err(last.map_or_else(|| anyhow!("no endpoint answered"), anyhow::Error::new));
        }
    }
}

/// Why one endpoint didn't give a window's logs.
enum Passed {
    /// It hasn't reached the window's end yet: it may later.
    Behind(anyhow::Error),
    /// It limited the rate of requests.
    Limited(anyhow::Error, Option<Duration>),
    /// It refused the range as too wide, and was not to read it in parts.
    TooWide(anyhow::Error),
    /// Anything else.
    Over(anyhow::Error),
}

/// The logs `filter` matches from `from` to `to`, read whole by an endpoint
/// that had reached `to` ([`Client::logs`]).
///
/// Endpoints of one list may cap a log query's width very differently, so
/// the window goes first to those not known to refuse it whole, and an
/// endpoint that refuses it as too wide passes it on while another may yet
/// take it whole. Only an endpoint known to refuse it, or the last that may
/// take it, reads it in parts: those known to refuse it last, the widest
/// first, each in parts as wide as the widest it has taken. Were it
/// instead halved at the first endpoint to refuse it, an endpoint that
/// takes a hundred blocks would read a window another takes whole in
/// dozens of queries, every window, for as long as it answered first.
async fn read_window(
    endpoints: &Endpoints,
    heads: &Heads,
    filter: &Filter,
    from: u64,
    to: u64,
) -> Result<Vec<Log>> {
    let mut last = None;
    let mut behind_tries = 0;
    let mut limits = RateLimits::default();
    let width = to - from + 1;
    // Once every endpoint that could have taken the window whole has
    // refused it as too wide, or was passed over, each reads it in parts.
    let mut in_parts = false;
    loop {
        let mut behind = false;
        let mut too_wide = false;
        let mut round = RateLimits::round();
        let mut order = endpoints.order();
        order.sort_by_key(|at| {
            let known = endpoints.each[*at].log_width();
            if known.may_take(width) {
                (false, std::cmp::Reverse(0))
            } else {
                (true, std::cmp::Reverse(known.widest_possible()))
            }
        });
        for at in order {
            let endpoint = &endpoints.each[at];
            let provider = &endpoint.provider;
            // Its head, asked again only when the last it gave falls short.
            let head = match heads.of(at).filter(|head| *head >= to) {
                Some(head) => head,
                None => match provider.get_block_number().await {
                    Ok(head) => {
                        heads.saw(at, head);
                        head
                    }
                    Err(error) => {
                        round.saw(&error);
                        last = Some(
                            anyhow::Error::new(error)
                                .context(format!("the head of log endpoint {}", at + 1)),
                        );
                        continue;
                    }
                },
            };
            if head < to {
                behind = true;
                round.other();
                last = Some(anyhow!(
                    "log endpoint {} has reached block {head}, not yet {to}",
                    at + 1
                ));
                continue;
            }
            let parts = in_parts
                || !endpoint.log_width().may_take(width)
                || !endpoints
                    .each
                    .iter()
                    .enumerate()
                    .any(|(other, each)| other != at && each.log_width().may_take(width));
            match read_range(endpoint, filter, from, to, parts).await {
                Ok(logs) => {
                    endpoints.answered(at);
                    return Ok(logs);
                }
                Err(Passed::TooWide(error)) => {
                    too_wide = true;
                    round.other();
                    last = Some(error);
                }
                Err(Passed::Behind(error)) => {
                    behind = true;
                    round.other();
                    last = Some(error);
                }
                Err(Passed::Limited(error, asked)) => {
                    round.limited = true;
                    round.retry_after = round.retry_after.max(asked);
                    last = Some(error);
                }
                Err(Passed::Over(error)) => {
                    round.other();
                    last = Some(error);
                }
            }
        }
        if too_wide && !in_parts {
            in_parts = true;
            continue;
        }
        if behind && behind_tries < LOGS_BEHIND_TRIES {
            behind_tries += 1;
            tokio::time::sleep(LOGS_BEHIND_PAUSE).await;
            continue;
        }
        if limits.wait_after(round).await {
            continue;
        }
        break;
    }
    Err(last
        .unwrap_or_else(|| anyhow!("no log endpoint answered"))
        .context(format!("reading logs from block {from} to {to}")))
}

/// The logs `filter` matches from `from` to `to` at one endpoint. With
/// `parts`, a range it refuses as too wide is halved, and one it is known to
/// refuse is not asked for but read in parts as wide as the widest it has
/// taken; without, a refusal as too wide is [`Passed::TooWide`]. What it
/// takes and refuses is kept ([`LogWidth`]).
async fn read_range(
    endpoint: &Endpoint,
    filter: &Filter,
    from: u64,
    to: u64,
    parts: bool,
) -> Result<Vec<Log>, Passed> {
    let provider = &endpoint.provider;
    let mut found = Vec::new();
    // Popped last-in first-out: the earliest is pushed last.
    let mut pending = vec![(from, to)];
    while let Some((from, to)) = pending.pop() {
        let width = to - from + 1;
        let known = endpoint.log_width();
        if parts && to > from && !known.may_take(width) {
            let part = known.part(width);
            let mut parts_of = Vec::new();
            let mut start = from;
            loop {
                let end = start.saturating_add(part - 1).min(to);
                parts_of.push((start, end));
                if end == to {
                    break;
                }
                start = end + 1;
            }
            pending.extend(parts_of.into_iter().rev());
            continue;
        }
        let part = filter.clone().from_block(from).to_block(to);
        match provider.get_logs(&part).await {
            Ok(logs) => {
                endpoint.saw_log_width(to - from + 1, true);
                found.extend(logs);
            }
            Err(error) => {
                let text = error.to_string();
                // A rate limit, or missing logs, is the endpoint's trouble,
                // whatever else its words say.
                let trouble = rate_limited(&error) || text.contains(MISSING_STATE);
                if !trouble && to > from && refused_as_too_wide(&text) {
                    endpoint.saw_log_width(to - from + 1, false);
                    if !parts {
                        return Err(Passed::TooWide(
                            anyhow::Error::new(error)
                                .context(format!("reading logs from block {from} to {to}")),
                        ));
                    }
                    let middle = from + (to - from) / 2;
                    // Popped last-in first-out: the earlier half first.
                    pending.push((middle + 1, to));
                    pending.push((from, middle));
                    continue;
                }
                let limited = rate_limited(&error);
                let asked = retry_after(&error);
                let error = anyhow::Error::new(error)
                    .context(format!("reading logs from block {from} to {to}"));
                return Err(if limited {
                    Passed::Limited(error, asked)
                } else if !trouble && refused_as_not_there_yet(&text) {
                    Passed::Behind(error)
                } else {
                    Passed::Over(error)
                });
            }
        }
    }
    Ok(found)
}

/// How many times a window no endpoint has reached yet is asked again.
const LOGS_BEHIND_TRIES: u32 = 4;

/// How long to wait before asking again for a window no endpoint had
/// reached: a few of the testnets' blocks.
const LOGS_BEHIND_PAUSE: Duration = Duration::from_millis(1_500);

/// Whether a provider refused a log range for its width, in any of the ways
/// providers say so.
///
/// A proxy over several upstreams may lead with one upstream's complaint
/// that it hasn't the block and name another's that the range is too wide
/// only among the causes: that is a refusal of the width, since the proxy
/// has the block (a window goes only to an endpoint whose head has reached
/// it) and one of its upstreams would take a narrower range.
fn refused_as_too_wide(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    if ["too large complaint", "errendpointrequesttoolarge"]
        .iter()
        .any(|phrase| error.contains(phrase))
        && !is_rate_limit(0, &error)
    {
        return true;
    }
    [
        "range",
        "too many",
        "too large",
        "limit exceeded",
        "exceeds the limit",
        "response size",
        "query returned more than",
        "block span",
        "blocks are not supported",
    ]
    .iter()
    .any(|phrase| error.contains(phrase))
        && !refused_as_not_there_yet(&error)
        && !is_rate_limit(0, &error)
}

/// How many times a pinned read no endpoint has reached the block of is
/// asked again: a read pinned to the head's block one endpoint just gave.
const PINNED_BEHIND_TRIES: u32 = 3;

/// How long to wait before asking again for a pinned read at a block no
/// endpoint had reached: about one of the slower testnets' blocks.
const PINNED_BEHIND_PAUSE: Duration = Duration::from_millis(1_000);

/// Whether a JSON-RPC refusal of a pinned read is the endpoint saying it
/// hasn't reached the block yet. A revert, whatever its reason says, is the
/// chain's answer, never this.
fn not_reached_yet(code: i64, message: &str) -> bool {
    code != 3
        && !message.to_ascii_lowercase().contains("revert")
        && refused_as_not_there_yet(message)
}

/// Whether a node refused a log range because it hasn't reached its end yet.
fn refused_as_not_there_yet(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "block not found",
        "not available on the upstream",
        "unknown block",
        "header not found",
        "beyond current head",
        "after last accepted block",
    ]
    .iter()
    .any(|phrase| error.contains(phrase))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbered_log_observations_match_direct_filters_only_within_their_coverage() {
        use alloy::eips::BlockNumberOrTag;
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let unknown = Address::repeat_byte(3);
        let topic = B256::repeat_byte(7);
        let logs: Vec<Log> = (10..=12)
            .flat_map(|block| {
                [a, b].map(|address| Log {
                    inner: alloy::primitives::Log::new_unchecked(
                        address,
                        vec![topic],
                        Bytes::new(),
                    ),
                    block_number: Some(block),
                    block_hash: Some(B256::repeat_byte(block as u8)),
                    ..Default::default()
                })
            })
            .collect();
        let snapshot = LogSnapshot {
            from: 10,
            to: 12,
            addresses: vec![a, b],
            logs: logs.clone(),
        };
        for from in 10..=12 {
            for to in from..=12 {
                for addresses in [vec![a], vec![b], vec![a, b]] {
                    for topics in [vec![topic], vec![B256::ZERO], vec![]] {
                        let filter = Filter::new()
                            .address(addresses.clone())
                            .from_block(from)
                            .to_block(to)
                            .event_signature(topics);
                        let expected: Vec<_> = logs
                            .iter()
                            .filter(|log| filter.rpc_matches(log))
                            .cloned()
                            .collect();
                        assert_eq!(snapshot.read(&filter).unwrap(), expected);
                    }
                }
            }
        }
        for filter in [
            Filter::new().address(a).from_block(9).to_block(12),
            Filter::new().address(a).from_block(10).to_block(13),
            Filter::new().address(a).from_block(12).to_block(10),
            Filter::new()
                .address(vec![a, unknown])
                .from_block(10)
                .to_block(12),
            Filter::new().from_block(10).to_block(12),
            Filter::new()
                .address(a)
                .from_block(10)
                .to_block(BlockNumberOrTag::Latest),
            Filter::new().address(a).at_block_hash(B256::ZERO),
        ] {
            assert!(snapshot.read(&filter).is_none(), "{filter:?}");
        }
    }

    use super::*;

    #[tokio::test]
    async fn protocol_refusals_retry_identical_signed_bytes_and_count_each_attempt() {
        use alloy::rpc::json_rpc::{Request, SerializedRequest};
        use tower::Service;
        for (case, refusals, reason, expected, succeeds) in [
            (0, 1, h2::Reason::REFUSED_STREAM, 2, true),
            (1, 100, h2::Reason::REFUSED_STREAM, 3, false),
            (2, 1, h2::Reason::CANCEL, 1, false),
        ] {
            let (url, bodies, server) = crate::http_retry::refusing_server(refusals, reason).await;
            let http = alloy::transports::http::reqwest::Client::builder()
                .http2_prior_knowledge()
                .retry(alloy::transports::http::reqwest::retry::never())
                .redirect(alloy::transports::http::reqwest::redirect::Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap();
            let chain = 9_876_544_000 + case;
            let mut transport = endpoint_transport(&url, "rpc", &http, None, Some(chain)).unwrap();
            let request: SerializedRequest =
                Request::new("eth_sendRawTransaction", 1u64.into(), ("0x1234",))
                    .try_into()
                    .unwrap();
            let answer = transport.call(request.into()).await;
            assert_eq!(answer.is_ok(), succeeds, "case {case}: {answer:?}");
            let bodies = bodies.lock().unwrap();
            assert_eq!(bodies.len(), expected);
            assert!(bodies.iter().all(|body| body == &bodies[0]));
            let snapshot = crate::rpc_counts::snapshot();
            let row = snapshot
                .counters
                .iter()
                .find(|row| {
                    row.chain == format!("evm:{chain}") && row.method == "eth_sendRawTransaction"
                })
                .unwrap();
            assert_eq!(row.requests, expected as u64);
            server.abort();
        }
    }

    #[tokio::test]
    async fn a_single_endpoint_counts_its_chain_and_does_not_follow_a_redirect() {
        use alloy::rpc::json_rpc::{Request, SerializedRequest};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tower::Service;

        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let destination = format!("http://{}", target.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 8192];
            let received = socket.read(&mut buffer).await.unwrap();
            assert!(received > 0);
            socket.write_all(format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ).as_bytes()).await.unwrap();
        });
        let chain = 9_876_543_210;
        let mut transport =
            endpoint_transport(&url, "rpc", &http_client().unwrap(), None, Some(chain)).unwrap();
        let packet = || {
            let request: SerializedRequest = Request::new("eth_call", 1u64.into(), ())
                .try_into()
                .unwrap();
            request.into()
        };
        // Constructing a future that is never polled dispatches nothing.
        drop(transport.call(packet()));
        assert!(transport.call(packet()).await.is_err());
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), target.accept())
                .await
                .is_err()
        );
        let snapshot = crate::rpc_counts::snapshot();
        let row = snapshot
            .counters
            .iter()
            .find(|row| row.chain == format!("evm:{chain}") && row.method == "eth_call")
            .unwrap();
        assert_eq!(row.requests, 1);
    }

    #[test]
    fn only_a_send_that_was_broadcast_may_still_land() {
        let lost = anyhow::Error::new(NeverLanded).context("approveKey");
        assert!(unanswered(&lost) && may_still_land(&lost));
        // Unanswered all the same, but nothing was sent.
        let unsent = NeverSent::wrap(anyhow::Error::new(NeverAcknowledged)).context("approveKey");
        assert!(unanswered(&unsent));
        assert!(!may_still_land(&unsent));
        // A refusal is an answer either way.
        assert!(!may_still_land(&anyhow::Error::new(Reverted)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_unanswered_after_an_earlier_note_lapsed_still_holds_the_next_back() {
        let key = Address::repeat_byte(1);
        let lost = || anyhow::Error::new(NeverLanded).context("bless");
        let unanswered = Unanswered::default();

        // A first send goes unanswered: it may land for the landing window.
        unanswered.failed(key, &lost());
        assert!(unanswered.may_still_land(key));

        // The window passes: that one no longer may.
        tokio::time::advance(LANDING_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert!(!unanswered.may_still_land(key));

        // The next goes unanswered too, and may land just as the first might
        // have: its note replaces the lapsed one.
        unanswered.failed(key, &lost());
        assert!(unanswered.may_still_land(key));
    }

    #[tokio::test(start_paused = true)]
    async fn a_note_still_running_is_kept_when_its_send_is_seen_to_fail() {
        let key = Address::repeat_byte(1);
        let unanswered = Unanswered::default();
        // Noted at its broadcast, for as long as its receipt is watched for.
        let _watching = Noted::start(unanswered.clone(), key);
        // Its caller sees it go unanswered: the note it already has stands,
        // rather than being cut back to the caller's own window.
        unanswered.failed(key, &anyhow::Error::new(NeverLanded).context("bless"));
        tokio::time::advance(LANDING_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert!(unanswered.may_still_land(key));
    }

    #[test]
    fn every_part_of_a_paid_url_that_could_be_a_key_is_scrubbed() {
        // Made-up keys, built here so they don't read as real ones.
        let path_key = "abcdef01".repeat(2);
        let query_key = "fedcba98".repeat(2);
        let user = "user0123456789";
        let configured = format!("https://{user}@eth.example.com/v2/{path_key}?apikey={query_key}");
        let keep = KeepUrlOut::new(&configured, &configured.parse().unwrap());
        for leak in [
            format!("Cannot POST /v2/{path_key}"),
            format!("unknown key {path_key}"),
            format!("bad apikey={query_key}"),
            format!("bad key {query_key}"),
            format!(
                "error sending request for url (https://eth.example.com/v2/{path_key}?apikey={query_key})"
            ),
            format!("no account {user} at this gateway"),
        ] {
            let scrubbed = keep.scrub(&leak);
            assert!(
                !scrubbed.contains(&path_key)
                    && !scrubbed.contains(&query_key)
                    && !scrubbed.contains(user),
                "{scrubbed}"
            );
        }
        // Short words in the path are not taken for keys.
        assert_eq!(keep.scrub("v2 rpc"), "v2 rpc");
    }

    #[test]
    fn a_transaction_s_gas_is_counted_once_and_its_hash_forgotten_after_the_window() {
        let start = std::time::Instant::now();
        let mut spent = Spent::default();
        let one = B256::repeat_byte(1);
        spent.count(one, 100, start);
        spent.count(one, 100, start + Duration::from_secs(5));
        assert_eq!(spent.total, 100);
        // Remembered from the last watch that found it.
        spent.count(
            B256::repeat_byte(2),
            7,
            start + Duration::from_secs(5) + Spent::REMEMBERED_FOR,
        );
        assert_eq!(spent.total, 107);
        assert!(!spent.counted.contains_key(&one), "kept past the window");
    }

    #[test]
    fn only_a_node_that_holds_the_transaction_is_taken_as_holding_it() {
        let refusal = |message: &str| {
            alloy::rpc::json_rpc::RpcError::ErrorResp(alloy::rpc::json_rpc::ErrorPayload {
                code: -32000,
                message: message.to_string().into(),
                data: None,
            })
        };
        for held in [
            "already known",
            "AlreadyKnown",
            "known transaction: 0xabc",
            "Transaction with the same hash was already imported.",
        ] {
            assert!(already_held(&refusal(held)), "{held}");
        }
        for refused in [
            "nonce too low",
            "insufficient funds for gas * price + value",
            "unknown transaction",
            "replacement transaction underpriced",
        ] {
            assert!(!already_held(&refusal(refused)), "{refused}");
        }
    }

    #[test]
    fn only_a_judgement_of_the_transaction_itself_is_a_refusal_of_a_send() {
        let refusal = |code: i64, message: &str| {
            alloy::rpc::json_rpc::RpcError::ErrorResp(alloy::rpc::json_rpc::ErrorPayload {
                code,
                message: message.to_string().into(),
                data: None,
            })
        };
        for clear in [
            (-32000, "nonce too low"),
            (-32000, "insufficient funds for gas * price + value"),
            (-32000, "replacement transaction underpriced"),
            (-32000, "max fee per gas less than block base fee"),
            (-32000, "intrinsic gas too low"),
            (-32000, "invalid sender"),
            (3, "execution reverted: NotBlessed()"),
            (-32602, "invalid params"),
        ] {
            assert!(
                judges_the_transaction(&refusal(clear.0, clear.1)),
                "{clear:?}"
            );
        }
        for unclear in [
            (-32603, "Internal error"),
            (-32000, "upstream request timeout"),
            (
                30,
                "Request timeout on the free tier, please upgrade your tier to the paid one",
            ),
            (-32000, "server error"),
            (-32002, "resource unavailable"),
        ] {
            assert!(
                !judges_the_transaction(&refusal(unclear.0, unclear.1)),
                "{unclear:?}"
            );
        }
    }

    #[test]
    fn a_mnemonic_that_fails_its_checksum_is_not_quoted_in_the_error() {
        // A development chain's public mnemonic with its last word changed
        // to another real word: the checksum no longer holds.
        let phrase = "test test test test test test test test test test test zoo";
        let Err(error) = keys_from_mnemonic(phrase, 1) else {
            panic!("the checksum should fail");
        };
        let text = format!("{error:#} / {error:?}");
        assert!(
            !text.contains("test test"),
            "the error quotes the mnemonic: {text}"
        );
    }

    #[test]
    fn a_mnemonic_derives_the_accounts_a_development_chain_would() {
        // Anvil's published mnemonic, and the first two accounts it hands out.
        let keys = keys_from_mnemonic(
            "test test test test test test test test test test test junk",
            2,
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", keys[0].address()),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
        assert_eq!(
            format!("{:?}", keys[1].address()),
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
        );
    }

    #[test]
    fn a_range_refused_for_its_width_is_told_apart_from_one_not_reached_yet() {
        for wide in [
            "error code 35: ranges over 10000 blocks are not supported on free plan",
            "query returned more than 10000 results",
            "eth_getLogs block range too large, max is 2000",
            "error code -32005: limit exceeded",
            "block range is too wide",
        ] {
            assert!(refused_as_too_wide(wide), "{wide}");
            assert!(!refused_as_not_there_yet(wide), "{wide}");
        }
        for behind in [
            "error code -32603: block not found, because requested block (toBlock 66130301) is not available on the upstream node",
            "header not found",
            "unknown block",
        ] {
            assert!(refused_as_not_there_yet(behind), "{behind}");
            assert!(!refused_as_too_wide(behind), "{behind}");
        }
        assert!(!refused_as_too_wide("execution reverted"));
        assert!(!refused_as_not_there_yet("rate limited"));
    }

    #[test]
    fn a_proxy_naming_an_upstream_s_too_wide_complaint_refused_the_range_for_its_width() {
        let proxied = r#"server returned an error response: error code -32603: block not found, because requested block (toBlock 66301299) is not available on the upstream node, data: {"code":"ErrUpstreamsExhausted","message":"all upstream attempts failed (1 upstream missing data, 1 upstream too large complaints)","cause":[{"code":"ErrEndpointMissingData"},{"code":"ErrEndpointRequestTooLarge"}]}"#;
        assert!(refused_as_too_wide(proxied));
    }

    /// Every block of `from..=to` in exactly one window, in order, none
    /// wider than the cap.
    #[test]
    fn a_rate_limited_read_waits_longer_each_time_and_honours_a_retry_after_within_a_bound() {
        assert_eq!(rate_limit_pause(0, None), Duration::from_millis(500));
        assert_eq!(rate_limit_pause(1, None), Duration::from_secs(1));
        assert_eq!(rate_limit_pause(2, None), Duration::from_secs(2));
        assert_eq!(
            rate_limit_pause(0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            rate_limit_pause(2, Some(Duration::from_secs(1))),
            Duration::from_secs(2)
        );
        assert_eq!(
            rate_limit_pause(0, Some(Duration::from_secs(600))),
            RATE_LIMIT_PAUSE_MOST
        );
        assert_eq!(rate_limit_pause(30, None), RATE_LIMIT_PAUSE_MOST);
    }

    #[test]
    fn only_reads_are_asked_again_for_a_rate_limit() {
        use alloy::rpc::json_rpc::{Request, RequestPacket, SerializedRequest};
        let packet = |method: &'static str| -> RequestPacket {
            let request: SerializedRequest =
                Request::new(method, 1u64.into(), ()).try_into().unwrap();
            request.into()
        };
        assert!(retried_when_rate_limited(&packet("eth_call")));
        assert!(retried_when_rate_limited(&packet("eth_blockNumber")));
        assert!(!retried_when_rate_limited(&packet(
            "eth_sendRawTransaction"
        )));
        assert!(!retried_when_rate_limited(&packet(
            "eth_sendRawTransactionSync"
        )));
        assert!(!retried_when_rate_limited(&packet("eth_signTypedData_v4")));
        assert!(!retried_when_rate_limited(&packet("debug_traceCall")));
    }

    #[test]
    fn a_block_not_reached_yet_is_told_from_a_revert() {
        for behind in [
            "header not found",
            "unknown block",
            "block not found",
            "Block 0x3e8 not found... unknown block",
        ] {
            assert!(not_reached_yet(-32000, behind), "{behind}");
        }
        assert!(!not_reached_yet(3, "header not found"));
        assert!(!not_reached_yet(
            -32000,
            "execution reverted: unknown block"
        ));
        assert!(!not_reached_yet(-32000, "insufficient funds"));
    }

    fn covers_once(from: u64, to: u64, cap: Option<u64>) -> Vec<(u64, u64)> {
        let windows: Vec<_> = LogWindows::new(from, to, cap).collect();
        let mut next = from;
        for &(start, end) in &windows {
            assert_eq!(start, next, "a gap or an overlap: {windows:?}");
            assert!(start <= end, "{windows:?}");
            if let Some(cap) = cap {
                assert!(end - start < cap, "wider than {cap}: {windows:?}");
            }
            next = end.wrapping_add(1);
        }
        assert_eq!(
            next,
            to.wrapping_add(1),
            "the range's end was not reached: {windows:?}"
        );
        windows
    }

    #[test]
    fn log_windows_cover_a_range_once_with_no_gap_or_overlap() {
        // Smaller than the cap: one window.
        assert_eq!(covers_once(100, 150, Some(500)), vec![(100, 150)]);
        // An exact multiple.
        assert_eq!(
            covers_once(1, 30, Some(10)),
            vec![(1, 10), (11, 20), (21, 30)]
        );
        // One block.
        assert_eq!(covers_once(7, 7, Some(10)), vec![(7, 7)]);
        assert_eq!(covers_once(7, 7, Some(1)), vec![(7, 7)]);
        // A remainder.
        assert_eq!(
            covers_once(0, 24, Some(10)),
            vec![(0, 9), (10, 19), (20, 24)]
        );
        // A cap of one block.
        assert_eq!(covers_once(5, 8, Some(1)).len(), 4);
        // No cap: the whole range at once.
        assert_eq!(covers_once(0, 1_000_000, None), vec![(0, 1_000_000)]);
        // The top of the number range doesn't wrap.
        covers_once(u64::MAX - 25, u64::MAX, Some(10));
        // A backwards range has none.
        assert_eq!(LogWindows::new(9, 8, Some(10)).count(), 0);
    }

    #[test]
    fn missing_history_is_told_from_a_revert_and_from_a_refusal_of_the_request() {
        // The wordings the testnets' public endpoints give.
        for pruned in [
            "historical state 0c1857f57b3ecc4ebf8d1889d12e66d39d324e81518720fb83598755297c268b is not available",
            "missing trie node 0c1857 (path ) state 0x0c1857 is not available, not found",
            "state at block #45748311 is pruned",
            "Unknown state. First available state is 1",
            "no available upstreams to process a request. Cause - x - Upstream lower height 34112653 of type STATE is greater than 4000000",
            "pruned history unavailable",
            "no state found for block 0x1e8480",
        ] {
            assert!(is_missing_history(-32000, pruned), "{pruned}");
        }
        for answer in [
            "execution reverted",
            "execution reverted: state is pruned",
            "execution reverted: no state found for block",
            "invalid argument 0: hex string without 0x prefix",
            "nonce too low",
            "block range too large",
        ] {
            assert!(!is_missing_history(-32000, answer), "{answer}");
        }
        assert!(!is_missing_history(3, "historical state is not available"));
    }

    #[test]
    fn a_send_is_never_numbered_below_the_last_one_seen_to_land() {
        let floor = NonceFloor::default();
        let (one, two) = (Address::repeat_byte(1), Address::repeat_byte(2));
        // Nothing seen yet: the node's count.
        assert_eq!(floor.number(one, 4), 4);
        floor.landed(one, 4);
        // A node a block behind still counts four.
        assert_eq!(floor.number(one, 4), 5);
        // One ahead (a send still pending) is taken as it is.
        assert_eq!(floor.number(one, 7), 7);
        // Another key has a floor of its own.
        assert_eq!(floor.number(two, 0), 0);
        // A send that lands late never lowers the floor.
        floor.landed(one, 9);
        floor.landed(one, 6);
        assert_eq!(floor.number(one, 3), 10);
    }

    #[test]
    fn a_rate_limit_is_told_from_a_refusal_of_the_request_itself() {
        assert!(is_rate_limit(-32005, "rate limited"));
        assert!(is_rate_limit(-32000, "Too Many Requests"));
        assert!(is_rate_limit(429, "slow down"));
        assert!(!is_rate_limit(
            -32005,
            "query returned more than 10000 results"
        ));
        assert!(!is_rate_limit(3, "execution reverted"));
        assert!(!is_rate_limit(-32000, "nonce too low"));
        // A revert whose reason reads like a rate limit is the chain's
        // answer.
        assert!(!is_rate_limit(3, "Too many requests"));
        assert!(!is_rate_limit(-32000, "execution reverted: rate limit"));
        assert!(!is_rate_limit(429, "execution reverted"));
    }

    #[test]
    fn a_rate_limit_on_a_log_query_is_never_taken_as_too_wide() {
        for limited in [
            "HTTP error 429 with body: Too Many Requests",
            "too many requests, request limit exceeded for this range",
            "rate limit exceeded",
        ] {
            assert!(!refused_as_too_wide(limited), "{limited}");
        }
        assert!(refused_as_too_wide("block range too large"));
    }

    #[test]
    fn a_rate_limit_however_it_arrives_is_the_node_s_trouble() {
        let wrapped = anyhow::anyhow!("25/second request limit reached")
            .context("reading isBlessedAs")
            .context("the registry would not take the registration");
        assert!(
            node_trouble(&wrapped),
            "a public endpoint's per-second limit"
        );
        let limited = anyhow::anyhow!("HTTP error 429 with body: Too Many Requests")
            .context("the registry would not take the registration");
        assert!(node_trouble(&limited));
        let refused =
            anyhow::Error::new(Reverted).context("the registry would not take the registration");
        assert!(!node_trouble(&refused));
    }
}
