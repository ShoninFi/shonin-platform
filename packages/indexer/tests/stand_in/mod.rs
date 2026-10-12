//! A stand-in node: one vault, every read answered with zeros unless the
//! test's own answer says otherwise.

#![allow(dead_code)]

use std::sync::Arc;

use alloy::primitives::{Address, B256, Log as Primitive};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

pub const DEPLOYED: u64 = 1;
pub const VAULT: &str = "00000000000000000000000000000000000000be";

pub fn vault() -> Address {
    format!("0x{VAULT}").parse().unwrap()
}

pub fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// Stable fixture identities include the complete height without byte wrapping.
pub fn block_hash(number: u64) -> B256 {
    branch_hash(number, 0)
}
fn branch_hash(number: u64, marker: u8) -> B256 {
    let mut bytes = [0; 32];
    bytes[0] = marker;
    bytes[24..].copy_from_slice(&number.checked_add(1).unwrap().to_be_bytes());
    B256::from(bytes)
}
pub fn hash_number(hash: B256) -> u64 {
    u64::from_be_bytes(hash.as_slice()[24..].try_into().unwrap())
        .checked_sub(1)
        .unwrap()
}
/// Hash-bound reads reach the same numbered fixture oracle, including empty logs.
pub fn numbered_data_request(request: &Value) -> Value {
    let mut request = request.clone();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let hash = match method.as_str() {
        "eth_getBlockByHash" => request["params"][0].as_str(),
        "eth_call" => request["params"][1]["blockHash"].as_str(),
        "eth_getLogs" => request["params"][0]["blockHash"].as_str(),
        _ => None,
    };
    if let Some(hash) = hash {
        let number = hash_number(hash.parse().unwrap());
        let tag = json!(format!("0x{number:x}"));
        match method.as_str() {
            "eth_getBlockByHash" => {
                request["method"] = json!("eth_getBlockByNumber");
                request["params"][0] = tag;
            }
            "eth_call" => request["params"][1] = tag,
            "eth_getLogs" => {
                let f = request["params"][0].as_object_mut().unwrap();
                f.remove("blockHash");
                f.insert("fromBlock".into(), tag.clone());
                f.insert("toBlock".into(), tag);
            }
            _ => unreachable!(),
        }
    }
    request
}
/// A reference EVM BLOCKHASH answer for the fixture's honest linear headers.
pub fn ancestry_answer(request: &Value, time: impl Fn(u64) -> u64) -> Option<Value> {
    use alloy::primitives::U256;
    use chain::header_read::HeaderRead;
    if request["method"] != "eth_call"
        || !input(request).starts_with(&selector::<HeaderRead::readCall>())
        || request["params"].as_array()?.len() != 3
    {
        return None;
    }
    let hash: B256 = request["params"][1]["blockHash"].as_str()?.parse().ok()?;
    let number = hash_number(hash);
    let call = HeaderRead::readCall::abi_decode(
        &alloy::hex::decode(input(request).strip_prefix("0x")?).ok()?,
    )
    .ok()?;
    let ancestor: u64 = call.ancestorNumber.try_into().ok()?;
    if ancestor >= number || number - ancestor > 256 {
        return None;
    }
    let answer = HeaderRead::readReturn {
        number: U256::from(number),
        timestamp: U256::from(time(number)),
        parentHash: branch_hash(number - 1, hash[0]),
        ancestorHash: branch_hash(ancestor, hash[0]),
    };
    Some(json!(format!(
        "0x{}",
        alloy::hex::encode(HeaderRead::readCall::abi_encode_returns(&answer))
    )))
}
/// A read-only client with explicit fixture catch-up limits via normal config.
pub async fn client_with_limits(config: &ChainConfig, limits: &[(&str, u64)]) -> Client {
    let mut text = include_str!("../../../../config/chains.toml").to_string();
    text.push_str("\n[rpc.chains.\"evm:31337\"]\n");
    for (field, value) in limits {
        text.push_str(&format!("{field} = {value}\n"));
    }
    let configs = chain::ChainConfigs::from_toml(&text, |_| None).unwrap();
    chain::Target {
        network: chain::NetworkName::Mainnet,
        vault_chain: shared::Chain::Base,
        key: "evm:31337".parse().unwrap(),
        config: config.clone(),
        configs,
        deployments_dir: std::path::PathBuf::new(),
    }
    .connect()
    .await
    .unwrap()
}

/// The requested numbered header, or the current head for a dynamic tag.
pub fn requested_block(request: &Value, head: u64) -> u64 {
    request["params"][0]
        .as_str()
        .and_then(|tag| tag.strip_prefix("0x"))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .unwrap_or(head)
}

/// What a request asks for: the call's input, lowercased, for `eth_call`.
pub fn input(request: &Value) -> String {
    request["params"][0]["input"]
        .as_str()
        .or_else(|| request["params"][0]["data"].as_str())
        .unwrap_or("0x")
        .to_ascii_lowercase()
}

/// Whether a log request is for the vault's own logs.
pub fn asks_the_vault(request: &Value) -> bool {
    let address = &request["params"][0]["address"];
    address
        .as_str()
        .is_some_and(|address| address.to_lowercase().ends_with(VAULT))
        || address.as_array().is_some_and(|addresses| {
            addresses.iter().any(|address| {
                address
                    .as_str()
                    .is_some_and(|address| address.to_lowercase().ends_with(VAULT))
            })
        })
}

/// A log of the vault's, in `block` at `index`.
pub fn logged<E: SolEvent>(event: E, block: u64, index: u64) -> Value {
    let log = alloy::rpc::types::Log {
        inner: Primitive {
            address: vault(),
            data: event.encode_log_data(),
        },
        block_hash: Some(block_hash(block)),
        block_number: Some(block),
        block_timestamp: Some(1_790_000_000 + block),
        transaction_hash: Some(B256::repeat_byte(0x40 + index as u8)),
        transaction_index: Some(0),
        log_index: Some(index),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// A test's answer to a request: `Some(Ok(result))`, `Some(Err(message))`
/// for a JSON-RPC error, or `None` for the default.
pub type Answer = Arc<dyn Fn(&Value) -> Option<Result<Value, String>> + Send + Sync>;

/// Translate hash-bound requests to the fixture's numbered state oracle.
/// The fixture has short branches with repeated-byte identities; a backend
/// refuses hashes outside its current branch rather than answering at latest.
fn numbered_request(request: &Value, answer: &Answer) -> Result<Value, String> {
    let mut numbered = request.clone();
    let method = request["method"].as_str().unwrap_or_default();
    let hash = match method {
        "eth_getBlockByHash" => request["params"][0].as_str(),
        "eth_call" => request["params"][1]["blockHash"].as_str(),
        "eth_getLogs" => request["params"][0]["blockHash"].as_str(),
        _ => None,
    };
    if let Some(hash) = hash {
        let hash: B256 = hash.parse().map_err(|_| "unknown block hash")?;
        let latest = json!({"method":"eth_getBlockByNumber","params":["latest",false]});
        let head = match answer(&latest) {
            Some(result) => result?,
            None => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                serde_json::to_value(block).unwrap()
            }
        };
        let head: alloy::rpc::types::Block<alloy::rpc::types::Transaction> =
            serde_json::from_value(head).map_err(|_| "unavailable latest header")?;
        let marker = if head.header.hash == B256::ZERO {
            0
        } else {
            head.header.hash[0] ^ head.header.inner.number as u8
        };
        let number = hash_number(hash);
        if hash != branch_hash(number, marker) || number > head.header.inner.number {
            return Err("unknown block hash".into());
        }
        let tag = json!(format!("0x{number:x}"));
        match method {
            "eth_getBlockByHash" => {
                numbered["method"] = json!("eth_getBlockByNumber");
                numbered["params"][0] = tag;
            }
            "eth_call" => numbered["params"][1] = tag,
            "eth_getLogs" => {
                let filter = numbered["params"][0].as_object_mut().unwrap();
                filter.remove("blockHash");
                filter.insert("fromBlock".into(), tag.clone());
                filter.insert("toBlock".into(), tag);
            }
            _ => unreachable!(),
        }
    }
    Ok(numbered)
}

pub async fn a_node(answer: Answer) -> String {
    let app =
        Router::new().route(
            "/",
            post(move |axum::Json(request): axum::Json<Value>| {
                let answer = answer.clone();
                async move {
                    let id = request["id"].clone();
                    let request = match numbered_request(&request, &answer) {
                        Ok(request) => request,
                        Err(message) => return axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})),
                    };
                    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                    let reply = match answer(&request) {
                        Some(Ok(result)) => ok(result),
                        Some(Err(message)) => json!({
                            "jsonrpc": "2.0",
                            "id": request["id"].clone(),
                            "error": {"code": -32000, "message": message}
                        }),
                        None => {
                            let method = request["method"].as_str().unwrap_or_default();
                            match method {
                                "eth_chainId" => ok(json!("0x7a69")),
                                "eth_blockNumber" => ok(json!("0x10")),
                                "eth_getBlockByNumber" => {
                                    let mut block = alloy::rpc::types::Block::<
                                        alloy::rpc::types::Transaction,
                                    >::default(
                                    );
                                    let number = requested_block(&request, 0x10);
                                    block.header.inner.number = number;
                                    block.header.hash = B256::repeat_byte(number as u8);
                                    block.header.inner.timestamp = 1_790_000_000;
                                    ok(serde_json::to_value(block).unwrap())
                                }
                                "eth_call" => {
                                    let data = input(&request);
                                    if data
                                        == selector::<
                                            chain::bindings::factory::IVaultFactory::vaultsCall,
                                        >()
                                    {
                                        ok(json!(format!(
                                            "0x{}{}{}",
                                            "0".repeat(62) + "20",
                                            "0".repeat(63) + "1",
                                            "0".repeat(24) + VAULT
                                        )))
                                    } else {
                                        ok(json!(format!("0x{}", "0".repeat(64 * 128))))
                                    }
                                }
                                _ => ok(json!([])),
                            }
                        }
                    };
                    let mut reply = reply;
                    if request["method"] == "eth_getBlockByNumber"
                        && let Ok(mut block) = serde_json::from_value::<alloy::rpc::types::Block<alloy::rpc::types::Transaction>>(reply["result"].clone())
                    {
                        let number = block.header.inner.number;
                        let marker = if block.header.hash == B256::ZERO {0} else {block.header.hash[0] ^ number as u8};
                        block.header.hash = branch_hash(number, marker);
                        block.header.inner.parent_hash = if number == 0 {B256::ZERO} else {branch_hash(number-1,marker)};
                        // False positives force the event paths to run in these fixtures.
                        block.header.inner.logs_bloom = alloy::primitives::Bloom::from([255;256]);
                        reply["result"] = serde_json::to_value(block).unwrap();
                    }
                    if request["method"] == "eth_getLogs"
                        && let Some(logs) = reply["result"].as_array_mut()
                    {
                        let filter: alloy::rpc::types::Filter =
                            serde_json::from_value(request["params"][0].clone()).unwrap();
                        logs.retain(|log| {
                            filter.rpc_matches(
                                &serde_json::from_value::<alloy::rpc::types::Log>(log.clone())
                                    .unwrap(),
                            )
                        });
                    }
                    axum::Json(reply)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

pub fn deployments() -> Deployments {
    let one = "0x0000000000000000000000000000000000000001";
    let mut fields = vec![
        r#""chain_key":"evm:31337""#.to_string(),
        r#""chain_id":31337"#.to_string(),
        r#""rpc_url":"""#.to_string(),
        format!(r#""deployed_block":{DEPLOYED}"#),
    ];
    for name in [
        "usd",
        "venue",
        "attestation_verifier",
        "attestation",
        "publishing",
        "prices",
        "strategies",
        "reviews",
        "profiles",
        "factory",
        "station_factory",
        "driver",
        "bridge",
    ] {
        fields.push(format!(r#""{name}":"{one}""#));
    }
    let roles: Vec<String> = [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| format!(r#""{role}":"{one}""#))
    .collect();
    fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
    Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
}

pub async fn an_indexer(answer: Answer, name: &str) -> Arc<Indexer> {
    let node = a_node(answer).await;
    let config = ChainConfig {
        label: "stand-in".to_string(),
        rpc: node.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: node,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = Client::connect(&config, Signing::ReadOnly).await.unwrap();
    let store = std::env::temp_dir().join(format!(
        "shonin-indexer-stand-in-{name}-{}",
        std::process::id()
    ));
    let indexer = Arc::new(Indexer::new(vec![shared::Chain::Base], store, None));
    indexer
        .add(Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse::<ChainKey>().unwrap(),
            client,
            deployments: deployments(),
            native_usd: None,
            public_rpc: String::new(),
        })
        .await;
    indexer
}
