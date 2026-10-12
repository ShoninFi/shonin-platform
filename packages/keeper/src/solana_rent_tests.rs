use super::*;
use axum::{Json, Router, extract::State, routing::post};
use chain::args::ChainArgs;
use chain::config::NetworkName;
use chain::solana::programs::anchor_lang::{
    AccountDeserialize, AccountSerialize, Discriminator, Space,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Fixture {
    vault: Vault,
    scans: u64,
    fail_scan_at: Option<u64>,
    address: Pubkey,
    payer: Pubkey,
    closable: bool,
    send_attempts: u64,
}

fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let word = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        result.push(DIGITS[(word >> 18) as usize & 63] as char);
        result.push(DIGITS[(word >> 12) as usize & 63] as char);
        result.push(if chunk.len() > 1 {
            DIGITS[(word >> 6) as usize & 63] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            DIGITS[word as usize & 63] as char
        } else {
            '='
        });
    }
    result
}

#[tokio::test]
async fn solana_rent_is_read_at_the_configured_round_and_when_settlement_changes() {
    for (rounds, failed_scan) in [(1, 1), (30, 1), (1, 2), (30, 2)] {
        let mut bytes = vec![0; 8 + Vault::INIT_SPACE];
        bytes[..8].copy_from_slice(Vault::DISCRIMINATOR);
        let vault = Vault::try_deserialize(&mut &bytes[..]).unwrap();
        let address = Pubkey::new_unique();
        let fixture = Arc::new(Mutex::new(Fixture {
            vault,
            scans: 0,
            fail_scan_at: None,
            address,
            payer: Pubkey::new_unique(),
            closable: false,
            send_attempts: 0,
        }));
        let app = Router::new().route("/", post(|State(state):State<Arc<Mutex<Fixture>>>,Json(body):Json<Value>| async move {
            let mut state = state.lock().unwrap();
            let result = match body["method"].as_str().unwrap() {
                "getGenesisHash" => json!("stand-in-genesis"),
                "getAccountInfo" => {
                    let mut data=Vec::new();state.vault.try_serialize(&mut data).unwrap();
                    json!({"context":{"slot":1},"value":{"lamports":1_000_000,"owner":chain::solana::programs::vault::ID.to_string(),"executable":false,"rentEpoch":0,"data":[base64(&data),"base64"]}})
                },
                "getLatestBlockhash" => {
                    state.send_attempts += 1;
                    return Json(json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32001,"message":"refuse before sending"}}));
                },
                "getProgramAccounts" => {
                    state.scans+=1;
                    if state.fail_scan_at == Some(state.scans) {
                        state.fail_scan_at = None;
                        return Json(json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32001,"message":"temporary scan failure"}}));
                    }
                    let mut values = Vec::new();
                    if state.closable {
                        let request_filter = body["params"][1]["filters"][0]["memcmp"]["bytes"] == bs58::encode(Request::DISCRIMINATOR).into_string();
                        let (record, data) = if request_filter {
                            let mut bytes = Request::DISCRIMINATOR.to_vec();
                            bytes.resize(8 + Request::INIT_SPACE, 0);
                            let mut request = Request::try_deserialize(&mut &bytes[..]).unwrap();
                            request.vault = state.address;
                            request.id = 1;
                            request.done = true;
                            request.payer = state.payer;
                            let mut data = Vec::new(); request.try_serialize(&mut data).unwrap();
                            (Pubkey::new_unique(), data)
                        } else {
                            let mut bytes = Claim::DISCRIMINATOR.to_vec();
                            bytes.resize(8 + Claim::INIT_SPACE, 0);
                            let mut claim = Claim::try_deserialize(&mut &bytes[..]).unwrap();
                            claim.id = 1;
                            claim.state = ClaimState::Paid;
                            claim.payer = state.payer;
                            let mut data = Vec::new(); claim.try_serialize(&mut data).unwrap();
                            (vp::state::claim_address(&state.address, 1).0, data)
                        };
                        values.push(json!({"pubkey":record.to_string(),"account":{"lamports":1_000_000,"owner":chain::solana::programs::vault::ID.to_string(),"executable":false,"rentEpoch":0,"data":[base64(&data),"base64"]}}));
                    }
                    if body["params"][1]["withContext"] == true {
                        json!({"context":{"slot":1},"value":values})
                    } else {
                        json!(values)
                    }
                },
                other => panic!("unexpected rent request: {other}"),
            };
            Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
        })).with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let keeper = Keypair::new();
        let dir = std::env::temp_dir().join(format!("keeper-rent-{}", keeper.pubkey()));
        std::fs::create_dir_all(&dir).unwrap();
        let defaults = include_str!("../../../config/chains.toml")
            .split_once("[rpc.defaults]")
            .unwrap()
            .1
            .split("\n[chains.")
            .next()
            .unwrap();
        let file = dir.join("chains.toml");
        std::fs::write(&file,format!("[rpc.defaults]{defaults}\n[chains.\"solana:localnet\"]\nlabel=\"rent fixture\"\nrpc=\"{url}\"\ndollar=\"USDC\"\n[networks.local]\nsolana=\"solana:localnet\"\n[rpc.chains.\"solana:localnet\"]\nsolana_rent_poll_rounds={rounds}\n")).unwrap();
        let args = ChainArgs {
            network: NetworkName::Local,
            chains: file,
            deployments_dir: dir.join("deployments"),
            vault_chain: Some(shared::Chain::Solana),
        };
        let target = args.resolve().unwrap();
        assert_eq!(
            target.config.rpc, url,
            "the fixture must stay on its own loopback endpoint"
        );
        let client = target.connect_solana().await.unwrap();
        assert_eq!(client.rpc_policy().solana_rent_poll_rounds, rounds);
        let platform = Platform {
            cluster_name: client.cluster.clone(),
            cluster: client.cluster_value(),
            mint: Pubkey::new_unique(),
            admin: Pubkey::new_unique(),
            platform: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            checker: None,
        };
        let keeping = Keeping {
            client,
            platform,
            keeper,
            executor: "http://127.0.0.1:1".into(),
        };
        let mut memory = Memory {
            rounds: 100,
            ..Default::default()
        };
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(fixture.lock().unwrap().scans, 2);
        memory.rounds = 100 + rounds - 1;
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().scans,
            2,
            "a scan before its configured round"
        );
        memory.rounds = 100 + rounds;
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().scans,
            4,
            "a scan missing at its configured round"
        );
        fixture.lock().unwrap().vault.request_cursor = 1;
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().scans,
            6,
            "settlement progress did not trigger an earlier scan"
        );
        let previously_read = memory.rent_read;
        memory.rounds += rounds;
        fixture.lock().unwrap().fail_scan_at = Some(6 + failed_scan);
        assert!(keeping.rent(&address, &mut memory).await.is_err());
        assert_eq!(
            memory.rent_read, previously_read,
            "failed scan suppressed retry"
        );
        assert_eq!(fixture.lock().unwrap().scans, 6 + failed_scan);
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().scans,
            8 + failed_scan,
            "failed scan was not retried immediately"
        );
        assert_eq!(memory.rent_read, Some((1, memory.rounds)));
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(fixture.lock().unwrap().scans, 8 + failed_scan);
        let last_success = memory.rent_read;
        memory.rounds += rounds;
        fixture.lock().unwrap().closable = true;
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().send_attempts,
            1,
            "no rent close was attempted"
        );
        assert_eq!(
            memory.rent_read, last_success,
            "refused close suppressed retry"
        );
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(
            fixture.lock().unwrap().send_attempts,
            2,
            "refused close was not retried immediately"
        );
        assert_eq!(memory.rent_read, last_success);
        fixture.lock().unwrap().closable = false;
        keeping.rent(&address, &mut memory).await.unwrap();
        assert_eq!(memory.rent_read, Some((1, memory.rounds)));
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
