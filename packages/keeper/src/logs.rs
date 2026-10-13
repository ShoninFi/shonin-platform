//! The logs the keeper's decisions read, read for a whole chain at once.
//!
//! A vault's `ProRataTaken` (who holds units) and `StateChanged` (when it
//! was retired), and a station's `RevokeSkipped` and `KeyRevoked` (keys a
//! skipped revoke left approved) are read by one range reader over every
//! vault and station on the chain, page by page, each page checked against
//! the followed branch (see `chain::follow::ranges`). A wake's decisions
//! read them from here (see [`crate::observed::Observed::logs`]) up to the
//! block the reader has checked; a log the reader has not read (an address
//! watched only lately, before it was watched) is read of the chain itself.
//!
//! The reader reads on when a look shows something those logs record may
//! have changed (a vault's units, state or keys), after the keeper's own
//! sends, while it is catching up, and at least every `keeper_log_scan_ms`.
//! Until it has caught up with the head once, decisions read their logs of
//! the chain itself, as before.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use alloy::sol_types::SolEvent;
use chain::bindings::station::IStation;
use chain::bindings::vault::IVault;
use chain::follow::{self, Follower, RangeRead, RangeReader, Watch};
use chain::rpc_policy::RpcPolicy;

use crate::observed::Logs;

/// The first topics of the logs the keeper's decisions read.
pub fn topics() -> Vec<B256> {
    vec![
        IVault::ProRataTaken::SIGNATURE_HASH,
        IVault::StateChanged::SIGNATURE_HASH,
        IStation::RevokeSkipped::SIGNATURE_HASH,
        IStation::KeyRevoked::SIGNATURE_HASH,
    ]
}

/// What became of one read on.
#[derive(Debug, PartialEq)]
pub enum Scanned {
    /// Nothing was due.
    Idle,
    /// Read and checked through the reader's newest block.
    Read,
    /// The blocks above `fork_at` were replaced: everything read of the
    /// chain is read again.
    Replaced { fork_at: u64 },
    /// Replaced deeper than the anchors reach.
    Deep,
    /// Nothing was taken (an endpoint behind or on another branch, a failed
    /// read): it is read again at the next look.
    Unverified(String),
}

/// The chain's log reader.
pub struct LogReader {
    first: u64,
    policy: RpcPolicy,
    reader: Option<RangeReader>,
    /// The addresses the reader watches, sorted.
    watched: Vec<Address>,
    logs: Logs,
    /// Whether it has caught up with a look's head once.
    caught_up: bool,
    last_read: Option<Instant>,
    /// Whether the next look reads on, whatever the backstop.
    wanted: bool,
    /// Since when every read on has been left unchecked, while they are.
    unverified_since: Option<Instant>,
}

impl LogReader {
    /// A reader of the chain's logs from `first` (the deployment's block).
    pub fn new(first: u64, policy: &RpcPolicy) -> Self {
        Self {
            first,
            policy: policy.clone(),
            reader: None,
            watched: Vec::new(),
            logs: Logs {
                through: first.saturating_sub(1),
                topics: topics(),
                ..Logs::default()
            },
            caught_up: false,
            last_read: None,
            wanted: true,
            unverified_since: None,
        }
    }

    /// Watch `addresses` (every vault and station on the chain). One newly
    /// watched is read from the reader's place on; its earlier logs are read
    /// of the chain itself when a decision asks for them. The new reader
    /// starts the overlap below where the last one stopped, so what it reads
    /// first is checked against the followed branch again (a log the last
    /// read was not given there is found).
    pub fn watch(&mut self, mut addresses: Vec<Address>) {
        addresses.sort();
        addresses.dedup();
        if addresses == self.watched && self.reader.is_some() {
            return;
        }
        // From the overlap below the last block checked: however often the
        // watch changes while pages are refused, never further back.
        let next = if self.reader.is_some() {
            self.logs
                .through
                .saturating_add(1)
                .saturating_sub(self.policy.record_overlap_blocks)
                .max(self.first)
        } else {
            self.first
        };
        for address in &addresses {
            self.logs.from.entry(*address).or_insert(next);
        }
        self.logs
            .from
            .retain(|address, _| addresses.contains(address));
        self.watched = addresses;
        self.reader = self.reader_from(next);
        self.wanted = true;
    }

    /// Read on at the next look.
    pub fn want(&mut self) {
        self.wanted = true;
    }

    fn due(&self, now: Instant) -> bool {
        self.wanted
            || !self.caught_up
            || self.last_read.is_none_or(|last| {
                now.saturating_duration_since(last)
                    >= Duration::from_millis(self.policy.keeper_log_scan_ms)
            })
    }

    /// Read on up to the block below `observed` (a look just checked as
    /// extending the followed branch), if due.
    pub async fn scan(
        &mut self,
        follower: &mut Follower,
        observed: &follow::Observed,
        now: Instant,
    ) -> Scanned {
        if !self.due(now) {
            return Scanned::Idle;
        }
        let Some(reader) = self.reader.as_mut() else {
            return Scanned::Idle;
        };
        let to = observed.number.saturating_sub(1);
        let pages = match reader
            .pages(follower, to, Some((observed.endpoint, observed.number)))
            .await
        {
            Ok(Some(pages)) => pages,
            Ok(None) => {
                let through = reader.next_block().saturating_sub(1);
                self.read_through(through, true, now);
                return Scanned::Read;
            }
            Err(error) => {
                self.unverified(now);
                return Scanned::Unverified(format!("reading logs: {error:#}"));
            }
        };
        match reader.settle(pages, follower, observed).await {
            RangeRead::Read {
                logs,
                through,
                caught_up,
            } => {
                tracing::debug!(
                    through,
                    caught_up,
                    found = logs.len(),
                    "logs read and checked"
                );
                // A block read again (the overlap of a new reader) gives
                // its logs again: each is kept once.
                for log in logs {
                    let place = (log.block_hash, log.log_index);
                    if !self
                        .logs
                        .logs
                        .iter()
                        .any(|kept| (kept.block_hash, kept.log_index) == place)
                    {
                        self.logs.logs.push(log);
                    }
                }
                self.read_through(through, caught_up, now);
                Scanned::Read
            }
            RangeRead::Replaced { fork_at } => {
                self.forget_above(fork_at);
                Scanned::Replaced { fork_at }
            }
            RangeRead::Deep => {
                self.restart();
                Scanned::Deep
            }
            RangeRead::Unverified(why) => {
                self.unverified(now);
                Scanned::Unverified(why)
            }
        }
    }

    fn unverified(&mut self, now: Instant) {
        self.wanted = true;
        self.unverified_since.get_or_insert(now);
    }

    /// Whether reads on have been left unchecked for longer than the
    /// backstop: decisions then read their logs of the chain itself, as
    /// before, rather than wait on a reader that can't check its pages.
    pub fn stuck(&self, now: Instant) -> bool {
        self.unverified_since.is_some_and(|since| {
            now.saturating_duration_since(since)
                >= Duration::from_millis(self.policy.keeper_log_scan_ms)
        })
    }

    fn read_through(&mut self, through: u64, caught_up: bool, now: Instant) {
        self.unverified_since = None;
        self.logs.through = through;
        self.caught_up |= caught_up;
        self.wanted = !caught_up;
        self.last_read = Some(now);
    }

    /// The blocks above `fork_at` were replaced: what was read of them is
    /// read again, for every address watched.
    pub fn forget_above(&mut self, fork_at: u64) {
        let next = fork_at.saturating_add(1).max(self.first);
        self.logs
            .logs
            .retain(|log| log.block_number.is_some_and(|number| number < next));
        self.logs.through = self.logs.through.min(next - 1);
        for from in self.logs.from.values_mut() {
            *from = (*from).min(next);
        }
        self.reader = self.reader_from(next);
        self.wanted = true;
    }

    /// Everything is read again from the deployment's block.
    pub fn restart(&mut self) {
        self.logs.logs.clear();
        self.logs.through = self.first.saturating_sub(1);
        for from in self.logs.from.values_mut() {
            *from = self.first;
        }
        self.caught_up = false;
        self.reader = self.reader_from(self.first);
        self.wanted = true;
    }

    fn reader_from(&self, next: u64) -> Option<RangeReader> {
        (!self.watched.is_empty()).then(|| {
            RangeReader::new(
                Watch {
                    addresses: self.watched.clone(),
                    topics: topics(),
                },
                next,
                &self.policy,
            )
        })
    }

    /// What decisions read their logs from, once the reader has caught up
    /// with the head and while it isn't stuck (see [`LogReader::stuck`]).
    pub fn view(&self) -> Option<Arc<Logs>> {
        (self.caught_up && !self.stuck(Instant::now())).then(|| {
            Arc::new(Logs {
                through: self.logs.through,
                from: self.logs.from.clone(),
                topics: self.logs.topics.clone(),
                logs: self.logs.logs.clone(),
            })
        })
    }
}

/// What is watched, and where each watched address's logs start, for the
/// tests.
#[cfg(test)]
impl LogReader {
    pub fn watched(&self) -> &[Address] {
        &self.watched
    }

    pub fn from(&self) -> &std::collections::HashMap<Address, u64> {
        &self.logs.from
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_watched_later_is_read_from_the_reader_s_next_block() {
        let policy = RpcPolicy::default();
        let mut reader = LogReader::new(100, &policy);
        let one = Address::repeat_byte(1);
        reader.watch(vec![one]);
        assert_eq!(reader.from()[&one], 100);
        reader.forget_above(150);
        let two = Address::repeat_byte(2);
        reader.watch(vec![two, one]);
        assert_eq!(reader.from()[&one], 100);
        assert_eq!(reader.watched(), &[one, two]);
    }

    #[test]
    fn decisions_read_logs_of_the_chain_itself_until_the_reader_has_caught_up() {
        let policy = RpcPolicy::default();
        let mut reader = LogReader::new(100, &policy);
        reader.watch(vec![Address::repeat_byte(1)]);
        assert!(reader.view().is_none());
        reader.read_through(5_000, false, Instant::now());
        assert!(reader.view().is_none());
        reader.read_through(9_000, true, Instant::now());
        assert_eq!(reader.view().map(|view| view.through), Some(9_000));
    }

    #[test]
    fn a_replacement_forgets_the_logs_above_it_and_reads_on_at_the_next_look() {
        let policy = RpcPolicy::default();
        let mut reader = LogReader::new(100, &policy);
        let one = Address::repeat_byte(1);
        reader.watch(vec![one]);
        for block in [200, 120] {
            reader.logs.logs.push(alloy::rpc::types::Log {
                block_number: Some(block),
                ..Default::default()
            });
        }
        reader.read_through(300, true, Instant::now());
        assert!(!reader.due(Instant::now()));
        reader.forget_above(150);
        assert_eq!(reader.logs.logs.len(), 1);
        assert_eq!(reader.logs.through, 150);
        assert!(reader.due(Instant::now()));
    }

    #[test]
    fn an_unchecked_page_is_never_taken_and_a_reader_that_cannot_check_any_is_set_aside() {
        let policy = RpcPolicy {
            keeper_log_scan_ms: 60_000,
            ..RpcPolicy::default()
        };
        let mut reader = LogReader::new(100, &policy);
        reader.watch(vec![Address::repeat_byte(1)]);
        let then = Instant::now();
        reader.read_through(300, true, then);
        reader.unverified(then);
        assert_eq!(reader.view().map(|view| view.through), Some(300));
        assert!(reader.due(then));
        assert!(!reader.stuck(then + Duration::from_secs(59)));
        assert!(reader.stuck(then + Duration::from_secs(60)));
        reader.read_through(320, true, then + Duration::from_secs(61));
        assert!(!reader.stuck(then + Duration::from_secs(62)));
    }

    #[test]
    fn a_reader_made_for_a_new_watch_checks_the_overlap_below_where_the_last_stopped() {
        let policy = RpcPolicy {
            record_overlap_blocks: 32,
            ..RpcPolicy::default()
        };
        let mut reader = LogReader::new(100, &policy);
        let one = Address::repeat_byte(1);
        reader.watch(vec![one]);
        reader.read_through(200, true, Instant::now());
        let next = |reader: &LogReader| reader.reader.as_ref().map(RangeReader::next_block);
        reader.watch(vec![one, Address::repeat_byte(2)]);
        assert_eq!(next(&reader), Some(169));
        assert_eq!(reader.from()[&Address::repeat_byte(2)], 169);
        assert_eq!(reader.from()[&one], 100);
        // Changed again before anything more is checked: no further back.
        reader.watch(vec![one, Address::repeat_byte(2), Address::repeat_byte(3)]);
        assert_eq!(next(&reader), Some(169));
    }

    #[test]
    fn the_reader_reads_on_at_least_every_backstop() {
        let policy = RpcPolicy {
            keeper_log_scan_ms: 60_000,
            ..RpcPolicy::default()
        };
        let mut reader = LogReader::new(100, &policy);
        reader.watch(vec![Address::repeat_byte(1)]);
        let then = Instant::now();
        reader.read_through(300, true, then);
        assert!(!reader.due(then + Duration::from_secs(59)));
        assert!(reader.due(then + Duration::from_secs(60)));
    }
}
