//! A read inside the ViewReads helper runs with a gas allowance of its own.
//! A view that calls another contract which needs more than that ends with
//! an empty revert (Solidity passes the failed call's empty data on), which
//! looks exactly like a view that reverts with nothing to say. Such a read is
//! read again with more gas at the same block, so a view that returns when
//! given the gas is never reported as reverted; a real empty revert is still
//! reported as one, as a read of its own would find it.

mod stand_in_node;

use alloy::primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use chain::follow::{At, Item, Path, Plan};
use stand_in_node::{Anvil, FollowProbe, PROBE, policy};

/// Where the tests put the forwarder.
const CALLER: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xca, 0x11, 0xe7, 1,
]);

/// A contract that passes its calldata on to the probe with every bit of gas
/// it has (a static call), returns what the probe returned, and reverts with
/// no data when the probe's call failed, whatever the reason: as a
/// Solidity view calling another contract does.
fn forwarder() -> Bytes {
    let mut code = vec![
        0x36, // CALLDATASIZE
        0x60, 0x00, // PUSH1 0
        0x60, 0x00, // PUSH1 0
        0x37, // CALLDATACOPY: the calldata to memory 0
        0x60, 0x00, // PUSH1 0: return size
        0x60, 0x00, // PUSH1 0: return offset
        0x36, // CALLDATASIZE: argument size
        0x60, 0x00, // PUSH1 0: argument offset
        0x73, // PUSH20 the probe
    ];
    code.extend_from_slice(PROBE.as_slice());
    code.extend_from_slice(&[
        0x5a, // GAS
        0xfa, // STATICCALL
        0x15, // ISZERO
        0x60, 0x32, // PUSH1 the failure's offset
        0x57, // JUMPI
        0x3d, // RETURNDATASIZE
        0x60, 0x00, // PUSH1 0
        0x60, 0x00, // PUSH1 0
        0x3e, // RETURNDATACOPY
        0x3d, // RETURNDATASIZE
        0x60, 0x00, // PUSH1 0
        0xf3, // RETURN
        0x5b, // JUMPDEST (the failure, at 0x32)
        0x60, 0x00, // PUSH1 0
        0x80, // DUP1
        0xfd, // REVERT, with no data
    ]);
    assert_eq!(code[0x32], 0x5b, "the failure's offset is its JUMPDEST");
    code.into()
}

/// Rounds of the probe's burn that need about two million gas: more than
/// the helper gives a read, far less than a call's whole budget.
const ROUNDS: u64 = 20_000;

/// A chain with the probe (value 5) and the forwarder, and Multicall3 where
/// `multicall`.
async fn chain(id: u64, multicall: bool) -> Anvil {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 5).await;
    anvil.set_code(CALLER, &forwarder()).await;
    if multicall {
        anvil
            .set_code(
                stand_in_node::MULTICALL,
                &stand_in_node::code("StandInMulticall3"),
            )
            .await;
    }
    anvil.mine(3, 1).await;
    anvil
}

/// The reads, and what each must be answered with.
fn reads() -> (Plan, Vec<Item>) {
    let mut plan = Plan::default();
    let burn = FollowProbe::burnCall {
        rounds: U256::from(ROUNDS),
    };
    plan.read(CALLER, &burn);
    plan.read(PROBE, &FollowProbe::refuseEmptyCall {});
    plan.read(CALLER, &FollowProbe::refuseEmptyCall {});
    plan.read(PROBE, &FollowProbe::valueCall {});
    let word = |value: u64| Item::Returned(Bytes::from(U256::from(value).to_be_bytes::<32>()));
    (
        plan,
        vec![
            word(5 + ROUNDS),
            Item::Reverted(Bytes::new()),
            Item::Reverted(Bytes::new()),
            word(5),
        ],
    )
}

async fn observed_as_expected(anvil: &Anvil, multicall: bool) {
    let mut follower = if multicall {
        stand_in_node::follower(&[&anvil.url], policy()).await
    } else {
        // Multicall3 looked for where it is not.
        chain::follow::Follower::new(stand_in_node::connect(&[&anvil.url]).await, policy())
    };
    let (plan, expected) = reads();
    for at in ["the head", "a pinned block"] {
        let observed = if at == "the head" {
            follower.observe(&plan, At::Latest).await
        } else {
            let head = anvil.head().await;
            let (hash, parent_hash, timestamp) = anvil.block(head).await;
            follower
                .observe(
                    &plan,
                    At::Pinned(chain::follow::Header {
                        number: head,
                        hash,
                        parent_hash,
                        timestamp,
                    }),
                )
                .await
        }
        .unwrap_or_else(|error| panic!("an observation at {at}: {error:#}"));
        assert_eq!(observed.path, Path::ViewReads, "at {at}");
        assert_eq!(observed.items, expected, "at {at}");
    }
}

#[tokio::test]
async fn a_view_out_of_gas_inside_a_call_is_read_again_through_multicall3() {
    let anvil = chain(31_490_001, true).await;
    observed_as_expected(&anvil, true).await;
}

#[tokio::test]
async fn a_view_out_of_gas_inside_a_call_is_read_alone_where_hash_pins_are_honoured() {
    let anvil = chain(31_490_002, false).await;
    observed_as_expected(&anvil, false).await;
}

/// The premise: inside the helper, with the gas it gives a read, the
/// forwarded burn does run out and revert empty, and given a call's gas it
/// returns.
#[tokio::test]
async fn the_forwarded_burn_needs_more_gas_than_the_helper_gives_a_read() {
    let anvil = chain(31_490_003, false).await;
    let call = |gas: u64| {
        let anvil = &anvil;
        async move {
            anvil
                .rpc(
                    "eth_call",
                    serde_json::json!([{
                        "to": CALLER,
                        "data": Bytes::from(FollowProbe::burnCall { rounds: U256::from(ROUNDS) }.abi_encode()),
                        "gas": format!("0x{gas:x}"),
                    }, "latest"]),
                )
                .await
        }
    };
    assert!(
        call(1_000_000).await.is_err(),
        "a million gas is too little"
    );
    assert!(call(30_000_000).await.is_ok(), "a call's budget is enough");
}

/// Where the endpoint has no Multicall3 and honours no hash pin, nothing can
/// give the read more gas at the block: the observation still stands, the
/// read answered as the helper answered it (as a revert that says nothing),
/// and the real reverts and the other reads as ever.
#[tokio::test]
async fn where_no_read_can_have_more_gas_the_observation_stands_as_the_helper_answered() {
    let anvil = chain(31_490_004, false).await;
    let proxy = stand_in_node::Proxy::start(&anvil.url).await;
    proxy.script(|script| script.ignore_pins = true);
    let mut follower =
        chain::follow::Follower::new(stand_in_node::connect(&[&proxy.url]).await, policy());
    let (plan, mut expected) = reads();
    expected[0] = Item::Reverted(Bytes::new());
    let observed = follower
        .observe(&plan, At::Latest)
        .await
        .expect("an observation");
    assert_eq!(observed.path, Path::ViewReads);
    assert_eq!(observed.items, expected);
    let known = follower.capabilities(0).unwrap();
    assert_eq!(
        (known.multicall3, known.hash_pin),
        (Some(false), Some(false)),
        "both were asked about"
    );
}
