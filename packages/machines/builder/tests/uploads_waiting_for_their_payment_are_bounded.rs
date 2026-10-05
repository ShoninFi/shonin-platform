//! Uploads waiting for their payment are bounded: three per wallet named,
//! and fifty in all. Past either, the oldest upload waiting under that bound
//! is forgotten to make room, and what is remembered of it turns a payment
//! for it into a credit. An upload sent again before it is paid for is still
//! the one already waiting.
//!
//! An upload can name any wallet, so without a bound anyone could fill the
//! service's memory and disk with archives under someone else's name; and
//! nobody proves they own the wallet they name, so a bound that turned new
//! uploads away would let anyone lock a wallet, or everyone, out.
//!
//! Sequence: 25 different projects, all naming one wallet that never pays.
//! Each is taken, and the three newest are what is left waiting; the newest
//! sent again is still the one waiting. Then uploads from 48 other wallets,
//! one each, fill the service past 50: the oldest of the wallet's three is
//! forgotten. Finally the first upload of all, long forgotten, is paid for,
//! and the payment is kept as a credit.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_waiting_for_their_payment_are_bounded() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let named = Address::repeat_byte(0x77);
    let mut accepted = Vec::new();
    for n in 0..25 {
        let (status, body) = builder
            .upload(named, &black_box::project(&format!("named {n}")))
            .await;
        assert_eq!(status, 200, "upload {n} answered {status}: {body}");
        accepted.push(serde_json::from_str::<shared::UploadAccepted>(&body).unwrap());
    }
    let waiting = |builder: &black_box::Builder| -> Vec<String> {
        builder
            .jobs()
            .into_iter()
            .filter(|job| job.state == JobState::AwaitingPayment)
            .map(|job| job.id)
            .collect()
    };
    let newest: Vec<String> = accepted[22..].iter().map(|a| a.job_id.clone()).collect();
    assert_eq!(waiting(&builder), newest);

    let (status, body) = builder.upload(named, &black_box::project("named 24")).await;
    assert_eq!(status, 200, "an upload sent again was refused: {body}");
    let again: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_eq!(again.job_id, accepted[24].job_id);
    assert_eq!(waiting(&builder), newest);

    for n in 0..48u32 {
        let mut wallet = [0u8; 20];
        wallet[..4].copy_from_slice(&n.to_be_bytes());
        wallet[19] = 1;
        let (status, body) = builder
            .upload(
                Address::from(wallet),
                &black_box::project(&format!("other {n}")),
            )
            .await;
        assert_eq!(
            status, 200,
            "upload {n} from another wallet was refused: {body}"
        );
    }
    let left = waiting(&builder);
    assert_eq!(left.len(), 50);
    assert!(
        !left.contains(&accepted[22].job_id) && left.contains(&accepted[23].job_id),
        "the oldest upload waiting was not the one forgotten: {left:?}"
    );
    assert_eq!(
        builder.queue_file()["forgotten"].as_array().map(Vec::len),
        Some(23)
    );

    let first: B256 = accepted[0].upload.parse().unwrap();
    builder.node.pay(0x20, 9, named, first);
    builder.node.head.store(0x30, Ordering::SeqCst);
    let started = Instant::now();
    while builder.queue_file()["credits"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a payment for an upload forgotten to make room was not credited"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
