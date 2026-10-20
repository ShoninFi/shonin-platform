//! A paid claim keeps the share of the vault it closed. Paying a claim burns
//! its shares while the claim keeps its `shares`, so the supply after the
//! payment no longer counts them: measured against it, a claim for half the
//! vault would read as having closed all of it. The fraction is measured
//! against the supply before the payment (the block before its `ClaimPaid`,
//! or the share it stood for when last read waiting) and kept.
//!
//! Here: a claim for 50 of 100 shares, read once while it waits (0.5), then
//! paid (the supply now 50); and one first read already paid, its payment
//! in the vault's log, with deposits since bringing the supply to 150.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
const PAID_IN: u64 = 5;

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

/// A chain at block 0x10 whose vault holds one claim for 50 of 100 shares,
/// paid once `paid` is set. With `logged`, the vault's log shows the payment
/// at [`PAID_IN`], the supply read at the block before counts the claim's
/// shares, and deposits since have brought the supply to 150.
async fn an_indexer(paid: Arc<AtomicBool>, logged: bool, name: &str) -> Arc<Indexer> {
    let answer: Answer = Arc::new(move |request: &Value| {
        let paid = paid.load(Ordering::SeqCst);
        let word = |value: U256| Some(Ok(json!(format!("0x{value:064x}"))));
        match request["method"].as_str().unwrap_or_default() {
            "eth_getLogs" if logged => Some(Ok(json!(matching(request, &[the_payment()])))),
            "eth_call" => {
                let data = input(request);
                if data == selector::<IVault::claimCountCall>() {
                    word(U256::from(1))
                } else if data == selector::<IVault::totalSupplyCall>() {
                    // Paying the claim burned its 50 shares.
                    let before_payment = requested_block(request, 0) == PAID_IN - 1;
                    word(if !paid || before_payment {
                        shares(100)
                    } else if logged {
                        // And deposits since have brought it to 150.
                        shares(150)
                    } else {
                        shares(50)
                    })
                } else if data.starts_with(&selector::<IVault::claimAtCall>()) {
                    let claim = Claim {
                        owner: Address::repeat_byte(0x22),
                        shares: shares(50),
                        bookedAt: 1_790_000_000,
                        lastAttemptAt: 1_790_000_000,
                        state: if paid {
                            ClaimState::Paid
                        } else {
                            ClaimState::Pending
                        },
                        paid: if paid {
                            U256::from(50_000_000u64)
                        } else {
                            U256::ZERO
                        },
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
    an_indexer_on(Chain::at(HEAD), deployments(), answer, name).await
}

async fn fraction(indexer: &Indexer) -> f64 {
    indexer.snapshot.read().await.strategies[0].vault.claims[0].fraction_closed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_claim_keeps_the_fraction_it_closed() {
    let paid = Arc::new(AtomicBool::new(false));
    let indexer = an_indexer(paid.clone(), false, "paid-claim").await;
    indexer.refresh().await.unwrap();
    let waiting = fraction(&indexer).await;
    paid.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let after = fraction(&indexer).await;
    assert!(
        (waiting - 0.5).abs() < 1e-9 && (after - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares: {waiting} while waiting, {after} once paid"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_first_read_paid_is_measured_against_the_supply_before_its_payment() {
    let indexer = an_indexer(Arc::new(AtomicBool::new(true)), true, "paid-in-the-log").await;
    indexer.refresh().await.unwrap();
    let closed = fraction(&indexer).await;
    assert!(
        (closed - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares, first read paid: {closed}"
    );
}
