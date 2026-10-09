//! Publishing on a real local cluster the way the cloud build service and
//! reviewer do it, each with its own key and neither holding the other's: the
//! author's payment read back at the finalized commitment, the reviewer's
//! go-ahead signed apart and carried to the registration with the build
//! service's own signature, a leftover draft that says something else given
//! up first, a go-ahead over other source refused, a refusal recorded by the
//! reviewer alone, each service key's standing read as the publishing program
//! reads it, and the vaults found again by the scan as they change.
//!
//! The test deploys the programs itself (`contracts/solana/target/deploy`)
//! to a Surfpool of its own. Without Surfpool 1.2.0 or later, the Solana CLI
//! or the built programs, it says so and passes, as CI has no cluster.

#![cfg(feature = "solana")]

mod three_venues;

use std::time::{Duration, Instant};

use chain::Role;
use chain::solana::platform::Registration;
use chain::solana::programs::registry::state::ImageKind;
use chain::solana::programs::{common, prices, publishing, vault};
use chain::solana::scan::VaultScan;
use chain::solana::{Signer, name};

use three_venues::{Platformed, start};

/// How long a payment may take to be finalized on the local cluster.
const FINALIZED_WITHIN: Duration = Duration::from_secs(90);

#[tokio::test]
async fn a_publish_signed_apart_is_registered_and_its_payment_read_once_final() {
    let Some(cluster) = start(&[]) else { return };
    let p = Platformed::set_up(&cluster).await;
    let (client, platform) = (&p.client, &p.platform);
    let (author, builder, reviewer, attestor) = (
        p.role(Role::Author),
        p.role(Role::Builder),
        p.role(Role::Reviewer),
        p.role(Role::Attestor),
    );
    let digest = |part: &str| common::digest::keccak(&[part.as_bytes(), b"signed apart"]);

    // Not blessed yet: neither service counts as its kind.
    let standing = platform
        .service_standing(client, &builder.pubkey(), ImageKind::Builder)
        .await
        .unwrap();
    assert!(!standing.blessed && standing.until == 0);
    for (service, key) in [("builder", builder), ("reviewer", reviewer)] {
        platform
            .bless_service(client, key, attestor, service, &key.pubkey())
            .await
            .unwrap();
    }
    let standing = platform
        .service_standing(client, &builder.pubkey(), ImageKind::Builder)
        .await
        .unwrap();
    assert!(standing.blessed && standing.until > standing.now);
    // The builder's key is no reviewer's.
    assert!(
        !platform
            .service_standing(client, &builder.pubkey(), ImageKind::Reviewer)
            .await
            .unwrap()
            .blessed
    );

    // The author pays; the payment is read once it is final, by its id.
    let (submit, id) = platform
        .ix_submit(client, &author.pubkey(), digest("upload"))
        .await
        .unwrap();
    client.send(vec![submit], &[author]).await.unwrap();
    let started = Instant::now();
    loop {
        let (count, at) = platform.finalized_count(client).await.unwrap();
        if count >= id {
            assert!(at > 1_700_000_000, "the finalized clock reads real time");
            break;
        }
        assert!(
            started.elapsed() < FINALIZED_WITHIN,
            "the payment was never finalized"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let paid = platform
        .finalized_submission(client, id)
        .await
        .unwrap()
        .expect("a finalized payment is read");
    assert_eq!(paid.author, author.pubkey());
    assert_eq!(paid.upload, digest("upload"));
    assert!(
        platform
            .finalized_submission(client, id + 1)
            .await
            .unwrap()
            .is_none()
    );

    let registration = Registration {
        id,
        author: author.pubkey(),
        source_hash: digest("source"),
        bundle_hash: digest("bundle"),
        manifest_hash: digest("manifest"),
        lists: publishing::state::Lists {
            chains: vec![publishing::state::ChainNeed {
                chain: name("solana"),
                drivers: p.drivers.iter().map(|driver| driver.name).collect(),
            }],
            bridges: vec![],
            restricted: vec![name("SANCTIONED"), name("US")],
        },
    };
    let signed = |source: &[u8; 32]| -> [u8; 64] {
        reviewer
            .sign_message(&platform.clearance_digest(id, source))
            .as_ref()
            .try_into()
            .unwrap()
    };

    // A go-ahead over other source than the one registered is refused, and
    // nothing is recorded.
    assert!(
        platform
            .register_signed(
                client,
                builder,
                &registration,
                &reviewer.pubkey(),
                &signed(&digest("other source")),
            )
            .await
            .is_err()
    );
    assert!(platform.strategy(client, id).await.unwrap().is_none());

    // A draft left by a try that said something else is given up, and the
    // registration goes through with both signatures.
    let mut other = registration.clone();
    other.bundle_hash = digest("an older build");
    let _ = client
        .send(vec![platform.ix_discard(&builder.pubkey(), id)], &[builder])
        .await;
    client
        .send(
            vec![platform.ix_stage(&builder.pubkey(), &other)],
            &[builder],
        )
        .await
        .unwrap();
    platform
        .register_signed(
            client,
            builder,
            &registration,
            &reviewer.pubkey(),
            &signed(&registration.source_hash),
        )
        .await
        .unwrap();
    let record = platform.strategy(client, id).await.unwrap().unwrap();
    assert_eq!(record.bundle_hash, registration.bundle_hash);
    assert_eq!(record.author, author.pubkey());
    assert_eq!(record.lists, registration.lists);

    // The reviewer posts the review of what was registered, from its own key.
    let review = publishing::state::ReviewInput {
        bundle_hash: record.bundle_hash,
        source_hash: record.source_hash,
        model_id_hash: digest("model"),
        prompt_hash: digest("prompt"),
        report_hash: digest("report"),
        ratings: [3; 6],
        finding_count: 0,
    };
    let ix = platform
        .ix_submit_review(client, &reviewer.pubkey(), &reviewer.pubkey(), id, review)
        .await
        .unwrap();
    client.send(vec![ix], &[reviewer]).await.unwrap();
    assert!(
        platform
            .review(client, id)
            .await
            .unwrap()
            .is_some_and(|review| review.is_review())
    );

    // Another payment, refused by the reviewer alone.
    let (submit, refused) = platform
        .ix_submit(client, &author.pubkey(), digest("another upload"))
        .await
        .unwrap();
    client.send(vec![submit], &[author]).await.unwrap();
    let refusal = publishing::state::ReviewInput {
        bundle_hash: [0; 32],
        source_hash: digest("malicious source"),
        model_id_hash: digest("model"),
        prompt_hash: digest("prompt"),
        report_hash: digest("refusal"),
        ratings: [1; 6],
        finding_count: 1,
    };
    let ix = platform
        .ix_submit_refusal(
            client,
            &reviewer.pubkey(),
            &reviewer.pubkey(),
            refused,
            refusal,
        )
        .await
        .unwrap();
    client.send(vec![ix], &[reviewer]).await.unwrap();
    let recorded = platform.review(client, refused).await.unwrap().unwrap();
    assert!(recorded.refused);

    // The vault found by the scan, listed once, then read as it changes.
    let mut scan = VaultScan::new(Duration::from_secs(3_600));
    assert!(scan.look(client).await.unwrap().vaults.is_empty());
    let (_, statement) = platform
        .restricted_for(client, &author.pubkey(), id, &p.drivers)
        .await
        .unwrap();
    let factory = platform.factory(client).await.unwrap();
    let activate = platform
        .ix_activate(
            client,
            &author.pubkey(),
            id,
            &p.drivers,
            prices::Allowances::default(),
            500,
            factory.terms.minimum_tank,
            statement,
            vault::factory::terms_hash(&factory.terms),
        )
        .await
        .unwrap();
    client.send(vec![activate], &[author]).await.unwrap();
    let v = platform.vault_accounts(id);
    // A new vault moves the factory's count: it is found at the next look.
    let found = scan.look(client).await.unwrap();
    assert_eq!(found.vaults.len(), 1);
    assert_eq!(found.vaults[0].0, v.vault);
    let tank = found.vaults[0].1.tank.balance;
    client
        .send(
            vec![platform.ix_fund_tank(&author.pubkey(), &v.vault, 1_000_000)],
            &[author],
        )
        .await
        .unwrap();
    // Nothing counted moved: the vault is read again as it is now.
    let found = scan.look(client).await.unwrap();
    assert_eq!(found.vaults[0].1.tank.balance, tank + 1_000_000);
    client
        .send(
            vec![platform.ix_set_tank_keep(&author.pubkey(), &v.vault, tank + 1_000_000)],
            &[author],
        )
        .await
        .unwrap();
    assert_eq!(
        platform.vault(client, &v.vault).await.unwrap().tank.keep_at,
        tank + 1_000_000
    );
    // The modest allowances cost something a day at the published prices.
    let daily = platform
        .daily_cost(
            client,
            &prices::Allowances {
                fuel_per_sec: 10_000_000,
                memory_mib: 64,
                requests_per_min: 120,
                bytes_per_day: 50_000_000,
                streams: 4,
                inference_ms_per_min: 0,
                gas_per_day: 1_000_000,
            },
        )
        .await
        .unwrap();
    assert!(daily > 1_000_000, "{daily}");
}
