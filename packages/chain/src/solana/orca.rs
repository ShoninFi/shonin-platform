//! Orca Whirlpools, as the services reach it through a station's Orca
//! driver (`shonin-orca-driver`).
//!
//! Everything the driver fixes for its network (Orca's program, the pools it
//! lists, the dollar and wrapped SOL, Pyth's SOL/USD account) is read from
//! the driver's own crate, and every account of Orca's or Pyth's is read with
//! the driver's own readers, so the services see exactly what the driver
//! sees. What this module adds is what the driver never does: finding the
//! accounts a station's position stands on, laying out the group the driver
//! is given in a valuation or a close, building Orca's instructions for the
//! executor and the set-up, and the maths of a position's prices and
//! liquidity.
//!
//! **Prices.** A pool keeps its price as the square root of token B per
//! token A, in Q64.64. Here a price is dollars per whole SOL, scaled by
//! `10^PRICE_DECIMALS`, whichever of the two the pool keeps as A.

use anchor_lang::prelude::AccountInfo;
use anchor_spl::associated_token::{
    get_associated_token_address_with_program_id,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
};
use anchor_spl::token::spl_token;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use shonin_orca_driver::math::{self, U256};
use shonin_orca_driver::orca::{self as layout, Tick, TickArray, Whirlpool};
use shonin_orca_driver::pyth::Price;
use shonin_station::state::Pinned;

use super::client::SolanaClient;
use super::rpc::Account;
use super::{AccountMeta, Instruction, Pubkey};

pub use shonin_orca_driver::network::{
    self, ASSET_DECIMALS, ASSET_MINT, CONFIG, DOLLAR_DECIMALS, DOLLAR_MINT, FEED_ID, MAX_AGE,
    POOLS, PRICE_ACCOUNT, PYTH_RECEIVER, Pool as Listed, WHIRLPOOL_PROGRAM,
};
pub use shonin_orca_driver::orca::Position;
pub use shonin_orca_driver::{
    ASSOCIATED_TOKEN_PROGRAM, MAX_SLOTS, NAME, TOKEN_2022_PROGRAM, TOKEN_PROGRAM,
};

pub mod setup;

const SYSTEM: Pubkey = anchor_lang::solana_program::system_program::ID;
const RENT: Pubkey = anchor_lang::solana_program::sysvar::rent::ID;

/// The account Orca's program takes as the authority over a position NFT's
/// metadata.
pub const METADATA_UPDATE_AUTH: Pubkey =
    anchor_lang::prelude::pubkey!("3axbTs2z5GBy6usVbNVoqEgZMng3vZvMnAoX29BFfwhr");

/// Ticks in one tick array.
pub const TICKS_PER_ARRAY: i32 = layout::TICKS_PER_ARRAY;

/// How many places a price here carries: dollars per SOL times 10^12.
pub const PRICE_DECIMALS: u32 = 12;

/// The symbol of the token the listed pools pair with the dollar.
pub const TOKEN: &str = "SOL";

/// Whether `program` is the Orca driver this build lists.
pub fn is_orca(program: &Pubkey) -> bool {
    *program == shonin_orca_driver::ID
}

/// Whether the token a listed pool keeps as A is SOL (and B the dollar).
pub fn asset_is_a() -> bool {
    network::asset_is_a()
}

/// The name an agent knows the listed pool `p` by: `SOL/USD`, and for any
/// pool after the first its tick spacing as well.
pub fn pool_name(p: usize) -> String {
    match p {
        0 => format!("{TOKEN}/USD"),
        _ => format!("{TOKEN}/USD/{}", POOLS[p].tick_spacing),
    }
}

/// The listed pool an agent names, if any.
pub fn pool_named(name: &str) -> Option<usize> {
    (0..POOLS.len()).find(|p| pool_name(*p) == name)
}

// Addresses -----------------------------------------------------------------

pub fn tick_array_address(pool: &Pubkey, start: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), start.to_string().as_bytes()],
        &WHIRLPOOL_PROGRAM,
    )
    .0
}

pub fn position_address(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"position", mint.as_ref()], &WHIRLPOOL_PROGRAM).0
}

pub fn oracle_address(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"oracle", pool.as_ref()], &WHIRLPOOL_PROGRAM).0
}

pub fn fee_tier_address(config: &Pubkey, tick_spacing: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[b"fee_tier", config.as_ref(), &tick_spacing.to_le_bytes()],
        &WHIRLPOOL_PROGRAM,
    )
    .0
}

/// The pool of the listed pair at `tick_spacing`, and its bump.
pub fn pool_address(tick_spacing: u16) -> (Pubkey, u8) {
    let (a, b) = network::mints();
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
}

/// The authority's position mint at `slot`: an address of the station
/// program, which signs for it.
pub fn slot_mint(authority: &Pubkey, slot: u8) -> Pubkey {
    shonin_driver::position_mint(&shonin_station::ID, authority, slot).0
}

/// `owner`'s token account holding a position NFT minted at `mint`.
pub fn position_token(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, &TOKEN_2022_PROGRAM)
}

/// `owner`'s wrapped-SOL account.
pub fn wsol_of(owner: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, &ASSET_MINT, &spl_token::ID)
}

/// `owner`'s accounts of the pool's A and B: its wrapped SOL and its account
/// of the dollar, in the order the pool keeps them.
pub fn owners(owner: &Pubkey, dollars: &Pubkey) -> (Pubkey, Pubkey) {
    let wsol = wsol_of(owner);
    if asset_is_a() {
        (wsol, *dollars)
    } else {
        (*dollars, wsol)
    }
}

/// The first tick of the array of a pool of `tick_spacing` holding `tick`.
pub fn array_start(tick: i32, tick_spacing: u16) -> i32 {
    let span = TICKS_PER_ARRAY * tick_spacing as i32;
    tick.div_euclid(span) * span
}

/// The first ticks of the three arrays a swap from the pool's price now
/// crosses, A for B if `a_to_b`: the ones Orca's swap and the driver's sale
/// are given.
pub fn swap_starts(pool: &Whirlpool, a_to_b: bool) -> [i32; 3] {
    let span = TICKS_PER_ARRAY * pool.tick_spacing as i32;
    let base = pool.array_start(pool.tick_current);
    if a_to_b {
        [base, base - span, base - 2 * span]
    } else if pool.tick_current + pool.tick_spacing as i32 >= base + span {
        [base + span, base + 2 * span, base + 3 * span]
    } else {
        [base, base + span, base + 2 * span]
    }
}

/// Whether a tick array may start at `start` in a pool of `tick_spacing`:
/// it holds at least one tick a price can stand at.
pub fn array_in_range(start: i32, tick_spacing: u16) -> bool {
    let span = TICKS_PER_ARRAY * tick_spacing as i32;
    start + span > math::MIN_TICK && start <= math::MAX_TICK
}

// Reading -------------------------------------------------------------------

/// Run one of the driver's readers over an account as the cluster gave it.
fn read<T>(
    address: &Pubkey,
    account: &Account,
    reader: impl FnOnce(&AccountInfo) -> anchor_lang::Result<T>,
) -> Result<T> {
    let mut lamports = account.lamports;
    let mut data = account.data.clone();
    let info = AccountInfo::new(
        address,
        false,
        false,
        &mut lamports,
        &mut data,
        &account.owner,
        account.executable,
        0,
    );
    reader(&info)
        .map_err(|error| anyhow!("{address} does not read as Orca's driver reads it: {error}"))
}

/// A pool's account, read as the driver reads it.
pub fn read_pool(address: &Pubkey, account: &Account) -> Result<Whirlpool> {
    read(address, account, Whirlpool::load)
}

pub fn read_position(address: &Pubkey, account: &Account) -> Result<Position> {
    read(address, account, Position::load)
}

/// Pyth's SOL/USD account, read as the driver reads it.
pub fn read_price(account: &Account) -> Result<Price> {
    read(&PRICE_ACCOUNT, account, Price::load)?
        .ok_or_else(|| anyhow!("Pyth's SOL/USD account holds no price the driver can read"))
}

/// The tick `tick` of a pool of `tick_spacing` from the tick array holding
/// it; an array that isn't there holds no initialized tick.
pub fn read_tick(
    address: &Pubkey,
    account: Option<&Account>,
    tick: i32,
    tick_spacing: u16,
) -> Result<Tick> {
    let Some(account) = account else {
        return Ok(Tick::default());
    };
    read(address, account, |info| match TickArray::load(info)? {
        Some(array) => array.tick(tick, tick_spacing),
        None => Ok(Tick::default()),
    })
}

/// A token account's balance, or nothing if there is no such account.
pub fn token_amount(account: Option<&Account>) -> u64 {
    account
        .filter(|account| account.data.len() >= 72)
        .map_or(0, |account| {
            u64::from_le_bytes(account.data[64..72].try_into().expect("eight bytes"))
        })
}

/// One listed pool as the cluster holds it now.
#[derive(Clone, Debug)]
pub struct PoolNow {
    pub address: Pubkey,
    pub state: Whirlpool,
}

/// A position the authority holds at one of its slots, with its fees
/// brought up to date as the driver brings them.
#[derive(Clone, Debug)]
pub struct Held {
    pub slot: u8,
    pub mint: Pubkey,
    pub address: Pubkey,
    /// Which listed pool it is in.
    pub pool: usize,
    pub state: Position,
    /// Its fees owed, and earned since its last update.
    pub fees_a: u64,
    pub fees_b: u64,
    /// Whether bringing its fees up to date changes anything.
    pub earning: bool,
}

impl Held {
    /// The tick arrays of its two ticks.
    pub fn arrays(&self, pools: &[PoolNow]) -> (Pubkey, Pubkey) {
        let pool = &pools[self.pool];
        let spacing = pool.state.tick_spacing;
        (
            tick_array_address(&pool.address, array_start(self.state.tick_lower, spacing)),
            tick_array_address(&pool.address, array_start(self.state.tick_upper, spacing)),
        )
    }
}

/// Everything the station holds at Orca through one authority, read at
/// once.
#[derive(Clone, Debug)]
pub struct Holdings {
    pub authority: Pubkey,
    /// Every listed pool, in the listed order.
    pub pools: Vec<PoolNow>,
    /// The authority's wrapped SOL, and whether its account exists.
    pub asset: u64,
    pub asset_account: bool,
    /// Pyth's price, if its account reads as one.
    pub price: Option<Price>,
    /// Each slot's position, by slot; `None` for an empty slot.
    pub slots: Vec<Option<Held>>,
}

impl Holdings {
    /// Read them from the cluster.
    pub async fn read(client: &SolanaClient, authority: &Pubkey) -> Result<Self> {
        let mints: Vec<Pubkey> = (0..MAX_SLOTS)
            .map(|slot| slot_mint(authority, slot))
            .collect();
        let mut addresses: Vec<Pubkey> = POOLS.iter().map(|listed| listed.address).collect();
        addresses.push(PRICE_ACCOUNT);
        addresses.push(wsol_of(authority));
        addresses.extend(mints.iter().map(position_address));
        let found = client.rpc.accounts(&addresses).await?;
        let mut pools = Vec::with_capacity(POOLS.len());
        for (listed, account) in POOLS.iter().zip(&found) {
            let account = account.as_ref().ok_or_else(|| {
                anyhow!(
                    "the pool {} the Orca driver lists is not on this cluster",
                    listed.address
                )
            })?;
            pools.push(PoolNow {
                address: listed.address,
                state: read_pool(&listed.address, account)?,
            });
        }
        let rest = &found[POOLS.len()..];
        let price = rest[0]
            .as_ref()
            .and_then(|account| read_price(account).ok());
        let asset_account = rest[1].is_some();
        let asset = token_amount(rest[1].as_ref());
        let mut slots = Vec::with_capacity(mints.len());
        for (slot, (mint, account)) in mints.iter().zip(&rest[2..]).enumerate() {
            let Some(account) = account else {
                slots.push(None);
                continue;
            };
            let address = position_address(mint);
            let state = read_position(&address, account)?;
            let pool = POOLS
                .iter()
                .position(|listed| listed.address == state.whirlpool)
                .ok_or_else(|| {
                    anyhow!("the position {address} is in a pool the driver doesn't list")
                })?;
            slots.push(Some(Held {
                slot: slot as u8,
                mint: *mint,
                address,
                pool,
                fees_a: state.fee_owed_a,
                fees_b: state.fee_owed_b,
                earning: false,
                state,
            }));
        }
        let mut holdings = Self {
            authority: *authority,
            pools,
            asset,
            asset_account,
            price,
            slots,
        };
        holdings.bring_fees_up_to_date(client).await?;
        Ok(holdings)
    }

    /// Each position's fees as updating them would leave them, from the
    /// fee growth its ticks' arrays record, as the driver works them out.
    async fn bring_fees_up_to_date(&mut self, client: &SolanaClient) -> Result<()> {
        let mut wanted = Vec::new();
        for held in self.held() {
            if held.state.liquidity > 0 {
                let (lower, upper) = held.arrays(&self.pools);
                wanted.extend([lower, upper]);
            }
        }
        if wanted.is_empty() {
            return Ok(());
        }
        let found = client.rpc.accounts(&wanted).await?;
        let pools = self.pools.clone();
        for held in self.slots.iter_mut().flatten() {
            if held.state.liquidity == 0 {
                continue;
            }
            let pool = &pools[held.pool].state;
            let (lower_at, upper_at) = held.arrays(&pools);
            let find = |address: &Pubkey| {
                wanted
                    .iter()
                    .position(|w| w == address)
                    .and_then(|i| found[i].as_ref())
            };
            let lower = read_tick(
                &lower_at,
                find(&lower_at),
                held.state.tick_lower,
                pool.tick_spacing,
            )?;
            let upper = read_tick(
                &upper_at,
                find(&upper_at),
                held.state.tick_upper,
                pool.tick_spacing,
            )?;
            let (inside_a, inside_b) = layout::fee_growth_inside(
                pool,
                &lower,
                held.state.tick_lower,
                &upper,
                held.state.tick_upper,
            );
            let p = &held.state;
            let earned_a = math::fees_earned(p.liquidity, inside_a, p.checkpoint_a);
            let earned_b = math::fees_earned(p.liquidity, inside_b, p.checkpoint_b);
            held.fees_a = p.fee_owed_a.wrapping_add(earned_a);
            held.fees_b = p.fee_owed_b.wrapping_add(earned_b);
            held.earning = earned_a != 0 || earned_b != 0;
        }
        Ok(())
    }

    /// The positions held, slot by slot.
    pub fn held(&self) -> impl Iterator<Item = &Held> {
        self.slots.iter().flatten()
    }

    /// The position at `address`, if the authority holds it.
    pub fn position(&self, address: &Pubkey) -> Option<&Held> {
        self.held().find(|held| held.address == *address)
    }

    /// The first slot holding no position.
    pub fn free_slot(&self) -> Option<u8> {
        self.slots
            .iter()
            .position(Option::is_none)
            .map(|slot| slot as u8)
    }

    /// The accounts the Orca driver is given for this authority, as its
    /// layout says, after its program: the authority (read) and its dollar
    /// account (written), Orca, the token program, the price, the
    /// authority's wrapped SOL (written), each listed pool and its vaults
    /// (written), the first pool's oracle, the three tick arrays a sale of
    /// SOL there crosses from its price now (written), each slot (its
    /// position, written, or the slot's mint, read), each position's token
    /// account (read), then the tick arrays of every position with
    /// liquidity (written). What a valuation, a close and a sweep are given.
    pub fn group(&self, program: &Pubkey, dollars: &Pubkey) -> Vec<AccountMeta> {
        let first = &self.pools[0];
        let mut group = vec![
            AccountMeta::new_readonly(*program, false),
            AccountMeta::new_readonly(self.authority, false),
            AccountMeta::new(*dollars, false),
            AccountMeta::new_readonly(WHIRLPOOL_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(PRICE_ACCOUNT, false),
            AccountMeta::new(wsol_of(&self.authority), false),
        ];
        for pool in &self.pools {
            group.push(AccountMeta::new(pool.address, false));
            group.push(AccountMeta::new(pool.state.vault_a, false));
            group.push(AccountMeta::new(pool.state.vault_b, false));
        }
        group.push(AccountMeta::new_readonly(
            oracle_address(&first.address),
            false,
        ));
        group.extend(
            swap_starts(&first.state, asset_is_a())
                .map(|start| AccountMeta::new(tick_array_address(&first.address, start), false)),
        );
        let mut tokens = Vec::new();
        let mut arrays: Vec<Pubkey> = Vec::new();
        for (slot, held) in self.slots.iter().enumerate() {
            match held {
                Some(held) => {
                    group.push(AccountMeta::new(held.address, false));
                    tokens.push(AccountMeta::new_readonly(
                        position_token(&self.authority, &held.mint),
                        false,
                    ));
                    if held.state.liquidity > 0 {
                        let (lower, upper) = held.arrays(&self.pools);
                        for array in [lower, upper] {
                            if !arrays.contains(&array) {
                                arrays.push(array);
                            }
                        }
                    }
                }
                None => group.push(AccountMeta::new_readonly(
                    slot_mint(&self.authority, slot as u8),
                    false,
                )),
            }
        }
        group.extend(tokens);
        group.extend(
            arrays
                .into_iter()
                .map(|array| AccountMeta::new(array, false)),
        );
        group
    }
}

/// The group a pinned Orca driver is given: see [`Holdings::group`].
pub async fn group(client: &SolanaClient, pinned: &Pinned) -> Result<Vec<AccountMeta>> {
    let holdings = Holdings::read(client, &pinned.authority)
        .await
        .context("reading the station's holdings at Orca")?;
    Ok(holdings.group(&pinned.program, &pinned.dollars))
}

// Prices --------------------------------------------------------------------

fn power_of_ten(exponent: u32) -> U256 {
    U256::from(10u64).pow(U256::from(exponent))
}

/// `10^(PRICE_DECIMALS + ASSET_DECIMALS - DOLLAR_DECIMALS)`: a price here is
/// dollar-units per lamport times this.
fn price_scale() -> U256 {
    power_of_ten(PRICE_DECIMALS + ASSET_DECIMALS - DOLLAR_DECIMALS)
}

/// A pool's square-root price as a price here, rounded down.
pub fn price_of_sqrt(sqrt_price: u128) -> u128 {
    let square = U256::from(sqrt_price) * U256::from(sqrt_price);
    if square.is_zero() {
        return 0;
    }
    let price = if asset_is_a() {
        // Dollar-units per lamport, times 2^128.
        (square * price_scale()) >> 128
    } else {
        // Lamports per dollar-unit, times 2^128.
        (price_scale() << 128) / square
    };
    if price > U256::from(u128::MAX) {
        u128::MAX
    } else {
        price.low_u128()
    }
}

/// The price at a tick.
pub fn price_at_tick(tick: i32) -> Result<u128> {
    Ok(price_of_sqrt(
        math::sqrt_price_at(tick).map_err(|_| anyhow!("tick {tick} is out of range"))?,
    ))
}

/// The square-root price of a price here, rounded down, within the bounds a
/// pool accepts.
pub fn sqrt_of_price(price: u128) -> u128 {
    if price == 0 {
        return if asset_is_a() {
            math::MIN_SQRT_PRICE
        } else {
            math::MAX_SQRT_PRICE
        };
    }
    // The pool's price, B per A, times 2^128.
    let raw = if asset_is_a() {
        (U256::from(price) << 128) / price_scale()
    } else {
        (price_scale() << 128) / U256::from(price)
    };
    let raw = shonin_orca_driver::wide::W(raw.0);
    shonin_orca_driver::logic::sqrt_price_for(raw)
}

/// The highest tick whose square-root price is at most `sqrt_price`.
pub fn tick_at_or_below(sqrt_price: u128) -> i32 {
    let (mut low, mut high) = (math::MIN_TICK, math::MAX_TICK);
    if sqrt_price <= math::MIN_SQRT_PRICE {
        return math::MIN_TICK;
    }
    while low < high {
        let mid = low + (high - low + 1) / 2;
        if math::sqrt_price_at(mid).unwrap_or(u128::MAX) <= sqrt_price {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// The ticks of a position between two prices here, rounded outward to
/// `tick_spacing` and kept inside the ticks a pool allows; the lower tick
/// first, whichever way the pool keeps its prices.
pub fn ticks_between(
    lower_price: u128,
    upper_price: u128,
    tick_spacing: u16,
) -> Result<(i32, i32)> {
    if lower_price == 0 || upper_price <= lower_price {
        bail!("a band's upper price must be above its lower price, and both above zero");
    }
    let spacing = tick_spacing as i32;
    let roots = [sqrt_of_price(lower_price), sqrt_of_price(upper_price)];
    let (low_root, high_root) = (roots[0].min(roots[1]), roots[0].max(roots[1]));
    let lowest = math::MIN_TICK.div_euclid(spacing) * spacing + spacing;
    let highest = math::MAX_TICK.div_euclid(spacing) * spacing;
    let lower = tick_at_or_below(low_root).div_euclid(spacing) * spacing;
    let upper_exact = tick_at_or_below(high_root);
    let upper = if math::sqrt_price_at(upper_exact).ok() == Some(high_root)
        && upper_exact.rem_euclid(spacing) == 0
    {
        upper_exact
    } else {
        (upper_exact.div_euclid(spacing) + 1) * spacing
    };
    let (lower, upper) = (lower.max(lowest), upper.min(highest));
    if lower >= upper {
        bail!("the band is narrower than the pool's price step");
    }
    Ok((lower, upper))
}

// Liquidity -----------------------------------------------------------------

/// What adding `liquidity` between two ticks takes of A and B, exactly as the
/// pool takes it (rounded up): which tokens by the pool's current tick
/// against the ticks, how much by its square-root price.
pub fn deposit(
    current_tick: i32,
    sqrt_price: u128,
    (tick_lower, tick_upper): (i32, i32),
    liquidity: u128,
) -> Result<(u64, u64)> {
    if liquidity == 0 {
        return Ok((0, 0));
    }
    let lower = math::sqrt_price_at(tick_lower).map_err(|_| anyhow!("tick out of range"))?;
    let upper = math::sqrt_price_at(tick_upper).map_err(|_| anyhow!("tick out of range"))?;
    let overflow = |_| anyhow!("the liquidity is too large");
    Ok(if current_tick < tick_lower {
        (
            math::amount_a(lower, upper, liquidity, true).map_err(overflow)?,
            0,
        )
    } else if current_tick < tick_upper {
        (
            math::amount_a(sqrt_price, upper, liquidity, true).map_err(overflow)?,
            math::amount_b(lower, sqrt_price, liquidity, true).map_err(overflow)?,
        )
    } else {
        (
            0,
            math::amount_b(lower, upper, liquidity, true).map_err(overflow)?,
        )
    })
}

/// The most liquidity between two ticks that `max_a` of A and `max_b` of B
/// pay for at the pool's price now, as the pool takes it: never more of
/// either than its maximum. Zero if they pay for none.
pub fn liquidity_for(
    current_tick: i32,
    sqrt_price: u128,
    ticks: (i32, i32),
    max_a: u64,
    max_b: u64,
) -> Result<u128> {
    let lower = math::sqrt_price_at(ticks.0).map_err(|_| anyhow!("tick out of range"))?;
    let upper = math::sqrt_price_at(ticks.1).map_err(|_| anyhow!("tick out of range"))?;
    // From A: L = a * s * upper / ((upper - s) * 2^64), with s the larger of
    // the price and the lower root.
    let from_a = |s: u128| -> U256 {
        if s >= upper {
            return U256::MAX;
        }
        (U256::from(max_a) * U256::from(s) * U256::from(upper)) / (U256::from(upper - s) << 64)
    };
    // From B: L = b * 2^64 / (s - lower), with s the smaller of the price
    // and the upper root.
    let from_b = |s: u128| -> U256 {
        if s <= lower {
            return U256::MAX;
        }
        (U256::from(max_b) << 64) / U256::from(s - lower)
    };
    let estimate = if current_tick < ticks.0 {
        from_a(lower)
    } else if current_tick < ticks.1 {
        from_a(sqrt_price.max(lower)).min(from_b(sqrt_price.min(upper)))
    } else {
        from_b(upper)
    };
    let mut liquidity = if estimate > U256::from(u128::MAX) {
        u128::MAX
    } else {
        estimate.low_u128()
    };
    // The pool rounds what it takes up: shave until both fit.
    for _ in 0..64 {
        if liquidity == 0 {
            return Ok(0);
        }
        match deposit(current_tick, sqrt_price, ticks, liquidity) {
            Ok((a, b)) if a <= max_a && b <= max_b => return Ok(liquidity),
            _ => liquidity -= (liquidity / 1_000_000).max(1),
        }
    }
    bail!("no liquidity between those ticks fits those amounts")
}

/// The square-root prices at the two edges of `band_bps` around `price` (a
/// price here, Pyth's), each half the band from its root as the driver's
/// close bounds a sale (a root moves about half as far as its price, and
/// never less): the root at the band's lowest dollar price first, then at
/// its highest, whichever way the pool keeps its prices.
pub fn band_roots(price: u128, band_bps: u16) -> (u128, u128) {
    let root = U256::from(sqrt_of_price(price));
    let (two, band) = (U256::from(20_000u64), U256::from(band_bps as u64));
    let clamp = |value: U256| -> u128 {
        value
            .min(U256::from(math::MAX_SQRT_PRICE))
            .max(U256::from(math::MIN_SQRT_PRICE))
            .low_u128()
    };
    let down = clamp(root * (two - band.min(two)) / two);
    let up = clamp(root * (two + band) / two);
    if asset_is_a() { (down, up) } else { (up, down) }
}

/// The most liquidity between two ticks that `max_a` of A and `max_b` of B
/// pay for wherever the pool's square-root price stands between `roots`
/// (either order), and the most of each it then takes: A is taken most at
/// the lowest root, B at the highest. So an increase given these maxima
/// takes nothing at a price outside them, however the pool is pushed before
/// it lands. Zero if they pay for none.
pub fn liquidity_within(
    ticks: (i32, i32),
    roots: (u128, u128),
    max_a: u64,
    max_b: u64,
) -> Result<(u128, (u64, u64))> {
    let (low, high) = (roots.0.min(roots.1), roots.0.max(roots.1));
    let lower = math::sqrt_price_at(ticks.0).map_err(|_| anyhow!("tick out of range"))?;
    let upper = math::sqrt_price_at(ticks.1).map_err(|_| anyhow!("tick out of range"))?;
    // From A at the lowest root: L = a * s * upper / ((upper - s) * 2^64).
    let from_a = if low >= upper {
        U256::MAX
    } else {
        let s = low.max(lower);
        (U256::from(max_a) * U256::from(s) * U256::from(upper)) / (U256::from(upper - s) << 64)
    };
    // From B at the highest: L = b * 2^64 / (s - lower).
    let from_b = if high <= lower {
        U256::MAX
    } else {
        let s = high.min(upper);
        (U256::from(max_b) << 64) / U256::from(s - lower)
    };
    let estimate = from_a.min(from_b);
    let mut liquidity = if estimate > U256::from(u128::MAX) {
        u128::MAX
    } else {
        estimate.low_u128()
    };
    let takes = |liquidity: u128| -> Result<(u64, u64)> {
        let a = deposit(tick_at_or_below(low), low, ticks, liquidity)?.0;
        let b = deposit(tick_at_or_below(high), high, ticks, liquidity)?.1;
        Ok((a, b))
    };
    // The pool rounds what it takes up: shave until both fit.
    for _ in 0..64 {
        if liquidity == 0 {
            return Ok((0, (0, 0)));
        }
        match takes(liquidity) {
            Ok((a, b)) if a <= max_a && b <= max_b => return Ok((liquidity, (a, b))),
            _ => liquidity -= (liquidity / 1_000_000).max(1),
        }
    }
    bail!("no liquidity between those ticks fits those amounts")
}

/// What a position holds of A and B at the square-root price `sqrt_price`,
/// rounded down.
pub fn holds(position: &Position, sqrt_price: u128) -> Result<(u64, u64)> {
    let lower =
        math::sqrt_price_at(position.tick_lower).map_err(|_| anyhow!("tick out of range"))?;
    let upper =
        math::sqrt_price_at(position.tick_upper).map_err(|_| anyhow!("tick out of range"))?;
    math::amounts_at(sqrt_price, lower, upper, position.liquidity)
        .map_err(|_| anyhow!("the position's amounts overflow"))
}

/// The least of A and B a withdrawal of `liquidity` from a position between
/// two ticks pays with the pool's price anywhere within `band_bps` of the
/// price at `sqrt_price`, as the driver's close bounds them: worked out at
/// the square-root price moved by the whole band either way, which is past
/// the band's own roots (a root moves about half as far as its price). A
/// pays least at the highest, B at the lowest.
pub fn withdrawal_minimums(
    sqrt_price: u128,
    (tick_lower, tick_upper): (i32, i32),
    liquidity: u128,
    band_bps: u16,
) -> Result<(u64, u64)> {
    let lower = math::sqrt_price_at(tick_lower).map_err(|_| anyhow!("tick out of range"))?;
    let upper = math::sqrt_price_at(tick_upper).map_err(|_| anyhow!("tick out of range"))?;
    let band = band_bps as u128;
    let scaled = |numerator: u128| -> u128 {
        let value = U256::from(sqrt_price) * U256::from(numerator) / U256::from(10_000u64);
        value
            .min(U256::from(math::MAX_SQRT_PRICE))
            .max(U256::from(math::MIN_SQRT_PRICE))
            .low_u128()
    };
    let (top, bottom) = (
        scaled(10_000 + band),
        scaled(10_000u128.saturating_sub(band)),
    );
    let overflow = |_| anyhow!("the liquidity is too large");
    let min_a = if top >= upper {
        0
    } else {
        math::amount_a(top.max(lower), upper, liquidity, false).map_err(overflow)?
    };
    let min_b = if bottom <= lower {
        0
    } else {
        math::amount_b(lower, bottom.min(upper), liquidity, false).map_err(overflow)?
    };
    Ok((min_a, min_b))
}

// Orca's instructions --------------------------------------------------------

/// `sha256("<prefix>:<name>")[..8]`, as Anchor names an instruction or an
/// account kind.
pub fn discriminator(prefix: &str, name: &str) -> [u8; 8] {
    let hash = Sha256::digest(format!("{prefix}:{name}").as_bytes());
    hash[..8].try_into().expect("eight bytes")
}

fn data(name: &str, args: &[&[u8]]) -> Vec<u8> {
    let mut out = discriminator("global", name).to_vec();
    for arg in args {
        out.extend_from_slice(arg);
    }
    out
}

/// `owner`'s wrapped-SOL account, made if it isn't there, `payer` paying.
pub fn ix_open_wsol(payer: &Pubkey, owner: &Pubkey) -> Instruction {
    create_associated_token_account_idempotent(payer, owner, &ASSET_MINT, &spl_token::ID)
}

/// A position of `owner`'s in `pool` between two ticks, its NFT minted at
/// `mint`, `funder` paying its rent. A mint that is a program's address (a
/// station's slot mint, which the station signs for) is not a signer here.
pub fn ix_open_position(
    funder: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    pool: &Pubkey,
    (tick_lower, tick_upper): (i32, i32),
) -> Instruction {
    let mint_signs = mint.is_on_curve();
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new(*funder, true),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new(position_address(mint), false),
            AccountMeta::new(*mint, mint_signs),
            AccountMeta::new(position_token(owner, mint), false),
            AccountMeta::new_readonly(*pool, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(METADATA_UPDATE_AUTH, false),
        ],
        data: data(
            "open_position_with_token_extensions",
            &[&tick_lower.to_le_bytes(), &tick_upper.to_le_bytes(), &[0]],
        ),
    }
}

/// An empty position closed, its rent to `receiver`.
pub fn ix_close_position(owner: &Pubkey, receiver: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*receiver, false),
            AccountMeta::new(position_address(mint), false),
            AccountMeta::new(*mint, false),
            AccountMeta::new(position_token(owner, mint), false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM, false),
        ],
        data: data("close_position_with_token_extensions", &[]),
    }
}

/// A change of liquidity: `increase_liquidity` with the most of A and B it
/// may take, or `decrease_liquidity` with the least it must pay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Increase,
    Decrease,
}

/// `owner_a` and `owner_b` are the owner's accounts of the pool's A and B.
#[allow(clippy::too_many_arguments)]
pub fn ix_modify(
    change: Change,
    owner: &Pubkey,
    pool: &PoolNow,
    position: &Position,
    mint: &Pubkey,
    (owner_a, owner_b): (Pubkey, Pubkey),
    liquidity: u128,
    (limit_a, limit_b): (u64, u64),
) -> Instruction {
    let spacing = pool.state.tick_spacing;
    let name = match change {
        Change::Increase => "increase_liquidity",
        Change::Decrease => "decrease_liquidity",
    };
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new(pool.address, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(position_address(mint), false),
            AccountMeta::new_readonly(position_token(owner, mint), false),
            AccountMeta::new(owner_a, false),
            AccountMeta::new(owner_b, false),
            AccountMeta::new(pool.state.vault_a, false),
            AccountMeta::new(pool.state.vault_b, false),
            AccountMeta::new(
                tick_array_address(&pool.address, array_start(position.tick_lower, spacing)),
                false,
            ),
            AccountMeta::new(
                tick_array_address(&pool.address, array_start(position.tick_upper, spacing)),
                false,
            ),
        ],
        data: data(
            name,
            &[
                &liquidity.to_le_bytes(),
                &limit_a.to_le_bytes(),
                &limit_b.to_le_bytes(),
            ],
        ),
    }
}

/// A position's fees and rewards brought up to date: anyone may.
pub fn ix_update_fees(pool: &PoolNow, position: &Position, mint: &Pubkey) -> Instruction {
    let spacing = pool.state.tick_spacing;
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new(pool.address, false),
            AccountMeta::new(position_address(mint), false),
            AccountMeta::new_readonly(
                tick_array_address(&pool.address, array_start(position.tick_lower, spacing)),
                false,
            ),
            AccountMeta::new_readonly(
                tick_array_address(&pool.address, array_start(position.tick_upper, spacing)),
                false,
            ),
        ],
        data: data("update_fees_and_rewards", &[]),
    }
}

/// A position's fees, into the owner's accounts of A and B.
pub fn ix_collect_fees(
    owner: &Pubkey,
    pool: &PoolNow,
    mint: &Pubkey,
    (owner_a, owner_b): (Pubkey, Pubkey),
) -> Instruction {
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(pool.address, false),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(position_address(mint), false),
            AccountMeta::new_readonly(position_token(owner, mint), false),
            AccountMeta::new(owner_a, false),
            AccountMeta::new(pool.state.vault_a, false),
            AccountMeta::new(owner_b, false),
            AccountMeta::new(pool.state.vault_b, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: data("collect_fees", &[]),
    }
}

/// A swap in `pool` of `amount` in, A for B if `a_to_b`, no further than the
/// square-root price `limit` and for no less than `threshold` out.
#[allow(clippy::too_many_arguments)]
pub fn ix_swap(
    owner: &Pubkey,
    pool: &PoolNow,
    (owner_a, owner_b): (Pubkey, Pubkey),
    amount: u64,
    threshold: u64,
    limit: u128,
    a_to_b: bool,
) -> Instruction {
    let arrays =
        swap_starts(&pool.state, a_to_b).map(|start| tick_array_address(&pool.address, start));
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(pool.address, false),
            AccountMeta::new(owner_a, false),
            AccountMeta::new(pool.state.vault_a, false),
            AccountMeta::new(owner_b, false),
            AccountMeta::new(pool.state.vault_b, false),
            AccountMeta::new(arrays[0], false),
            AccountMeta::new(arrays[1], false),
            AccountMeta::new(arrays[2], false),
            AccountMeta::new_readonly(oracle_address(&pool.address), false),
        ],
        // The amount is what goes in.
        data: shonin_orca_driver::orca::swap(amount, threshold, limit, a_to_b),
    }
}

/// A fixed tick array of `pool` starting at `start`, `funder` paying its
/// rent: anyone may.
pub fn ix_initialize_tick_array(pool: &Pubkey, funder: &Pubkey, start: i32) -> Instruction {
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(*pool, false),
            AccountMeta::new(*funder, true),
            AccountMeta::new(tick_array_address(pool, start), false),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data: data("initialize_tick_array", &[&start.to_le_bytes()]),
    }
}

/// The listed pair's pool at `tick_spacing`, on the fee tier at that spacing,
/// starting at `sqrt_price`; its two vaults are new keypairs that sign, and
/// `funder` pays.
pub fn ix_initialize_pool(
    funder: &Pubkey,
    tick_spacing: u16,
    sqrt_price: u128,
    vaults: (Pubkey, Pubkey),
) -> Instruction {
    let (a, b) = network::mints();
    let (address, bump) = pool_address(tick_spacing);
    Instruction {
        program_id: WHIRLPOOL_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(CONFIG, false),
            AccountMeta::new_readonly(a, false),
            AccountMeta::new_readonly(b, false),
            AccountMeta::new(*funder, true),
            AccountMeta::new(address, false),
            AccountMeta::new(vaults.0, true),
            AccountMeta::new(vaults.1, true),
            AccountMeta::new_readonly(fee_tier_address(&CONFIG, tick_spacing), false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new_readonly(RENT, false),
        ],
        data: data(
            "initialize_pool",
            &[
                &[bump],
                &tick_spacing.to_le_bytes(),
                &sqrt_price.to_le_bytes(),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shonin_orca_driver::layout;

    /// $150 a SOL, as a price here.
    const PRICE: u128 = 150 * 10u128.pow(PRICE_DECIMALS);

    fn a_pool(tick_current: i32, tick_spacing: u16) -> Whirlpool {
        Whirlpool {
            config: CONFIG,
            tick_spacing,
            fee_tier_index: tick_spacing,
            fee_rate: 2_000,
            protocol_fee_rate: 300,
            liquidity: 0,
            sqrt_price: math::sqrt_price_at(tick_current).unwrap(),
            tick_current,
            mint_a: network::mints().0,
            vault_a: Pubkey::new_unique(),
            fee_growth_global_a: 0,
            mint_b: network::mints().1,
            vault_b: Pubkey::new_unique(),
            fee_growth_global_b: 0,
        }
    }

    #[test]
    fn a_price_and_its_square_root_price_agree_to_within_a_hair() {
        for price in [PRICE, PRICE / 1_000, PRICE * 37, 1_234_567_890_123] {
            let back = price_of_sqrt(sqrt_of_price(price));
            assert!(back <= price, "{back} is above {price}");
            assert!(
                price - back <= price / 1_000_000_000 + 1,
                "{back} against {price}"
            );
        }
    }

    #[test]
    fn the_square_root_price_of_a_price_is_the_one_the_driver_works_out() {
        // Dollar-units per lamport, times 2^128, as the driver keeps a price:
        // $150 is 150e8 in Pyth's units over 10^11.
        let raw = (U256::from(150_00000000u64) << 128) / U256::from(100_000_000_000u64);
        let driver = shonin_orca_driver::logic::sqrt_price_for(shonin_orca_driver::wide::W(raw.0));
        assert_eq!(sqrt_of_price(PRICE), driver);
    }

    #[test]
    fn a_tick_is_the_highest_whose_square_root_price_is_at_most_the_one_asked() {
        for tick in [-443_000, -30_000, -1, 0, 1, 64, 12_345, 443_000] {
            let root = math::sqrt_price_at(tick).unwrap();
            assert_eq!(tick_at_or_below(root), tick);
            assert_eq!(tick_at_or_below(root + 1), tick);
            assert_eq!(tick_at_or_below(root - 1), tick - 1);
        }
    }

    #[test]
    fn a_band_s_ticks_are_on_the_spacing_and_reach_past_both_its_prices() {
        let spacing = 64;
        let (lower, upper) = ticks_between(PRICE * 96 / 100, PRICE * 104 / 100, spacing).unwrap();
        assert_eq!(lower % spacing as i32, 0);
        assert_eq!(upper % spacing as i32, 0);
        assert!(price_at_tick(lower).unwrap() <= PRICE * 96 / 100);
        assert!(price_at_tick(upper).unwrap() >= PRICE * 104 / 100 - 1);
        // No more than a step wider on either side.
        assert!(price_at_tick(lower + spacing as i32).unwrap() > PRICE * 96 / 100);
        assert!(price_at_tick(upper - spacing as i32).unwrap() < PRICE * 104 / 100);
        assert!(ticks_between(PRICE, PRICE, spacing).is_err());
        assert!(ticks_between(PRICE, PRICE + 1, spacing).is_ok());
        assert!(ticks_between(0, PRICE, spacing).is_err());
    }

    #[test]
    fn the_liquidity_bought_never_takes_more_than_either_maximum_and_uses_one_up() {
        let tick = tick_at_or_below(sqrt_of_price(PRICE));
        let s = sqrt_of_price(PRICE);
        let ticks = ticks_between(PRICE * 96 / 100, PRICE * 104 / 100, 64).unwrap();
        // 2 SOL and $200: the SOL runs out first at $150 inside ±4%.
        for (max_a, max_b) in [(2_000_000_000, 200_000_000), (10_000_000_000, 50_000_000)] {
            let liquidity = liquidity_for(tick, s, ticks, max_a, max_b).unwrap();
            let (a, b) = deposit(tick, s, ticks, liquidity).unwrap();
            assert!(a <= max_a && b <= max_b);
            // Within a millionth and a unit of one of them.
            let close = |used: u64, max: u64| max - used <= max / 500_000 + 2;
            assert!(
                close(a, max_a) || close(b, max_b),
                "{a}/{max_a} {b}/{max_b}"
            );
        }
        // Below the band only SOL is taken, above it only dollars.
        let below = liquidity_for(
            ticks.0 - 1,
            math::sqrt_price_at(ticks.0 - 1).unwrap(),
            ticks,
            1_000_000_000,
            0,
        )
        .unwrap();
        assert!(below > 0);
        assert_eq!(deposit(ticks.0 - 1, 0, ticks, below).unwrap().1, 0);
        let above = liquidity_for(
            ticks.1,
            math::sqrt_price_at(ticks.1).unwrap(),
            ticks,
            0,
            100_000_000,
        )
        .unwrap();
        assert!(above > 0);
        assert_eq!(deposit(ticks.1, 0, ticks, above).unwrap().0, 0);
        assert_eq!(liquidity_for(tick, s, ticks, 0, 0).unwrap(), 0);
    }

    #[test]
    fn liquidity_bought_within_a_band_takes_no_more_than_offered_anywhere_in_it_and_more_outside() {
        let ticks = ticks_between(PRICE * 96 / 100, PRICE * 104 / 100, 64).unwrap();
        let (low, high) = band_roots(PRICE, 100);
        assert!(low < sqrt_of_price(PRICE) && sqrt_of_price(PRICE) < high);
        // Half the band either way in the root: about the band in the price.
        let (at_low, at_high) = (price_of_sqrt(low), price_of_sqrt(high));
        assert!((PRICE * 9_899 / 10_000..=PRICE * 9_901 / 10_000).contains(&at_low));
        assert!((PRICE * 10_099 / 10_000..=PRICE * 10_101 / 10_000).contains(&at_high));
        let (max_a, max_b) = (10_000_000_000, 150_000_000);
        let (liquidity, (a, b)) = liquidity_within(ticks, (low, high), max_a, max_b).unwrap();
        assert!(liquidity > 0 && a <= max_a && b <= max_b);
        for root in [low, sqrt_of_price(PRICE), high] {
            let (took_a, took_b) = deposit(tick_at_or_below(root), root, ticks, liquidity).unwrap();
            assert!(took_a <= a && took_b <= b, "{took_a} {took_b} at {root}");
        }
        // A pool pushed a percent past either edge takes more than allowed.
        for (root, side) in [(low - low / 200, 0), (high + high / 200, 1)] {
            let took = deposit(tick_at_or_below(root), root, ticks, liquidity).unwrap();
            let (took, most) = if side == 0 { (took.0, a) } else { (took.1, b) };
            assert!(took > most, "{took} against {most}");
        }
        assert_eq!(liquidity_within(ticks, (low, high), 0, 0).unwrap().0, 0);
    }

    #[test]
    fn a_withdrawal_s_minimums_are_never_more_than_it_pays_at_the_price_now() {
        let s = sqrt_of_price(PRICE);
        let tick = tick_at_or_below(s);
        let ticks = ticks_between(PRICE * 96 / 100, PRICE * 104 / 100, 64).unwrap();
        let liquidity = liquidity_for(tick, s, ticks, 2_000_000_000, 300_000_000).unwrap();
        let roots = (
            math::sqrt_price_at(ticks.0).unwrap(),
            math::sqrt_price_at(ticks.1).unwrap(),
        );
        let paid = math::withdrawal_between(tick, s, ticks, roots, liquidity).unwrap();
        let (min_a, min_b) = withdrawal_minimums(s, ticks, liquidity, 100).unwrap();
        assert!(min_a <= paid.0 && min_b <= paid.1);
        // Anywhere inside the band (the root moving half as far as the
        // price), the withdrawal still pays them.
        for numerator in [9_950u64, 10_050] {
            let moved = (U256::from(s) * U256::from(numerator) / U256::from(10_000u64)).low_u128();
            let at = tick_at_or_below(moved);
            let pays = math::withdrawal_between(at, moved, ticks, roots, liquidity).unwrap();
            assert!(
                pays.0 >= min_a && pays.1 >= min_b,
                "{pays:?} against {min_a}, {min_b}"
            );
        }
        assert!(min_a > 0 && min_b > 0);
    }

    #[test]
    fn a_swap_is_given_the_arrays_the_driver_s_sale_reads() {
        // The driver's sale starts at the array holding the price and goes
        // down selling A; up, from the next array when the price is in the
        // last step of its own.
        let spacing = 64u16;
        let span = TICKS_PER_ARRAY * spacing as i32;
        let pool = a_pool(-20_000, spacing);
        let base = array_start(-20_000, spacing);
        assert_eq!(
            swap_starts(&pool, true),
            [base, base - span, base - 2 * span]
        );
        assert_eq!(
            swap_starts(&pool, false),
            [base, base + span, base + 2 * span]
        );
        let edge = a_pool(base + span - 1, spacing);
        assert_eq!(
            swap_starts(&edge, false),
            [base + span, base + 2 * span, base + 3 * span]
        );
    }

    #[test]
    fn every_instruction_the_driver_allows_is_named_as_the_driver_names_it() {
        use shonin_orca_driver::orca::ix;
        let named = [
            (
                "open_position_with_token_extensions",
                ix::OPEN_POSITION_WITH_TOKEN_EXTENSIONS,
            ),
            ("increase_liquidity", ix::INCREASE_LIQUIDITY),
            ("decrease_liquidity", ix::DECREASE_LIQUIDITY),
            ("update_fees_and_rewards", ix::UPDATE_FEES_AND_REWARDS),
            ("collect_fees", ix::COLLECT_FEES),
            (
                "close_position_with_token_extensions",
                ix::CLOSE_POSITION_WITH_TOKEN_EXTENSIONS,
            ),
            ("swap", ix::SWAP),
        ];
        for (name, expected) in named {
            assert_eq!(discriminator("global", name), expected, "{name}");
        }
        assert_eq!(discriminator("account", "Whirlpool"), layout_whirlpool());
    }

    fn layout_whirlpool() -> [u8; 8] {
        shonin_orca_driver::orca::WHIRLPOOL
    }

    #[test]
    fn a_station_s_position_is_opened_at_its_slot_mint_with_the_station_signing_for_the_mint() {
        let authority =
            shonin_station::venue_authority(&Pubkey::new_unique(), &WHIRLPOOL_PROGRAM).0;
        let funder = Pubkey::new_unique();
        let mint = slot_mint(&authority, 0);
        let pool = POOLS[0].address;
        let ix = ix_open_position(&funder, &authority, &mint, &pool, (-640, 640));
        assert_eq!(ix.accounts[0], AccountMeta::new(funder, true));
        assert_eq!(ix.accounts[1], AccountMeta::new_readonly(authority, false));
        assert_eq!(ix.accounts[2].pubkey, position_address(&mint));
        assert_eq!(
            ix.accounts[3],
            AccountMeta::new(mint, false),
            "the station signs for it"
        );
        assert_eq!(
            ix.accounts[4],
            AccountMeta::new(position_token(&authority, &mint), false)
        );
        assert_eq!(ix.accounts[5].pubkey, pool);
        assert_eq!(ix.accounts[9].pubkey, METADATA_UPDATE_AUTH);
        assert_eq!(
            &ix.data[8..],
            &[0x80, 0xfd, 0xff, 0xff, 0x80, 0x02, 0, 0, 0][..]
        );
        // Anyone else's fresh mint signs.
        let fresh = solana_keypair::Keypair::new();
        use solana_signer::Signer;
        let theirs = ix_open_position(&funder, &funder, &fresh.pubkey(), &pool, (-640, 640));
        assert!(theirs.accounts[3].is_signer);
    }

    #[test]
    fn the_driver_s_group_stands_as_its_layout_says_and_writes_what_a_close_writes() {
        let station = Pubkey::new_unique();
        let authority = shonin_station::venue_authority(&station, &WHIRLPOOL_PROGRAM).0;
        let dollars = Pubkey::new_unique();
        let spacing = POOLS[0].tick_spacing;
        let pool = PoolNow {
            address: POOLS[0].address,
            state: a_pool(-19_999, spacing),
        };
        let mint = slot_mint(&authority, 0);
        let position = Position {
            whirlpool: pool.address,
            mint,
            liquidity: 5,
            tick_lower: -30_016,
            tick_upper: -10_048,
            checkpoint_a: 0,
            fee_owed_a: 0,
            checkpoint_b: 0,
            fee_owed_b: 0,
        };
        let empty = Holdings {
            authority,
            pools: vec![pool.clone()],
            asset: 0,
            asset_account: false,
            price: None,
            slots: vec![None],
        };
        let group = empty.group(&shonin_orca_driver::ID, &dollars);
        // The driver counts from the authority, which follows the program.
        let at = |index: usize| &group[index - 1 + 1];
        assert_eq!(
            group[0],
            AccountMeta::new_readonly(shonin_orca_driver::ID, false)
        );
        assert_eq!(group[1], AccountMeta::new_readonly(authority, false));
        assert_eq!(group[2], AccountMeta::new(dollars, false));
        assert_eq!(
            *at(layout::ORCA + 1),
            AccountMeta::new_readonly(WHIRLPOOL_PROGRAM, false)
        );
        assert_eq!(
            *at(layout::TOKEN_PROGRAM + 1),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false)
        );
        assert_eq!(
            *at(layout::PRICE + 1),
            AccountMeta::new_readonly(PRICE_ACCOUNT, false)
        );
        assert_eq!(
            *at(layout::ASSET + 1),
            AccountMeta::new(wsol_of(&authority), false)
        );
        assert_eq!(
            *at(layout::pool(0) + 1),
            AccountMeta::new(pool.address, false)
        );
        assert_eq!(
            *at(layout::vault_a(0) + 1),
            AccountMeta::new(pool.state.vault_a, false)
        );
        assert_eq!(
            *at(layout::vault_b(0) + 1),
            AccountMeta::new(pool.state.vault_b, false)
        );
        assert_eq!(
            *at(layout::oracle() + 1),
            AccountMeta::new_readonly(oracle_address(&pool.address), false)
        );
        let base = array_start(-19_999, spacing);
        let span = TICKS_PER_ARRAY * spacing as i32;
        for (i, start) in [base, base - span, base - 2 * span].into_iter().enumerate() {
            assert_eq!(
                *at(layout::sale_arrays() + i + 1),
                AccountMeta::new(tick_array_address(&pool.address, start), false)
            );
        }
        assert_eq!(
            *at(layout::slots() + 1),
            AccountMeta::new_readonly(mint, false)
        );
        assert_eq!(group.len(), layout::tokens() + 1);

        let held = Holdings {
            slots: vec![Some(Held {
                slot: 0,
                mint,
                address: position_address(&mint),
                pool: 0,
                state: position,
                fees_a: 0,
                fees_b: 0,
                earning: false,
            })],
            ..empty
        };
        let group = held.group(&shonin_orca_driver::ID, &dollars);
        let at = |index: usize| &group[index];
        assert_eq!(
            *at(layout::slots() + 1),
            AccountMeta::new(position_address(&mint), false)
        );
        assert_eq!(
            *at(layout::tokens() + 1),
            AccountMeta::new_readonly(position_token(&authority, &mint), false)
        );
        assert_eq!(
            group[layout::tokens() + 2..],
            [
                AccountMeta::new(
                    tick_array_address(&pool.address, array_start(-30_016, spacing)),
                    false
                ),
                AccountMeta::new(
                    tick_array_address(&pool.address, array_start(-10_048, spacing)),
                    false
                ),
            ]
        );
    }

    #[test]
    fn the_published_list_says_of_orca_what_its_driver_says_of_itself() {
        use shared::catalogue::{Catalogue, Custody, DriverChain, Exit, Valuation};
        use shonin_driver::{Custody as Held, ExitPath, ValuationKind};
        let listed = &Catalogue::published().drivers["orca"];
        let described = shonin_orca_driver::logic::description();
        assert_eq!(described.name, NAME);
        assert_eq!(&NAME[..4], b"orca");
        assert_eq!(listed.chain, DriverChain::On(shared::Chain::Solana));
        assert_eq!(described.custody, Held::Station);
        assert_eq!(listed.custody, Custody::Station);
        assert_eq!(described.exit_path, ExitPath::Contract);
        assert_eq!(listed.exit, Exit::Contract);
        assert_eq!(described.valuation, ValuationKind::OnChain);
        assert_eq!(listed.valuation, Valuation::OnChain);
        // What the driver's `restricted()` answers: Orca's terms exclude
        // sanctioned persons and name no country.
        assert_eq!(listed.restricted, vec!["SANCTIONED".to_string()]);
        assert!(!listed.networks.contains(&"mainnet".to_string()));
    }

    #[test]
    fn the_listed_pool_is_named_sol_usd() {
        assert_eq!(pool_name(0), "SOL/USD");
        assert_eq!(pool_named("SOL/USD"), Some(0));
        assert_eq!(pool_named("ETH/USD"), None);
    }

    #[test]
    fn devnet_s_fee_tier_at_spacing_64_is_the_one_orca_made_there() {
        let config = anchor_lang::prelude::pubkey!("FcrweFY1G9HJAHG5inkGB6pKg1HZ6x9UC2WioAfWrGkR");
        assert_eq!(
            fee_tier_address(&config, 64),
            anchor_lang::prelude::pubkey!("nhg1SS1hNFnJKZrJ9FBf3L6SxTjwEnkehN7dmAbg25t")
        );
    }
}
