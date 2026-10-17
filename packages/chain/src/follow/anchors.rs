//! Anchors: block hashes held as proven on the followed branch, and the
//! continuity of each new observation with them.
//!
//! An observation brings the hashes of the blocks below the one it ran at
//! (all 256 the EVM keeps, from the same execution), so continuity is proved
//! by comparing hashes, never by walking headers. An anchor the observation
//! did not carry (older than its window) is read as one header on the same
//! endpoint.
//!
//! The rules, in order:
//! - a zero hash inside the window, or any read that fails, is `Unknown`, and
//!   nothing held changes;
//! - an observation numbered below the followed head is `Lagging`, and so is
//!   one at the followed head that disagrees with an anchor: a lower or equal
//!   head is never a replacement;
//! - an observation above the followed head whose hash differs at an anchor
//!   is `Replaced`, at the highest anchor that still matches, or `Deep` when
//!   none held (at most `max_reorg_blocks` below the newest) matches.

use std::collections::BTreeMap;

use alloy_primitives::B256;

/// The hash of a block on the branch an observation came from, read as a
/// header on the endpoint that answered the observation.
pub trait Headers {
    /// The hash of block `number` on that endpoint's branch, or `None` when
    /// the endpoint says it has no such block.
    fn hash_of(
        &mut self,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<B256>>> + Send;
}

/// What one observation says about the branch it ran on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// The block it ran at.
    pub number: u64,
    /// Nonzero hashes it gave, by block number, all from its own branch: the
    /// blocks below it and, when the caller pinned a header, its own.
    pub hashes: BTreeMap<u64, B256>,
    /// Numbers inside its 256-block window that it answered with zero. A
    /// node answers zero there only when something is wrong.
    pub zero: Vec<u64>,
}

/// How an observation stands to the branch followed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Continuity {
    /// Every anchor it could be compared with matches: it is on the followed
    /// branch, at or above the followed head.
    Extends,
    /// It is numbered below the followed head, or at it with a hash that
    /// differs: an endpoint behind, or a sibling that is not (yet) longer.
    /// It is discarded and nothing held changes.
    Lagging { number: u64, head: u64 },
    /// It could not be compared: a read failed, or a hash inside its window
    /// was zero. Nothing held changes; the next observation decides.
    Unknown(String),
    /// It is numbered above the followed head and differs at an anchor. The
    /// blocks above `fork_at` were replaced; `depth` is how far `fork_at` is
    /// below the newest anchor held before.
    Replaced { fork_at: u64, depth: u64 },
    /// It is numbered above the followed head and no anchor held matches it:
    /// the replacement goes deeper than the anchors reach.
    Deep,
}

/// Block hashes proven on the followed branch, at most `max_reorg_blocks`
/// below the newest, and the followed head.
#[derive(Clone, Debug)]
pub struct Anchors {
    held: BTreeMap<u64, B256>,
    head: Option<u64>,
    max_reorg_blocks: u64,
}

impl Anchors {
    pub fn new(max_reorg_blocks: u64) -> Self {
        Self {
            held: BTreeMap::new(),
            head: None,
            max_reorg_blocks: max_reorg_blocks.max(1),
        }
    }

    /// The highest block an accepted observation ran at, or the highest
    /// anchor, whichever is higher.
    pub fn head(&self) -> Option<u64> {
        self.head
    }

    /// The newest anchor.
    pub fn newest(&self) -> Option<(u64, B256)> {
        self.held
            .last_key_value()
            .map(|(number, hash)| (*number, *hash))
    }

    /// The hash held for block `number`, if one is.
    pub fn hash(&self, number: u64) -> Option<B256> {
        self.held.get(&number).copied()
    }

    /// Every anchor, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = (u64, B256)> + '_ {
        self.held.iter().map(|(number, hash)| (*number, *hash))
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Hold `hash` for block `number`: proven on the followed branch by the
    /// caller (a log-bearing block or a page end checked against an
    /// observation). A number already held keeps its hash: a differing one
    /// is never written over silently, it is for [`Anchors::check`] to find.
    pub fn hold(&mut self, number: u64, hash: B256) {
        if hash == B256::ZERO {
            return;
        }
        self.held.entry(number).or_insert(hash);
        self.head = Some(self.head.map_or(number, |head| head.max(number)));
        self.prune();
    }

    /// Forget every anchor above `number`, after a replacement the caller
    /// learned of another way. The followed head goes back to `number`.
    pub fn forget_above(&mut self, number: u64) {
        self.held.retain(|held, _| *held <= number);
        self.head = self.head.map(|head| head.min(number));
    }

    /// Forget everything: the next observation is taken as it is.
    pub fn clear(&mut self) {
        self.held.clear();
        self.head = None;
    }

    fn prune(&mut self) {
        if let Some((newest, _)) = self.newest() {
            let oldest = newest.saturating_sub(self.max_reorg_blocks);
            self.held = self.held.split_off(&oldest);
        }
    }

    /// Take `seen`'s hashes as anchors and its number as the followed head.
    fn adopt(&mut self, seen: &Seen) {
        for (number, hash) in &seen.hashes {
            if *hash != B256::ZERO {
                self.held.insert(*number, *hash);
            }
        }
        self.head = Some(self.head.map_or(seen.number, |head| head.max(seen.number)));
        self.prune();
    }

    /// How `seen` stands to the anchors, and, unless it is `Lagging` or
    /// `Unknown`, the anchors moved to its branch: on `Extends` its hashes
    /// are added; on `Replaced` the anchors above the fork are dropped first;
    /// on `Deep` all are. Header reads go through `headers`, on the endpoint
    /// that answered `seen`, only for anchors `seen` did not carry; a failed
    /// read leaves everything as it was.
    pub async fn check<H: Headers>(&mut self, seen: &Seen, headers: &mut H) -> Continuity {
        if let Some(number) = seen.zero.first() {
            return Continuity::Unknown(format!(
                "block {number}, inside the observation's window, was answered with a zero hash"
            ));
        }
        let Some(head) = self.head else {
            self.adopt(seen);
            return Continuity::Extends;
        };
        if seen.number < head {
            return Continuity::Lagging {
                number: seen.number,
                head,
            };
        }
        // The anchor to compare first: the newest whose hash on the observed
        // branch can be had (carried, or below the observed block).
        let reference = self
            .held
            .keys()
            .rev()
            .copied()
            .find(|number| *number < seen.number || seen.hashes.contains_key(number));
        let Some(reference) = reference else {
            self.adopt(seen);
            return Continuity::Extends;
        };
        let mut observed = Observed {
            seen,
            headers,
            read: BTreeMap::new(),
        };
        let at_reference = match observed.hash(reference).await {
            Ok(hash) => hash,
            Err(error) => return Continuity::Unknown(error),
        };
        if Some(at_reference) == self.hash(reference) {
            // Agreeing at the newest anchor means agreeing below it, so a
            // carried hash below it that disagrees is not a branch: it is an
            // answer that contradicts itself.
            if let Some(number) = self.disagreeing(seen, reference) {
                return Continuity::Unknown(format!(
                    "the observation agrees at block {reference} and not at block {number}"
                ));
            }
            self.adopt(seen);
            return Continuity::Extends;
        }
        if seen.number <= head {
            return Continuity::Lagging {
                number: seen.number,
                head,
            };
        }
        let below: Vec<u64> = self.held.range(..reference).map(|(n, _)| *n).collect();
        let fork_at = match self.highest_agreeing(&below, &mut observed).await {
            Ok(fork_at) => fork_at,
            Err(error) => return Continuity::Unknown(error),
        };
        let Some(fork_at) = fork_at else {
            self.held.clear();
            self.head = None;
            self.adopt(seen);
            return Continuity::Deep;
        };
        // Every carried anchor at or below the fork must agree and every one
        // above it must not; otherwise the answers contradict each other.
        for (number, hash) in self.held.range(..=reference) {
            if let Some(carried) = seen.hashes.get(number)
                && (carried == hash) != (*number <= fork_at)
            {
                return Continuity::Unknown(format!(
                    "the observation's hashes put the fork at block {fork_at} and disagree at block {number}"
                ));
            }
        }
        let newest = self.newest().map_or(reference, |(number, _)| number);
        let depth = newest - fork_at;
        if depth > self.max_reorg_blocks {
            self.held.clear();
            self.head = None;
            self.adopt(seen);
            return Continuity::Deep;
        }
        self.forget_above(fork_at);
        self.adopt(seen);
        Continuity::Replaced { fork_at, depth }
    }

    /// A carried hash below `reference` that differs from the anchor held
    /// for its number.
    fn disagreeing(&self, seen: &Seen, reference: u64) -> Option<u64> {
        self.held
            .range(..reference)
            .find(|(number, hash)| seen.hashes.get(number).is_some_and(|seen| seen != *hash))
            .map(|(number, _)| *number)
    }

    /// The highest of `below` (ascending anchor numbers) whose held hash is
    /// the observed branch's. Carried hashes are compared directly from the
    /// top; the rest are searched by halves with header reads, since on one
    /// branch the anchors that agree are all below those that do not.
    async fn highest_agreeing<H: Headers>(
        &self,
        below: &[u64],
        observed: &mut Observed<'_, H>,
    ) -> Result<Option<u64>, String> {
        let mut top = below.len();
        while top > 0 && observed.seen.hashes.contains_key(&below[top - 1]) {
            let number = below[top - 1];
            if Some(observed.seen.hashes[&number]) == self.hash(number) {
                return Ok(Some(number));
            }
            top -= 1;
        }
        // below[..top]: the agreeing ones form a prefix. Find its end.
        let (mut low, mut high) = (0usize, top);
        while low < high {
            let middle = low + (high - low) / 2;
            let number = below[middle];
            if Some(observed.hash(number).await?) == self.hash(number) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low.checked_sub(1).map(|index| below[index]))
    }
}

/// The observed branch's hashes: carried by the observation, or read once
/// each as a header.
struct Observed<'a, H> {
    seen: &'a Seen,
    headers: &'a mut H,
    read: BTreeMap<u64, B256>,
}

impl<H: Headers> Observed<'_, H> {
    async fn hash(&mut self, number: u64) -> Result<B256, String> {
        if let Some(hash) = self.seen.hashes.get(&number) {
            return Ok(*hash);
        }
        if let Some(hash) = self.read.get(&number) {
            return Ok(*hash);
        }
        if number >= self.seen.number {
            return Err(format!(
                "block {number} is not below the observed block {}",
                self.seen.number
            ));
        }
        match self.headers.hash_of(number).await {
            Ok(Some(hash)) if hash != B256::ZERO => {
                self.read.insert(number, hash);
                Ok(hash)
            }
            Ok(_) => Err(format!(
                "the endpoint that answered the observation has no block {number}"
            )),
            Err(error) => Err(format!("reading block {number}'s header: {error:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two branches: `hash(branch, number)`, the same below `fork`.
    fn hash(branch: u8, number: u64, fork: u64) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&number.to_be_bytes());
        bytes[31] = if number <= fork { 1 } else { branch };
        B256::from(bytes)
    }

    /// Headers of one branch, counting reads, optionally failing.
    struct Branch {
        branch: u8,
        fork: u64,
        tip: u64,
        reads: Vec<u64>,
        fail: bool,
    }

    impl Headers for Branch {
        async fn hash_of(&mut self, number: u64) -> anyhow::Result<Option<B256>> {
            self.reads.push(number);
            if self.fail {
                anyhow::bail!("the endpoint limited the rate of requests");
            }
            Ok((number <= self.tip).then(|| hash(self.branch, number, self.fork)))
        }
    }

    fn branch(branch: u8, fork: u64, tip: u64) -> Branch {
        Branch {
            branch,
            fork,
            tip,
            reads: Vec::new(),
            fail: false,
        }
    }

    /// An observation at `number` on `branch`, carrying the 256 hashes below.
    fn seen(branch: u8, number: u64, fork: u64) -> Seen {
        Seen {
            number,
            hashes: (number.saturating_sub(256)..number)
                .filter(|n| *n > 0)
                .map(|n| (n, hash(branch, n, fork)))
                .collect(),
            zero: Vec::new(),
        }
    }

    async fn followed(to: u64) -> Anchors {
        let mut anchors = Anchors::new(256);
        let mut headers = branch(1, u64::MAX, to);
        assert_eq!(
            anchors
                .check(&seen(1, to - 1, u64::MAX), &mut headers)
                .await,
            Continuity::Extends
        );
        assert_eq!(
            anchors.check(&seen(1, to, u64::MAX), &mut headers).await,
            Continuity::Extends
        );
        assert!(headers.reads.is_empty());
        anchors
    }

    #[tokio::test]
    async fn an_observation_on_the_same_branch_extends_without_a_header_read() {
        let mut anchors = followed(600).await;
        let mut headers = branch(1, u64::MAX, 700);
        assert_eq!(
            anchors.check(&seen(1, 640, u64::MAX), &mut headers).await,
            Continuity::Extends
        );
        assert!(headers.reads.is_empty());
        assert_eq!(anchors.head(), Some(640));
        assert_eq!(anchors.newest().unwrap().0, 639);
        assert_eq!(anchors.iter().next().unwrap().0, 639 - 256);
    }

    #[tokio::test]
    async fn a_lower_head_is_lagging_whatever_its_hashes() {
        for other in [1u8, 2] {
            let mut anchors = followed(600).await;
            let before = anchors.held.clone();
            let mut headers = branch(other, 500, 599);
            assert_eq!(
                anchors.check(&seen(other, 599, 500), &mut headers).await,
                Continuity::Lagging {
                    number: 599,
                    head: 600
                }
            );
            assert_eq!(anchors.held, before);
            assert_eq!(anchors.head(), Some(600));
        }
    }

    #[tokio::test]
    async fn an_equal_head_on_another_branch_is_lagging_not_replaced() {
        let mut anchors = followed(600).await;
        let before = anchors.held.clone();
        let mut headers = branch(2, 590, 600);
        assert_eq!(
            anchors.check(&seen(2, 600, 590), &mut headers).await,
            Continuity::Lagging {
                number: 600,
                head: 600
            }
        );
        assert_eq!(anchors.held, before);
    }

    #[tokio::test]
    async fn a_zero_hash_or_a_failed_read_is_unknown_and_changes_nothing() {
        let mut anchors = followed(600).await;
        let before = anchors.held.clone();
        let mut zero = seen(2, 610, 590);
        zero.zero.push(605);
        let mut headers = branch(2, 590, 610);
        assert!(matches!(
            anchors.check(&zero, &mut headers).await,
            Continuity::Unknown(_)
        ));
        // Far above: the newest anchor needs a header, and the read fails.
        let mut failing = branch(2, 590, 1_000);
        failing.fail = true;
        assert!(matches!(
            anchors.check(&seen(2, 1_000, 590), &mut failing).await,
            Continuity::Unknown(_)
        ));
        // The endpoint has no such block.
        let mut short = branch(2, 590, 100);
        assert!(matches!(
            anchors.check(&seen(2, 1_000, 590), &mut short).await,
            Continuity::Unknown(_)
        ));
        assert_eq!(anchors.held, before);
        assert_eq!(anchors.head(), Some(600));
    }

    #[tokio::test]
    async fn the_fork_point_is_exact_for_every_depth_the_anchors_reach_and_deep_beyond() {
        // Anchors held: 343..=599, newest 599.
        for depth in [1u64, 2, 17, 255, 256, 257, 300] {
            for above in [1u64, 40, 300, 2_000] {
                let mut anchors = followed(600).await;
                let newest = anchors.newest().unwrap().0;
                let fork = newest - depth;
                let number = 600 + above;
                let mut headers = branch(2, fork, number);
                let found = anchors.check(&seen(2, number, fork), &mut headers).await;
                if depth <= 256 {
                    assert_eq!(
                        found,
                        Continuity::Replaced {
                            fork_at: fork,
                            depth
                        },
                        "depth {depth}, {above} above"
                    );
                    // Header reads only where the window does not reach, and
                    // then by halves, never one per block.
                    assert!(headers.reads.len() <= 10, "{:?}", headers.reads);
                } else {
                    assert_eq!(found, Continuity::Deep, "depth {depth}, {above} above");
                }
                // The anchors now follow the new branch.
                assert_eq!(anchors.head(), Some(number));
                assert_eq!(anchors.hash(number - 1), Some(hash(2, number - 1, fork)));
                // The fork is still held while within the bound of the
                // newest anchor.
                if depth <= 256 && fork + 256 >= number - 1 {
                    assert_eq!(anchors.hash(fork), Some(hash(1, fork, fork)));
                }
            }
        }
    }

    #[tokio::test]
    async fn a_gap_longer_than_the_window_costs_one_header_and_is_not_a_replacement() {
        let mut anchors = followed(600).await;
        let mut headers = branch(1, u64::MAX, 5_000);
        assert_eq!(
            anchors.check(&seen(1, 5_000, u64::MAX), &mut headers).await,
            Continuity::Extends
        );
        assert_eq!(headers.reads, vec![599]);
    }

    #[tokio::test]
    async fn answers_that_contradict_themselves_are_unknown() {
        let mut anchors = followed(600).await;
        let before = anchors.held.clone();
        // Agrees at the newest anchor, disagrees below it.
        let mut odd = seen(1, 620, u64::MAX);
        odd.hashes.insert(500, B256::repeat_byte(9));
        let mut headers = branch(1, u64::MAX, 620);
        assert!(matches!(
            anchors.check(&odd, &mut headers).await,
            Continuity::Unknown(_)
        ));
        // Disagrees at the newest, agrees above a disagreeing one below.
        let mut odd = seen(2, 620, 550);
        odd.hashes.insert(400, B256::repeat_byte(9));
        assert!(matches!(
            anchors.check(&odd, &mut headers).await,
            Continuity::Unknown(_)
        ));
        assert_eq!(anchors.held, before);
    }

    #[tokio::test]
    async fn a_held_anchor_is_never_written_over_and_forgetting_moves_the_head_back() {
        let mut anchors = followed(600).await;
        let held = anchors.hash(590).unwrap();
        anchors.hold(590, B256::repeat_byte(7));
        assert_eq!(anchors.hash(590), Some(held));
        anchors.hold(600, B256::repeat_byte(7));
        assert_eq!(anchors.newest().unwrap().0, 600);
        anchors.forget_above(550);
        assert_eq!(anchors.newest().unwrap().0, 550);
        assert_eq!(anchors.head(), Some(550));
    }

    #[tokio::test]
    async fn a_smaller_reorganisation_bound_calls_deeper_forks_deep() {
        let mut anchors = Anchors::new(16);
        let mut headers = branch(1, u64::MAX, 600);
        anchors.check(&seen(1, 600, u64::MAX), &mut headers).await;
        assert_eq!(anchors.len(), 17);
        let newest = anchors.newest().unwrap().0;
        for (depth, deep) in [(16u64, false), (17, true)] {
            let mut anchors = anchors.clone();
            let fork = newest - depth;
            let mut other = branch(2, fork, 700);
            let found = anchors.check(&seen(2, 700, fork), &mut other).await;
            if deep {
                assert_eq!(found, Continuity::Deep);
            } else {
                assert_eq!(
                    found,
                    Continuity::Replaced {
                        fork_at: fork,
                        depth
                    }
                );
            }
        }
    }
}
