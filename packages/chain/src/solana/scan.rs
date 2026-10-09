//! Finding every vault and claim the vault program holds, without asking a
//! public node for all of its accounts each time.
//!
//! Listing a program's accounts (`getProgramAccounts`) is among the dearest
//! calls a node answers, and a public endpoint limits how often it may be
//! made. Yet nothing the keeper or the executor reads changes which vaults
//! and claims exist without a count saying so: the factory counts every
//! vault it has made, and each vault counts every claim it has booked. So
//! the program is listed once, and after that each look reads the factory,
//! every vault and every claim already known in one call
//! (`getMultipleAccounts`), and lists the program again only when a count
//! has moved, or after a while in any case. A local cluster is listed at
//! every look, as before.
//!
//! A public endpoint answers each call from whichever of its nodes takes it,
//! some a slot or two behind the others. So every read is pinned to the
//! slot of the one before it (`minContextSlot`): the vaults are listed as of
//! the factory's count or later, the claims as of the vaults' counts or
//! later, and each later look as of the listing or later. No count is then
//! trusted against a listing that is older than it, and a claim a node
//! behind doesn't have yet is never taken as closed.

use std::time::{Duration, Instant};

use anchor_lang::{AccountDeserialize, Discriminator};
use anyhow::{Context, Result};
use shonin_vault::state::{self as vs, Claim, Vault};

use super::Pubkey;
use super::client::SolanaClient;
use super::rpc::Account;

/// How long a public cluster's vault program goes unlisted at most, whatever
/// the counts say.
pub const LIST_AT_LEAST_EVERY: Duration = Duration::from_secs(5 * 60);

/// What a look found: every vault, by strategy id, and every claim of each.
pub struct Found {
    pub vaults: Vec<(Pubkey, Vault)>,
    /// Each claim with its vault and its own address, by vault and id.
    pub claims: Vec<(Pubkey, Pubkey, Claim)>,
}

/// The vaults and claims known from the last listing, and the counts that
/// say whether a newer one is needed.
pub struct VaultScan {
    every: Duration,
    listed_at: Option<Instant>,
    vault_count: u64,
    vaults: Vec<Pubkey>,
    claim_total: u64,
    claims: Vec<(Pubkey, Pubkey)>,
    /// The latest slot a read was answered at: every read is answered as of
    /// it or later.
    slot: u64,
}

impl VaultScan {
    /// Listing the program at most every `every`; `Duration::ZERO` lists it
    /// at every look.
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            listed_at: None,
            vault_count: 0,
            vaults: Vec::new(),
            claim_total: 0,
            claims: Vec::new(),
            slot: 0,
        }
    }

    /// As a cluster wants it: listed at every look on a local cluster, whose
    /// node limits nothing; at most every [`LIST_AT_LEAST_EVERY`] elsewhere.
    pub fn for_cluster(cluster: &str) -> Self {
        Self::new(if cluster == "localnet" {
            Duration::ZERO
        } else {
            LIST_AT_LEAST_EVERY
        })
    }

    /// Every vault and claim as they are now.
    pub async fn look(&mut self, client: &SolanaClient) -> Result<Found> {
        if !listing_due(self.listed_at, self.every, Instant::now())
            && let Some(found) = self.read_known(client).await?
        {
            return Ok(found);
        }
        self.list(client).await
    }

    /// The program listed afresh. Each count is read before the listing it
    /// stands for, and the listing as of the count's slot or later, so the
    /// listing holds everything the count does; an account made in between
    /// moves the count again at the next look, and is listed then.
    async fn list(&mut self, client: &SolanaClient) -> Result<Found> {
        let (slot, factory) = client
            .rpc
            .accounts_since(&[vs::factory_address()], self.slot)
            .await?;
        let factory: vs::Factory = match factory.first() {
            Some(Some(account)) => decode(account).context("the vault factory")?,
            _ => anyhow::bail!("the vault factory is not set up"),
        };
        let (slot, found) = client
            .rpc
            .program_accounts_since(
                &shonin_vault::ID,
                &[(0, Vault::DISCRIMINATOR.to_vec())],
                self.slot.max(slot),
            )
            .await?;
        let mut vaults = Vec::with_capacity(found.len());
        for (address, account) in found {
            let vault: Vault = decode(&account)
                .with_context(|| format!("the vault at {address} doesn't read as one"))?;
            // Only an account at the vault's own address is the vault.
            if vs::vault_address(vault.strategy_id).0 == address {
                vaults.push((address, vault));
            }
        }
        vaults.sort_by_key(|(_, vault)| vault.strategy_id);
        let addresses: Vec<Pubkey> = vaults.iter().map(|(address, _)| *address).collect();
        let (slot, found) = client
            .rpc
            .program_accounts_since(
                &shonin_vault::ID,
                &[(0, Claim::DISCRIMINATOR.to_vec())],
                self.slot.max(slot),
            )
            .await?;
        let mut claims = Vec::new();
        for (address, account) in found {
            let claim: Claim =
                decode(&account).context("a claim account that doesn't read as one")?;
            // A claim doesn't name its vault: it is matched by address.
            if let Some(vault) = addresses
                .iter()
                .find(|vault| vs::claim_address(vault, claim.id).0 == address)
            {
                claims.push((*vault, address, claim));
            }
        }
        claims.sort_by_key(|(vault, _, claim)| (*vault, claim.id));
        self.slot = self.slot.max(slot);
        self.listed_at = Some(Instant::now());
        self.vault_count = factory.vault_count;
        self.vaults = addresses;
        self.claim_total = claim_total(&vaults);
        self.claims = claims
            .iter()
            .map(|(vault, address, _)| (*vault, *address))
            .collect();
        Ok(Found { vaults, claims })
    }

    /// The factory, the vaults and the claims known, read in one call; `None`
    /// when a count has moved since the listing, so the program must be
    /// listed again.
    async fn read_known(&mut self, client: &SolanaClient) -> Result<Option<Found>> {
        let mut addresses = vec![vs::factory_address()];
        addresses.extend(&self.vaults);
        addresses.extend(self.claims.iter().map(|(_, claim)| *claim));
        let (slot, accounts) = client.rpc.accounts_since(&addresses, self.slot).await?;
        self.slot = self.slot.max(slot);
        let (factory, rest) = accounts
            .split_first()
            .context("the node answered no account at all")?;
        let (vault_accounts, claim_accounts) = rest.split_at(self.vaults.len());
        let factory: vs::Factory = match factory {
            Some(account) => decode(account).context("the vault factory")?,
            None => return Ok(None),
        };
        if !listing_stands((self.vault_count, 0), (factory.vault_count, 0)) {
            return Ok(None);
        }
        let mut vaults = Vec::with_capacity(self.vaults.len());
        for (address, account) in self.vaults.iter().zip(vault_accounts) {
            // A vault is never closed as an account: one gone is a reason to
            // look at the whole program again.
            let Some(account) = account else {
                return Ok(None);
            };
            let vault: Vault = decode(account)
                .with_context(|| format!("the vault at {address} doesn't read as one"))?;
            vaults.push((*address, vault));
        }
        if !listing_stands(
            (self.vault_count, self.claim_total),
            (factory.vault_count, claim_total(&vaults)),
        ) {
            return Ok(None);
        }
        let mut claims = Vec::with_capacity(self.claims.len());
        for ((vault, address), account) in self.claims.iter().zip(claim_accounts) {
            // A claim closed for its rent is simply no longer there: read as
            // of the listing or later, one missing is closed, not unseen.
            let Some(account) = account else { continue };
            let claim: Claim = decode(account)
                .with_context(|| format!("the claim at {address} doesn't read as one"))?;
            claims.push((*vault, *address, claim));
        }
        self.claims = claims
            .iter()
            .map(|(vault, address, _)| (*vault, *address))
            .collect();
        Ok(Some(Found { vaults, claims }))
    }
}

/// Whether the program is to be listed afresh at `now`: never listed yet, or
/// listed `every` ago or longer (at every look when `every` is zero).
fn listing_due(listed_at: Option<Instant>, every: Duration, now: Instant) -> bool {
    listed_at.is_none_or(|at| every.is_zero() || now.saturating_duration_since(at) >= every)
}

/// Whether what was listed still stands: neither the factory's count of
/// vaults nor the vaults' count of claims has moved since.
fn listing_stands(listed: (u64, u64), now: (u64, u64)) -> bool {
    listed == now
}

/// Every claim the vaults have booked, all told.
fn claim_total(vaults: &[(Pubkey, Vault)]) -> u64 {
    vaults.iter().fold(0u64, |total, (_, vault)| {
        total.saturating_add(vault.claim_count)
    })
}

/// An account of the vault program, read as `T`.
fn decode<T: AccountDeserialize>(account: &Account) -> Result<T> {
    anyhow::ensure!(
        account.owner == shonin_vault::ID,
        "the account is not the vault program's"
    );
    Ok(T::try_deserialize(&mut &account.data[..])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_cluster_is_listed_at_every_look_and_a_public_one_seldom() {
        assert!(VaultScan::for_cluster("localnet").every.is_zero());
        assert_eq!(VaultScan::for_cluster("devnet").every, LIST_AT_LEAST_EVERY);
        let now = Instant::now();
        let every = LIST_AT_LEAST_EVERY;
        assert!(listing_due(None, every, now));
        assert!(!listing_due(Some(now), every, now + every / 2));
        assert!(listing_due(Some(now), every, now + every));
        assert!(listing_due(Some(now), Duration::ZERO, now));
    }

    #[test]
    fn a_new_vault_or_claim_has_the_program_listed_again_and_nothing_else_does() {
        assert!(listing_stands((3, 5), (3, 5)));
        // A vault activated, or a claim booked, since the listing.
        assert!(!listing_stands((3, 5), (4, 5)));
        assert!(!listing_stands((3, 5), (3, 6)));
    }
}
