//! What a vault's round memory keeps across a proved replacement of the
//! branch, and what a vault whose timers are under the RPC settings' margin
//! is still owed.

use super::*;

/// A proved replacement forgets what was read of the chain (finished
/// prefixes, logs and what they showed, a closed vault's last finding), and
/// keeps every pace, hold, back-off and "sent" mark: nothing goes twice.
#[test]
fn a_replacement_forgets_only_what_was_read_of_the_chain() {
    let mut memory = Memory {
        claims_from: 4,
        requests_from: 7,
        holders: BTreeSet::from([Address::repeat_byte(1)]),
        holders_read_to: 900,
        retired_at: Some(1_000),
        retired_read_to: 900,
        skipped: BTreeSet::from([(B256::repeat_byte(2), Address::repeat_byte(3))]),
        skipped_read_to: 900,
        quiet_since: Some(1_000),
        closer_closed_nothing: Some(5),
        daily_at: Some(1_000),
        unwound_in: Some(990),
        bill_sent_as_of: 800,
        close_held_since: Some((950, std::time::Instant::now())),
        written_off_for: Some(940),
        keep_again: Some(SendAgain {
            wait: 2,
            next: 1_020,
            next_round: 9,
        }),
        ..Memory::default()
    };
    memory.claims_tried.insert(
        5,
        Tried {
            state: 1,
            wait: 2,
            next: 1_020,
            next_round: 8,
        },
    );
    memory.serve_again.insert(
        Address::repeat_byte(1),
        SendAgain {
            wait: 1,
            next: 1_010,
            next_round: 7,
        },
    );
    memory.forget_branch();
    assert_eq!(
        (
            memory.claims_from,
            memory.requests_from,
            memory.holders.len(),
            memory.holders_read_to,
            memory.retired_at,
            memory.retired_read_to,
            memory.skipped.len(),
            memory.skipped_read_to,
            memory.quiet_since,
            memory.closer_closed_nothing,
        ),
        (0, 0, 0, 0, None, 0, 0, 0, None, None),
        "what was read of the chain is read again"
    );
    assert_eq!(memory.daily_at, Some(1_000));
    assert_eq!(memory.unwound_in, Some(990));
    assert_eq!(memory.bill_sent_as_of, 800);
    assert!(memory.close_held_since.is_some());
    assert_eq!(memory.written_off_for, Some(940));
    assert_eq!(memory.keep_again.map(|again| again.next), Some(1_020));
    assert_eq!(memory.claims_tried[&5].next, 1_020);
    assert_eq!(memory.serve_again[&Address::repeat_byte(1)].next, 1_010);
}

/// The factory refuses no slot window above zero. A vault whose slot window
/// is under the keeper's settings' margin may have its valuations arrive
/// late; nothing about its activation window, claims, freeze, halt grace or
/// bill depends on that window. The actions those are owed are still sent.
#[tokio::test]
async fn a_vault_whose_slot_window_is_under_the_margin_is_still_cancelled_past_its_window() {
    let now = BETWEEN_SLOTS;
    let node = node(now);
    let mut short = timers();
    short.slotWindow = 1;
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    answers(&node, VAULT, IVault::timersCall {}, short.clone());
    answers(&node, VAULT, IVault::createdAtCall {}, now - 4 * DAY);
    crate::stand_in::takes(&node, VAULT, IVault::cancelCall {});
    let client = connect(&node).await;
    let policy = chain::rpc_policy::RpcPolicy::default();
    assert!(
        judge_timers(&policy, &policy, VAULT, &short, 1_000, 1)
            .iter()
            .any(|fit| *fit != chain::rpc_policy::Fit::Fits),
        "setup: the slot window must be under the margin"
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

/// Startup refusals, judged as the established settings would: only a timer
/// the established settings meet and the configured ones miss is refused;
/// one the established settings miss too is served best-effort. Every
/// endpoint an observation may try counts against the window.
#[test]
fn startup_refuses_only_what_the_established_settings_meet_and_counts_every_endpoint() {
    use chain::rpc_policy::{Fit, RpcPolicy};
    let established = RpcPolicy::default();
    let demo = timers();
    let fits = |configured: &RpcPolicy, timers: &chain::bindings::vault::Timers, endpoints| {
        judge_timers(configured, &established, VAULT, timers, 2_000, endpoints)
    };
    // 2 * (2 s + 1 s) = 6 s against 5 s + 3 s - 2 s: exactly met.
    assert_eq!(fits(&established, &demo, 1)[0], Fit::Fits);
    // A second endpoint makes it 2 * (2 s + 2 s) = 8 s: missed by the
    // established settings too, so served best-effort.
    assert!(matches!(
        fits(&established, &demo, 2)[0],
        Fit::BestEffort(_)
    ));
    // One millisecond more of read budget: refused, by name.
    let slower = RpcPolicy {
        read_budget_ms: established.read_budget_ms + 1,
        ..established.clone()
    };
    match &fits(&slower, &demo, 1)[0] {
        Fit::Refused(why) => assert!(
            why.contains("read_budget_ms=1001") && why.contains("slotLead=5s"),
            "{why}"
        ),
        other => panic!("not refused: {other:?}"),
    }
    // A head as old as the lag bound must still show the slot's requests:
    // 4 s + a 2 s block is not below a 5 s lead even at the established
    // settings for this chain kind, so it is best-effort, never refused.
    let lagging = RpcPolicy {
        head_lag_max_ms: 4_000,
        ..established.clone()
    };
    let judged = judge_timers(&lagging, &lagging, VAULT, &demo, 2_000, 1);
    assert!(matches!(judged[1], Fit::BestEffort(_)), "{judged:?}");
    // The notice bound and an ask fit a 600 s grace and the last orders.
    assert_eq!(fits(&established, &demo, 1)[2], Fit::Fits);
}
