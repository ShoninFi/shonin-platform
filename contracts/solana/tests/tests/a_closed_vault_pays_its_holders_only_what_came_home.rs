//! A killed vault with its only depositor's money at work, wound up as the
//! keeper winds it up and paid out through the closed vault's door, every
//! dollar-unit accounted for: what was deposited, what the station sent home
//! and the vault booked, the fees and who took them, the dead shares' part,
//! the author's tank, and the venue's reserves. What the development chain's
//! seed does to its stopped vault: one depositor of $25,000, half of it moved
//! to the station, deposited at the venue and four fifths of that spent on
//! spot at the ask; then the admin's kill, the station stopped and unwound,
//! the vault valued and closed, and the depositor paid.
//!
//! The depositor is paid exactly what came home for the pool, less the fees
//! and the dead shares' part: a flat market pays them less than they put in
//! (the spread, and the profit fee on the deposit's transaction fee, which
//! stays in the pool); a rise pays them more by what the station brought home
//! less the fee on it; nothing comes out of the tank, the venue's reserves
//! beyond the trade's own gain, or another vault.

use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The seed's deposit: $25,000.
const DEPOSIT: u64 = 25_000_000_000;
/// Half of it put to work.
const AT_WORK: u64 = DEPOSIT / 2;
/// The development chain's venue reserves.
const VENUE_RESERVES: u64 = 10_000_000_000_000;
/// The spread the development chain's venue quotes: a hundredth of a percent
/// either side of the mark.
const SPREAD_BPS: u64 = 2;
const YEAR: u64 = 365 * DAY;

#[derive(Clone, Copy, Debug)]
enum Market {
    Flat,
    /// The spot position gains this many dollar-units at the mark.
    Rise(i128),
    Fall(i128),
}

fn spot_market(env: &Env) -> shonin_local_venue::Market {
    env.load(&shonin_local_venue::market_address(&spot()))
        .expect("the spot market is listed")
}

fn ceil_scale(size: u128, price: u64) -> u128 {
    (size * price as u128).div_ceil(UNIT)
}

/// Pays `who`'s whole holding through the closed vault's door.
fn redeem_closed(env: &mut Env, v: &V, who: &Keypair) -> u64 {
    let shares = env.shares(v, &who.pubkey());
    if shares == 0 {
        return 0;
    }
    env.token_account(&who.pubkey());
    let before = env.usd_balance(&who.pubkey());
    let ix = env.ix_redeem_closed(v, &who.pubkey(), shares, &who.pubkey());
    expect_ok(env.send(vec![ix], &[who]));
    env.usd_balance(&who.pubkey()) - before
}

/// What one run came to, in dollar-units.
#[derive(Debug)]
struct Ledger {
    /// What the trade made: what the station sent home less what it was sent.
    pnl: i128,
    paid: u64,
    fees: u64,
    dead_part: u64,
}

fn run(market: Market) -> Ledger {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.venue_reserves(VENUE_RESERVES);
    env.set_spot(PRICE, SPREAD_BPS);

    // Another vault at the same venue, its money at work there too: none of
    // it may reach this vault's holders.
    let w = env.open(&[Declared::local()]);
    env.deposit_as(&w, &bob, 10_000_000_000);
    let ask = spot_market(&env).ask;
    env.trade(
        &w,
        5_000_000_000,
        vec![order(spot(), true, UNIT / 20, ask + ask / 100)],
    );
    let other = env.vault(&w);
    let other_account = env.margin_account(&w.home.authority(0)).unwrap();
    let other_sent = env.sent_home(&w.home);
    let other_dollars = env.token_balance(&w.home.vault_dollars);

    // The only depositor.
    let tank_at_start = env.vault(&v).tank.balance;
    let deposited_at = env.now();
    let shares = env.deposit_as(&v, &alice, DEPOSIT);
    assert!(shares > 0);
    let vault = env.vault(&v);
    assert_eq!(vault.cash, DEAD_ASSETS + DEPOSIT, "the deposit is all cash");
    let kept = (DEPOSIT as u128 * vault.fees.transaction_bps as u128).div_ceil(BPS as u128);

    // Half to the station, all of that to the venue, four fifths of it on spot
    // at the ask, as the seed's agent does.
    let venue_before = env.venue_vault_balance();
    let ask = spot_market(&env).ask;
    let limit = ask + ask / 100;
    let size = (AT_WORK as u128 * 8 / 10) * UNIT / limit as u128;
    env.trade(&v, AT_WORK, vec![order(spot(), true, size, limit)]);
    let authority = v.home.authority(0);
    assert_eq!(env.position(&authority, spot()), size as i128);
    let cost = ceil_scale(size, ask);
    assert_eq!(
        env.venue_cash(&authority),
        AT_WORK as i128 - cost as i128,
        "the venue charged the ask and nothing else"
    );
    assert_eq!(env.venue_vault_balance(), venue_before + AT_WORK);
    assert_eq!(env.vault(&v).cash, DEAD_ASSETS + DEPOSIT - AT_WORK);
    assert_eq!(env.vault(&v).sent_total, AT_WORK as u128);

    // Killed; the market does what it does; the keeper winds it up.
    expect_ok(env.kill(&v));
    match market {
        Market::Flat => {}
        Market::Rise(dollars) => env.gain(&v, dollars),
        Market::Fall(dollars) => env.gain(&v, -dollars),
    }
    let halted = env.vault(&v).halted_at;
    env.warp(halted + HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.settle_all(&v);

    let sent_before = env.sent_home(&v.home);
    let booked_before = env.vault(&v).station_booked;
    assert_eq!(
        sent_before, booked_before,
        "nothing came home before the unwind"
    );
    let cash_before = env.vault(&v).cash;
    let bid = spot_market(&env).bid;
    let proceeds = size * bid as u128 / UNIT;
    let payer = env.payer.insecure_clone();
    let unwind = env.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
    expect_ok(env.send_fitting(vec![unwind], &[&payer]));
    let sent = env.sent_home(&v.home) - sent_before;
    // Exactly the venue account's cash and the spot sold at the bid: no
    // funding on spot, nothing from the reserves beyond the sale.
    assert_eq!(
        sent,
        AT_WORK as u128 - cost + proceeds,
        "the station sent home something other than the trade's proceeds"
    );
    let pnl = sent as i128 - AT_WORK as i128;
    assert_eq!(
        env.venue_vault_balance() as i128,
        venue_before as i128 - pnl,
        "the venue's reserves moved by more than the trade"
    );
    assert_eq!(env.station(&v.home).idle, 0);
    let left = env.margin_account(&authority).unwrap();
    assert_eq!(
        (left.cash, left.positions.len()),
        (0, 0),
        "left at the venue"
    );

    // Booked once, when the vault next looked.
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    env.settle_all(&v);
    let vault = env.vault(&v);
    assert_eq!(vault.station_booked, env.sent_home(&v.home));
    assert_eq!(vault.cash as u128, cash_before as u128 + sent);
    let cash = vault.cash;
    assert_eq!(
        cash as i128,
        (DEAD_ASSETS + DEPOSIT) as i128 + pnl,
        "the pool's cash is the deposit, the dead assets and the trade"
    );
    assert_eq!(
        vault.tank.balance, tank_at_start,
        "the tank paid for something"
    );
    let tank_before_close = vault.tank.balance;
    assert_eq!(env.shares(&v, &env.author.pubkey()), 0);
    assert_eq!(env.shares(&v, &env.platform.pubkey()), 0);

    expect_ok(env.close_vault(&v));
    let vault = env.vault(&v);
    assert_eq!(vault.state, VaultState::Closed);
    assert_eq!(vault.dead_shares, 0);
    let dead_part = vault.tank.balance - tank_before_close;
    assert_eq!(
        cash - vault.cash,
        dead_part,
        "the cash fell by other than the dead shares' part"
    );
    assert!(
        dead_part <= 2 * DEAD_ASSETS,
        "the dead shares took {dead_part}, more than their part"
    );

    // Everyone paid through the door; a second unwind finds nothing more.
    let paid = redeem_closed(&mut env, &v, &alice);
    let (author, platform) = (env.author.insecure_clone(), env.platform.insecure_clone());
    let to_author = redeem_closed(&mut env, &v, &author);
    let to_platform = redeem_closed(&mut env, &v, &platform);
    let fees = to_author + to_platform;
    let carol = Keypair::new();
    env.svm.airdrop(&carol.pubkey(), 1_000_000_000).unwrap();
    let outcome = env
        .unwind_stopped(&v.home, &carol, &[0])
        .expect("a second unwind");
    assert_eq!(outcome.sent_home, 0, "the station sent home twice");

    let vault = env.vault(&v);
    assert_eq!(vault.total_supply, 0);
    let residual = vault.cash;
    assert!(
        residual <= 4,
        "{residual} left in a closed vault nobody owns"
    );
    assert_eq!(
        env.token_balance(&v.home.vault_dollars),
        vault.tank.balance + vault.cash + vault.escrow + vault.held_total,
        "the vault's dollars are other than its tank and its cash"
    );
    assert_eq!(vault.tank.balance, tank_at_start + dead_part);

    // Every unit of the pool's cash went to a holder, a fee or the dead
    // shares' part.
    assert_eq!(
        paid as u128 + fees as u128 + dead_part as u128 + residual as u128,
        cash as u128
    );
    // So the depositor is paid the deposit and the trade, less the fees, less
    // the dead shares' part beyond the dead assets they brought.
    assert_eq!(
        paid as i128,
        DEPOSIT as i128 + pnl + DEAD_ASSETS as i128
            - dead_part as i128
            - fees as i128
            - residual as i128
    );
    // The fees: the profit fee on what the pool made above the price the
    // deposit joined at (the trade, and the transaction fee the deposit left
    // in the pool), and the holding fee for the time it ran.
    let f = vault.fees;
    let made = (pnl + kept as i128).max(0) as u128;
    let profit_most = made * (f.author_bps + f.platform_bps) as u128 / BPS as u128;
    // The pool held the deposit from its request to the close.
    let elapsed = env.now() - deposited_at;
    let holding_most =
        cash as u128 * f.holding_bps as u128 * elapsed as u128 / (BPS as u128 * YEAR as u128);
    assert!(
        fees as u128 <= profit_most + holding_most + 10,
        "fees of {fees} past the profit fee {profit_most} and the holding fee {holding_most}"
    );
    if to_author != 0 {
        // The author's part of the profit fee is two thirds of it.
        approx_rel(
            to_author as u128 * (f.author_bps + f.platform_bps) as u128,
            (fees as u128).saturating_sub(holding_most) * f.author_bps as u128,
            50_000_000_000_000_000,
        );
    }

    // The other vault's money is where it was.
    let after = env.vault(&w);
    assert_eq!(after.cash, other.cash);
    assert_eq!(env.sent_home(&w.home), other_sent);
    assert_eq!(env.token_balance(&w.home.vault_dollars), other_dollars);
    let account = env.margin_account(&w.home.authority(0)).unwrap();
    assert_eq!(account.cash, other_account.cash);
    assert_eq!(account.positions, other_account.positions);

    println!(
        "{market:?}: deposit {DEPOSIT}, sent to the station {AT_WORK}, spot {size} bought for \
         {cost} at {ask} and sold for {proceeds} at {bid}; the station sent home {sent} (trade \
         {pnl:+}), the vault booked it once; the pool's cash at the close {cash} = deposit + \
         dead assets {DEAD_ASSETS} + trade; the deposit's transaction fee {kept} stayed in the \
         pool; fees {fees} (author {to_author}, platform {to_platform}); the dead shares' part \
         {dead_part} to the tank ({tank_at_start} -> {}); left {residual}; the depositor was paid \
         {paid} ({:+}); the venue's reserves moved {:+}",
        vault.tank.balance,
        paid as i128 - DEPOSIT as i128,
        -pnl,
    );
    Ledger {
        pnl,
        paid,
        fees,
        dead_part,
    }
}

#[test]
fn a_flat_market_pays_the_depositor_less_than_they_put_in() {
    let l = run(Market::Flat);
    // Bought at the ask, sold at the bid: the spread, and nothing else.
    assert!(l.pnl < 0);
    approx(
        (-l.pnl) as u128,
        (AT_WORK as u128 * 8 / 10) * SPREAD_BPS as u128 / BPS as u128,
        1_000_000,
    );
    assert!(
        l.paid <= DEPOSIT,
        "a flat market paid {} for {DEPOSIT}",
        l.paid
    );
    assert!(l.dead_part > 0);
}

#[test]
fn a_rise_pays_the_depositor_what_came_home_less_the_fee_on_it() {
    let flat = run(Market::Flat);
    let rise = run(Market::Rise(100_000_000));
    assert!(rise.pnl > 0);
    assert!(rise.paid > DEPOSIT);
    // What the rise added to the payout is what it added to what came home,
    // less the profit fee on that (fifteen percent), less the dead shares'
    // part of it (a few millionths) and the holding fee's: within a hundred
    // dollar-units on a hundred dollars.
    let gained = (rise.pnl - flat.pnl) as u128;
    let more = (rise.paid - flat.paid) as u128;
    approx(more, gained * 8_500 / 10_000, 100);
    approx(
        (rise.fees - flat.fees) as u128,
        gained * 1_500 / 10_000,
        100,
    );
    assert!(rise.dead_part >= flat.dead_part);
    assert!(rise.paid as i128 - (DEPOSIT as i128) < rise.pnl);
}

#[test]
fn a_fall_costs_the_depositor_what_the_station_lost() {
    let flat = run(Market::Flat);
    let fall = run(Market::Fall(100_000_000));
    assert!(fall.pnl < flat.pnl);
    assert!(fall.paid < flat.paid);
    // Below the price the deposit joined at, no profit fee is owed, so the
    // depositor carries the whole of the loss.
    assert!(fall.fees <= flat.fees);
    assert!(fall.paid < DEPOSIT);
}

/// The seed's stopped vault was paid $25,000.062006: six cents over. A flat
/// market pays $5.44 under (the spread, and the profit fee on the deposit's
/// transaction fee left in the pool); each dollar the trade makes adds 85
/// cents. A rise of about $39 in the price of a coin (a sixteenth of a
/// percent, a fraction of one hour's move on the development chain's tape)
/// is all six cents over takes.
#[test]
fn six_cents_over_is_a_sixteenth_of_a_percent_rise() {
    let l = run(Market::Rise(6_470_000));
    assert!(l.paid > DEPOSIT);
    approx(l.paid as u128, 25_000_062_006, 20_000);
}
