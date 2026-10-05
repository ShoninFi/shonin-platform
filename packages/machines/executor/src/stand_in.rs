//! A stand-in chain for the executor's tests.
//!
//! The executor is a binary with no library target, so this lives beside the
//! code as a test module. It answers the handful of JSON-RPC calls the meter
//! makes, so the meter can be driven end to end without a node: `eth_chainId`,
//! `eth_call` on the vault, and `eth_getBlockByNumber` for the chain's clock.
//! It also has logs for a vault that halts, and takes transfers from the
//! accounts it signs for while never managing to say what became of them.
//!
//! [`serving`] is a second stand-in, for a serve's start: it follows a key's
//! blessing and the venue's approval of it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolCall;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::prices::Allowances as OnChainAllowances;
use chain::bindings::station::IStationVault;
use chain::bindings::vault::{IVault, Tank, VaultState};
use serde_json::{Value, json};

/// What the stand-in chain holds, and so what every read returns.
pub struct Chain {
    pub tank: Tank,
    pub allowances: OnChainAllowances,
    pub timestamp: u64,
    pub bundle: B256,
    /// The chain's head block.
    pub head: u64,
    /// The block ranges the logs were asked for, in order.
    pub log_reads: Vec<(u64, u64)>,
    /// A vault that halted, and the block it halted in.
    pub halted: Option<(Address, u64)>,
    /// How long before the head's timestamp the vault halted.
    pub halted_secs_ago: u64,
    /// What each transaction it was sent carried, in order. They all land,
    /// and every address holds their sum; but every read of a receipt is
    /// turned away, as a gateway that is rate limiting does.
    pub sent: Vec<U256>,
    /// Whether what was sent is still in the pool: the balance as of the
    /// latest block is nothing, and only the pending state holds the sum.
    pub in_the_pool: bool,
    /// Whether the node is down: every call answers with an HTTP error.
    pub down: bool,
    /// How often the factory's list of vaults was read. The list is empty.
    pub vault_list_reads: usize,
}

pub type Shared = Arc<Mutex<Chain>>;

/// A chain whose vault's tank holds `balance` and has collected `settled`,
/// with modest allowances.
pub fn chain(balance: u64, settled: u64) -> Shared {
    Arc::new(Mutex::new(Chain {
        tank: Tank {
            balance: U256::from(balance),
            keepAt: U256::ZERO,
            usageSettled: U256::from(settled),
            usageAsOf: 0,
            lastCollection: 0,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
        allowances: chain::allowances(&shared::Allowances::modest()),
        timestamp: 1_700_000_000,
        bundle: B256::repeat_byte(0x42),
        head: 16,
        log_reads: Vec::new(),
        halted: None,
        halted_secs_ago: 0,
        sent: Vec::new(),
        in_the_pool: false,
        down: false,
        vault_list_reads: 0,
    }))
}

/// Answer one `eth_call` by its selector, as the contracts would.
fn answer_call(state: &Chain, input: &[u8]) -> Vec<u8> {
    let selector: [u8; 4] = input[..4].try_into().unwrap();
    match selector {
        IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&state.tank),
        IVault::allowancesCall::SELECTOR => {
            IVault::allowancesCall::abi_encode_returns(&state.allowances)
        }
        IVault::bundleHashCall::SELECTOR => {
            IVault::bundleHashCall::abi_encode_returns(&state.bundle)
        }
        IVault::homeStationCall::SELECTOR => {
            IVault::homeStationCall::abi_encode_returns(&Address::repeat_byte(0x55))
        }
        // No key approved at the station.
        chain::bindings::station::IStation::approvedKeyCall::SELECTOR => {
            chain::bindings::station::IStation::approvedKeyCall::abi_encode_returns(&Address::ZERO)
        }
        IVault::stateCall::SELECTOR => IVault::stateCall::abi_encode_returns(&VaultState::Open),
        // Made a day before the chain's clock.
        IVault::createdAtCall::SELECTOR => {
            IVault::createdAtCall::abi_encode_returns(&(state.timestamp - 86_400))
        }
        IStationVault::haltedAtCall::SELECTOR => {
            let at = if state.halted.is_some() {
                state.timestamp - state.halted_secs_ago
            } else {
                0
            };
            U256::from(at).to_be_bytes::<32>().to_vec()
        }
        // An empty vault: nothing waiting, nothing short.
        IVault::cashCall::SELECTOR
        | IVault::sharePriceCall::SELECTOR
        | IVault::totalValueCall::SELECTOR
        | IVault::requestCountCall::SELECTOR
        | IVault::claimCountCall::SELECTOR => U256::ZERO.to_be_bytes::<32>().to_vec(),
        IVault::idleFloorCall::SELECTOR => [0u8; 96].to_vec(),
        IVault::usageDigestCall::SELECTOR => {
            let call = IVault::usageDigestCall::abi_decode(input).unwrap();
            let mut seed = call.total.to_be_bytes::<32>().to_vec();
            seed.extend_from_slice(&call.asOf.to_be_bytes());
            IVault::usageDigestCall::abi_encode_returns(&alloy::primitives::keccak256(seed))
        }
        _ => panic!("the stand-in chain was asked something it does not answer: {selector:?}"),
    }
}

async fn rpc(State(state): State<Shared>, Json(request): Json<Value>) -> Response {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let result = {
        let mut state = state.lock().unwrap();
        if state.down && method == "eth_call" {
            return (StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
        }
        match method.as_str() {
            "eth_chainId" => json!("0x7a69"),
            "eth_accounts" => json!([]),
            "eth_blockNumber" => json!(format!("0x{:x}", state.head)),
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_feeHistory" => json!({
                "oldestBlock": "0x10",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x1"]],
            }),
            "eth_getLogs" => {
                let filter = &request["params"][0];
                let block = |name: &str| {
                    u64::from_str_radix(filter[name].as_str().unwrap().trim_start_matches("0x"), 16)
                        .unwrap()
                };
                let range = (block("fromBlock"), block("toBlock"));
                state.log_reads.push(range);
                let asked = filter["address"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                match state.halted {
                    Some((vault, at))
                        if asked == format!("{vault:?}").to_lowercase()
                            && range.0 <= at
                            && at <= range.1 =>
                    {
                        halted_log(vault, at)
                    }
                    _ => json!([]),
                }
            }
            "eth_sendTransaction" => {
                let value = request["params"][0]["value"]
                    .as_str()
                    .and_then(|value| value.parse::<U256>().ok())
                    .unwrap_or_default();
                state.sent.push(value);
                json!(B256::with_last_byte(state.sent.len() as u8))
            }
            "eth_getBalance" => {
                if state.in_the_pool && request["params"][1].as_str() != Some("pending") {
                    json!(U256::ZERO)
                } else {
                    json!(state.sent.iter().fold(U256::ZERO, |sum, sent| sum + sent))
                }
            }
            "eth_getTransactionReceipt" => {
                return (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response();
            }
            "eth_call" => {
                let tx = &request["params"][0];
                let data = tx["input"]
                    .as_str()
                    .or_else(|| tx["data"].as_str())
                    .expect("a call carries data");
                let input = alloy::hex::decode(data).unwrap();
                if input.starts_with(&IVaultFactory::vaultsCall::SELECTOR) {
                    state.vault_list_reads += 1;
                    let none = IVaultFactory::vaultsCall::abi_encode_returns(&Vec::new());
                    json!(alloy::hex::encode_prefixed(none))
                } else {
                    json!(alloy::hex::encode_prefixed(answer_call(&state, &input)))
                }
            }
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.timestamp = state.timestamp;
                block.header.inner.number = 16;
                serde_json::to_value(block).unwrap()
            }
            other => panic!("the stand-in chain was asked {other}"),
        }
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

/// The log a vault writes when it halts.
fn halted_log(vault: Address, at: u64) -> Value {
    use alloy::sol_types::SolEvent;
    let event = IVault::Halted {
        reason: chain::names::name(b"killed"),
        haltedAt: 1_700_000_000,
    };
    json!([{
        "address": vault,
        "topics": [IVault::Halted::SIGNATURE_HASH],
        "data": alloy::hex::encode_prefixed(event.encode_data()),
        "blockNumber": format!("0x{at:x}"),
        "blockHash": B256::repeat_byte(0xbb),
        "transactionHash": B256::repeat_byte(0xcc),
        "transactionIndex": "0x0",
        "logIndex": "0x0",
        "removed": false,
    }])
}

/// Serve the stand-in chain on a port of its own, and say where.
pub async fn start(state: Shared) -> String {
    let app = Router::new().route("/", post(rpc)).with_state(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

/// Deployments with every contract and role at the zero address: the
/// stand-in answers by selector, whatever the address.
pub fn deployments() -> chain::Deployments {
    let zero = json!(format!("{:?}", Address::ZERO));
    let mut deployments = json!({
        "chain_key": "evm:31337", "chain_id": 31337, "rpc_url": "", "deployed_block": 0,
    });
    deployments["roles"] = [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| (role.to_string(), zero.clone()))
    .collect::<serde_json::Map<_, _>>()
    .into();
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
        deployments[name] = zero.clone();
    }
    serde_json::from_value(deployments).unwrap()
}

pub async fn client(url: &str) -> chain::Client {
    let config = chain::ChainConfig {
        label: "stand-in".to_string(),
        rpc: url.to_string(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: Some(2_500.0),
        public_rpc: url.to_string(),
        confirmations: None,
    };
    chain::Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .expect("the stand-in chain answers")
}

pub fn key(client: &chain::Client) -> chain::KeyClient {
    client
        .with_key(alloy::signers::local::PrivateKeySigner::random())
        .unwrap()
}

/// The total the meter last signed for a vault.
pub fn latest_total(totals: &crate::usage::Totals, vault: Address) -> Option<u64> {
    totals
        .lock()
        .unwrap()
        .get(&(31337, vault))
        .map(|signed| signed.total.parse::<u64>().unwrap())
}

/// Wait until the meter has signed a total, above `after` if given, and
/// return it.
pub async fn signed_total(
    totals: &crate::usage::Totals,
    vault: Address,
    after: Option<u64>,
) -> u64 {
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(total) = latest_total(totals, vault)
            && after.is_none_or(|previous| total > previous)
        {
            return total;
        }
    }
    panic!("the meter never signed a total");
}

/// A stand-in for a serve's start that knows enough of the registry, the
/// station and the venue to follow a key's blessing and its venue approval:
/// a blessing sent sets the key's `blessedUntil` a TTL from now, and an
/// approval sent sets the venue's `agentValidUntil` for the key to that
/// blessing, as `Station.approveKey` does. Anything it isn't told to answer
/// (the strategy's id, say) reverts, which ends a serve right after the
/// key's blessing and approval are settled.
pub mod serving {
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use alloy::consensus::{Transaction, TxEnvelope};
    use alloy::eips::eip2718::Decodable2718;
    use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
    use alloy::sol_types::SolCall;
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router, routing::post};
    use chain::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
    use chain::bindings::station::{IStation, IStationVault};
    use chain::bindings::vault::{IVault, Tank, VaultState};
    use chain::bindings::venue::ILocalVenue;
    use serde_json::{Value, json};

    pub const TTL: u64 = 72 * 3_600;
    pub const NOW: u64 = 1_700_000_000;

    pub struct Node {
        pub state: VaultState,
        pub halted_at: u64,
        pub grace: u64,
        pub bundle: B256,
        pub station: Address,
        /// The key the station records as approved.
        pub approved_key: Address,
        /// The registry's `blessedUntil` for the key.
        pub blessed_until: u64,
        /// What the venue holds the key approved until: what decides whether
        /// the agent's orders are taken.
        pub venue_until: u64,
        /// How many approvals to come revert on the chain.
        pub failing_approvals: u32,
        /// How many approvals to come land while the node answers the send
        /// with an error, as one whose reply is lost does.
        pub unanswered_approvals: u32,
        /// Keys whose blessing has lapsed; every other key is blessed until
        /// `blessed_until`.
        pub lapsed: HashSet<Address>,
        /// Whether the registry has revoked the station's driver, so every
        /// approval reverts.
        pub driver_revoked: bool,
        /// Approvals the node has taken and not yet mined: no receipt, and
        /// nothing changed, until the test mines them.
        pub held_approvals: bool,
        /// The approvals taken while `held_approvals`, waiting to be mined.
        pub pending: Vec<(B256, Address)>,
        /// Every transaction sent, by hash, with the selector it called.
        pub sent: Vec<(B256, [u8; 4])>,
        /// The transactions that reverted.
        reverted: HashSet<B256>,
        /// How many times the tank was read: once each time a meter signs a
        /// total, and once for each health report (every 15 s).
        pub tank_reads: usize,
        /// How many times the allowances were read: once for every start of
        /// the agent, and every two seconds while one runs or waits.
        pub agent_starts: usize,
    }

    pub type Shared = Arc<Mutex<Node>>;

    pub fn node(state: VaultState) -> Shared {
        Arc::new(Mutex::new(Node {
            state,
            halted_at: 0,
            grace: 600,
            bundle: B256::repeat_byte(0x42),
            station: Address::repeat_byte(0x55),
            approved_key: Address::ZERO,
            blessed_until: 0,
            venue_until: 0,
            failing_approvals: 0,
            unanswered_approvals: 0,
            lapsed: HashSet::new(),
            driver_revoked: false,
            held_approvals: false,
            pending: Vec::new(),
            sent: Vec::new(),
            reverted: HashSet::new(),
            tank_reads: 0,
            agent_starts: 0,
        }))
    }

    /// How many approvals were sent, landed or not.
    pub fn approvals(node: &Shared) -> usize {
        node.lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(_, selector)| *selector == IStation::approveKeyCall::SELECTOR)
            .count()
    }

    /// Mine the approvals the node has been holding: each lands as
    /// `Station.approveKey` does.
    pub fn mine(node: &Shared) {
        let mut node = node.lock().unwrap();
        node.held_approvals = false;
        for (hash, key) in std::mem::take(&mut node.pending) {
            node.venue_until = node.blessed_until;
            node.approved_key = key;
            node.sent.push((hash, IStation::approveKeyCall::SELECTOR));
        }
    }

    fn reverted(id: Value) -> Response {
        Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}}))
            .into_response()
    }

    fn answer_call(node: &Node, input: &[u8]) -> Option<Vec<u8>> {
        let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
        Some(match selector {
            IVault::stateCall::SELECTOR => IVault::stateCall::abi_encode_returns(&node.state),
            IStationVault::haltedAtCall::SELECTOR => {
                IStationVault::haltedAtCall::abi_encode_returns(&node.halted_at)
            }
            IStationVault::haltGraceCall::SELECTOR => {
                IStationVault::haltGraceCall::abi_encode_returns(&node.grace)
            }
            IVault::bundleHashCall::SELECTOR => {
                IVault::bundleHashCall::abi_encode_returns(&node.bundle)
            }
            IVault::homeStationCall::SELECTOR => {
                IVault::homeStationCall::abi_encode_returns(&node.station)
            }
            IStation::approvedKeyCall::SELECTOR => {
                IStation::approvedKeyCall::abi_encode_returns(&node.approved_key)
            }
            IAttestationRegistry::isBlessedForCall::SELECTOR => {
                let signer = IAttestationRegistry::isBlessedForCall::abi_decode(input)
                    .ok()?
                    .signer;
                IAttestationRegistry::isBlessedForCall::abi_encode_returns(
                    &(node.blessed_until > NOW && !node.lapsed.contains(&signer)),
                )
            }
            IAttestationRegistry::blessedUntilCall::SELECTOR => {
                IAttestationRegistry::blessedUntilCall::abi_encode_returns(&node.blessed_until)
            }
            ILocalVenue::agentValidUntilCall::SELECTOR => {
                ILocalVenue::agentValidUntilCall::abi_encode_returns(&node.venue_until)
            }
            IAttestationRegistry::isImageAllowedCall::SELECTOR => {
                IAttestationRegistry::isImageAllowedCall::abi_encode_returns(&true)
            }
            IAttestationRegistry::isProjectAllowedCall::SELECTOR => {
                IAttestationRegistry::isProjectAllowedCall::abi_encode_returns(&true)
            }
            IStation::pinnedDriverCall::SELECTOR => {
                IStation::pinnedDriverCall::abi_encode_returns(&Address::repeat_byte(0x66))
            }
            IAttestationRegistry::isDriverRevokedCall::SELECTOR => {
                IAttestationRegistry::isDriverRevokedCall::abi_encode_returns(&node.driver_revoked)
            }
            IAttestationRegistry::blessingTtlCall::SELECTOR => {
                IAttestationRegistry::blessingTtlCall::abi_encode_returns(&TTL)
            }
            IAttestationRegistry::keyInfoCall::SELECTOR => {
                IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                    imageDigest: B256::ZERO,
                    projectId: B256::ZERO,
                    kind: ImageKind::Executor,
                    strategyHash: node.bundle,
                    vaultChain: U256::from(31337u64),
                    vault: Address::repeat_byte(0x11),
                    trustRoot: B256::ZERO,
                    verifier: Address::ZERO,
                    blessedUntil: node.blessed_until,
                })
            }
            IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&Tank {
                balance: U256::from(1_000_000_000_000u64),
                keepAt: U256::ZERO,
                usageSettled: U256::ZERO,
                usageAsOf: 0,
                lastCollection: 0,
                escapeRequestedAt: 0,
                capCarried: U256::ZERO,
                fillPending: false,
            }),
            IVault::allowancesCall::SELECTOR => IVault::allowancesCall::abi_encode_returns(
                &chain::allowances(&shared::Allowances::modest()),
            ),
            IVault::usageDigestCall::SELECTOR => {
                IVault::usageDigestCall::abi_encode_returns(&keccak256(input))
            }
            _ => return None,
        })
    }

    fn receipt(hash: B256, landed: bool) -> Value {
        json!({
            "transactionHash": hash,
            "transactionIndex": "0x0",
            "blockHash": B256::repeat_byte(0xbb),
            "blockNumber": "0x11",
            "from": Address::ZERO,
            "to": Address::repeat_byte(0x22),
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x3b9aca00",
            "contractAddress": null,
            "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "status": if landed { "0x1" } else { "0x0" },
            "type": "0x2",
        })
    }

    async fn rpc(State(node): State<Shared>, Json(request): Json<Value>) -> Response {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let mut node = node.lock().unwrap();
        let result = match method.as_str() {
            "eth_chainId" => json!("0x7a69"),
            "eth_accounts" => json!([]),
            "eth_blockNumber" => json!("0x10"),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = NOW;
                block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                serde_json::to_value(block).unwrap()
            }
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
            "eth_feeHistory" => json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x3b9aca00"]],
            }),
            "eth_estimateGas" => json!("0x30000"),
            "eth_getTransactionCount" => {
                json!(format!("0x{:x}", node.sent.len() + node.pending.len()))
            }
            // Plenty: no stake is needed.
            "eth_getBalance" => json!(U256::from(10u128.pow(19))),
            "eth_sign" => json!(Bytes::from(vec![0x11u8; 65])),
            "eth_getLogs" => json!([]),
            "eth_sendRawTransaction" => {
                let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                let envelope = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                let hash = *envelope.tx_hash();
                let selector: [u8; 4] = envelope.input()[..4].try_into().unwrap();
                match selector {
                    IAttestationRegistry::blessCall::SELECTOR => node.blessed_until = NOW + TTL,
                    IStation::approveKeyCall::SELECTOR if node.driver_revoked => {
                        node.reverted.insert(hash);
                    }
                    IStation::approveKeyCall::SELECTOR if node.failing_approvals > 0 => {
                        node.failing_approvals -= 1;
                        node.reverted.insert(hash);
                    }
                    // Taken and not yet mined: the node holds it, and its
                    // receipt, until the test mines it.
                    IStation::approveKeyCall::SELECTOR if node.held_approvals => {
                        let key = IStation::approveKeyCall::abi_decode(envelope.input())
                            .map(|call| call.key)
                            .unwrap_or_default();
                        node.pending.push((hash, key));
                        return Json(json!({"jsonrpc": "2.0", "id": id, "result": hash}))
                            .into_response();
                    }
                    // As `Station.approveKey`: the venue holds the key
                    // approved until its blessing runs out.
                    IStation::approveKeyCall::SELECTOR => {
                        node.venue_until = node.blessed_until;
                        if let Ok(call) = IStation::approveKeyCall::abi_decode(envelope.input()) {
                            node.approved_key = call.key;
                        }
                    }
                    _ => {}
                }
                node.sent.push((hash, selector));
                if selector == IStation::approveKeyCall::SELECTOR && node.unanswered_approvals > 0 {
                    node.unanswered_approvals -= 1;
                    return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "the reply was lost"}}))
                        .into_response();
                }
                json!(hash)
            }
            "eth_getTransactionReceipt" => {
                let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                if node.sent.iter().any(|(hash, _)| *hash == asked) {
                    receipt(asked, !node.reverted.contains(&asked))
                } else {
                    Value::Null
                }
            }
            "eth_call" => {
                let tx = &request["params"][0];
                let data = tx["input"]
                    .as_str()
                    .or_else(|| tx["data"].as_str())
                    .unwrap();
                let input = alloy::hex::decode(data).unwrap();
                if input.starts_with(&IVault::tankCall::SELECTOR) {
                    node.tank_reads += 1;
                }
                if input.starts_with(&IVault::allowancesCall::SELECTOR) {
                    node.agent_starts += 1;
                }
                match answer_call(&node, &input) {
                    Some(out) => json!(alloy::hex::encode_prefixed(out)),
                    None => return reverted(id),
                }
            }
            _ => return reverted(id),
        };
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
    }

    /// Serve the stand-in on a port of its own, and say where.
    pub async fn start(node: Shared) -> String {
        let app = Router::new().route("/", post(rpc)).with_state(node);
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }
}

/// A stand-in for a public chain (the node unlocks no accounts) at a base fee
/// of the test's choosing, where a key holds what the test says. It records
/// the transfers it is asked for, and has a tank and allowances to report on.
pub mod public {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use alloy::primitives::{Address, B256, U256};
    use alloy::sol_types::SolCall;
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router, routing::post};
    use chain::bindings::attestation::IAttestationRegistry;
    use chain::bindings::vault::{IVault, Tank};
    use serde_json::{Value, json};

    pub struct Public {
        /// The latest block's base fee, and the chain's gas price.
        pub base_fee: u128,
        pub key_holds: u128,
        /// Transfers asked of the node, by value.
        pub sent: Vec<U256>,
    }

    pub type Node = Arc<Mutex<Public>>;

    fn hex(value: u128) -> Value {
        json!(format!("0x{value:x}"))
    }

    async fn rpc(State(node): State<Node>, Json(request): Json<Value>) -> Response {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let mut node = node.lock().unwrap();
        let result = match method.as_str() {
            "eth_chainId" => json!("0x7a69"),
            // Nothing unlocked: not a development chain.
            "eth_accounts" => json!([]),
            "eth_blockNumber" => json!("0x10"),
            "eth_gasPrice" => hex(node.base_fee),
            "eth_feeHistory" => json!({
                "oldestBlock": "0x10",
                "baseFeePerGas": [hex(node.base_fee), hex(node.base_fee)],
                "gasUsedRatio": [0.5],
                "reward": [[hex(1)]],
            }),
            "eth_getBalance" => json!(U256::from(node.key_holds)),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = 1_700_000_000;
                block.header.inner.base_fee_per_gas = Some(node.base_fee as u64);
                serde_json::to_value(block).unwrap()
            }
            "eth_sendTransaction" => {
                let value = request["params"][0]["value"]
                    .as_str()
                    .and_then(|value| value.parse::<U256>().ok())
                    .unwrap_or_default();
                node.sent.push(value);
                json!(B256::with_last_byte(node.sent.len() as u8))
            }
            "eth_getTransactionReceipt" => {
                let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                json!({
                    "transactionHash": asked,
                    "transactionIndex": "0x0",
                    "blockHash": B256::repeat_byte(0xbb),
                    "blockNumber": "0x11",
                    "from": Address::ZERO,
                    "to": Address::ZERO,
                    "cumulativeGasUsed": "0x5208",
                    "gasUsed": "0x5208",
                    "effectiveGasPrice": "0x3b9aca00",
                    "contractAddress": null,
                    "logs": [],
                    "logsBloom": format!("0x{}", "00".repeat(256)),
                    "status": "0x1",
                    "type": "0x2",
                })
            }
            "eth_call" => {
                let tx = &request["params"][0];
                let data = tx["input"]
                    .as_str()
                    .or_else(|| tx["data"].as_str())
                    .unwrap();
                let input = alloy::hex::decode(data).unwrap();
                let selector: [u8; 4] = input[..4].try_into().unwrap();
                let out = match selector {
                    IVault::bundleHashCall::SELECTOR => {
                        IVault::bundleHashCall::abi_encode_returns(&B256::repeat_byte(0x42))
                    }
                    IAttestationRegistry::isBlessedForCall::SELECTOR => {
                        IAttestationRegistry::isBlessedForCall::abi_encode_returns(&true)
                    }
                    IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&Tank {
                        balance: U256::from(1_000_000_000u64),
                        keepAt: U256::ZERO,
                        usageSettled: U256::ZERO,
                        usageAsOf: 0,
                        lastCollection: 0,
                        escapeRequestedAt: 0,
                        capCarried: U256::ZERO,
                        fillPending: false,
                    }),
                    IVault::allowancesCall::SELECTOR => IVault::allowancesCall::abi_encode_returns(
                        &chain::allowances(&shared::Allowances::modest()),
                    ),
                    other => panic!("the stand-in was asked {other:?}"),
                };
                json!(alloy::hex::encode_prefixed(out))
            }
            other => panic!("the stand-in was asked {other}"),
        };
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
    }

    /// Start a public chain at this base fee, with the key holding
    /// `key_holds`.
    pub async fn start(base_fee: u128, key_holds: u128) -> (Node, chain::Client) {
        let node = Arc::new(Mutex::new(Public {
            base_fee,
            key_holds,
            sent: Vec::new(),
        }));
        let app = Router::new().route("/", post(rpc)).with_state(node.clone());
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = super::client(&format!("http://{address}")).await;
        assert!(!client.is_development(), "the stand-in is a public chain");
        (node, client)
    }
}

/// A stand-in for a serve of a killed vault a minute into its ten-minute
/// grace, whose key is blessed for a while and approved at the venue until
/// then, so a serve goes straight to running the agent. It counts the
/// agent's starts by its reads of the allowances, which nothing else makes
/// during such a serve.
pub mod killed {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use alloy::primitives::{Address, B256, U256};
    use alloy::sol_types::SolCall;
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::{Json, Router, routing::post};
    use chain::bindings::attestation::IAttestationRegistry;
    use chain::bindings::vault::IVault;
    use serde_json::{Value, json};

    use chain::bindings::attestation::{ImageKind, KeyInfo};
    use chain::bindings::station::{IStation, IStationVault};
    use chain::bindings::strategies::{IStrategyRegistry, Strategy};
    use chain::bindings::vault::VaultState;
    use chain::bindings::venue::ILocalVenue;

    pub const NOW: u64 = 1_700_000_000;
    const TTL: u64 = 72 * 3_600;

    pub struct Killed {
        pub key: Address,
        pub manifest_hash: B256,
        /// How many times the allowances were read: once for every start of
        /// the agent, and every two seconds while one runs.
        pub agent_starts: usize,
        /// How many reads to come fail, as a node that stops answering for a
        /// moment does.
        pub failing_reads: usize,
        /// Another key the station approves instead of `key`, once another
        /// executor has taken the vault over. Every key is blessed here.
        pub approved_instead: Option<Address>,
        /// The vault's allowances as it reports them.
        pub allowances: shared::Allowances,
    }

    pub type Shared = Arc<Mutex<Killed>>;

    fn answer_call(node: &mut Killed, input: &[u8]) -> Vec<u8> {
        let selector: [u8; 4] = input[..4].try_into().unwrap();
        let word = |value: u64| U256::from(value).to_be_bytes::<32>().to_vec();
        match selector {
            IVault::stateCall::SELECTOR => {
                IVault::stateCall::abi_encode_returns(&VaultState::Killed)
            }
            IStationVault::haltedAtCall::SELECTOR => word(NOW - 60),
            IStationVault::haltGraceCall::SELECTOR => word(600),
            IVault::bundleHashCall::SELECTOR => {
                IVault::bundleHashCall::abi_encode_returns(&B256::repeat_byte(0x42))
            }
            IVault::homeStationCall::SELECTOR => {
                IVault::homeStationCall::abi_encode_returns(&Address::repeat_byte(0x55))
            }
            IVault::strategyIdCall::SELECTOR => word(7),
            IVault::allowancesCall::SELECTOR => {
                node.agent_starts += 1;
                IVault::allowancesCall::abi_encode_returns(&chain::allowances(&node.allowances))
            }
            IVault::cashCall::SELECTOR
            | IVault::sharePriceCall::SELECTOR
            | IVault::totalValueCall::SELECTOR
            | IVault::requestCountCall::SELECTOR
            | IVault::claimCountCall::SELECTOR => word(0),
            IVault::idleFloorCall::SELECTOR => [0u8; 96].to_vec(),
            IStation::approvedKeyCall::SELECTOR => IStation::approvedKeyCall::abi_encode_returns(
                &node.approved_instead.unwrap_or(node.key),
            ),
            IAttestationRegistry::isBlessedForCall::SELECTOR => {
                IAttestationRegistry::isBlessedForCall::abi_encode_returns(&true)
            }
            IAttestationRegistry::blessingTtlCall::SELECTOR => word(TTL),
            IAttestationRegistry::blessedUntilCall::SELECTOR => word(NOW + TTL * 3 / 4),
            ILocalVenue::agentValidUntilCall::SELECTOR => word(NOW + TTL * 3 / 4),
            IAttestationRegistry::keyInfoCall::SELECTOR => {
                IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                    imageDigest: B256::ZERO,
                    projectId: B256::ZERO,
                    kind: ImageKind::Executor,
                    strategyHash: B256::repeat_byte(0x42),
                    vaultChain: U256::from(31337u64),
                    vault: Address::repeat_byte(0x11),
                    trustRoot: B256::ZERO,
                    verifier: Address::ZERO,
                    blessedUntil: NOW + TTL * 3 / 4,
                })
            }
            IStrategyRegistry::strategyOfCall::SELECTOR => {
                IStrategyRegistry::strategyOfCall::abi_encode_returns(&Strategy {
                    author: Address::ZERO,
                    sourceHash: B256::ZERO,
                    bundleHash: B256::repeat_byte(0x42),
                    manifestHash: node.manifest_hash,
                    registeredAt: 0,
                    chains: Vec::new(),
                    bridges: Vec::new(),
                    restricted: Vec::new(),
                })
            }
            other => panic!("the stand-in was asked {other:?}"),
        }
    }

    async fn rpc(State(node): State<Shared>, Json(request): Json<Value>) -> Response {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let mut node = node.lock().unwrap();
        if method == "eth_call" && node.failing_reads > 0 {
            node.failing_reads -= 1;
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
        }
        let result = match method.as_str() {
            "eth_chainId" => json!("0x7a69"),
            "eth_accounts" => json!([]),
            "eth_blockNumber" => json!("0x10"),
            "eth_getLogs" => json!([]),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = NOW;
                serde_json::to_value(block).unwrap()
            }
            "eth_call" => {
                let tx = &request["params"][0];
                let data = tx["input"]
                    .as_str()
                    .or_else(|| tx["data"].as_str())
                    .unwrap();
                let input = alloy::hex::decode(data).unwrap();
                json!(alloy::hex::encode_prefixed(answer_call(&mut node, &input)))
            }
            other => panic!("the stand-in was asked {other}"),
        };
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
    }

    /// A component that won't load: the agent stops as soon as it starts, as
    /// one that faults does.
    pub const NOT_A_COMPONENT: &[u8] = b"not a component";

    /// A store holding `component` as the vault's bundle, and a manifest
    /// naming the local driver, recorded for that bundle and that component,
    /// under the hash the registry records.
    pub fn store(dir: &std::path::Path, component: &[u8]) -> B256 {
        let manifest = shared::Manifest::from_toml(
            "[vault]\nchain = \"base\"\n[description]\ntitle = \"t\"\ntext = \"x\"\n[drivers.local]\n",
        )
        .unwrap();
        let canonical = shared::Canonical {
            strategy_id: 7,
            vault_chain: shared::Chain::Base,
            author: "0xa11ce".into(),
            bundle_hash: format!("sha256:{}", alloy::hex::encode(B256::repeat_byte(0x42))),
            component_hash: shared::manifest::hash_bytes(component),
            source_hash: "sha256:00".into(),
            sealed: false,
            world: "shonin:agent@0.1.0".into(),
            toolchain: "rust".into(),
            builder_image: "sha256:builder".into(),
            manifest,
        };
        let hash = canonical.hash();
        let manifest_dir = dir.join(hash.replace(':', "-"));
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("manifest.json"),
            serde_json::to_vec(&canonical).unwrap(),
        )
        .unwrap();
        let path = crate::bundle_path(dir, B256::repeat_byte(0x42));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, component).unwrap();
        B256::from_slice(&alloy::hex::decode(hash.trim_start_matches("sha256:")).unwrap())
    }

    pub async fn start(node: Shared) -> chain::Client {
        let app = Router::new().route("/", post(rpc)).with_state(node);
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        super::client(&format!("http://{address}")).await
    }
}
