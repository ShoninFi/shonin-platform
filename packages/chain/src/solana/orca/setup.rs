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
const LAMPORTS: u64 = 1_000_000_000;
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

/// The operator's position across the pool's price, `half_width_bps`
/// either side, of `sol` lamports and the dollars that go with them, the
/// admin minting those.
pub async fn seed(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
    sol: u64,
    half_width_bps: u128,
) -> Result<()> {
    let pool = pool_now(client).await?;
    let price = venue::price_of_sqrt(pool.state.sqrt_price);
    let ticks = venue::ticks_between(
        price * (10_000 - half_width_bps) / 10_000,
        price * (10_000 + half_width_bps) / 10_000,
        pool.state.tick_spacing,
    )?;
    ensure_arrays(
        client,
        operator,
        &pool,
        &[ticks.0, ticks.1].map(|tick| venue::array_start(tick, pool.state.tick_spacing)),
    )
    .await?;
    // As many dollars as the SOL is worth, and a tenth more.
    let dollars = (sol as u128 * price / PRICE_ONE / 1_000 * 11 / 10) as u64;
    client
        .send(ixs_wrap(&operator.pubkey(), sol)?, &[operator])
        .await
        .context("wrapping the operator's SOL")?;
    let short = dollars.saturating_sub(
        client
            .token_balance(&platform.dollars(&operator.pubkey()))
            .await?,
    );
    if short > 0 {
        client
            .send(
                vec![platform.ix_mint_to(&admin.pubkey(), &operator.pubkey(), short)?],
                &[admin],
            )
            .await
            .context("minting the operator's dollars")?;
    }
    let mint = Keypair::new();
    let open = venue::ix_open_position(
        &operator.pubkey(),
        &operator.pubkey(),
        &mint.pubkey(),
        &pool.address,
        ticks,
    );
    client
        .send(vec![open], &[operator, &mint])
        .await
        .context("opening the operator's position")?;
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
    let position = venue::Position {
        whirlpool: pool.address,
        mint: mint.pubkey(),
        liquidity: 0,
        tick_lower: ticks.0,
        tick_upper: ticks.1,
        checkpoint_a: 0,
        fee_owed_a: 0,
        checkpoint_b: 0,
        fee_owed_b: 0,
    };
    let add = venue::ix_modify(
        Change::Increase,
        &operator.pubkey(),
        &pool,
        &position,
        &mint.pubkey(),
        venue::owners(&operator.pubkey(), &platform.dollars(&operator.pubkey())),
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
        let wrapped = client
            .token_balance(&venue::wsol_of(&who))
            .await
            .unwrap_or(0);
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
