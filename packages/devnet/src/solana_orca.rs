//! Orca on a Solana cluster, as the Orca driver built for that cluster lists
//! it: its pool of wrapped SOL against the platform's dollar, at the price
//! Pyth gives, with depth from the market operator.
//!
//! **Locally** nothing of Orca's is there, and nothing of Pyth's: Orca's
//! program is put at its own address from the binary the Solana tests pin
//! (`.shonin/fixtures/orca_whirlpool.so`, checked against
//! `contracts/solana/tests/fixtures/programs.txt`), under the upgradeable
//! loader as on every cluster; its settings account and the fee tier are
//! written as Orca lays them out, its fee authorities the operator's (only
//! Orca's own keys could create them on a public cluster); and Pyth's SOL/USD
//! account is a stand-in at its sponsored address, posted by the clock's
//! driver at every jump from a tape of its own and published at the new
//! time, as Pyth posts SOL's price; the market operator then brings the pool
//! to it, as arbitrage would. The pool itself, its tick arrays and the
//! operator's seed are made by Orca's program.
//!
//! **On devnet** Orca's program, its settings and the fee tier are Orca's own,
//! and Pyth posts the price: [`set_up_devnet`] creates the pool the driver
//! lists at Pyth's price, its tick arrays around it, and seeds it from the
//! operator at a position only the operator can open; [`keep_with_pyth`]
//! brings the pool back to Pyth's price whenever it has strayed and sets the
//! seed again around it once Pyth leaves its inner half, as the market
//! operator's part of the testnet market. Every step looks at the cluster first, so each is safe to run
//! again.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::orca::setup::{
    FEE_RATE, OPERATOR_KEEPS, PRICE_ONE, config_data, ensure_arrays_around, ensure_pool,
    fee_tier_data, keep_seed_at, load_program, pool_now, post_price, program_there, put, pyth_now,
    seed_position,
};
use chain::solana::orca::{self as venue};
use chain::solana::platform::Platform;
use chain::solana::{Keypair, Signer, SolanaClient};
use sha2::{Digest, Sha256};

/// The Solana tests' pins of the third-party programs they load: the local
/// cluster loads the same binary.
const PINNED: &str = include_str!("../../../contracts/solana/tests/fixtures/programs.txt");
const ORCA_FILE: &str = "orca_whirlpool.so";

/// SOL's price on a local cluster at the start: $150, as the driver's tests
/// price it.
pub const LOCAL_START: u128 = 150 * PRICE_ONE;
pub use chain::solana::orca::setup::LAMPORTS;

/// The operator's seed on a local cluster: the depth the default demo
/// deposit's closes need (see `in_band_share` in the chain crate's Orca
/// set-up): $25,000 at 80% deployed and $150 a SOL is at most 139 SOL held,
/// so 3 × 139 × 8 = 3,333 SOL taken inside the band, which a seed ±2% wide
/// gives with 6,650 SOL.
const LOCAL_SEED_SOL: u64 = 8_000 * LAMPORTS;
/// The tick arrays made either side of the price's own at the set-up.
const LOCAL_ARRAYS_EACH_SIDE: i32 = 3;
const DEVNET_ARRAYS_EACH_SIDE: i32 = 2;

// Local -----------------------------------------------------------------------

/// Orca's program, settings and fee tier, the stand-in price, the pool, its
/// tick arrays and the operator's depth on a local cluster. Skipped, with a
/// warning, when the pinned binary isn't there: the stand-in venue serves
/// the cluster all the same.
pub async fn set_up_local(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
    payer: &Keypair,
    binary: &Path,
) -> Result<()> {
    if !program_there(client).await? {
        let Some(bytes) = pinned_binary(binary)? else {
            tracing::warn!(
                file = %binary.display(),
                "Orca's program is not here: run `task contracts:solana:fixtures` to fetch it; the cluster has no Orca until then"
            );
            return Ok(());
        };
        load_program(client, &bytes).await?;
        tracing::info!(program = %venue::WHIRLPOOL_PROGRAM, "Orca's program is loaded at its own address");
    }
    if client.account(&venue::CONFIG).await?.is_none() {
        put(
            client,
            &venue::CONFIG,
            venue::WHIRLPOOL_PROGRAM,
            config_data(&operator.pubkey()),
        )
        .await?;
    }
    let spacing = venue::POOLS[0].tick_spacing;
    let tier = venue::fee_tier_address(&venue::CONFIG, spacing);
    if client.account(&tier).await?.is_none() {
        put(
            client,
            &tier,
            venue::WHIRLPOOL_PROGRAM,
            fee_tier_data(spacing, FEE_RATE),
        )
        .await?;
    }
    let pool = ensure_pool(client, payer, LOCAL_START).await?;
    // The stand-in's own price: the market operator posts it from then on.
    let price = match client.account(&venue::PRICE_ACCOUNT).await? {
        Some(account) => venue::read_price(&account)
            .ok()
            .and_then(|price| {
                let exponent = price.exponent + venue::PRICE_DECIMALS as i32;
                (exponent <= 0).then(|| price.price as u128 / 10u128.pow(exponent.unsigned_abs()))
            })
            .unwrap_or(LOCAL_START),
        None => LOCAL_START,
    };
    post_price(client, price, client.now().await?).await?;
    ensure_arrays_around(client, payer, &pool, LOCAL_ARRAYS_EACH_SIDE).await?;
    if seed_position(client, operator).await?.is_none()
        && client.lamports(&operator.pubkey()).await? < LOCAL_SEED_SOL + 10 * LAMPORTS
    {
        client
            .airdrop(&operator.pubkey(), LOCAL_SEED_SOL + 10 * LAMPORTS)
            .await?;
    }
    keep_seed_at(
        client,
        platform,
        admin,
        operator,
        price,
        Some(LOCAL_SEED_SOL),
    )
    .await?;
    tracing::info!(pool = %pool.address, "Orca's pool is open, with the operator's depth");
    Ok(())
}

/// The pinned binary's bytes, checked against its pin; `None` if the file
/// isn't there. One that is there and isn't the pinned binary is refused.
fn pinned_binary(path: &Path) -> Result<Option<Vec<u8>>> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(None);
    };
    let pin = PINNED
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&ORCA_FILE))
        .and_then(|fields| fields.get(2).map(|sha| sha.to_string()))
        .ok_or_else(|| anyhow!("{ORCA_FILE} has no pin in the Solana tests' fixtures"))?;
    let got = alloy::hex::encode(Sha256::digest(&bytes));
    if got != pin {
        bail!(
            "{} is not the pinned binary (its hash is {got}, the pin {pin}): fetch it again with `task contracts:solana:fixtures`",
            path.display()
        );
    }
    Ok(Some(bytes))
}

// Both clusters -------------------------------------------------------------------

// Devnet ----------------------------------------------------------------------------

/// The pool the driver lists on devnet, created at Pyth's price if it isn't
/// there, the tick arrays around its price, and the operator's position of
/// `seed_sol` lamports (moved to it from the deployer as it needs) with the
/// dollars that go with them, once. Refuses unless the cluster is the one
/// the driver was built for: Orca's settings and fee tier and Pyth's price
/// all there. Nothing on a cluster whose dollar the build's driver doesn't
/// list.
pub async fn set_up_devnet(
    client: &SolanaClient,
    platform: &Platform,
    deployer: &Keypair,
    admin: &Keypair,
    operator: &Keypair,
    seed_sol: u64,
) -> Result<()> {
    // Only a build for this network lists this cluster's dollar; any other
    // was refused already unless the programs stand in for the network's
    // (a test's cluster), where Orca's pool has nothing to list.
    if platform.mint != venue::DOLLAR_MINT {
        tracing::warn!(
            listed = %venue::DOLLAR_MINT,
            dollar = %platform.mint,
            "this build's Orca driver lists another dollar than this cluster's: Orca's pool is not set up"
        );
        return Ok(());
    }
    if !program_there(client).await? {
        bail!("Orca's program is not on {}", client.key);
    }
    let spacing = venue::POOLS[0].tick_spacing;
    for (what, address) in [
        ("Orca's settings", venue::CONFIG),
        (
            "Orca's fee tier",
            venue::fee_tier_address(&venue::CONFIG, spacing),
        ),
    ] {
        let owned = client
            .account(&address)
            .await?
            .is_some_and(|account| account.owner == venue::WHIRLPOOL_PROGRAM);
        if !owned {
            bail!("{what} ({address}) is not on {}", client.key);
        }
    }
    let oracle = pyth_now(client).await?;
    let pool = ensure_pool(client, deployer, oracle).await?;
    ensure_arrays_around(client, deployer, &pool, DEVNET_ARRAYS_EACH_SIDE).await?;
    // The operator's own seed, whatever anyone else holds in the pool:
    // at Pyth's price (the pool steered there first), and set again around
    // it if it has moved since.
    if seed_sol > 0 {
        let have = seed_position(client, operator).await?.is_some();
        let wrapped = client
            .token_balance(&venue::wsol_of(&operator.pubkey()))
            .await
            .unwrap_or(0);
        let need = (seed_sol + OPERATOR_KEEPS)
            .saturating_sub(client.lamports(&operator.pubkey()).await? + wrapped);
        if need > 0 && !have {
            client
                .send(
                    vec![chain::solana::executor::ix_fund(
                        &deployer.pubkey(),
                        &operator.pubkey(),
                        need,
                    )],
                    &[deployer],
                )
                .await
                .context("giving the operator the SOL it seeds the pool with")?;
        }
        keep_seed_at(client, platform, admin, operator, oracle, Some(seed_sol)).await?;
    }
    let after = pool_now(client).await?;
    tracing::info!(
        pool = %after.address,
        price = venue::price_of_sqrt(after.state.sqrt_price) as f64 / PRICE_ONE as f64,
        pyth = oracle as f64 / PRICE_ONE as f64,
        "Orca's pool is set up"
    );
    Ok(())
}

/// The market operator's part at Orca on devnet: the pool brought back to
/// Pyth's price when it has strayed, and the operator's seed of `seed_sol`
/// set again around it, as large as it was, once Pyth has left the seed's
/// inner half. Nothing on a cluster without the pool, or with no seed set
/// up.
pub async fn keep_with_pyth(
    client: &SolanaClient,
    platform: &Platform,
    admin: &Keypair,
    operator: &Keypair,
) -> Result<()> {
    if client.account(&venue::POOLS[0].address).await?.is_none()
        || seed_position(client, operator).await?.is_none()
    {
        return Ok(());
    }
    let target = pyth_now(client).await?;
    let price = keep_seed_at(client, platform, admin, operator, target, None).await?;
    tracing::info!(
        pool = price as f64 / PRICE_ONE as f64,
        pyth = target as f64 / PRICE_ONE as f64,
        "Orca's pool is with Pyth"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::Pubkey;
    use chain::solana::orca::setup::price_update;
    use chain::solana::rpc::Account;

    #[test]
    fn the_local_settings_and_fee_tier_are_laid_out_as_orca_reads_them() {
        let authority = Pubkey::new_unique();
        let config = config_data(&authority);
        assert_eq!(config.len(), 8 + 3 * 32 + 2 + 2);
        assert_eq!(&config[8..40], authority.as_ref());
        let tier = fee_tier_data(64, FEE_RATE);
        assert_eq!(tier.len(), 8 + 32 + 2 + 2);
        assert_eq!(&tier[8..40], venue::CONFIG.as_ref());
        assert_eq!(u16::from_le_bytes([tier[40], tier[41]]), 64);
        assert_eq!(u16::from_le_bytes([tier[42], tier[43]]), FEE_RATE);
    }

    #[test]
    fn the_stand_in_price_reads_as_the_driver_reads_pyth_s() {
        let data = price_update(150_00000000, 1_500_000, 1_800_000_000, 9);
        let account = Account {
            lamports: 1,
            owner: venue::PYTH_RECEIVER,
            executable: false,
            data,
        };
        let price = venue::read_price(&account).unwrap();
        assert_eq!(price.price, 150_00000000);
        assert_eq!(price.exponent, -8);
        assert_eq!(price.publish_time, 1_800_000_000);
        assert!(price.full);
        assert!(price.fit(1_800_000_000, venue::MAX_AGE, 100));
        // Owned by anyone but Pyth's receiver, it isn't the price at all.
        let forged = Account {
            owner: Pubkey::new_unique(),
            ..account
        };
        assert!(venue::read_price(&forged).is_err());
    }

    #[test]
    fn the_local_pool_is_the_one_the_driver_lists_and_starts_at_150() {
        assert_eq!(venue::pool_address(64).0, venue::POOLS[0].address);
        let start = venue::price_of_sqrt(venue::sqrt_of_price(LOCAL_START));
        assert!(LOCAL_START - start < PRICE_ONE / 1_000_000);
    }

    #[test]
    fn the_pinned_binary_is_named_with_its_hash() {
        let line = PINNED
            .lines()
            .find(|line| line.starts_with(ORCA_FILE))
            .expect("Orca's binary is pinned");
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields[1], venue::WHIRLPOOL_PROGRAM.to_string());
        assert_eq!(fields[2].len(), 64);
    }
}
