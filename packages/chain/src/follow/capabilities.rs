//! What one endpoint can do, found out by asking it, and remembered.
//!
//! Endpoints differ: some refuse state overrides, some ignore a block pin
//! (answering at their head), some run the EVM with another chain's block
//! numbers (Arbitrum Orbit), some cap a call's gas or answer's size without
//! saying so. A missing capability costs requests, never correctness: every
//! path is checked against the block it reports, and a capability is used
//! only once it has been shown.

use std::time::Duration;

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::state::AccountOverride;
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, anyhow, bail};

use super::Header;
use super::multicall::IArbSys;
use super::view_reads::{self, Call};

/// Where the block number and block hashes inside a call come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSource {
    /// `block.number` and `blockhash`.
    Plain,
    /// ArbSys at `0x64` (`arbBlockNumber`, `arbBlockHash`): on an Arbitrum
    /// Orbit chain `block.number` is the parent chain's and `blockhash` of
    /// the chain's own blocks is zero.
    ArbSys,
}

/// The address of the ArbSys precompile.
pub const ARB_SYS: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64,
]);

/// How often the same pin check is repeated before an endpoint is trusted
/// with reads that cannot check their own block: an endpoint over several
/// upstreams may honour a pin on some and not others.
pub const PIN_CHECKS: u64 = 6;

/// How long one capability check may take.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long remembered capabilities are kept before they are asked again.
pub const RECHECK_AFTER: Duration = Duration::from_secs(3_600);

/// How long after a failure an endpoint's capabilities are asked again
/// (not sooner, so a failing endpoint is not asked on every observation).
pub const RECHECK_AFTER_FAILURE: Duration = Duration::from_secs(60);

/// What one endpoint has been shown to do. `None` is not known yet.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// Whether it runs the helper put in place by a state override, with a
    /// block source whose number and parent hash equal its own header's.
    pub overrides: Option<bool>,
    /// The block source that matched its headers.
    pub source: Option<BlockSource>,
    /// Whether it answers a read pinned to a block hash as of that block,
    /// every time it was asked ([`PIN_CHECKS`]), and refuses an unknown hash.
    pub hash_pin: Option<bool>,
    /// Whether Multicall3 is at its canonical address with its known code.
    pub multicall3: Option<bool>,
    /// Whether a read pinned some way into the past is answered as of that
    /// block, and how far back it was asked.
    pub past_blocks: Option<(bool, u64)>,
    /// The gas the helper found available at entry, last time.
    pub gas_at_entry: Option<u64>,
    /// The most reads one call ran before its gas ran low.
    pub max_reads: Option<usize>,
    /// The largest answer it refused or failed to give, halved.
    pub output_bytes: Option<u64>,
    /// Whether an observation here needed reads of their own, so whether
    /// hash pins are honoured is asked before the next one.
    pub wants_hash_pin: bool,
    pub(super) checked_at: Option<tokio::time::Instant>,
    pub(super) failed_at: Option<tokio::time::Instant>,
}

impl Capabilities {
    /// Whether the remembered capabilities should be asked again first.
    pub(super) fn due(&self) -> bool {
        match (self.checked_at, self.failed_at) {
            (None, _) => true,
            (Some(checked), failed) => {
                checked.elapsed() >= RECHECK_AFTER
                    || failed.is_some_and(|failed| {
                        failed > checked && failed.elapsed() >= RECHECK_AFTER_FAILURE
                    })
            }
        }
    }

    /// Forget what was found, keeping when it failed.
    pub(super) fn reset(&mut self) {
        let failed_at = self.failed_at;
        *self = Self {
            failed_at,
            ..Self::default()
        };
    }
}

/// A header read on one endpoint, checked to be the block asked for.
pub async fn header(endpoint: &DynProvider, at: BlockNumberOrTag) -> Result<Header> {
    let block = endpoint
        .get_block_by_number(at)
        .await
        .context("reading a header")?
        .ok_or_else(|| anyhow!("the endpoint has no block {at}"))?;
    if let BlockNumberOrTag::Number(number) = at
        && block.header.number != number
    {
        bail!(
            "asked for block {number}, the endpoint gave block {}",
            block.header.number
        );
    }
    Ok(Header {
        number: block.header.number,
        hash: block.header.hash,
        parent_hash: block.header.parent_hash,
        timestamp: block.header.timestamp,
    })
}

/// The helper's call with no reads, only the parent's hash.
fn bare(source: BlockSource) -> Call {
    Call {
        reads: Vec::new(),
        hashes_of: Vec::new(),
        recent: 1,
        balances_of: Vec::new(),
        inner_gas: view_reads::MAX_INNER_GAS,
        result_bytes: view_reads::MIN_RESULT_BYTES,
        input_bytes: view_reads::MAX_INPUT_BYTES,
        arb_sys: source == BlockSource::ArbSys,
    }
}

/// Run the helper once on `endpoint` at `block`.
pub async fn run_helper(
    endpoint: &DynProvider,
    call: &Call,
    block: BlockId,
    gas: u64,
) -> Result<view_reads::Answer, CallError> {
    let request = TransactionRequest::default()
        .with_from(Address::ZERO)
        .with_to(Address::ZERO)
        .with_input(call.encode())
        .with_gas_limit(gas);
    let answer = endpoint
        .call(request)
        .block(block)
        .account_override(
            Address::ZERO,
            AccountOverride::default().with_code(view_reads::runtime()),
        )
        .await
        .map_err(CallError::from)?;
    call.decode(&answer).map_err(CallError::Answer)
}

/// Why one call did not give an answer.
#[derive(Debug)]
pub enum CallError {
    /// The call reverted, with this data.
    Reverted(Bytes),
    /// The endpoint answered with an error that is not a revert.
    Refused { code: i64, message: String },
    /// No answer: the connection, a timeout, a rate limit.
    Transport(String),
    /// An answer that does not check out.
    Answer(anyhow::Error),
}

impl From<alloy::transports::TransportError> for CallError {
    fn from(error: alloy::transports::TransportError) -> Self {
        match &error {
            alloy::rpc::json_rpc::RpcError::ErrorResp(payload) => match payload.as_revert_data() {
                Some(data) => Self::Reverted(data),
                None => Self::Refused {
                    code: payload.code,
                    message: payload.message.to_string(),
                },
            },
            _ => Self::Transport(error.to_string()),
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reverted(data) => write!(f, "the call reverted ({data})"),
            Self::Refused { code, message } => {
                write!(f, "the endpoint refused the call ({code}: {message})")
            }
            Self::Transport(error) => write!(f, "no answer: {error}"),
            Self::Answer(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for CallError {}

impl CallError {
    /// Whether the refusal names the answer's or the request's size.
    pub fn too_big(&self) -> bool {
        let text = match self {
            Self::Refused { message, .. } => message.to_ascii_lowercase(),
            Self::Transport(error) => error.to_ascii_lowercase(),
            _ => return false,
        };
        [
            "too big",
            "too large",
            "response size",
            "exceeds the limit",
            "body length",
        ]
        .iter()
        .any(|phrase| text.contains(phrase))
    }
}

/// Find out whether `endpoint` runs the helper under a state override, and
/// with which block source its numbers and parent hashes are its headers':
/// `None` when it refuses overrides or neither source matches its headers.
/// ArbSys is asked first: where it is absent the helper says so and the
/// plain source is asked. An answer is checked against the endpoint's own
/// header for the block it reports; one that matches neither leaves the
/// helper unused there.
pub async fn probe_overrides(endpoint: &DynProvider, gas: u64) -> Result<Option<BlockSource>> {
    // Twice: a node behind its own head, or a block between the two reads,
    // makes one try inconclusive.
    for _ in 0..2 {
        let head = header(endpoint, BlockNumberOrTag::Latest).await?;
        let mut inconclusive = false;
        for source in [BlockSource::ArbSys, BlockSource::Plain] {
            let answer = match run_helper(endpoint, &bare(source), BlockId::latest(), gas).await {
                Ok(answer) => answer,
                // The helper's own refusal: this source has no block number.
                Err(CallError::Reverted(_)) => continue,
                // The endpoint refuses the call as such: overrides are refused.
                Err(CallError::Refused { .. }) => return Ok(None),
                Err(error) => return Err(anyhow!(error).context("asking whether overrides run")),
            };
            if answer.number < head.number || answer.parent_hash == B256::ZERO {
                // A number below the head (another chain's, or a node
                // behind) or no parent hash: not this source, or not yet.
                inconclusive |= answer.number >= head.number;
                continue;
            }
            // Its own header for the block the helper says it ran at.
            let ran_at = match header(endpoint, BlockNumberOrTag::Number(answer.number)).await {
                Ok(ran_at) => ran_at,
                Err(_) => {
                    inconclusive = true;
                    continue;
                }
            };
            if ran_at.parent_hash == answer.parent_hash
                && ran_at.timestamp == answer.timestamp
                && answer.recent.first() == Some(&answer.parent_hash)
            {
                return Ok(Some(source));
            }
            inconclusive = true;
        }
        if !inconclusive {
            // Neither source gives this endpoint's own numbers: the helper
            // is not used here.
            return Ok(None);
        }
    }
    bail!("the helper's block could not be matched with a header on this endpoint yet")
}

/// A call that says which block it ran at without any helper or override:
/// the ArbSys block number where that is the source, otherwise code run as
/// a contract creation that returns `NUMBER`.
fn number_request(source: BlockSource) -> TransactionRequest {
    match source {
        BlockSource::ArbSys => TransactionRequest::default()
            .with_to(ARB_SYS)
            .with_input(IArbSys::arbBlockNumberCall {}.abi_encode()),
        BlockSource::Plain => {
            // NUMBER PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN
            let mut request = TransactionRequest::default().with_input(Bytes::from_static(&[
                0x43, 0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3,
            ]));
            request.to = Some(alloy::primitives::TxKind::Create);
            request
        }
    }
}

/// The block a pinned call ran at, as the call itself says.
async fn number_at(
    endpoint: &DynProvider,
    source: BlockSource,
    block: BlockId,
) -> Result<u64, CallError> {
    let answer = endpoint
        .call(number_request(source))
        .block(block)
        .await
        .map_err(CallError::from)?;
    if answer.len() != 32 {
        return Err(CallError::Answer(anyhow!(
            "the block number answer is {} bytes",
            answer.len()
        )));
    }
    u64::try_from(U256::from_be_slice(&answer))
        .map_err(|_| CallError::Answer(anyhow!("the block number does not fit")))
}

/// Find out whether `endpoint` answers a read pinned to a block hash as of
/// that block: [`PIN_CHECKS`] recent blocks, each read by its own call that
/// says where it ran, must each be answered at that block, and a hash no
/// block has must be refused rather than answered at the head.
pub async fn probe_hash_pin(endpoint: &DynProvider, source: BlockSource) -> Result<bool> {
    let head = header(endpoint, BlockNumberOrTag::Latest).await?;
    for back in 1..=PIN_CHECKS {
        let Some(number) = head.number.checked_sub(back) else {
            return Ok(false);
        };
        let block = header(endpoint, BlockNumberOrTag::Number(number)).await?;
        match number_at(endpoint, source, BlockId::hash_canonical(block.hash)).await {
            Ok(ran_at) if ran_at == number => {}
            Ok(_) => return Ok(false),
            Err(CallError::Transport(error)) => {
                bail!("asking whether a hash pin is honoured: {error}")
            }
            Err(_) => return Ok(false),
        }
    }
    let mut unknown = head.hash;
    unknown[0] ^= 0x80;
    match number_at(endpoint, source, BlockId::hash_canonical(unknown)).await {
        Err(CallError::Refused { .. }) => Ok(true),
        Err(CallError::Transport(error)) => {
            bail!("asking whether an unknown hash is refused: {error}")
        }
        _ => Ok(false),
    }
}

/// Find out whether a read pinned `depth` blocks below the head is answered
/// as of that block (one call that says where it ran).
pub async fn probe_past_blocks(
    endpoint: &DynProvider,
    source: BlockSource,
    depth: u64,
) -> Result<bool> {
    let head = header(endpoint, BlockNumberOrTag::Latest).await?;
    let Some(number) = head.number.checked_sub(depth) else {
        bail!("the chain is not {depth} blocks long yet");
    };
    match number_at(endpoint, source, BlockId::number(number)).await {
        Ok(ran_at) => Ok(ran_at == number),
        // A refusal for missing state is an answer: not kept that far back.
        Err(CallError::Refused { .. }) | Err(CallError::Reverted(_)) => Ok(false),
        Err(error) => Err(anyhow!(error).context("asking whether past blocks are honoured")),
    }
}

/// Whether the code at `at` is exactly the code whose hash is `expected`.
pub async fn probe_code(endpoint: &DynProvider, at: Address, expected: B256) -> Result<bool> {
    let code = endpoint
        .get_code_at(at)
        .await
        .context("reading Multicall3's code")?;
    Ok(!code.is_empty() && keccak256(&code) == expected)
}
