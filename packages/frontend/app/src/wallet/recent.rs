//! What this browser sent: the last ten deposits, withdrawals and claims,
//! kept in the browser's storage so the portfolio can show a payout or a
//! pending withdrawal after the page that sent it is gone. It is this
//! browser's record only: the wallet's own history, and the chain, have
//! everything.

use std::rc::Rc;

use futures_signals::signal::Mutable;
use serde::{Deserialize, Serialize};

/// How many sends are kept.
pub const KEEP: usize = 10;

const KEY: &str = "recent.v1";

/// Which kind of wallet sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Evm,
    Solana,
}

/// What was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deposit,
    Withdrawal,
    Claim,
}

/// One send, as this browser saw it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The transaction's hash or signature: what it is known by.
    pub tx: String,
    /// Its page on the chain's explorer.
    pub tx_url: Option<String>,
    pub family: Family,
    /// The wallet that sent it, as the chain writes it.
    pub account: String,
    pub slug: String,
    pub title: String,
    pub kind: Kind,
    /// "$250.00" or "12.5 shares"; for a claim, what was done:
    /// "Settling claim 3".
    pub amount: String,
    /// When it was sent, in milliseconds since the Unix epoch.
    pub at_ms: f64,
    /// How it ended, in words, once its flow said: "Paid $251.20".
    pub outcome: Option<String>,
}

impl Entry {
    /// Where it stands, in a few words.
    pub fn status(&self) -> String {
        match (&self.outcome, self.kind) {
            (Some(outcome), _) => outcome.clone(),
            (None, Kind::Deposit | Kind::Withdrawal) => {
                "Pending, priced at the next valuation".to_string()
            }
            (None, Kind::Claim) => "Sent".to_string(),
        }
    }

    /// "Withdrawal of 12.5 shares from Market maker on Orca".
    pub fn what(&self) -> String {
        match self.kind {
            Kind::Deposit => format!("Deposit of {} to {}", self.amount, self.title),
            Kind::Withdrawal => format!("Withdrawal of {} from {}", self.amount, self.title),
            Kind::Claim => format!("{} on {}", self.amount, self.title),
        }
    }
}

/// `list` with `entry` first: a send already listed (by its transaction) is
/// replaced, and only the newest [`KEEP`] stay.
pub fn with(list: &[Entry], entry: Entry) -> Vec<Entry> {
    let tx = entry.tx.clone();
    let mut next = vec![entry];
    next.extend(list.iter().filter(|old| old.tx != tx).cloned());
    next.truncate(KEEP);
    next
}

/// `list` with the send known by `tx` marked as ended `outcome`.
pub fn settled(list: &[Entry], tx: &str, outcome: &str) -> Vec<Entry> {
    list.iter()
        .map(|entry| {
            let mut entry = entry.clone();
            if entry.tx == tx {
                entry.outcome = Some(outcome.to_string());
            }
            entry
        })
        .collect()
}

/// What storage held, read: anything that doesn't read is no record at all.
pub fn parse(raw: Option<&str>) -> Vec<Entry> {
    let mut list: Vec<Entry> = raw
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    list.truncate(KEEP);
    list
}

thread_local! {
    static RECENT: Rc<Mutable<Vec<Entry>>> = Rc::new(Mutable::new(load()));
}

fn load() -> Vec<Entry> {
    #[cfg(target_arch = "wasm32")]
    {
        parse(crate::web::storage_get(KEY).as_deref())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = KEY;
        Vec::new()
    }
}

fn save(list: &[Entry]) {
    #[cfg(target_arch = "wasm32")]
    if let Ok(raw) = serde_json::to_string(list) {
        crate::web::storage_set(KEY, &raw);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = list;
}

/// This browser's sends, newest first, as they change.
pub fn get() -> Rc<Mutable<Vec<Entry>>> {
    RECENT.with(Rc::clone)
}

/// Keep a send this browser just made.
pub fn record(entry: Entry) {
    let recent = get();
    let next = with(&recent.lock_ref(), entry);
    save(&next);
    recent.set(next);
}

/// Say how a kept send ended.
pub fn settle(tx: &str, outcome: &str) {
    let recent = get();
    if !recent.lock_ref().iter().any(|entry| entry.tx == tx) {
        return;
    }
    let next = settled(&recent.lock_ref(), tx, outcome);
    save(&next);
    recent.set(next);
}

/// The sends of one wallet, newest first.
pub fn of(list: &[Entry], family: Family, account: &str) -> Vec<Entry> {
    list.iter()
        .filter(|entry| entry.family == family && entry.account == account)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tx: &str, kind: Kind) -> Entry {
        Entry {
            tx: tx.to_string(),
            tx_url: Some(format!("https://sepolia.basescan.org/tx/{tx}")),
            family: Family::Evm,
            account: "0x7e".to_string(),
            slug: "base-2".to_string(),
            title: "Funding basis".to_string(),
            kind,
            amount: "12.5 shares".to_string(),
            at_ms: 1.0,
            outcome: None,
        }
    }

    #[test]
    fn only_the_last_ten_sends_are_kept_newest_first_and_a_send_is_listed_once() {
        let mut list = Vec::new();
        for i in 0..12 {
            list = with(&list, entry(&format!("0x{i}"), Kind::Deposit));
        }
        assert_eq!(list.len(), KEEP);
        assert_eq!(list[0].tx, "0x11");
        assert_eq!(list[9].tx, "0x2");
        let again = with(&list, entry("0x5", Kind::Deposit));
        assert_eq!(again.len(), KEEP);
        assert_eq!(again[0].tx, "0x5");
        assert_eq!(again.iter().filter(|e| e.tx == "0x5").count(), 1);
    }

    #[test]
    fn a_withdrawal_says_paid_once_its_flow_said_so_and_pending_until_then() {
        let list = with(&[], entry("0xaa", Kind::Withdrawal));
        assert_eq!(list[0].status(), "Pending, priced at the next valuation");
        let list = settled(&list, "0xaa", "Paid $251.20");
        assert_eq!(list[0].status(), "Paid $251.20");
        assert_eq!(
            list[0].what(),
            "Withdrawal of 12.5 shares from Funding basis"
        );
        // Another send's ending changes nothing here.
        let list = settled(&list, "0xbb", "Paid $1.00");
        assert_eq!(list[0].status(), "Paid $251.20");
    }

    #[test]
    fn a_record_storage_mangled_or_refused_is_no_record_rather_than_a_failure() {
        assert!(parse(None).is_empty());
        assert!(parse(Some("not json")).is_empty());
        assert!(parse(Some("{\"tx\":1}")).is_empty());
        let raw = serde_json::to_string(&vec![entry("0x1", Kind::Claim)]).unwrap();
        assert_eq!(parse(Some(&raw)), vec![entry("0x1", Kind::Claim)]);
    }

    #[test]
    fn each_wallet_sees_only_its_own_sends() {
        let mut solana = entry("sig", Kind::Deposit);
        solana.family = Family::Solana;
        solana.account = "B16P".to_string();
        let list = with(&with(&[], entry("0x1", Kind::Deposit)), solana);
        assert_eq!(of(&list, Family::Evm, "0x7e").len(), 1);
        assert_eq!(of(&list, Family::Solana, "B16P")[0].tx, "sig");
        assert!(of(&list, Family::Solana, "0x7e").is_empty());
    }
}
