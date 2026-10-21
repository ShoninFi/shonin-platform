//! What the driver lists, fixed at build time for one network, as its
//! address is: Orca's program and the pools' settings account, the pools a
//! station may trade, the vault's dollar and the one other token those pools
//! pair it with (wrapped SOL), the Pyth account that prices that token, and
//! how old that price may be.
//!
//! A pool's address is its seeds' (the settings, both mints in order, its
//! tick spacing) under Orca's program, so each listed pool is the pool of
//! that pair at that spacing and nothing else; the tests check each against
//! its seeds. A pool is listed only if neither of its tokens is a Token-2022
//! token and its fee tier is a plain one (not an adaptive fee).
//!
//! The price is Pyth's sponsored SOL/USD account (shard 0), the same address
//! on every network, written only by Pyth's receiver program; Pyth updates
//! it about every minute on mainnet and about every five minutes on devnet,
//! so a price older than [`MAX_AGE`] is stale.

use anchor_lang::prelude::*;

/// Orca's Whirlpools program, at the same address on every network.
pub const WHIRLPOOL_PROGRAM: Pubkey = pubkey!("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
/// Pyth's receiver program, the owner of every price account it writes.
pub const PYTH_RECEIVER: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
/// Wrapped SOL: the token each listed pool pairs the dollar with.
pub const ASSET_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
pub const ASSET_DECIMALS: u32 = 9;
/// The dollar's decimals, the same on every network.
pub const DOLLAR_DECIMALS: u32 = 6;
/// Pyth's sponsored SOL/USD price account.
pub const PRICE_ACCOUNT: Pubkey = pubkey!("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE");
/// Pyth's SOL/USD feed.
pub const FEED_ID: [u8; 32] = [
    0xef, 0x0d, 0x8b, 0x6f, 0xda, 0x2c, 0xeb, 0xa4, 0x1d, 0xa1, 0x5d, 0x40, 0x95, 0xd1, 0xda, 0x39,
    0x2a, 0x0d, 0x2f, 0x8e, 0xd0, 0xc6, 0xc7, 0xbc, 0x0f, 0x4c, 0xfa, 0xc8, 0xc2, 0x80, 0xb5, 0x6d,
];

/// One listed pool: its address, the tick spacing its seeds name, and the
/// least change of liquidity a tick must make for a sale to cross it
/// exactly and count it against its crossings (see [`crate::sale::sell`]).
///
/// The threshold is fixed for the pool, never read from the pool's state,
/// which anyone can shape: a tick at it holds real capital at the price, not
/// a position's rent. A smaller tick is worked into a sale so that the sale
/// never fetches more than Orca's swap would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pool {
    pub address: Pubkey,
    pub tick_spacing: u16,
    pub theta: u128,
}

#[cfg(feature = "mainnet")]
mod listed {
    use super::*;
    /// Orca's mainnet settings.
    pub const CONFIG: Pubkey = pubkey!("2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ");
    /// USDC.
    pub const DOLLAR_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    /// SOL/USDC at tick spacing 4 (a 0.04% fee). Its threshold is the
    /// liquidity that holds about $1,000 across one spacing at the price: at
    /// SOL near $110 the square-root price is about 0.3325 (in the pool's
    /// units, lamports and USDC's millionths), a spacing moves it by a
    /// factor of 1.0002, so a spacing of liquidity L holds L × 0.3325 ×
    /// 0.0002 = L × 6.65e-5 millionths of a dollar, and $1,000 is L =
    /// 1.5e13. The pool's active liquidity was about 6.29e14 (2.4% of it).
    pub const POOLS: &[Pool] = &[Pool {
        address: pubkey!("Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE"),
        tick_spacing: 4,
        theta: 15_000_000_000_000,
    }];
    pub const MAX_AGE: u64 = 120;
    /// Orca's fee here is 0.04% and arbitrage keeps the pool within about
    /// that of the market; Pyth's price, up to [`MAX_AGE`] old, trails it
    /// by about a tenth of a percent in a minute or two of SOL's usual
    /// moves. A quarter of a percent leaves both room.
    pub const NEAR_BPS: u64 = 25;
}

#[cfg(all(feature = "devnet", not(feature = "mainnet")))]
mod listed {
    use super::*;
    /// Orca's devnet settings.
    pub const CONFIG: Pubkey = pubkey!("FcrweFY1G9HJAHG5inkGB6pKg1HZ6x9UC2WioAfWrGkR");
    /// Circle's USDC on devnet, as on every public network: no dollar of
    /// the platform's own.
    pub const DOLLAR_MINT: Pubkey = pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
    /// Wrapped SOL against Circle's devnet USDC at tick spacing 64, kept at
    /// Pyth's price by the testnet's market account, which seeds it with
    /// what a faucet gives. Its threshold is a twentieth of that seed: 0.7
    /// SOL across ±5% of the price is liquidity of about 0.7e9 × 0.324 /
    /// 0.0247 = 9.2e9 at SOL near $105 (square-root price 0.324; ±5% is a
    /// factor of 1.0247 in it), so 4.6e8, which holds about $0.5 across one
    /// spacing (a factor of 1.0032 in the square-root price).
    pub const POOLS: &[Pool] = &[Pool {
        address: pubkey!("EQ5KAPVdHvSP5aBqYos4vtEsZApx43iM9AnibexL4WYA"),
        tick_spacing: 64,
        theta: 460_000_000,
    }];
    pub const MAX_AGE: u64 = 900;
    /// The testnet's market account brings the pool back to Pyth's price
    /// once it strays a quarter of a percent, and Pyth's price here may be
    /// a quarter of an hour old: half a percent leaves both room.
    pub const NEAR_BPS: u64 = 50;
}

#[cfg(not(any(feature = "devnet", feature = "mainnet")))]
mod listed {
    use super::*;
    /// A local cluster and the tests copy devnet's settings account.
    pub const CONFIG: Pubkey = pubkey!("FcrweFY1G9HJAHG5inkGB6pKg1HZ6x9UC2WioAfWrGkR");
    /// The local dollar: the development mnemonic's mint.
    pub const DOLLAR_MINT: Pubkey = pubkey!("CqMbRgMuEhQi9BUS8xP44Wk5nENm48FqJnfjEi4eNb1k");
    /// The tests' pool, its active liquidity about 9.5e11 from one wide
    /// position: a hundredth of it, about $12 across one spacing at $150.
    pub const POOLS: &[Pool] = &[Pool {
        address: pubkey!("3jTN5MsREtjxh4gPdSuysPJY1o6F2w4quvmTehe9jXuS"),
        tick_spacing: 64,
        theta: 10_000_000_000,
    }];
    pub const MAX_AGE: u64 = 900;
    /// The tests' pool is thin: a station's own sale there moves it by
    /// about a percent. A percent, the tests' usual band and a tenth of the
    /// widest a station may have.
    pub const NEAR_BPS: u64 = 100;
}

/// The pools' settings account.
pub const CONFIG: Pubkey = listed::CONFIG;
/// The vault's dollar.
pub const DOLLAR_MINT: Pubkey = listed::DOLLAR_MINT;
/// The pools a station may trade, the first being the one a close sells in.
pub const POOLS: &[Pool] = listed::POOLS;
/// How old, in seconds, a price may be and still count.
pub const MAX_AGE: u64 = listed::MAX_AGE;
/// How far, in basis points of price, the selling pool's price (and that of
/// any pool where the station holds liquidity) may stand from Pyth's for
/// anything to be valued or a claim closed: far tighter than a station's
/// band, which is room for the station's own sale, so that a pool pushed in
/// the transaction that values or closes moves neither by more than this.
/// A band narrower than this bounds the pool by the band instead.
pub const NEAR_BPS: u64 = listed::NEAR_BPS;

/// The two mints in the order a pool keeps them: token A sorts first.
pub fn mints() -> (Pubkey, Pubkey) {
    if ASSET_MINT.to_bytes() < DOLLAR_MINT.to_bytes() {
        (ASSET_MINT, DOLLAR_MINT)
    } else {
        (DOLLAR_MINT, ASSET_MINT)
    }
}

/// Whether the token a pool keeps as A is the asset (and so B the dollar).
pub fn asset_is_a() -> bool {
    mints().0 == ASSET_MINT
}

/// Where a pool of the listed pair at `tick_spacing` is.
pub fn pool_address(tick_spacing: u16) -> Pubkey {
    let (a, b) = mints();
    Pubkey::find_program_address(
        &[
            b"whirlpool",
            CONFIG.as_ref(),
            a.as_ref(),
            b.as_ref(),
            &tick_spacing.to_le_bytes(),
        ],
        &WHIRLPOOL_PROGRAM,
    )
    .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_pool_is_the_pool_of_its_seeds() {
        assert!(!POOLS.is_empty());
        for pool in POOLS {
            assert_eq!(pool.address, pool_address(pool.tick_spacing));
        }
    }

    #[test]
    fn the_price_account_is_pyth_s_sponsored_address_for_the_feed() {
        let push_oracle = pubkey!("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");
        let shard = 0u16.to_le_bytes();
        let derived = Pubkey::find_program_address(&[&shard, &FEED_ID], &push_oracle).0;
        assert_eq!(derived, PRICE_ACCOUNT);
    }

    #[test]
    fn the_pool_may_stand_from_pyth_s_price_by_a_few_tenths_of_a_percent_at_most() {
        #[cfg(any(feature = "devnet", feature = "mainnet"))]
        assert!((10..=50).contains(&NEAR_BPS));
        assert!(NEAR_BPS <= shonin_station::state::MAX_EXIT_BAND_BPS as u64 / 10);
    }

    #[test]
    fn a_price_may_be_older_than_a_heartbeat_and_no_older_than_three() {
        // Pyth's sponsored feeds beat about every minute on mainnet and every
        // five on devnet.
        #[cfg(feature = "mainnet")]
        assert!((60..=180).contains(&MAX_AGE));
        #[cfg(not(feature = "mainnet"))]
        assert!((300..=900).contains(&MAX_AGE));
    }
}
