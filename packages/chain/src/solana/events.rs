//! A landed transaction as the node keeps it, and the events in it.
//!
//! The vault emits the figures a record depends on (each valuation, each
//! settlement) as calls of its own program carrying the event as instruction
//! data, which a node keeps whole however long the logs run; everything else
//! is in the logs as `Program data:` lines. Both kinds are read here, from
//! the same record, so an indexer and a sender read events the same way.

use anchor_lang::{AnchorDeserialize, Discriminator};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde_json::Value;

use super::Pubkey;
use super::rpc::parse_pubkey;

/// One instruction of a transaction, its accounts resolved to addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Called {
    pub program: Pubkey,
    pub accounts: Vec<Pubkey>,
    pub data: Vec<u8>,
}

/// A transaction the node holds, read from `getTransaction`.
#[derive(Clone, Debug)]
pub struct TransactionRecord {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    /// The error it failed with, if it did.
    pub err: Option<Value>,
    /// What it paid in fees, in lamports.
    pub fee: u64,
    /// Every account it named, static then loaded from tables, as the
    /// balances below are ordered.
    pub account_keys: Vec<Pubkey>,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    /// Its own instructions, in order.
    pub instructions: Vec<Called>,
    /// The calls each of its own instructions made, by that instruction's
    /// index, in the order they were made.
    pub inner: Vec<(usize, Vec<Called>)>,
    pub logs: Vec<String>,
    pub units: Option<u64>,
}

impl TransactionRecord {
    /// Read the node's `json`-encoded answer for `signature`.
    pub fn from_json(signature: &str, value: &Value) -> Result<Self> {
        let meta = &value["meta"];
        let message = &value["transaction"]["message"];
        let mut account_keys: Vec<Pubkey> = message["accountKeys"]
            .as_array()
            .ok_or_else(|| anyhow!("a transaction with no accounts"))?
            .iter()
            .map(|key| parse_pubkey(key.as_str().unwrap_or_default()))
            .collect::<Result<_>>()?;
        for kind in ["writable", "readonly"] {
            for key in meta["loadedAddresses"][kind]
                .as_array()
                .into_iter()
                .flatten()
            {
                account_keys.push(parse_pubkey(key.as_str().unwrap_or_default())?);
            }
        }
        let called = |instruction: &Value| -> Result<Called> {
            let program = instruction["programIdIndex"]
                .as_u64()
                .and_then(|index| account_keys.get(index as usize))
                .copied()
                .ok_or_else(|| anyhow!("an instruction of an unnamed program"))?;
            let accounts = instruction["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|index| {
                    index
                        .as_u64()
                        .and_then(|index| account_keys.get(index as usize))
                        .copied()
                        .ok_or_else(|| anyhow!("an instruction naming an unnamed account"))
                })
                .collect::<Result<_>>()?;
            let data = bs58::decode(instruction["data"].as_str().unwrap_or_default())
                .into_vec()
                .context("an instruction's data is not base58")?;
            Ok(Called {
                program,
                accounts,
                data,
            })
        };
        let instructions = message["instructions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(called)
            .collect::<Result<_>>()?;
        let inner = meta["innerInstructions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|group| {
                let index = group["index"].as_u64().unwrap_or(0) as usize;
                let calls = group["instructions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(called)
                    .collect::<Result<Vec<_>>>()?;
                Ok((index, calls))
            })
            .collect::<Result<_>>()?;
        let numbers = |field: &str| -> Vec<u64> {
            meta[field]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect()
        };
        Ok(Self {
            signature: signature.to_string(),
            slot: value["slot"].as_u64().unwrap_or(0),
            block_time: value["blockTime"].as_i64(),
            err: meta.get("err").cloned().filter(|err| !err.is_null()),
            fee: meta["fee"].as_u64().unwrap_or(0),
            pre_balances: numbers("preBalances"),
            post_balances: numbers("postBalances"),
            account_keys,
            instructions,
            inner,
            logs: meta["logMessages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|line| line.as_str().map(str::to_string))
                .collect(),
            units: meta["computeUnitsConsumed"].as_u64(),
        })
    }

    /// How far `account`'s lamports fell in this transaction (negative: how
    /// far they rose). For the fee payer it is the fee plus the rent of every
    /// account it paid for, less any rent refunded to it.
    pub fn spent_by(&self, account: &Pubkey) -> i128 {
        self.account_keys
            .iter()
            .position(|key| key == account)
            .map_or(0, |at| {
                let before = self.pre_balances.get(at).copied().unwrap_or(0) as i128;
                let after = self.post_balances.get(at).copied().unwrap_or(0) as i128;
                before - after
            })
    }

    /// The events of kind `E` that `program` emitted through its own
    /// instruction data, with the index of the transaction's instruction
    /// they were made under, in order.
    pub fn cpi_events<E: Discriminator + AnchorDeserialize>(
        &self,
        program: &Pubkey,
    ) -> Vec<(usize, E)> {
        let mut out = Vec::new();
        for (index, calls) in &self.inner {
            for call in calls {
                if call.program != *program {
                    continue;
                }
                if let Some(event) = decode_cpi_event::<E>(&call.data) {
                    out.push((*index, event));
                }
            }
        }
        out
    }

    /// The events of kind `E` written to the logs (`Program data:`), in
    /// order. Logs are cut at the node's limit, which is why the figures a
    /// record depends on are never read from here.
    pub fn log_events<E: Discriminator + AnchorDeserialize>(&self) -> Vec<E> {
        log_events(&self.logs)
    }
}

/// An event carried as a self-call's instruction data: Anchor's event tag,
/// the event's discriminator, then the event.
pub fn decode_cpi_event<E: Discriminator + AnchorDeserialize>(data: &[u8]) -> Option<E> {
    let data = data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE)?;
    let mut data = data.strip_prefix(E::DISCRIMINATOR)?;
    E::deserialize(&mut data).ok()
}

/// The events of kind `E` in a transaction's logs, in order.
pub fn log_events<E: Discriminator + AnchorDeserialize>(logs: &[String]) -> Vec<E> {
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
        .filter_map(|bytes| {
            let mut rest = bytes.strip_prefix(E::DISCRIMINATOR)?;
            E::deserialize(&mut rest).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::AnchorSerialize;
    use shonin_vault::events::Valued;

    fn valued() -> Valued {
        Valued {
            index: 1,
            time: 1_000,
            gross: 2,
            gross_exit: 3,
            supply: 5,
            fee: 4,
        }
    }

    #[test]
    fn a_self_call_event_decodes_and_another_kind_does_not() {
        let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
        data.extend_from_slice(Valued::DISCRIMINATOR);
        data.extend_from_slice(&valued().try_to_vec().unwrap());
        let decoded: Valued = decode_cpi_event(&data).expect("a Valued event");
        assert_eq!(decoded.time, 1_000);
        assert_eq!(decoded.supply, 5);
        assert!(decode_cpi_event::<shonin_vault::events::Halted>(&data).is_none());
        // Without the event tag it is an ordinary instruction, not an event.
        assert!(decode_cpi_event::<Valued>(&data[8..]).is_none());
    }

    #[test]
    fn a_log_event_decodes_from_its_program_data_line() {
        let mut bytes = Valued::DISCRIMINATOR.to_vec();
        bytes.extend_from_slice(&valued().try_to_vec().unwrap());
        let logs = vec![
            "Program log: something".to_string(),
            format!(
                "Program data: {}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
        ];
        let events: Vec<Valued> = log_events(&logs);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].fee, 4);
    }
}
