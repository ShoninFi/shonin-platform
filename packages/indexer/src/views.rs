//! What the record is built from: the answers of one observation of the
//! chain, every read at one block, and the facts a contract never changes
//! once made, as an earlier observation read them.
//!
//! The record's code reads the chain through [`Views`] as it would read a
//! node, one getter at a time, and nothing it reads goes to the chain: a
//! read the observation did not carry is noted as missing (and fails), so
//! the chain's reader observes it at the same block and builds again. What
//! the record reads is learned this way from the code that reads it, so no
//! list written by hand can leave a read out.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use alloy::primitives::{Address, Bytes};
use alloy::sol_types::SolCall;
use anyhow::{Result, anyhow, bail};
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::IVault;
use chain::follow::Item;

/// What a read not yet carried is answered as while the record is built
/// to find what it reads: zeros, which every getter's answer decodes from.
static ZEROS: [u8; 4096] = [0; 4096];

/// One read, as an observation keys it: the contract and the calldata.
pub type Key = (Address, Bytes);

/// Facts a contract never changes once made, each with the block whose
/// observation read it: a vault's identity (its strategy, author, creation
/// time, timers and home station) and a strategy's registration.
pub type Facts = HashMap<Key, (Bytes, u64)>;

/// Whether `data` asks for a fact its contract never changes once made.
pub fn fixed(data: &[u8]) -> bool {
    let Some(selector) = data.get(..4) else {
        return false;
    };
    [
        IVault::strategyIdCall::SELECTOR,
        IVault::authorCall::SELECTOR,
        IVault::createdAtCall::SELECTOR,
        IVault::timersCall::SELECTOR,
        IVault::homeStationCall::SELECTOR,
        IStrategyRegistry::strategyOfCall::SELECTOR,
    ]
    .iter()
    .any(|fixed| fixed.as_slice() == selector)
}

/// Reads answered from one observation and the facts held.
pub struct Views<'a> {
    answers: &'a HashMap<Key, Item>,
    facts: &'a Facts,
    /// Every read asked of the observation (facts held aside).
    asked: Mutex<BTreeSet<Key>>,
    /// Reads the observation did not carry.
    missing: Mutex<BTreeSet<Key>>,
}

impl<'a> Views<'a> {
    pub fn new(answers: &'a HashMap<Key, Item>, facts: &'a Facts) -> Self {
        Self {
            answers,
            facts,
            asked: Mutex::new(BTreeSet::new()),
            missing: Mutex::new(BTreeSet::new()),
        }
    }

    /// Read `call` of `to`, as a node would answer it at the observation's
    /// block: what it returned, or an error for a revert, an answer that
    /// does not decode, or a read the observation did not carry.
    pub fn read<C: SolCall>(&self, to: Address, call: C) -> Result<C::Return> {
        let key: Key = (to, Bytes::from(call.abi_encode()));
        if let Some((answer, _)) = self.facts.get(&key) {
            return C::abi_decode_returns(answer)
                .map_err(|error| anyhow!("{} at {to} did not decode: {error}", C::SIGNATURE));
        }
        lock(&self.asked).insert(key.clone());
        match self.answers.get(&key) {
            Some(Item::Returned(answer)) => C::abi_decode_returns(answer)
                .map_err(|error| anyhow!("{} at {to} did not decode: {error}", C::SIGNATURE)),
            Some(Item::Reverted(data)) => bail!("{} at {to} reverted ({data})", C::SIGNATURE),
            // Not carried: noted, and answered as zeros for now, so the
            // build goes on and finds every other read it makes at this
            // depth in the same round. Its result is not used while any
            // read is missing.
            None => {
                lock(&self.missing).insert(key);
                C::abi_decode_returns(&ZEROS)
                    .map_err(|_| anyhow!("{} at {to} is not read yet", C::SIGNATURE))
            }
        }
    }

    /// The reads the observation did not carry.
    pub fn missing(&self) -> BTreeSet<Key> {
        lock(&self.missing).clone()
    }

    /// Every read asked of the observation.
    pub fn asked(&self) -> BTreeSet<Key> {
        lock(&self.asked).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
