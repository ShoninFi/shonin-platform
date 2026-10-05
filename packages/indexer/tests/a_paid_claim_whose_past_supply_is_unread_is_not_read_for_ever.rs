//! A paid claim whose supply before its payment can't be read is not read
//! again at every refresh for ever. A paid claim is measured against the
//! vault's `totalSupply` at the block before its `ClaimPaid`; a node that
//! keeps no state that far back (most full nodes) never answers that, so
//! once it has gone unanswered five refreshes in a row the claim is kept
//! with the share it was last read to close, as a reading the vault doesn't
//! answer is passed over, and the reads don't grow with every claim the
//! vault has ever paid.
//!
//! Here: one claim, paid in block 5; the node refuses every read of past
//! state. Ten refreshes.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::vault::{Claim, ClaimState, IVault};
use serde_json::json;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_claim_whose_past_supply_is_unread_is_not_read_for_ever() {
    let claim_reads = Arc::new(AtomicU32::new(0));
    let counted = claim_reads.clone();
    let indexer = an_indexer(
        Arc::new(move |request| match request["method"].as_str() {
            Some("eth_getLogs") if asks_the_vault(request) => Some(Ok(json!([logged(
                IVault::ClaimPaid {
                    claimId: U256::from(1),
                    assets: U256::from(50_000_000u64),
                },
                5,
                0
            )]))),
            Some("eth_call") => {
                // No state kept for any block but the head.
                let past = request["params"][1]
                    .as_str()
                    .is_some_and(|tag| tag.starts_with("0x") && tag != "0x10");
                if past {
                    return Some(Err("missing trie node".to_string()));
                }
                let data = input(request);
                if data == selector::<IVault::claimCountCall>() {
                    Some(Ok(json!(format!("0x{:064x}", U256::from(1)))))
                } else if data.starts_with(&selector::<IVault::claimAtCall>()) {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let claim = Claim {
                        owner: Address::repeat_byte(0x22),
                        shares: U256::from(50u64) * U256::from(10u64.pow(18)),
                        bookedAt: 1_790_000_000,
                        lastAttemptAt: 1_790_000_000,
                        state: ClaimState::Paid,
                        paid: U256::from(50_000_000u64),
                    };
                    Some(Ok(json!(format!(
                        "0x{}",
                        alloy::hex::encode(IVault::claimAtCall::abi_encode_returns(&claim))
                    ))))
                } else {
                    None
                }
            }
            _ => None,
        }),
        "paid-claim-unread-supply",
    )
    .await;
    for _ in 0..6 {
        indexer.refresh().await.unwrap();
    }
    let after_six = claim_reads.load(Ordering::SeqCst);
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let later = claim_reads.load(Ordering::SeqCst) - after_six;
    assert_eq!(
        later, 0,
        "a paid claim whose past supply no node answers was read {later} more times in four refreshes after six"
    );
}
