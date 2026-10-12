//! What a round remembers across head observations, and what a vault whose
//! timers are under the observation margin is still owed.

use super::*;

async fn one_round(
    client: &Client,
    memory: &mut HashMap<Address, Memory>,
    factories: &mut Factories,
    schedule: &mut schedule::Schedule,
) {
    tick(
        Chain::Base,
        client,
        &deployments(),
        KEEPER,
        "http://127.0.0.1:1",
        memory,
        factories,
        schedule,
    )
    .await
    .unwrap();
}

/// A public endpoint list is served one endpoint at a time, and the next is
/// asked when one refuses; the nodes behind them are often a block apart.
/// The head read after a failover may therefore be numbered one below the
/// head read before it, on the same chain, with no block replaced. The
/// vault's round memory (the slot its stopped station was last unwound in,
/// the hold on a halted vault's close, the backoff of a reverted send) must
/// survive that: nothing it describes belongs to a replaced branch.
#[tokio::test]
async fn a_head_a_block_behind_the_last_one_keeps_the_vaults_round_memory() {
    let now = BETWEEN_SLOTS;
    let node = node(now);
    an_open_vault(&node, a_tank(now, 0, now));
    a_quiet_vault(&node, now);
    let factory = deployments().factory;
    answers(&node, factory, IVaultFactory::vaultsCall {}, vec![VAULT]);
    answers(
        &node,
        factory,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    let client = connect(&node).await;
    let mut schedule = schedule::Schedule::default();
    let mut memory = HashMap::new();
    let mut factories = Factories::default();
    // The first round starts the head observer; the second consumes its
    // first observation of the head.
    one_round(&client, &mut memory, &mut factories, &mut schedule).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    one_round(&client, &mut memory, &mut factories, &mut schedule).await;
    let marker = 0x5107u64;
    memory.get_mut(&VAULT).unwrap().unwound_in = Some(marker);
    let rounds = memory[&VAULT].rounds;
    assert_eq!(rounds, 2);
    // The next head read is answered by a node one block behind the last.
    node.lock().unwrap().head -= 1;
    let stop = std::time::Instant::now() + Duration::from_secs(7);
    let mut later = 0;
    while std::time::Instant::now() < stop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        one_round(&client, &mut memory, &mut factories, &mut schedule).await;
        later += 1;
        let remembered = &memory[&VAULT];
        assert_eq!(
            (remembered.unwound_in, remembered.rounds),
            (Some(marker), rounds + later),
            "a head one block behind the last one wiped the vault's round memory"
        );
    }
}

/// The factory refuses no slot window above zero. A vault whose slot window
/// is under the keeper's observation margin may have its valuations arrive
/// late; nothing about its activation window, claims, freeze, halt grace or
/// bill depends on that window. The actions those are owed must still be
/// sent.
#[tokio::test]
async fn a_vault_whose_slot_window_is_under_the_margin_is_still_cancelled_past_its_window() {
    let now = BETWEEN_SLOTS;
    let node = node(now);
    let mut short = timers();
    short.slotWindow = 2;
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    answers(&node, VAULT, IVault::timersCall {}, short.clone());
    answers(&node, VAULT, IVault::createdAtCall {}, now - 4 * DAY);
    stand_in::takes(&node, VAULT, IVault::cancelCall {});
    let client = connect(&node).await;
    client.rpc_policy().validate("observation").unwrap();
    assert!(
        validate_timers(&client, VAULT, &short).is_err(),
        "setup: the slot window must be under the configured margin"
    );
    let mut memory = Memory::default();
    let done = keep_one(
        &client,
        KEEPER,
        &deployments(),
        "http://127.0.0.1:1",
        VAULT,
        &mut memory,
    )
    .await;
    assert_eq!(
        sent::<IVault::cancelCall>(&node, VAULT),
        1,
        "the cancel owed past the activation window was not sent: {done:?}; sent {:?}",
        everything_sent(&node)
    );
}
