//! A stranger cannot lock a wallet, or everyone, out of publishing. An
//! upload can name any wallet with no proof of owning it, so the bounds on
//! uploads waiting for their payment never turn a new one away: past a
//! bound, the oldest waiting under it is forgotten instead.
//!
//! Sequence (per wallet): a stranger uploads three throwaway projects naming
//! the victim's wallet. The victim then uploads their own project, and it is
//! taken and waits for its payment.
//!
//! Sequence (overall): a stranger uploads fifty throwaway projects, each
//! naming a different made-up wallet. A real author's upload is taken and
//! waits for its payment.

mod black_box;

use alloy::primitives::Address;
use shared::model::registry::JobState;

/// Whether the upload accepted as `body` is waiting for its payment.
fn waiting(builder: &black_box::Builder, body: &str) -> bool {
    let accepted: shared::UploadAccepted = serde_json::from_str(body).unwrap();
    builder
        .jobs()
        .iter()
        .any(|job| job.id == accepted.job_id && job.state == JobState::AwaitingPayment)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_uploads_naming_a_wallet_do_not_lock_its_owner_out() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let victim = Address::repeat_byte(0x77);
    for n in 0..3 {
        let (status, body) = builder
            .upload(victim, &black_box::project(&format!("stranger {n}")))
            .await;
        assert_eq!(status, 200, "{body}");
    }
    let (status, body) = builder
        .upload(victim, &black_box::project("the victim's own agent"))
        .await;
    assert!(
        status == 200 && waiting(&builder, &body),
        "the wallet's owner cannot upload their own project after a stranger sent three in \
         its name: {status} {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fifty_uploads_naming_made_up_wallets_do_not_lock_everyone_out() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for n in 0..50u32 {
        let mut wallet = [0u8; 20];
        wallet[..4].copy_from_slice(&n.to_be_bytes());
        wallet[19] = 0xee;
        let (status, body) = builder
            .upload(
                Address::from(wallet),
                &black_box::project(&format!("junk {n}")),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }
    let (status, body) = builder
        .upload(
            Address::repeat_byte(0x42),
            &black_box::project("a real author"),
        )
        .await;
    assert!(
        status == 200 && waiting(&builder, &body),
        "a real author is refused after fifty uploads naming made-up wallets: {status} {body}"
    );
}
