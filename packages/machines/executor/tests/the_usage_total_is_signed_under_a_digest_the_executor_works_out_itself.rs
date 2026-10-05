//! The executor signs a vault's running total with the vault's blessed key,
//! and the vault believes a total signed so (up to its cap). The digest it
//! signs is worked out by the executor itself, as the vault writes it
//! (`keccak256(abi.encode(chainid, vault, "shonin-usage", total, asOf))`),
//! never read from the node: whoever answers the executor's RPC could
//! otherwise hand it the digest of any total, or of any other message the
//! key's signature counts for, and have it signed.
//!
//! Sequence: an open vault, its key blessed and approved; the node answers
//! every `usageDigest` call with the digest of a total of 10^24, whatever
//! total it is asked about. The total the executor serves at `/usage` is
//! signed over that total, and over nothing else.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::{Address, Signature, U256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_usage_total_is_signed_under_a_digest_the_executor_works_out_itself() {
    let executor = black_box::start("not run", black_box::Script::Approved).await;
    let huge = U256::from(10u64).pow(U256::from(24u64));
    *executor.node.lying_total.lock().unwrap() = Some(huge);

    let url = format!(
        "http://127.0.0.1:{}/usage/31337/{:?}",
        executor.port,
        black_box::vault()
    );
    let started = Instant::now();
    let served = loop {
        if let Ok(answer) = reqwest::get(&url).await
            && answer.status().is_success()
        {
            break answer.json::<serde_json::Value>().await.unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no total was ever served:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    drop(executor);

    let total: U256 = served["total"].as_str().unwrap().parse().unwrap();
    let as_of = served["as_of"].as_u64().unwrap();
    let signature = alloy::hex::decode(served["signature"].as_str().unwrap()).unwrap();
    let signature = Signature::try_from(signature.as_slice()).unwrap();
    let key = black_box::executor_key(black_box::vault());
    let signer_of = |total: U256| -> Address {
        signature
            .recover_address_from_msg(black_box::usage_digest(total, as_of).as_slice())
            .unwrap()
    };

    assert!(total < huge, "the meter served {total}");
    assert_ne!(
        signer_of(huge),
        key,
        "the vault's key signed a total of {huge} (10^24) because the node said that was the \
         digest of the {total} the meter served"
    );
    assert_eq!(
        signer_of(total),
        key,
        "the signature served with the total is not over that total"
    );
}
