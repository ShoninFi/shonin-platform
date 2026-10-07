//! A testnet service started without the mnemonic knows every role's
//! account from the deployment, but holds the key of one at most (the
//! keeper's). Connecting needs no key; sending or signing as a role whose
//! key it doesn't hold fails at that call, saying which role, before
//! anything reaches the node.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use chain::bindings::erc20::IERC20;
use chain::{Role, Signing};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sending_or_signing_as_a_role_without_its_key_fails_at_the_call_and_sends_nothing() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let keeper = PrivateKeySigner::random();
    let mut accounts: Vec<Address> = (1..=chain::ROLE_COUNT as u8)
        .map(Address::repeat_byte)
        .collect();
    accounts[Role::Keeper.account_index()] = keeper.address();
    let client = chain::Client::connect(
        &stand_in::config(&url),
        Signing::Roles {
            accounts: accounts.clone(),
            keys: vec![keeper.clone()],
        },
    )
    .await
    .expect("connecting needs no key");

    // Every role's account is known.
    let admin = client.account(Role::Admin).unwrap();
    assert_eq!(admin, accounts[Role::Admin.account_index()]);
    assert_eq!(client.account(Role::Keeper).unwrap(), keeper.address());
    assert!(!client.is_development());
    client.holds_key_for(keeper.address()).unwrap();

    let error = client
        .send(
            admin,
            Address::repeat_byte(0x22),
            IERC20::transferCall {
                to: Address::ZERO,
                value: U256::from(1),
            },
        )
        .await
        .expect_err("no key for the admin");
    let text = format!("{error:#}");
    assert!(text.contains("admin role"), "{text}");
    assert!(
        error.chain().any(|cause| cause.is::<chain::NeverSent>()),
        "{text}"
    );
    assert!(!chain::may_still_land(&error), "{text}");

    let error = client
        .sign(admin, B256::repeat_byte(1))
        .await
        .expect_err("no key for the admin");
    assert!(format!("{error:#}").contains("admin role"), "{error:#}");

    assert!(node.lock().unwrap().sent.is_empty(), "something was sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_with_no_key_at_all_still_reads_and_refuses_every_send() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let accounts: Vec<Address> = (1..=chain::ROLE_COUNT as u8)
        .map(Address::repeat_byte)
        .collect();
    let client = chain::Client::connect(
        &stand_in::config(&url),
        Signing::Roles {
            accounts,
            keys: Vec::new(),
        },
    )
    .await
    .unwrap();
    let keeper = client.account(Role::Keeper).unwrap();
    let error = client
        .transfer(keeper, Address::repeat_byte(0x22), U256::from(1))
        .await
        .expect_err("no key for the keeper");
    assert!(format!("{error:#}").contains("keeper role"), "{error:#}");
    assert!(node.lock().unwrap().sent.is_empty(), "something was sent");
}
