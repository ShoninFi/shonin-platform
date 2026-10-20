//! A vault the factory lists only after the record's logs have been read
//! past the block it was created in (its activation read, but the vault
//! not yet listed when it was) has its own logs read apart, from the block
//! it was created in, never from the deployment, until it has caught up
//! with the rest; from then on it is read in the same requests as every
//! other address.
//!
//! Here: the vault was activated at block 500 and deposited into at 600;
//! the factory lists it from the second refresh on.

mod stand_in;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, U256};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::IVault;
use serde_json::{Value, json};
use shared::model::activity::EventKind;
use stand_in::*;

const CREATED_IN: u64 = 500;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_listed_after_its_logs_were_passed_is_read_from_its_creation() {
    let listed = Arc::new(AtomicBool::new(false));
    // Each log read naming the vault: its first block, and how many
    // addresses it named.
    let reads: Arc<Mutex<Vec<(u64, usize)>>> = Arc::default();
    let logs = vec![
        a_log(
            Address::with_last_byte(1),
            IVaultFactory::Activated {
                strategyId: U256::from(1),
                author: Address::repeat_byte(0x22),
                vault: vault(),
                station: Address::ZERO,
                tankFunding: U256::ZERO,
            },
            CREATED_IN,
            0,
        ),
        logged(
            IVault::Deposited {
                requestId: U256::from(1),
                owner: Address::repeat_byte(0x33),
                assets: U256::from(25_000_000u64),
                shares: U256::from(25u64) * U256::from(10u64.pow(18)),
            },
            600,
            0,
        ),
    ];
    let answer: Answer = {
        let listed = listed.clone();
        let reads = reads.clone();
        Arc::new(move |request: &Value| match request["method"].as_str() {
            Some("eth_getLogs") => {
                if asks_the_vault(request) {
                    let named = request["params"][0]["address"]
                        .as_array()
                        .map_or(1, Vec::len);
                    reads.lock().unwrap().push((log_range(request).0, named));
                }
                Some(Ok(json!(matching(request, &logs))))
            }
            Some("eth_call")
                if input(request)
                    == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>()
                    && !listed.load(Ordering::SeqCst) =>
            {
                Some(Ok(no_vaults()))
            }
            _ => None,
        })
    };
    let chain = Chain::at(1_000);
    let indexer = an_indexer_on(chain.clone(), deployments(), answer, "listed-late").await;

    indexer.refresh().await.unwrap();
    assert!(indexer.snapshot.read().await.strategies.is_empty());
    assert!(reads.lock().unwrap().is_empty());

    // Listed now, its activation long read.
    listed.store(true, Ordering::SeqCst);
    chain.head.store(1_010, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let alone: Vec<(u64, usize)> = reads.lock().unwrap().clone();
    assert!(
        !alone.is_empty()
            && alone
                .iter()
                .all(|(from, named)| *named == 1 && *from == CREATED_IN),
        "the vault's own logs were read as {alone:?}"
    );
    {
        let snapshot = indexer.snapshot.read().await;
        let deposits: Vec<_> = snapshot.strategies[0]
            .events
            .iter()
            .filter(|event| event.kind == EventKind::Deposited)
            .collect();
        assert_eq!(
            deposits.len(),
            1,
            "the deposit before it was listed is read"
        );
        assert!(!snapshot.chains[0].catching_up);
    }

    // From then on, read with the rest.
    reads.lock().unwrap().clear();
    chain.head.store(1_020, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let after = reads.lock().unwrap().clone();
    assert!(
        !after.is_empty()
            && after
                .iter()
                .all(|(from, named)| *named > 1 && *from > 1_000 - 64),
        "once caught up the vault's logs were read as {after:?}"
    );
}
