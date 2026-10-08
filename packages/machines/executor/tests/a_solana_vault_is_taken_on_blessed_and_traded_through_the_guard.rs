//! The executor alone on a local Solana cluster the devnet has set up and is
//! driving: it takes on a vault whose bundle is in its store, keeps its key
//! blessed past the first blessing's end, has the station approve the key,
//! trades at the stand-in venue through the station's guard, serves a signed
//! usage total that verifies, and posts signed health reports.
//!
//! The test plays the build service, the Reviewer, the author, a depositor
//! and the keeper with the development keys, as the seed does, so the vault
//! runs a real component. As the keeper, it relays the blessing of the key
//! the executor publishes on its status port, sealed for the development
//! checker, and gives the key lamports once the registry counts it: the
//! executor itself holds no role account. It needs a cluster, so it is ignored unless asked
//! for:
//!
//! ```text
//! SHONIN_TEST_SOLANA_CHAINS=<chains.toml> \
//! SHONIN_TEST_SOLANA_DEPLOYMENTS=<deployments dir> \
//! SHONIN_TEST_COMPONENT=<an agent's component.wasm> \
//! cargo test -p shonin-executor --test a_solana_vault_is_taken_on_blessed_and_traded_through_the_guard -- --ignored --nocapture
//! ```

mod status_port;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{Json, Router, routing::post};
use chain::solana::executor::{Blessed, SignedTotal, signature_from_base58, verify_ed25519};
use chain::solana::platform::{Declared, Registration, VaultAccounts};
use chain::solana::programs::{local_driver, prices, publishing, vault};
use chain::solana::{Platform, Pubkey, Signer, SolanaClient, name};
use chain::{ChainArgs, NetworkName, Role};
use shared::HealthPost;

const PATIENCE: Duration = Duration::from_secs(300);
const DEPOSIT: u64 = 40_000_000_000;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// What the build service would have written for a funding-basis bundle on
/// a Solana vault: the component under the bundle's hash, the manifest under
/// its own. Answers the bundle's and the manifest's hashes.
fn store_bundle(store: &Path, component: &[u8], id: u64, author: &Pubkey) -> ([u8; 32], [u8; 32]) {
    let bundle = sha256(&[component, &id.to_le_bytes(), b"solana"]);
    let manifest = shared::Manifest::from_toml(
        r#"
[vault]
chain = "solana"

[description]
title = "Funding basis on Solana"
text = "Holds the spot asset and shorts the perpetual against it."

[drivers.local]
"#,
    )
    .unwrap();
    let canonical = shared::Canonical {
        strategy_id: id,
        vault_chain: shared::Chain::Solana,
        author: author.to_string(),
        bundle_hash: format!("sha256:{}", hex::encode(bundle)),
        component_hash: shared::manifest::hash_bytes(component),
        source_hash: format!("sha256:{}", hex::encode(sha256(&[b"source", &bundle]))),
        sealed: false,
        world: "shonin:agent@0.1.0".to_string(),
        toolchain: "rust · wasm32-wasip2".to_string(),
        builder_image: "0x00".to_string(),
        manifest,
    };
    let manifest_hash = shared::manifest::hash_to_bytes(&canonical.hash()).unwrap();
    let bundle_dir = store.join(format!("sha256-{}", hex::encode(bundle)));
    std::fs::create_dir_all(&bundle_dir).unwrap();
    std::fs::write(bundle_dir.join("component.wasm"), component).unwrap();
    let manifest_dir = store.join(format!("sha256-{}", hex::encode(manifest_hash)));
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&canonical).unwrap(),
    )
    .unwrap();
    (bundle, manifest_hash)
}

struct Cluster {
    client: SolanaClient,
    platform: Platform,
}

impl Cluster {
    async fn publish(&self, store: &Path, component: &[u8]) -> (u64, [u8; 32]) {
        let author = self.client.role(Role::Author).unwrap();
        let builder = self.client.role(Role::Builder).unwrap();
        let reviewer = self.client.role(Role::Reviewer).unwrap();
        let attestor = self.client.role(Role::Attestor).unwrap();
        for (service, key) in [("builder", builder), ("reviewer", reviewer)] {
            self.platform
                .bless_service(&self.client, key, attestor, service, &key.pubkey())
                .await
                .unwrap();
        }
        let (submit, id) = self
            .platform
            .ix_submit(
                &self.client,
                &author.pubkey(),
                sha256(&[b"upload", component]),
            )
            .await
            .unwrap();
        self.client.send(vec![submit], &[author]).await.unwrap();
        let (bundle, manifest) = store_bundle(store, component, id, &author.pubkey());
        let registration = Registration {
            id,
            author: author.pubkey(),
            source_hash: sha256(&[b"source", &bundle]),
            bundle_hash: bundle,
            manifest_hash: manifest,
            lists: publishing::state::Lists {
                chains: vec![publishing::state::ChainNeed {
                    chain: name("solana"),
                    drivers: vec![local_driver::NAME],
                }],
                bridges: vec![],
                restricted: vec![],
            },
        };
        self.platform
            .register(&self.client, builder, &registration, builder, reviewer)
            .await
            .unwrap();
        let review = publishing::state::ReviewInput {
            bundle_hash: bundle,
            source_hash: registration.source_hash,
            model_id_hash: sha256(&[b"model"]),
            prompt_hash: sha256(&[b"prompt"]),
            report_hash: sha256(&[b"report"]),
            ratings: [3; 6],
            finding_count: 0,
        };
        let ix = self
            .platform
            .ix_submit_review(
                &self.client,
                &reviewer.pubkey(),
                &reviewer.pubkey(),
                id,
                review,
            )
            .await
            .unwrap();
        self.client.send(vec![ix], &[reviewer]).await.unwrap();
        (id, bundle)
    }

    async fn activate(&self, id: u64) -> VaultAccounts {
        let author = self.client.role(Role::Author).unwrap();
        let drivers = [Declared::local()];
        let (_, statement) = self
            .platform
            .restricted_for(&self.client, &author.pubkey(), id, &drivers)
            .await
            .unwrap();
        let factory = self.platform.factory(&self.client).await.unwrap();
        let modest = shared::Allowances::modest();
        let ix = self
            .platform
            .ix_activate(
                &self.client,
                &author.pubkey(),
                id,
                &drivers,
                prices::Allowances {
                    fuel_per_sec: modest.fuel_per_sec,
                    memory_mib: modest.memory_mib,
                    requests_per_min: modest.requests_per_min,
                    bytes_per_day: modest.bytes_per_day,
                    streams: modest.streams,
                    inference_ms_per_min: modest.inference_ms_per_min,
                    gas_per_day: (modest.gas_per_day * 1e6) as u64,
                },
                500,
                factory.terms.minimum_tank.max(100_000_000),
                statement,
                vault::factory::terms_hash(&factory.terms),
            )
            .await
            .unwrap();
        self.client.send(vec![ix], &[author]).await.unwrap();
        self.platform.vault_accounts(id)
    }

    /// A depositor in, and priced: the keeper's part played here.
    async fn deposit_and_price(&self, v: &VaultAccounts) {
        let viewer = self.client.role(Role::Viewer).unwrap();
        let keeper = self.client.role(Role::Keeper).unwrap();
        let statement = self
            .platform
            .vault(&self.client, &v.vault)
            .await
            .unwrap()
            .restricted_hash;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let ix = self.platform.ix_request_deposit(
            &v.vault,
            &viewer.pubkey(),
            &viewer.pubkey(),
            DEPOSIT,
            Some(statement),
            nonce,
        );
        self.client.send(vec![ix], &[viewer]).await.unwrap();
        let began = Instant::now();
        while self
            .platform
            .shares(&self.client, &v.vault, &viewer.pubkey())
            .await
            .unwrap()
            == 0
        {
            assert!(began.elapsed() < PATIENCE, "the deposit was never priced");
            let _ = self.platform.value(&self.client, keeper, v).await;
            let _ = self.platform.settle_all(&self.client, keeper, v).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Play the keeper once: relay the blessing of the key the executor
    /// publishes for `vault`, and give it lamports once it is counted.
    /// Answers the key, once published.
    async fn keep(&self, port: u16, vault: &Pubkey, bundle: &[u8; 32]) -> Option<Pubkey> {
        let status: serde_json::Value =
            reqwest::get(format!("http://127.0.0.1:{port}/attestation"))
                .await
                .ok()?
                .json()
                .await
                .ok()?;
        let cluster = status["solana"].as_array()?.first()?.clone();
        let leaf = cluster["leaves"]
            .as_array()?
            .iter()
            .find(|leaf| leaf["vault"].as_str() == Some(&vault.to_string()))?
            .clone();
        let key: Pubkey = leaf["key"].as_str()?.parse().ok()?;
        let keeper = self.client.role(Role::Keeper).unwrap();
        let now = self.client.now().await.unwrap();
        if self.blessed(&key, vault, bundle).await.due(now) {
            let journal = alloy::hex::decode(cluster["journal"].as_str()?).ok()?;
            let journal = <chain::solana::programs::registry::state::Journal as chain::solana::programs::anchor_lang::AnchorDeserialize>::deserialize(&mut journal.as_slice()).ok()?;
            let seal: chain::solana::attest::SolanaSeal =
                serde_json::from_value(cluster["seal"].clone()).ok()?;
            let proof = leaf["proof"]
                .as_array()?
                .iter()
                .map(|node| {
                    alloy::hex::decode(node.as_str().unwrap())
                        .unwrap()
                        .try_into()
                        .unwrap()
                })
                .collect();
            let config = self.platform.registry_config(&self.client).await.unwrap();
            let ixs = self
                .platform
                .bless_with(
                    &config,
                    now,
                    &keeper.pubkey(),
                    &journal,
                    &seal,
                    &chain::solana::attest::KeyLeaf {
                        vault_chain: self.platform.cluster,
                        vault: vault.to_bytes(),
                        strategy_hash: *bundle,
                        key,
                        proof,
                    },
                )
                .unwrap();
            let _ = self.client.send(ixs, &[keeper]).await;
        } else if self.client.lamports(&key).await.unwrap() < 1_000_000_000 {
            let _ = self
                .client
                .send(
                    vec![chain::solana::executor::ix_fund(
                        &keeper.pubkey(),
                        &key,
                        2_000_000_000,
                    )],
                    &[keeper],
                )
                .await;
        }
        Some(key)
    }

    async fn blessed(&self, key: &Pubkey, vault: &Pubkey, bundle: &[u8; 32]) -> Blessed {
        Blessed::read(&self.client, &self.platform, key, vault, bundle)
            .await
            .unwrap()
    }
}

/// The API, as far as health reports go: what was posted.
async fn health_sink() -> (u16, Arc<Mutex<Vec<HealthPost>>>) {
    let posts: Arc<Mutex<Vec<HealthPost>>> = Default::default();
    let kept = posts.clone();
    let app = Router::new().route(
        "/api/solana/health",
        post(move |Json(post): Json<HealthPost>| {
            let kept = kept.clone();
            async move {
                kept.lock().unwrap().push(post);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (port, posts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local Solana cluster the devnet has set up"]
async fn a_solana_vault_is_taken_on_blessed_and_traded_through_the_guard() {
    let chains = env_path("SHONIN_TEST_SOLANA_CHAINS");
    let deployments = env_path("SHONIN_TEST_SOLANA_DEPLOYMENTS");
    let component = std::fs::read(env_path("SHONIN_TEST_COMPONENT")).unwrap();
    let args = ChainArgs {
        network: NetworkName::Local,
        chains: chains.clone(),
        deployments_dir: deployments.clone(),
        vault_chain: Some(shared::Chain::Solana),
    };
    let served = args.solana_targets().unwrap()[0]
        .serve_solana()
        .await
        .unwrap();
    let cluster = Cluster {
        client: served.client.clone(),
        platform: served.platform.clone(),
    };
    let store = tempfile_dir();

    let (id, bundle) = cluster.publish(&store, &component).await;
    let v = cluster.activate(id).await;
    cluster.deposit_and_price(&v).await;
    println!(
        "vault {} (strategy {id}) is open with a depositor's money",
        v.vault
    );

    let (api_port, posts) = health_sink().await;
    let mut executor = status_port::start(
        Command::new(env!("CARGO_BIN_EXE_executor"))
            .args(["--network", "local", "--vault-chain", "solana", "--chains"])
            .arg(&chains)
            .arg("--deployments-dir")
            .arg(&deployments)
            .arg("--store")
            .arg(&store)
            .args(["--api-url", &format!("http://127.0.0.1:{api_port}")]),
    )
    .await;
    let began = Instant::now();
    let said = || executor.said.lock().unwrap().clone();

    // Blessed (the keeper relaying) and approved at the station.
    let (key, first) = loop {
        let Some(key) = cluster.keep(executor.port, &v.vault, &bundle).await else {
            assert!(
                began.elapsed() < PATIENCE,
                "the key was never published:\n{}",
                said()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let blessed = cluster.blessed(&key, &v.vault, &bundle).await;
        let station = cluster
            .platform
            .station(&cluster.client, &v.station)
            .await
            .unwrap();
        if blessed.holds(cluster.client.now().await.unwrap()) && station.drivers[0].key == key {
            break (key, blessed);
        }
        assert!(
            began.elapsed() < PATIENCE,
            "never blessed and approved:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    println!("key {key} blessed until {} and approved", first.until);

    // Money at the venue through the guard, and a position held there.
    let authority = cluster
        .platform
        .station(&cluster.client, &v.station)
        .await
        .unwrap()
        .drivers[0]
        .authority;
    let held = loop {
        let account = cluster
            .platform
            .margin_account(&cluster.client, &authority)
            .await
            .unwrap();
        if let Some(account) = account
            && !account.positions.is_empty()
        {
            break account;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the agent never traded:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    println!(
        "the station holds {:?} at the venue, cash {}",
        held.positions
            .iter()
            .map(|p| (
                String::from_utf8_lossy(&p.market)
                    .trim_end_matches('\0')
                    .to_string(),
                p.size
            ))
            .collect::<Vec<_>>(),
        held.cash
    );

    // Blessed again before the first blessing ran out, and still approved.
    loop {
        cluster.keep(executor.port, &v.vault, &bundle).await;
        let blessed = cluster.blessed(&key, &v.vault, &bundle).await;
        if blessed.until > first.until {
            assert!(blessed.holds(cluster.client.now().await.unwrap()));
            println!("blessed again until {}", blessed.until);
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "never blessed again:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // A signed total that verifies under the vault's own cluster value.
    let url = format!(
        "http://127.0.0.1:{}/usage/{}/{}",
        executor.port, cluster.client.cluster, v.vault
    );
    let signed: SignedTotal = loop {
        if let Ok(answer) = reqwest::get(&url).await
            && answer.status().is_success()
        {
            break answer.json().await.unwrap();
        }
        assert!(began.elapsed() < PATIENCE, "no total served:\n{}", said());
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let account = cluster
        .platform
        .vault(&cluster.client, &v.vault)
        .await
        .unwrap();
    assert_eq!(signed.signer, key.to_string());
    assert!(
        signed.verify(&account.cluster),
        "the served total does not verify: {signed:?}"
    );
    assert!(signed.as_of <= cluster.client.now().await.unwrap());
    println!(
        "served total {} as of {}, verified",
        signed.total, signed.as_of
    );

    // A signed report, by the same key.
    loop {
        if let Some(post) = posts.lock().unwrap().first().cloned() {
            assert_eq!(post.signer.as_deref(), Some(key.to_string().as_str()));
            let digest = HealthPost::solana_digest(
                &cluster.client.cluster_value(),
                &post.vault,
                &post.report,
            );
            let signature = signature_from_base58(&post.signature).unwrap();
            assert!(verify_ed25519(&key, &digest, signature));
            println!("a health report signed by the vault's key arrived");
            break;
        }
        assert!(began.elapsed() < PATIENCE, "no report arrived:\n{}", said());
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let _ = executor.child.kill();
    let _ = std::fs::remove_dir_all(&store);
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-solana-executor-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
