//! A Solana vault's claims are in the record: a claim's id is the id of the
//! withdrawal it was booked from, not its place in the vault's count of
//! claims, so the record finds each claim by the `ClaimBooked` its history
//! holds. A claim booked and paid is listed with its owner, shares, state
//! and what it paid, and stays listed once its account is closed and its
//! rent taken back, from what the history says of it.
//!
//! Here: a fake cluster with one vault that has booked one claim (id 3, from
//! its third withdrawal) and paid it, then closed it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use chain::solana::programs::anchor_lang::{
    AccountDeserialize, AccountSerialize, Discriminator, Event, Space,
};
use chain::solana::programs::{publishing, registry, vault as sv};
use chain::solana::{Platform, Pubkey, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind, SolanaDeployments, SolanaServed};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::Chain;
use shared::model::vault::{Claim, ClaimState};

/// A transaction the fake cluster holds: its signature, its time and the
/// vault program's events it logged.
struct Landed {
    signature: String,
    at: i64,
    events: Vec<Vec<u8>>,
}

#[derive(Default)]
struct Node {
    accounts: Mutex<HashMap<Pubkey, Vec<u8>>>,
    /// The vault's transactions, oldest first.
    history: Mutex<Vec<Landed>>,
    vault: Mutex<Option<Pubkey>>,
}

impl Node {
    fn put(&self, address: Pubkey, data: Vec<u8>) {
        self.accounts.lock().unwrap().insert(address, data);
    }

    fn close(&self, address: &Pubkey) {
        self.accounts.lock().unwrap().remove(address);
    }

    fn land(&self, signature: &str, at: i64, events: Vec<Vec<u8>>) {
        self.history.lock().unwrap().push(Landed {
            signature: signature.to_string(),
            at,
            events,
        });
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

    /// The vault's transactions newer than `until`, newest first.
    fn signatures(&self, address: &str, until: Option<&str>) -> Value {
        let vault = self.vault.lock().unwrap().map(|vault| vault.to_string());
        if vault.as_deref() != Some(address) {
            return json!([]);
        }
        let history = self.history.lock().unwrap();
        let listed: Vec<Value> = history
            .iter()
            .rev()
            .take_while(|landed| Some(landed.signature.as_str()) != until)
            .map(|landed| {
                json!({
                    "signature": landed.signature,
                    "slot": 42,
                    "err": null,
                    "blockTime": landed.at,
                })
            })
            .collect();
        json!(listed)
    }

    /// A transaction with one vault instruction naming the vault, and the
    /// events the vault program logged under it.
    fn transaction(&self, signature: &str) -> Value {
        let vault = self.vault.lock().unwrap().unwrap();
        let history = self.history.lock().unwrap();
        let Some(landed) = history.iter().find(|landed| landed.signature == signature) else {
            return Value::Null;
        };
        let mut logs = vec![format!("Program {} invoke [1]", sv::ID)];
        for event in &landed.events {
            logs.push(format!(
                "Program data: {}",
                base64::engine::general_purpose::STANDARD.encode(event)
            ));
        }
        logs.push(format!("Program {} success", sv::ID));
        json!({
            "slot": 42,
            "blockTime": landed.at,
            "meta": {
                "err": null,
                "fee": 5_000,
                "preBalances": [],
                "postBalances": [],
                "innerInstructions": [],
                "logMessages": logs,
            },
            "transaction": {
                "message": {
                    "accountKeys": [vault.to_string(), sv::ID.to_string()],
                    "instructions": [{"programIdIndex": 1, "accounts": [0], "data": ""}],
                }
            }
        })
    }
}

async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Json<Value> {
    let method = request["method"].as_str().unwrap().to_string();
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
            // The vaults, by their discriminator; no request waiting.
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
        "getSignaturesForAddress" => {
            node.signatures(params[0].as_str().unwrap(), params[1]["until"].as_str())
        }
        "getTransaction" => node.transaction(params[0].as_str().unwrap()),
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

const NOW: u64 = 1_800_000_000;
const BOOKED_AT: u64 = NOW - 7_200;
const PAID_AT: u64 = NOW - 3_600;

/// Twenty shares, in share units.
const SHARES: u128 = 20 * 1_000_000_000_000_000_000;
/// $19,978.432856, in dollar units.
const PAID: u64 = 19_978_432_856;
/// The claim's id: the vault's third withdrawal, its first claim.
const CLAIM_ID: u64 = 3;

/// The clock sysvar at `NOW`.
fn clock() -> Vec<u8> {
    let mut data = vec![0; 40];
    data[32..40].copy_from_slice(&(NOW as i64).to_le_bytes());
    data
}

fn a_vault(station: Pubkey) -> sv::state::Vault {
    let mut vault: sv::state::Vault = zeroed(sv::state::Vault::INIT_SPACE);
    vault.strategy_id = 1;
    vault.author = Pubkey::new_from_array([0xa1; 32]);
    vault.station = station;
    vault.state = sv::state::VaultState::Open;
    vault.created_at = NOW - 86_400;
    vault.cash = 5_000_000;
    vault.total_supply = 80 * 1_000_000_000_000_000_000;
    vault.request_count = 3;
    vault.request_cursor = 3;
    vault.claim_count = 1;
    vault
}

fn a_paid_claim(owner: Pubkey) -> sv::state::Claim {
    let mut claim: sv::state::Claim = zeroed(sv::state::Claim::INIT_SPACE);
    claim.id = CLAIM_ID;
    claim.owner = owner;
    claim.shares = SHARES;
    claim.booked_at = BOOKED_AT;
    claim.state = sv::state::ClaimState::Paid;
    claim.paid = PAID;
    claim
}

async fn setup() -> (Arc<Indexer>, Arc<Node>, Pubkey, Pubkey) {
    let node = Arc::new(Node::default());
    let vault = sv::state::vault_address(1).0;
    *node.vault.lock().unwrap() = Some(vault);
    let station = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
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
    node.put(vault, bytes(&a_vault(station)));

    // The claim, booked from the third withdrawal and paid.
    node.put(
        sv::state::claim_address(&vault, CLAIM_ID).0,
        bytes(&a_paid_claim(owner)),
    );
    node.land(
        "booked",
        BOOKED_AT as i64,
        vec![
            sv::events::ClaimBooked {
                claim_id: CLAIM_ID,
                owner,
                shares: SHARES,
            }
            .data(),
        ],
    );
    node.land(
        "paid",
        PAID_AT as i64,
        vec![
            sv::events::Withdrawn {
                request_id: 0,
                owner,
                shares: SHARES,
                assets: PAID,
                fee_shares: 0,
            }
            .data(),
            sv::events::ClaimPaid {
                claim_id: CLAIM_ID,
                assets: PAID,
            }
            .data(),
        ],
    );

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
        std::env::temp_dir().join(format!("solana-claims-{}", std::process::id())),
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
    (indexer, node, vault, owner)
}

async fn claims(indexer: &Indexer) -> Vec<Claim> {
    let snapshot = indexer.snapshot.read().await.clone();
    assert_eq!(snapshot.strategies.len(), 1, "{:?}", snapshot.strategies);
    let strategy = &snapshot.strategies[0];
    assert!(strategy.stale_as_of.is_none(), "the vault was read whole");
    strategy.vault.claims.clone()
}

fn assert_paid(claims: &[Claim], owner: &Pubkey, when: &str) {
    assert_eq!(claims.len(), 1, "{when}: the claim is listed: {claims:?}");
    let claim = &claims[0];
    assert_eq!(claim.id, CLAIM_ID as u32, "{when}: its id");
    assert_eq!(claim.owner, owner.to_string(), "{when}: its owner");
    assert!(
        (claim.shares - 20.0).abs() < 1e-9,
        "{when}: {}",
        claim.shares
    );
    assert_eq!(claim.state, ClaimState::Paid, "{when}: its state");
    let paid = claim.settled_for.expect("what it paid");
    assert!((paid - 19_978.432856).abs() < 1e-6, "{when}: paid {paid}");
    // Twenty shares of the hundred there were before they were burnt.
    assert!(
        (claim.fraction_closed - 0.2).abs() < 1e-9,
        "{when}: {}",
        claim.fraction_closed
    );
    assert_eq!(
        claim.booked,
        time::OffsetDateTime::from_unix_timestamp(BOOKED_AT as i64)
            .unwrap()
            .date(),
        "{when}: booked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_solana_claim_booked_and_paid_is_in_the_record_before_and_after_its_account_closes() {
    let (indexer, node, vault, owner) = setup().await;
    indexer.refresh().await.unwrap();
    assert_paid(&claims(&indexer).await, &owner, "paid, its account open");

    // A refresh with nothing new keeps it.
    indexer.refresh().await.unwrap();
    assert_paid(&claims(&indexer).await, &owner, "read again unchanged");

    // Its account is closed and its rent taken back: the vault's history
    // still says what it was.
    node.close(&sv::state::claim_address(&vault, CLAIM_ID).0);
    node.land("closed", (NOW - 60) as i64, vec![]);
    indexer.refresh().await.unwrap();
    assert_paid(&claims(&indexer).await, &owner, "its account closed");
}
