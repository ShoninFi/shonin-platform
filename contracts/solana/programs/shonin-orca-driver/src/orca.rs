//! Orca's accounts as the driver reads them, and the instructions it
//! prepares, laid out as Orca's program (version 0.9) lays them out.
//!
//! Each account is read only after checking it is Orca's (its owner) and of
//! the kind it stands for (its discriminator): only Orca's program writes an
//! account it owns, and it creates a pool, a position or a tick array only
//! at that account's own seeds, so an account of Orca's of the right kind
//! whose pool field names a listed pool is that pool's. Where a field is
//! read, its offset is the one Orca's program keeps it at.

use anchor_lang::prelude::*;

use crate::DriverError;

/// `sha256("account:<Name>")[..8]` of each account kind read here.
pub const WHIRLPOOL: [u8; 8] = [0x3f, 0x95, 0xd1, 0x0c, 0xe1, 0x80, 0x63, 0x09];
pub const POSITION: [u8; 8] = [0xaa, 0xbc, 0x8f, 0xe4, 0x7a, 0x40, 0xf7, 0xd0];
pub const TICK_ARRAY: [u8; 8] = [0x45, 0x61, 0xbd, 0xbe, 0x6e, 0x07, 0x42, 0xbb];
pub const DYNAMIC_TICK_ARRAY: [u8; 8] = [0x11, 0xd8, 0xf6, 0x8e, 0xe1, 0xc7, 0xda, 0x38];

/// `sha256("global:<name>")[..8]` of each instruction on the allowlist.
pub mod ix {
    pub const OPEN_POSITION_WITH_TOKEN_EXTENSIONS: [u8; 8] =
        [0xd4, 0x2f, 0x5f, 0x5c, 0x72, 0x66, 0x83, 0xfa];
    pub const INCREASE_LIQUIDITY: [u8; 8] = [0x2e, 0x9c, 0xf3, 0x76, 0x0d, 0xcd, 0xfb, 0xb2];
    pub const DECREASE_LIQUIDITY: [u8; 8] = [0xa0, 0x26, 0xd0, 0x6f, 0x68, 0x5b, 0x2c, 0x01];
    pub const UPDATE_FEES_AND_REWARDS: [u8; 8] = [0x9a, 0xe6, 0xfa, 0x0d, 0xec, 0xd1, 0x4b, 0xdf];
    pub const COLLECT_FEES: [u8; 8] = [0xa4, 0x98, 0xcf, 0x63, 0x1e, 0xba, 0x13, 0xb6];
    pub const COLLECT_REWARD: [u8; 8] = [0x46, 0x05, 0x84, 0x57, 0x56, 0xeb, 0xb1, 0x22];
    pub const CLOSE_POSITION_WITH_TOKEN_EXTENSIONS: [u8; 8] =
        [0x01, 0xb6, 0x87, 0x3b, 0x9b, 0x19, 0x63, 0xdf];
    pub const SWAP: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
}

/// Ticks in one tick array.
pub const TICKS_PER_ARRAY: i32 = 88;
/// Where a fixed tick array keeps its pool.
const FIXED_ARRAY_LEN: usize = 9_988;
const FIXED_TICK_LEN: usize = 113;
const DYNAMIC_HEADER: usize = 60;
const DYNAMIC_TICK_LEN: usize = 112;

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
fn i32_at(data: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
fn u128_at(data: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(data[at..at + 16].try_into().unwrap())
}
fn i128_at(data: &[u8], at: usize) -> i128 {
    i128::from_le_bytes(data[at..at + 16].try_into().unwrap())
}
fn key_at(data: &[u8], at: usize) -> Pubkey {
    Pubkey::new_from_array(data[at..at + 32].try_into().unwrap())
}

/// The data of an account Orca's program owns, of the kind `discriminator`
/// names, at least `len` bytes long.
fn orca_data<'a>(
    info: &'a AccountInfo,
    discriminator: &[u8; 8],
    len: usize,
) -> Result<std::cell::Ref<'a, &'a mut [u8]>> {
    require_keys_eq!(
        *info.owner,
        crate::network::WHIRLPOOL_PROGRAM,
        DriverError::WrongAccounts
    );
    let data = info.try_borrow_data()?;
    require!(
        data.len() >= len && data[..8] == discriminator[..],
        DriverError::WrongAccounts
    );
    Ok(data)
}

/// What the driver reads of a pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whirlpool {
    pub config: Pubkey,
    pub tick_spacing: u16,
    pub fee_tier_index: u16,
    /// Of each swap's input, in millionths.
    pub fee_rate: u16,
    /// Of each fee, in basis points, Orca's own share.
    pub protocol_fee_rate: u16,
    /// The liquidity active at the pool's price.
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub tick_current: i32,
    pub mint_a: Pubkey,
    pub vault_a: Pubkey,
    pub fee_growth_global_a: u128,
    pub mint_b: Pubkey,
    pub vault_b: Pubkey,
    pub fee_growth_global_b: u128,
}

impl Whirlpool {
    pub const LEN: usize = 653;

    pub fn load(info: &AccountInfo) -> Result<Self> {
        let data = orca_data(info, &WHIRLPOOL, Self::LEN)?;
        Ok(Self {
            config: key_at(&data, 8),
            tick_spacing: u16_at(&data, 41),
            fee_tier_index: u16_at(&data, 43),
            fee_rate: u16_at(&data, 45),
            protocol_fee_rate: u16_at(&data, 47),
            liquidity: u128_at(&data, 49),
            sqrt_price: u128_at(&data, 65),
            tick_current: i32_at(&data, 81),
            mint_a: key_at(&data, 101),
            vault_a: key_at(&data, 133),
            fee_growth_global_a: u128_at(&data, 165),
            mint_b: key_at(&data, 181),
            vault_b: key_at(&data, 213),
            fee_growth_global_b: u128_at(&data, 245),
        })
    }

    /// The first tick of the tick array holding `tick`.
    pub fn array_start(&self, tick: i32) -> i32 {
        let span = TICKS_PER_ARRAY * self.tick_spacing as i32;
        tick.div_euclid(span) * span
    }
}

/// What the driver reads of a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub whirlpool: Pubkey,
    pub mint: Pubkey,
    pub liquidity: u128,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub checkpoint_a: u128,
    pub fee_owed_a: u64,
    pub checkpoint_b: u128,
    pub fee_owed_b: u64,
}

impl Position {
    pub const LEN: usize = 216;

    pub fn load(info: &AccountInfo) -> Result<Self> {
        let data = orca_data(info, &POSITION, Self::LEN)?;
        Ok(Self {
            whirlpool: key_at(&data, 8),
            mint: key_at(&data, 40),
            liquidity: u128_at(&data, 72),
            tick_lower: i32_at(&data, 88),
            tick_upper: i32_at(&data, 92),
            checkpoint_a: u128_at(&data, 96),
            fee_owed_a: u64_at(&data, 112),
            checkpoint_b: u128_at(&data, 120),
            fee_owed_b: u64_at(&data, 136),
        })
    }
}

/// What a tick keeps that a valuation needs: whether it is initialized, and
/// the fee growth outside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub initialized: bool,
    /// The liquidity that starts at this tick going up (ends, going down).
    pub liquidity_net: i128,
    pub fee_growth_outside_a: u128,
    pub fee_growth_outside_b: u128,
}

/// A tick array of either kind: its pool, its first tick, and the ticks
/// read from it.
pub struct TickArray<'a> {
    pub whirlpool: Pubkey,
    pub start: i32,
    dynamic: bool,
    data: std::cell::Ref<'a, &'a mut [u8]>,
}

impl<'a> TickArray<'a> {
    /// Reads the array if `info` is one of Orca's tick arrays; `None` if it
    /// is anything else (an account that doesn't exist, say).
    pub fn load(info: &'a AccountInfo) -> Result<Option<Self>> {
        if info.owner != &crate::network::WHIRLPOOL_PROGRAM {
            return Ok(None);
        }
        let data = info.try_borrow_data()?;
        if data.len() >= FIXED_ARRAY_LEN && data[..8] == TICK_ARRAY {
            return Ok(Some(Self {
                whirlpool: key_at(&data, 9_956),
                start: i32_at(&data, 8),
                dynamic: false,
                data,
            }));
        }
        if data.len() >= DYNAMIC_HEADER + TICKS_PER_ARRAY as usize
            && data[..8] == DYNAMIC_TICK_ARRAY
        {
            return Ok(Some(Self {
                whirlpool: key_at(&data, 12),
                start: i32_at(&data, 8),
                dynamic: true,
                data,
            }));
        }
        Ok(None)
    }

    /// The offset of the first initialized tick from `offset` (counted in
    /// ticks of the array, 0 to 87) downwards, itself included, or upwards,
    /// itself excluded.
    pub fn next_initialized(&self, offset: i32, down: bool) -> Option<i32> {
        let data = &self.data;
        if self.dynamic {
            let bitmap = u128_at(data, 44);
            if down {
                if offset < 0 {
                    return None;
                }
                let below = bitmap & (u128::MAX >> (127 - offset.min(127) as u32));
                (below != 0).then(|| 127 - below.leading_zeros() as i32)
            } else {
                let from = offset + 1;
                if from >= TICKS_PER_ARRAY {
                    return None;
                }
                let above = bitmap & !((1u128 << from) - 1);
                (above != 0).then(|| above.trailing_zeros() as i32)
            }
        } else if down {
            let mut at = offset.min(TICKS_PER_ARRAY - 1);
            while at >= 0 {
                if data[12 + at as usize * FIXED_TICK_LEN] != 0 {
                    return Some(at);
                }
                at -= 1;
            }
            None
        } else {
            let mut at = offset + 1;
            while at < TICKS_PER_ARRAY {
                if data[12 + at as usize * FIXED_TICK_LEN] != 0 {
                    return Some(at);
                }
                at += 1;
            }
            None
        }
    }

    /// The tick `tick` of a pool of `tick_spacing`, which this array must
    /// hold.
    pub fn tick(&self, tick: i32, tick_spacing: u16) -> Result<Tick> {
        let spacing = tick_spacing as i32;
        let offset = tick - self.start;
        require!(
            offset >= 0 && offset % spacing == 0 && offset / spacing < TICKS_PER_ARRAY,
            DriverError::WrongAccounts
        );
        let at = (offset / spacing) as usize;
        let data = &self.data;
        if !self.dynamic {
            let base = 12 + at * FIXED_TICK_LEN;
            return Ok(Tick {
                initialized: data[base] != 0,
                liquidity_net: i128_at(data, base + 1),
                fee_growth_outside_a: u128_at(data, base + 33),
                fee_growth_outside_b: u128_at(data, base + 49),
            });
        }
        // A dynamic array keeps a bitmap of its initialized ticks, then each
        // tick in order: one byte for one that isn't, that byte and its data
        // for one that is.
        let bitmap = u128_at(data, 44);
        if bitmap & (1u128 << at) == 0 {
            return Ok(Tick::default());
        }
        let below = (bitmap & ((1u128 << at) - 1)).count_ones() as usize;
        let base = DYNAMIC_HEADER + below * (DYNAMIC_TICK_LEN + 1) + (at - below) + 1;
        require!(
            data.len() >= base + DYNAMIC_TICK_LEN && data[base - 1] == 1,
            DriverError::WrongAccounts
        );
        Ok(Tick {
            initialized: true,
            liquidity_net: i128_at(data, base),
            fee_growth_outside_a: u128_at(data, base + 32),
            fee_growth_outside_b: u128_at(data, base + 48),
        })
    }
}

/// The fee growth inside a position's range, as Orca's program works it out
/// from the pool's global growth and the growth outside each of its ticks.
pub fn fee_growth_inside(
    pool: &Whirlpool,
    lower: &Tick,
    tick_lower: i32,
    upper: &Tick,
    tick_upper: i32,
) -> (u128, u128) {
    let (global_a, global_b) = (pool.fee_growth_global_a, pool.fee_growth_global_b);
    let current = pool.tick_current;
    // By the pool's convention an uninitialized lower tick has had all fees
    // earned below it, and an uninitialized upper tick none above it.
    let (below_a, below_b) = if !lower.initialized {
        (global_a, global_b)
    } else if current < tick_lower {
        (
            global_a.wrapping_sub(lower.fee_growth_outside_a),
            global_b.wrapping_sub(lower.fee_growth_outside_b),
        )
    } else {
        (lower.fee_growth_outside_a, lower.fee_growth_outside_b)
    };
    let (above_a, above_b) = if !upper.initialized {
        (0, 0)
    } else if current < tick_upper {
        (upper.fee_growth_outside_a, upper.fee_growth_outside_b)
    } else {
        (
            global_a.wrapping_sub(upper.fee_growth_outside_a),
            global_b.wrapping_sub(upper.fee_growth_outside_b),
        )
    };
    (
        global_a.wrapping_sub(below_a).wrapping_sub(above_a),
        global_b.wrapping_sub(below_b).wrapping_sub(above_b),
    )
}

/// The data of each instruction the driver prepares.
pub fn decrease_liquidity(liquidity: u128, min_a: u64, min_b: u64) -> Vec<u8> {
    let mut data = ix::DECREASE_LIQUIDITY.to_vec();
    data.extend_from_slice(&liquidity.to_le_bytes());
    data.extend_from_slice(&min_a.to_le_bytes());
    data.extend_from_slice(&min_b.to_le_bytes());
    data
}

pub fn swap(amount: u64, threshold: u64, sqrt_price_limit: u128, a_to_b: bool) -> Vec<u8> {
    let mut data = ix::SWAP.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&threshold.to_le_bytes());
    data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    // The amount is what is sold.
    data.push(1);
    data.push(u8::from(a_to_b));
    data
}
