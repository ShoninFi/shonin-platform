//! Withdrawals on their way: dollars the exchange has taken out of a
//! station's accounts that are not yet in its pending balance on-chain.
//!
//! The exchange carries a withdrawal out in one of its own (L2) blocks,
//! taking the dollars out of the account there. The exchange contract
//! credits them to the owner's pending balance only when the batch holding
//! that block is executed on-chain, minutes later. In between the dollars are
//! in neither the account's worth nor the pending balance, so the attested
//! read names them.
//!
//! Which withdrawals have been credited by a given block of the chain is read
//! from the chain itself: the exchange contract's count of executed batches
//! at that block, and the last L2 block of that batch. A withdrawal carried
//! out in an L2 block up to that one is in the pending balance (or already
//! paid out of it); a later one is not.
//!
//! Every figure here is read exactly; anything missing, of another type or
//! out of order stops the read, so nothing is ever counted as zero for want
//! of a figure.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

/// The exchange's asset index for its dollar on Robinhood Chain (USDG, six
/// decimals): the asset whose pending balance the post reads, and the only
/// one the station withdraws.
pub const DOLLAR_ASSET: u16 = 3;

/// The exchange's transaction types for a withdrawal: one requested on-chain
/// (a priority request, as a station's own `withdraw` is) and one signed with
/// an account's API key.
pub const WITHDRAW_FROM_CHAIN: u64 = 6;
pub const WITHDRAW_SIGNED: u64 = 13;

/// The explorer's names for a withdrawal's log.
pub const WITHDRAW_LOGS: [&str; 2] = ["Withdraw", "WithdrawV2"];

/// One withdrawal as the exchange carried it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Withdrawal {
    pub hash: String,
    /// The exchange's (L2) block it was carried out in.
    pub height: u64,
    /// When the exchange applied it to the account, in microseconds, on the
    /// same clock as an account's `transaction_time`.
    pub applied_us: u64,
    pub asset: u16,
    /// In the asset's own units (the dollar's: millionths).
    pub amount: u128,
    /// The exchange refused it when it came to carry it out: nothing left
    /// the account.
    pub failed: bool,
}

/// The hash of one entry of the explorer's list of an account's withdrawal
/// logs, which must be a withdrawal's.
pub fn listed_hash(log: &Value) -> Result<String> {
    let kind = log["pubdata_type"].as_str().unwrap_or_default();
    if !WITHDRAW_LOGS.contains(&kind) {
        bail!("the explorer listed a {kind:?} log among withdrawals: {log}");
    }
    log["hash"]
        .as_str()
        .filter(|hash| !hash.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("a withdrawal the explorer listed has no hash: {log}"))
}

/// A withdrawal of `account` from the exchange API's answer for one
/// transaction (`/api/v1/tx`): its block, when it was applied, and what it
/// took out, from the event the exchange recorded for it.
pub fn withdrawal(tx: &Value, account: u64) -> Result<Withdrawal> {
    let hash = tx["hash"]
        .as_str()
        .ok_or_else(|| anyhow!("a transaction without a hash: {tx}"))?
        .to_string();
    let kind = tx["type"].as_u64();
    if kind != Some(WITHDRAW_FROM_CHAIN) && kind != Some(WITHDRAW_SIGNED) {
        bail!(
            "transaction {hash} is not a withdrawal: type {:?}",
            tx["type"]
        );
    }
    if tx["account_index"].as_u64() != Some(account) {
        bail!(
            "withdrawal {hash} is of account {}, not of account {account}",
            tx["account_index"]
        );
    }
    let height = tx["block_height"]
        .as_u64()
        .filter(|height| *height > 0)
        .ok_or_else(|| anyhow!("withdrawal {hash} has no block: {}", tx["block_height"]))?;
    let applied_us = tx["transaction_time"]
        .as_u64()
        .filter(|at| *at > 0)
        .ok_or_else(|| anyhow!("withdrawal {hash} has no time: {}", tx["transaction_time"]))?;
    let event: Value = tx["event_info"]
        .as_str()
        .ok_or_else(|| anyhow!("withdrawal {hash} has no event"))
        .and_then(|text| {
            serde_json::from_str(text)
                .map_err(|_| anyhow!("withdrawal {hash}'s event can't be read: {text}"))
        })?;
    if event["a"].as_u64() != Some(account) {
        bail!(
            "withdrawal {hash}'s event is of account {}, not {account}",
            event["a"]
        );
    }
    let asset = event["as"]
        .as_u64()
        .and_then(|asset| u16::try_from(asset).ok())
        .ok_or_else(|| anyhow!("withdrawal {hash}'s asset can't be read: {}", event["as"]))?;
    let amount = event["c"]
        .as_u64()
        .map(u128::from)
        .ok_or_else(|| anyhow!("withdrawal {hash}'s amount can't be read: {}", event["c"]))?;
    let failed = match &event["ae"] {
        Value::String(error) => !error.is_empty(),
        Value::Null => false,
        other => bail!("withdrawal {hash}'s outcome can't be read: {other}"),
    };
    Ok(Withdrawal {
        hash,
        height,
        applied_us,
        asset,
        amount,
        failed,
    })
}

/// The last L2 block of batch `number`, from the explorer's answer for it
/// (`/api/batches/{number}`), which lists every block in it.
pub fn batch_end(batch: &Value, number: u64) -> Result<u64> {
    if batch["batch_number"].as_u64() != Some(number) {
        bail!(
            "asked for batch {number}, the explorer answered with {}",
            batch["batch_number"]
        );
    }
    let blocks = batch["blocks"]
        .as_array()
        .filter(|blocks| !blocks.is_empty())
        .ok_or_else(|| anyhow!("batch {number}'s blocks can't be read"))?;
    let mut end = 0;
    for block in blocks {
        let height = block["block_number"]
            .as_u64()
            .ok_or_else(|| anyhow!("one of batch {number}'s blocks has no number: {block}"))?;
        end = end.max(height);
    }
    Ok(end)
}

/// When the explorer's newest block was made, in microseconds, from its
/// list of recent blocks (`/api/blocks`): how far it has caught up.
pub fn newest_block_us(blocks: &Value) -> Result<u64> {
    let newest = blocks
        .as_array()
        .and_then(|blocks| blocks.first())
        .ok_or_else(|| anyhow!("the explorer lists no block: {blocks}"))?;
    let at = newest["updated_at"]
        .as_str()
        .ok_or_else(|| anyhow!("the explorer's newest block has no time: {newest}"))?;
    utc_micros(at).ok_or_else(|| anyhow!("the explorer's newest block's time can't be read: {at}"))
}

/// Microseconds since 1970 of a UTC time written `YYYY-MM-DDTHH:MM:SS[.f]Z`.
pub fn utc_micros(text: &str) -> Option<u64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (whole, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut whole = whole.split(':');
    let hour: i64 = whole.next()?.parse().ok()?;
    let minute: i64 = whole.next()?.parse().ok()?;
    let second: i64 = whole.next()?.parse().ok()?;
    if whole.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    if !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Microseconds, the fraction cut to six digits.
    let micros: i64 = format!("{:0<6}", &fraction[..fraction.len().min(6)])
        .parse()
        .ok()?;
    // Days from 1970-01-01 to the date (the proleptic Gregorian calendar).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let of_era = y - era * 400;
    let of_year = (153 * m + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds * 1_000_000 + micros).ok()
}

/// The dollars on their way out of one account, tallied from its
/// withdrawals newest first: those carried out after the last L2 block the
/// chain had credited at the post's block (`credited_through`) and applied
/// to the account at or before its read (`read_at_us`, the account's own
/// `transaction_time` as read). The first withdrawal at or before
/// `credited_through` ends the tally: every older one was credited before it.
#[derive(Clone, Debug)]
pub struct OnTheirWay {
    read_at_us: u64,
    credited_through: u64,
    last_height: Option<u64>,
    total: u128,
    done: bool,
}

impl OnTheirWay {
    pub fn new(read_at_us: u64, credited_through: u64) -> Self {
        Self {
            read_at_us,
            credited_through,
            last_height: None,
            total: 0,
            done: false,
        }
    }

    /// Take the next withdrawal, newest first.
    pub fn take(&mut self, withdrawal: &Withdrawal) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if self
            .last_height
            .is_some_and(|last| withdrawal.height > last)
        {
            bail!(
                "the account's withdrawals are not listed newest first: {} (block {}) after block {}",
                withdrawal.hash,
                withdrawal.height,
                self.last_height.unwrap_or_default()
            );
        }
        self.last_height = Some(withdrawal.height);
        if withdrawal.height <= self.credited_through {
            if !withdrawal.failed && withdrawal.applied_us > self.read_at_us {
                bail!(
                    "withdrawal {} is credited on-chain yet came after the account was read",
                    withdrawal.hash
                );
            }
            self.done = true;
            return Ok(());
        }
        // Refused when carried out: nothing left the account. Applied after
        // the account was read: still in the worth read.
        if withdrawal.failed || withdrawal.applied_us > self.read_at_us {
            return Ok(());
        }
        if withdrawal.asset != DOLLAR_ASSET {
            bail!(
                "withdrawal {} of asset {} is on its way, and only dollars are counted",
                withdrawal.hash,
                withdrawal.asset
            );
        }
        self.total = self
            .total
            .checked_add(withdrawal.amount)
            .ok_or_else(|| anyhow!("the withdrawals on their way overflow"))?;
        Ok(())
    }

    /// Whether a withdrawal already credited has been met: nothing older
    /// needs to be read.
    pub fn done(&self) -> bool {
        self.done
    }

    /// The dollars on their way, in millionths.
    pub fn total(&self) -> u128 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn w(height: u64, applied_us: u64, amount: u128) -> Withdrawal {
        Withdrawal {
            hash: format!("h{height}"),
            height,
            applied_us,
            asset: DOLLAR_ASSET,
            amount,
            failed: false,
        }
    }

    #[test]
    fn a_withdrawal_is_on_its_way_from_the_account_s_read_until_its_batch_is_executed() {
        // Credited through L2 block 100; the account read at time 1_000.
        let mut tally = OnTheirWay::new(1_000, 100);
        // Carried out after the read: still in the account's worth as read.
        tally.take(&w(130, 1_001, 7)).unwrap();
        // Carried out before the read, batch not executed: on its way.
        tally.take(&w(120, 900, 5_000_000)).unwrap();
        tally.take(&w(101, 1_000, 2)).unwrap();
        assert!(!tally.done());
        // In a batch executed by the post's block: in the pending balance,
        // and so is everything older.
        tally.take(&w(100, 800, 9)).unwrap();
        assert!(tally.done());
        tally.take(&w(50, 1, 11)).unwrap();
        assert_eq!(tally.total(), 5_000_002);
    }

    #[test]
    fn a_withdrawal_the_exchange_refused_is_not_on_its_way() {
        let mut tally = OnTheirWay::new(1_000, 0);
        tally
            .take(&Withdrawal {
                failed: true,
                ..w(5, 10, 100)
            })
            .unwrap();
        assert_eq!(tally.total(), 0);
    }

    #[test]
    fn what_can_t_be_told_stops_the_tally() {
        // Listed out of order.
        let mut tally = OnTheirWay::new(1_000, 0);
        tally.take(&w(5, 10, 1)).unwrap();
        assert!(tally.take(&w(6, 10, 1)).is_err());
        // Another asset on its way.
        let mut tally = OnTheirWay::new(1_000, 0);
        assert!(
            tally
                .take(&Withdrawal {
                    asset: 20,
                    ..w(5, 10, 1)
                })
                .is_err()
        );
        // Credited on-chain, yet applied after the account was read: the
        // readings don't fit together.
        let mut tally = OnTheirWay::new(1_000, 10);
        assert!(tally.take(&w(5, 2_000, 1)).is_err());
    }

    #[test]
    fn a_withdrawal_is_read_from_the_exchange_s_record_of_it() {
        let tx = json!({"code": 200, "hash": "abc", "type": 13, "account_index": 5,
            "block_height": 33607223, "transaction_time": 1791281646042647u64,
            "event_info": "{\"a\":5,\"l\":\"0xe3\",\"as\":3,\"rt\":0,\"c\":1000000000,\"ae\":\"\"}"});
        assert_eq!(
            withdrawal(&tx, 5).unwrap(),
            Withdrawal {
                hash: "abc".into(),
                height: 33607223,
                applied_us: 1791281646042647,
                asset: 3,
                amount: 1_000_000_000,
                failed: false,
            }
        );
        let refused = json!({"hash": "abc", "type": 6, "account_index": 5, "block_height": 1,
            "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1,\"ae\":\"not enough\"}"});
        assert!(withdrawal(&refused, 5).unwrap().failed);
        for broken in [
            json!({"hash": "abc", "type": 14, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1}"}),
            json!({"hash": "abc", "type": 13, "account_index": 6, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 0, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":\"1\"}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":4,\"as\":3,\"c\":1}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "{\"a\":5,\"as\":3,\"c\":1,\"ae\":7}"}),
            json!({"hash": "abc", "type": 13, "account_index": 5, "block_height": 1, "transaction_time": 1, "event_info": "nonsense"}),
        ] {
            assert!(withdrawal(&broken, 5).is_err(), "{broken}");
        }
    }

    #[test]
    fn a_batch_ends_at_its_last_block() {
        let batch = json!({"batch_number": 7, "blocks": [
            {"block_number": 30685596}, {"block_number": 30686395}, {"block_number": 30686000}]});
        assert_eq!(batch_end(&batch, 7).unwrap(), 30686395);
        assert!(batch_end(&batch, 8).is_err());
        assert!(batch_end(&json!({"batch_number": 7, "blocks": []}), 7).is_err());
        assert!(batch_end(&json!({"batch_number": 7, "blocks": [{}]}), 7).is_err());
    }

    #[test]
    fn only_a_withdrawal_s_log_is_taken_from_the_list() {
        assert_eq!(
            listed_hash(&json!({"pubdata_type": "WithdrawV2", "hash": "ab"})).unwrap(),
            "ab"
        );
        assert!(listed_hash(&json!({"pubdata_type": "L2Transfer", "hash": "ab"})).is_err());
        assert!(listed_hash(&json!({"pubdata_type": "Withdraw", "hash": ""})).is_err());
    }

    #[test]
    fn the_explorer_s_times_read_as_microseconds_since_1970() {
        assert_eq!(utc_micros("1970-01-01T00:00:00Z"), Some(0));
        // The exchange's own clock for the same moment.
        assert_eq!(
            utc_micros("2026-10-06T10:14:06.042647Z"),
            Some(1_791_281_646_042_647)
        );
        assert_eq!(
            utc_micros("2026-10-06T10:14:05.9Z"),
            Some(1_791_281_645_900_000)
        );
        assert_eq!(
            utc_micros("2000-02-29T23:59:59.1234567Z"),
            Some(951_868_799_123_456)
        );
        for broken in [
            "2026-10-06 10:14:05Z",
            "2026-10-06T10:14:05",
            "2026-13-06T10:14:05Z",
            "2026-10-06T10:14:05.x1Z",
        ] {
            assert_eq!(utc_micros(broken), None, "{broken}");
        }
        assert_eq!(
            newest_block_us(
                &json!([{"block_height": 2, "updated_at": "1970-01-01T00:00:01Z"},
                {"block_height": 1, "updated_at": "1970-01-01T00:00:00Z"}])
            )
            .unwrap(),
            1_000_000
        );
        assert!(newest_block_us(&json!([])).is_err());
    }
}
