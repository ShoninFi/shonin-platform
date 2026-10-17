//! Logs between observations, read by ranges.
//!
//! One `eth_getLogs` per page covers every watched address and topic
//! (split only past `log_batch_addresses` addresses), in pages of
//! `record_page_blocks`, at most `record_pages_per_refresh` a read, each
//! window narrowed as the endpoint requires (the client's learned width).
//! A read starts `record_overlap_blocks` below the blocks already read, and
//! what it finds there must be what was found before.
//!
//! Logs are checked, never trusted: each log's block hash must be the
//! followed branch's hash at its number (from the observation's window, the
//! anchors, or one header beyond them), and so must the last block of each
//! window of logs, as the endpoint that answered the window gives it just
//! after (one header there, whichever endpoint it is: the one the
//! observation came from may have moved to another branch since). A window
//! from an endpoint on another branch, which may leave out a log the
//! followed branch has, is refused that way. That is one comparison per
//! window and per log-bearing block, never one per block.

use std::collections::{BTreeMap, BTreeSet};

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, B256};
use alloy::rpc::types::{Filter, Log};
use anyhow::{Result, bail};

use super::{At, Continuity, Follower, Observed, Plan};
use crate::rpc_policy::RpcPolicy;

/// What a reader watches: every log of these addresses whose first topic
/// is one of `topics` (any first topic when empty).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    pub addresses: Vec<Address>,
    pub topics: Vec<B256>,
}

/// What one read gave.
#[derive(Clone, Debug, PartialEq)]
pub enum RangeRead {
    /// New logs (none read before), in block and log order, checked against
    /// the followed branch; every block through `through` has been read.
    Read {
        logs: Vec<Log>,
        through: u64,
        caught_up: bool,
    },
    /// The blocks above `fork_at` were replaced (an observation proved it,
    /// or the overlap re-read found other logs than before). The reader now
    /// reads again from `fork_at + 1`; the caller forgets what it took from
    /// the blocks above `fork_at`.
    Replaced { fork_at: u64 },
    /// The replacement went deeper than the anchors reach: the reader reads
    /// again from its first block; the caller forgets everything it read.
    Deep,
    /// Nothing was taken: the logs could not be checked against the
    /// followed branch (an endpoint behind or on another branch, a failed
    /// read, a lagging observation). The next read tries again.
    Unverified(String),
}

/// Pages read but not yet checked.
#[derive(Clone, Debug)]
pub struct Pages {
    /// The first block re-read (the overlap's start).
    pub from: u64,
    /// The first block not read before.
    pub start: u64,
    /// The last block read.
    pub to: u64,
    /// The last block of each window of logs, with its hash as the
    /// endpoint that answered the window gave it just after the logs.
    pub ends: Vec<(u64, B256)>,
    pub logs: Vec<Log>,
}

/// Reads one watch's logs forward, page by page.
#[derive(Clone, Debug)]
pub struct RangeReader {
    watch: Watch,
    first: u64,
    next: u64,
    /// For the newest `overlap` blocks read: each block's logs, as (block
    /// hash, log index). A block with none is absent.
    held: BTreeMap<u64, BTreeSet<(B256, u64)>>,
    /// The first block `held` speaks for: the overlap re-read starts no
    /// lower, so a block whose logs were not kept is never compared.
    held_from: u64,
    page_blocks: u64,
    pages_per_read: u64,
    overlap: u64,
    addresses_per_filter: usize,
}

impl RangeReader {
    /// A reader of `watch` from block `first`.
    pub fn new(watch: Watch, first: u64, policy: &RpcPolicy) -> Self {
        Self {
            watch,
            first,
            next: first,
            held: BTreeMap::new(),
            held_from: first,
            page_blocks: policy.record_page_blocks.max(1),
            pages_per_read: policy.record_pages_per_refresh.max(1),
            overlap: policy.record_overlap_blocks,
            addresses_per_filter: usize::try_from(policy.log_batch_addresses.max(1))
                .unwrap_or(usize::MAX),
        }
    }

    /// The first block not yet read.
    pub fn next_block(&self) -> u64 {
        self.next
    }

    /// Forget every block above `block`: they are read again.
    pub fn forget_above(&mut self, block: u64) {
        self.next = self.next.min(block.saturating_add(1)).max(self.first);
        self.held.retain(|number, _| *number <= block);
        self.held_from = self.held_from.min(self.next);
    }

    /// Forget everything: read again from the first block.
    pub fn restart(&mut self) {
        self.next = self.first;
        self.held.clear();
        self.held_from = self.first;
    }

    /// Read up to `to` (at most the configured pages), on `endpoint` first
    /// when it was just seen at `seen` (no head read for it), unchecked.
    pub async fn pages(
        &self,
        follower: &Follower,
        to: u64,
        seen: Option<(usize, u64)>,
    ) -> Result<Option<Pages>> {
        if to < self.next {
            return Ok(None);
        }
        let start = self.next;
        let from = start.saturating_sub(self.overlap).max(self.held_from);
        let last = start
            .saturating_add(self.page_blocks.saturating_mul(self.pages_per_read))
            .saturating_sub(1)
            .min(to);
        let mut logs = Vec::new();
        let mut ends = Vec::new();
        let mut page_from = from;
        while page_from <= last {
            let page_to = page_from
                .max(start)
                .saturating_add(self.page_blocks - 1)
                .min(last);
            for addresses in self.address_sets() {
                let mut filter = Filter::new()
                    .from_block(BlockNumberOrTag::Number(page_from))
                    .to_block(BlockNumberOrTag::Number(page_to))
                    .address(addresses);
                if !self.watch.topics.is_empty() {
                    filter = filter.event_signature(self.watch.topics.clone());
                }
                for window in follower.client().logs_by_window(&filter, seen).await? {
                    let Some(end_hash) = window.end_hash else {
                        bail!(
                            "the logs from block {} came without block {}'s hash",
                            window.from,
                            window.to
                        );
                    };
                    ends.push((window.to, end_hash));
                    logs.extend(window.logs);
                }
            }
            page_from = page_to + 1;
        }
        Ok(Some(Pages {
            from,
            start,
            to: last,
            ends,
            logs,
        }))
    }

    fn address_sets(&self) -> Vec<Vec<Address>> {
        if self.watch.addresses.is_empty() {
            return vec![Vec::new()];
        }
        self.watch
            .addresses
            .chunks(self.addresses_per_filter)
            .map(<[Address]>::to_vec)
            .collect()
    }

    /// Check `pages` against the followed branch, as `observed` (already
    /// checked by the caller as extending it) and the anchors give it, with
    /// headers on `observed`'s endpoint beyond both; then take them.
    pub async fn settle(
        &mut self,
        pages: Pages,
        follower: &mut Follower,
        observed: &Observed,
    ) -> RangeRead {
        if pages.start != self.next || pages.to < pages.start {
            return RangeRead::Unverified("the pages do not start where the reader is".into());
        }
        // Each log must be one of ours, in range, with its place known.
        let mut by_block: BTreeMap<u64, (B256, BTreeSet<(B256, u64)>)> = BTreeMap::new();
        let mut logs = Vec::new();
        let mut seen_logs = BTreeSet::new();
        for log in pages.logs {
            let (Some(number), Some(hash), Some(index)) =
                (log.block_number, log.block_hash, log.log_index)
            else {
                return RangeRead::Unverified("a log came without its block or place".into());
            };
            if log.removed
                || number < pages.from
                || number > pages.to
                || (!self.watch.addresses.is_empty()
                    && !self.watch.addresses.contains(&log.address()))
                || (!self.watch.topics.is_empty()
                    && !log
                        .topics()
                        .first()
                        .is_some_and(|topic| self.watch.topics.contains(topic)))
            {
                return RangeRead::Unverified(format!(
                    "a log at block {number} is not one the read asked for"
                ));
            }
            let entry = by_block.entry(number).or_insert((hash, BTreeSet::new()));
            if entry.0 != hash {
                return RangeRead::Unverified(format!(
                    "block {number}'s logs name two block hashes"
                ));
            }
            entry.1.insert((hash, index));
            if number >= pages.start && seen_logs.insert((hash, index)) {
                logs.push(log);
            }
        }
        // The hashes to compare: each log-bearing block's, and each window
        // end's as the endpoint that answered the window gave it.
        let mut wanted: BTreeMap<u64, BTreeSet<B256>> = by_block
            .iter()
            .map(|(n, (h, _))| (*n, BTreeSet::from([*h])))
            .collect();
        for (end, hash) in &pages.ends {
            if *end < pages.from || *end > pages.to {
                return RangeRead::Unverified(format!(
                    "a window of logs ends at block {end}, outside the read"
                ));
            }
            wanted.entry(*end).or_default().insert(*hash);
        }
        let mut proven = Vec::new();
        for (number, found) in wanted {
            let expected = match observed
                .hashes
                .get(&number)
                .copied()
                .or_else(|| follower.anchors.hash(number))
            {
                Some(hash) => hash,
                None if number < observed.number => {
                    match follower
                        .header(observed.endpoint, BlockNumberOrTag::Number(number))
                        .await
                    {
                        Ok(header) => header.hash,
                        Err(error) => {
                            return RangeRead::Unverified(format!(
                                "block {number}'s header: {error:#}"
                            ));
                        }
                    }
                }
                None => {
                    return RangeRead::Unverified(format!(
                        "block {number} is not below the observed block {}",
                        observed.number
                    ));
                }
            };
            if let Some(found) = found.iter().find(|found| **found != expected) {
                return RangeRead::Unverified(format!(
                    "the logs up to block {number} came from a branch with hash {found} there, the followed branch has {expected}"
                ));
            }
            proven.push((number, expected));
        }
        // The overlap must hold what it held before.
        for number in pages.from..pages.start {
            let now = by_block.get(&number).map(|(_, logs)| logs);
            let before = self.held.get(&number);
            if now != before {
                let fork_at = number.saturating_sub(1);
                self.forget_above(fork_at);
                return RangeRead::Replaced { fork_at };
            }
        }
        for (number, hash) in proven {
            follower.anchors.hold(number, hash);
        }
        for (number, (_, held)) in by_block.range(pages.start..) {
            self.held.insert(*number, held.clone());
        }
        let keep_from = pages
            .to
            .saturating_add(1)
            .saturating_sub(self.overlap)
            .max(self.first);
        self.held.retain(|number, _| *number >= keep_from);
        self.held_from = keep_from;
        self.next = pages.to + 1;
        logs.sort_by_key(|log| (log.block_number, log.log_index));
        RangeRead::Read {
            logs,
            through: pages.to,
            caught_up: pages.to >= observed.number.saturating_sub(1),
        }
    }

    /// One whole read: an observation at the head (which proves continuity
    /// and carries the hashes of the 256 blocks below it), the pages up to
    /// the block below it (or `to`, if lower), on the same endpoint, and the
    /// check.
    pub async fn read(&mut self, follower: &mut Follower, to: Option<u64>) -> RangeRead {
        let step = follower.step(&Plan::default(), At::Latest).await;
        let Some(observed) = step.observed else {
            return RangeRead::Unverified(format!("no observation: {:?}", step.continuity));
        };
        match step.continuity {
            Continuity::Extends => {}
            Continuity::Replaced { fork_at, .. } => {
                self.forget_above(fork_at);
                return RangeRead::Replaced { fork_at };
            }
            Continuity::Deep => {
                self.restart();
                return RangeRead::Deep;
            }
            other => return RangeRead::Unverified(format!("{other:?}")),
        }
        let below = observed.number.saturating_sub(1);
        let to = to.map_or(below, |to| to.min(below));
        let pages = match self
            .pages(follower, to, Some((observed.endpoint, observed.number)))
            .await
        {
            Ok(Some(pages)) => pages,
            Ok(None) => {
                return RangeRead::Read {
                    logs: Vec::new(),
                    through: self.next.saturating_sub(1),
                    caught_up: true,
                };
            }
            Err(error) => return RangeRead::Unverified(format!("reading logs: {error:#}")),
        };
        self.settle(pages, follower, &observed).await
    }
}
