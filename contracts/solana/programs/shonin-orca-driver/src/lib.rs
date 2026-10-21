//! Orca Whirlpools' driver: values a station's liquidity positions at Pyth's
//! price and prepares their closes, on the interface every driver answers
//! (`shonin-driver`). It holds nothing and calls nothing; it reads Orca's,
//! Pyth's and the token program's accounts and answers.
//!
//! **What it lists** is fixed at build time ([`network`]): Orca's program,
//! the named pools of wrapped SOL against the vault's dollar, Pyth's SOL/USD
//! account and how old its price may be. A station trades nowhere else
//! through it.
//!
//! **Positions.** Orca keeps a position as an NFT its holder keeps, created
//! at the address of a mint that signs its creation. A station's authority
//! opens positions only at its own position mints, [`MAX_SLOTS`] slot(s) of
//! the station program's addresses (`shonin_driver::position_mint`), each a
//! Token-2022 NFT that Orca closes with its position, freeing its slot. So
//! every position the station can hold is at a slot this driver looks at,
//! and none can be left out of a valuation.
//!
//! **Its accounts**, after the station's authority at Orca (0) and that
//! authority's dollar account (1), in this order, each checked as said:
//!
//! | index | account | how it is checked |
//! | --- | --- | --- |
//! | 2 | Orca's program | its address |
//! | 3 | the token program | its address |
//! | 4 | Pyth's SOL/USD price | the listed address, owned by Pyth's receiver, a `PriceUpdateV2` of the SOL/USD feed |
//! | 5 | the authority's wrapped-SOL account | the authority's associated account for wrapped SOL; its balance counts (nothing there if it doesn't exist) |
//! | 6 + 3p, 7 + 3p, 8 + 3p | each listed pool `p`, in the listed order, then its vault for A and for B | the listed address, Orca's, a pool of the listed settings, mints and tick spacing on a plain fee tier; the vaults are the pool's |
//! | then 4 accounts | the first pool's oracle, then three consecutive tick arrays of that pool in the sale's direction, the first holding the pool's price or the one before it, together reaching the sale's limit | the oracle is handed to Orca's swap, which checks it; each array is Orca's, of that pool, at its start, or, not there yet, its address holding nothing |
//! | then [`MAX_SLOTS`] accounts | slot by slot: the slot's position, or, for a slot holding none, its mint's address | a position is Orca's, of the slot's mint and a listed pool; an empty slot's mint address holds nothing |
//! | then one a position | each position's token account, in slot order | under a token program, holding the slot's mint, the authority's, amount 1, no delegate |
//! | the rest | tick arrays, any order | Orca's, of a listed pool; each position with liquidity must find the arrays of both its ticks among them |
//!
//! Whoever asks writes the pools, their vaults, the wrapped-SOL account, the
//! sale's tick arrays and every position and tick array given, as a close
//! does.
//!
//! **Value.** `mark` counts each position's tokens at Pyth's price, never
//! the pool's (which can be pushed for a transaction), with its fees owed
//! and earned since (from the tick arrays' fee growth, so a deposit never
//! takes the stayers' earned fees) and the authority's wrapped SOL, all at
//! Pyth's price. `exit` is what closing everything now would fetch: each
//! position withdrawn at its pool's price (as a withdrawal pays), its fees,
//! and all the SOL sold in the first pool, worked out step by step as Orca's
//! swap works it out through everyone else's liquidity, no further than
//! half the band from Pyth's square-root price, which the given tick arrays
//! must reach. The pool's own price is where that sale starts, and anyone
//! can push it for a transaction: so nothing is valued or closed with the
//! selling pool, or a pool holding the station's liquidity, as far as
//! [`network::NEAR_BPS`] from Pyth's price (or the band, if narrower), and
//! a push in the transaction that values moves `exit` by no more than that;
//! the band is room for the sale's own way through the pool. Less a small
//! allowance (twice Orca's share of the pool's fee, and a millionth) for
//! what a leaver's sale leaks to Orca where the stayers' own liquidity takes
//! part of it. So a leaver's part never fetches less than its share of
//! `exit` (selling part of it, through more liquidity, does better than
//! selling all), and what it fetches and what the stayers keep add up to it:
//! the station pays the leaver no more than came home and the stayers keep
//! their part. `exit` may be above `mark` when a pool sits a little above
//! Pyth's price. `outside` when Pyth's price can't be read, is not fully
//! verified, is older than allowed or less certain than the band, when the
//! selling pool or a pool where the station has liquidity is that far from
//! Pyth's price, or when that sale can't take all the SOL before the band's
//! limit: nobody is paid at a value no sale inside the band reaches. A sale
//! crosses exactly only ticks changing the liquidity by at least the listed
//! pool's fixed threshold (real capital at the price, never read from the
//! pool's state), and no more than so many of them: a valuation's as many
//! as it affords ([`sale::VALUE_CROSSINGS`]), a close's as many as one close
//! can afford ([`sale::CLOSE_CROSSINGS`]); what is left unsold there is
//! valued at the band's limit, the worst price inside the band, where the
//! rest of the way is shown to hold the depth to take it, and is outside
//! otherwise. Smaller ticks, which anyone can place for a position's rent,
//! are paired so that the sale never fetches more than Orca's swap would
//! (see [`sale::sell`]). A claim's close whose own swap would cross more
//! initialized ticks of any size than one close affords
//! ([`sale::SWAP_CROSSINGS`]) is refused this try, the valuation standing,
//! and the vault may serve the claim in parts. A stopped station's unwind
//! sells up to a unit short of the tick past those, inside the band, its
//! valuation what that sale fetches, and leaves the rest to the next
//! unwind. With nothing of worth held at Orca (no position with liquidity
//! or fees, and wrapped SOL worth under a dollar at Pyth's last price),
//! the valuation is nothing, never outside, and a close leaves it. At the
//! full band, a stopped station's last resort, nothing is outside: the sale
//! goes a tick spacing short of the furthest three arrays from the pool's
//! price reach, or a unit short of the tick past its last crossing, and
//! what it can't take there counts for nothing until the next unwind sells
//! on from there.
//!
//! **Close.** For a fraction `f` of everything: each position's liquidity is
//! decreased by `floor(f * L)`, its minimums what a withdrawal pays anywhere
//! inside the band, its fees brought up to date and collected into the
//! authority's own accounts; then one swap in the first pool sells the SOL
//! the decreases freed and `f` of the rest of the authority's SOL (the fees
//! collected included), a lamport less for each instruction that paid it
//! (should Orca round a unit differently), never past half the band from
//! Pyth's square-root price. A push before a claim's close moves what the
//! leaver is paid by no more than [`network::NEAR_BPS`] too, as the close is
//! refused with the pool any further. A claim's close leaving anything is
//! refused, as outside, also when its sale, worked out through less
//! liquidity than Orca's swap will find, would take the pool that far: the
//! stayers' part is valued after it, and the vault may serve a part, the
//! pool coming back between. A close that would leave no position with
//! liquidity and under a dollar of wrapped SOL sells all of it instead, so
//! what remains is nothing of worth both before and after: the station keeps
//! the stayers' part of the proceeds, counted. At the full band the swap is
//! made even through arrays with nothing to sell into, walking the price on
//! for the next unwind. Every dollar it brings goes home with the
//! station's own sweep of the authority's dollar account, so a sweep here
//! prepares nothing. A pool that can't take the sale inside the band leaves
//! the close short, and the station refuses it whole.
//!
//! **Compute.** Measured with the vault's call around it (`orca_driver.rs`),
//! with Orca's fixed tick arrays and its dynamic ones, the sale crossing from
//! one array into the next: a claim's close of a full slot with its fees,
//! its swap crossing as many initialized ticks as one close may, at most
//! about 379,000 units on a pool with a tick initialized at every spacing on
//! the way, and 382,000 with every tick of the sale's three arrays
//! initialized; a stopped station's unwind at most about 273,000; a
//! valuation at a 10% band about 67,000, at the full band (every tick of the
//! three arrays initialized) about 242,000. All inside
//! `shonin_driver::COMPUTE_PER_DRIVER`. A valuation and a claim's close read
//! every initialized tick on the way to the band's limit, about 800 units
//! each: at spacing 64 a 10% band is some 17 of them; at spacing 4 a band of
//! a few percent is already more than one close affords. Each slot more
//! costs about 85,000, which is why there is one.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;
use shonin_driver::{
    Allowlist, Class, Close, Custody, Description, ExitPath, Meta, Own, Plan, Position, Prepared,
    Rule, Valuation, ValuationKind, BPS, MAX_MARKETS, MAX_RETURN_DATA, WAD,
};

pub mod math;
pub mod network;
pub mod orca;
pub mod pyth;
pub mod sale;
pub mod wide;

use network::{Pool, ASSET_MINT, POOLS, WHIRLPOOL_PROGRAM};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// The name `shonin.toml` writes for it.
pub const NAME: [u8; 32] = *b"orca\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// The most positions a station holds at Orca at once: the slots of its
/// position mints. Every valuation looks at each, and a close of all of them
/// with its sale must fit `shonin_driver::COMPUTE_PER_DRIVER` with room: one
/// does, two would not. A range moves through it in one transaction:
/// emptied, closed and opened again elsewhere.
pub const MAX_SLOTS: u8 = 1;

/// The classic token program, which holds both of the pools' tokens.
pub const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022, which holds the positions' NFTs.
pub const TOKEN_2022_PROGRAM: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const ASSOCIATED_TOKEN_PROGRAM: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// A dollar, in the dollar's units: wrapped SOL worth less than this at
/// Orca, with nothing else held there, is nothing held there.
pub const DOLLAR: u128 = 1_000_000;

/// The station's authority at the venue and its dollar account, which come
/// first.
pub const STATION: u8 = 0;
pub const STATION_DOLLARS: u8 = 1;

/// Where each account stands among the ones the driver is given, the
/// authority's two included.
pub mod layout {
    use super::*;

    pub const ORCA: usize = 2;
    pub const TOKEN_PROGRAM: usize = 3;
    pub const PRICE: usize = 4;
    pub const ASSET: usize = 5;
    pub const POOLS: usize = 6;

    /// The pool `p`, and its vaults.
    pub fn pool(p: usize) -> usize {
        POOLS + 3 * p
    }
    pub fn vault_a(p: usize) -> usize {
        pool(p) + 1
    }
    pub fn vault_b(p: usize) -> usize {
        pool(p) + 2
    }
    /// The first pool's oracle, then the sale's three tick arrays.
    pub fn oracle() -> usize {
        POOLS + 3 * super::POOLS.len()
    }
    pub fn sale_arrays() -> usize {
        oracle() + 1
    }
    /// The first slot.
    pub fn slots() -> usize {
        sale_arrays() + 3
    }
    /// The first position's token account.
    pub fn tokens() -> usize {
        slots() + MAX_SLOTS as usize
    }
}

/// Where each set of accounts the allowlist pins stands among its sets.
pub mod fixed {
    /// The listed pools.
    pub const POOLS: u8 = 0;
    /// Wrapped SOL and the dollar.
    pub const MINTS: u8 = 1;
}

#[program]
pub mod shonin_orca_driver {
    use super::*;

    /// Orca's program and the instructions a station may make there.
    pub fn allowlist(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::allowlist())
    }

    /// Orca's terms of use exclude anyone on a sanctions list, and name no
    /// country; the vault's baseline still applies.
    pub fn restricted(_ctx: Context<Empty>) -> Result<()> {
        answer(&vec![code(b"SANCTIONED")])
    }

    pub fn describe(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::description())
    }

    pub fn value<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(&ctx.accounts.given(), ctx.remaining_accounts)?;
        answer(&book.valuation(band_bps)?)
    }

    pub fn close<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        fraction: u64,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(&ctx.accounts.given(), ctx.remaining_accounts)?;
        let (value, sale) = book.assess(band_bps, logic::Asked::Close)?;
        let plan = book.close(fraction, band_bps, value.outside, sale, false)?;
        answer(&Close { value, plan })
    }

    /// A stopped station's unwind of everything: as a close of it all, but
    /// a sale that would cross more ticks than one close affords sells up to
    /// where it stops, inside the band, and leaves the rest for the next
    /// unwind; the valuation answered with it is what that sale fetches.
    pub fn unwind<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(&ctx.accounts.given(), ctx.remaining_accounts)?;
        let (value, sale) = book.assess(band_bps, logic::Asked::Unwind)?;
        let plan = book.close(WAD, band_bps, value.outside, sale, true)?;
        answer(&Close { value, plan })
    }

    /// Nothing to prepare: a close sells what it frees itself, and every
    /// dollar it brings is swept home by the station.
    pub fn sweep<'info>(
        _ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        _amount: u64,
    ) -> Result<()> {
        answer(&Plan::default())
    }
}

/// A restricted code, zero-padded.
fn code(text: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..text.len()].copy_from_slice(text);
    out
}

/// Leaves `value` as the program's answer, which must fit the return data.
pub fn answer(value: &impl AnchorSerialize) -> Result<()> {
    let bytes = value.try_to_vec()?;
    require!(bytes.len() <= MAX_RETURN_DATA, DriverError::TooLarge);
    set_return_data(&bytes);
    Ok(())
}

#[derive(Accounts)]
pub struct Empty {}

#[derive(Accounts)]
pub struct Read<'info> {
    /// CHECK: the station's authority at Orca, the owner of everything read.
    pub authority: UncheckedAccount<'info>,
    /// CHECK: the authority's dollar account, checked to hold the dollar.
    pub authority_dollars: UncheckedAccount<'info>,
}

impl<'info> Read<'info> {
    fn given(&self) -> [AccountInfo<'info>; 2] {
        [
            self.authority.to_account_info(),
            self.authority_dollars.to_account_info(),
        ]
    }
}

/// The driver's reading, valuing and preparing, apart from its entrypoint.
pub mod logic {
    use super::*;
    use crate::math::{amounts_at, fees_earned};
    use crate::orca::{fee_growth_inside, TickArray, Whirlpool};
    use crate::pyth::Price;
    use crate::wide::W;

    /// One position the authority holds, at one of its slots.
    pub struct Held {
        pub slot: u8,
        /// Where its account, its token account and its pool stand among
        /// the accounts given, and the tick arrays of its two ticks (none
        /// for a position without liquidity, whose fees can't grow).
        pub position_at: u8,
        pub token_at: u8,
        pub pool: usize,
        pub lower_at: u8,
        pub upper_at: u8,
        pub position: orca::Position,
        /// The square-root prices of its two ticks.
        pub lower_sqrt: u128,
        pub upper_sqrt: u128,
        /// Its fees once brought up to date, and whether bringing them up to
        /// date changes anything.
        pub fees_a: u64,
        pub fees_b: u64,
        pub earning: bool,
    }

    /// Everything the driver read, each account checked to be the one it
    /// stands for.
    pub struct Book<'a> {
        /// Pyth's price, `None` when its account holds none the driver can
        /// read.
        pub price: Option<Price>,
        /// Pyth's price as the pool keeps prices, and its square root; zero
        /// when there is none.
        pub pyth: W,
        pub pyth_sqrt: u128,
        pub now: i64,
        pub pools: Vec<Whirlpool>,
        /// The authority's wrapped SOL.
        pub asset: u64,
        pub held: Vec<Held>,
        /// The selling pool's tick arrays a sale may cross, those that
        /// exist, and the starts of all three given, in the sale's
        /// direction.
        pub sale_arrays: Vec<TickArray<'a>>,
        pub sale_starts: [i32; 3],
        /// Whether the first of them holds the pool's price (not the one
        /// before it): the set reaching furthest.
        pub sale_from_price: bool,
    }

    /// Reads the station's holdings at Orca from `accounts` (the authority
    /// and its dollar account in `given`), laid out as [`layout`] says.
    pub fn read<'a, 'info>(
        given: &[AccountInfo<'info>; 2],
        rest: &'a [AccountInfo<'info>],
    ) -> Result<Book<'a>> {
        let at = |index: usize| -> Result<&'a AccountInfo<'info>> {
            rest.get(index - 2)
                .ok_or(error!(DriverError::WrongAccounts))
        };
        let authority = given[0].key;
        require!(
            rest.len() + 2 >= layout::tokens(),
            DriverError::WrongAccounts
        );
        require!(
            at(layout::ORCA)?.key == &WHIRLPOOL_PROGRAM
                && at(layout::TOKEN_PROGRAM)?.key == &TOKEN_PROGRAM,
            DriverError::WrongAccounts
        );
        // The authority's dollar account holds the listed dollar: a station
        // of another dollar is not one this driver can value.
        {
            let dollars = &given[1];
            require_keys_eq!(*dollars.owner, TOKEN_PROGRAM, DriverError::WrongAccounts);
            let data = dollars.try_borrow_data()?;
            require!(
                data.len() >= 72 && data[..32] == network::DOLLAR_MINT.to_bytes(),
                DriverError::WrongAccounts
            );
        }
        let price = Price::load(at(layout::PRICE)?)?;
        let asset = asset_balance(at(layout::ASSET)?, authority)?;

        let mut pools = Vec::with_capacity(POOLS.len());
        for (p, listed) in POOLS.iter().enumerate() {
            let info = at(layout::pool(p))?;
            require_keys_eq!(*info.key, listed.address, DriverError::WrongAccounts);
            let pool = Whirlpool::load(info)?;
            let (a, b) = network::mints();
            require!(
                pool.config == network::CONFIG
                    && pool.mint_a == a
                    && pool.mint_b == b
                    && pool.tick_spacing == listed.tick_spacing
                    && pool.fee_tier_index == listed.tick_spacing
                    && at(layout::vault_a(p))?.key == &pool.vault_a
                    && at(layout::vault_b(p))?.key == &pool.vault_b,
                DriverError::WrongAccounts
            );
            pools.push(pool);
        }

        // Every slot, in order: a position, or a mint address with nothing
        // there.
        let mut held = Vec::new();
        let mut token_at = layout::tokens();
        for slot in 0..MAX_SLOTS {
            let mint = shonin_driver::position_mint(&shonin_station::ID, authority, slot).0;
            let index = layout::slots() + slot as usize;
            let info = at(index)?;
            if info.key == &mint {
                require!(
                    info.owner == &anchor_lang::system_program::ID && info.data_is_empty(),
                    DriverError::WrongAccounts
                );
                continue;
            }
            let position = orca::Position::load(info)?;
            require_keys_eq!(position.mint, mint, DriverError::WrongAccounts);
            let pool = POOLS
                .iter()
                .position(|listed| listed.address == position.whirlpool)
                .ok_or(DriverError::WrongAccounts)?;
            // Its token: the authority's alone, nobody's delegate on it.
            let token = at(token_at)?;
            require!(
                token_holds(token, &mint, authority)?,
                DriverError::WrongAccounts
            );
            let lower_sqrt = math::sqrt_price_at(position.tick_lower).map_err(math_error)?;
            let upper_sqrt = math::sqrt_price_at(position.tick_upper).map_err(math_error)?;
            held.push(Held {
                slot,
                lower_sqrt,
                upper_sqrt,
                position_at: index as u8,
                token_at: token_at as u8,
                pool,
                lower_at: 0,
                upper_at: 0,
                fees_a: position.fee_owed_a,
                fees_b: position.fee_owed_b,
                earning: false,
                position,
            });
            token_at += 1;
        }

        // Tick arrays: each one Orca's, of a listed pool.
        let arrays = &rest[(token_at - 2).min(rest.len())..];
        for info in arrays {
            let array = TickArray::load(info)?.ok_or(DriverError::WrongAccounts)?;
            require!(
                POOLS.iter().any(|listed| listed.address == array.whirlpool),
                DriverError::WrongAccounts
            );
        }
        for h in held.iter_mut() {
            if h.position.liquidity == 0 {
                continue;
            }
            let pool = &pools[h.pool];
            let find = |tick: i32| -> Result<(usize, orca::Tick)> {
                let start = pool.array_start(tick);
                for (i, info) in arrays.iter().enumerate() {
                    let Some(array) = TickArray::load(info)? else {
                        continue;
                    };
                    if array.whirlpool == POOLS[h.pool].address && array.start == start {
                        return Ok((token_at + i, array.tick(tick, pool.tick_spacing)?));
                    }
                }
                err!(DriverError::WrongAccounts)
            };
            let (lower_at, lower) = find(h.position.tick_lower)?;
            let (upper_at, upper) = find(h.position.tick_upper)?;
            h.lower_at = lower_at as u8;
            h.upper_at = upper_at as u8;
            let (inside_a, inside_b) = fee_growth_inside(
                pool,
                &lower,
                h.position.tick_lower,
                &upper,
                h.position.tick_upper,
            );
            let p = &h.position;
            let earned_a = fees_earned(p.liquidity, inside_a, p.checkpoint_a);
            let earned_b = fees_earned(p.liquidity, inside_b, p.checkpoint_b);
            h.fees_a = p.fee_owed_a.wrapping_add(earned_a);
            h.fees_b = p.fee_owed_b.wrapping_add(earned_b);
            h.earning = earned_a != 0 || earned_b != 0;
        }
        // The sale's tick arrays: three consecutive arrays of the first pool
        // in the sale's direction, each at its own address (Orca's, or not
        // there yet and holding nothing), the pool's price in one of the
        // first two. Those a swap selling now would cross, or, after a
        // close's own sale has taken the price into the next array, the
        // same three the close was prepared with. Nobody valuing the
        // station can pass others and make a sale look poorer than it is.
        let first = &pools[0];
        let span = orca::TICKS_PER_ARRAY * first.tick_spacing as i32;
        let current = first.array_start(first.tick_current);
        let step = if network::asset_is_a() { -span } else { span };
        let infos = [
            at(layout::sale_arrays())?,
            at(layout::sale_arrays() + 1)?,
            at(layout::sale_arrays() + 2)?,
        ];
        let mut loaded: [Option<TickArray<'a>>; 3] = [None, None, None];
        for (slot, info) in loaded.iter_mut().zip(infos) {
            *slot = TickArray::load(info)?;
            if let Some(array) = slot {
                require_keys_eq!(
                    array.whirlpool,
                    POOLS[0].address,
                    DriverError::WrongAccounts
                );
            }
        }
        // The first array's start: the price's own array, or the one before
        // it in the sale's direction, when a close's own sale has moved the
        // price on.
        let mut sale_starts = None;
        let mut sale_from_price = false;
        for candidate in [current, current - step] {
            let starts = [candidate, candidate + step, candidate + 2 * step];
            let mut fits = true;
            for (i, info) in infos.iter().enumerate() {
                fits &= match &loaded[i] {
                    Some(array) => array.start == starts[i],
                    // Not there yet: then it must be at that array's
                    // address, holding nothing.
                    None => {
                        let (address, _) = Pubkey::find_program_address(
                            &[
                                b"tick_array",
                                POOLS[0].address.as_ref(),
                                starts[i].to_string().as_bytes(),
                            ],
                            &WHIRLPOOL_PROGRAM,
                        );
                        info.key == &address
                            && info.owner == &anchor_lang::system_program::ID
                            && info.data_is_empty()
                    }
                };
                if !fits {
                    break;
                }
            }
            if fits {
                sale_starts = Some(starts);
                sale_from_price = candidate == current;
                break;
            }
        }
        let sale_starts = sale_starts.ok_or(DriverError::WrongAccounts)?;
        let sale_arrays: Vec<TickArray<'a>> = loaded.into_iter().flatten().collect();
        let mut book = Book {
            price,
            sale_arrays,
            sale_starts,
            sale_from_price,
            pyth: W::ZERO,
            pyth_sqrt: 0,
            now: Clock::get()?.unix_timestamp,
            pools,
            asset,
            held,
        };
        // A price the driver can't read, or can't put as the pool keeps
        // prices, is no price.
        match book.pyth_raw() {
            Ok(raw) => book.pyth = raw,
            Err(_) => {
                book.price = None;
                return Ok(book);
            }
        }
        // Its root, found from the first pool's, which the band keeps
        // close to it whenever anything is valued.
        book.pyth_sqrt = match (book.pyth.to_u128(), u64::try_from(book.pools[0].sqrt_price)) {
            (Some(n), Ok(guess)) => {
                wide::sqrt_near(n, guess).clamp(math::MIN_SQRT_PRICE, math::MAX_SQRT_PRICE)
            }
            _ => Book::sqrt_at(book.pyth),
        };
        Ok(book)
    }

    /// The authority's wrapped SOL: its associated account's balance, or
    /// nothing if the account doesn't exist yet.
    fn asset_balance(info: &AccountInfo, authority: &Pubkey) -> Result<u64> {
        let (address, _) = Pubkey::find_program_address(
            &[
                authority.as_ref(),
                TOKEN_PROGRAM.as_ref(),
                ASSET_MINT.as_ref(),
            ],
            &ASSOCIATED_TOKEN_PROGRAM,
        );
        require_keys_eq!(*info.key, address, DriverError::WrongAccounts);
        if info.owner == &anchor_lang::system_program::ID && info.data_is_empty() {
            return Ok(0);
        }
        require_keys_eq!(*info.owner, TOKEN_PROGRAM, DriverError::WrongAccounts);
        let data = info.try_borrow_data()?;
        require!(
            data.len() >= 165
                && data[..32] == ASSET_MINT.to_bytes()
                && data[32..64] == authority.to_bytes(),
            DriverError::WrongAccounts
        );
        Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
    }

    /// Whether `info` is a token account holding the one token of `mint`,
    /// the authority's, with no delegate.
    fn token_holds(info: &AccountInfo, mint: &Pubkey, authority: &Pubkey) -> Result<bool> {
        if info.owner != &TOKEN_2022_PROGRAM && info.owner != &TOKEN_PROGRAM {
            return Ok(false);
        }
        let data = info.try_borrow_data()?;
        Ok(data.len() >= 165
            && data[..32] == mint.to_bytes()
            && data[32..64] == authority.to_bytes()
            && data[64..72] == 1u64.to_le_bytes()
            && data[72..76] == [0; 4])
    }

    /// A price as the pool keeps it: units of token B for one unit of token
    /// A, times 2^128 (the square of a square-root price). Whether the asset
    /// is A or B, every price here is kept this way, so the pool's own needs
    /// no conversion; the asset's price in dollars rises with it when the
    /// asset is A and falls with it when the asset is B.
    type Raw = W;

    impl Book<'_> {
        /// Pyth's price as a [`Raw`] price.
        pub fn pyth_raw(&self) -> Result<Raw> {
            let price = self.price.ok_or(DriverError::BadPrice)?;
            // Dollar-units for one unit of the asset are the price times
            // 10^exponent, shifted by the two tokens' decimals.
            let exponent =
                price.exponent + network::DOLLAR_DECIMALS as i32 - network::ASSET_DECIMALS as i32;
            let power = 10u64
                .checked_pow(exponent.unsigned_abs())
                .ok_or(DriverError::BadPrice)?;
            let price = price.price;
            let one = W([0, 0, 1, 0]);
            let raw = match (network::asset_is_a(), exponent >= 0) {
                (true, true) => W::from_u128(price as u128)
                    .mul_word(power)
                    .and_then(|p| p.shl_words(2)),
                (true, false) => W::from_u128(price as u128)
                    .shl_words(2)
                    .map(|p| p.div_word(power).0),
                // The asset is B: units of it for one dollar-unit.
                (false, true) => price.checked_mul(power).map(|p| one.div_word(p).0),
                (false, false) => one.mul_word(power).map(|p| p.div_word(price).0),
            };
            raw.filter(|raw| !raw.is_zero())
                .ok_or(error!(DriverError::BadPrice))
        }

        /// A pool's own price.
        pub fn pool_raw(&self, p: usize) -> Raw {
            let s = self.pools[p].sqrt_price;
            W::mul(s, s)
        }

        /// The square-root price of `raw`.
        fn sqrt_at(raw: Raw) -> u128 {
            raw.sqrt().clamp(math::MIN_SQRT_PRICE, math::MAX_SQRT_PRICE)
        }

        /// Pyth's square-root price scaled by `numerator / denominator`,
        /// rounded down.
        fn scaled_root(&self, numerator: u64, denominator: u64) -> u128 {
            let (q, _) = W::mul(self.pyth_sqrt, numerator as u128).div_word(denominator);
            q.to_u128()
                .unwrap_or(u128::MAX)
                .clamp(math::MIN_SQRT_PRICE, math::MAX_SQRT_PRICE)
        }

        /// What `amount` of the asset is worth in dollar-units at `raw`,
        /// rounded down.
        fn worth(amount: u128, raw: Raw) -> u128 {
            if network::asset_is_a() {
                raw.mul_shr128(amount)
            } else {
                let (q, _) = (math::U256::from(amount) << 128).div_mod(math::U256(raw.0));
                if q > math::U256::from(u128::MAX) {
                    u128::MAX
                } else {
                    q.low_u128()
                }
            }
        }

        /// The asset and the dollars every position holds at the square-root
        /// price `at(p)` of its pool `p`, with their fees and the authority's
        /// wrapped SOL.
        fn holdings(&self, at: impl Fn(usize) -> u128, withdrawing: bool) -> Result<(u128, u128)> {
            let mut asset = self.asset as u128;
            let mut dollars = 0u128;
            for h in &self.held {
                let p = &h.position;
                let roots = (h.lower_sqrt, h.upper_sqrt);
                let (a, b) = if withdrawing {
                    // What withdrawing it pays now, as the pool works it out.
                    let pool = &self.pools[h.pool];
                    math::withdrawal_between(
                        pool.tick_current,
                        at(h.pool),
                        (p.tick_lower, p.tick_upper),
                        roots,
                        p.liquidity,
                    )
                    .map_err(math_error)?
                } else {
                    amounts_at(at(h.pool), roots.0, roots.1, p.liquidity).map_err(math_error)?
                };
                let (a, b) = (a as u128 + h.fees_a as u128, b as u128 + h.fees_b as u128);
                let (to_asset, to_dollars) = if network::asset_is_a() {
                    (a, b)
                } else {
                    (b, a)
                };
                asset += to_asset;
                dollars += to_dollars;
            }
            Ok((asset, dollars))
        }

        /// How far, in basis points of price, a pool may stand from Pyth's
        /// for anything to be valued or closed inside `band_bps`: no further
        /// than [`network::NEAR_BPS`], nor than the band.
        fn near_bps(band_bps: u16) -> u64 {
            (band_bps as u64).min(network::NEAR_BPS)
        }

        /// The square-root prices `bps` of price either side of Pyth's, each
        /// rounded towards Pyth's: below, its root times `1 - t / 2` (a
        /// price `(1 - t / 2)^2`, above `1 - t`), and a unit more; above, its
        /// root times `1 + t / 2 - t^2 / 8` (a price `1 + t - t^3 / 8 + t^4 /
        /// 64`, below `1 + t`). A pool at or past either is too far from
        /// Pyth's to be valued or closed through.
        fn near_roots(&self, bps: u64) -> (u128, u128) {
            let whole = 8 * BPS * BPS;
            (
                self.scaled_root(2 * BPS - bps, 2 * BPS).saturating_add(1),
                self.scaled_root(whole + 4 * BPS * bps - bps * bps, whole),
            )
        }

        /// Whether the square-root price `root` is at or past either of
        /// [`Self::near_roots`] for `band_bps`.
        fn far(&self, root: u128, band_bps: u16) -> bool {
            let (low, high) = self.near_roots(Self::near_bps(band_bps));
            root <= low || root >= high
        }

        /// Whether the station holds anything of worth at Orca: a position
        /// with liquidity or fees, or wrapped SOL worth a dollar or more at
        /// Pyth's last price, fresh or not (with no price to read, any
        /// wrapped SOL at all). An emptied position and less than a dollar
        /// anyone could send are nothing to value or close: the station's
        /// valuation counts them at nothing and its close leaves them.
        fn holds_anything(&self) -> bool {
            self.held
                .iter()
                .any(|h| h.position.liquidity > 0 || h.fees_a != 0 || h.fees_b != 0)
                || match self.price {
                    Some(_) => Self::worth(self.asset as u128, self.pyth) >= DOLLAR,
                    None => self.asset > 0,
                }
        }

        /// Whether the prices put the holdings beyond the band: Pyth's
        /// price unreadable or unfit, or the selling pool or a pool holding
        /// liquidity priced at or past [`network::NEAR_BPS`] from it (or the
        /// band, if narrower). A sale is worked out from the pool's own
        /// price, which anyone can push for a transaction; held this near
        /// Pyth's, a push in the transaction that values or closes moves
        /// what the sale fetches by no more than that, while the band stays
        /// room for the sale's own way through the pool.
        fn prices_outside(&self, band_bps: u16) -> bool {
            let Some(price) = &self.price else {
                return true;
            };
            if !price.fit(self.now, network::MAX_AGE, band_bps) {
                return true;
            }
            (0..self.pools.len()).any(|p| {
                let holds = p == 0
                    || self
                        .held
                        .iter()
                        .any(|h| h.pool == p && h.position.liquidity > 0);
                holds && self.far(self.pools[p].sqrt_price, band_bps)
            })
        }

        /// The valuation. With nothing of worth held, nothing: `mark` and
        /// `exit` zero, never outside. Otherwise `mark` counts every token
        /// at Pyth's price, whatever the pools' own prices, and `exit` is
        /// what closing everything now would fetch: each position withdrawn
        /// at its pool's own price (as a withdrawal pays), and all the SOL
        /// sold in the first pool through everyone else's liquidity (see
        /// [`crate::sale`]), no further than the band allows. `outside` when
        /// the prices are (see [`Self::prices_outside`]) or when that sale
        /// can't take all the SOL inside the band: nobody is paid at a value
        /// no sale inside the band reaches. At the full band, a stopped
        /// station's last resort, nothing is outside, the sale goes as far
        /// as the given arrays reach, and what it can't take there counts
        /// for nothing. A pool pushed up, no further than
        /// [`network::NEAR_BPS`] from Pyth's price, may lift `exit` above
        /// `mark` by about that.
        pub fn valuation(&self, band_bps: u16) -> Result<Valuation> {
            Ok(self.assess(band_bps, Asked::Value)?.0)
        }

        /// The valuation, as `asked` for it, and how the sale it works out
        /// ends.
        pub fn assess(&self, band_bps: u16, asked: Asked) -> Result<(Valuation, SaleEnd)> {
            let nothing = |outside| {
                Ok((
                    Valuation {
                        mark: 0,
                        exit: 0,
                        outside,
                    },
                    SaleEnd::default(),
                ))
            };
            if !self.holds_anything() {
                return nothing(false);
            }
            let full = band_bps as u64 >= BPS;
            let mark = match self.price {
                Some(_) => {
                    let (asset, dollars) = self.holdings(|_| self.pyth_sqrt, false)?;
                    dollars.saturating_add(Self::worth(asset, self.pyth))
                }
                None if full => 0,
                None => return nothing(true),
            };
            // Prices outside the band answer that before any sale is worked
            // out, so no sale's reach can be asked of them; `exit` is then
            // nothing, as nobody is paid at it.
            if !full && self.prices_outside(band_bps) {
                return Ok((
                    Valuation {
                        mark: clamp(mark),
                        exit: 0,
                        outside: true,
                    },
                    SaleEnd::default(),
                ));
            }
            let crossings = match asked {
                Asked::Value => sale::VALUE_CROSSINGS,
                Asked::Close | Asked::Unwind => sale::CLOSE_CROSSINGS,
            };
            let (exit, sold_all, sale) =
                self.liquidation(band_bps, crossings, asked == Asked::Unwind)?;
            // A sale stopped short of a tick it may not cross sells up to
            // there in an unwind, where nobody is paid at the valuation;
            // anything else short of all of it is outside.
            let stopped = asked == Asked::Unwind && sale.stop.is_some();
            let outside = !full && !sold_all && !stopped;
            Ok((
                Valuation {
                    mark: clamp(mark),
                    exit: clamp(exit),
                    outside,
                },
                sale,
            ))
        }

        /// What selling everything now fetches, as [`Self::valuation`]'s
        /// `exit` says, whether the sale takes all the SOL, and how it ends.
        fn liquidation(
            &self,
            band_bps: u16,
            crossings: u32,
            unwind: bool,
        ) -> Result<(u128, bool, SaleEnd)> {
            let (asset, dollars) = self.holdings(|p| self.pools[p].sqrt_price, true)?;
            let asset = clamp(asset);
            let pool = &self.pools[0];
            let own = self.own();
            let band_limit = self.sale_limit(band_bps)?;
            // A valuation's sale, and a claim's, is worked out over every
            // tick on the way to the limit (the stayers' valuation after a
            // close then meets no tick the one before it didn't), and what
            // its crossings leave unsold is valued at the limit. An unwind's
            // swap crosses no more initialized ticks than one transaction
            // affords, however small: it reads no further than the one past
            // them, and goes no further than a unit short of it.
            let path = sale::path(
                pool,
                &self.sale_arrays,
                &self.sale_starts,
                network::asset_is_a(),
                if unwind {
                    sale::Reach::First(sale::SWAP_CROSSINGS + 1)
                } else {
                    sale::Reach::Limit(band_limit)
                },
            )?;
            let limit = if unwind {
                Self::swap_limit(&path, band_limit)?
            } else {
                band_limit
            };
            let sold = if asset == 0 {
                sale::Sale::default()
            } else {
                sale::sell(
                    pool,
                    &self.sale_starts,
                    &path,
                    POOLS[0].theta,
                    &own,
                    asset,
                    network::asset_is_a(),
                    limit,
                    crossings,
                    !unwind,
                )?
            };
            let worth = dollars.saturating_add(sold.proceeds as u128);
            // Less what a leaver's sale may leak out of the station that a
            // sale of everything wouldn't: where the stayers' own liquidity
            // takes part of the sale, Orca keeps its share of the fee the
            // stayers earn on it. That share of that fee, twice over, and a
            // millionth for the pool's rounding, comes off what everything
            // is worth, so that what a leaver is paid never leaves the
            // stayers short of theirs; it is the leaver's cost, as the rest
            // of their sale is.
            let haircut = 2 * pool.fee_rate as u128 * pool.protocol_fee_rate as u128 + 10_000;
            let off = W::mul(worth, haircut)
                .div_word(10_000_000_000)
                .0
                .to_u128()
                .unwrap_or(u128::MAX);
            Ok((
                worth.saturating_sub(off + 1),
                sold.sold >= asset,
                SaleEnd {
                    sells: sold.sold > 0,
                    // Where an unwind's swap stops; a claim's is decided on
                    // its own sale.
                    stop: match sold.stop {
                        _ if !unwind => None,
                        Some(stop) => Some(stop),
                        // Stopped by what Orca's swap may cross.
                        None if limit != band_limit && sold.end == limit && sold.sold < asset => {
                            Some(limit)
                        }
                        None => None,
                    },
                    path: Some(path),
                },
            ))
        }

        /// The station's own liquidity in the selling pool.
        fn own(&self) -> Vec<sale::Own> {
            self.held
                .iter()
                .filter(|h| h.pool == 0 && h.position.liquidity > 0)
                .map(|h| sale::Own {
                    liquidity: h.position.liquidity,
                    tick_lower: h.position.tick_lower,
                    tick_upper: h.position.tick_upper,
                })
                .collect()
        }

        /// `limit`, or a unit short of the initialized tick past the most a
        /// swap may cross if that comes first: Orca's swap crosses every
        /// initialized tick, however small, at its cost.
        fn swap_limit(path: &sale::Path, limit: u128) -> Result<u128> {
            let a_to_b = network::asset_is_a();
            let Some(&(past, _)) = path.ticks.get(sale::SWAP_CROSSINGS as usize) else {
                return Ok(limit);
            };
            let root = math::sqrt_price_at(past).map_err(math_error)?;
            Ok(if a_to_b {
                limit.max(root + 1)
            } else {
                limit.min(root - 1)
            })
        }

        /// The square-root price at the far edge of the given sale arrays in
        /// the sale's direction, or a tick spacing inside it.
        fn sale_reach(&self, inside: bool) -> Result<u128> {
            let pool = &self.pools[0];
            let spacing = pool.tick_spacing as i32;
            let span = orca::TICKS_PER_ARRAY * spacing;
            let in_by = if inside { spacing } else { 0 };
            let tick = if network::asset_is_a() {
                self.sale_starts.iter().min().unwrap() + in_by
            } else {
                self.sale_starts.iter().max().unwrap() + span - in_by
            };
            math::sqrt_price_at(tick.clamp(math::MIN_TICK, math::MAX_TICK)).map_err(math_error)
        }

        /// The square-root price the sale may go no further than: Pyth's
        /// price with the asset cheaper by the band. Selling A lowers the
        /// pool's price and selling B raises it; either way the limit is
        /// rounded towards the price it guards. The given arrays must reach
        /// it, so that nobody can value or close the station through a
        /// shorter reach. At the full band, where any price goes, the limit
        /// is a tick spacing inside the furthest the arrays can reach, which
        /// must then start at the pool's price: a sale stops there rather
        /// than run out of arrays, and the next goes on from where it
        /// stopped.
        fn sale_limit(&self, band_bps: u16) -> Result<u128> {
            let selling_a = network::asset_is_a();
            if band_bps as u64 >= BPS {
                require!(self.sale_from_price, DriverError::WrongAccounts);
                return self.sale_reach(true);
            }
            // The root of a price moved by `b` moves by about half as much,
            // and never by less: `sqrt(1 - b) <= 1 - b / 2` and
            // `1 + b / 2 <= 1 / sqrt(1 - b)`. So a limit half the band from
            // Pyth's root is never past the band, and keeps a sale a hair
            // inside it.
            let band = band_bps as u64;
            let limit = if selling_a {
                self.scaled_root(2 * BPS - band, 2 * BPS).saturating_add(1)
            } else {
                self.scaled_root(2 * BPS + band, 2 * BPS)
            };
            let edge = self.sale_reach(false)?;
            let reaches = if selling_a {
                edge <= limit
            } else {
                edge >= limit
            };
            require!(reaches, DriverError::WrongAccounts);
            Ok(limit)
        }

        /// The least of each token a withdrawal of `liquidity` from `h` may
        /// pay with its pool's price anywhere inside the band: A pays least
        /// at the band's highest square-root price, B at its lowest.
        fn minimums(h: &Held, liquidity: u128, (top, bottom): (u128, u128)) -> Result<(u64, u64)> {
            let (lower, upper) = (h.lower_sqrt, h.upper_sqrt);
            // A at the top: none above the range, all of it below.
            let min_a = if top >= upper {
                0
            } else {
                math::amount_a(top.max(lower), upper, liquidity, false).map_err(math_error)?
            };
            // B at the bottom: none below the range, all of it above.
            let min_b = if bottom <= lower {
                0
            } else {
                math::amount_b(lower, bottom.min(upper), liquidity, false).map_err(math_error)?
            };
            Ok((min_a, min_b))
        }

        /// The plan closing `fraction` of everything, as the module's
        /// documentation says, `outside` as the valuation says and `ending`
        /// how its sale of everything ends: when it sells nothing through the
        /// given arrays, no swap is prepared, except at the full band, where
        /// Orca's walks the price on; when it stops short of a tick it may
        /// not cross (only ever in a stopped station's unwind, or at the full
        /// band, as it is outside otherwise), the swap goes no further
        /// either.
        pub fn close(
            &self,
            fraction: u64,
            band_bps: u16,
            outside: bool,
            ending: SaleEnd,
            unwind: bool,
        ) -> Result<Plan> {
            require!(fraction <= WAD, DriverError::WrongAccounts);
            // Nothing of worth at Orca: nothing to close.
            if !self.holds_anything() {
                return Ok(Plan::default());
            }
            if outside {
                return Ok(Plan {
                    outside: true,
                    ..Plan::default()
                });
            }
            let meta = |index: usize, writable: bool| Meta {
                index: index as u8,
                writable,
            };
            let asset_is_a = network::asset_is_a();
            let (owner_a, owner_b) = if asset_is_a {
                (layout::ASSET, STATION_DOLLARS as usize)
            } else {
                (STATION_DOLLARS as usize, layout::ASSET)
            };
            let mut instructions = Vec::new();
            let mut freed: u128 = 0;
            let mut fees: u128 = 0;
            // Each instruction paying the asset may pay a unit less than
            // worked out here, should Orca round differently: the sale holds
            // that much back rather than fail for want of it.
            let mut margin: u64 = 0;
            let mut decreases: u8 = 0;
            // Whether any position keeps liquidity after the close.
            let mut keeps = false;
            // The band's highest and lowest square-root prices, which bound
            // what each withdrawal may pay; at the full band, anything.
            let band = band_bps as u64;
            let edges = if band >= BPS {
                None
            } else {
                // Pyth's root moved by the whole band either way: about
                // twice as far as the band's own roots (the root of a price
                // moved by `b` moves by about `b / 2`), so the minimums are
                // never more than a withdrawal inside the band pays. The
                // plan runs in the instruction that prepared it, so the
                // pool can't move between.
                Some((
                    self.scaled_root(BPS + band, BPS),
                    self.scaled_root(BPS - band, BPS),
                ))
            };
            for h in &self.held {
                let p = &h.position;
                let pool = &self.pools[h.pool];
                let liquidity = math::mul_div_u128(p.liquidity, fraction as u128, WAD as u128)
                    .ok_or(DriverError::Overflow)?;
                keeps |= p.liquidity > liquidity;
                if liquidity > 0 {
                    let (a, b) = math::withdrawal_between(
                        pool.tick_current,
                        pool.sqrt_price,
                        (p.tick_lower, p.tick_upper),
                        (h.lower_sqrt, h.upper_sqrt),
                        liquidity,
                    )
                    .map_err(math_error)?;
                    let (min_a, min_b) = match edges {
                        Some(edges) => Self::minimums(h, liquidity, edges)?,
                        None => (0, 0),
                    };
                    freed += if asset_is_a { a } else { b } as u128;
                    margin += 1;
                    decreases += 1;
                    instructions.push(Prepared {
                        program: layout::ORCA as u8,
                        accounts: vec![
                            meta(layout::pool(h.pool), true),
                            meta(layout::TOKEN_PROGRAM, false),
                            meta(STATION as usize, false),
                            meta(h.position_at as usize, true),
                            meta(h.token_at as usize, false),
                            meta(owner_a, true),
                            meta(owner_b, true),
                            meta(layout::vault_a(h.pool), true),
                            meta(layout::vault_b(h.pool), true),
                            meta(h.lower_at as usize, true),
                            meta(h.upper_at as usize, true),
                        ],
                        data: orca::decrease_liquidity(liquidity, min_a.min(a), min_b.min(b)),
                    });
                } else if p.liquidity > 0 && h.earning {
                    instructions.push(Prepared {
                        program: layout::ORCA as u8,
                        accounts: vec![
                            meta(layout::pool(h.pool), true),
                            meta(h.position_at as usize, true),
                            meta(h.lower_at as usize, false),
                            meta(h.upper_at as usize, false),
                        ],
                        data: orca::ix::UPDATE_FEES_AND_REWARDS.to_vec(),
                    });
                }
                if h.fees_a != 0 || h.fees_b != 0 {
                    let asset_fee = if asset_is_a { h.fees_a } else { h.fees_b };
                    fees += asset_fee as u128;
                    margin += u64::from(asset_fee != 0);
                    instructions.push(Prepared {
                        program: layout::ORCA as u8,
                        accounts: vec![
                            meta(layout::pool(h.pool), false),
                            meta(STATION as usize, false),
                            meta(h.position_at as usize, true),
                            meta(h.token_at as usize, false),
                            meta(owner_a, true),
                            meta(layout::vault_a(h.pool), true),
                            meta(owner_b, true),
                            meta(layout::vault_b(h.pool), true),
                            meta(layout::TOKEN_PROGRAM, false),
                        ],
                        data: orca::ix::COLLECT_FEES.to_vec(),
                    });
                }
            }
            // The leaver's SOL: all the decreases freed, and their fraction
            // of what the authority held and collected besides.
            let rest = math::mul_div_u128(self.asset as u128 + fees, fraction as u128, WAD as u128)
                .ok_or(DriverError::Overflow)?;
            // Every fee is collected, so a close that leaves no position
            // with liquidity and less than a dollar of wrapped SOL leaves
            // nothing of worth: it sells all the wrapped SOL, and the station
            // keeps the stayers' part of what it brings, counted, rather than
            // have them hold what no valuation counts.
            let all = self.asset as u128 + fees;
            let left = all - rest;
            let sell_all = !keeps && self.price.is_some() && Self::worth(left, self.pyth) < DOLLAR;
            let sale = clamp(freed + if sell_all { all } else { rest }).saturating_sub(margin);
            let full = band_bps as u64 >= BPS;
            let mut orders = decreases;
            // At the full band the swap is made even when the given arrays
            // hold nothing to sell into: Orca's own walks the price on
            // towards the limit, and the next unwind from there.
            if sale > 0 && (ending.sells || full) {
                let pool = &self.pools[0];
                let band_limit = self.sale_limit(band_bps)?;
                let path = match ending.path {
                    Some(path) => path,
                    None => sale::path(
                        pool,
                        &self.sale_arrays,
                        &self.sale_starts,
                        asset_is_a,
                        if unwind {
                            sale::Reach::First(sale::SWAP_CROSSINGS + 1)
                        } else {
                            sale::Reach::Limit(band_limit)
                        },
                    )?,
                };
                let limit = match ending.stop {
                    Some(stop) => stop,
                    None if unwind => Self::swap_limit(&path, band_limit)?,
                    None => {
                        // A claim's swap crosses no more initialized ticks
                        // than one close affords: if the leaver's own sale,
                        // worked out through less liquidity than Orca's
                        // swap will find, can't be made before that, this
                        // try is refused (the vault may serve a part).
                        let limit = Self::swap_limit(&path, band_limit)?;
                        let theirs = sale::sell(
                            pool,
                            &self.sale_starts,
                            &path,
                            POOLS[0].theta,
                            &self.own(),
                            sale,
                            asset_is_a,
                            limit,
                            sale::CLOSE_CROSSINGS,
                            false,
                        )?;
                        // Nor, leaving anything, may it take the pool
                        // further from Pyth's than a valuation may find it:
                        // the stayers' part is valued after it. A claim too
                        // large for that is refused this try (the vault may
                        // serve a part). Orca's swap, through more liquidity
                        // than worked out here, ends nearer than this sale.
                        let far = !full && !sell_all && self.far(theirs.end, band_bps);
                        if theirs.sold < sale || far {
                            return Ok(Plan {
                                outside: true,
                                ..Plan::default()
                            });
                        }
                        limit
                    }
                };
                // Inside the band the pool's price is short of the limit by
                // construction; a pool sitting on it can't sell at all.
                let room = if asset_is_a {
                    limit < pool.sqrt_price
                } else {
                    limit > pool.sqrt_price
                };
                if !room {
                    return Ok(Plan {
                        outside: true,
                        ..Plan::default()
                    });
                }
                let arrays = layout::sale_arrays();
                instructions.push(Prepared {
                    program: layout::ORCA as u8,
                    accounts: vec![
                        meta(layout::TOKEN_PROGRAM, false),
                        meta(STATION as usize, false),
                        meta(layout::pool(0), true),
                        meta(owner_a, true),
                        meta(layout::vault_a(0), true),
                        meta(owner_b, true),
                        meta(layout::vault_b(0), true),
                        meta(arrays, true),
                        meta(arrays + 1, true),
                        meta(arrays + 2, true),
                        meta(layout::oracle(), false),
                    ],
                    data: orca::swap(sale, 0, limit, asset_is_a),
                });
                orders += 1;
            }
            Ok(Plan {
                outside: false,
                instructions,
                orders: orders.min(MAX_MARKETS as u8),
            })
        }
    }

    /// What a valuation is asked for: to value (a valuation's sale may cross
    /// as many ticks as [`sale::VALUE_CROSSINGS`]), for a claim's close, or
    /// for a stopped station's unwind (as many as a close affords,
    /// [`sale::CLOSE_CROSSINGS`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Asked {
        Value,
        Close,
        Unwind,
    }

    /// How a sale of everything ends: whether it sells anything, and where
    /// it stops short of a tick it may not cross, if it does.
    #[derive(Clone, Debug, Default)]
    pub struct SaleEnd {
        pub sells: bool,
        pub stop: Option<u128>,
        /// The ticks on the way to the band's limit, and their threshold.
        pub path: Option<sale::Path>,
    }

    fn clamp(value: u128) -> u64 {
        value.min(u64::MAX as u128) as u64
    }

    fn math_error(_: math::MathError) -> Error {
        error!(DriverError::Overflow)
    }

    /// The instructions a station may make at Orca: positions opened only at
    /// the authority's position mints, in a listed pool, and closed only
    /// there; liquidity added and removed, fees and rewards collected and
    /// swaps made only in the listed pools, with the authority's own
    /// positions and into its own token accounts of the listed mints (Orca
    /// checks a destination only by its mint). Opening, adding and swapping
    /// put money at risk; removing, collecting and closing only bring it
    /// back, so a revoked driver still exits.
    pub fn allowlist() -> Allowlist {
        let own = |index: u8, own: Own| Position { index, own };
        let pool = || Own::Fixed(fixed::POOLS);
        let token = || Own::StationToken(Some(fixed::MINTS));
        let slots = Own::StationMint(MAX_SLOTS);
        let rule = |ix: [u8; 8], class: Class, positions: Vec<Position>| Rule {
            discriminator: ix.to_vec(),
            class,
            positions,
        };
        use orca::ix::*;
        Allowlist {
            venue: WHIRLPOOL_PROGRAM,
            fixed: vec![
                POOLS.iter().map(|listed: &Pool| listed.address).collect(),
                vec![network::ASSET_MINT, network::DOLLAR_MINT],
            ],
            rules: vec![
                // funder, owner, position, mint, its token account, pool,
                // Token-2022, system, associated token, metadata authority
                rule(
                    OPEN_POSITION_WITH_TOKEN_EXTENSIONS,
                    Class::Trade,
                    vec![
                        own(1, Own::Station),
                        own(3, slots.clone()),
                        own(4, Own::StationPositionToken(3)),
                        own(5, pool()),
                    ],
                ),
                // pool, token program, authority, position, its token
                // account, owner A, owner B, vaults, tick arrays
                rule(
                    INCREASE_LIQUIDITY,
                    Class::Trade,
                    vec![
                        own(0, pool()),
                        own(2, Own::Station),
                        own(4, Own::StationPositionHeld),
                        own(5, token()),
                        own(6, token()),
                    ],
                ),
                // token program, authority, pool, owner A, vault A, owner B,
                // vault B, tick arrays, oracle
                rule(
                    SWAP,
                    Class::Trade,
                    vec![
                        own(1, Own::Station),
                        own(2, pool()),
                        own(3, token()),
                        own(5, token()),
                    ],
                ),
                rule(
                    DECREASE_LIQUIDITY,
                    Class::Exit,
                    vec![
                        own(0, pool()),
                        own(2, Own::Station),
                        own(4, Own::StationPositionHeld),
                        own(5, token()),
                        own(6, token()),
                    ],
                ),
                // pool, position, tick arrays: anyone may, no signature
                rule(UPDATE_FEES_AND_REWARDS, Class::Exit, vec![own(0, pool())]),
                // pool, authority, position, its token account, owner A,
                // vault A, owner B, vault B, token program
                rule(
                    COLLECT_FEES,
                    Class::Exit,
                    vec![
                        own(0, pool()),
                        own(1, Own::Station),
                        own(3, Own::StationPositionHeld),
                        own(4, token()),
                        own(6, token()),
                    ],
                ),
                // pool, authority, position, its token account, the
                // authority's account of the reward's mint, reward vault,
                // token program: a reward of a mint nobody listed is still
                // the authority's, and its position can then be closed.
                rule(
                    COLLECT_REWARD,
                    Class::Exit,
                    vec![
                        own(0, pool()),
                        own(1, Own::Station),
                        own(3, Own::StationPositionHeld),
                        own(4, Own::StationToken(None)),
                    ],
                ),
                // authority, receiver (whoever paid the rent), position,
                // mint, its token account, Token-2022
                rule(
                    CLOSE_POSITION_WITH_TOKEN_EXTENSIONS,
                    Class::Exit,
                    vec![
                        own(0, Own::Station),
                        own(3, slots),
                        own(4, Own::StationPositionToken(3)),
                    ],
                ),
            ],
        }
    }

    pub fn description() -> Description {
        Description {
            name: NAME,
            custody: Custody::Station,
            exit_path: ExitPath::Contract,
            valuation: ValuationKind::OnChain,
        }
    }

    /// The square-root price of a price kept as the pool keeps it (B per
    /// A, times 2^128), for callers working out a pool's starting price.
    pub fn sqrt_price_for(raw: W) -> u128 {
        Book::sqrt_at(raw)
    }
}

#[error_code]
pub enum DriverError {
    #[msg("An account is not the account it stands for, or one is missing")]
    WrongAccounts,
    #[msg("The price account holds no usable price")]
    BadPrice,
    #[msg("The answer is larger than the return data holds")]
    TooLarge,
    #[msg("Arithmetic overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_fits_an_answer_and_a_station_s_limits() {
        let allowlist = logic::allowlist();
        assert!(allowlist.try_to_vec().unwrap().len() <= MAX_RETURN_DATA);
        assert!(allowlist.rules.len() <= shonin_driver::MAX_RULES);
        for rule in &allowlist.rules {
            assert!(rule.positions.len() <= shonin_driver::MAX_POSITIONS);
        }
        let pinned: usize = allowlist.fixed.iter().map(Vec::len).sum();
        assert!(pinned <= shonin_driver::MAX_FIXED);
        assert!(MAX_SLOTS as usize <= shonin_driver::MAX_POSITION_MINTS);
    }

    #[test]
    fn a_close_never_credits_more_orders_than_a_station_allows() {
        // One decrease a position and one sale.
        assert!((MAX_SLOTS as usize) < MAX_MARKETS);
    }

    #[test]
    fn the_restricted_list_names_sanctioned_persons_only() {
        assert_eq!(&code(b"SANCTIONED")[..10], b"SANCTIONED");
    }
}
