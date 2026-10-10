//! Counts requests at the transport boundary, including retries and every
//! method in a batch. Only canonical chain and fixed method names are kept;
//! endpoints, arguments, signatures and node answers never enter the record.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, OnceLock};

use serde::Serialize;

#[derive(Clone, Copy, Debug)]
pub enum Component {
    Api,
    Indexer,
    Keeper,
    Executor,
}

impl Component {
    fn name(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Indexer => "indexer",
            Self::Keeper => "keeper",
            Self::Executor => "executor",
        }
    }
}

static PROCESS: OnceLock<(Component, String)> = OnceLock::new();
static COUNTS: LazyLock<Mutex<Counts>> = LazyLock::new(Default::default);

/// Set once, before connecting any chain. The caller supplies a unique
/// process generation so two observations cannot span a restart unnoticed.
pub fn start(component: Component, generation: String) {
    let _ = PROCESS.set((component, generation));
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub chain: String,
    pub method: &'static str,
    pub requests: u64,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub component: &'static str,
    pub generation: String,
    pub counters: Vec<Row>,
}

/// Cumulative counters; taking a snapshot never clears them.
pub fn snapshot() -> Snapshot {
    let process = PROCESS.get();
    let counts = COUNTS.lock().unwrap_or_else(|error| error.into_inner());
    Snapshot {
        component: process.map_or("other", |(component, _)| component.name()),
        generation: process.map_or_else(
            || "uninitialized".into(),
            |(_, generation)| generation.clone(),
        ),
        counters: counts
            .0
            .iter()
            .map(|((chain, method), requests)| Row {
                chain: chain.clone(),
                method,
                requests: *requests,
            })
            .collect(),
    }
}

pub(crate) fn evm(chain: Option<u64>, method: &str) {
    note(
        chain.map_or_else(|| "evm:connecting".into(), |id| format!("evm:{id}")),
        method,
    );
}

pub(crate) fn solana(cluster: &'static str, method: &str) {
    note(format!("solana:{cluster}"), method);
}

fn note(chain: String, method: &str) {
    COUNTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .note(chain, method);
}

#[derive(Default)]
struct Counts(BTreeMap<(String, &'static str), u64>);

impl Counts {
    fn note(&mut self, chain: String, method: &str) {
        let mut key = (chain, named_method(method));
        // Even a caller constructing arbitrary clients cannot grow this
        // diagnostic indefinitely. Overflow is retained, not dropped: an
        // unknown-cost row prevents treating such a measurement as a budget.
        if self.0.len() >= 1024 && !self.0.contains_key(&key) {
            key = ("other".into(), "other");
        }
        let count = self.0.entry(key).or_default();
        *count = count.saturating_add(1);
    }
}

fn named_method(method: &str) -> &'static str {
    match method {
        "eth_accounts" => "eth_accounts",
        "eth_chainId" => "eth_chainId",
        "eth_blockNumber" => "eth_blockNumber",
        "eth_call" => "eth_call",
        "eth_estimateGas" => "eth_estimateGas",
        "eth_feeHistory" => "eth_feeHistory",
        "eth_gasPrice" => "eth_gasPrice",
        "eth_maxPriorityFeePerGas" => "eth_maxPriorityFeePerGas",
        "eth_getBalance" => "eth_getBalance",
        "eth_getBlockByHash" => "eth_getBlockByHash",
        "eth_getBlockByNumber" => "eth_getBlockByNumber",
        "eth_getCode" => "eth_getCode",
        "eth_getLogs" => "eth_getLogs",
        "eth_getProof" => "eth_getProof",
        "eth_getStorageAt" => "eth_getStorageAt",
        "eth_getTransactionByHash" => "eth_getTransactionByHash",
        "eth_getTransactionCount" => "eth_getTransactionCount",
        "eth_getTransactionReceipt" => "eth_getTransactionReceipt",
        "eth_sendRawTransaction" => "eth_sendRawTransaction",
        "eth_sendTransaction" => "eth_sendTransaction",
        "eth_sendRawTransactionSync" => "eth_sendRawTransactionSync",
        "eth_sendTransactionSync" => "eth_sendTransactionSync",
        "getAccountInfo" => "getAccountInfo",
        "getBalance" => "getBalance",
        "getBlockHeight" => "getBlockHeight",
        "getGenesisHash" => "getGenesisHash",
        "getLatestBlockhash" => "getLatestBlockhash",
        "getMinimumBalanceForRentExemption" => "getMinimumBalanceForRentExemption",
        "getMultipleAccounts" => "getMultipleAccounts",
        "getProgramAccounts" => "getProgramAccounts",
        "getSignatureStatuses" => "getSignatureStatuses",
        "getSignaturesForAddress" => "getSignaturesForAddress",
        "getSlot" => "getSlot",
        "getTransaction" => "getTransaction",
        "sendTransaction" => "sendTransaction",
        "simulateTransaction" => "simulateTransaction",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_counted_separately_and_unknown_names_are_never_retained() {
        let mut counts = Counts::default();
        for method in [
            "eth_call",
            "eth_call",
            "eth_getLogs",
            "secret-argument",
            "another-argument",
        ] {
            counts.note("evm:1".into(), method);
        }
        assert_eq!(counts.0.len(), 3);
        assert_eq!(counts.0[&("evm:1".into(), "eth_call")], 2);
        assert_eq!(counts.0[&("evm:1".into(), "other")], 2);
    }

    #[test]
    fn overflow_is_counted_without_unbounded_labels() {
        let mut counts = Counts::default();
        for id in 0..2048 {
            counts.note(format!("evm:{id}"), "eth_call");
        }
        assert_eq!(counts.0.len(), 1025);
        assert_eq!(counts.0[&("other".into(), "other")], 1024);
        assert_eq!(counts.0.values().sum::<u64>(), 2048);
    }
}
