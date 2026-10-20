//! A vault's own logs cost no request of their own before the block it was
//! created in (its factory's `Activated` log): they are read in the
//! platform's pages, and a vault read apart is read from its creation, not
//! from the platform's deployment, so a vault created long after the
//! deployment does not have every block between asked for its logs. A vault's kill is shown however many of its events
//! follow it, and two claims paid in one block each close the share of the
//! supply left when it was paid, in the order the block paid them.
//!
//! Here: the platform was deployed at block 1; the one vault was created at
//! block 40,000, the head is 40,100.

mod stand_in;

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolCall;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::{Claim, ClaimState, IVault};
use indexer::Indexer;
use serde_json::{Value, json};
use stand_in::*;

const DEPLOYED: u64 = 1;
const CREATED_IN: u64 = 40_000;
const HEAD: u64 = 40_100;
/// The block both claims were paid in.
const PAID_IN: u64 = 40_050;

fn shares(count: u64) -> U256 {
    U256::from(count) * U256::from(10u64.pow(18))
}

/// The vault's own logs: it was stopped, sixty-one deposits were priced
/// after that, and both its claims were paid in one block, claim 2 first.
fn the_vault_s_logs() -> Vec<Value> {
    let mut logs = vec![logged(
        IVault::Halted {
            reason: B256::right_padding_from(b"killed"),
            haltedAt: 1_790_000_000,
        },
        CREATED_IN + 10,
        0,
    )];
    for index in 0..61 {
        logs.push(logged(
            IVault::Deposited {
                requestId: U256::from(index),
                owner: Address::repeat_byte(0x33),
                assets: U256::from(1_000_000u64),
                shares: shares(1),
            },
            CREATED_IN + 20,
            index,
        ));
    }
    for (log_index, claim) in [(4, 2), (7, 1)] {
        logs.push(logged(
            IVault::ClaimPaid {
                claimId: U256::from(claim),
                assets: U256::from(50_000_000u64),
            },
            PAID_IN,
            log_index,
        ));
    }
    logs
}

/// The factory's `Activated` for the vault, at [`CREATED_IN`].
fn the_activation() -> Value {
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
    )
}

/// An indexer refreshed until it has read the chain through, and every log
/// request that named the vault, by its range and how many addresses it
/// named.
async fn read_through(name: &str) -> (Arc<Indexer>, Vec<(u64, u64, usize)>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = asked.clone();
    let answer: Answer = Arc::new(move |request: &Value| {
        let word = |value: U256| Some(Ok(json!(format!("0x{value:064x}"))));
        match request["method"].as_str().unwrap_or_default() {
            "eth_getLogs" => {
                if asks_the_vault(request) {
                    let (from, to) = log_range(request);
                    let named = request["params"][0]["address"]
                        .as_array()
                        .map_or(1, Vec::len);
                    kept.lock().unwrap().push((from, to, named));
                }
                let mut logs = the_vault_s_logs();
                logs.push(the_activation());
                Some(Ok(json!(matching(request, &logs))))
            }
            "eth_call" => {
                let data = input(request);
                if data == selector::<IVault::claimCountCall>() {
                    word(U256::from(2))
                } else if data == selector::<IVault::totalSupplyCall>() {
                    // 200 before the block both claims were paid in, 100
                    // once both had burned their 50.
                    let before = requested_block(request, HEAD) == PAID_IN - 1;
                    word(if before { shares(200) } else { shares(100) })
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
    let indexer = an_indexer_on(Chain::at(HEAD), deployments_from(DEPLOYED), answer, name).await;
    for _ in 0..3 {
        indexer.refresh().await.unwrap();
    }
    let asked = asked.lock().unwrap().clone();
    (indexer, asked)
}

/// A vault's logs cost no log request of their own over the blocks before
/// it was created: they are read in the same requests as the platform's
/// (one request a page for every address watched), and a request naming the
/// vault alone starts no earlier than the block it was created in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_s_own_logs_are_read_from_the_block_it_was_created_in() {
    let (_, asked) = read_through("from-creation").await;
    assert!(!asked.is_empty(), "the vault's logs were never read");
    for (from, to, named) in &asked {
        assert!(
            *named > 1 || *from >= CREATED_IN,
            "the vault's logs alone were read from block {from} to {to}; it was created in {CREATED_IN}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kill_is_shown_however_many_events_follow_it() {
    let (indexer, _) = read_through("kill-shown").await;
    let snapshot = indexer.snapshot.read().await;
    assert!(
        snapshot.strategies[0].kill.is_some(),
        "a vault stopped sixty-one events ago shows no kill"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_claims_paid_in_one_block_each_close_the_share_left_when_it_was_paid() {
    let (indexer, _) = read_through("same-block-claims").await;
    let snapshot = indexer.snapshot.read().await;
    let claims = &snapshot.strategies[0].vault.claims;
    let (first, second) = (claims[0].fraction_closed, claims[1].fraction_closed);
    // Claim 2 was paid first, from 200 shares; claim 1 from the 150 left.
    assert!(
        (second - 0.25).abs() < 1e-9 && (first - 50.0 / 150.0).abs() < 1e-9,
        "claim 1 closed {first}, claim 2 closed {second}"
    );
}
