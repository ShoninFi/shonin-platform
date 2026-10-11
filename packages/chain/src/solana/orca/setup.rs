//! Setting Orca up on a cluster, and keeping its pool where it should be:
//! what the devnet does on a local cluster and on devnet, and what tests on
//! a local cluster do the same way.
//!
//! On a local cluster nothing of Orca's or Pyth's is there: Orca's program
//! is written at its own address under the upgradeable loader
//! ([`load_program`]), its settings and the fee tier as Orca lays them out
//! ([`config_data`], [`fee_tier_data`]), and Pyth's SOL/USD account as a
//! stand-in at its sponsored address ([`post_price`]), all through
//! Surfpool's account cheatcode. The pool, its tick arrays and every
//! position are made by Orca's program itself.

use anchor_spl::token::spl_token;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::solana::orca::{self as venue, Change, PoolNow};
use crate::solana::platform::{Platform, program_data_address};
use crate::solana::rpc::Account;
use crate::solana::{Instruction, Keypair, Pubkey, Signer, SolanaClient};

/// One dollar a SOL, as [`venue`] counts prices.
pub const PRICE_ONE: u128 = 10u128.pow(venue::PRICE_DECIMALS);
/// The fee tier's fee at the listed spacing (in millionths), as devnet's
/// tier has it, and Orca's share of it (in basis points).
pub const FEE_RATE: u16 = 2_000;
pub const PROTOCOL_FEE_RATE: u16 = 300;
pub const LAMPORTS: u64 = 1_000_000_000;
/// What the operator keeps of its SOL for fees and rent, never wrapped.
pub const OPERATOR_KEEPS: u64 = LAMPORTS / 2;
/// How far the pool may stray from its target before the operator brings
/// it back, in basis points: a quarter of the exit band a vault is created
/// with.
pub const STRAY_BPS: u128 = 25;

/// Whether Orca's program is on the cluster.
pub async fn program_there(client: &SolanaClient) -> Result<bool> {
    Ok(client
        .account(&venue::WHIRLPOOL_PROGRAM)
        .await?
        .is_some_and(|account| account.executable))
}

/// How long an ELF file is by its own headers: to the end of its section
/// headers or of its last segment, whichever is further. A program dumped
/// from a cluster is its whole data account, padded with zeros well past it.
pub fn elf_len(bytes: &[u8]) -> Result<usize> {
    let read = |at: usize, size: usize| -> Result<u64> {
        let field = bytes
            .get(at..at + size)
            .ok_or_else(|| anyhow!("the ELF header is cut short"))?;
        let mut word = [0u8; 8];
        word[..size].copy_from_slice(field);
        Ok(u64::from_le_bytes(word))
    };
    if !bytes.starts_with(b"\x7fELF\x02") {
        bail!("not a 64-bit ELF file");
    }
    let sections = read(0x28, 8)? + read(0x3A, 2)? * read(0x3C, 2)?;
    let (headers, size, count) = (read(0x20, 8)?, read(0x36, 2)?, read(0x38, 2)?);
    let mut end = sections;
    for k in 0..count {
        let at = (headers + k * size) as usize;
        end = end.max(read(at + 8, 8)? + read(at + 32, 8)?);
    }
    let end = end as usize;
    if end > bytes.len() {
        bail!("the ELF file is cut short");
    }
    Ok(end)
}

/// The upgradeable loader's two accounts for Orca's program: the program,
/// naming its data, and the data (the ELF file, without the padding a dump
/// carries), its upgrade authority an address nobody here holds, as Orca's
/// own is on every cluster.
pub async fn load_program(client: &SolanaClient, elf: &[u8]) -> Result<()> {
    let elf = &elf[..elf_len(elf)?];
    let loader = anchor_lang::solana_program::bpf_loader_upgradeable::ID;
    let data_address = program_data_address(&venue::WHIRLPOOL_PROGRAM);
    let nobody =
        Pubkey::new_from_array(Sha256::digest(b"shonin-local:orca-upgrade-authority").into());
    let mut program_data = vec![3, 0, 0, 0];
    program_data.extend_from_slice(&0u64.to_le_bytes());
    program_data.push(1);
    program_data.extend_from_slice(nobody.as_ref());
    program_data.extend_from_slice(elf);
    let mut program = vec![2, 0, 0, 0];
    program.extend_from_slice(data_address.as_ref());
    for (address, data, executable) in [
        (data_address, program_data, false),
        (venue::WHIRLPOOL_PROGRAM, program, true),
    ] {
        let lamports = client.rpc.rent_exempt_minimum(data.len()).await?;
        client
            .rpc
            .set_account(
                &address,
                &Account {
                    lamports,
                    owner: loader,
                    executable,
                    data,
                },
            )
            .await
            .with_context(|| format!("writing {address}"))?;
    }
    Ok(())
}

/// An account written as given, rent-exempt.
pub async fn put(
    client: &SolanaClient,
    address: &Pubkey,
    owner: Pubkey,
    data: Vec<u8>,
) -> Result<()> {
    let lamports = client.rpc.rent_exempt_minimum(data.len()).await?;
    client
        .rpc
        .set_account(
            address,
            &Account {
                lamports,
                owner,
                executable: false,
                data,
            },
        )
        .await
        .with_context(|| format!("writing {address}"))
}

/// Orca's settings account: its three authorities `authority`, Orca's share
/// of each fee, and no feature flags.
pub fn config_data(authority: &Pubkey) -> Vec<u8> {
    let mut data = venue::discriminator("account", "WhirlpoolsConfig").to_vec();
    for _ in 0..3 {
        data.extend_from_slice(authority.as_ref());
    }
    data.extend_from_slice(&PROTOCOL_FEE_RATE.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data
}

/// The fee tier at `tick_spacing` of the listed settings.
pub fn fee_tier_data(tick_spacing: u16, fee_rate: u16) -> Vec<u8> {
    let mut data = venue::discriminator("account", "FeeTier").to_vec();
    data.extend_from_slice(venue::CONFIG.as_ref());
    data.extend_from_slice(&tick_spacing.to_le_bytes());
    data.extend_from_slice(&fee_rate.to_le_bytes());
    data
}

/// Pyth's `PriceUpdateV2` for SOL/USD as its receiver lays it out: `price`
/// and `confidence` in its units (eight decimals), fully verified, published
/// at `published`.
pub fn price_update(price: i64, confidence: u64, published: i64, slot: u64) -> Vec<u8> {
    let mut data = venue::discriminator("account", "PriceUpdateV2").to_vec();
    data.extend_from_slice(venue::PRICE_ACCOUNT.as_ref());
    data.push(1);
    data.extend_from_slice(&venue::FEED_ID);
    data.extend_from_slice(&price.to_le_bytes());
    data.extend_from_slice(&confidence.to_le_bytes());
    data.extend_from_slice(&(-8i32).to_le_bytes());
    data.extend_from_slice(&published.to_le_bytes());
    data.extend_from_slice(&(published - 1).to_le_bytes());
    data.extend_from_slice(&price.to_le_bytes());
    data.extend_from_slice(&confidence.to_le_bytes());
    data.extend_from_slice(&slot.to_le_bytes());
    data.resize(134, 0);
    data
}

/// The stand-in price account at `price` (as the chain crate counts prices),
/// published at `published`, a hundredth of a percent sure.
pub async fn post_price(client: &SolanaClient, price: u128, published: u64) -> Result<()> {
    // Pyth's eight decimals.
    let units = (price / (PRICE_ONE / 100_000_000)).min(i64::MAX as u128) as i64;
    let slot = client.rpc.slot().await?;
    let data = price_update(
        units,
        (units / 10_000).max(1) as u64,
        published as i64,
        slot,
    );
    put(client, &venue::PRICE_ACCOUNT, venue::PYTH_RECEIVER, data).await
}

/// After the clock's jump to `at`: the stand-in price set to the pool's own,
/// published then. Nothing on a cluster without the pool.
pub async fn price_follows_pool(client: &SolanaClient, at: u64) -> Result<()> {
    let address = venue::POOLS[0].address;
    let Some(account) = client.account(&address).await? else {
        return Ok(());
    };
    let pool = venue::read_pool(&address, &account)?;
    post_price(client, venue::price_of_sqrt(pool.sqrt_price), at).await
}

/// The listed pool, created at `price` if it isn't there, `payer` paying.
pub async fn ensure_pool(client: &SolanaClient, payer: &Keypair, price: u128) -> Result<PoolNow> {
    let listed = venue::POOLS[0];
    if client.account(&listed.address).await?.is_none() {
        let vaults = (Keypair::new(), Keypair::new());
        let ix = venue::ix_initialize_pool(
            &payer.pubkey(),
            listed.tick_spacing,
            venue::sqrt_of_price(price),
            (vaults.0.pubkey(), vaults.1.pubkey()),
        );
        client
            .send(vec![ix], &[payer, &vaults.0, &vaults.1])
            .await
            .context("creating Orca's pool")?;
        tracing::info!(pool = %listed.address, price = price as f64 / PRICE_ONE as f64, "Orca's pool is created");
    }
    pool_now(client).await
}

pub async fn pool_now(client: &SolanaClient) -> Result<PoolNow> {
    let address = venue::POOLS[0].address;
    let account = client
        .account(&address)
        .await?
        .ok_or_else(|| anyhow!("Orca's pool {address} is not there"))?;
    Ok(PoolNow {
        address,
        state: venue::read_pool(&address, &account)?,
    })
}

/// The tick arrays `each_side` either side of the one holding the pool's
/// price, and that one, made where they aren't there.
pub async fn ensure_arrays_around(
    client: &SolanaClient,
    payer: &Keypair,
    pool: &PoolNow,
    each_side: i32,
) -> Result<()> {
    let spacing = pool.state.tick_spacing;
    let span = venue::TICKS_PER_ARRAY * spacing as i32;
    let base = venue::array_start(pool.state.tick_current, spacing);
    let starts: Vec<i32> = (-each_side..=each_side).map(|k| base + k * span).collect();
    ensure_arrays(client, payer, pool, &starts).await
}

pub async fn ensure_arrays(
    client: &SolanaClient,
    payer: &Keypair,
    pool: &PoolNow,
    starts: &[i32],
) -> Result<()> {
    let starts: Vec<i32> = starts
        .iter()
        .copied()
        .filter(|start| venue::array_in_range(*start, pool.state.tick_spacing))
        .collect();
    let addresses: Vec<Pubkey> = starts
        .iter()
        .map(|start| venue::tick_array_address(&pool.address, *start))
        .collect();
    let found = client.rpc.accounts(&addresses).await?;
    let missing: Vec<Instruction> = starts
        .iter()
        .zip(found)
        .filter(|(_, account)| account.is_none())
        .map(|(start, _)| venue::ix_initialize_tick_array(&pool.address, &payer.pubkey(), *start))
        .collect();
    // A few to a transaction: each names the payer, the pool and its own.
    for chunk in missing.chunks(4) {
        client
            .send(chunk.to_vec(), &[payer])
            .await
            .context("making the pool's tick arrays")?;
    }
    Ok(())
}

/// `who`'s SOL wrapped, up to `lamports` more.
pub fn ixs_wrap(who: &Pubkey, lamports: u64) -> Result<Vec<Instruction>> {
    let account = venue::wsol_of(who);
    Ok(vec![
        venue::ix_open_wsol(who, who),
        anchor_lang::solana_program::system_instruction::transfer(who, &account, lamports),
        spl_token::instruction::sync_native(&spl_token::ID, &account)?,
    ])
}

/// The depth the operator's seed must give, and how much SOL that takes.
///
/// A station's close sells its SOL into everyone else's liquidity no further
/// than the exit band `b` from Pyth's price. The rule: what others' liquidity
/// takes inside the band is at least three times the most SOL the station
/// can hold, times `2R/b` (the agent's band of `±R` crosses the exit band
/// `2R/b` times over, each time its whole SOL changing hands). A station of
/// `deployed` dollars in a band of `±R` holds the most SOL at its band's
/// foot: `deployed / (P(1 - R))`.
///
/// A position of `S` SOL and as many dollars between `P(1 - w)` and
/// `P(1 + w)` takes, while the price falls by `b`, the share
/// `(1/sqrt(1 - b) - 1) / (1 - sqrt(1 - w))` of its dollars' worth in SOL.
///
/// On devnet, with Pyth at about $110 (9 October 2026), a $100 demo deposit
/// at 80% deployed in the agent's ±4% band (R = 0.04) and the 1% exit band:
/// the most SOL is 80 / 106.2 = 0.75, the rule asks 3 × 0.75 × 8 = 18.1 SOL
/// taken inside the band. At w = 2% a seed takes 0.501 of its SOL inside the
/// band, so it needs 36.1 SOL: [`DEVNET_SEED_SOL`] is 40. No seed of 10 SOL
/// meets it at any width that still holds the band (w >= b: at most 10.05
/// SOL taken).
pub fn in_band_share(half_width: f64, band: f64) -> f64 {
    (1.0 / (1.0 - band).sqrt() - 1.0) / (1.0 - (1.0 - half_width).sqrt())
}

/// What others' liquidity must take inside the band, in SOL, for a station
/// of `deployed` dollars in a band of `±range` at `price` (see
/// [`in_band_share`]).
pub fn depth_needed(deployed: f64, price: f64, range: f64, band: f64) -> f64 {
    let most_sol = deployed / (price * (1.0 - range));
    3.0 * most_sol * 2.0 * range / band
}

/// The SOL a seed `half_width` either side needs to meet [`depth_needed`].
pub fn seed_sol_for(deployed: f64, price: f64, range: f64, band: f64, half_width: f64) -> f64 {
    depth_needed(deployed, price, range, band) / in_band_share(half_width, band)
}

/// The operator's seed: this far either side of Pyth's price, in basis
/// points (twice the exit band, so the band around Pyth stays inside it
/// while Pyth stays inside its inner half, see [`needs_recentring`]).
pub const SEED_HALF_WIDTH_BPS: u128 = 200;
/// The SOL the devnet seed holds, with as many dollars (see
/// [`in_band_share`]).
pub const DEVNET_SEED_SOL: u64 = 40 * LAMPORTS;

/// The mint of the operator's seed position: derived from the operator's
/// own key, so only the operator can open it, and the set-up and the market
/// operator always find the same one (Orca closes the mint with the
/// position, so a seed re-centred opens at it again).
pub fn seed_mint(operator: &Keypair) -> Keypair {
    let digest = Sha256::digest([&operator.to_bytes()[..], b"shonin:orca-seed-position"].concat());
    solana_keypair::keypair_from_seed(&digest).expect("a 32-byte seed makes a keypair")
}

/// The operator's seed position, if it is open.
pub async fn seed_position(
    client: &SolanaClient,
    operator: &Keypair,
) -> Result<Option<venue::Position>> {
    let address = venue::position_address(&seed_mint(operator).pubkey());
    match client.account(&address).await? {
        Some(account) => Ok(Some(venue::read_position(&address, &account)?)),
        None => Ok(None),
    }
}

/// Whether Pyth's `price` has left the inner half of the seed's range (the
/// middle half of its ticks), so the seed is to be set again around it.
pub fn needs_recentring(seed: &venue::Position, price: u128) -> bool {
    let quarter = (seed.tick_upper - seed.tick_lower) / 4;
    let tick = venue::tick_at_or_below(venue::sqrt_of_price(price));
    tick < seed.tick_lower + quarter || tick >= seed.tick_upper - quarter
}

/// The operator's seed opened across the pool's price, `half_width_bps`
/// either side, of `sol` lamports and the dollars that go with them: its
/// own wrapped SOL used first and only the rest wrapped, the dollars short
/// minted by the admin. A zero-liquidity position resumes its funding; a
/// funded seed is left alone.
pub async fn seed(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
    sol: u64,
    half_width_bps: u128,
) -> Result<()> {
    let existing = seed_position(client, operator).await?;
    if existing
        .as_ref()
        .is_some_and(|position| position.liquidity > 0)
    {
        return Ok(());
    }
    anyhow::ensure!(sol > 0, "the seed needs a positive SOL amount");
    let pool = pool_now(client).await?;
    let price = venue::price_of_sqrt(pool.state.sqrt_price);
    let ticks = match &existing {
        Some(position) => (position.tick_lower, position.tick_upper),
        None => venue::ticks_between(
            price * (10_000 - half_width_bps) / 10_000,
            price * (10_000 + half_width_bps) / 10_000,
            pool.state.tick_spacing,
        )?,
    };
    ensure_arrays(
        client,
        operator,
        &pool,
        &[ticks.0, ticks.1].map(|tick| venue::array_start(tick, pool.state.tick_spacing)),
    )
    .await?;
    let who = operator.pubkey();
    // As many dollars as the SOL is worth, and a tenth more.
    let dollars = (sol as u128 * price / PRICE_ONE / 1_000 * 11 / 10) as u64;
    let wrapped = client.token_balance(&venue::wsol_of(&who)).await?;
    if wrapped < sol {
        client
            .send(ixs_wrap(&who, sol - wrapped)?, &[operator])
            .await
            .context("wrapping the operator's SOL")?;
    }
    let short = dollars.saturating_sub(client.token_balance(&platform.dollars(&who)).await?);
    if short > 0 {
        client
            .send(
                vec![platform.ix_mint_to(&admin.pubkey(), &who, short)?],
                &[admin],
            )
            .await
            .context("minting the operator's dollars")?;
    }
    let mint = seed_mint(operator);
    if existing.is_none() {
        let open = venue::ix_open_position(&who, &who, &mint.pubkey(), &pool.address, ticks);
        client
            .send(vec![open], &[operator, &mint])
            .await
            .context("opening the operator's position")?;
    }
    let (max_a, max_b) = if venue::asset_is_a() {
        (sol, dollars)
    } else {
        (dollars, sol)
    };
    let liquidity = venue::liquidity_for(
        pool.state.tick_current,
        pool.state.sqrt_price,
        ticks,
        max_a,
        max_b,
    )?;
    anyhow::ensure!(liquidity > 0, "the offered seed buys no liquidity");
    let position = existing.unwrap_or(venue::Position {
        whirlpool: pool.address,
        mint: mint.pubkey(),
        liquidity: 0,
        tick_lower: ticks.0,
        tick_upper: ticks.1,
        checkpoint_a: 0,
        fee_owed_a: 0,
        checkpoint_b: 0,
        fee_owed_b: 0,
    });
    let add = venue::ix_modify(
        Change::Increase,
        &who,
        &pool,
        &position,
        &mint.pubkey(),
        venue::owners(&who, &platform.dollars(&who)),
        liquidity,
        (max_a, max_b),
    );
    client
        .send(vec![add], &[operator])
        .await
        .context("funding the operator's position")?;
    tracing::info!(
        sol = sol as f64 / LAMPORTS as f64,
        dollars = dollars as f64 / 1e6,
        "the operator gave the pool its depth"
    );
    Ok(())
}

/// The operator's seed taken out whole, its fees collected and the position
/// closed (its rent back to the operator). Nothing if it isn't open.
pub async fn withdraw_seed(
    client: &SolanaClient,
    platform: &Platform,
    operator: &Keypair,
) -> Result<()> {
    let Some(position) = seed_position(client, operator).await? else {
        return Ok(());
    };
    let pool = pool_now(client).await?;
    let who = operator.pubkey();
    let mint = seed_mint(operator).pubkey();
    let owners = venue::owners(&who, &platform.dollars(&who));
    let mut ixs = vec![venue::ix_open_wsol(&who, &who)];
    if position.liquidity > 0 {
        ixs.push(venue::ix_modify(
            Change::Decrease,
            &who,
            &pool,
            &position,
            &mint,
            owners,
            position.liquidity,
            (0, 0),
        ));
    }
    ixs.push(venue::ix_collect_fees(&who, &pool, &mint, owners));
    ixs.push(venue::ix_close_position(&who, &who, &mint));
    client
        .send(ixs, &[operator])
        .await
        .context("taking the operator's seed out")?;
    Ok(())
}

/// The SOL a seed holding `position` is set again with: half its worth at
/// the pool's square-root price `sqrt_price` (`price` a price here), in SOL.
pub fn seed_size(position: &venue::Position, sqrt_price: u128, price: u128) -> Result<u64> {
    let (a, b) = venue::holds(position, sqrt_price)?;
    let (sol, dollars) = if venue::asset_is_a() { (a, b) } else { (b, a) };
    // Dollar-units to lamports at `price`: d * 10^3 * PRICE_ONE / price.
    let as_sol = (dollars as u128 * 1_000 * PRICE_ONE)
        .checked_div(price)
        .unwrap_or(0);
    Ok(((sol as u128 + as_sol) / 2).min(u64::MAX as u128) as u64)
}

/// The market operator's part at Orca: the pool brought to `target`, and
/// the operator's seed set again around it once `target` has left the
/// seed's inner half, with `sol` or, without, as much as it held; or opened
/// with `sol` if it isn't open. Says the pool's price after.
pub async fn keep_seed_at(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
    target: u128,
    sol: Option<u64>,
) -> Result<u128> {
    // The pool first: a seed opens across the pool's own price.
    let steered = steer(client, platform, admin, operator, target).await;
    match seed_position(client, operator).await? {
        Some(position) if position.liquidity == 0 => {
            let Some(sol) = sol else {
                bail!("the operator's seed is unfunded; run setup to restore its configured size");
            };
            seed(client, platform, admin, operator, sol, SEED_HALF_WIDTH_BPS).await?;
            steer(client, platform, admin, operator, target).await?;
            if needs_recentring(&position, target) {
                withdraw_seed(client, platform, operator).await?;
                steer(client, platform, admin, operator, target).await?;
                seed(client, platform, admin, operator, sol, SEED_HALF_WIDTH_BPS).await?;
            }
        }
        Some(position) if !needs_recentring(&position, target) => {
            steered?;
        }
        Some(position) => {
            let pool = pool_now(client).await?;
            let sol = match sol {
                Some(sol) => sol,
                None => seed_size(
                    &position,
                    pool.state.sqrt_price,
                    venue::price_of_sqrt(pool.state.sqrt_price),
                )?,
            };
            withdraw_seed(client, platform, operator).await?;
            steer(client, platform, admin, operator, target).await?;
            seed(client, platform, admin, operator, sol, SEED_HALF_WIDTH_BPS).await?;
            tracing::info!(
                target = target as f64 / PRICE_ONE as f64,
                "the operator's seed is set again around the price"
            );
        }
        None => {
            let Some(sol) = sol else {
                bail!("the operator has no seed to keep");
            };
            seed(client, platform, admin, operator, sol, SEED_HALF_WIDTH_BPS).await?;
            // A pool nobody's liquidity could move is moved with the seed's,
            // then the seed is set again around the price.
            if steered.is_err() {
                steer(client, platform, admin, operator, target).await?;
                withdraw_seed(client, platform, operator).await?;
                steer(client, platform, admin, operator, target).await?;
                seed(client, platform, admin, operator, sol, SEED_HALF_WIDTH_BPS).await?;
            }
        }
    }
    let after = pool_now(client).await?;
    Ok(venue::price_of_sqrt(after.state.sqrt_price))
}

/// The operator brings the pool's price to `target` (as the chain crate
/// counts prices), buying or selling SOL no further than it, when the pool
/// has strayed more than [`STRAY_BPS`] from it: dollars minted by the admin
/// and SOL wrapped as it needs them, and the tick arrays the swap crosses
/// made. Says the pool's price after.
pub async fn steer(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
    target: u128,
) -> Result<u128> {
    let pool = pool_now(client).await?;
    let now = venue::price_of_sqrt(pool.state.sqrt_price);
    if target == 0 || now.abs_diff(target) * 10_000 <= target * STRAY_BPS {
        return Ok(now);
    }
    let buying = now < target;
    let a_to_b = buying != venue::asset_is_a();
    let who = operator.pubkey();
    let dollars_account = platform.dollars(&who);
    let mut ready = Vec::new();
    // What it could need to move the pool there: its whole depth's worth,
    // bounded by what it has.
    let amount = if buying {
        let have = client.token_balance(&dollars_account).await?;
        let want = 1_000_000_000_000u64;
        if have < want / 2 {
            ready.push(platform.ix_mint_to(&admin.pubkey(), &who, want - have)?);
        }
        want.max(have)
    } else {
        let wrapped = client.token_balance(&venue::wsol_of(&who)).await?;
        let spare = client.lamports(&who).await?.saturating_sub(OPERATOR_KEEPS);
        if spare > 0 {
            ready.extend(ixs_wrap(&who, spare)?);
        }
        wrapped + spare
    };
    if amount == 0 {
        bail!("the operator has nothing to move the pool with");
    }
    if !ready.is_empty() {
        let mut signers = vec![operator];
        if buying {
            signers.push(admin);
        }
        // The operator pays; the admin signs its mint.
        client
            .send(ready, &signers)
            .await
            .context("readying the operator's tokens")?;
    }
    ensure_arrays(
        client,
        operator,
        &pool,
        &venue::swap_starts(&pool.state, a_to_b),
    )
    .await?;
    let swap = venue::ix_swap(
        &who,
        &pool,
        venue::owners(&who, &dollars_account),
        amount,
        0,
        venue::sqrt_of_price(target),
        a_to_b,
    );
    client
        .send(vec![swap], &[operator])
        .await
        .context("moving the pool")?;
    let after = pool_now(client).await?;
    Ok(venue::price_of_sqrt(after.state.sqrt_price))
}

/// Pyth's SOL/USD price as the cluster holds it, warned about when it is
/// older than the driver takes.
pub async fn pyth_now(client: &SolanaClient) -> Result<u128> {
    let account = client
        .account(&venue::PRICE_ACCOUNT)
        .await?
        .ok_or_else(|| anyhow!("Pyth's SOL/USD account is not on {}", client.key))?;
    let price = venue::read_price(&account)?;
    let age = client
        .now()
        .await?
        .saturating_sub(price.publish_time.max(0) as u64);
    if age > venue::MAX_AGE {
        tracing::warn!(
            age,
            "Pyth's SOL/USD price is older than the driver takes; it is used all the same"
        );
    }
    let exponent = price.exponent + 12;
    let ten = 10u128.pow(exponent.unsigned_abs());
    Ok(if exponent >= 0 {
        price.price as u128 * ten
    } else {
        price.price as u128 / ten
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(tick_lower: i32, tick_upper: i32) -> venue::Position {
        venue::Position {
            whirlpool: venue::POOLS[0].address,
            mint: Pubkey::new_unique(),
            liquidity: 1,
            tick_lower,
            tick_upper,
            checkpoint_a: 0,
            fee_owed_a: 0,
            checkpoint_b: 0,
            fee_owed_b: 0,
        }
    }

    #[test]
    fn the_devnet_seed_gives_the_depth_a_demo_vault_s_closes_need() {
        // A $100 deposit, 80% in the agent's ±4% band, SOL at $110.6, the
        // 1% exit band.
        let needed = depth_needed(80.0, 110.6, 0.04, 0.01);
        assert!((18.0..18.2).contains(&needed), "{needed}");
        let share = in_band_share(0.02, 0.01);
        assert!((0.50..0.502).contains(&share), "{share}");
        let sol = seed_sol_for(80.0, 110.6, 0.04, 0.01, 0.02);
        assert!(sol < DEVNET_SEED_SOL as f64 / LAMPORTS as f64, "{sol}");
        assert_eq!(SEED_HALF_WIDTH_BPS, 200);
        // Ten SOL is short at any width that still holds the band.
        assert!(10.0 * in_band_share(0.01, 0.01) < needed);
        // A tenth of the price, ten times the depth.
        assert!((depth_needed(80.0, 11.06, 0.04, 0.01) / needed - 10.0).abs() < 1e-9);
    }

    #[test]
    fn the_seed_is_set_again_once_the_price_leaves_its_inner_half() {
        let price = 150 * PRICE_ONE;
        let ticks = venue::ticks_between(price * 98 / 100, price * 102 / 100, 64).unwrap();
        let seed = position(ticks.0, ticks.1);
        assert!(!needs_recentring(&seed, price));
        assert!(!needs_recentring(&seed, price * 1_005 / 1_000));
        assert!(needs_recentring(&seed, price * 1_012 / 1_000));
        assert!(needs_recentring(&seed, price * 988 / 1_000));
        assert!(needs_recentring(&seed, price * 2));
    }

    #[test]
    fn a_seed_set_again_holds_as_much_as_it_did() {
        let price = 150 * PRICE_ONE;
        let s = venue::sqrt_of_price(price);
        let tick = venue::tick_at_or_below(s);
        let ticks = venue::ticks_between(price * 98 / 100, price * 102 / 100, 64).unwrap();
        // 40 SOL and $6,000.
        let liquidity = venue::liquidity_for(tick, s, ticks, 40 * LAMPORTS, 6_000_000_000).unwrap();
        let mut seed = position(ticks.0, ticks.1);
        seed.liquidity = liquidity;
        let size = seed_size(&seed, s, price).unwrap();
        // Its worth, half of it in SOL: within the shave a liquidity takes.
        assert!(size <= 40 * LAMPORTS && size > 39 * LAMPORTS, "{size}");
    }

    #[test]
    fn the_seed_s_mint_is_the_operator_s_own_and_always_the_same() {
        let operator = Keypair::new();
        assert_eq!(seed_mint(&operator).pubkey(), seed_mint(&operator).pubkey());
        assert_ne!(
            seed_mint(&operator).pubkey(),
            seed_mint(&Keypair::new()).pubkey()
        );
        assert_ne!(seed_mint(&operator).pubkey(), operator.pubkey());
    }

    #[test]
    fn a_dumped_program_is_loaded_without_its_padding_and_nothing_of_its_own_is_cut() {
        // A 64-bit ELF of two segments, its section headers last, padded.
        let mut elf = vec![0u8; 0x300];
        elf[..5].copy_from_slice(b"\x7fELF\x02");
        elf[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes());
        elf[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        elf[0x38..0x3A].copy_from_slice(&2u16.to_le_bytes());
        for (k, (offset, size)) in [(0x100u64, 0x20u64), (0x120, 0x30)].into_iter().enumerate() {
            let at = 0x40 + k * 56;
            elf[at + 8..at + 16].copy_from_slice(&offset.to_le_bytes());
            elf[at + 32..at + 40].copy_from_slice(&size.to_le_bytes());
        }
        elf[0x28..0x30].copy_from_slice(&0x150u64.to_le_bytes());
        elf[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
        elf[0x3C..0x3E].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            elf_len(&elf).unwrap(),
            0x190,
            "to the end of its section headers"
        );
        // Its last segment further out than its section headers.
        elf[0x40 + 56 + 32..0x40 + 56 + 40].copy_from_slice(&0x100u64.to_le_bytes());
        assert_eq!(
            elf_len(&elf).unwrap(),
            0x220,
            "to the end of its last segment"
        );
        assert!(
            elf_len(&elf[..0x100]).is_err(),
            "a file cut short is refused"
        );
        assert!(elf_len(b"not an elf").is_err());
    }
}
