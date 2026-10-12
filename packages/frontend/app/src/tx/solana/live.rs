//! The author's flows against a running local cluster, through the same
//! flow code the browser runs: the reader over the cluster's JSON-RPC and
//! the published development phrase's author key signing in place of a
//! wallet. Ignored by default; run with a local stack up:
//!
//! `SHONIN_TEST_SOLANA_RPC=http://127.0.0.1:<port> SHONIN_TEST_SOLANA_API=http://localhost:<port>/api
//! SHONIN_TEST_SOLANA_VAULT=<vault> cargo test -p shonin-app --lib live -- --ignored --nocapture`

use chain::solana_ix::Pubkey;
use chain::solana_ix::accounts::Vault;
use chain::solana_ix::transaction::Unsigned;
use futures::executor::block_on;
use serde_json::{Value, json};

use super::author::{AuthorAction, run, signed_usage};
use super::error::SolError;
use super::flow::{Cluster, Ctx, Outcome, Step};
use super::port::{Signed, SolReader, SolSigner};
use crate::tx::flow::{Cancel, Pace};
use crate::tx::port::Clock;
use crate::wallet::rpc::{RpcError, Transport};
use crate::wallet::solana::key::SolKey;
use crate::wallet::solana::rpc::SolRpc;

struct Http(String);

impl Transport for Http {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let reply: Value = reqwest::blocking::Client::new()
            .post(&self.0)
            .json(&body)
            .send()
            .and_then(|r| r.json())
            .map_err(|e| RpcError::unreachable(e.to_string()))?;
        if let Some(error) = reply.get("error") {
            return Err(RpcError::from_json(error));
        }
        Ok(reply["result"].clone())
    }
}

struct Key(SolKey);

impl SolSigner for Key {
    fn account(&self) -> Option<Pubkey> {
        Some(self.0.pubkey())
    }
    fn name(&self) -> String {
        "the author's key".to_string()
    }
    fn supports(&self, _chain: &str) -> bool {
        true
    }
    async fn sign(&self, unsigned: &Unsigned, _chain: &str) -> Result<Signed, SolError> {
        Ok(Signed::Wire(
            unsigned.signed(&[self.0.sign(unsigned.message())]).unwrap(),
        ))
    }
}

struct Sleep;

impl Clock for Sleep {
    async fn sleep(&self, millis: u32) {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(millis.max(200))));
    }
}

fn vault(reader: &SolRpc<Http>, address: &Pubkey) -> Vault {
    Vault::read(&block_on(reader.account(address)).unwrap().unwrap().data).unwrap()
}

#[test]
#[ignore = "needs a local Solana cluster"]
fn the_authors_flows_land_on_a_local_cluster() {
    let rpc = std::env::var("SHONIN_TEST_SOLANA_RPC").expect("SHONIN_TEST_SOLANA_RPC");
    let api = std::env::var("SHONIN_TEST_SOLANA_API").expect("SHONIN_TEST_SOLANA_API");
    let address: Pubkey = std::env::var("SHONIN_TEST_SOLANA_VAULT")
        .expect("SHONIN_TEST_SOLANA_VAULT")
        .parse()
        .unwrap();
    let reader = SolRpc {
        transport: Http(rpc),
    };
    // The published development phrase's author role (role 5, account 15).
    let key = SolKey::from_phrase(
        "test test test test test test test test test test test junk",
        15,
    )
    .unwrap();
    let before = vault(&reader, &address);
    assert_eq!(
        before.author,
        key.pubkey(),
        "the vault's author is the role key"
    );
    let cluster = Cluster {
        name: "localnet".to_string(),
        chain: "solana:localnet".to_string(),
        mint: before.mint,
        local_endpoint: true,
    };
    let signer = Key(key);
    let on_step = |step: Step| {
        if let Step::Failed(error) = &step {
            println!("  failed: {error:?}");
        }
    };
    let ctx = Ctx {
        reader: &reader,
        signer: &signer,
        clock: &Sleep,
        cluster: &cluster,
        vault: address,
        account: signer.0.pubkey(),
        cancel: Cancel::new(),
        pace: Pace::LIVE,
        on_step: &on_step,
    };
    let act = |name: &str, action: AuthorAction| {
        let outcome = block_on(run(&ctx, action));
        match &outcome {
            Ok(Outcome::Changed { signature }) => println!("{name}: {signature}"),
            other => println!("{name}: {other:?}"),
        }
        outcome
    };
    // Fund the tank with a dollar.
    act("fund_tank 1.000000", AuthorAction::FundTank(1_000_000)).unwrap();
    let funded = vault(&reader, &address);
    assert_eq!(funded.tank.balance, before.tank.balance + 1_000_000);
    println!("tank {} -> {}", before.tank.balance, funded.tank.balance);
    // The level auto-fill keeps it at, then off again.
    act("set_tank_keep 50.000000", AuthorAction::SetKeep(50_000_000)).unwrap();
    assert_eq!(vault(&reader, &address).tank.keep_at, 50_000_000);
    act("set_tank_keep 0", AuthorAction::SetKeep(0)).unwrap();
    assert_eq!(vault(&reader, &address).tank.keep_at, 0);
    // The allowances as they stand, set again; the idle floor likewise.
    let a = before.allowances;
    act(
        "set_allowances",
        AuthorAction::SetAllowances(chain::solana_ix::vault::Allowances {
            fuel_per_sec: a.fuel_per_sec,
            memory_mib: a.memory_mib,
            requests_per_min: a.requests_per_min,
            bytes_per_day: a.bytes_per_day,
            streams: a.streams,
            inference_ms_per_min: a.inference_ms_per_min,
            gas_per_day: a.gas_per_day,
        }),
    )
    .unwrap();
    act(
        "set_idle_floor",
        AuthorAction::SetIdleFloor(before.floor_bps),
    )
    .unwrap();
    // The executor's signed usage total, as the app reads it from the API,
    // and a dollar back out of the tank with it.
    let usage: Value = reqwest::blocking::get(format!("{api}/solana/usage/{address}"))
        .and_then(|r| r.json())
        .unwrap();
    let signed = signed_usage(&usage, &address).expect("a signed total");
    let tank = vault(&reader, &address).tank.balance;
    act(
        "withdraw_tank 1.000000 (signed)",
        AuthorAction::WithdrawTank {
            assets: 1_000_000,
            signed: Some(signed),
        },
    )
    .unwrap();
    let after = vault(&reader, &address);
    println!(
        "tank {tank} -> {} (usage settled {} -> {})",
        after.tank.balance, before.tank.usage_settled, after.tank.usage_settled
    );
    assert!(after.tank.balance <= tank - 1_000_000);
    // The unsigned escape: asked for, then refused until its delay passes.
    act("request_tank_withdrawal", AuthorAction::RequestEscape).unwrap();
    assert_ne!(vault(&reader, &address).tank.escape_requested_at, 0);
    let early = act(
        "withdraw_tank_unsigned (too early)",
        AuthorAction::WithdrawTank {
            assets: 1,
            signed: None,
        },
    );
    assert!(
        matches!(early, Err(SolError::WouldFail { .. })),
        "{early:?}"
    );
    // Top up from the author's fee shares, when it holds any.
    let held = block_on(super::flow::holder(&reader, &address, &signer.0.pubkey()))
        .unwrap()
        .map_or(0, |h| h.shares);
    if held > 1_000 {
        act(
            "top_up_from_shares",
            AuthorAction::TopUpFromShares(held / 1_000),
        )
        .unwrap();
    } else {
        println!("top_up_from_shares: the author holds no fee shares");
    }
}
