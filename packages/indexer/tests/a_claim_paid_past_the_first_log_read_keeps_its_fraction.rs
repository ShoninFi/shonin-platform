//! A claim first read paid keeps the share of the vault it closed even when
//! its `ClaimPaid` lies past what one refresh reads of the vault's log (25
//! pages of 2,000 blocks): until the payment is read, the claim's fraction is
//! not kept for good, and once it is, it is measured against the supply
//! before the payment.
//!
//! Here: the indexer starts with a vault whose claim (50 of 100 shares) was
//! paid at block 60,000, and deposits since have brought the supply to 150.
//! Measured against today's supply with its shares put back, it would read
//! 0.25, and if kept as that, would stay 0.25 once the log is read through.

mod stand_in;

use std::sync::Arc;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::vault::{Claim, ClaimState, IVault};
use indexer::Indexer;
use serde_json::{Value, json};
use stand_in::*;

fn shares(count: u64) -> U256 {
    U256::from(count) * U256::from(10u64.pow(18))
}

/// The block the claim was paid in, when the vault's log shows it.
const PAID_IN: u64 = 60_000;

/// The vault's `ClaimPaid` for claim 1, in block [`PAID_IN`].
fn the_payment() -> Value {
    logged(
        IVault::ClaimPaid {
            claimId: U256::from(1),
            assets: U256::from(50_000_000u64),
        },
        PAID_IN,
        0,
    )
}

/// A chain past [`PAID_IN`] whose one vault holds one claim for 50 of 100
/// shares, paid there: its log shows the payment, the supply read at the
/// block before counts the claim's shares, and deposits since have brought
/// the supply to 150.
async fn an_indexer_of_a_paid_claim(name: &str) -> Arc<Indexer> {
    let answer: Answer = Arc::new(|request: &Value| {
        let word = |value: U256| Some(Ok(json!(format!("0x{value:064x}"))));
        match request["method"].as_str().unwrap_or_default() {
            "eth_getLogs" => Some(Ok(json!(matching(request, &[the_payment()])))),
            "eth_call" => {
                let data = input(request);
                if data == selector::<IVault::claimCountCall>() {
                    word(U256::from(1))
                } else if data == selector::<IVault::totalSupplyCall>() {
                    // Paying the claim burned its 50 shares, and deposits
                    // since have brought it to 150.
                    let before_payment = requested_block(request, 0) == PAID_IN - 1;
                    word(if before_payment {
                        shares(100)
                    } else {
                        shares(150)
                    })
                } else if data.starts_with(&selector::<IVault::claimAtCall>()) {
                    let claim = Claim {
                        owner: Address::repeat_byte(0x22),
                        shares: shares(50),
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
        }
    });
    an_indexer_on(Chain::at(PAID_IN + 10), deployments(), answer, name).await
}

async fn fraction(indexer: &Indexer) -> f64 {
    indexer.snapshot.read().await.strategies[0].vault.claims[0].fraction_closed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_paid_past_the_first_refresh_s_log_read_keeps_the_fraction_it_closed() {
    let indexer = an_indexer_of_a_paid_claim("paid-late").await;
    // The first refreshes read the factory's and the vault's logs only part
    // of the way; later ones read them through to the payment.
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let closed = fraction(&indexer).await;
    assert!(
        (closed - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares, first read paid: {closed}"
    );
}
