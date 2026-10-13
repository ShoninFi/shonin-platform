//! Reading a plan at one block on one endpoint, by the cheapest path the
//! endpoint has been shown to support, and checking where each answer ran.

use std::collections::BTreeMap;
use std::ops::Range;

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::TransactionRequest;
use anyhow::{Result, anyhow, bail};
use tokio::time::Instant;

use super::capabilities::{self, BlockSource, CallError, Capabilities, header};
use super::multicall::Batch;
use super::view_reads::{self, Answer, Call, Outcome};
use super::{At, Follower, Header, Item, Observed, Path, Plan, Read, wall_ms};

/// How many hashes below the observed block every observation carries.
const RECENT: u64 = view_reads::MAX_RECENT;

/// How many times a follow-up round re-reads items the first answer left
/// open before the rest are read one by one.
const FOLLOW_UP_ROUNDS: usize = 8;

/// How many times an observation is asked again, smaller, after an
/// endpoint refused an answer as too big.
const SHRINKS: usize = 3;

/// Why a path gave no observation.
pub(super) enum Failure {
    /// The endpoint is in trouble (no answer, an answer from another block,
    /// a malformed answer): the next endpoint is asked.
    Trouble(anyhow::Error),
    /// The path is not available here: the next path is tried.
    Unsupported(anyhow::Error),
    /// The answer was refused as too big: asked again, smaller.
    TooBig,
    /// The call as a whole ran out of gas.
    OutOfGas,
    /// Fewer reads a call were learned: asked again.
    Fewer,
}

impl From<CallError> for Failure {
    fn from(error: CallError) -> Self {
        if error.too_big() {
            return Self::TooBig;
        }
        match error {
            CallError::Refused { .. } | CallError::Reverted(_) => Self::Unsupported(anyhow!(error)),
            CallError::Transport(_) | CallError::Answer(_) => Self::Trouble(anyhow!(error)),
        }
    }
}

/// How large one call may be.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub max_reads: usize,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub result_bytes: u64,
}

/// Split `reads` into consecutive calls within `limits`; the first also
/// carries `hashes` chosen hashes, `recent` recent ones and `balances`.
pub(super) fn split(
    reads: &[Read],
    hashes: usize,
    recent: u64,
    balances: usize,
    limits: Limits,
) -> Result<Vec<Range<usize>>> {
    let items: Vec<view_reads::Item> = reads.iter().map(item).collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    loop {
        let first = chunks.is_empty();
        let (h, r, b) = if first {
            (hashes, recent, balances)
        } else {
            (0, 0, 0)
        };
        let fits = |end: usize| {
            end - start <= limits.max_reads
                && view_reads::input_size(&items[start..end], h, b) <= limits.input_bytes
                && view_reads::output_bound(end - start, h, r, b, limits.result_bytes)
                    <= limits.output_bytes
        };
        if !fits(start) {
            bail!("the plan's hashes and balances alone exceed one call's bounds");
        }
        let mut end = start;
        while end < items.len() && fits(end + 1) {
            end += 1;
        }
        if end == start && start < items.len() {
            bail!("read {start} alone exceeds one call's bounds");
        }
        chunks.push(start..end);
        start = end;
        if start >= items.len() {
            return Ok(chunks);
        }
    }
}

fn item(read: &Read) -> view_reads::Item {
    view_reads::Item {
        target: read.target,
        data: read.data.clone(),
    }
}

/// Why an item is still open after a helper call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    NotRun,
    Oversized(u64),
    OutOfGas,
}

/// What an observation has gathered so far.
struct Gathered {
    items: Vec<Option<Item>>,
    open: Vec<(usize, Open)>,
    hashes: BTreeMap<u64, B256>,
    zero: Vec<u64>,
    balances: Vec<U256>,
    requests: u32,
}

impl Gathered {
    fn new(reads: usize) -> Self {
        Self {
            items: vec![None; reads],
            open: Vec::new(),
            hashes: BTreeMap::new(),
            zero: Vec::new(),
            balances: Vec::new(),
            requests: 0,
        }
    }

    /// A hash for block `number` from an observation at `at`: held when
    /// nonzero, noted when zero inside the window. Block 0 is never held.
    fn hash(&mut self, at: u64, number: u64, hash: B256) {
        if number == 0 || number >= at {
            return;
        }
        if hash != B256::ZERO {
            self.hashes.insert(number, hash);
        } else if at - number <= RECENT {
            self.zero.push(number);
        }
    }

    fn observed(
        self,
        endpoint: usize,
        path: Path,
        block: Header,
        own_hash: bool,
        wall: u64,
        gas: Option<u64>,
    ) -> Result<Observed, Failure> {
        let mut items = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.into_iter().enumerate() {
            items.push(item.ok_or_else(|| {
                Failure::Trouble(anyhow!("read {index} was left without an answer"))
            })?);
        }
        let mut hashes = self.hashes;
        if own_hash {
            hashes.insert(block.number, block.hash);
        }
        if block.number > 0 && block.parent_hash != B256::ZERO {
            hashes.insert(block.number - 1, block.parent_hash);
        }
        Ok(Observed {
            endpoint,
            path,
            number: block.number,
            timestamp: block.timestamp,
            parent_hash: block.parent_hash,
            hash: own_hash.then_some(block.hash),
            hashes,
            zero: self.zero,
            balances: self.balances,
            items,
            wall_ms: wall,
            gas_at_entry: gas,
            requests: self.requests,
        })
    }
}

/// Whether `answer` ran at `block`.
fn ran_at(answer: &Answer, block: &Header) -> bool {
    answer.number == block.number
        && answer.timestamp == block.timestamp
        && answer.parent_hash == block.parent_hash
}

impl Follower {
    /// Observe `plan` at `at`: from the endpoint that answered last, then
    /// the others in turn, each alone for the whole observation.
    pub async fn observe(&mut self, plan: &Plan, at: At) -> Result<Observed> {
        plan.check()?;
        let count = self.endpoints.len();
        let mut failures = Vec::new();
        for step in 0..count {
            let endpoint = (self.preferred + step) % count;
            match self.observe_on(endpoint, plan, at).await {
                Ok(observed) => {
                    self.preferred = endpoint;
                    return Ok(observed);
                }
                Err(error) => failures.push(format!("endpoint {}: {error:#}", endpoint + 1)),
            }
        }
        bail!("no endpoint gave an observation ({})", failures.join("; "))
    }

    /// Observe `plan` at `at` on `endpoint` alone, within `read_budget_ms`
    /// whatever the endpoint does: the wait for a check of what it can do
    /// (`ready`), the paths, and a second try when reads of
    /// their own were found to need a hash pin, all share that one budget.
    pub async fn observe_on(&mut self, endpoint: usize, plan: &Plan, at: At) -> Result<Observed> {
        plan.check()?;
        if endpoint >= self.endpoints.len() {
            bail!("there is no endpoint {}", endpoint + 1);
        }
        let deadline = Instant::now() + self.budget();
        self.ready(endpoint, deadline).await;
        let mut result = self.timed(endpoint, plan, at, deadline).await;
        // Reads that need a read of their own wait for the endpoint to be
        // shown to honour hash pins: asked now, and the observation made
        // again if that is known within the budget.
        if result.is_err() {
            let known = self.known(endpoint);
            if known.wants_hash_pin && known.hash_pin.is_none() {
                self.ready(endpoint, deadline).await;
                if self.known(endpoint).hash_pin == Some(true) {
                    result = self.timed(endpoint, plan, at, deadline).await;
                }
            }
        }
        if result.is_err() {
            self.learn(endpoint, |known| known.failed_at = Some(Instant::now()));
        }
        result
    }

    /// The paths, until `deadline`.
    async fn timed(
        &mut self,
        endpoint: usize,
        plan: &Plan,
        at: At,
        deadline: Instant,
    ) -> Result<Observed> {
        tokio::time::timeout_at(deadline, self.paths(endpoint, plan, at))
            .await
            .unwrap_or_else(|_| {
                Err(anyhow!(
                    "the observation took longer than read_budget_ms={}",
                    self.policy.read_budget_ms
                ))
            })
    }

    /// Start a check of what `endpoint` can do (`capabilities::check`)
    /// when one is due, or when the paths need something not known yet and
    /// no check of it failed lately; never two at once. Wait for a running
    /// check, until `deadline`, only while the paths need what it is finding
    /// out: a recheck of what is known runs on while observations use it.
    /// A check the deadline cuts short goes on, and what it finds is there
    /// for the next observation.
    pub(super) async fn ready(&mut self, endpoint: usize, deadline: Instant) {
        // Twice at most: a check that ended may have found that the paths
        // need one more thing (Multicall3 where overrides are refused).
        for _ in 0..2 {
            if self.checks[endpoint]
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                self.checks[endpoint] = None;
            }
            let known = self.known(endpoint);
            if self.checks[endpoint].is_none() {
                if !known.due() && !(known.missing() && !known.check_failed_lately()) {
                    return;
                }
                self.checks[endpoint] = Some(tokio::spawn(capabilities::check(
                    self.endpoints[endpoint].clone(),
                    self.capabilities[endpoint].clone(),
                    self.policy.view_outer_gas,
                    self.options.multicall3,
                    self.options.multicall3_code_hash,
                )));
            }
            if !known.missing() {
                return;
            }
            let Some(check) = self.checks[endpoint].as_mut() else {
                return;
            };
            if tokio::time::timeout_at(deadline, check).await.is_err() {
                return;
            }
            self.checks[endpoint] = None;
        }
    }

    /// What `endpoint` is known to do, now.
    fn known(&self, endpoint: usize) -> Capabilities {
        capabilities::lock(&self.capabilities[endpoint]).clone()
    }

    /// Note something learned of `endpoint`.
    fn learn(&self, endpoint: usize, learned: impl FnOnce(&mut Capabilities)) {
        learned(&mut capabilities::lock(&self.capabilities[endpoint]));
    }

    /// The paths in order, each used only where it was shown to work here.
    async fn paths(&mut self, endpoint: usize, plan: &Plan, at: At) -> Result<Observed> {
        let mut unsupported = Vec::new();
        let capabilities = self.known(endpoint);
        if capabilities.overrides == Some(true)
            && let Some(source) = capabilities.source
        {
            for _ in 0..=SHRINKS {
                match self.by_view_reads(endpoint, plan, at, source).await {
                    Ok(observed) => return Ok(observed),
                    Err(Failure::Trouble(error)) => return Err(error),
                    Err(Failure::Unsupported(error)) => {
                        unsupported.push(format!("the helper: {error:#}"));
                        break;
                    }
                    Err(Failure::TooBig) => self.shrink(endpoint),
                    Err(Failure::OutOfGas | Failure::Fewer) => {}
                }
            }
        }
        let capabilities = self.known(endpoint);
        if let Some(source) = capabilities.source {
            if capabilities.multicall3 == Some(true) {
                for _ in 0..=SHRINKS {
                    match self.by_multicall(endpoint, plan, at, source).await {
                        Ok(observed) => return Ok(observed),
                        Err(Failure::Trouble(error)) => return Err(error),
                        Err(Failure::Unsupported(error)) => {
                            unsupported.push(format!("Multicall3: {error:#}"));
                            break;
                        }
                        Err(Failure::TooBig) => self.shrink(endpoint),
                        Err(Failure::OutOfGas | Failure::Fewer) => {}
                    }
                }
            }
            let capabilities = self.known(endpoint);
            if capabilities.hash_pin == Some(true) {
                match self.by_individual(endpoint, plan, at).await {
                    Ok(observed) => return Ok(observed),
                    Err(Failure::Trouble(error)) => return Err(error),
                    Err(Failure::Unsupported(error)) => {
                        unsupported.push(format!("single reads: {error:#}"))
                    }
                    Err(Failure::TooBig) => {
                        unsupported.push("single reads: an answer was too big".into())
                    }
                    Err(Failure::OutOfGas | Failure::Fewer) => {
                        unsupported.push("single reads: a read ran out of gas".into())
                    }
                }
            }
        }
        if unsupported.is_empty() {
            bail!("no path is known to work on this endpoint yet");
        }
        bail!("{}", unsupported.join("; "))
    }

    /// Ask for answers half as large from now on.
    fn shrink(&mut self, endpoint: usize) {
        let largest = self.policy.view_output_bytes;
        self.learn(endpoint, |known| {
            let now = known.output_bytes.unwrap_or(largest);
            known.output_bytes = Some((now / 2).max(32_768));
        });
    }

    fn limits(&self, endpoint: usize) -> Limits {
        let capabilities = self.known(endpoint);
        let max_reads = usize::try_from(self.policy.view_batch_reads)
            .unwrap_or(usize::MAX)
            .min(view_reads::MAX_READS)
            .min(capabilities.max_reads.unwrap_or(usize::MAX))
            .max(1);
        Limits {
            max_reads,
            input_bytes: self
                .policy
                .view_batch_input_bytes
                .min(view_reads::MAX_INPUT_BYTES),
            output_bytes: self
                .policy
                .view_output_bytes
                .min(capabilities.output_bytes.unwrap_or(u64::MAX))
                .min(view_reads::MAX_OUTPUT_BYTES),
            result_bytes: self
                .policy
                .view_result_bytes
                .clamp(view_reads::MIN_RESULT_BYTES, view_reads::MAX_RESULT_BYTES),
        }
    }

    /// The block to pin to: the one asked for, or the endpoint's head read
    /// as a header when an observation needs more than one call.
    async fn block_for(
        &self,
        provider: &DynProvider,
        at: At,
        gathered: &mut Gathered,
    ) -> Result<Header, Failure> {
        match at {
            At::Pinned(block) => Ok(block),
            At::Latest => {
                gathered.requests += 1;
                header(provider, BlockNumberOrTag::Latest)
                    .await
                    .map_err(Failure::Trouble)
            }
        }
    }

    /// One helper call: at the head, or pinned to `block` (by its hash, then
    /// by its number when the endpoint ran it elsewhere) and accepted only
    /// if it says it ran at that block.
    async fn helper(
        &self,
        provider: &DynProvider,
        call: &Call,
        block: Option<&Header>,
        gathered: &mut Gathered,
    ) -> Result<Answer, Failure> {
        let gas = self.policy.view_outer_gas;
        let Some(block) = block else {
            gathered.requests += 1;
            return Ok(capabilities::run_helper(provider, call, BlockId::latest(), gas).await?);
        };
        let mut elsewhere = None;
        for pin in [
            BlockId::hash_canonical(block.hash),
            BlockId::number(block.number),
        ] {
            gathered.requests += 1;
            match capabilities::run_helper(provider, call, pin, gas).await {
                Ok(answer) if ran_at(&answer, block) => return Ok(answer),
                Ok(answer) => elsewhere = Some(answer.number),
                Err(error) if error.too_big() => return Err(Failure::TooBig),
                // The endpoint has no such block (behind, or on another
                // branch): asked by number, it may say where it is.
                Err(CallError::Refused { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Failure::Trouble(match elsewhere {
            Some(number) => anyhow!(
                "the helper ran at block {number} when block {} was asked for",
                block.number
            ),
            None => anyhow!(
                "the endpoint does not run the helper at block {}",
                block.number
            ),
        }))
    }

    /// The ViewReads path: the plan in as few helper calls as its bounds
    /// allow, the first carrying the hashes and balances; items it leaves
    /// open (gas ran low, an answer over the result cap, a read out of gas)
    /// read again at the same block, larger, and the rest one by one.
    async fn by_view_reads(
        &mut self,
        endpoint: usize,
        plan: &Plan,
        at: At,
        source: BlockSource,
    ) -> Result<Observed, Failure> {
        let provider = self.endpoints[endpoint].clone();
        let limits = self.limits(endpoint);
        let chunks = split(
            &plan.reads,
            plan.hashes.len(),
            RECENT,
            plan.balances.len(),
            limits,
        )
        .map_err(Failure::Unsupported)?;
        let mut gathered = Gathered::new(plan.reads.len());
        let mut block = match (at, chunks.len()) {
            (At::Latest, 1) => None,
            _ => Some(self.block_for(&provider, at, &mut gathered).await?),
        };
        let arb_sys = source == BlockSource::ArbSys;
        let mut context: Option<Header> = block;
        let mut wall = 0;
        let mut gas_at_entry = None;
        for (index, range) in chunks.iter().enumerate() {
            let first = index == 0;
            let call = Call {
                reads: plan.reads[range.clone()].iter().map(item).collect(),
                hashes_of: if first {
                    plan.hashes.clone()
                } else {
                    Vec::new()
                },
                recent: if first { RECENT } else { 0 },
                balances_of: if first {
                    plan.balances.clone()
                } else {
                    Vec::new()
                },
                inner_gas: self.policy.view_inner_gas.min(view_reads::MAX_INNER_GAS),
                result_bytes: limits.result_bytes,
                input_bytes: limits.input_bytes,
                arb_sys,
            };
            let answer = self
                .helper(&provider, &call, block.as_ref(), &mut gathered)
                .await?;
            if first {
                wall = wall_ms();
                gas_at_entry = Some(answer.gas_at_entry);
                if answer.number == 0
                    || answer.parent_hash == B256::ZERO
                    || answer.recent.first() != Some(&answer.parent_hash)
                {
                    return Err(Failure::Trouble(anyhow!(
                        "the helper's block {} came without its parent's hash",
                        answer.number
                    )));
                }
                let number = answer.number;
                for (offset, hash) in answer.recent.iter().enumerate() {
                    if let Some(below) = number.checked_sub(1 + offset as u64) {
                        gathered.hash(number, below, *hash);
                    }
                }
                for (asked, hash) in plan.hashes.iter().zip(&answer.hashes) {
                    gathered.hash(number, *asked, *hash);
                }
                gathered.balances = answer.balances.clone();
                if context.is_none() {
                    context = Some(Header {
                        number,
                        hash: B256::ZERO,
                        parent_hash: answer.parent_hash,
                        timestamp: answer.timestamp,
                    });
                }
            }
            self.take(endpoint, range.start, &answer, &mut gathered, true);
        }
        let mut context = context.expect("the first call sets the block");
        if !gathered.open.is_empty() {
            // Follow-ups are pinned: by the header asked for, or by the
            // endpoint's own header for the block the first answer ran at.
            if block.is_none() {
                gathered.requests += 1;
                let own = header(&provider, BlockNumberOrTag::Number(context.number))
                    .await
                    .map_err(Failure::Trouble)?;
                if own.parent_hash != context.parent_hash || own.timestamp != context.timestamp {
                    return Err(Failure::Trouble(anyhow!(
                        "block {} changed between the observation and its follow-up",
                        context.number
                    )));
                }
                context = own;
                block = Some(own);
            }
            self.follow_up(
                endpoint,
                &provider,
                plan,
                &context,
                limits,
                arb_sys,
                &mut gathered,
            )
            .await?;
        }
        gathered.observed(
            endpoint,
            Path::ViewReads,
            context,
            block.is_some(),
            wall,
            gas_at_entry,
        )
    }

    /// Record a helper answer's items, from `start` in the plan, and, with
    /// `learn` (a call at the usual result cap: a larger cap reserves more
    /// gas a read and runs fewer), learn how many reads one call gets
    /// through before its gas runs low.
    fn take(
        &self,
        endpoint: usize,
        start: usize,
        answer: &Answer,
        gathered: &mut Gathered,
        learn: bool,
    ) {
        let mut ran = 0;
        let mut ran_low = false;
        for (offset, result) in answer.results.iter().enumerate() {
            let index = start + offset;
            let open = match result.outcome {
                Outcome::Returned => {
                    gathered.items[index] = Some(Item::Returned(result.data.clone()));
                    None
                }
                Outcome::Reverted => {
                    gathered.items[index] = Some(Item::Reverted(result.data.clone()));
                    None
                }
                // The helper reports an oversized answer's size; anything
                // else is taken as too large for any re-read in the helper.
                Outcome::Oversized => Some(Open::Oversized(if result.data.len() == 32 {
                    u64::try_from(U256::from_be_slice(&result.data)).unwrap_or(u64::MAX)
                } else {
                    u64::MAX
                })),
                Outcome::OutOfGas => Some(Open::OutOfGas),
                Outcome::NotRun => {
                    ran_low = true;
                    Some(Open::NotRun)
                }
                _ => Some(Open::OutOfGas),
            };
            if let Some(open) = open {
                gathered.open.push((index, open));
            }
            if result.outcome != Outcome::NotRun {
                ran += 1;
            }
        }
        self.learn(endpoint, |known| {
            known.gas_at_entry = Some(answer.gas_at_entry);
            if ran_low && learn {
                known.max_reads = Some(known.max_reads.unwrap_or(usize::MAX).min(ran.max(1)));
            }
        });
    }

    /// Read the open items again at `block`: those whose gas ran low as
    /// they were, those over the result cap with the helper's largest cap,
    /// and what is still open after that one by one.
    #[allow(clippy::too_many_arguments)]
    async fn follow_up(
        &mut self,
        endpoint: usize,
        provider: &DynProvider,
        plan: &Plan,
        block: &Header,
        limits: Limits,
        arb_sys: bool,
        gathered: &mut Gathered,
    ) -> Result<(), Failure> {
        for _ in 0..FOLLOW_UP_ROUNDS {
            let open = std::mem::take(&mut gathered.open);
            let mut single = Vec::new();
            let mut again = Vec::new();
            let mut larger = Vec::new();
            for (index, why) in open {
                match why {
                    Open::NotRun => again.push(index),
                    Open::Oversized(size)
                        if size <= view_reads::MAX_RESULT_BYTES
                            && limits.result_bytes < view_reads::MAX_RESULT_BYTES =>
                    {
                        larger.push(index)
                    }
                    _ => single.push((index, why)),
                }
            }
            if again.is_empty() && larger.is_empty() {
                gathered.open = single;
                break;
            }
            for (indices, result_bytes) in [
                (again, limits.result_bytes),
                (larger, view_reads::MAX_RESULT_BYTES),
            ] {
                if indices.is_empty() {
                    continue;
                }
                let reads: Vec<Read> = indices.iter().map(|i| plan.reads[*i].clone()).collect();
                let limits = Limits {
                    result_bytes,
                    ..self.limits(endpoint)
                };
                let chunks = split(&reads, 0, 0, 0, limits).map_err(Failure::Unsupported)?;
                for range in chunks {
                    let call = Call {
                        reads: reads[range.clone()].iter().map(item).collect(),
                        hashes_of: Vec::new(),
                        recent: 0,
                        balances_of: Vec::new(),
                        inner_gas: self.policy.view_inner_gas.min(view_reads::MAX_INNER_GAS),
                        result_bytes,
                        input_bytes: limits.input_bytes,
                        arb_sys,
                    };
                    let answer = self.helper(provider, &call, Some(block), gathered).await?;
                    let before = gathered.open.len();
                    // Map the call's items back to their places in the plan.
                    let mut placed = Gathered::new(range.len());
                    self.take(
                        endpoint,
                        0,
                        &answer,
                        &mut placed,
                        result_bytes == self.limits(endpoint).result_bytes,
                    );
                    for (offset, done) in placed.items.into_iter().enumerate() {
                        if let Some(done) = done {
                            gathered.items[indices[range.start + offset]] = Some(done);
                        }
                    }
                    for (offset, why) in placed.open {
                        gathered.open.push((indices[range.start + offset], why));
                    }
                    // A call that ran none of its reads makes no progress:
                    // those reads are read one by one.
                    let none_ran = answer.results.iter().all(|r| r.outcome == Outcome::NotRun);
                    if none_ran {
                        let stuck: Vec<(usize, Open)> = gathered.open.drain(before..).collect();
                        single.extend(stuck.into_iter().map(|(i, _)| (i, Open::OutOfGas)));
                    }
                }
            }
            gathered.open.extend(single);
        }
        let rest = std::mem::take(&mut gathered.open);
        if rest.is_empty() {
            return Ok(());
        }
        if self.known(endpoint).hash_pin != Some(true) {
            self.learn(endpoint, |known| known.wants_hash_pin = true);
            return Err(Failure::Unsupported(anyhow!(
                "{} reads need a read of their own, and this endpoint has not been shown to honour a hash pin",
                rest.len()
            )));
        }
        for (index, _) in rest {
            gathered.items[index] =
                Some(single_read(provider, &plan.reads[index], block, gathered).await?);
        }
        Ok(())
    }

    /// The Multicall3 path: the endpoint's header first, then every call
    /// pinned to it and accepted only if Multicall3 reports that block.
    ///
    /// Multicall3 gives each read all the gas left and reports a read that
    /// ran out of it as it reports a revert with no data: an endpoint that
    /// caps a call's gas without saying so makes a costly read fail so, while
    /// the cheap ones after it still return. A read that failed with no data
    /// is therefore no answer: it is read alone, pinned to the block's hash,
    /// on an endpoint shown to honour hash pins, where running out of gas is
    /// an error and not a revert; elsewhere the observation fails. A read
    /// that then answers had been starved by the reads before it, so calls
    /// here take fewer reads from then on; a call that runs out of gas as a
    /// whole is asked again with half its reads.
    async fn by_multicall(
        &mut self,
        endpoint: usize,
        plan: &Plan,
        at: At,
        source: BlockSource,
    ) -> Result<Observed, Failure> {
        let provider = self.endpoints[endpoint].clone();
        let limits = self.limits(endpoint);
        let mut gathered = Gathered::new(plan.reads.len());
        let block = self.block_for(&provider, at, &mut gathered).await?;
        let window: Vec<u64> = (block.number.saturating_sub(RECENT).max(1)..block.number).collect();
        let chunks = split(
            &plan.reads,
            window.len() + plan.hashes.len(),
            0,
            plan.balances.len(),
            limits,
        )
        .map_err(Failure::Unsupported)?;
        let mut wall = 0;
        // Reads that failed with no data: (place in the plan, place among
        // its call's reads).
        let mut unclear = Vec::new();
        for (index, range) in chunks.iter().enumerate() {
            let first = index == 0;
            let mut numbers = if first { window.clone() } else { Vec::new() };
            if first {
                numbers.extend(plan.hashes.iter().copied());
            }
            if block.number > 0 && !numbers.contains(&(block.number - 1)) {
                numbers.push(block.number - 1);
            }
            let batch = Batch::new(
                self.options.multicall3,
                source,
                numbers,
                if first { &plan.balances } else { &[] },
                &plan.reads[range.clone()],
            );
            let parts = match self
                .multicall_at(&provider, &batch, &block, &mut gathered)
                .await
            {
                Ok(parts) => parts,
                Err(Failure::OutOfGas) if range.len() > 1 => {
                    let fewer = range.len() / 2;
                    self.learn(endpoint, |known| {
                        known.max_reads = Some(known.max_reads.unwrap_or(usize::MAX).min(fewer));
                    });
                    return Err(Failure::Fewer);
                }
                Err(Failure::OutOfGas) => {
                    return Err(Failure::Unsupported(anyhow!(
                        "a Multicall3 call with {} reads ran out of gas",
                        range.len()
                    )));
                }
                Err(failure) => return Err(failure),
            };
            if first {
                wall = wall_ms();
                for (number, hash) in &parts.hashes {
                    gathered.hash(block.number, *number, *hash);
                }
                gathered.balances = parts.balances.clone();
            }
            for (offset, result) in parts.reads.iter().enumerate() {
                let index = range.start + offset;
                if result.success {
                    gathered.items[index] = Some(Item::Returned(result.returnData.clone()));
                } else if result.returnData.is_empty() {
                    unclear.push((index, offset));
                } else {
                    gathered.items[index] = Some(Item::Reverted(result.returnData.clone()));
                }
            }
        }
        if !unclear.is_empty() {
            if self.known(endpoint).hash_pin != Some(true) {
                self.learn(endpoint, |known| known.wants_hash_pin = true);
                return Err(Failure::Unsupported(anyhow!(
                    "{} reads failed with no data in Multicall3, which reports running out of gas so too, and this endpoint has not been shown to honour a hash pin to read them alone",
                    unclear.len()
                )));
            }
            for (index, place) in unclear {
                let alone =
                    single_read(&provider, &plan.reads[index], &block, &mut gathered).await?;
                if place > 0 && alone != Item::Reverted(Bytes::new()) {
                    self.learn(endpoint, |known| {
                        known.max_reads = Some(known.max_reads.unwrap_or(usize::MAX).min(place));
                    });
                }
                gathered.items[index] = Some(alone);
            }
        }
        let gas = self.known(endpoint).gas_at_entry;
        gathered.observed(endpoint, Path::Multicall3, block, true, wall, gas)
    }

    /// One `aggregate3` pinned to `block`: by hash, then by number, accepted
    /// only when its number, timestamp and parent hash are the block's.
    async fn multicall_at(
        &self,
        provider: &DynProvider,
        batch: &Batch,
        block: &Header,
        gathered: &mut Gathered,
    ) -> Result<super::multicall::Parts, Failure> {
        let request = TransactionRequest::default()
            .with_to(self.options.multicall3)
            .with_input(batch.encode())
            .with_gas_limit(self.policy.view_outer_gas);
        let mut elsewhere = None;
        for pin in [
            BlockId::hash_canonical(block.hash),
            BlockId::number(block.number),
        ] {
            gathered.requests += 1;
            match provider
                .call(request.clone())
                .block(pin)
                .await
                .map_err(CallError::from)
            {
                Ok(answer) => {
                    let parts = batch.decode(&answer).map_err(Failure::Trouble)?;
                    let parent = parts
                        .hashes
                        .iter()
                        .find(|(number, _)| *number + 1 == block.number)
                        .map(|(_, hash)| *hash);
                    if parts.number == block.number
                        && parts.timestamp == block.timestamp
                        && parent == Some(block.parent_hash)
                    {
                        return Ok(parts);
                    }
                    elsewhere = Some(parts.number);
                }
                Err(error) if error.too_big() => return Err(Failure::TooBig),
                Err(error) if error.out_of_gas() => return Err(Failure::OutOfGas),
                Err(CallError::Refused { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Failure::Trouble(match elsewhere {
            Some(number) => anyhow!(
                "Multicall3 ran at block {number} when block {} was asked for",
                block.number
            ),
            None => anyhow!(
                "the endpoint does not run Multicall3 at block {}",
                block.number
            ),
        }))
    }

    /// The last path: the header, then one call per read and one balance
    /// read per balance, each pinned to the block's hash, on an endpoint
    /// shown to answer hash pins as of that block.
    async fn by_individual(
        &mut self,
        endpoint: usize,
        plan: &Plan,
        at: At,
    ) -> Result<Observed, Failure> {
        let provider = self.endpoints[endpoint].clone();
        let mut gathered = Gathered::new(plan.reads.len());
        let block = self.block_for(&provider, at, &mut gathered).await?;
        let wall = wall_ms();
        for (index, read) in plan.reads.iter().enumerate() {
            gathered.items[index] =
                Some(single_read(&provider, read, &block, &mut gathered).await?);
        }
        let pin = BlockId::hash_canonical(block.hash);
        for of in &plan.balances {
            gathered.requests += 1;
            let balance = provider
                .get_balance(*of)
                .block_id(pin)
                .await
                .map_err(|error| Failure::Trouble(anyhow!(error).context("reading a balance")))?;
            gathered.balances.push(balance);
        }
        for number in &plan.hashes {
            if *number >= block.number || *number == 0 {
                continue;
            }
            gathered.requests += 1;
            let own = header(&provider, BlockNumberOrTag::Number(*number))
                .await
                .map_err(Failure::Trouble)?;
            gathered.hash(block.number, *number, own.hash);
        }
        gathered.observed(endpoint, Path::Individual, block, true, wall, None)
    }
}

/// One read pinned to `block`'s hash: what it returned, or its revert.
async fn single_read(
    provider: &DynProvider,
    read: &Read,
    block: &Header,
    gathered: &mut Gathered,
) -> Result<Item, Failure> {
    gathered.requests += 1;
    let request = TransactionRequest::default()
        .with_from(Address::ZERO)
        .with_to(read.target)
        .with_input(read.data.clone());
    match provider
        .call(request)
        .block(BlockId::hash_canonical(block.hash))
        .await
        .map_err(CallError::from)
    {
        Ok(data) => Ok(Item::Returned(data)),
        Err(CallError::Reverted(data)) => Ok(Item::Reverted(data)),
        Err(error) => Err(Failure::Trouble(
            anyhow!(error).context(format!("reading {} at block {}", read.target, block.number)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(count: usize, size: usize) -> Vec<Read> {
        (0..count)
            .map(|i| Read {
                target: Address::repeat_byte(i as u8),
                data: Bytes::from(vec![1u8; size]),
            })
            .collect()
    }

    fn limits() -> Limits {
        Limits {
            max_reads: 256,
            input_bytes: 262_144,
            output_bytes: 400_000,
            result_bytes: 1_024,
        }
    }

    #[test]
    fn each_call_of_a_split_plan_is_within_every_bound_and_every_read_is_in_one() {
        for (count, size, limits) in [
            (0, 4, limits()),
            (1, 4, limits()),
            (600, 36, limits()),
            (
                600,
                36,
                Limits {
                    max_reads: 7,
                    ..limits()
                },
            ),
            (
                300,
                2_000,
                Limits {
                    input_bytes: 20_000,
                    ..limits()
                },
            ),
            (
                300,
                4,
                Limits {
                    output_bytes: 65_536,
                    ..limits()
                },
            ),
        ] {
            let plan = reads(count, size);
            let chunks = split(&plan, 10, RECENT, 5, limits).unwrap();
            let mut next = 0;
            for (index, chunk) in chunks.iter().enumerate() {
                assert_eq!(chunk.start, next);
                next = chunk.end;
                let items: Vec<view_reads::Item> = plan[chunk.clone()].iter().map(item).collect();
                let (h, r, b) = if index == 0 {
                    (10, RECENT, 5)
                } else {
                    (0, 0, 0)
                };
                assert!(items.len() <= limits.max_reads);
                assert!(view_reads::input_size(&items, h, b) <= limits.input_bytes);
                assert!(
                    view_reads::output_bound(items.len(), h, r, b, limits.result_bytes)
                        <= limits.output_bytes
                );
                // Every call but the last is full: one more read would not fit.
                if chunk.end < plan.len() {
                    let more: Vec<view_reads::Item> =
                        plan[chunk.start..chunk.end + 1].iter().map(item).collect();
                    assert!(
                        more.len() > limits.max_reads
                            || view_reads::input_size(&more, h, b) > limits.input_bytes
                            || view_reads::output_bound(more.len(), h, r, b, limits.result_bytes)
                                > limits.output_bytes
                    );
                }
            }
            assert_eq!(next, plan.len());
        }
    }

    #[test]
    fn a_read_too_large_for_any_call_is_refused_not_dropped() {
        let plan = reads(3, 300_000);
        assert!(split(&plan, 0, 0, 0, limits()).is_err());
    }
}
