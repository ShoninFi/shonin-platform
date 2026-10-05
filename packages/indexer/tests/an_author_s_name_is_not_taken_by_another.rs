//! One author's name can't be put on another author's strategies.
//!
//! The record names a strategy's author by the handle they set on
//! `AuthorProfiles`, or by their address (`{:?}`, checksummed) when they set
//! none, and the web app's author page lists every strategy whose `author`
//! equals the page's name, ignoring case. `AuthorProfiles` keeps a handle
//! unique only by the keccak of its exact bytes, so anyone may take as their
//! handle the address another author is shown by (or another spelling of
//! their handle, or their handle on another chain). An author is their
//! address, and a handle only what they are called: it never makes two
//! authors one.
//!
//! Here Alice publishes and sets no handle, so her strategy reads "by
//! 0xA11c…". Mallory sets `0xA11c…` as her handle, which the contract takes
//! (that handle's hash is unowned), and publishes. Each strategy is shown by
//! a name of its author's own, and the record lists two authors: Alice's
//! page lists only hers, and Mallory's card never wears Alice's name.
//! Publishing's defence against an author relaunching to escape a record
//! ("an author's page shows every strategy they've published") holds.

mod stand_in;

use std::sync::Arc;

use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use chain::bindings::vault::IVault;
use serde_json::json;
use stand_in::*;

const ALICE_VAULT: &str = "00000000000000000000000000000000000000be";
const MALLORY_VAULT: &str = "00000000000000000000000000000000000000bf";

fn alice() -> Address {
    "0xA11cE00000000000000000000000000000000001"
        .parse()
        .unwrap()
}

fn mallory() -> Address {
    "0x3a110700000000000000000000000000000000a2"
        .parse()
        .unwrap()
}

fn word(address: Address) -> String {
    format!("0x{}{}", "0".repeat(24), alloy::hex::encode(address))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_author_s_name_is_not_taken_by_another() {
    let indexer = an_indexer(
        Arc::new(|request| {
            if request["method"] != "eth_call" {
                return None;
            }
            let to = request["params"][0]["to"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let data = input(request);
            if data == selector::<IVaultFactory::vaultsCall>() {
                return Some(Ok(json!(format!(
                    "0x{}{}{}{}",
                    "0".repeat(62) + "20",
                    "0".repeat(63) + "2",
                    "0".repeat(24) + ALICE_VAULT,
                    "0".repeat(24) + MALLORY_VAULT
                ))));
            }
            if data == selector::<IVault::authorCall>() {
                let author = if to.ends_with(MALLORY_VAULT) {
                    mallory()
                } else {
                    alice()
                };
                return Some(Ok(json!(word(author))));
            }
            if data.starts_with(&selector::<IAuthorProfiles::profileOfCall>()) {
                // Mallory's handle is the name Alice is shown by; Alice set
                // none.
                let asks_for_mallory =
                    data.ends_with(&alloy::hex::encode(mallory()).to_ascii_lowercase());
                let profile = Profile {
                    handle: if asks_for_mallory {
                        format!("{:?}", alice())
                    } else {
                        String::new()
                    },
                    blurb: String::new(),
                    linkLabels: vec![],
                    linkUrls: vec![],
                    joinedAt: 1,
                };
                return Some(Ok(json!(format!(
                    "0x{}",
                    alloy::hex::encode(IAuthorProfiles::profileOfCall::abi_encode_returns(
                        &profile
                    ))
                ))));
            }
            None
        }),
        "an-author-s-name",
    )
    .await;

    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(snapshot.strategies.len(), 2, "both vaults are read");
    let by: Vec<(String, String)> = snapshot
        .strategies
        .iter()
        .map(|strategy| (strategy.author.clone(), strategy.author_address.clone()))
        .collect();
    assert!(
        !by[0].0.eq_ignore_ascii_case(&by[1].0),
        "two different authors are shown by one name, so one author page lists both: {by:?}"
    );
    let alice_s = by
        .iter()
        .find(|(_, address)| address.eq_ignore_ascii_case(&format!("{:?}", alice())))
        .expect("Alice's strategy is read");
    assert_eq!(
        alice_s.0,
        format!("{:?}", alice()),
        "Alice, who set no handle, is shown by her address"
    );
    assert_eq!(
        snapshot.authors.len(),
        2,
        "two authors published, and the record lists {}",
        snapshot.authors.len()
    );
    let mut listed: Vec<String> = snapshot
        .authors
        .iter()
        .map(|author| author.handle.to_lowercase())
        .collect();
    let mut shown: Vec<String> = by.iter().map(|(name, _)| name.to_lowercase()).collect();
    listed.sort();
    shown.sort();
    assert_eq!(
        listed, shown,
        "each author is listed by the name their strategy shows"
    );
}
