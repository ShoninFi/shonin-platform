//! A Solana cluster's record is read in one batch of accounts a refresh
//! (`getMultipleAccounts`, a hundred to a call): the vaults, their claims and
//! stations, their registrations, reviews and authors' profiles, the
//! platform's settings and the cluster's clock. The vaults are listed again
//! only when the factory's count of them changes (or after a while), and a
//! vault's requests and history are read again only once its accounts have
//! changed. Each leg names the venue of the drivers the vault's station
//! pinned, by the label the published list of drivers gives it.
//!
//! Here: a fake cluster with one vault, its station pinned to the `orca`
//! driver, and nothing in its history.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use chain::solana::programs::anchor_lang::{
    AccountDeserialize, AccountSerialize, Discriminator, Space,
};
use chain::solana::programs::{publishing, registry, station, vault as sv};
use chain::solana::{Platform, Pubkey, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind, SolanaDeployments, SolanaServed};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::Chain;

#[derive(Default)]
struct Node {
    accounts: Mutex<HashMap<Pubkey, Vec<u8>>>,
    asked: Mutex<HashMap<String, u64>>,
}

impl Node {
    fn asked(&self, method: &str) -> u64 {
        self.asked.lock().unwrap().get(method).copied().unwrap_or(0)
    }

    fn put(&self, address: Pubkey, data: Vec<u8>) {
        self.accounts.lock().unwrap().insert(address, data);
    }

    fn account(&self, address: &Pubkey) -> Value {
        match self.accounts.lock().unwrap().get(address) {
            Some(data) => json!({
                "lamports": 1_000_000,
                "owner": sv::ID.to_string(),
                "executable": false,
                "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
                "rentEpoch": 0
            }),
            None => Value::Null,
        }
    }
}

async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Json<Value> {
    let method = request["method"].as_str().unwrap().to_string();
    *node
        .asked
        .lock()
        .unwrap()
        .entry(method.clone())
        .or_default() += 1;
    let params = &request["params"];
    let result = match method.as_str() {
        "getGenesisHash" => json!("stand-in-genesis"),
        "getAccountInfo" => {
            let address: Pubkey = params[0].as_str().unwrap().parse().unwrap();
            json!({"context": {"slot": 42}, "value": node.account(&address)})
        }
        "getMultipleAccounts" => {
            let value: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|address| node.account(&address.as_str().unwrap().parse().unwrap()))
                .collect();
            json!({"context": {"slot": 42}, "value": value})
        }
        "getProgramAccounts" => {
            // The vaults, by their discriminator; no request.
            let wanted = params[1]["filters"][0]["memcmp"]["bytes"]
                .as_str()
                .map(|bytes| bs58::decode(bytes).into_vec().unwrap());
            let listed: Vec<Pubkey> = node
                .accounts
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, data)| {
                    wanted
                        .as_ref()
                        .is_some_and(|wanted| data.starts_with(wanted))
                        && params[1]["filters"]
                            .as_array()
                            .is_some_and(|f| f.len() == 1)
                })
                .map(|(address, _)| *address)
                .collect();
            let found: Vec<Value> = listed
                .iter()
                .map(|address| json!({"pubkey": address.to_string(), "account": node.account(address)}))
                .collect();
            json!(found)
        }
        "getSignaturesForAddress" => json!([]),
        method => panic!("unexpected {method}"),
    };
    Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
}

fn zeroed<T: AccountDeserialize + Discriminator>(len: usize) -> T {
    let mut data = vec![0; 8 + len];
    data[..8].copy_from_slice(T::DISCRIMINATOR);
    T::try_deserialize(&mut &data[..]).unwrap()
}

fn bytes<T: AccountSerialize>(value: &T) -> Vec<u8> {
    let mut data = Vec::new();
    value.try_serialize(&mut data).unwrap();
    data
}

/// A name as the programs write it: zero-padded to 32 bytes.
fn name(text: &str) -> [u8; 32] {
    let mut out = [0; 32];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

const NOW: u64 = 1_800_000_000;

/// The clock sysvar at `NOW`.
fn clock() -> Vec<u8> {
    let mut data = vec![0; 40];
    data[32..40].copy_from_slice(&(NOW as i64).to_le_bytes());
    data
}

fn a_vault(station: Pubkey, cash: u64) -> sv::state::Vault {
    let mut vault: sv::state::Vault = zeroed(sv::state::Vault::INIT_SPACE);
    vault.strategy_id = 1;
    vault.author = Pubkey::new_from_array([0xa1; 32]);
    vault.station = station;
    vault.state = sv::state::VaultState::Open;
    vault.created_at = NOW - 86_400;
    vault.cash = cash;
    vault.drivers = vec![name("orca")];
    vault
}

fn a_station(vault: Pubkey) -> station::state::Station {
    station::state::Station {
        vault,
        vault_program: sv::ID,
        vault_dollars: Pubkey::new_unique(),
        dollars: Pubkey::new_unique(),
        mint: Pubkey::new_unique(),
        bundle_hash: [0; 32],
        cluster: [0; 32],
        exit_band_bps: 100,
        idle: 0,
        sent_home: 0,
        stopped: false,
        stopped_at: 0,
        payer: Pubkey::new_unique(),
        bump: 255,
        drivers: vec![station::state::Pinned {
            name: name("orca"),
            program: Pubkey::new_unique(),
            venue: Pubkey::new_unique(),
            authority: Pubkey::new_unique(),
            authority_bump: 255,
            dollars: Pubkey::new_unique(),
            key: Pubkey::default(),
            rules: Vec::new(),
            fixed: Vec::new(),
            addresses: Vec::new(),
            position_mints: Vec::new(),
            token_accounts: Vec::new(),
        }],
    }
}

async fn setup() -> (Arc<Indexer>, Arc<Node>, Pubkey, Pubkey) {
    let node = Arc::new(Node::default());
    let vault = sv::state::vault_address(1).0;
    let station = Pubkey::new_unique();
    node.put(
        chain::solana::programs::anchor_lang::solana_program::sysvar::clock::ID,
        clock(),
    );
    let mut factory: sv::state::Factory = zeroed(sv::state::Factory::INIT_SPACE);
    factory.vault_count = 1;
    node.put(sv::state::factory_address(), bytes(&factory));
    let publishing: publishing::state::Config = zeroed(publishing::state::Config::INIT_SPACE);
    node.put(
        chain::solana::platform::publishing_config(),
        bytes(&publishing),
    );
    let config: registry::state::Config = zeroed(registry::state::Config::INIT_SPACE);
    node.put(registry::read::config_address(), bytes(&config));
    node.put(vault, bytes(&a_vault(station, 5_000_000)));
    node.put(station, bytes(&a_station(vault)));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().route("/", post(rpc)).with_state(node.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let config = ChainConfig {
        label: "stand-in".into(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: url.clone(),
        logs_rpc: None,
        log_range: None,
    };
    let key = ChainKey::new(ChainKind::Solana, "localnet");
    let client = SolanaClient::connect_with(&key, &config, None)
        .await
        .unwrap();
    let platform = Platform {
        cluster_name: "localnet".into(),
        cluster: client.cluster_value(),
        mint: Pubkey::new_unique(),
        admin: Pubkey::new_unique(),
        platform: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        checker: None,
    };
    let indexer = Arc::new(Indexer::new(
        vec![Chain::Solana],
        std::env::temp_dir().join(format!("solana-one-batch-{}", std::process::id())),
        None,
    ));
    indexer
        .add_solana(SolanaServed {
            vault_chain: Chain::Solana,
            key,
            client,
            deployments: SolanaDeployments::from_json("{\"programs\":{}}").unwrap(),
            platform,
            public_rpc: url,
        })
        .await;
    (indexer, node, vault, station)
}

/// How many times each method was asked for during `refresh`.
async fn counted(node: &Node, refresh: impl std::future::Future<Output = ()>) -> [u64; 4] {
    let methods = [
        "getMultipleAccounts",
        "getProgramAccounts",
        "getSignaturesForAddress",
        "getAccountInfo",
    ];
    let before = methods.map(|method| node.asked(method));
    refresh.await;
    let after = methods.map(|method| node.asked(method));
    [0, 1, 2, 3].map(|index| after[index] - before[index])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_solana_cluster_is_read_in_one_batch_of_accounts_a_refresh() {
    let (indexer, node, vault, station) = setup().await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await.clone();
    assert_eq!(snapshot.strategies.len(), 1, "{:?}", snapshot.strategies);
    let strategy = &snapshot.strategies[0];
    assert!(strategy.stale_as_of.is_none());
    assert_eq!(strategy.legs[0].venue, "Orca", "the leg names its venue");

    // Nothing changed: one batch of accounts, no listing, no history.
    let [batches, listings, histories, singles] =
        counted(&node, async { indexer.refresh().await.unwrap() }).await;
    assert_eq!(
        (batches, listings, histories, singles),
        (1, 0, 0, 0),
        "a refresh of an unchanged cluster: batches, listings, histories, single accounts"
    );

    // The vault's account changes: its requests and history are read again.
    node.put(vault, bytes(&a_vault(station, 7_000_000)));
    let [batches, listings, histories, singles] =
        counted(&node, async { indexer.refresh().await.unwrap() }).await;
    assert_eq!(
        (batches, listings, histories, singles),
        (1, 1, 1, 0),
        "a refresh after the vault changed: batches, listings (its requests), histories, single accounts"
    );
    let snapshot = indexer.snapshot.read().await.clone();
    assert!((snapshot.strategies[0].vault.idle - 7.0).abs() < 1e-9);
}

/// What a depositor sees a driver as is its venue, as the published list
/// labels it, or its name where the list has none.
#[test]
fn a_driver_is_shown_as_its_venue() {
    assert_eq!(indexer::solana::venue_of("orca"), "Orca");
    assert_eq!(indexer::solana::venue_of("local"), "Local venue");
    assert_eq!(indexer::solana::venue_of("unlisted"), "unlisted");
}
