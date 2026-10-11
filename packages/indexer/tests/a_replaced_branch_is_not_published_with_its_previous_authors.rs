//! A branch replacement discards derived cursors and authors. Failed rebuilds
//! keep the dated old publication until a complete new observation succeeds.
mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use alloy::primitives::B256;
use alloy::sol_types::SolCall;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use serde_json::json;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaced_branch_is_not_published_with_its_previous_authors() {
    let phase = Arc::new(AtomicU8::new(0));
    let current = phase.clone();
    let indexer = an_indexer(
        Arc::new(move |request| {
            let phase = current.load(Ordering::SeqCst);
            let head = 0x10 + u64::from(phase) * 0x10;
            match request["method"].as_str().unwrap_or_default() {
                "eth_blockNumber" => Some(Ok(json!(format!("0x{head:x}")))),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    let number = requested_block(request, head);
                    block.header.inner.number = number;
                    block.header.inner.timestamp = 1_790_000_000 + number;
                    let branch = if phase == 0 { 0 } else { 0x80 };
                    block.header.hash = B256::repeat_byte(number as u8 ^ branch);
                    Some(Ok(serde_json::to_value(block).unwrap()))
                }
                "eth_call"
                    if input(request)
                        .starts_with(&selector::<IAuthorProfiles::profileOfCall>()) =>
                {
                    if matches!(phase, 1 | 2) {
                        Some(Err(
                            "the replacement branch's profiles are unavailable".into()
                        ))
                    } else {
                        let profile = Profile {
                            handle: if phase == 0 { "old" } else { "new" }.into(),
                            blurb: String::new(),
                            linkLabels: vec![],
                            linkUrls: vec![],
                            joinedAt: 0,
                        };
                        Some(Ok(json!(format!(
                            "0x{}",
                            alloy::hex::encode(IAuthorProfiles::profileOfCall::abi_encode_returns(
                                &profile
                            ),)
                        ))))
                    }
                }
                _ => None,
            }
        }),
        "replacement-authors",
    )
    .await;

    indexer.refresh().await.unwrap();
    let old = indexer.snapshot.read().await.clone();
    assert_eq!(old.chains[0].block, 0x10);
    assert_eq!(old.authors[0].handle, "old");
    for failed in [1, 2] {
        phase.store(failed, Ordering::SeqCst);
        indexer.refresh().await.unwrap();
        let held = indexer.snapshot.read().await;
        assert_eq!(held.chains[0].block, old.chains[0].block);
        assert_eq!(held.chains[0].as_of, old.chains[0].as_of);
        assert_eq!(held.authors[0].handle, "old");
    }
    phase.store(3, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let rebuilt = indexer.snapshot.read().await;
    assert_eq!(rebuilt.chains[0].block, 0x40);
    assert_eq!(rebuilt.authors[0].handle, "new");
}
