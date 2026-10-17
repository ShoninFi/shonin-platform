//! What one endpoint can do, found out by asking it, and remembered.
//!
//! Endpoints differ: some refuse state overrides, some ignore a block pin
//! (answering at their head), some run the EVM with another chain's block
//! numbers (Arbitrum Orbit), some cap a call's gas or answer's size without
//! saying so. A missing capability costs requests, never correctness: every
//! path is checked against the block it reports, and a capability is used
//! only once it has been shown.

use std::future::Future;
use std::sync::{Arc, Mutex};
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

/// How long one capability check may take. Checks run apart from the
/// observations that need them (`check`): an observation waits for one at
/// most until its own `read_budget_ms` runs out, and the check goes on, up
/// to this long, so a slow endpoint is still found out.
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
    /// The gas a call found available at entry, last time: the helper's
    /// own report where it runs, otherwise code run as a creation call
    /// ([`probe_gas`]), asked where Multicall3 is the path.
    pub gas_at_entry: Option<u64>,
    /// The most reads one call ran before its gas ran low: learned from the
    /// helper's reads left not run, or from Multicall3 reads that failed in
    /// a batch and returned when read alone.
    pub max_reads: Option<usize>,
    /// The largest answer it refused or failed to give, halved.
    pub output_bytes: Option<u64>,
    /// Whether an observation here needed reads of their own, so whether
    /// hash pins are honoured is asked before the next one.
    pub wants_hash_pin: bool,
    pub(super) checked_at: Option<tokio::time::Instant>,
    /// When an observation or a check here last failed.
    pub(super) failed_at: Option<tokio::time::Instant>,
    /// When a check here last failed or gave no answer: what it was to find
    /// out is not asked again for [`RECHECK_AFTER_FAILURE`]. A check the
    /// endpoint turned away (a rate limit, a server error) does not set it.
    pub(super) check_failed_at: Option<tokio::time::Instant>,
}

/// Which capabilities the paths need, given what is known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Wanted {
    pub source: bool,
    pub multicall3: bool,
    pub gas: bool,
    pub hash_pin: bool,
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

    /// Whether the helper is known to run here.
    pub(super) fn helper(&self) -> bool {
        self.overrides == Some(true) && self.source.is_some()
    }

    /// What the paths need asked besides whether overrides run: the block
    /// source, Multicall3 and its gas where the helper does not run, and
    /// whether hash pins are honoured where reads of their own are needed
    /// or no batch path is left.
    pub(super) fn wanted(&self) -> Wanted {
        let helper = self.helper();
        Wanted {
            source: !helper || self.wants_hash_pin,
            multicall3: !helper,
            gas: !helper && self.multicall3 == Some(true),
            hash_pin: self.wants_hash_pin || (!helper && self.multicall3 == Some(false)),
        }
    }

    /// Whether something the paths need is not known yet (the gas is only
    /// reported, so it is not waited for).
    pub(super) fn missing(&self) -> bool {
        let wanted = self.wanted();
        self.overrides.is_none()
            || (wanted.source && self.source.is_none())
            || (wanted.multicall3 && self.multicall3.is_none())
            || (wanted.hash_pin && self.hash_pin.is_none())
    }

    /// Whether a check failed here too recently to be asked again.
    pub(super) fn check_failed_lately(&self) -> bool {
        self.check_failed_at
            .is_some_and(|failed| failed.elapsed() < RECHECK_AFTER_FAILURE)
    }

    fn check_failed(&mut self) {
        let now = tokio::time::Instant::now();
        self.check_failed_at = Some(now);
        self.failed_at = Some(now);
    }

    /// A check the endpoint turned away in part: what it did not settle is
    /// asked again the next time it is wanted, at once, and a recheck it cut
    /// short is due again after [`RECHECK_AFTER_FAILURE`] (as after any
    /// failure here), but the endpoint is not passed over meanwhile.
    fn check_turned_away(&mut self) {
        self.failed_at = Some(tokio::time::Instant::now());
    }
}

/// One endpoint's capabilities, shared by the follower and the check that
/// finds them out.
pub(super) type Shared = Arc<Mutex<Capabilities>>;

pub(super) fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Capabilities> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How one check ended.
enum Asked<T> {
    Found(T),
    /// It answered, but not so the question was settled.
    Failed,
    /// It turned a request away for now (a rate limit, an HTTP server
    /// error): nothing was learned, and the question is asked again the
    /// next time it is wanted.
    TurnedAway,
    /// It did not answer within [`CHECK_TIMEOUT`] (or not at all): the
    /// endpoint is in trouble and nothing more is asked of it now.
    Silent,
}

async fn ask<T>(within: Duration, check: impl Future<Output = Result<T>>) -> Asked<T> {
    match tokio::time::timeout(within, check).await {
        Ok(Ok(found)) => Asked::Found(found),
        Ok(Err(error)) if turned_away(&error) => Asked::TurnedAway,
        Ok(Err(error)) if silent(&error) => Asked::Silent,
        Ok(Err(_)) => Asked::Failed,
        Err(_) => Asked::Silent,
    }
}

/// Whether an error is the endpoint turning a request away for now (a rate
/// limit however it arrived, or an HTTP server error), which is an answer
/// of a kind: the endpoint is there, and may answer the next time it is
/// asked.
fn turned_away(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| match cause.downcast_ref::<CallError>() {
            Some(CallError::Transport(text)) => crate::client::turned_away_text(text),
            Some(_) => false,
            None => cause
                .downcast_ref::<alloy::transports::TransportError>()
                .is_some_and(crate::client::turned_away),
        })
}

/// Whether an error is the endpoint not answering at all (the connection,
/// a timeout), rather than an answer.
fn silent(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CallError>(),
            Some(CallError::Transport(_))
        ) || cause
            .downcast_ref::<alloy::transports::TransportError>()
            .is_some_and(|error| !matches!(error, alloy::rpc::json_rpc::RpcError::ErrorResp(_)))
    })
}

/// Find out what `endpoint` can do, into `shared`: everything again when it
/// is due ([`Capabilities::due`], which also forgets what was learned of its
/// limits), otherwise only what the paths need and is not known. A check
/// that fails leaves what was known before and marks the failure, so it is
/// not asked again for [`RECHECK_AFTER_FAILURE`]; one the endpoint does not
/// answer ends the whole check. One the endpoint turns away (a rate limit,
/// a server error) also leaves what was known, but is not marked so: it is
/// asked again the next time it is wanted, so an endpoint that limited a
/// burst of requests is used as soon as it answers again. Run apart from
/// any observation, which waits for it at most until its budget runs out.
pub(super) async fn check(
    endpoint: DynProvider,
    shared: Shared,
    gas: u64,
    multicall3: Address,
    multicall3_code_hash: B256,
) {
    let again = {
        let mut known = lock(&shared);
        let again = known.due();
        if again {
            known.checked_at = Some(tokio::time::Instant::now());
            known.max_reads = None;
            known.output_bytes = None;
            known.gas_at_entry = None;
            known.past_blocks = None;
        }
        again
    };
    // Asked when wanted and not known, or, on a recheck, when known or wanted.
    let asks = |known: bool, wanted: bool| (wanted && !known) || (again && (known || wanted));
    // Whether the endpoint turned a step away.
    let mut turned = false;
    macro_rules! settle {
        ($asked:expr, $found:pat => $write:expr) => {
            match $asked {
                Asked::Found($found) => $write,
                Asked::Failed => lock(&shared).check_failed(),
                Asked::TurnedAway => turned = true,
                Asked::Silent => {
                    lock(&shared).check_failed();
                    return;
                }
            }
        };
    }
    let known = lock(&shared).clone();
    let mut source_found = false;
    if asks(known.overrides.is_some(), true) {
        settle!(ask(CHECK_TIMEOUT, probe_overrides(&endpoint, gas)).await, source => {
            let mut known = lock(&shared);
            known.overrides = Some(source.is_some());
            if source.is_some() {
                known.source = source;
                source_found = true;
            }
        });
    }
    let known = lock(&shared).clone();
    let wanted = known.wanted();
    if !source_found && asks(known.source.is_some(), wanted.source) {
        settle!(ask(CHECK_TIMEOUT, probe_source(&endpoint)).await, source => {
            lock(&shared).source = Some(source);
        });
    }
    if asks(known.multicall3.is_some(), wanted.multicall3) {
        settle!(
            ask(CHECK_TIMEOUT, probe_code(&endpoint, multicall3, multicall3_code_hash)).await,
            present => lock(&shared).multicall3 = Some(present)
        );
    }
    let known = lock(&shared).clone();
    let wanted = known.wanted();
    if asks(known.gas_at_entry.is_some(), wanted.gas) {
        // Only reported: an endpoint that refuses creation calls is not
        // backed off for it, and is asked again at the next check.
        match ask(CHECK_TIMEOUT, probe_gas(&endpoint, gas)).await {
            Asked::Found(found) => lock(&shared).gas_at_entry = Some(found),
            Asked::Failed | Asked::TurnedAway => {}
            Asked::Silent => {
                lock(&shared).check_failed();
                return;
            }
        }
    }
    if asks(known.hash_pin.is_some(), wanted.hash_pin) {
        let Some(source) = known.source else {
            // Not to be asked without the block source: one the endpoint
            // turned away is asked again the next time it is wanted, one
            // that failed not for a while.
            let mut known = lock(&shared);
            if turned {
                known.check_turned_away();
            } else {
                known.check_failed();
            }
            return;
        };
        settle!(
            ask(CHECK_TIMEOUT * 2, probe_hash_pin(&endpoint, source)).await,
            honours => lock(&shared).hash_pin = Some(honours)
        );
    }
    if turned {
        lock(&shared).check_turned_away();
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
    /// Whether the endpoint said the call as a whole ran out of gas (not a
    /// revert: a call that runs out of gas has no revert data), in any of
    /// the ways nodes say so: "out of gas", "gas required exceeds
    /// allowance", or an EVM's own name for it (`OutOfGas`, `MemoryOOG`,
    /// `PrecompileOOG`, ...).
    pub fn out_of_gas(&self) -> bool {
        let text = match self {
            Self::Refused { message, .. } => message.to_ascii_lowercase(),
            _ => return false,
        };
        text.contains("out of gas")
            || text.contains("gas required exceeds")
            || text
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word == "outofgas" || (word.ends_with("oog") && word != "google"))
    }

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
/// header for the block it reports, whatever that block is: behind a load
/// balancer the call may run on a node a block or two behind the one that
/// gave the head. An answer that matches no header is conclusive only when
/// its number is far from the head (another chain's numbers, as Orbit's
/// `block.number` is the parent chain's); near the head it may be a node
/// behind or a block mined between the reads, and the probe is made again.
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
            let near = answer.number.abs_diff(head.number) <= view_reads::MAX_RECENT;
            if answer.parent_hash == B256::ZERO {
                // No parent hash: not this source, or not yet.
                inconclusive |= near;
                continue;
            }
            // Its own header for the block the helper says it ran at.
            match header(endpoint, BlockNumberOrTag::Number(answer.number)).await {
                Ok(ran_at)
                    if ran_at.parent_hash == answer.parent_hash
                        && ran_at.timestamp == answer.timestamp
                        && answer.recent.first() == Some(&answer.parent_hash) =>
                {
                    return Ok(Some(source));
                }
                // Turned away, the header says nothing either way.
                Err(error) if turned_away(&error) => return Err(error),
                Ok(_) | Err(_) => inconclusive |= near,
            }
        }
        if !inconclusive {
            // Neither source gives this endpoint's own numbers: the helper
            // is not used here.
            return Ok(None);
        }
    }
    bail!("the helper's block could not be matched with a header on this endpoint yet")
}

/// The block source where the helper did not say: ArbSys where it answers
/// with a block number, the plain one where the call is refused or answers
/// otherwise.
pub async fn probe_source(endpoint: &DynProvider) -> Result<BlockSource> {
    let request = TransactionRequest::default()
        .with_to(ARB_SYS)
        .with_input(IArbSys::arbBlockNumberCall {}.abi_encode());
    match endpoint.call(request).await.map_err(CallError::from) {
        Ok(bytes) if bytes.len() == 32 => Ok(BlockSource::ArbSys),
        Ok(_) | Err(CallError::Reverted(_)) | Err(CallError::Refused { .. }) => {
            Ok(BlockSource::Plain)
        }
        Err(error) => Err(anyhow!(error).context("asking for ArbSys's block number")),
    }
}

/// The gas a call asking for `gas` is given here: what code run as a
/// creation call finds left at entry (`GAS`), which needs no override and no
/// contract. Endpoints cap a call's gas without saying so.
pub async fn probe_gas(endpoint: &DynProvider, gas: u64) -> Result<u64> {
    // GAS PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN
    let mut request = TransactionRequest::default()
        .with_input(Bytes::from_static(&[
            0x5a, 0x60, 0, 0x52, 0x60, 0x20, 0x60, 0, 0xf3,
        ]))
        .with_gas_limit(gas);
    request.to = Some(alloy::primitives::TxKind::Create);
    let answer = endpoint
        .call(request)
        .await
        .map_err(|error| anyhow!(CallError::from(error)).context("asking for a call's gas"))?;
    if answer.len() != 32 {
        bail!("the gas answer is {} bytes", answer.len());
    }
    u64::try_from(U256::from_be_slice(&answer)).map_err(|_| anyhow!("the gas does not fit"))
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
            Err(error @ CallError::Transport(_)) => {
                return Err(anyhow!(error).context("asking whether a hash pin is honoured"));
            }
            Err(_) => return Ok(false),
        }
    }
    let mut unknown = head.hash;
    unknown[0] ^= 0x80;
    match number_at(endpoint, source, BlockId::hash_canonical(unknown)).await {
        Err(CallError::Refused { .. }) => Ok(true),
        Err(error @ CallError::Transport(_)) => {
            Err(anyhow!(error).context("asking whether an unknown hash is refused"))
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
