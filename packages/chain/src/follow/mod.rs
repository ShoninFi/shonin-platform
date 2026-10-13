//! The shared chain follower: one consistent read of everything a process
//! needs on a chain (an observation), the branch it is on (anchors), the
//! logs between observations (ranges), the chain's clock against the wall
//! clock, and what each endpoint can do (capabilities).
//!
//! - [`Follower::observe`] reads a [`Plan`] (contract reads, native
//!   balances, block hashes) at one block, from one endpoint, and returns
//!   it with the block it ran at and the hashes of the 256 blocks below. It
//!   uses the ViewReads helper under a state override where the endpoint
//!   runs it, Multicall3 where it does not, and single reads pinned to the
//!   block's hash where neither is available; every path checks the block
//!   an answer says it ran at, and a result from another block is never
//!   accepted.
//! - [`Follower::check`] compares an observation with the anchors held
//!   ([`Continuity`]), and notes its head in the [`ChainClock`].
//! - [`RangeReader`] reads logs in pages and checks them per page and per
//!   log-bearing block against hashes held or observed.
//!
//! An observation is a read, never permission: what a caller does with it
//! is the caller's decision, and a failed or doubtful observation leaves the
//! caller on its active path.

pub mod anchors;
pub mod capabilities;
pub mod clock;
pub mod identity;
pub mod multicall;
mod observe;
pub mod ranges;
pub mod view_reads;

use std::collections::BTreeMap;

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::DynProvider;
use alloy_sol_types::SolCall;
use anyhow::{Result, anyhow, bail};

pub use anchors::{Anchors, Continuity, Headers, Seen};
pub use capabilities::{BlockSource, Capabilities};
pub use clock::{ChainClock, Unbounded};
pub use ranges::{Pages, RangeRead, RangeReader, Watch};

use crate::Client;
use crate::rpc_policy::RpcPolicy;

/// One block's identity, as its header gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
}

/// Which block an observation is read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// Whatever block the endpoint is at; the observation says which.
    Latest,
    /// This block: an answer from any other is refused.
    Pinned(Header),
}

/// One read: a call to a view function, as its calldata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub target: Address,
    pub data: Bytes,
}

/// What one observation reads. Reads must be view functions: the helper runs
/// each as a staticcall, so one that writes fails there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub reads: Vec<Read>,
    /// Native balances, at most 256.
    pub balances: Vec<Address>,
    /// Block numbers whose hashes are wanted besides the 256 below the
    /// observed block (which every observation carries), at most 256.
    pub hashes: Vec<u64>,
}

impl Plan {
    /// Add a read; its index in [`Observed::items`].
    pub fn read<C: SolCall>(&mut self, target: Address, call: &C) -> usize {
        self.reads.push(Read {
            target,
            data: call.abi_encode().into(),
        });
        self.reads.len() - 1
    }

    /// Add a balance; its index in [`Observed::balances`].
    pub fn balance(&mut self, of: Address) -> usize {
        self.balances.push(of);
        self.balances.len() - 1
    }

    fn check(&self) -> Result<()> {
        if self.balances.len() > view_reads::MAX_BALANCES {
            bail!(
                "an observation reads at most {} balances, not {}",
                view_reads::MAX_BALANCES,
                self.balances.len()
            );
        }
        if self.hashes.len() > view_reads::MAX_HASHES {
            bail!(
                "an observation reads at most {} chosen hashes, not {}",
                view_reads::MAX_HASHES,
                self.hashes.len()
            );
        }
        Ok(())
    }
}

/// How an observation was read: the counters and this say what it cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// The ViewReads helper under a state override.
    ViewReads,
    /// Multicall3's `aggregate3`.
    Multicall3,
    /// One call per read, each pinned to the block's hash.
    Individual,
}

/// One read's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// What the function returned.
    Returned(Bytes),
    /// It reverted, with this data.
    Reverted(Bytes),
}

/// One observation: every read of a plan at one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// The endpoint that answered (its place in the client's `rpc` list).
    pub endpoint: usize,
    pub path: Path,
    pub number: u64,
    pub timestamp: u64,
    pub parent_hash: B256,
    /// The block's own hash, when it was pinned or read as a header.
    pub hash: Option<B256>,
    /// Nonzero hashes from the observed block's branch: the blocks below it
    /// (up to 256), those the plan asked for, and its own when known.
    pub hashes: BTreeMap<u64, B256>,
    /// Numbers inside the 256-block window that were answered with zero.
    pub zero: Vec<u64>,
    /// One per planned balance.
    pub balances: Vec<U256>,
    /// One per planned read.
    pub items: Vec<Item>,
    /// The wall clock (ms since the Unix epoch) when the answer arrived.
    pub wall_ms: u64,
    /// The gas the helper found available, when the helper ran.
    pub gas_at_entry: Option<u64>,
    /// How many requests the observation took.
    pub requests: u32,
}

impl Observed {
    /// What it says of the branch, for [`Anchors::check`].
    pub fn seen(&self) -> Seen {
        Seen {
            number: self.number,
            hashes: self.hashes.clone(),
            zero: self.zero.clone(),
        }
    }

    /// The block, when its own hash is known.
    pub fn header(&self) -> Option<Header> {
        Some(Header {
            number: self.number,
            hash: self.hash?,
            parent_hash: self.parent_hash,
            timestamp: self.timestamp,
        })
    }

    /// Read `index`'s answer as `C` returns it. A revert, or an answer that
    /// does not decode, is an error naming the function.
    pub fn decode<C: SolCall>(&self, index: usize) -> Result<C::Return> {
        match self.items.get(index) {
            Some(Item::Returned(data)) => C::abi_decode_returns(data)
                .map_err(|error| anyhow!("{} did not decode: {error}", C::SIGNATURE)),
            Some(Item::Reverted(data)) => bail!("{} reverted ({data})", C::SIGNATURE),
            None => bail!("the observation has no read {index}"),
        }
    }
}

/// Where Multicall3 is looked for, and the code it must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub multicall3: Address,
    pub multicall3_code_hash: B256,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            multicall3: multicall::MULTICALL3,
            multicall3_code_hash: multicall::MULTICALL3_CODE_HASH,
        }
    }
}

/// One chain's follower.
///
/// An observation costs at most `read_budget_ms` on each endpoint it asks,
/// so at most `read_budget_ms` times the number of endpoints in all, however
/// they answer or fail to: what an endpoint can do is found out by a check
/// run apart, which the observation waits for only within its budget
/// (`capabilities::check`). [`Follower::check`] adds at most one more
/// `read_budget_ms`, and only when the anchors need headers read.
pub struct Follower {
    endpoints: Vec<DynProvider>,
    capabilities: Vec<capabilities::Shared>,
    /// Each endpoint's check, while it runs.
    checks: Vec<Option<tokio::task::JoinHandle<()>>>,
    preferred: usize,
    policy: RpcPolicy,
    options: Options,
    client: Client,
    pub anchors: Anchors,
    pub clock: ChainClock,
}

/// One observation and how it stands.
#[derive(Clone, Debug)]
pub struct Step {
    /// `None` when no endpoint gave an observation.
    pub observed: Option<Observed>,
    pub continuity: Continuity,
    /// Whether the chain's clock is bounded after this observation; when it
    /// is not, the caller takes its active path.
    pub bounded: bool,
}

impl Follower {
    pub fn new(client: Client, policy: RpcPolicy) -> Self {
        Self::with_options(client, policy, Options::default())
    }

    pub fn with_options(client: Client, policy: RpcPolicy, options: Options) -> Self {
        let endpoints = client.each_endpoint();
        Self {
            capabilities: (0..endpoints.len()).map(|_| Default::default()).collect(),
            checks: (0..endpoints.len()).map(|_| None).collect(),
            endpoints,
            preferred: 0,
            anchors: Anchors::new(policy.max_reorg_blocks),
            clock: ChainClock::new(
                policy.head_lag_max_ms,
                policy.head_ahead_max_ms,
                policy.clock_settle_observations,
            ),
            policy,
            options,
            client,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn policy(&self) -> &RpcPolicy {
        &self.policy
    }

    /// How many endpoints it can ask.
    pub fn endpoints(&self) -> usize {
        self.endpoints.len()
    }

    /// What `endpoint` has been found to do, as of now.
    pub fn capabilities(&self, endpoint: usize) -> Option<Capabilities> {
        self.capabilities
            .get(endpoint)
            .map(|shared| capabilities::lock(shared).clone())
    }

    /// The endpoint asked first: the one that answered last.
    pub fn preferred(&self) -> usize {
        self.preferred
    }

    /// A header on `endpoint`, checked to be the block asked for.
    pub async fn header(&self, endpoint: usize, at: BlockNumberOrTag) -> Result<Header> {
        let provider = self
            .endpoints
            .get(endpoint)
            .ok_or_else(|| anyhow!("there is no endpoint {}", endpoint + 1))?;
        capabilities::header(provider, at).await
    }

    /// Compare `observed` with the anchors, moving them to its branch unless
    /// it is `Lagging` or `Unknown`, and note its head in the clock. Anchors
    /// it did not carry are read as headers on the endpoint that answered it,
    /// all within one `read_budget_ms` (past it they are `Unknown`).
    /// It is for observations of the head (`At::Latest`, or pinned to a
    /// header just read as the head): an older block would read as a
    /// lagging head and leave the clock unbounded.
    pub async fn check(&mut self, observed: &Observed) -> Continuity {
        self.clock.note(observed.timestamp, observed.wall_ms);
        let Some(provider) = self.endpoints.get(observed.endpoint).cloned() else {
            return Continuity::Unknown(format!(
                "the observation names endpoint {}, which there is not",
                observed.endpoint + 1
            ));
        };
        let mut headers = EndpointHeaders {
            provider,
            deadline: tokio::time::Instant::now() + self.budget(),
            budget_ms: self.policy.read_budget_ms,
        };
        self.anchors.check(&observed.seen(), &mut headers).await
    }

    /// Observe and check: an observation that failed is `Unknown`, and
    /// leaves the anchors as they were.
    pub async fn step(&mut self, plan: &Plan, at: At) -> Step {
        match self.observe(plan, at).await {
            Ok(observed) => {
                let continuity = self.check(&observed).await;
                Step {
                    observed: Some(observed),
                    continuity,
                    bounded: self.clock.bounded(),
                }
            }
            Err(error) => Step {
                observed: None,
                continuity: Continuity::Unknown(format!("{error:#}")),
                bounded: false,
            },
        }
    }

    /// Whether `endpoint` answers a read pinned `depth` blocks back as of
    /// that block, asked at most once an hour (and after a failure), each
    /// question within [`capabilities::CHECK_TIMEOUT`].
    pub async fn honours_past_blocks(&mut self, endpoint: usize, depth: u64) -> Option<bool> {
        let provider = self.endpoints.get(endpoint)?.clone();
        self.ready(
            endpoint,
            tokio::time::Instant::now() + capabilities::CHECK_TIMEOUT,
        )
        .await;
        let known = self.capabilities(endpoint)?;
        if let Some((honours, asked)) = known.past_blocks
            && asked == depth
        {
            return Some(honours);
        }
        let source = known.source.unwrap_or(BlockSource::Plain);
        let asked = tokio::time::timeout(
            capabilities::CHECK_TIMEOUT,
            capabilities::probe_past_blocks(&provider, source, depth),
        )
        .await;
        let mut known = capabilities::lock(&self.capabilities[endpoint]);
        match asked {
            Ok(Ok(honours)) => {
                known.past_blocks = Some((honours, depth));
                Some(honours)
            }
            _ => {
                known.failed_at = Some(tokio::time::Instant::now());
                None
            }
        }
    }

    fn budget(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.policy.read_budget_ms)
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        for check in self.checks.iter().flatten() {
            check.abort();
        }
    }
}

/// Headers read on one endpoint, until a deadline.
struct EndpointHeaders {
    provider: DynProvider,
    deadline: tokio::time::Instant,
    budget_ms: u64,
}

impl Headers for EndpointHeaders {
    async fn hash_of(&mut self, number: u64) -> Result<Option<B256>> {
        let read = tokio::time::timeout_at(
            self.deadline,
            capabilities::header(&self.provider, BlockNumberOrTag::Number(number)),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "the headers took longer than read_budget_ms={}",
                self.budget_ms
            )
        })?;
        match read {
            Ok(header) => Ok(Some(header.hash)),
            Err(error) if error.to_string().contains("has no block") => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The wall clock, in ms since the Unix epoch.
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
