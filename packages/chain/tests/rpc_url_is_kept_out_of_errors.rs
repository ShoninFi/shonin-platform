//! A paid RPC URL carries its key, so no error a service might log or return
//! names it. Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use alloy::primitives::Address;
use chain::bindings::erc20::IERC20;
use chain::{ChainKey, ChainKind, NetworkName, Signing};

const SECRET: &str = "sk-live-0123456789abcdef";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_error_does_not_name_the_rpc_url() {
    let url = stand_in::start(stand_in::node(2)).await;
    let paid = format!("{url}/v2/{SECRET}");
    let key = ChainKey::new(ChainKind::Evm, "1");

    let error = match chain::connect_to(NetworkName::Mainnet, &key, &stand_in::config(&paid)).await
    {
        Ok(_) => panic!("chain 2 is not chain 1"),
        Err(error) => error,
    };
    let text = format!("{error:#}");
    assert!(
        !text.contains(SECRET),
        "the error carries the RPC key: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_that_fails_on_the_way_does_not_name_the_rpc_url() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let paid = format!("{url}/v2/{SECRET}");
    let client = chain::Client::connect(&stand_in::config(&paid), Signing::ReadOnly)
        .await
        .unwrap();

    // The node drops the connection: the HTTP client's own error names the
    // URL it was sending to.
    node.lock().unwrap().hangs_up = true;
    let error = client
        .read(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
        )
        .await
        .expect_err("the node hung up");
    let text = format!("{error:#}");
    assert!(
        !text.contains(SECRET),
        "the error carries the RPC key: {text}"
    );
    // Still a failure to be answered, not a refusal.
    assert!(chain::unanswered(&error), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_that_names_the_request_path_does_not_name_the_rpc_key() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let paid = format!("{url}/v2/{SECRET}");
    let client = chain::Client::connect(&stand_in::config(&paid), Signing::ReadOnly)
        .await
        .unwrap();

    // The gateway answers with its default 404, which names the path it was
    // asked for.
    node.lock().unwrap().echo_path = true;
    let error = client
        .read(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
        )
        .await
        .expect_err("the gateway answered 404");
    let text = format!("{error:#}");
    assert!(
        !text.contains(SECRET),
        "the error carries the RPC key: {text}"
    );
    // Still a failure to be answered, not a refusal of the call itself.
    assert!(chain::unanswered(&error), "{text}");
}

/// What a read comes back with when the node gives `body` with a 200.
async fn read_error_with(body: String) -> String {
    let answer: stand_in::Answer =
        std::sync::Arc::new(move |_method, _request| stand_in::Said::Raw(200, body.clone()));
    let url = stand_in::answering(Some(31337), answer).await;
    let paid = format!("{url}/v2/{SECRET}");
    let client = chain::Client::connect(&stand_in::config(&paid), Signing::ReadOnly)
        .await
        .unwrap();
    let error = client
        .read(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
        )
        .await
        .expect_err("the gateway refused");
    format!("{error:#} / {error:?}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bare_error_object_naming_the_path_does_not_carry_the_rpc_key() {
    // Not a JSON-RPC response, but a refusal the transport reads out of it.
    let text = read_error_with(format!(
        r#"{{"code":-32001,"message":"no project at /v2/{SECRET}","data":"/v2/{SECRET}"}}"#
    ))
    .await;
    assert!(
        !text.contains(SECRET),
        "the error carries the RPC key: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_error_given_as_a_string_does_not_carry_the_rpc_key() {
    // Neither: the parser's own message quotes the string it could not read.
    let text = read_error_with(format!(
        r#"{{"jsonrpc":"2.0","id":0,"error":"unknown api key {SECRET}"}}"#
    ))
    .await;
    assert!(
        !text.contains(SECRET),
        "the error carries the RPC key: {text}"
    );
}
