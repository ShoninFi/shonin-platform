//! Drivers and bridges wait like images: each is proposed behind the
//! timelock, can be withdrawn while it waits, and is revoked at once. What a
//! station created now pins is the latest approval unless it was revoked; a
//! revocation outlasts an earlier proposal of the same implementation, and a
//! revoked implementation proposed again waits a full timelock. Each must be
//! a program nobody can ever change, since a station pins it for good.

use anchor_lang::prelude::Pubkey;
use shonin_registry::error::RegistryError;
use shonin_registry::read::revoked_address;
use shonin_solana_tests::*;
use solana_signer::Signer;

const DRIVER: ApprovalKind = ApprovalKind::Driver;
const BRIDGE: ApprovalKind = ApprovalKind::Bridge;

fn test_driver() -> [u8; 32] {
    name("test-driver")
}

#[test]
fn a_driver_waits_out_the_timelock() {
    let mut env = Env::new();
    let v1 = env.final_program();
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    assert_eq!(env.trading_driver(test_driver()), Pubkey::default());
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(test_driver()), v1);
}

#[test]
fn a_new_driver_version_replaces_the_old_only_when_ready() {
    let mut env = Env::new();
    let (v1, v2) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    expect_ok(env.propose(DRIVER, test_driver(), v2));
    assert_eq!(env.trading_driver(test_driver()), v1);
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(test_driver()), v2);
}

#[test]
fn revoking_a_driver_stops_new_stations_pinning_it() {
    let mut env = Env::new();
    let v1 = env.final_program();
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, test_driver(), v1));
    assert_eq!(env.trading_driver(test_driver()), Pubkey::default());
    assert!(env.is_revoked(DRIVER, test_driver(), v1));
}

#[test]
fn withdrawing_a_proposal_leaves_the_driver_in_force() {
    let mut env = Env::new();
    let (v1, bad) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    expect_ok(env.propose(DRIVER, test_driver(), bad));
    expect_ok(env.withdraw(DRIVER, test_driver()));
    env.skip(TIMELOCK);
    // The waiting proposal never counts, and the one in force stands.
    assert_eq!(env.trading_driver(test_driver()), v1);
    assert!(!env.is_revoked(DRIVER, test_driver(), v1));
}

#[test]
fn an_older_driver_stations_still_pin_can_be_revoked() {
    let mut env = Env::new();
    let (v1, v2) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    expect_ok(env.propose(DRIVER, test_driver(), v2));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, test_driver(), v1));
    assert!(env.is_revoked(DRIVER, test_driver(), v1));
    assert_eq!(
        env.trading_driver(test_driver()),
        v2,
        "revoking an older one touched the newer"
    );
}

#[test]
fn revoking_a_proposal_still_waiting_withdraws_it() {
    let mut env = Env::new();
    let v1 = env.final_program();
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    expect_ok(env.revoke(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    assert_eq!(
        env.trading_driver(test_driver()),
        Pubkey::default(),
        "the revoked proposal went live"
    );
}

#[test]
fn a_fix_proposed_after_a_revocation_trades_again() {
    let mut env = Env::new();
    let (v1, v2) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, test_driver(), v1));
    expect_ok(env.propose(DRIVER, test_driver(), v2));
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(test_driver()), v2);
}

/// A proposal still waiting out its timelock can be withdrawn, even the
/// first one for a name, and never goes live.
#[test]
fn the_first_driver_proposal_can_be_withdrawn() {
    let mut env = Env::new();
    let wrong = env.final_program();
    expect_ok(env.propose(DRIVER, test_driver(), wrong));
    expect_ok(env.withdraw(DRIVER, test_driver()));
    env.skip(TIMELOCK);
    assert_ne!(
        env.trading_driver(test_driver()),
        wrong,
        "the withdrawn proposal went live"
    );
}

#[test]
fn there_is_nothing_to_withdraw_once_a_proposal_is_in_force() {
    let mut env = Env::new();
    expect_code(
        env.withdraw(DRIVER, name("local")),
        registry_error(RegistryError::NoImplementation),
    );
    expect_code(
        env.withdraw(DRIVER, name("never proposed")),
        registry_error(RegistryError::NoImplementation),
    );
}

#[test]
fn the_first_bridge_proposal_can_be_withdrawn() {
    let mut env = Env::new();
    let wrong = env.final_program();
    expect_ok(env.propose(BRIDGE, name("test-bridge"), wrong));
    expect_ok(env.withdraw(BRIDGE, name("test-bridge")));
    env.skip(TIMELOCK);
    assert_eq!(env.bridge_of(name("test-bridge")), Pubkey::default());
}

#[test]
fn bridges_are_approved_the_same_way() {
    let mut env = Env::new();
    let bridge = env.final_program();
    expect_ok(env.propose(BRIDGE, name("test-bridge"), bridge));
    assert_eq!(env.bridge_of(name("test-bridge")), Pubkey::default());
    env.skip(TIMELOCK);
    assert_eq!(env.bridge_of(name("test-bridge")), bridge);
    expect_ok(env.revoke(BRIDGE, name("test-bridge"), bridge));
    assert_eq!(env.bridge_of(name("test-bridge")), Pubkey::default());
    assert!(env.is_revoked(BRIDGE, name("test-bridge"), bridge));
}

/// Drivers and bridges are separate lists, so a name is one or the other:
/// approving or revoking a driver of a name touches no bridge of that name.
#[test]
fn a_name_on_one_list_is_not_on_the_other() {
    let mut env = Env::new();
    let program = env.final_program();
    expect_ok(env.propose(DRIVER, name("same"), program));
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(name("same")), program);
    assert_eq!(env.bridge_of(name("same")), Pubkey::default());
    expect_ok(env.revoke(BRIDGE, name("same"), program));
    assert_eq!(env.trading_driver(name("same")), program);
    assert!(!env.is_revoked(DRIVER, name("same"), program));
}

// Revocations hold ---------------------------------------------------------------

/// Proposing a revoked implementation again (the revoke was a mistake)
/// approves it again once a full timelock has passed.
#[test]
fn a_revoked_driver_proposed_again_trades_again_after_its_timelock() {
    let mut env = Env::new();
    let v1 = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), v1));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, name("local"), v1));
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
    expect_ok(env.propose(DRIVER, name("local"), v1));
    env.skip(TIMELOCK);
    assert!(!env.is_revoked(DRIVER, name("local"), v1));
    assert_eq!(
        env.trading_driver(name("local")),
        v1,
        "a re-proposed implementation never trades again"
    );
}

/// A revocation withdraws any waiting proposal of the same implementation,
/// so it can't lapse when that proposal would have come due.
#[test]
fn a_revoke_made_while_the_driver_is_reproposed_still_holds_after_the_timelock() {
    let mut env = Env::new();
    let driver = env.driver;
    // Day 0: the admin proposes the implementation in force again.
    expect_ok(env.propose(DRIVER, name("local"), driver));
    // Day 1: it needs it gone at once.
    env.skip(DAY);
    expect_ok(env.revoke(DRIVER, name("local"), driver));
    assert!(
        env.is_revoked(DRIVER, name("local"), driver),
        "revoked at once"
    );
    // Day 3: the re-proposal's timelock would have passed.
    env.skip(TIMELOCK);
    assert!(
        env.is_revoked(DRIVER, name("local"), driver),
        "the revoke lapsed when the earlier re-proposal matured"
    );
    assert_eq!(
        env.trading_driver(name("local")),
        Pubkey::default(),
        "the revoked driver is approved again"
    );
}

/// The proposal the revocation withdrew is gone: there is nothing left to
/// withdraw, and nothing comes due.
#[test]
fn a_revocation_leaves_no_proposal_of_the_same_implementation_waiting() {
    let mut env = Env::new();
    let driver = env.driver;
    expect_ok(env.propose(DRIVER, name("local"), driver));
    expect_ok(env.revoke(DRIVER, name("local"), driver));
    env.skip(TIMELOCK);
    expect_code(
        env.withdraw(DRIVER, name("local")),
        registry_error(RegistryError::NoImplementation),
    );
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
}

/// The implementation in force can be revoked at once even while the same
/// implementation is proposed again.
#[test]
fn a_pinned_driver_proposed_again_is_revoked_at_once_while_proposed_again() {
    let mut env = Env::new();
    let driver = env.driver;
    expect_ok(env.propose(DRIVER, name("local"), driver));
    assert_eq!(
        env.trading_driver(name("local")),
        driver,
        "the driver in force changed"
    );
    expect_ok(env.revoke(DRIVER, name("local"), driver));
    assert!(
        env.is_revoked(DRIVER, name("local"), driver),
        "the pinned driver is not revoked"
    );
}

#[test]
fn a_pinned_bridge_proposed_again_is_revoked_at_once_while_proposed_again() {
    let mut env = Env::new();
    let bridge = env.bridge;
    expect_ok(env.propose(BRIDGE, name("local-bridge"), bridge));
    expect_ok(env.revoke(BRIDGE, name("local-bridge"), bridge));
    assert!(env.is_revoked(BRIDGE, name("local-bridge"), bridge));
}

/// Proposing a revoked driver again is a fresh approval, and waits a full
/// timelock like any other: the revocation holds until the new proposal is
/// ready.
#[test]
fn a_revoked_driver_proposed_again_waits_its_timelock_before_trading() {
    let mut env = Env::new();
    let v1 = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), v1));
    env.skip(TIMELOCK);
    assert_eq!(env.trading_driver(name("local")), v1);
    expect_ok(env.revoke(DRIVER, name("local"), v1));
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());

    expect_ok(env.propose(DRIVER, name("local"), v1));
    assert_eq!(
        env.trading_driver(name("local")),
        Pubkey::default(),
        "a revoked driver trades again with no timelock"
    );
    env.skip(TIMELOCK - 1);
    assert_eq!(
        env.trading_driver(name("local")),
        Pubkey::default(),
        "a revoked driver trades before its timelock"
    );
    env.skip(1);
    assert_eq!(env.trading_driver(name("local")), v1);
}

/// The same when a waiting proposal was withdrawn first: v1 in force and v2
/// waiting, v2 withdrawn, v1 revoked, then v1 put back.
#[test]
fn the_same_after_a_revocation_that_withdrew_a_waiting_proposal() {
    let mut env = Env::new();
    let (v1, v2) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, name("local"), v1));
    env.skip(TIMELOCK);
    expect_ok(env.propose(DRIVER, name("local"), v2));
    expect_ok(env.withdraw(DRIVER, name("local")));
    expect_ok(env.revoke(DRIVER, name("local"), v1));
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
    expect_ok(env.propose(DRIVER, name("local"), v1));
    assert_eq!(
        env.trading_driver(name("local")),
        Pubkey::default(),
        "v1 is back before its timelock"
    );
}

#[test]
fn a_revoked_bridge_proposed_again_waits_its_timelock() {
    let mut env = Env::new();
    let bridge = env.final_program();
    expect_ok(env.propose(BRIDGE, name("local-bridge"), bridge));
    env.skip(TIMELOCK);
    assert_eq!(env.bridge_of(name("local-bridge")), bridge);
    expect_ok(env.revoke(BRIDGE, name("local-bridge"), bridge));
    assert_eq!(env.bridge_of(name("local-bridge")), Pubkey::default());
    expect_ok(env.propose(BRIDGE, name("local-bridge"), bridge));
    assert_eq!(
        env.bridge_of(name("local-bridge")),
        Pubkey::default(),
        "a revoked bridge is back with no timelock"
    );
    env.skip(TIMELOCK);
    assert_eq!(env.bridge_of(name("local-bridge")), bridge);
}

/// An implementation proposed again and ready, but not yet settled, is
/// approved: a station created then pins it. A later revocation of the old
/// implementation settles the slot and leaves it approved; only revoking that
/// exact implementation stops it.
#[test]
fn a_ready_unsettled_reproposal_is_approved_and_survives_the_slot_settling() {
    let mut env = Env::new();
    let a = env.driver;
    let b = env.final_program();
    expect_ok(env.propose(DRIVER, name("local"), b));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(DRIVER, name("local"), b)); // settles b as current, then revokes it
    assert_eq!(env.trading_driver(name("local")), Pubkey::default());
    expect_ok(env.propose(DRIVER, name("local"), b)); // re-proposed
    env.skip(TIMELOCK);
    // Ready but unsettled: a new station pins b.
    assert_eq!(env.trading_driver(name("local")), b);
    assert!(!env.is_revoked(DRIVER, name("local"), b));

    // Revoking the long-gone `a` settles the slot (b current, its old
    // revocation cleared), and b keeps trading.
    expect_ok(env.revoke(DRIVER, name("local"), a));
    assert!(
        !env.is_revoked(DRIVER, name("local"), b),
        "settling the slot re-revoked b"
    );
    assert_eq!(env.trading_driver(name("local")), b);

    // Only revoking b itself stops it.
    expect_ok(env.revoke(DRIVER, name("local"), b));
    assert!(env.is_revoked(DRIVER, name("local"), b));
    assert!(env.is_revoked(DRIVER, name("local"), a));
}

// What may be proposed -------------------------------------------------------------

#[test]
fn the_zero_implementation_is_never_proposed_or_revoked() {
    let mut env = Env::new();
    expect_code(
        env.propose(DRIVER, test_driver(), Pubkey::default()),
        registry_error(RegistryError::NoImplementation),
    );
    expect_code(
        env.revoke(DRIVER, test_driver(), Pubkey::default()),
        registry_error(RegistryError::NoImplementation),
    );
}

/// A station pins a driver or a bridge for good, so it must be a program
/// nobody can ever change: one with a live upgrade authority is refused, one
/// whose authority was removed, or that was deployed final, is accepted.
#[test]
fn a_driver_or_bridge_must_be_a_program_nobody_can_change() {
    let mut env = Env::new();
    let admin = env.admin.pubkey();

    let upgradeable = env.upgradeable_program(Some(env.deployer.pubkey()));
    for kind in [DRIVER, BRIDGE] {
        let ix = env.ix_propose(
            &admin,
            kind,
            test_driver(),
            upgradeable,
            Some(program_data_address(&upgradeable)),
        );
        expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));
    }

    // Its program data left out, or another program's passed off as its own.
    let ix = env.ix_propose(&admin, DRIVER, test_driver(), upgradeable, None);
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));
    let finalized = env.upgradeable_program(None);
    let ix = env.ix_propose(
        &admin,
        DRIVER,
        test_driver(),
        upgradeable,
        Some(program_data_address(&finalized)),
    );
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // An account that isn't a program at all.
    let wallet = env.alice.pubkey();
    let ix = env.ix_propose(&admin, DRIVER, test_driver(), wallet, None);
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // Made final: accepted.
    let ix = env.ix_propose(
        &admin,
        DRIVER,
        test_driver(),
        finalized,
        Some(program_data_address(&finalized)),
    );
    expect_ok(env.as_admin(ix));
    let deployed_final = env.final_program();
    expect_ok(env.propose(BRIDGE, name("test-bridge"), deployed_final));
}

/// Settling a proposal clears the revocation it carried, so the record must
/// be the one for that implementation, and must be there to be cleared.
#[test]
fn settling_a_proposal_needs_its_own_revocation_record() {
    let mut env = Env::new();
    let (v1, v2) = (env.final_program(), env.final_program());
    expect_ok(env.propose(DRIVER, test_driver(), v1));
    env.skip(TIMELOCK);
    let admin = env.admin.pubkey();

    let mut ix = env.ix_propose(&admin, DRIVER, test_driver(), v2, None);
    // The record the slot settles, left out.
    let without = {
        let mut ix = ix.clone();
        ix.accounts[3].pubkey = shonin_registry::ID;
        ix.accounts[3].is_writable = false;
        ix
    };
    expect_code(
        env.as_admin(without),
        registry_error(RegistryError::MissingRevocationRecord),
    );
    // Another implementation's record in its place.
    ix.accounts[3].pubkey = revoked_address(DRIVER, &test_driver(), &v2);
    expect_code(
        env.as_admin(ix),
        registry_error(RegistryError::AccountSubstituted),
    );
    // As sent honestly.
    expect_ok(env.propose(DRIVER, test_driver(), v2));
}
