//! The stand-in venue: an account belongs to one master, and only the master,
//! signing, moves dollars in or out, and only from and to a token account the
//! master owns; orders are immediate-or-cancel against the bid and ask, a
//! refused order leaves its siblings alone, reduce-only never opens or flips,
//! spot can't be borrowed, a perp needs margin behind it and keeps needing
//! it (or, below it, a withdrawal keeps the margin ratio the account had
//! before its last order), a fill's dollars round against whoever placed the order (a buy costs
//! its amount rounded up, a sale brings it rounded down), a position is
//! valued at the mark for staying and the worse side for
//! leaving, funding moves from longs to shorts, only the operator moves the
//! market, and an account lists exactly the markets it holds now.
//!
//! The EVM venue's agent keys have no counterpart: on Solana nothing but the
//! master signs here, and a station's trading key reaches the venue only
//! through the station's guard (see the guard's tests).

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::{AccountDeserialize, AnchorDeserialize};
use shonin_local_venue::{Fill, Market, PriceKind, VenueError};
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The venue set up, and alice's account holding 100k.
fn venue() -> (Env, Keypair) {
    let mut env = Env::new();
    env.set_up_stations();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), FUNDS);
    let from = env.token_account(&alice.pubkey());
    let open = env.ix_venue_open(&alice.pubkey(), &alice.pubkey());
    let deposit = env.ix_venue_deposit(&alice.pubkey(), &from, FUNDS);
    expect_ok(env.send(vec![open, deposit], &[&alice]));
    (env, alice)
}

fn place(env: &mut Env, master: &Keypair, orders: Vec<OrderRequest>) -> Vec<Fill> {
    let ix = env.ix_venue_place(&master.pubkey(), orders);
    let meta = expect_ok(env.send(vec![ix], &[master]));
    Vec::<Fill>::try_from_slice(&meta.return_data.data).unwrap()
}

/// `price` with `spread_bps` around it: mark and oracle at the price.
fn set_price(env: &mut Env, market: [u8; 32], price: u64, spread_bps: u64) {
    let half = price * spread_bps / 2 / BPS;
    env.set_prices(market, price, price, price - half, price + half);
}

fn value(env: &Env, master: &Pubkey, kind: PriceKind) -> i128 {
    let account = env.margin_account(master).unwrap();
    let markets: Vec<Market> = account
        .positions
        .iter()
        .map(|p| {
            env.load(&shonin_local_venue::market_address(&p.market))
                .unwrap()
        })
        .collect();
    account.value_at(&markets, kind).unwrap()
}

#[test]
fn a_closed_market_drops_off_the_accounts_list() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(spot(), true, UNIT, PRICE * 2)]);
    place(&mut env, &alice, vec![order(perp(), false, UNIT, 0)]);
    assert_eq!(env.held_markets(&alice.pubkey()).len(), 2);
    place(&mut env, &alice, vec![reduce(spot(), false, UNIT, 0)]);
    let held = env.margin_account(&alice.pubkey()).unwrap().positions;
    assert_eq!(held.len(), 1, "a closed market is still listed");
    assert_eq!(held[0].market, perp());
}

#[test]
fn only_the_master_trades_the_account() {
    let (mut env, alice) = venue();
    let bob = env.bob.insecure_clone();
    // Bob signing for alice's account: the account named is Bob's own, which
    // doesn't exist; with alice named as master, alice hasn't signed.
    let ix = env.ix_venue_place(&bob.pubkey(), vec![order(spot(), true, UNIT, PRICE * 2)]);
    expect_failure(env.send(vec![ix], &[&bob]));
    let mut ix = env.ix_venue_place(&alice.pubkey(), vec![order(spot(), true, UNIT, PRICE * 2)]);
    ix.accounts[0].is_signer = false;
    expect_failure(env.send(vec![ix], &[&bob]));
    assert_eq!(env.position(&alice.pubkey(), spot()), 0);
}

#[test]
fn nobody_but_the_master_takes_the_money_and_only_to_its_own_account() {
    let (mut env, alice) = venue();
    let bob = env.bob.insecure_clone();
    let bobs = env.token_account(&bob.pubkey());
    // Alice can't send her cash to Bob's account...
    let ix = env.ix_venue_withdraw(&alice.pubkey(), &bobs, 1);
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintTokenOwner),
    );
    // ...and Bob can't take it at all.
    let mut ix = env.ix_venue_withdraw(&alice.pubkey(), &bobs, 1);
    ix.accounts[0].is_signer = false;
    expect_failure(env.send(vec![ix], &[&bob]));
    assert_eq!(env.usd_balance(&bob.pubkey()), 0);
}

#[test]
fn the_master_gets_its_money_back() {
    let (mut env, alice) = venue();
    let own = env.token_account(&alice.pubkey());
    let ix = env.ix_venue_withdraw(&alice.pubkey(), &own, FUNDS);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.usd_balance(&alice.pubkey()), FUNDS);
}

#[test]
fn an_order_outside_its_limit_does_not_fill() {
    let (mut env, alice) = venue();
    set_price(&mut env, spot(), PRICE, 20);
    let fills = place(&mut env, &alice, vec![order(spot(), true, UNIT, PRICE)]);
    assert!(!fills[0].accepted);
    assert_eq!(fills[0].reason, "no fill at that price");
    assert_eq!(env.position(&alice.pubkey(), spot()), 0);
}

#[test]
fn a_rejected_order_leaves_its_siblings_alone() {
    let (mut env, alice) = venue();
    set_price(&mut env, spot(), PRICE, 20);
    let fills = place(
        &mut env,
        &alice,
        vec![
            order(spot(), true, UNIT / 10, PRICE * 2),
            order(spot(), true, UNIT / 10, 1),
        ],
    );
    assert!(fills[0].accepted);
    assert!(!fills[1].accepted);
    assert_eq!(env.position(&alice.pubkey(), spot()), (UNIT / 10) as i128);
}

#[test]
fn an_order_for_a_market_never_listed_is_refused_and_its_siblings_fill() {
    let (mut env, alice) = venue();
    let fills = place(
        &mut env,
        &alice,
        vec![
            order(name("NOPE"), true, UNIT, PRICE),
            order(spot(), true, UNIT / 10, PRICE),
        ],
    );
    assert_eq!(fills[0].reason, "no such market");
    assert!(fills[1].accepted);
    let fills = place(&mut env, &alice, vec![order(spot(), true, 0, PRICE)]);
    assert_eq!(fills[0].reason, "zero size");
}

#[test]
fn reduce_only_never_flips_a_position() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    let fills = place(&mut env, &alice, vec![reduce(perp(), false, 4 * UNIT, 1)]);
    assert!(fills[0].accepted);
    assert_eq!(fills[0].size, UNIT);
    assert_eq!(env.position(&alice.pubkey(), perp()), 0);
}

#[test]
fn reduce_only_refuses_to_open() {
    let (mut env, alice) = venue();
    let fills = place(&mut env, &alice, vec![reduce(perp(), false, UNIT, 1)]);
    assert!(!fills[0].accepted);
    assert_eq!(fills[0].reason, "nothing to reduce");
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    let fills = place(
        &mut env,
        &alice,
        vec![reduce(perp(), true, UNIT, PRICE * 2)],
    );
    assert_eq!(fills[0].reason, "would not reduce");
}

#[test]
fn spot_cannot_be_borrowed_against() {
    let (mut env, alice) = venue();
    let fills = place(
        &mut env,
        &alice,
        vec![order(spot(), true, 2 * UNIT, PRICE * 2)],
    );
    assert!(!fills[0].accepted);
    assert_eq!(fills[0].reason, "spot cannot be borrowed against");
    let fills = place(&mut env, &alice, vec![order(spot(), false, UNIT, 1)]);
    assert!(!fills[0].accepted);
}

#[test]
fn a_perp_may_be_held_on_margin() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    assert_eq!(env.venue_cash(&alice.pubkey()), 40_000_000_000);
    assert_eq!(value(&env, &alice.pubkey(), PriceKind::Mark), FUNDS as i128);
}

#[test]
fn a_perp_needs_margin_behind_it() {
    let (mut env, alice) = venue();
    let fills = place(
        &mut env,
        &alice,
        vec![order(perp(), true, 100 * UNIT, PRICE * 2)],
    );
    assert!(!fills[0].accepted);
    assert_eq!(fills[0].reason, "not enough margin");
}

#[test]
fn margin_has_to_stay_behind_it() {
    let (mut env, alice) = venue();
    place(
        &mut env,
        &alice,
        vec![order(perp(), true, 16 * UNIT, PRICE * 2)],
    );
    let own = env.token_account(&alice.pubkey());
    let ix = env.ix_venue_withdraw(&alice.pubkey(), &own, 50_000_000_000);
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::WouldGoNegative),
    );
}

/// Below the initial margin, a withdrawal may still keep the account's margin
/// ratio where it stood before its last order (within a few units of
/// rounding), and no more: so taking out what closing part of every position
/// freed goes through wherever closing all of them would.
#[test]
fn below_the_initial_margin_a_withdrawal_may_keep_the_ratio_and_no_more() {
    let (mut env, alice) = venue();
    let own = env.token_account(&alice.pubkey());
    // 16 BTC short at 60k on 100k: 960k of notional, 96k of margin.
    place(&mut env, &alice, vec![order(perp(), false, 16 * UNIT, 0)]);
    // The price up half a percent: worth 95.2k, short of 96.48k of margin.
    let up = PRICE * 1_005 / 1_000;
    set_price(&mut env, perp(), up, 0);
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Mark),
        95_200_000_000
    );
    let withdraw = |env: &mut Env, amount: u64| {
        let ix = env.ix_venue_withdraw(&alice.pubkey(), &own, amount);
        env.send(vec![ix], &[&alice])
    };
    // Nothing comes out while every position is open.
    expect_code(
        withdraw(&mut env, 1),
        venue_error(VenueError::WouldGoNegative),
    );
    // Half closed: what is left at the old ratio is 47.6k, so 47.6k comes out,
    // and the rounding allowed past it, and no more.
    place(
        &mut env,
        &alice,
        vec![reduce(perp(), true, 8 * UNIT, u64::MAX)],
    );
    let rounding = shonin_local_venue::RATIO_ROUNDING as u64;
    expect_code(
        withdraw(&mut env, 47_600_000_000 + rounding + 1),
        venue_error(VenueError::WouldGoNegative),
    );
    expect_ok(withdraw(&mut env, 47_600_000_000));
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Mark),
        47_600_000_000
    );
    expect_code(
        withdraw(&mut env, rounding + 1),
        venue_error(VenueError::WouldGoNegative),
    );
}

#[test]
fn a_short_gains_when_the_price_falls() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), false, UNIT, 1)]);
    set_price(&mut env, perp(), 50_000_000_000, 0);
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Mark),
        110_000_000_000
    );
}

/// A buy pays for every fraction of a dollar-unit it takes, so many buys
/// each worth just under a unit cost a unit each rather than nothing, and
/// leave the buyer worth no more than before; a sale is paid only the whole
/// units it brings.
#[test]
fn a_buy_rounds_its_cost_up_and_a_sale_its_proceeds_down() {
    let (mut env, alice) = venue();
    // Just under one dollar-unit's worth at 60,000 dollars.
    let size = (UNIT / PRICE as u128) - 1;
    let fills = place(&mut env, &alice, vec![order(spot(), true, size, PRICE); 8]);
    assert!(fills.iter().all(|fill| fill.accepted && fill.notional == 1));
    assert_eq!(env.venue_cash(&alice.pubkey()), (FUNDS - 8) as i128);
    let worth = value(&env, &alice.pubkey(), PriceKind::Mark);
    assert!(
        worth <= FUNDS as i128,
        "alice is worth {worth} after buying {} of BTC",
        8 * size
    );
    // Selling back what was bought brings home the whole units only.
    let fills = place(&mut env, &alice, vec![order(spot(), false, size, PRICE); 8]);
    assert!(fills.iter().all(|fill| fill.accepted && fill.notional == 0));
    assert_eq!(env.venue_cash(&alice.pubkey()), (FUNDS - 8) as i128);
}

#[test]
fn leaving_is_valued_at_the_worse_side() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    set_price(&mut env, perp(), PRICE, 100);
    assert_eq!(value(&env, &alice.pubkey(), PriceKind::Mark), FUNDS as i128);
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Exit),
        FUNDS as i128 - 300_000_000
    );
}

#[test]
fn funding_moves_from_longs_to_shorts() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), false, UNIT, 1)]);
    let before = value(&env, &alice.pubkey(), PriceKind::Mark);
    env.accrue_funding(perp(), 60_000_000);
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Mark),
        before + 60_000_000
    );
    place(
        &mut env,
        &alice,
        vec![reduce(perp(), true, UNIT, PRICE * 2)],
    );
    assert_eq!(
        value(&env, &alice.pubkey(), PriceKind::Mark),
        before + 60_000_000
    );
    assert_eq!(env.position(&alice.pubkey(), perp()), 0);
}

#[test]
fn only_the_operator_moves_the_market() {
    let (mut env, alice) = venue();
    let mut ix = env.ix_set_prices(perp(), 1, 1, 1, 1);
    ix.accounts[0] = AccountMeta::new_readonly(alice.pubkey(), true);
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::NotOperator),
    );
    let mut ix = env.ix_list_market(name("ETH"), false, 1);
    ix.accounts[0] = AccountMeta::new(alice.pubkey(), true);
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::NotOperator),
    );
}

#[test]
fn every_market_an_account_holds_must_be_given() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    // A placement without the held perp market can't value the account.
    let mut ix = env.ix_venue_place(
        &alice.pubkey(),
        vec![order(spot(), true, UNIT / 10, PRICE * 2)],
    );
    let perp_market = shonin_local_venue::market_address(&perp());
    ix.accounts.retain(|meta| meta.pubkey != perp_market);
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::MissingMarket),
    );
    let own = env.token_account(&alice.pubkey());
    let mut ix = env.ix_venue_withdraw(&alice.pubkey(), &own, 1);
    ix.accounts.retain(|meta| meta.pubkey != perp_market);
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::MissingMarket),
    );
}

#[test]
fn a_look_alike_market_is_refused() {
    let (mut env, alice) = venue();
    place(&mut env, &alice, vec![order(perp(), true, UNIT, PRICE * 2)]);
    // The perp market's bytes, priced at a fortune, at another address.
    let real = env
        .svm
        .get_account(&shonin_local_venue::market_address(&perp()))
        .unwrap();
    let mut market = Market::try_deserialize(&mut &real.data[..]).unwrap();
    market.mark = PRICE * 100;
    let mut data = Vec::new();
    anchor_lang::AccountSerialize::try_serialize(&market, &mut data).unwrap();
    let fake = Pubkey::new_unique();
    env.svm
        .set_account(
            fake,
            Account {
                lamports: real.lamports,
                data,
                owner: shonin_local_venue::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let own = env.token_account(&alice.pubkey());
    let mut ix = env.ix_venue_withdraw(&alice.pubkey(), &own, 1);
    let real_key = shonin_local_venue::market_address(&perp());
    for meta in &mut ix.accounts {
        if meta.pubkey == real_key {
            meta.pubkey = fake;
        }
    }
    expect_code(
        env.send(vec![ix], &[&alice]),
        venue_error(VenueError::NoSuchMarket),
    );
}

#[test]
fn an_empty_account_closes_and_refunds_its_payer_and_a_full_one_does_not() {
    let (mut env, alice) = venue();
    let bob = env.bob.insecure_clone();
    let close = env.ix_venue_close(&alice.pubkey(), &alice.pubkey());
    expect_code(
        env.send(vec![close], &[&alice]),
        venue_error(VenueError::NotEmpty),
    );
    let own = env.token_account(&alice.pubkey());
    let ix = env.ix_venue_withdraw(&alice.pubkey(), &own, FUNDS);
    expect_ok(env.send(vec![ix], &[&alice]));
    // The refund goes to whoever paid, never to someone the closer names.
    let close = env.ix_venue_close(&alice.pubkey(), &bob.pubkey());
    expect_code(
        env.send(vec![close], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintHasOne),
    );
    let before = env.lamports(&alice.pubkey());
    let rent = env.lamports(&shonin_local_venue::account_address(&alice.pubkey()));
    let close = env.ix_venue_close(&alice.pubkey(), &alice.pubkey());
    let payer = env.payer.insecure_clone();
    expect_ok(env.send(vec![close], &[&payer, &alice]));
    assert_eq!(env.lamports(&alice.pubkey()), before + rent);
    assert!(env.margin_account(&alice.pubkey()).is_none());
    // Opened again, it starts empty: nothing of the old one comes back.
    let open = env.ix_venue_open(&alice.pubkey(), &alice.pubkey());
    expect_ok(env.send(vec![open], &[&alice]));
    let account = env.margin_account(&alice.pubkey()).unwrap();
    assert_eq!(account.cash, 0);
    assert!(account.positions.is_empty());
}

#[test]
fn an_account_opens_once_and_only_by_its_master() {
    let (mut env, alice) = venue();
    let open = env.ix_venue_open(&alice.pubkey(), &alice.pubkey());
    expect_failure(env.send(vec![open], &[&alice]));
    let bob = env.bob.insecure_clone();
    let mut open = env.ix_venue_open(&bob.pubkey(), &alice.pubkey());
    open.accounts[1].is_signer = false;
    expect_failure(env.send(vec![open], &[&bob]));
}

#[test]
fn the_venue_is_set_up_once_by_its_deployer() {
    let mut env = Env::new();
    env.set_up_stations();
    let deployer = env.deployer.insecure_clone();
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: shonin_local_venue::ID,
        accounts: anchor_lang::ToAccountMetas::to_account_metas(
            &shonin_local_venue::accounts::Initialize {
                deployer: deployer.pubkey(),
                venue: shonin_local_venue::venue_address(),
                vault: shonin_local_venue::vault_address(),
                mint: env.mint,
                program: shonin_local_venue::ID,
                program_data: program_data_address(&shonin_local_venue::ID),
                token_program: anchor_spl::token::ID,
                system_program: solana_sdk_ids::system_program::ID,
            },
            None,
        ),
        data: anchor_lang::InstructionData::data(&shonin_local_venue::instruction::Initialize {
            operator: deployer.pubkey(),
            margin_bps: 1,
        }),
    };
    expect_failure(env.send(vec![ix], &[&deployer]));
}
