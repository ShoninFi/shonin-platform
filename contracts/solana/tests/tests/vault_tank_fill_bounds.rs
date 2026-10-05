//! A tank fill is bounded by what it may take, whatever it asks: never more
//! than the author's shares and the cash, and never a figure too large to
//! count, so a fill priced at a pool worth next to nothing settles like any
//! other. And the author can't ask the tank kept at more than the vault's
//! ceiling, neither later nor when they activate it.

use anchor_lang::{AnchorDeserialize, InstructionData};
use litesvm::types::TransactionMetadata;
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_vault::events::TankFilled;
use shonin_vault::state::MAX_TANK_KEEP;
use solana_signer::Signer;

fn keep_at(env: &mut Env, v: &V, assets: u64) -> TxResult {
    let ix = env.ix_author(v, shonin_vault::instruction::SetTankKeep { assets }.data());
    env.as_author(ix)
}

/// A vault the author and alice hold, its tank to be kept at the most it may
/// be, and every dollar but `left` in a levered perp at the venue whose
/// market then halves (inside the band: the mark, the venue's price, the bid
/// and the ask all agree), so the station is worth nothing. The keeper's
/// fill is queued and priced; answers with the author's shares as priced.
fn a_fill_priced_at_a_wiped_out_pool(env: &mut Env, v: &V, left: u64) -> u128 {
    let author = env.author.insecure_clone();
    let alice = env.alice.insecure_clone();
    env.deposit_as(v, &author, 100_000_000_000);
    env.deposit_as(v, &alice, 1_000_000_000_000);
    let floor = env.ix_author(v, shonin_vault::instruction::SetIdleFloor { bps: 0 }.data());
    expect_ok(env.as_author(floor));
    env.skip(DAY + SLOT);
    expect_ok(keep_at(env, v, MAX_TANK_KEEP));

    let out = env.vault(v).cash - left;
    let size = out as u128 * 5 * UNIT / PRICE as u128;
    env.trade(v, out, vec![order(perp(), true, size, PRICE)]);
    let half = PRICE / 2;
    env.set_prices(perp(), half, half, half, half);
    expect_ok(env.keep_unsigned(v));
    assert!(env.vault(v).tank.fill_pending);
    env.to_next_slot();
    expect_ok(env.value_vault(v));
    let vault = env.vault(v);
    assert!(vault.owed(), "the fill was priced");
    assert_eq!(
        vault.latest.gross_exit,
        left as u128 + 1,
        "the station is worth nothing"
    );
    env.shares(v, &author.pubkey())
}

fn settle(env: &mut Env, v: &V) -> TransactionMetadata {
    let from = env.vault(v).request_cursor;
    let ix = env.ix_settle(v, from, SETTLE_BATCH);
    let payer = env.payer.insecure_clone();
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
    let vault = env.vault(v);
    assert!(!vault.owed());
    assert!(!vault.tank.fill_pending);
    meta
}

/// Nothing at home: the fill's dollars come to more shares than a vault can
/// count, and nothing pays for them. It fills nothing, and the settlement
/// goes through.
#[test]
fn a_fill_too_large_to_count_settles() {
    let (mut env, v) = Env::with_vault();
    let shares = a_fill_priced_at_a_wiped_out_pool(&mut env, &v, 0);
    let tank = env.vault(&v).tank.balance;
    let meta = settle(&mut env, &v);
    assert!(emitted::<TankFilled>(&meta).is_empty());
    assert_eq!(env.shares(&v, &env.author.pubkey()), shares);
    assert_eq!(env.vault(&v).tank.balance, tank);
}

/// A thousand dollars at home: the fill asks for far more shares than the
/// author holds, takes every one of theirs and no more, and is paid what
/// they own of the cash.
#[test]
fn a_fill_takes_at_most_the_authors_shares() {
    let (mut env, v) = Env::with_vault();
    let left = 1_000_000_000;
    let shares = a_fill_priced_at_a_wiped_out_pool(&mut env, &v, left);
    let tank = env.vault(&v).tank.balance;
    let meta = settle(&mut env, &v);
    let filled: Vec<TankFilled> = emitted(&meta);
    assert_eq!(filled.len(), 1);
    assert_eq!(filled[0].shares, shares);
    assert!(filled[0].assets > 0 && filled[0].assets <= left);
    assert_eq!(env.shares(&v, &env.author.pubkey()), 0);
    let vault = env.vault(&v);
    assert_eq!(vault.tank.balance, tank + filled[0].assets);
    assert_eq!(vault.cash, left - filled[0].assets);
}

#[test]
fn a_tank_kept_past_the_ceiling_is_refused() {
    let (mut env, v) = Env::with_vault();
    expect_code(
        keep_at(&mut env, &v, MAX_TANK_KEEP + 1),
        vault_error(VaultError::OutOfRange),
    );
    expect_code(
        keep_at(&mut env, &v, u64::MAX),
        vault_error(VaultError::OutOfRange),
    );
    expect_ok(keep_at(&mut env, &v, MAX_TANK_KEEP));
    assert_eq!(env.vault(&v).tank.keep_at, MAX_TANK_KEEP);
    expect_ok(keep_at(&mut env, &v, 0));
    assert_eq!(env.vault(&v).tank.keep_at, 0);
}

/// The author's activation of a fresh strategy through the local driver,
/// asking the tank kept at `keep`.
fn activate_keeping(env: &mut Env, keep: u64) -> (TxResult, V) {
    let drivers = [Declared::local()];
    let names: Vec<[u8; 32]> = drivers.iter().map(|d| d.name).collect();
    let (id, bundle) = env.ready_with(&names, vec![name("SANCTIONED"), name("US")]);
    let (_, statement) = env.restricted_for(id, &drivers);
    let terms = env.terms_hash();
    let author = env.author.insecure_clone();
    env.give(&author.pubkey(), MINIMUM_TANK);
    let mut ix = env.ix_activate(id, bundle, &drivers, MINIMUM_TANK, statement, terms, &[]);
    let mut args = shonin_vault::ActivateArgs::try_from_slice(&ix.data[8..]).unwrap();
    args.tank_keep = keep;
    ix.data = shonin_vault::instruction::Activate { args }.data();
    let result = env.send_fitting(vec![ix], &[&author]);
    let programs: Vec<Pubkey> = drivers.iter().map(|d| d.program).collect();
    (result, env.v_of(id, bundle, &programs))
}

#[test]
fn an_activation_keeping_the_tank_past_the_ceiling_is_refused() {
    let mut env = Env::with_vaults();
    for keep in [MAX_TANK_KEEP + 1, u64::MAX] {
        let (result, v) = activate_keeping(&mut env, keep);
        expect_code(result, vault_error(VaultError::OutOfRange));
        assert!(env.svm.get_account(&v.vault).is_none());
    }
    let (result, v) = activate_keeping(&mut env, MAX_TANK_KEEP);
    expect_ok(result);
    assert_eq!(env.vault(&v).tank.keep_at, MAX_TANK_KEEP);
}
