//! On a seeded local Solana cluster with the keeper running, the keeper
//! collects an open vault's bill with the running total its executor signed:
//! it asks the executor, puts the signature in an Ed25519 instruction ahead
//! of `keep`, and the vault records the total as collected.
//!
//! It needs the cluster, its deployment and the keeper running against it,
//! asking the executor at `SHONIN_TEST_EXECUTOR_PORT`, which this test
//! serves, and `SHONIN_TEST_SOLANA_RPC` and `SHONIN_TEST_SOLANA_DEPLOYMENT`
//! naming the cluster and its deployment file. Without them it says so and
//! passes.

use std::sync::Arc;
use std::time::Duration;

use chain::solana::keys::{DEVELOPMENT_MNEMONIC, RoleKeys};
use chain::solana::programs::vault::state::VaultState;
use chain::solana::{Platform, Signer, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind, Role, SolanaDeployments};

#[tokio::test]
async fn a_solana_vault_s_bill_is_collected_with_the_executor_s_signed_total() {
    let (Ok(rpc), Ok(deployment), Ok(port)) = (
        std::env::var("SHONIN_TEST_SOLANA_RPC"),
        std::env::var("SHONIN_TEST_SOLANA_DEPLOYMENT"),
        std::env::var("SHONIN_TEST_EXECUTOR_PORT"),
    ) else {
        eprintln!("no seeded Solana cluster with a keeper given: skipping");
        return;
    };
    let config = ChainConfig {
        label: "Solana (test)".into(),
        rpc: rpc.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: rpc,
        logs_rpc: None,
        log_range: None,
    };
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    let client = SolanaClient::connect_with(
        &ChainKey::new(ChainKind::Solana, "localnet"),
        &config,
        Some(keys),
    )
    .await
    .unwrap();
    let deployments =
        SolanaDeployments::from_json(&std::fs::read_to_string(deployment).unwrap()).unwrap();
    let platform = Platform::from_deployments(&deployments).unwrap();

    // An open vault with something in its tank, and the executor's key
    // blessed for it now (a blessing is short on the fast clock).
    let (address, vault) = platform
        .vaults(&client)
        .await
        .unwrap()
        .into_iter()
        .find(|(_, vault)| vault.state == VaultState::Open && vault.tank.balance > 0)
        .expect("no open vault with a tank: seed the cluster first");
    let executor = client.role(Role::Executor).unwrap().insecure_clone();
    let attestor = client.role(Role::Attestor).unwrap();
    platform
        .bless_executor(
            &client,
            &executor,
            attestor,
            &executor.pubkey(),
            &address,
            &vault.bundle_hash,
        )
        .await
        .unwrap();

    // The executor's answer: a total above what the vault has collected,
    // signed now.
    let total = vault.tank.usage_settled + 1_000;
    let as_of = client.now().await.unwrap() - 1;
    let digest = chain::solana::programs::vault::upkeep::usage_digest(
        &vault.cluster,
        &address,
        total,
        as_of,
    );
    let signature = executor.sign_message(&digest).to_string();
    let path = format!("/usage/{}/{address}", client.cluster);
    let body = serde_json::json!({
        "total": total.to_string(),
        "as_of": as_of,
        "signature": signature,
        "signer": executor.pubkey().to_string(),
    });
    let served = axum::Router::new().route(
        &path,
        axum::routing::get(move || {
            let body = body.clone();
            async move { axum::Json(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, served).await });

    // The keeper asks once a chain-day for an open vault (a few seconds on
    // the fast clock), and collects in a round after.
    let mut collected = false;
    for _ in 0..180 {
        let now = platform.vault(&client, &address).await.unwrap();
        if now.tank.usage_settled == total && now.tank.usage_as_of == as_of {
            collected = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    server.abort();
    assert!(
        collected,
        "the keeper never collected the signed total for {address}"
    );
}
