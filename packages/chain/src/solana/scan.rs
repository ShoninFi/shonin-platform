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

use std::collections::{BTreeMap, BTreeSet};
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
    /// Finalized closed vaults omitted inside their established quiet period.
    /// Their historical accounts are never returned as current views.
    pub deferred: Vec<Pubkey>,
    /// Closed vaults whose quiet deadline has arrived, to read after active work.
    pub waking: Vec<Pubkey>,
}

/// The vaults and claims known from the last listing, and the counts that
/// say whether a newer one is needed.
pub struct VaultScan {
    include_claims: bool,
    every: Duration,
    listed_at: Option<Instant>,
    vault_count: u64,
    vaults: Vec<Pubkey>,
    claim_total: u64,
    claim_counts: BTreeMap<Pubkey, u64>,
    waking: BTreeSet<Pubkey>,
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
            include_claims: true,
            every,
            listed_at: None,
            vault_count: 0,
            vaults: Vec::new(),
            claim_total: 0,
            claim_counts: BTreeMap::new(),
            waking: BTreeSet::new(),
            claims: Vec::new(),
            slot: 0,
        }
    }

    /// Discover vaults by the factory count, reading known vault accounts
    /// afresh each time. Consumers that read claims separately need no claim
    /// listing here; a moved claim count does not create another vault.
    pub fn vaults_only(every: Duration) -> Self {
        Self {
            include_claims: false,
            ..Self::new(every)
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
        self.look_near_due(client, &BTreeMap::new()).await
    }

    /// Read active accounts afresh. The caller may omit a vault only after
    /// independently proving Closed at finalized commitment, and only until
    /// the keeper's existing quiet deadline. The fresh clock is read in the
    /// same batch; reaching a deadline or a missing clock forces fresh reads.
    pub async fn look_near_due(
        &mut self,
        client: &SolanaClient,
        quiet: &BTreeMap<Pubkey, u64>,
    ) -> Result<Found> {
        if !listing_due(self.listed_at, self.every, Instant::now())
            && let Some(found) = self.read_known(client, quiet).await?
        {
            return Ok(found);
        }
        // Discovery always reads the whole listing, including quiet accounts.
        self.list(client).await
    }

    pub fn known_vaults(&self) -> &[Pubkey] {
        &self.vaults
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
        anyhow::ensure!(
            vaults.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "the vault listing contains a duplicate account"
        );
        // Vault accounts are never closed. A successful but truncated listing
        // cannot stand for a count that already included the missing vaults.
        // New activations between the count and listing may add more accounts.
        anyhow::ensure!(
            vaults.len() as u64 >= factory.vault_count,
            "the vault listing contains {} accounts but the factory counts {}",
            vaults.len(),
            factory.vault_count
        );
        let addresses: Vec<Pubkey> = vaults.iter().map(|(address, _)| *address).collect();
        if !self.include_claims {
            self.slot = self.slot.max(slot);
            self.listed_at = Some(Instant::now());
            self.vault_count = factory.vault_count;
            self.vaults = addresses;
            self.waking.clear();
            return Ok(Found {
                vaults,
                claims: Vec::new(),
                deferred: Vec::new(),
                waking: Vec::new(),
            });
        }
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
        self.waking.clear();
        self.claim_total = claim_total(&vaults);
        self.claim_counts = vaults
            .iter()
            .map(|(address, vault)| (*address, vault.claim_count))
            .collect();
        self.claims = claims
            .iter()
            .map(|(vault, address, _)| (*vault, *address))
            .collect();
        Ok(Found {
            vaults,
            claims,
            deferred: Vec::new(),
            waking: Vec::new(),
        })
    }

    /// The factory, the vaults and the claims known, read in one call; `None`
    /// when a count has moved since the listing, so the program must be
    /// listed again.
    async fn read_known(
        &mut self,
        client: &SolanaClient,
        quiet: &BTreeMap<Pubkey, u64>,
    ) -> Result<Option<Found>> {
        let deferred: BTreeSet<_> = self
            .vaults
            .iter()
            .filter(|address| quiet.contains_key(address) && !self.waking.contains(address))
            .copied()
            .collect();
        let clock = anchor_lang::solana_program::sysvar::clock::ID;
        let mut addresses = vec![vs::factory_address()];
        if !deferred.is_empty() {
            addresses.push(clock);
        }
        let active: Vec<_> = self
            .vaults
            .iter()
            .filter(|address| !deferred.contains(address))
            .copied()
            .collect();
        let active_claims: Vec<_> = self
            .claims
            .iter()
            .filter(|(vault, _)| !deferred.contains(vault))
            .copied()
            .collect();
        addresses.extend(&active);
        addresses.extend(active_claims.iter().map(|(_, claim)| *claim));
        let (slot, accounts) = client.rpc.accounts_since(&addresses, self.slot).await?;
        anyhow::ensure!(
            accounts.len() == addresses.len(),
            "the node answered an incomplete account batch"
        );
        self.slot = self.slot.max(slot);
        let factory: vs::Factory = match &accounts[0] {
            Some(account) => decode(account).context("the vault factory")?,
            None => return Ok(None),
        };
        if factory.vault_count != self.vault_count {
            return Ok(None);
        }
        let offset = if deferred.is_empty() { 1 } else { 2 };
        let waking: Vec<_> = if deferred.is_empty() {
            Vec::new()
        } else {
            match super::client::clock_seconds(accounts[1].as_ref()) {
                Ok(now) => deferred
                    .iter()
                    .filter(|address| {
                        now.saturating_add(client.rpc_policy().due_lookahead_ms.div_ceil(1000))
                            >= quiet[address]
                    })
                    .copied()
                    .collect(),
                Err(_) => deferred.iter().copied().collect(),
            }
        };
        let mut vaults = Vec::new();
        for (address, account) in active.iter().zip(&accounts[offset..offset + active.len()]) {
            let Some(account) = account else {
                return Ok(None);
            };
            vaults.push((
                *address,
                decode::<Vault>(account)
                    .with_context(|| format!("the vault at {address} doesn't read as one"))?,
            ));
        }
        let mut claims = Vec::new();
        for ((vault, address), account) in
            active_claims.iter().zip(&accounts[offset + active.len()..])
        {
            if let Some(account) = account {
                claims.push((*vault, *address, decode::<Claim>(account)?));
            }
        }
        self.waking.extend(waking.iter().copied());
        let still_deferred: BTreeSet<_> = deferred
            .difference(&waking.iter().copied().collect())
            .copied()
            .collect();
        let count =
            claim_total(&vaults).saturating_add(deferred.iter().fold(0u64, |total, address| {
                total.saturating_add(self.claim_counts.get(address).copied().unwrap_or_default())
            }));
        if !listing_stands(
            (self.vault_count, self.claim_total),
            (
                factory.vault_count,
                if self.include_claims {
                    count
                } else {
                    self.claim_total
                },
            ),
        ) {
            return Ok(None);
        }
        let present: BTreeSet<_> = claims.iter().map(|(_, address, _)| *address).collect();
        self.claims
            .retain(|(vault, address)| deferred.contains(vault) || present.contains(address));
        vaults.sort_by_key(|(_, vault)| vault.strategy_id);
        claims.sort_by_key(|(vault, _, claim)| (*vault, claim.id));
        self.waking.retain(|address| !active.contains(address));
        Ok(Some(Found {
            vaults,
            claims,
            deferred: still_deferred.into_iter().collect(),
            waking,
        }))
    }
    /// Fresh accounts for closed vaults that woke, requested only after the
    /// caller has finished the active vaults. A changed claim count triggers
    /// discovery, but the returned views still contain only these vaults.
    pub async fn read_waking(&mut self, client: &SolanaClient, waking: &[Pubkey]) -> Result<Found> {
        if waking.is_empty() {
            return Ok(Found {
                vaults: Vec::new(),
                claims: Vec::new(),
                deferred: Vec::new(),
                waking: Vec::new(),
            });
        }
        let selected: BTreeSet<_> = waking.iter().copied().collect();
        let known_claims: Vec<_> = self
            .claims
            .iter()
            .filter(|(vault, _)| selected.contains(vault))
            .copied()
            .collect();
        let mut keys = waking.to_vec();
        keys.extend(known_claims.iter().map(|(_, address)| *address));
        let (slot, accounts) = client.rpc.accounts_since(&keys, self.slot).await?;
        anyhow::ensure!(
            accounts.len() == keys.len(),
            "the node answered an incomplete account batch"
        );
        self.slot = self.slot.max(slot);
        let mut vaults = Vec::new();
        let mut rediscover = false;
        for (address, account) in waking.iter().zip(&accounts[..waking.len()]) {
            if let Some(account) = account {
                let vault = decode::<Vault>(account)?;
                rediscover |= self.include_claims
                    && self.claim_counts.get(address) != Some(&vault.claim_count);
                vaults.push((*address, vault));
            } else {
                rediscover = true;
            }
        }
        if rediscover {
            let mut found = self.list(client).await?;
            found
                .vaults
                .retain(|(address, _)| selected.contains(address));
            found
                .claims
                .retain(|(vault, _, _)| selected.contains(vault));
            self.waking.retain(|address| !selected.contains(address));
            return Ok(found);
        }
        let mut claims = Vec::new();
        for ((vault, address), account) in known_claims.iter().zip(&accounts[waking.len()..]) {
            if let Some(account) = account {
                claims.push((*vault, *address, decode::<Claim>(account)?));
            }
        }
        let present: BTreeSet<_> = claims.iter().map(|(_, address, _)| *address).collect();
        self.claims
            .retain(|(vault, address)| !selected.contains(vault) || present.contains(address));
        vaults.sort_by_key(|(_, vault)| vault.strategy_id);
        claims.sort_by_key(|(vault, _, claim)| (*vault, claim.id));
        self.waking.retain(|address| !selected.contains(address));
        Ok(Found {
            vaults,
            claims,
            deferred: Vec::new(),
            waking: Vec::new(),
        })
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

    #[tokio::test]
    async fn known_vaults_stay_fresh_and_new_factory_counts_force_discovery() {
        use anchor_lang::{AccountSerialize, Space};
        use base64::Engine;
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        fn zeroed<T: AccountDeserialize + Discriminator + Space>() -> T {
            let mut bytes = T::DISCRIMINATOR.to_vec();
            bytes.resize(bytes.len() + T::INIT_SPACE, 0);
            T::try_deserialize(&mut &bytes[..]).unwrap()
        }
        fn encoded(value: &impl AccountSerialize) -> Value {
            let mut data = Vec::new();
            value.try_serialize(&mut data).unwrap();
            json!({"lamports": 1, "owner": shonin_vault::ID.to_string(),
                "executable": false,
                "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"]})
        }
        struct Node {
            count: u64,
            cash: u64,
            claim_count: u64,
            missing: bool,
            scans: u64,
            slot: u64,
            floors: Vec<u64>,
            clock: Option<u64>,
            requested: Vec<Vec<String>>,
            invalid_wake: bool,
            missing_claim: bool,
        }
        let node = Arc::new(Mutex::new(Node {
            count: 1,
            cash: 10,
            claim_count: 0,
            missing: false,
            scans: 0,
            slot: 40,
            floors: Vec::new(),
            clock: Some(1000),
            requested: Vec::new(),
            invalid_wake: false,
            missing_claim: false,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let answering = node.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let body = loop {
                    let mut buffer = [0u8; 4096];
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|n| n.trim().parse().unwrap())
                            })
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            break serde_json::from_slice::<Value>(
                                &request[end + 4..end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                let result = {
                    let mut node = answering.lock().unwrap();
                    if body["method"] == "getGenesisHash" {
                        json!("local-genesis")
                    } else {
                        let floor = body["params"][1]["minContextSlot"].as_u64().unwrap();
                        assert!(floor <= node.slot);
                        node.floors.push(floor);
                        node.slot += 1;
                        if body["method"] == "getMultipleAccounts" {
                            node.requested.push(
                                body["params"][0]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|key| key.as_str().unwrap().to_owned())
                                    .collect(),
                            );
                        }
                        let mut factory: vs::Factory = zeroed();
                        factory.vault_count = node.count;
                        let vault = |id| {
                            let mut vault: Vault = zeroed();
                            vault.strategy_id = id;
                            vault.cash = node.cash;
                            vault.claim_count = node.claim_count;
                            encoded(&vault)
                        };
                        let claim = |id| {
                            let mut claim: Claim = zeroed();
                            claim.id = id;
                            encoded(&claim)
                        };
                        let values: Vec<Value> = match body["method"].as_str().unwrap() {
                            "getMultipleAccounts" => body["params"][0]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|key| {
                                    if key.as_str() == Some(&vs::factory_address().to_string()) {
                                        encoded(&factory)
                                    } else if key.as_str() == Some(&anchor_lang::solana_program::sysvar::clock::ID.to_string()) {
                                        match node.clock {
                                            Some(now) => {
                                                let mut data = vec![0u8; 40];
                                                data[32..40].copy_from_slice(&(now as i64).to_le_bytes());
                                                json!({"lamports": 1, "owner": anchor_lang::solana_program::sysvar::ID.to_string(), "executable": false, "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"]})
                                            }
                                            None => Value::Null,
                                        }
                                    } else if node.missing {
                                        Value::Null
                                    } else {
                                        let id = (1..=node.count)
                                            .find(|id| {
                                                key.as_str()
                                                    == Some(&vs::vault_address(*id).0.to_string())
                                            })
                                            ;
                                        let Some(id) = id else {
                                            let claim_id = (1..=node.count).find_map(|vault_id| {
                                                (1..=node.claim_count).find(|id| key.as_str() == Some(&vs::claim_address(&vs::vault_address(vault_id).0, *id).0.to_string()))
                                            }).unwrap();
                                            return if node.missing_claim { Value::Null } else { claim(claim_id) };
                                        };
                                        let mut value = vault(id);
                                        if node.invalid_wake && id == 1 {
                                            value["owner"] = json!(anchor_lang::solana_program::system_program::ID.to_string());
                                        }
                                        value
                                    }
                                })
                                .collect(),
                            "getProgramAccounts" => {
                                let claims_filter = body["params"][1]["filters"][0]["memcmp"]["bytes"].as_str() == Some(&bs58::encode(Claim::DISCRIMINATOR).into_string());
                                let values = if claims_filter {
                                    (1..=node.count).flat_map(|vault_id| (1..=node.claim_count).map(move |id| (vault_id, id)))
                                        .map(|(vault_id, id)| json!({"pubkey": vs::claim_address(&vs::vault_address(vault_id).0, id).0.to_string(), "account": claim(id)})).collect()
                                } else {
                                    (1..=node.count).map(|id| json!({"pubkey": vs::vault_address(id).0.to_string(), "account": vault(id)})).collect()
                                };
                                node.scans += 1;
                                values
                            }
                            other => panic!("unexpected {other}"),
                        };
                        json!({"context": {"slot": node.slot}, "value": values})
                    }
                };
                let reply =
                    json!({"jsonrpc": "2.0", "id": body["id"], "result": result}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
            }
        });
        let config = crate::config::ChainConfig {
            label: "local scan".into(),
            rpc: url,
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            confirmations: None,
            logs_rpc: None,
            log_range: None,
            public_rpc: String::new(),
        };
        let key = crate::config::ChainKey::new(crate::config::ChainKind::Solana, "localnet");
        let client =
            SolanaClient::connect_signing(&key, &config, super::super::keys::Signing::ReadOnly)
                .await
                .unwrap();
        let mut scan = VaultScan::vaults_only(Duration::from_secs(300));
        assert_eq!(scan.look(&client).await.unwrap().vaults[0].1.cash, 10);
        {
            let mut node = node.lock().unwrap();
            node.cash = 20;
            node.claim_count = 1;
        }
        let found = scan.look(&client).await.unwrap();
        assert_eq!(found.vaults[0].1.cash, 20);
        assert_eq!(found.vaults[0].1.claim_count, 1);
        assert!(found.claims.is_empty());
        assert_eq!(node.lock().unwrap().scans, 1);
        node.lock().unwrap().count = 2;
        assert_eq!(scan.look(&client).await.unwrap().vaults.len(), 2);
        assert_eq!(node.lock().unwrap().scans, 2);
        let closed = vs::vault_address(1).0;
        let active = vs::vault_address(2).0;
        let quiet = BTreeMap::from([(closed, 2000)]);
        node.lock().unwrap().requested.clear();
        let found = scan.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(found.vaults.len(), 1);
        assert_eq!(found.vaults[0].0, active);
        assert_eq!(found.vaults[0].1.cash, 20);
        assert_eq!(found.deferred, vec![closed]);
        assert!(found.waking.is_empty());
        assert_eq!(scan.known_vaults(), &[closed, active]);
        assert!(
            node.lock()
                .unwrap()
                .requested
                .iter()
                .all(|keys| !keys.contains(&closed.to_string()))
        );
        node.lock().unwrap().clock = Some(1994);
        assert!(
            scan.look_near_due(&client, &quiet)
                .await
                .unwrap()
                .waking
                .is_empty()
        );
        for clock in [Some(1995), Some(2200), None] {
            {
                let mut fixture = node.lock().unwrap();
                fixture.clock = clock;
                fixture.cash += 1;
                fixture.requested.clear();
            }
            let found = scan.look_near_due(&client, &quiet).await.unwrap();
            assert_eq!(
                found.vaults.len(),
                1,
                "active work is available before waking reads"
            );
            assert_eq!(found.vaults[0].0, active);
            assert!(found.deferred.is_empty());
            assert_eq!(found.waking, vec![closed]);
            assert_eq!(node.lock().unwrap().requested.len(), 1);
            assert!(!node.lock().unwrap().requested[0].contains(&closed.to_string()));
            let woke = scan.read_waking(&client, &found.waking).await.unwrap();
            assert_eq!(woke.vaults.len(), 1);
            assert_eq!(woke.vaults[0].0, closed);
            assert_eq!(woke.vaults[0].1.cash, node.lock().unwrap().cash);
            assert_eq!(node.lock().unwrap().requested.len(), 2);
        }
        let found = scan.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(found.waking, vec![closed]);
        node.lock().unwrap().invalid_wake = true;
        assert!(scan.read_waking(&client, &found.waking).await.is_err());
        node.lock().unwrap().invalid_wake = false;
        let found = scan.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(
            found.vaults.len(),
            2,
            "a failed optional wake forces an ordinary fresh batch"
        );
        assert!(found.waking.is_empty() && found.deferred.is_empty());
        node.lock().unwrap().count = 3;
        let found = scan.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(found.vaults.len(), 3, "discovery reads quiet accounts too");
        assert!(found.deferred.is_empty() && found.waking.is_empty());
        assert_eq!(node.lock().unwrap().scans, 3);
        node.lock().unwrap().missing = true;
        // A vanished known account triggers a complete listing immediately.
        assert_eq!(scan.look(&client).await.unwrap().vaults.len(), 3);
        node.lock().unwrap().missing = false;
        node.lock().unwrap().clock = Some(1000);
        let split = node.lock().unwrap().floors.len();
        let mut full = VaultScan::new(Duration::from_secs(300));
        let found = full.look(&client).await.unwrap();
        assert_eq!(found.claims.len(), 3);
        let found = full.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(found.vaults.len(), 2);
        assert_eq!(found.claims.len(), 2);
        assert!(found.claims.iter().all(|(vault, _, _)| *vault != closed));
        node.lock().unwrap().missing_claim = true;
        assert!(
            full.look_near_due(&client, &quiet)
                .await
                .unwrap()
                .claims
                .is_empty()
        );
        // Missing active claims are pruned; omitted closed claims are retained.
        assert_eq!(full.claims.len(), 1);
        node.lock().unwrap().missing_claim = false;
        node.lock().unwrap().clock = Some(1995);
        let found = full.look_near_due(&client, &quiet).await.unwrap();
        let woke = full.read_waking(&client, &found.waking).await.unwrap();
        assert_eq!(woke.claims.len(), 1);
        assert_eq!(woke.claims[0].0, closed);
        node.lock().unwrap().clock = Some(1000);
        node.lock().unwrap().claim_count = 2;
        let found = full.look_near_due(&client, &quiet).await.unwrap();
        assert_eq!(found.vaults.len(), 3);
        assert_eq!(
            found.claims.len(),
            6,
            "a changed active claim count rediscovers all claims"
        );
        assert!(found.deferred.is_empty());
        let node = node.lock().unwrap();
        assert_eq!(node.scans, 8);
        for floors in [&node.floors[..split], &node.floors[split..]] {
            assert!(floors.windows(2).all(|pair| pair[1] >= pair[0]));
            assert!(floors.iter().skip(1).all(|floor| *floor >= 41));
        }
        server.abort();
    }
}
