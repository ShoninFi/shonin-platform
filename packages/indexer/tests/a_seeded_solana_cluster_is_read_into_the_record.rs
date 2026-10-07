//! The indexer against a seeded local Solana cluster: its vaults are in the
//! record with their figures and their history, and a health report about
//! one is taken from the key blessed as its executor and from no other.
//!
//! It needs a cluster the devnet set up and the seed published to, named by
//! `SHONIN_SOLANA_CHAINS` (the chain config) and `SHONIN_SOLANA_DEPLOYMENTS`
//! (the deployments directory). Without them it says so and passes, as CI
//! has no cluster.

use std::path::PathBuf;

use chain::solana::{Pubkey, Signer};
use chain::{ChainArgs, NetworkName, Role, SolanaServed};
use indexer::{HealthVerdict, Indexer};
use shared::model::vault::VaultState;
use shared::{Chain, HealthPost};

async fn seeded() -> Option<SolanaServed> {
    let (Ok(chains), Ok(deployments)) = (
        std::env::var("SHONIN_SOLANA_CHAINS"),
        std::env::var("SHONIN_SOLANA_DEPLOYMENTS"),
    ) else {
        eprintln!("no seeded Solana cluster named: skipping");
        return None;
    };
    let args = ChainArgs {
        network: NetworkName::Local,
        chains: PathBuf::from(chains),
        deployments_dir: PathBuf::from(deployments),
        vault_chain: Some(Chain::Solana),
    };
    let target = args.solana_targets().unwrap().pop().unwrap();
    Some(target.serve_solana().await.unwrap())
}

fn store() -> PathBuf {
    std::env::temp_dir().join(format!("shonin-indexer-solana-{}", std::process::id()))
}

#[tokio::test]
async fn the_seeded_solana_vaults_are_in_the_record_with_their_figures_and_history() {
    let Some(served) = seeded().await else {
        return;
    };
    let indexer = Indexer::new(vec![Chain::Solana], store(), None);
    indexer.add_solana(served).await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await.clone();

    let info = snapshot
        .chain(Chain::Solana)
        .expect("the chain is in the snapshot");
    assert_eq!(info.key, "solana:localnet");
    assert_eq!(info.chain_id, None);
    assert!(info.block > 0 && info.as_of.is_some());

    let open: Vec<_> = snapshot
        .strategies
        .iter()
        .filter(|strategy| strategy.vault.state == VaultState::Open)
        .collect();
    assert!(!open.is_empty(), "the seed leaves a vault open");
    for strategy in &snapshot.strategies {
        assert!(
            strategy.stale_as_of.is_none(),
            "{} was read whole",
            strategy.slug
        );
        assert!(strategy.slug.starts_with("solana-"));
        assert!(strategy.vault.address.parse::<Pubkey>().is_ok());
    }
    let live = open[0];
    assert!(live.vault.value > 0.0 && live.vault.share_price > 0.0);
    assert!(
        live.record.is_some(),
        "{}'s record is rebuilt from its valuations",
        live.slug
    );
    assert!(
        live.events
            .iter()
            .any(|event| event.kind == shared::EventKind::Deposited),
        "{}'s deposits are read from its history",
        live.slug
    );
    assert!(snapshot.totals.vault_capital > 0.0);
    // A vault the admin stopped and closed is shown as stopped.
    assert!(
        snapshot
            .strategies
            .iter()
            .any(|strategy| strategy.vault.state == VaultState::Closed && strategy.kill.is_some())
    );
}

#[tokio::test]
async fn a_solana_health_report_is_taken_only_from_the_vault_s_blessed_executor() {
    let Some(served) = seeded().await else {
        return;
    };
    let client = &served.client;
    let platform = &served.platform;
    let executor = client.role(Role::Executor).unwrap();
    let attestor = client.role(Role::Attestor).unwrap();
    let viewer = client.role(Role::Viewer).unwrap();

    let vaults = indexer::solana::vaults(client).await.unwrap();
    let (vault, account) = vaults
        .iter()
        .find(|(_, vault)| vault.state == chain::solana::programs::vault::state::VaultState::Open)
        .expect("the seed leaves a vault open");
    // A blessing lasts minutes on the fast clock: bless the executor again.
    platform
        .bless_executor(
            client,
            executor,
            attestor,
            &executor.pubkey(),
            vault,
            &account.bundle_hash,
        )
        .await
        .unwrap();

    let indexer = Indexer::new(vec![Chain::Solana], store(), None);
    indexer.add_solana(served.clone()).await;
    let report = |at: i64| shared::Health {
        signals: vec![],
        entries: vec![],
        updated: indexer::read::moment(
            (time::OffsetDateTime::now_utc().unix_timestamp() + at) as u64,
        ),
        signed_by: executor.pubkey().to_string(),
        at: 0,
    };
    let post = |by: &chain::solana::Keypair, report: shared::Health| {
        let digest =
            HealthPost::solana_digest(&served.client.cluster_value(), &vault.to_string(), &report);
        HealthPost {
            vault: vault.to_string(),
            signature: by.sign_message(&digest).to_string(),
            report,
            signer: Some(by.pubkey().to_string()),
        }
    };

    assert_eq!(
        indexer
            .take_health(Chain::Solana, post(viewer, report(-60)))
            .await,
        HealthVerdict::NotBlessed,
        "a key never blessed for the vault"
    );
    let mut claimed = post(viewer, report(-60));
    claimed.signer = Some(executor.pubkey().to_string());
    assert_eq!(
        indexer.take_health(Chain::Solana, claimed).await,
        HealthVerdict::NotBlessed,
        "a post naming the executor's key, signed by another"
    );
    assert_eq!(
        indexer
            .take_health(Chain::Solana, post(executor, report(-60)))
            .await,
        HealthVerdict::Taken
    );
    assert_eq!(
        indexer
            .take_health(Chain::Solana, post(executor, report(-120)))
            .await,
        HealthVerdict::Stale,
        "an older report can't be put back"
    );
}
