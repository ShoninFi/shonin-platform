//! Reads answered from one observation of a cluster instead of the node.
//!
//! A service that looks at a cluster in one read of every account it needs
//! (see [`super::observe`]) hands its decision code a client whose reads at
//! the confirmed commitment are answered from that read ([`Served`]), as of
//! its slot:
//!
//! - an account the observation read is answered from it;
//! - a listing of a program's accounts (`getProgramAccounts`) is answered
//!   from it only where the observation holds a whole listing of that kind
//!   ([`Listing`]): every such account its counts say exists, each read
//!   again at its slot, filtered here exactly as the node would filter;
//! - the cluster's clock only for a moment after the observation
//!   ([`Served::clock_fresh_for`]): a decision made later reads the time of
//!   the node;
//! - anything else (a simulation, a send, an account the observation didn't
//!   read, a read at another commitment) goes to the node, as before, and an
//!   account so read is noted ([`Learned`]), so the next observation holds
//!   it.
//!
//! Once a transaction may have gone out through it, every read for the rest
//! of the observation's life goes to the node: what a send changed is never
//! answered from the read made before it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::Pubkey;
use super::rpc::Account;

/// How long after an observation its clock is still taken as the
/// cluster's: longer than a look that sends nothing takes, short enough that
/// a decision made after a slow simulation reads the time again.
pub const CLOCK_FRESH: Duration = Duration::from_millis(500);

/// The clock sysvar's address.
pub fn clock_address() -> Pubkey {
    anchor_lang::solana_program::sysvar::clock::ID
}

/// Every account of one kind a program holds, as far as the observation
/// knows: whoever made it listed them, and its counts said none is missing.
#[derive(Clone, Debug)]
pub struct Listing {
    pub program: Pubkey,
    /// The filters the listing was made with: a listing asked for with these
    /// and possibly more is answered from it.
    pub filters: Vec<(usize, Vec<u8>)>,
    pub addresses: Vec<Pubkey>,
}

/// The accounts the decision code has read of the node, and when each was
/// last wanted: what the next observation reads.
#[derive(Clone, Debug, Default)]
pub struct Learned(Arc<Mutex<HashMap<Pubkey, Instant>>>);

impl Learned {
    /// `addresses` were wanted now.
    pub fn note(&self, addresses: &[Pubkey]) {
        let now = Instant::now();
        if let Ok(mut learned) = self.0.lock() {
            for address in addresses {
                learned.insert(*address, now);
            }
        }
    }

    /// Every address wanted within `keep` of `now`, the others forgotten.
    pub fn plan(&self, now: Instant, keep: Duration) -> Vec<Pubkey> {
        let Ok(mut learned) = self.0.lock() else {
            return Vec::new();
        };
        learned.retain(|_, at| now.saturating_duration_since(*at) < keep);
        let mut plan: Vec<Pubkey> = learned.keys().copied().collect();
        plan.sort_unstable();
        plan
    }

    /// Forget everything.
    pub fn clear(&self) {
        if let Ok(mut learned) = self.0.lock() {
            learned.clear();
        }
    }
}

/// One observation, as reads are answered from it.
#[derive(Debug)]
pub struct Served {
    /// The slot the observation was answered at.
    pub slot: u64,
    /// When its answer arrived, on this process's clock.
    pub taken: Instant,
    accounts: HashMap<Pubkey, Option<Account>>,
    listings: Vec<Listing>,
    /// Whether reads are answered at all: a view that only learns what is
    /// read (the path that reads the node itself) answers none.
    serving: bool,
    clock_fresh_for: Duration,
    /// Set once a transaction may have gone out: every read is the node's
    /// from then on.
    gone_out: AtomicBool,
    sends: AtomicU64,
    /// Reads answered here, and reads made of the node, for the tests and
    /// the debug log.
    answered: AtomicU64,
    asked: AtomicU64,
    learned: Learned,
}

impl Served {
    /// Reads answered from `accounts` (every account the observation read,
    /// `None` where none is) and `listings`, as of `slot`.
    pub fn new(
        slot: u64,
        taken: Instant,
        accounts: HashMap<Pubkey, Option<Account>>,
        listings: Vec<Listing>,
        learned: Learned,
    ) -> Self {
        Self {
            slot,
            taken,
            accounts,
            listings,
            serving: true,
            clock_fresh_for: CLOCK_FRESH,
            gone_out: AtomicBool::new(false),
            sends: AtomicU64::new(0),
            answered: AtomicU64::new(0),
            asked: AtomicU64::new(0),
            learned,
        }
    }

    /// A view that answers nothing and notes every account read of the
    /// node, so the next observation holds it.
    pub fn learning(learned: Learned) -> Self {
        let mut served = Self::new(0, Instant::now(), HashMap::new(), Vec::new(), learned);
        served.serving = false;
        served
    }

    /// The same, with the clock taken as the cluster's for `fresh` after the
    /// observation.
    pub fn clock_fresh_for(mut self, fresh: Duration) -> Self {
        self.clock_fresh_for = fresh;
        self
    }

    /// Whether reads are answered from the observation now.
    pub fn serving(&self) -> bool {
        self.serving && !self.gone_out.load(Ordering::SeqCst)
    }

    /// How many transactions may have gone out through it.
    pub fn sends(&self) -> u64 {
        self.sends.load(Ordering::SeqCst)
    }

    /// Reads answered from the observation, and reads made of the node
    /// while it was in use.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.answered.load(Ordering::SeqCst),
            self.asked.load(Ordering::SeqCst),
        )
    }

    /// A transaction may have gone out: the node answers every read from
    /// now on.
    pub fn went_out(&self) {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.gone_out.store(true, Ordering::SeqCst);
    }

    /// Whether the observation read `address`, whatever it found there.
    pub fn holds(&self, address: &Pubkey) -> bool {
        self.accounts.contains_key(address)
    }

    /// The account at `address`, if it is answered here (`Some(None)`: the
    /// observation found none there).
    pub fn account(&self, address: &Pubkey) -> Option<Option<Account>> {
        if !self.serving() {
            return None;
        }
        if *address == clock_address() && self.taken.elapsed() > self.clock_fresh_for {
            return None;
        }
        let found = self.accounts.get(address).cloned();
        if found.is_some() {
            self.answered.fetch_add(1, Ordering::SeqCst);
            self.learned.note(&[*address]);
        }
        found
    }

    /// Every account at `addresses`, if each is answered here.
    pub fn accounts(&self, addresses: &[Pubkey]) -> Option<Vec<Option<Account>>> {
        if !self.serving() {
            return None;
        }
        let mut out = Vec::with_capacity(addresses.len());
        for address in addresses {
            if *address == clock_address() && self.taken.elapsed() > self.clock_fresh_for {
                return None;
            }
            out.push(self.accounts.get(address)?.clone());
        }
        self.answered.fetch_add(1, Ordering::SeqCst);
        self.learned.note(addresses);
        Some(out)
    }

    /// Every account `program` owns whose data holds each of `filters`, if
    /// a whole listing here covers them: one made with a subset of the same
    /// filters, every account of which was read.
    pub fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Option<Vec<(Pubkey, Account)>> {
        if !self.serving() {
            return None;
        }
        let listing = self.listings.iter().find(|listing| {
            listing.program == *program
                && listing
                    .filters
                    .iter()
                    .all(|wanted| filters.contains(wanted))
                && listing
                    .addresses
                    .iter()
                    .all(|address| self.accounts.contains_key(address))
        })?;
        let found = listing
            .addresses
            .iter()
            .filter_map(|address| {
                let account = self.accounts.get(address)?.as_ref()?;
                (account.owner == *program && matches_filters(&account.data, filters))
                    .then(|| (*address, account.clone()))
            })
            .collect();
        self.answered.fetch_add(1, Ordering::SeqCst);
        Some(found)
    }

    /// `addresses` were read of the node while this view was in use.
    pub fn learn(&self, addresses: &[Pubkey]) {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.learned.note(addresses);
    }
}

/// Whether `data` holds each `(offset, bytes)`, as the node's `memcmp`
/// filter compares them.
pub fn matches_filters(data: &[u8], filters: &[(usize, Vec<u8>)]) -> bool {
    filters.iter().all(|(offset, bytes)| {
        offset
            .checked_add(bytes.len())
            .and_then(|end| data.get(*offset..end))
            .is_some_and(|there| there == bytes.as_slice())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(owner: Pubkey, data: &[u8]) -> Option<Account> {
        Some(Account {
            lamports: 7,
            owner,
            executable: false,
            data: data.to_vec(),
        })
    }

    #[test]
    fn a_listing_is_answered_only_where_a_whole_one_was_read_and_filtered_as_the_node_would() {
        let program = Pubkey::new_unique();
        let (a, b, c) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let accounts = HashMap::from([
            (a, account(program, &[1, 2, 9])),
            (b, account(program, &[1, 3, 9])),
            // Closed since the listing: not answered.
            (c, None),
        ]);
        let listing = Listing {
            program,
            filters: vec![(0, vec![1])],
            addresses: vec![a, b, c],
        };
        let served = Served::new(
            5,
            Instant::now(),
            accounts,
            vec![listing],
            Learned::default(),
        );
        let found = served
            .program_accounts(&program, &[(0, vec![1]), (1, vec![3])])
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, b);
        assert_eq!(
            served
                .program_accounts(&program, &[(0, vec![1])])
                .unwrap()
                .len(),
            2
        );
        // Another kind, another program, or a filter wider than the listing.
        assert!(served.program_accounts(&program, &[(0, vec![2])]).is_none());
        assert!(
            served
                .program_accounts(&Pubkey::new_unique(), &[(0, vec![1])])
                .is_none()
        );
        assert!(served.program_accounts(&program, &[]).is_none());
        // A filter past an account's end matches nothing.
        assert!(
            served
                .program_accounts(&program, &[(0, vec![1]), (2, vec![9, 9])])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn once_a_send_may_have_gone_out_nothing_is_answered_from_the_observation() {
        let address = Pubkey::new_unique();
        let accounts = HashMap::from([(address, account(Pubkey::new_unique(), &[1]))]);
        let served = Served::new(5, Instant::now(), accounts, Vec::new(), Learned::default());
        assert!(served.account(&address).is_some());
        served.went_out();
        assert!(served.account(&address).is_none());
        assert!(served.accounts(&[address]).is_none());
        assert_eq!(served.sends(), 1);
    }

    #[test]
    fn the_clock_is_answered_only_for_a_moment_after_the_observation() {
        let clock = clock_address();
        let accounts = HashMap::from([(clock, account(Pubkey::default(), &[0; 40]))]);
        let fresh = Served::new(
            5,
            Instant::now(),
            accounts.clone(),
            Vec::new(),
            Learned::default(),
        );
        assert!(fresh.account(&clock).is_some());
        let stale = Served::new(
            5,
            Instant::now() - CLOCK_FRESH - Duration::from_millis(1),
            accounts,
            Vec::new(),
            Learned::default(),
        );
        assert!(stale.account(&clock).is_none());
        assert!(stale.accounts(&[clock]).is_none());
    }

    #[test]
    fn what_is_read_of_the_node_is_learned_and_forgotten_once_unused() {
        let learned = Learned::default();
        let served = Served::learning(learned.clone());
        let address = Pubkey::new_unique();
        assert!(
            served.account(&address).is_none(),
            "a learning view answers nothing"
        );
        served.learn(&[address]);
        let now = Instant::now();
        assert_eq!(learned.plan(now, Duration::from_secs(60)), vec![address]);
        assert!(
            learned
                .plan(now + Duration::from_secs(61), Duration::from_secs(60))
                .is_empty()
        );
    }
}
