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

/// A connection to the chain, and the addresses of everything on it.
#[derive(Clone)]
pub struct Client {
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
        }
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
        let answer = self.inner.call(request);
        let keep = self.keep.clone();
        Box::pin(async move {
            answer
                .await
                .map(|answer| keep.scrub_answer(answer))
                .map_err(|error| keep.scrub_error(error))
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
/// worded), which also falls in the ranking.
fn rpc_client(rpc: &str) -> Result<alloy::rpc::client::RpcClient> {
    let http = alloy::transports::http::reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("making the HTTP client")?;
    let urls: Vec<&str> = rpc.split_whitespace().collect();
    let parse = |url: &str| -> Result<alloy::transports::http::reqwest::Url> {
        url.parse()
            .map_err(|_| anyhow!("the chain's rpc in the chain config is not a URL"))
    };
    if urls.len() <= 1 {
        let url = parse(rpc.trim())?;
        return Ok(alloy::rpc::client::ClientBuilder::default()
            .layer(KeepUrlOut::new(rpc.trim(), &url))
            .http_with_client(http, url));
    }
    let mut transports = Vec::with_capacity(urls.len());
    for configured in urls {
        let url = parse(configured)?;
        let keep = KeepUrlOut::new(configured, &url);
        let inner = alloy::transports::http::Http::with_client(http.clone(), url);
        transports.push(tower::Layer::layer(&keep, RateLimitIsAFailure { inner }));
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
    Ok(alloy::rpc::client::ClientBuilder::default().transport(fallback, false))
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

/// An answer that is a rate limit, whether as an HTTP status or inside a
/// JSON-RPC error, taken as the endpoint failing, so a ranking of endpoints
/// moves away from it.
#[derive(Clone)]
struct RateLimitIsAFailure<S> {
    inner: S,
}

impl<S> tower::Service<alloy::rpc::json_rpc::RequestPacket> for RateLimitIsAFailure<S>
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
        let answer = self.inner.call(request);
        Box::pin(async move {
            let answer = answer.await?;
            let limited = answer
                .responses()
                .iter()
                .filter_map(|response| response.payload.as_error())
                .any(|error| is_rate_limit(error.code, &error.message));
            if limited {
                return Err(alloy::transports::TransportErrorKind::custom_str(
                    "the endpoint is limiting the rate of requests",
                ));
            }
            Ok(answer)
        })
    }
}

/// Whether a JSON-RPC error is an endpoint limiting the rate of requests.
fn is_rate_limit(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    code == 429
        || message.contains("rate limit")
        || message.contains("too many requests")
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
    /// Connect, waiting for the chain to answer: `task dev` starts everything
    /// at once, so a service usually arrives before the chain does.
    pub async fn connect(config: &ChainConfig, signing: Signing) -> Result<Self> {
        let rpc_url = config.rpc.as_str();
        let rpc = rpc_client(rpc_url)?;
        // The chain id is asked for first, on a provider that signs nothing,
        // so a provider that signs is made for that id alone.
        let asking = ProviderBuilder::default()
            .connect_client(rpc.clone())
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

        let provider = match &signing {
            Signing::Keys(keys) if !keys.is_empty() => {
                let mut wallet = EthereumWallet::from(keys[0].clone());
                for key in &keys[1..] {
                    wallet.register_signer(key.clone());
                }
                signing_builder(chain_id)
                    .wallet(wallet)
                    .connect_client(rpc)
                    .erased()
            }
            // The node holds unlocked accounts' keys and numbers their
            // transactions itself.
            _ => asking,
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
            Signing::ReadOnly => (Vec::new(), None),
        };
        Ok(Self {
            provider,
            rpc_url: rpc_url.to_string(),
            chain_id,
            accounts,
            keys,
            turns: Default::default(),
        })
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

    /// The account that plays a part on this chain.
    pub fn account(&self, role: Role) -> Result<Address> {
        self.accounts
            .get(role.account_index())
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "no account plays the {} role here: on a testnet, set SHONIN_TESTNET_EVM_MNEMONIC; on mainnet, clients only read",
                    role.label()
                )
            })
    }

    pub fn provider(&self) -> &DynProvider {
        &self.provider
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
    pub async fn read_at<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: alloy::eips::BlockId,
    ) -> Result<C::Return> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(call.abi_encode());
        let returned = self
            .provider
            .call(request)
            .block(block)
            .await
            .with_context(|| format!("reading {} at block {block:?}", C::SIGNATURE))?;
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// When a past block was mined, in the chain's own time.
    pub async fn block_timestamp_at(&self, block: u64) -> Result<u64> {
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::number(block))
            .await
            .context("a past block")?
            .ok_or_else(|| anyhow!("no such block"))?;
        Ok(block.header.timestamp)
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
        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(data);
        let receipt = self
            .sent(request, RECEIPT_TIMEOUT, "sending", "a transaction", |_| {})
            .await?;
        if !receipt.status() {
            return Err(anyhow::Error::new(Reverted).context("the transaction reverted"));
        }
        Ok(receipt)
    }

    /// Deploy a contract from its bytecode and constructor arguments. A
    /// creation the chain reverted is an error: its receipt still names the
    /// address the contract would have had, with nothing there.
    pub async fn deploy(&self, from: Address, bytecode: Vec<u8>) -> Result<Address> {
        let request = TransactionRequest::default()
            .with_from(from)
            .with_deploy_code(bytecode);
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
        let pending = self
            .provider
            .send_transaction(request)
            .await
            .with_context(|| sending.to_string())?;
        let hash = *pending.tx_hash();
        sent_as(hash);
        receipt_within(&self.provider, hash, within, what).await
    }

    /// Have the node sign a digest as `who`, the way a wallet would: the
    /// signature a contract recovers with `toEthSignedMessageHash`.
    pub async fn sign(&self, who: Address, digest: B256) -> Result<Bytes> {
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

    /// The logs a filter matches, through whatever the provider allows.
    ///
    /// Public providers refuse ranges wider than they like, each in its own
    /// words, so a numbered range one refuses as too wide is read in halves
    /// (and those in halves, as far as one block). Several nodes often
    /// answer behind one name, and one behind the others may not have the
    /// range's newest block yet, so a range it says it doesn't have is asked
    /// again a few times, a second or two apart, before the error is
    /// returned. A filter with no numbered range is asked once.
    pub async fn logs(&self, filter: &Filter) -> Result<Vec<Log>> {
        let range = filter.get_from_block().zip(filter.get_to_block());
        let Some((from, to)) = range.filter(|(from, to)| from <= to) else {
            return self.provider.get_logs(filter).await.context("reading logs");
        };
        let mut found = Vec::new();
        let mut pending = vec![(from, to)];
        while let Some((from, to)) = pending.pop() {
            let part = filter.clone().from_block(from).to_block(to);
            let mut tries = 0;
            loop {
                match self.provider.get_logs(&part).await {
                    Ok(logs) => {
                        found.extend(logs);
                        break;
                    }
                    Err(error) => {
                        let text = error.to_string();
                        if to > from && refused_as_too_wide(&text) {
                            let middle = from + (to - from) / 2;
                            // Popped last-in first-out: the earlier half first.
                            pending.push((middle + 1, to));
                            pending.push((from, middle));
                            break;
                        }
                        if refused_as_not_there_yet(&text) && tries < LOGS_BEHIND_TRIES {
                            tries += 1;
                            tokio::time::sleep(Duration::from_millis(1_500)).await;
                            continue;
                        }
                        return Err(anyhow::Error::new(error)
                            .context(format!("reading logs from block {from} to {to}")));
                    }
                }
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
        let block = self
            .provider
            .get_block(alloy::eips::BlockId::latest())
            .await
            .context("the head block")?
            .ok_or_else(|| anyhow!("the chain has no blocks yet"))?;
        Ok((block.header.number, block.header.timestamp))
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
            .connect_client(rpc_client(&self.rpc_url)?);
        Ok(KeyClient {
            provider: signing.clone().erased(),
            signing,
            signer,
            address,
            sending: Default::default(),
            spent: Default::default(),
            noting: None,
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
/// lands its gas is counted all the same. Only a refusal (the node answered
/// and said no) means it was not taken. Everything up to the receipt runs
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
    signature: &'static str,
) -> Result<TransactionReceipt> {
    use alloy::eips::eip2718::Encodable2718;
    use alloy::rpc::json_rpc::RpcError;

    // Nothing has been sent while this runs: only the node's reads are
    // asked for. So whatever fails here, nothing can land.
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
    let reply = tokio::time::timeout_at(deadline, provider.send_raw_transaction(&raw)).await;
    if let Ok(Err(refusal @ RpcError::ErrorResp(_))) = reply
        && !already_held(&refusal)
    {
        drop(turn);
        return Err(anyhow::Error::new(refusal).context(format!("sending {signature}")));
    }
    // Taken, already held, or no reply came back: whichever, it may land.
    let landing = land(provider, hash, spent);
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
/// already has.
async fn land(
    provider: DynProvider,
    hash: B256,
    spent: Arc<std::sync::Mutex<Spent>>,
) -> Result<TransactionReceipt> {
    let receipt = receipt_by(
        &provider,
        hash,
        tokio::time::Instant::now() + LANDING_TIMEOUT,
    )
    .await?;
    if let Ok(mut spent) = spent.lock() {
        spent.count(
            hash,
            (receipt.gas_used as u128).saturating_mul(receipt.effective_gas_price),
            std::time::Instant::now(),
        );
    }
    Ok(receipt)
}

/// How many times a range a node says it hasn't reached yet is asked again.
const LOGS_BEHIND_TRIES: u32 = 4;

/// Whether a provider refused a log range for its width, in any of the ways
/// providers say so.
fn refused_as_too_wide(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
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
    use super::*;

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
    }
}
