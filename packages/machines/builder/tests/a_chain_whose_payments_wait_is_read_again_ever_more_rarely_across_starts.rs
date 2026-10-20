//! A chain whose payments wait (neither a page's logs nor the registry's
//! record give them) is read again ever more rarely, across starts as
//! within one: each start goes on from the last wait, twice as long, not
//! from the first. A start the retry time woke reads that chain, then exits
//! after its idle spell; one woken before that time for anything else
//! leaves the chain for its time and is not kept up for it.
//!
//! Sequence: as in the payment that is waited for, the build service exits
//! after two idle seconds. Each run starts with where payments were read to
//! and when to read that chain again as the last run left them. After the
//! first, a run started an hour before the chain's time (woken for
//! something else) exits without reading the chain, the wait kept as it
//! was; a run started at the time kept, as the waker would, reads it again
//! and exits, and the wait kept is twice the last.

mod black_box;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};

/// A chain standing at 3,000 whose payments wait at block 0x20.
fn a_chain_whose_payments_wait() -> black_box::Node {
    let node = black_box::node(3_000, None);
    node.frozen_time
        .store(true, std::sync::atomic::Ordering::SeqCst);
    node.pay(0x20, 1, Address::repeat_byte(0x77), B256::repeat_byte(0x44));
    node.pay(
        2_000,
        2,
        Address::repeat_byte(0x78),
        B256::repeat_byte(0x45),
    );
    node.unreadable.lock().unwrap().push(0x20);
    node
}

/// What one run left: whether it read the chain, the one chain's wait and
/// retry time kept, and the files to start the next run with.
struct Left {
    read: bool,
    wait: u64,
    retry_at: u64,
    kept: Vec<(&'static str, String)>,
}

/// Run the build service with `kept` in its store until it exits, cleanly.
async fn run(name: &str, kept: &[(&'static str, String)]) -> Left {
    let mut builder = black_box::spawn_with(
        a_chain_whose_payments_wait(),
        kept,
        &["--idle-exit-seconds", "2"],
    )
    .await;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = builder.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{name}: the build service stayed up for a chain whose payments wait:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let said = builder.said.lock().unwrap().clone();
    assert!(
        status.success(),
        "{name} exited, but not cleanly ({status}):\n{said}"
    );
    let stalls = std::fs::read_to_string(builder.store().join("stalls.json"))
        .unwrap_or_else(|_| panic!("{name} kept no wait:\n{said}"));
    let chains: serde_json::Value = serde_json::from_str(&stalls).unwrap();
    let chains = chains.as_object().unwrap();
    assert_eq!(chains.len(), 1, "{stalls}");
    let chain = chains.values().next().unwrap();
    let mut kept = vec![("stalls.json", stalls.clone())];
    // Where payments were read to, if they were read past any block.
    if let Ok(payments) = std::fs::read_to_string(builder.store().join("payments.json")) {
        kept.push(("payments.json", payments));
    }
    Left {
        read: said.contains("payments on this chain wait at these blocks"),
        wait: chain["wait"].as_u64().unwrap(),
        retry_at: chain["retry_at"].as_u64().unwrap(),
        kept,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_whose_payments_wait_is_read_again_ever_more_rarely_across_starts() {
    let mut last = run("the first run", &[]).await;
    assert!(last.read, "the first run never read the chain");
    for again in 1..=2 {
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        let later = now() + 3_600;
        let mut early_kept = last.kept.clone();
        early_kept[0].1 = early_kept[0].1.replace(
            &format!("\"retry_at\":{}", last.retry_at),
            &format!("\"retry_at\":{later}"),
        );
        let early = run(&format!("run {again}, started early"), &early_kept).await;
        assert!(
            !early.read,
            "run {again}, started before the time kept, read the chain"
        );
        assert_eq!(
            (early.wait, early.retry_at),
            (last.wait, later),
            "run {again}, started early"
        );
        tokio::time::sleep(Duration::from_secs(last.retry_at.saturating_sub(now()))).await;
        let timely = run(&format!("run {again}, started at its time"), &last.kept).await;
        assert!(
            timely.read,
            "run {again}, started at the time kept, never read the chain"
        );
        assert_eq!(
            timely.wait,
            last.wait * 2,
            "run {again}: the wait did not go on from the last"
        );
        last = timely;
    }
}
