//! JSON-RPC to the Solana cluster through its public endpoint: reading
//! accounts, simulating, sending, and following a sent transaction. The
//! parsing is plain functions over JSON, tested natively; the transport is
//! the EVM side's ([`crate::wallet::rpc::Transport`]), the browser's `fetch`.

use base64::Engine;
use chain::solana_ix::{Hash, Pubkey};
use serde_json::{Value, json};

use crate::tx::solana::error::SolError;
use crate::tx::solana::port::{
    AccountData, Blockhash, Failure, Record, Sig, Simulated, SolReader, Status,
};
use crate::wallet::rpc::{RpcError, Transport};

/// The clock sysvar: the cluster's own time.
const CLOCK: Pubkey =
    chain::solana_ix::Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn shape(what: &str) -> SolError {
    SolError::Network(format!("the cluster's {what} came in an unexpected shape"))
}

fn rpc_error(error: RpcError) -> SolError {
    SolError::Network(if error.code == 0 {
        error.message
    } else {
        format!("the cluster answered: {}", error.message)
    })
}

/// An account from a node's `base64` answer; `None` for `null`.
pub fn parse_account(value: &Value) -> Result<Option<AccountData>, SolError> {
    if value.is_null() {
        return Ok(None);
    }
    let data = value["data"][0]
        .as_str()
        .and_then(base64_decode)
        .ok_or_else(|| shape("account"))?;
    let owner = value["owner"]
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| shape("account"))?;
    let lamports = value["lamports"].as_u64().ok_or_else(|| shape("account"))?;
    Ok(Some(AccountData {
        owner,
        lamports,
        data,
    }))
}

/// A simulation's answer.
pub fn parse_simulation(value: &Value) -> Result<Simulated, SolError> {
    let value = &value["value"];
    if value.is_null() {
        return Err(shape("simulation"));
    }
    let logs = logs_of(&value["logs"]);
    if !value["err"].is_null() {
        return Ok(Simulated::Failed(Failure {
            err: value["err"].to_string(),
            logs,
        }));
    }
    Ok(Simulated::Ok {
        units: value["unitsConsumed"].as_u64().unwrap_or(0),
    })
}

fn logs_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| line.as_str().map(str::to_string))
        .collect()
}

/// A status from `getSignatureStatuses`; `None` while the node hasn't seen it.
pub fn parse_status(value: &Value) -> Result<Option<Status>, SolError> {
    let entry = &value["value"][0];
    if entry.is_null() {
        return Ok(None);
    }
    let confirmed = matches!(
        entry["confirmationStatus"].as_str(),
        Some("confirmed" | "finalized")
    );
    Ok(Some(Status {
        slot: entry["slot"].as_u64().ok_or_else(|| shape("status"))?,
        err: (!entry["err"].is_null()).then(|| entry["err"].to_string()),
        confirmed,
    }))
}

/// A landed transaction from `getTransaction` (`json` encoding): its
/// logs, and its inner calls with their programs resolved.
pub fn parse_record(value: &Value) -> Result<Option<Record>, SolError> {
    if value.is_null() {
        return Ok(None);
    }
    let meta = &value["meta"];
    let mut keys: Vec<Pubkey> = value["transaction"]["message"]["accountKeys"]
        .as_array()
        .ok_or_else(|| shape("transaction"))?
        .iter()
        .map(|key| key.as_str().and_then(|text| text.parse().ok()))
        .collect::<Option<_>>()
        .ok_or_else(|| shape("transaction"))?;
    for kind in ["writable", "readonly"] {
        for key in meta["loadedAddresses"][kind]
            .as_array()
            .into_iter()
            .flatten()
        {
            keys.push(
                key.as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(|| shape("transaction"))?,
            );
        }
    }
    let mut calls = Vec::new();
    for group in meta["innerInstructions"].as_array().into_iter().flatten() {
        for call in group["instructions"].as_array().into_iter().flatten() {
            let program = call["programIdIndex"]
                .as_u64()
                .and_then(|index| keys.get(index as usize))
                .copied()
                .ok_or_else(|| shape("transaction"))?;
            let data = bs58::decode(call["data"].as_str().unwrap_or_default())
                .into_vec()
                .map_err(|_| shape("transaction"))?;
            calls.push((program, data));
        }
    }
    Ok(Some(Record {
        slot: value["slot"].as_u64().unwrap_or(0),
        err: (!meta["err"].is_null()).then(|| meta["err"].to_string()),
        logs: logs_of(&meta["logMessages"]),
        calls,
    }))
}

/// Reading the cluster through a transport.
pub struct SolRpc<T> {
    pub transport: T,
}

impl<T: Transport> SolRpc<T> {
    async fn ask(&self, method: &str, params: Value) -> Result<Value, SolError> {
        self.transport
            .request(method, params)
            .await
            .map_err(rpc_error)
    }
}

const CONFIRMED: &str = "confirmed";

impl<T: Transport> SolReader for SolRpc<T> {
    async fn genesis(&self) -> Result<String, SolError> {
        self.ask("getGenesisHash", json!([]))
            .await?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| shape("genesis hash"))
    }

    async fn account(&self, key: &Pubkey) -> Result<Option<AccountData>, SolError> {
        let answer = self
            .ask(
                "getAccountInfo",
                json!([key.to_string(), {"encoding": "base64", "commitment": CONFIRMED}]),
            )
            .await?;
        parse_account(&answer["value"])
    }

    async fn accounts(&self, keys: &[Pubkey]) -> Result<Vec<Option<AccountData>>, SolError> {
        let mut out = Vec::with_capacity(keys.len());
        // The node takes at most 100 at once.
        for chunk in keys.chunks(100) {
            let names: Vec<String> = chunk.iter().map(Pubkey::to_string).collect();
            let answer = self
                .ask(
                    "getMultipleAccounts",
                    json!([names, {"encoding": "base64", "commitment": CONFIRMED}]),
                )
                .await?;
            let values = answer["value"]
                .as_array()
                .ok_or_else(|| shape("accounts"))?;
            if values.len() != chunk.len() {
                return Err(shape("accounts"));
            }
            for value in values {
                out.push(parse_account(value)?);
            }
        }
        Ok(out)
    }

    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[(usize, Vec<u8>)],
    ) -> Result<Vec<(Pubkey, AccountData)>, SolError> {
        let filters: Vec<Value> = filters
            .iter()
            .map(|(offset, bytes)| {
                json!({"memcmp": {"offset": offset, "bytes": bs58::encode(bytes).into_string(), "encoding": "base58"}})
            })
            .collect();
        let answer = self
            .ask(
                "getProgramAccounts",
                json!([program.to_string(), {"encoding": "base64", "commitment": CONFIRMED, "filters": filters}]),
            )
            .await?;
        let mut out = Vec::new();
        for entry in answer.as_array().ok_or_else(|| shape("program accounts"))? {
            let key = entry["pubkey"]
                .as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| shape("program accounts"))?;
            if let Some(account) = parse_account(&entry["account"])? {
                out.push((key, account));
            }
        }
        Ok(out)
    }

    async fn blockhash(&self) -> Result<Blockhash, SolError> {
        let answer = self
            .ask("getLatestBlockhash", json!([{"commitment": CONFIRMED}]))
            .await?;
        let value = &answer["value"];
        let hash: Hash = value["blockhash"]
            .as_str()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| shape("blockhash"))?;
        Ok(Blockhash {
            hash,
            last_valid: value["lastValidBlockHeight"]
                .as_u64()
                .ok_or_else(|| shape("blockhash"))?,
        })
    }

    async fn block_height(&self) -> Result<u64, SolError> {
        self.ask("getBlockHeight", json!([{"commitment": CONFIRMED}]))
            .await?
            .as_u64()
            .ok_or_else(|| shape("block height"))
    }

    async fn chain_time(&self) -> Result<u64, SolError> {
        let clock = self.account(&CLOCK).await?.ok_or_else(|| shape("clock"))?;
        let time = clock
            .data
            .get(32..40)
            .and_then(|bytes| bytes.try_into().ok())
            .map(i64::from_le_bytes)
            .ok_or_else(|| shape("clock"))?;
        u64::try_from(time).map_err(|_| shape("clock"))
    }

    async fn simulate(&self, wire: &[u8]) -> Result<Simulated, SolError> {
        let answer = self
            .ask(
                "simulateTransaction",
                json!([base64_encode(wire), {
                    "encoding": "base64",
                    "sigVerify": false,
                    "replaceRecentBlockhash": false,
                    "commitment": CONFIRMED,
                }]),
            )
            .await?;
        parse_simulation(&answer)
    }

    async fn send(&self, wire: &[u8]) -> Result<Sig, SolError> {
        let answer = self
            .transport
            .request(
                "sendTransaction",
                json!([base64_encode(wire), {
                    "encoding": "base64",
                    "skipPreflight": false,
                    "preflightCommitment": CONFIRMED,
                }]),
            )
            .await;
        match answer {
            Ok(value) => value
                .as_str()
                .and_then(Sig::parse)
                .ok_or_else(|| shape("signature")),
            // The node ran it first and refused it: nothing was sent.
            Err(error) if error.code == -32002 => Err(SolError::WouldFail {
                reason: crate::tx::solana::error::Reason::from_failure(&Failure {
                    err: error.message.clone(),
                    logs: Vec::new(),
                }),
            }),
            Err(error) => Err(rpc_error(error)),
        }
    }

    async fn status(&self, signature: &Sig, history: bool) -> Result<Option<Status>, SolError> {
        let answer = self
            .ask(
                "getSignatureStatuses",
                json!([[signature.to_string()], {"searchTransactionHistory": history}]),
            )
            .await?;
        parse_status(&answer)
    }

    async fn signatures_for(&self, key: &Pubkey, limit: usize) -> Result<Vec<Sig>, SolError> {
        let answer = self
            .ask(
                "getSignaturesForAddress",
                json!([key.to_string(), {"limit": limit, "commitment": CONFIRMED}]),
            )
            .await?;
        Ok(answer
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["signature"].as_str().and_then(Sig::parse))
            .collect())
    }

    async fn transaction(&self, signature: &Sig) -> Result<Option<Record>, SolError> {
        let answer = self
            .ask(
                "getTransaction",
                json!([signature.to_string(), {
                    "encoding": "json",
                    "commitment": CONFIRMED,
                    "maxSupportedTransactionVersion": 0,
                }]),
            )
            .await?;
        parse_record(&answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_is_read_from_base64_with_its_owner_and_lamports() {
        let owner = Pubkey::new_from_array([3; 32]);
        let value = json!({"data": [base64_encode(&[1, 2, 3]), "base64"], "owner": owner.to_string(), "lamports": 42, "executable": false});
        let account = parse_account(&value).unwrap().unwrap();
        assert_eq!(account.data, vec![1, 2, 3]);
        assert_eq!(account.owner, owner);
        assert_eq!(account.lamports, 42);
        assert_eq!(parse_account(&Value::Null).unwrap(), None);
        assert!(parse_account(&json!({"data": ["%%", "base64"]})).is_err());
    }

    #[test]
    fn a_failed_simulation_keeps_its_error_and_logs() {
        let answer = json!({"value": {"err": {"InstructionError": [2, {"Custom": 6000}]}, "logs": ["a", "b"], "unitsConsumed": 5}});
        match parse_simulation(&answer).unwrap() {
            Simulated::Failed(failure) => {
                assert!(failure.err.contains("6000"));
                assert_eq!(failure.logs, vec!["a", "b"]);
            }
            other => panic!("{other:?}"),
        }
        let ok = json!({"value": {"err": null, "logs": [], "unitsConsumed": 1234}});
        assert_eq!(
            parse_simulation(&ok).unwrap(),
            Simulated::Ok { units: 1234 }
        );
    }

    #[test]
    fn a_status_is_confirmed_only_once_a_supermajority_voted() {
        let processed =
            json!({"value": [{"slot": 7, "err": null, "confirmationStatus": "processed"}]});
        assert!(!parse_status(&processed).unwrap().unwrap().confirmed);
        let confirmed =
            json!({"value": [{"slot": 7, "err": null, "confirmationStatus": "confirmed"}]});
        assert!(parse_status(&confirmed).unwrap().unwrap().confirmed);
        let failed = json!({"value": [{"slot": 7, "err": {"InstructionError": [0, "Custom"]}, "confirmationStatus": "finalized"}]});
        assert!(parse_status(&failed).unwrap().unwrap().err.is_some());
        assert_eq!(parse_status(&json!({"value": [null]})).unwrap(), None);
    }

    #[test]
    fn a_records_inner_calls_name_their_programs_through_its_account_list() {
        let a = Pubkey::new_from_array([1; 32]);
        let b = Pubkey::new_from_array([2; 32]);
        let loaded = Pubkey::new_from_array([3; 32]);
        let value = json!({
            "slot": 9,
            "transaction": {"message": {"accountKeys": [a.to_string(), b.to_string()]}},
            "meta": {
                "err": null,
                "logMessages": ["Program log: hi"],
                "loadedAddresses": {"writable": [loaded.to_string()], "readonly": []},
                "innerInstructions": [{"index": 0, "instructions": [
                    {"programIdIndex": 1, "accounts": [0], "data": bs58::encode([7, 8]).into_string()},
                    {"programIdIndex": 2, "accounts": [], "data": ""},
                ]}],
            },
        });
        let record = parse_record(&value).unwrap().unwrap();
        assert_eq!(record.slot, 9);
        assert_eq!(record.logs, vec!["Program log: hi"]);
        assert_eq!(record.calls, vec![(b, vec![7, 8]), (loaded, vec![])]);
        assert_eq!(parse_record(&Value::Null).unwrap(), None);
    }
}
