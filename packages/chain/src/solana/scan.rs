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

use std::time::{Duration, Instant};

use anchor_lang::AccountDeserialize;
use anyhow::{Context, Result};
use shonin_vault::state::{self as vs, Claim, Vault};

use super::Pubkey;
use super::client::SolanaClient;
use super::platform::Platform;
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
    pub async fn look(&mut self, client: &SolanaClient, platform: &Platform) -> Result<Found> {
        let due = self
            .listed_at
            .is_none_or(|at| at.elapsed() >= self.every || self.every.is_zero());
        if !due && let Some(found) = self.read_known(client).await? {
            return Ok(found);
        }
        self.list(client, platform).await
    }

    /// The program listed afresh. Each count is read before the listing it
    /// stands for, so an account made in between moves it again at the next
    /// look, and is listed then.
    async fn list(&mut self, client: &SolanaClient, platform: &Platform) -> Result<Found> {
        let factory = platform.factory(client).await?;
        let vaults = platform.vaults(client).await?;
        let addresses: Vec<Pubkey> = vaults.iter().map(|(address, _)| *address).collect();
        let claims = platform.claims_of(client, &addresses).await?;
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
        let accounts = client.rpc.accounts(&addresses).await?;
        let (factory, rest) = accounts
            .split_first()
            .context("the node answered no account at all")?;
        let (vault_accounts, claim_accounts) = rest.split_at(self.vaults.len());
        let factory: vs::Factory = match factory {
            Some(account) => decode(account).context("the vault factory")?,
            None => return Ok(None),
        };
        if factory.vault_count != self.vault_count {
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
        if claim_total(&vaults) != self.claim_total {
            return Ok(None);
        }
        let mut claims = Vec::with_capacity(self.claims.len());
        for ((vault, address), account) in self.claims.iter().zip(claim_accounts) {
            // A claim closed for its rent is simply no longer there.
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
    }
}
