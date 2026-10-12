//! Rewrites every station the first station program wrote in the current
//! layout, once the station program is upgraded.
//!
//! The first station program wrote each driver of a station ending at its
//! `addresses`; the current one writes two lists after them (its position
//! mints and token accounts), and reads only that layout. After the program
//! is upgraded in place, a station in the first layout is refused by every
//! instruction until `migrate_station` rewrites it. This finds every account
//! of the station program carrying the station's discriminator, reads each
//! in both layouts by the program's own rule, and sends one
//! `migrate_station` per station still in the first layout, paid by the
//! deployer. Each is read back afterwards: it must be in the current layout
//! with every value it had, its new lists empty.
//!
//! Nothing is sent when no station needs it, and nothing at all with
//! `dry_run`, which lists what would be rewritten.

use anyhow::{Context, Result, bail};
use chain::solana::platform::{ix_migrate_station, read_station};
use chain::solana::programs::anchor_lang::{AccountSerialize, Discriminator};
use chain::solana::programs::station::{
    self,
    state::{Layout, Station},
};
use chain::solana::{Pubkey, Signer, SolanaClient};

/// One account of the station program's with the station's discriminator,
/// as it reads.
enum Found {
    Current,
    First(Box<Station>, usize),
    Unreadable(String),
}

/// Every account of the station program with the station's discriminator,
/// read by the program's own rule.
async fn stations(client: &SolanaClient) -> Result<Vec<(Pubkey, Found)>> {
    let accounts = client
        .rpc
        .program_accounts(&station::ID, &[(0, Station::DISCRIMINATOR.to_vec())])
        .await
        .context("listing the station program's stations")?;
    let mut found: Vec<(Pubkey, Found)> = accounts
        .into_iter()
        .map(|(address, account)| {
            let read = match read_station(&address, &account) {
                Ok((_, Layout::Current)) => Found::Current,
                Ok((station, Layout::First)) => Found::First(Box::new(station), account.data.len()),
                Err(error) => Found::Unreadable(format!("{error:#}")),
            };
            (address, read)
        })
        .collect();
    found.sort_by_key(|(address, _)| *address);
    Ok(found)
}

/// The station's bytes in the current layout: what `migrate_station` must
/// leave in its account.
fn current_bytes(station: &Station) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    station.try_serialize(&mut bytes)?;
    Ok(bytes)
}

/// Lists, and unless `dry_run` rewrites, every station still in the first
/// layout; what it found and did, one line each.
pub async fn run(client: &SolanaClient, dry_run: bool) -> Result<String> {
    let found = stations(client).await?;
    let current = found
        .iter()
        .filter(|(_, read)| matches!(read, Found::Current))
        .count();
    let unreadable: Vec<String> = found
        .iter()
        .filter_map(|(address, read)| match read {
            Found::Unreadable(why) => Some(format!("{address}: {why}")),
            _ => None,
        })
        .collect();
    let first: Vec<(Pubkey, &Station, usize)> = found
        .iter()
        .filter_map(|(address, read)| match read {
            Found::First(station, size) => Some((*address, station.as_ref(), *size)),
            _ => None,
        })
        .collect();

    let mut said = vec![format!(
        "{} stations: {current} in the current layout, {} in the first, {} unreadable",
        found.len(),
        first.len(),
        unreadable.len()
    )];
    for (address, station, size) in &first {
        let after = current_bytes(station)?.len();
        let rent = client
            .rpc
            .rent_exempt_minimum(after)
            .await?
            .saturating_sub(client.lamports(address).await?);
        said.push(format!(
            "  {address} (vault {}, {} driver(s), {}): {size} bytes, {after} after, {rent} lamports of rent to add{}",
            station.vault,
            station.drivers.len(),
            if station.stopped {
                "stopped"
            } else {
                "running"
            },
            if dry_run { "" } else { "; rewriting" },
        ));
    }
    for line in &unreadable {
        said.push(format!("  unreadable, left alone: {line}"));
    }

    if !dry_run && !first.is_empty() {
        let deployer = &client.keys()?.deployer;
        let mut failed = Vec::new();
        for (address, station, _) in &first {
            match migrate_one(client, deployer, address, station).await {
                Ok(()) => said.push(format!("  {address}: rewritten and read back unchanged")),
                Err(error) => {
                    said.push(format!("  {address}: failed: {error:#}"));
                    failed.push(address.to_string());
                }
            }
        }
        if !failed.is_empty() {
            bail!(
                "{}\nnot rewritten: {} (is the station program upgraded? task contracts:solana:deploy NETWORK=testnet)",
                said.join("\n"),
                failed.join(", ")
            );
        }
    }
    if !unreadable.is_empty() {
        bail!(
            "{}\n{} account(s) with the station's discriminator read in neither layout",
            said.join("\n"),
            unreadable.len()
        );
    }
    Ok(said.join("\n"))
}

/// Sends `migrate_station` for one station and reads it back: in the current
/// layout, every value as it was (`before`, read from the first layout with
/// its new lists empty), at exactly the current layout's size and rent
/// exempt.
async fn migrate_one(
    client: &SolanaClient,
    deployer: &chain::solana::Keypair,
    address: &Pubkey,
    before: &Station,
) -> Result<()> {
    client
        .send(
            vec![ix_migrate_station(&deployer.pubkey(), address)],
            &[deployer],
        )
        .await
        .context("sending migrate_station")?;
    let account = client
        .account(address)
        .await?
        .with_context(|| format!("{address} is gone"))?;
    let (after, layout) = read_station(address, &account)?;
    if layout != Layout::Current {
        bail!("{address} still reads in the first layout");
    }
    if &after != before {
        bail!("{address} reads back different: {after:?}, where it was {before:?}");
    }
    if account.data != current_bytes(before)? {
        bail!("{address} holds other bytes than the current layout of what it held");
    }
    let rent = client.rpc.rent_exempt_minimum(account.data.len()).await?;
    if account.lamports < rent {
        bail!(
            "{address} holds {} lamports, short of the {rent} its size needs",
            account.lamports
        );
    }
    Ok(())
}
