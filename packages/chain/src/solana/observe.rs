//! One look at a cluster: every account a service decides from, read in
//! one call (`getMultipleAccounts`, a hundred accounts a call) as of one
//! slot or later, with the cluster's clock among them.
//!
//! What it reads: the clock, the vault factory, every vault, every claim and
//! request the vaults have, every holding of a vault whose units are not
//! zero, and whatever else its caller names (the accounts its decision code
//! read of the node before, see [`super::served::Learned`]).
//!
//! The vault program's accounts are listed (`getProgramAccounts`, among the
//! dearest calls a node answers) only when a count says something new is
//! there, as [`super::scan::VaultScan`] does: the factory counts its vaults,
//! each vault its claims and its requests, and a vault's units say whether a
//! holding has units to serve. A listing is also made when a vault is gone,
//! and after [`Observer::new`]'s `every` in any case. Between listings each
//! look reads every account listed again. A claim, a request or a holding
//! that is gone was closed (none comes back at the same address), and is no
//! reason to list again.
//!
//! Every read is pinned to the slot of the one before it
//! (`minContextSlot`): a node behind that slot is asked again a few times
//! and then the look is [`Looked::Lagging`], and nothing in it is taken. A
//! node that answers as of an older slot all the same is lagging too.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use anchor_lang::{AccountDeserialize, Discriminator};
use anyhow::{Context, Result, anyhow};
use shonin_vault::state::{self as vs, Claim, Factory, Holder, Request, Vault};

use super::Pubkey;
use super::client::{SolanaClient, clock_seconds};
use super::rpc::{Account, behind};
use super::served::{Learned, Listing, Served, clock_address};

/// What one look found.
pub struct Observation {
    /// The slot the read was answered at.
    pub slot: u64,
    /// The cluster's clock at that slot, in seconds, if the node answered
    /// it (a look without it decides nothing from it).
    pub now: Option<u64>,
    /// When the answer arrived, on this process's clock and the wall's
    /// (milliseconds since the Unix epoch).
    pub arrived: Instant,
    pub wall_ms: u64,
    pub factory: Factory,
    /// Every vault, by strategy id.
    pub vaults: Vec<(Pubkey, Vault)>,
    /// Each claim with its vault and its own address, by vault and id.
    pub claims: Vec<(Pubkey, Pubkey, Claim)>,
    /// Why the program was listed for this look, if it was.
    pub listed: Option<&'static str>,
    accounts: HashMap<Pubkey, Option<Account>>,
    listings: Vec<Listing>,
    /// The calls the look made, and of them the listings.
    pub calls: u64,
    pub listing_calls: u64,
}

impl Observation {
    /// Reads answered from this look (see [`Served`]), what is read of the
    /// node noted in `learned`.
    pub fn served(&self, learned: Learned) -> Served {
        Served::new(
            self.slot,
            self.arrived,
            self.accounts.clone(),
            self.listings.clone(),
            learned,
        )
    }

    /// The account at `address`, if the look read it (`Some(None)`: none is
    /// there).
    pub fn account(&self, address: &Pubkey) -> Option<&Option<Account>> {
        self.accounts.get(address)
    }

    /// The account at `address` read as `T`, if the look read one there.
    pub fn read<T: AccountDeserialize>(&self, address: &Pubkey) -> Option<T> {
        let account = self.accounts.get(address)?.as_ref()?;
        T::try_deserialize(&mut &account.data[..]).ok()
    }

    /// Every holding of `vault` the look read whose units are not zero:
    /// `None` where it holds no whole listing of the vault's holdings.
    pub fn holders(&self, vault: &Pubkey) -> Option<Vec<Holder>> {
        let listing = self.listings.iter().find(|listing| {
            listing.program == shonin_vault::ID && listing.filters == holder_filters(vault)
        })?;
        Some(
            listing
                .addresses
                .iter()
                .filter_map(|address| self.read::<Holder>(address))
                .filter(|holder| holder.units != 0 && holder.vault == *vault)
                .collect(),
        )
    }

    /// How many accounts the look read.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}

/// What a look came to.
pub enum Looked {
    Observed(Box<Observation>),
    /// The node answered as of a slot older than one already seen
    /// (`answered`, when it said), or kept saying it hadn't reached it:
    /// nothing it answered is taken.
    Lagging {
        answered: Option<u64>,
        seen: u64,
    },
}

/// What the last listing found, and the counts that say whether it still
/// holds everything.
#[derive(Clone, Debug, Default)]
struct Listed {
    vault_count: u64,
    vaults: Vec<Pubkey>,
    claim_total: u64,
    claims: Vec<(Pubkey, Pubkey)>,
    request_total: u64,
    requests: Vec<Pubkey>,
    /// Each vault whose units were not zero: its units then, and every
    /// holding of it.
    holders: Vec<(Pubkey, u128, Vec<Pubkey>)>,
}

/// The program's accounts as last listed, and the latest slot read at.
#[derive(Debug)]
pub struct Observer {
    every: Duration,
    listed_at: Option<Instant>,
    listed: Listed,
    /// Why the next look lists the program again, whatever the counts say.
    relist: Option<&'static str>,
    slot: u64,
}

/// The filters that list one vault's holdings.
fn holder_filters(vault: &Pubkey) -> Vec<(usize, Vec<u8>)> {
    vec![
        (0, Holder::DISCRIMINATOR.to_vec()),
        (8, vault.to_bytes().to_vec()),
    ]
}

impl Observer {
    /// Listing the program at most every `every` when the counts say
    /// nothing new is there; `Duration::ZERO` lists it at every look.
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            listed_at: None,
            listed: Listed::default(),
            relist: None,
            slot: 0,
        }
    }

    /// The latest slot a look was answered at.
    pub fn slot(&self) -> u64 {
        self.slot
    }

    /// The next look lists the program again, for `why`.
    pub fn relist(&mut self, why: &'static str) {
        self.relist.get_or_insert(why);
    }

    /// Every account as it is now, `extra` with them.
    pub async fn look(&mut self, client: &SolanaClient, extra: &[Pubkey]) -> Result<Looked> {
        let client = client.live();
        let mut calls = 0;
        let mut listing_calls = 0;
        let mut listed = None;
        for attempt in 0..2 {
            let due = if self.listed_at.is_none() {
                Some("the first look")
            } else if self.every.is_zero()
                || self.listed_at.is_some_and(|at| at.elapsed() >= self.every)
            {
                Some("the listing's age")
            } else {
                self.relist.take()
            };
            if let Some(why) = due {
                match self.list(&client).await {
                    Ok(Some(made)) => {
                        listing_calls += made;
                        calls += made;
                        listed = Some(why);
                    }
                    Ok(None) => {
                        return Ok(Looked::Lagging {
                            answered: None,
                            seen: self.slot,
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
            let mut found = match self.read(&client, extra).await? {
                Read::Lagging(looked) => return Ok(looked),
                Read::Found(found) => found,
            };
            calls += found.calls;
            found.observation.calls = calls;
            found.observation.listing_calls = listing_calls;
            found.observation.listed = listed;
            match found.unlisted {
                None => return Ok(Looked::Observed(found.observation)),
                // Something new since the listing: listed again at once,
                // and once only (a busy cluster's newest accounts wait for
                // the next look).
                Some(why) if attempt == 0 => self.relist = Some(why),
                Some(why) => {
                    self.relist = Some(why);
                    return Ok(Looked::Observed(found.observation));
                }
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// The program listed afresh, each count read before the listing it
    /// stands for and the listing as of the count's slot or later, so the
    /// listing holds everything the count does. How many calls it took, or
    /// `None` when the node stayed behind.
    async fn list(&mut self, client: &SolanaClient) -> Result<Option<u64>> {
        let rpc = &client.rpc;
        let pinned = |result: Result<(u64, Vec<(Pubkey, Account)>)>| match result {
            Ok(found) => Ok(Some(found)),
            Err(error) if behind(&error) => Ok(None),
            Err(error) => Err(error),
        };
        let (slot, factory) = match rpc
            .accounts_since(&[vs::factory_address()], self.slot)
            .await
        {
            Ok(read) => read,
            Err(error) if behind(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let factory: Factory = match factory.first() {
            Some(Some(account)) => decode(account).context("the vault factory")?,
            _ => anyhow::bail!("the vault factory is not set up"),
        };
        let mut calls = 1;
        let mut floor = self.slot.max(slot);
        let Some((slot, found)) = pinned(
            rpc.program_accounts_since(
                &shonin_vault::ID,
                &[(0, Vault::DISCRIMINATOR.to_vec())],
                floor,
            )
            .await,
        )?
        else {
            return Ok(None);
        };
        calls += 1;
        floor = floor.max(slot);
        let mut vaults: Vec<(Pubkey, Vault)> = Vec::with_capacity(found.len());
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

        let Some((slot, found)) = pinned(
            rpc.program_accounts_since(
                &shonin_vault::ID,
                &[(0, Claim::DISCRIMINATOR.to_vec())],
                floor,
            )
            .await,
        )?
        else {
            return Ok(None);
        };
        calls += 1;
        floor = floor.max(slot);
        let mut claims = Vec::new();
        for (address, account) in found {
            let claim: Claim =
                decode(&account).context("a claim account that doesn't read as one")?;
            // A claim doesn't name its vault: it is matched by address.
            if let Some(vault) = addresses
                .iter()
                .find(|vault| vs::claim_address(vault, claim.id).0 == address)
            {
                claims.push((*vault, address, claim.id));
            }
        }
        claims.sort_by_key(|(vault, _, id)| (*vault, *id));

        let Some((slot, found)) = pinned(
            rpc.program_accounts_since(
                &shonin_vault::ID,
                &[(0, Request::DISCRIMINATOR.to_vec())],
                floor,
            )
            .await,
        )?
        else {
            return Ok(None);
        };
        calls += 1;
        floor = floor.max(slot);
        let mut requests: Vec<Pubkey> = found.into_iter().map(|(address, _)| address).collect();
        requests.sort_unstable();

        let mut holders = Vec::new();
        for (address, vault) in vaults.iter().filter(|(_, vault)| vault.units != 0) {
            let Some((slot, found)) = pinned(
                rpc.program_accounts_since(&shonin_vault::ID, &holder_filters(address), floor)
                    .await,
            )?
            else {
                return Ok(None);
            };
            calls += 1;
            floor = floor.max(slot);
            let mut held = Vec::new();
            for (at, account) in found {
                let holder: Holder =
                    decode(&account).context("a holding that doesn't read as one")?;
                // Only an account at the holding's own address is the
                // holding.
                if holder.vault == *address && vs::holder_address(address, &holder.owner).0 == at {
                    held.push(at);
                }
            }
            held.sort_unstable();
            holders.push((*address, vault.units, held));
        }

        self.slot = floor;
        self.listed_at = Some(Instant::now());
        self.relist = None;
        self.listed = Listed {
            vault_count: factory.vault_count,
            claim_total: total(&vaults, |vault| vault.claim_count),
            request_total: total(&vaults, |vault| vault.request_count),
            vaults: addresses,
            claims: claims
                .into_iter()
                .map(|(vault, address, _)| (vault, address))
                .collect(),
            requests,
            holders,
        };
        Ok(Some(calls))
    }

    /// Every account listed, the clock and `extra`, in one read pinned to
    /// the latest slot seen.
    async fn read(&mut self, client: &SolanaClient, extra: &[Pubkey]) -> Result<Read> {
        let listed = self.listed.clone();
        let mut addresses = vec![clock_address(), vs::factory_address()];
        addresses.extend(&listed.vaults);
        addresses.extend(listed.claims.iter().map(|(_, claim)| *claim));
        addresses.extend(&listed.requests);
        for (_, _, held) in &listed.holders {
            addresses.extend(held);
        }
        let mut seen: BTreeSet<Pubkey> = addresses.iter().copied().collect();
        addresses.extend(extra.iter().filter(|address| seen.insert(**address)));
        let (slot, accounts) = match client.rpc.accounts_since(&addresses, self.slot).await {
            Ok(read) => read,
            Err(error) if behind(&error) => {
                return Ok(Read::Lagging(Looked::Lagging {
                    answered: None,
                    seen: self.slot,
                }));
            }
            Err(error) => return Err(error),
        };
        let arrived = Instant::now();
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        if slot < self.slot {
            return Ok(Read::Lagging(Looked::Lagging {
                answered: Some(slot),
                seen: self.slot,
            }));
        }
        self.slot = slot;
        let calls = addresses.len().div_ceil(100) as u64;
        let accounts: HashMap<Pubkey, Option<Account>> =
            addresses.iter().copied().zip(accounts).collect();
        let now = clock_seconds(accounts.get(&clock_address()).and_then(Option::as_ref)).ok();
        let factory: Factory = match accounts.get(&vs::factory_address()) {
            Some(Some(account)) => decode(account).context("the vault factory")?,
            _ => anyhow::bail!("the vault factory is not set up"),
        };

        let mut unlisted = None;
        let mut vaults = Vec::with_capacity(listed.vaults.len());
        for address in &listed.vaults {
            // A vault is never closed as an account: one gone is a reason
            // to list the program again.
            match accounts.get(address) {
                Some(Some(account)) => {
                    let vault: Vault = decode(account)
                        .with_context(|| format!("the vault at {address} doesn't read as one"))?;
                    vaults.push((*address, vault));
                }
                _ => {
                    unlisted.get_or_insert("a vault is gone");
                }
            }
        }
        let vaults_whole = unlisted.is_none() && factory.vault_count == listed.vault_count;
        if factory.vault_count != listed.vault_count {
            unlisted.get_or_insert("a new vault");
        }
        let claims_whole =
            vaults_whole && total(&vaults, |vault| vault.claim_count) == listed.claim_total;
        if !claims_whole {
            unlisted.get_or_insert("a new claim");
        }
        let requests_whole =
            vaults_whole && total(&vaults, |vault| vault.request_count) == listed.request_total;
        if !requests_whole {
            unlisted.get_or_insert("a new request");
        }
        let mut holders_whole = Vec::new();
        for (address, vault) in &vaults {
            let was = listed
                .holders
                .iter()
                .find(|(listed, _, _)| listed == address);
            match was {
                Some((_, units, held)) if *units == vault.units => {
                    holders_whole.push((*address, held.clone()));
                }
                None if vault.units == 0 => {}
                _ => {
                    unlisted.get_or_insert("a vault's units changed");
                }
            }
        }

        let mut claims = Vec::with_capacity(listed.claims.len());
        for (vault, address) in &listed.claims {
            // A claim closed for its rent is simply no longer there: read as
            // of the listing or later, one missing is closed, not unseen.
            let Some(Some(account)) = accounts.get(address) else {
                continue;
            };
            let claim: Claim = decode(account)
                .with_context(|| format!("the claim at {address} doesn't read as one"))?;
            claims.push((*vault, *address, claim));
        }
        // What was closed since is not read again.
        self.listed.claims.retain(|(_, address)| {
            accounts
                .get(address)
                .is_some_and(|account| account.is_some())
        });
        self.listed.requests.retain(|address| {
            accounts
                .get(address)
                .is_some_and(|account| account.is_some())
        });
        for (_, _, held) in &mut self.listed.holders {
            held.retain(|address| {
                accounts
                    .get(address)
                    .is_some_and(|account| account.is_some())
            });
        }

        let mut listings = Vec::new();
        if vaults_whole {
            listings.push(Listing {
                program: shonin_vault::ID,
                filters: vec![(0, Vault::DISCRIMINATOR.to_vec())],
                addresses: vaults.iter().map(|(address, _)| *address).collect(),
            });
        }
        if claims_whole {
            listings.push(Listing {
                program: shonin_vault::ID,
                filters: vec![(0, Claim::DISCRIMINATOR.to_vec())],
                addresses: listed.claims.iter().map(|(_, claim)| *claim).collect(),
            });
        }
        if requests_whole {
            listings.push(Listing {
                program: shonin_vault::ID,
                filters: vec![(0, Request::DISCRIMINATOR.to_vec())],
                addresses: listed.requests.clone(),
            });
        }
        for (vault, held) in holders_whole {
            listings.push(Listing {
                program: shonin_vault::ID,
                filters: holder_filters(&vault),
                addresses: held,
            });
        }
        Ok(Read::Found(Found {
            observation: Box::new(Observation {
                slot,
                now,
                arrived,
                wall_ms,
                factory,
                vaults,
                claims,
                listed: None,
                accounts,
                listings,
                calls: 0,
                listing_calls: 0,
            }),
            unlisted,
            calls,
        }))
    }
}

/// What one read came to.
enum Read {
    Found(Found),
    Lagging(Looked),
}

struct Found {
    observation: Box<Observation>,
    /// Why the listing no longer holds everything, if it doesn't.
    unlisted: Option<&'static str>,
    calls: u64,
}

/// A count summed over every vault.
fn total(vaults: &[(Pubkey, Vault)], count: impl Fn(&Vault) -> u64) -> u64 {
    vaults
        .iter()
        .fold(0u64, |sum, (_, vault)| sum.saturating_add(count(vault)))
}

/// An account of the vault program, read as `T`.
fn decode<T: AccountDeserialize>(account: &Account) -> Result<T> {
    if account.owner != shonin_vault::ID {
        return Err(anyhow!("the account is not the vault program's"));
    }
    Ok(T::try_deserialize(&mut &account.data[..])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChainConfig, ChainKey, ChainKind};
    use anchor_lang::{AccountSerialize, Space};
    use base64::Engine;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A node holding a factory, vaults and claims, answering as of `slot`
    /// (lowered by a test to play a node behind), and counting each method.
    #[derive(Default)]
    struct Node {
        accounts: HashMap<Pubkey, Vec<u8>>,
        slot: u64,
        /// Answer every read as not having reached its slot.
        behind: bool,
        asked: Vec<String>,
    }

    impl Node {
        fn answer(&mut self, method: &str, params: &Value) -> Value {
            self.asked.push(method.to_string());
            let raw = |data: &Vec<u8>| {
                json!({
                    "lamports": 1_000_000u64, "owner": shonin_vault::ID.to_string(),
                    "executable": false, "rentEpoch": 0,
                    "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
                })
            };
            let context = json!({"slot": self.slot});
            if self.behind && method != "getGenesisHash" {
                return json!({"error": {"code": -32016, "message": "Minimum context slot has not been reached"}});
            }
            let result = match method {
                "getGenesisHash" => json!("stand-in"),
                "getMultipleAccounts" => {
                    let values: Vec<Value> = params[0]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|address| {
                            let address: Pubkey = address.as_str().unwrap().parse().unwrap();
                            if address == clock_address() {
                                let mut clock = vec![0u8; 40];
                                clock[32..40].copy_from_slice(&1_800_000_000i64.to_le_bytes());
                                return json!({
                                    "lamports": 1u64, "owner": Pubkey::default().to_string(),
                                    "executable": false, "rentEpoch": 0,
                                    "data": [base64::engine::general_purpose::STANDARD.encode(clock), "base64"],
                                });
                            }
                            self.accounts.get(&address).map_or(Value::Null, raw)
                        })
                        .collect();
                    json!({"context": context, "value": values})
                }
                "getProgramAccounts" => {
                    let filters: Vec<(usize, Vec<u8>)> = params[1]["filters"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|filter| {
                            (
                                filter["memcmp"]["offset"].as_u64().unwrap() as usize,
                                bs58::decode(filter["memcmp"]["bytes"].as_str().unwrap())
                                    .into_vec()
                                    .unwrap(),
                            )
                        })
                        .collect();
                    let found: Vec<Value> = self
                        .accounts
                        .iter()
                        .filter(|(_, data)| super::super::served::matches_filters(data, &filters))
                        .map(|(address, data)| json!({"pubkey": address.to_string(), "account": raw(data)}))
                        .collect();
                    json!({"context": context, "value": found})
                }
                other => panic!("not asked of a look: {other}"),
            };
            json!({"result": result})
        }
    }

    async fn serve(node: Arc<Mutex<Node>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let node = node.clone();
                tokio::spawn(async move {
                    loop {
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 65536];
                        let body = loop {
                            let n = socket.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buffer.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&buffer).to_string();
                            if let Some(split) = text.find("\r\n\r\n") {
                                let length = text[..split]
                                    .lines()
                                    .find_map(|line| {
                                        line.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .map(|v| v.trim().parse::<usize>().unwrap())
                                    })
                                    .unwrap_or(0);
                                if buffer.len() >= split + 4 + length {
                                    break buffer[split + 4..split + 4 + length].to_vec();
                                }
                            }
                        };
                        let request: Value = serde_json::from_slice(&body).unwrap();
                        let mut answer = node
                            .lock()
                            .unwrap()
                            .answer(request["method"].as_str().unwrap(), &request["params"]);
                        answer["jsonrpc"] = json!("2.0");
                        answer["id"] = request["id"].clone();
                        let reply = answer.to_string();
                        let http = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                            reply.len()
                        );
                        if socket.write_all(http.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        url
    }

    fn encode<T: AccountSerialize>(account: &T) -> Vec<u8> {
        let mut out = Vec::new();
        account.try_serialize(&mut out).unwrap();
        out
    }

    fn zeroed<T: AccountDeserialize + Discriminator>(len: usize) -> T {
        let mut bytes = vec![0u8; 8 + len];
        bytes[..8].copy_from_slice(T::DISCRIMINATOR);
        T::try_deserialize(&mut &bytes[..]).unwrap()
    }

    /// A node with `vaults` open vaults, each with one claim, as of slot 10.
    fn a_node(vaults: u64) -> Node {
        let mut node = Node {
            slot: 10,
            ..Node::default()
        };
        let mut factory: Factory = zeroed(Factory::INIT_SPACE);
        factory.vault_count = vaults;
        node.accounts
            .insert(vs::factory_address(), encode(&factory));
        for id in 1..=vaults {
            let address = vs::vault_address(id).0;
            let mut vault: Vault = zeroed(Vault::INIT_SPACE);
            vault.strategy_id = id;
            vault.claim_count = 1;
            node.accounts.insert(address, encode(&vault));
            let mut claim: Claim = zeroed(Claim::INIT_SPACE);
            claim.id = 1;
            node.accounts
                .insert(vs::claim_address(&address, 1).0, encode(&claim));
        }
        node
    }

    async fn client(url: &str) -> SolanaClient {
        let config = ChainConfig {
            label: "stand-in".into(),
            rpc: url.into(),
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            confirmations: None,
            public_rpc: url.into(),
            logs_rpc: None,
            log_range: None,
        };
        SolanaClient::connect_with(&ChainKey::new(ChainKind::Solana, "localnet"), &config, None)
            .await
            .unwrap()
    }

    fn asked(node: &Arc<Mutex<Node>>) -> Vec<String> {
        std::mem::take(&mut node.lock().unwrap().asked)
    }

    fn observed(looked: Looked) -> Box<Observation> {
        match looked {
            Looked::Observed(observation) => observation,
            Looked::Lagging { answered, seen } => {
                panic!("lagging: answered {answered:?}, seen {seen}")
            }
        }
    }

    #[tokio::test]
    async fn a_look_whose_counts_hold_is_one_read_of_every_account_and_no_listing() {
        let node = Arc::new(Mutex::new(a_node(3)));
        let client = client(&serve(node.clone()).await).await;
        let mut observer = Observer::new(Duration::from_secs(300));
        asked(&node);
        let first = observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(first.listed, Some("the first look"));
        assert_eq!(first.vaults.len(), 3);
        assert_eq!(first.claims.len(), 3);
        assert_eq!(first.now, Some(1_800_000_000));
        asked(&node);
        let extra = Pubkey::new_unique();
        let second = observed(observer.look(&client, &[extra]).await.unwrap());
        assert_eq!(asked(&node), vec!["getMultipleAccounts"]);
        assert_eq!((second.calls, second.listing_calls), (1, 0));
        assert_eq!(second.listed, None);
        assert_eq!(
            second.account(&extra),
            Some(&None),
            "what was named is read"
        );
        // Its listings answer what a decision would list.
        let served = second.served(Learned::default());
        let claims = served
            .program_accounts(&shonin_vault::ID, &[(0, Claim::DISCRIMINATOR.to_vec())])
            .unwrap();
        assert_eq!(claims.len(), 3);
        assert!(
            served
                .program_accounts(&shonin_vault::ID, &[(0, Request::DISCRIMINATOR.to_vec())])
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_vault_gone_or_a_count_moved_has_the_program_listed_again() {
        let node = Arc::new(Mutex::new(a_node(2)));
        let client = client(&serve(node.clone()).await).await;
        let mut observer = Observer::new(Duration::from_secs(300));
        observed(observer.look(&client, &[]).await.unwrap());
        asked(&node);

        // A new claim on a known vault: its count moves.
        {
            let mut node = node.lock().unwrap();
            let address = vs::vault_address(1).0;
            let mut vault: Vault =
                Vault::try_deserialize(&mut &node.accounts[&address][..]).unwrap();
            vault.claim_count = 2;
            node.accounts.insert(address, encode(&vault));
            let mut claim: Claim = zeroed(Claim::INIT_SPACE);
            claim.id = 2;
            node.accounts
                .insert(vs::claim_address(&address, 2).0, encode(&claim));
        }
        let look = observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(look.listed, Some("a new claim"));
        assert_eq!(
            look.claims.len(),
            3,
            "the new claim is read in the same look"
        );
        assert!(asked(&node).contains(&"getProgramAccounts".to_string()));

        // A vault gone.
        node.lock()
            .unwrap()
            .accounts
            .remove(&vs::vault_address(2).0);
        let look = observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(look.listed, Some("a vault is gone"));
        assert!(asked(&node).contains(&"getProgramAccounts".to_string()));
        assert_eq!(look.vaults.len(), 1);

        // A claim closed for its rent is no reason to list again.
        node.lock()
            .unwrap()
            .accounts
            .remove(&vs::claim_address(&vs::vault_address(1).0, 1).0);
        let look = observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(look.listed, None);
        assert_eq!(asked(&node), vec!["getMultipleAccounts"]);
        assert_eq!(look.claims.len(), 1);
    }

    #[tokio::test]
    async fn a_node_behind_the_slot_already_seen_is_lagging_and_nothing_it_answers_is_taken() {
        let node = Arc::new(Mutex::new(a_node(1)));
        let client = client(&serve(node.clone()).await).await;
        let mut observer = Observer::new(Duration::from_secs(300));
        observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(observer.slot(), 10);
        // A node that answers as of an older slot all the same.
        node.lock().unwrap().slot = 7;
        match observer.look(&client, &[]).await.unwrap() {
            Looked::Lagging { answered, seen } => assert_eq!((answered, seen), (Some(7), 10)),
            Looked::Observed(_) => panic!("an older slot's answer was taken"),
        }
        assert_eq!(observer.slot(), 10, "the slot seen never goes back");
        // A node that says it hasn't reached the slot.
        node.lock().unwrap().behind = true;
        match observer.look(&client, &[]).await.unwrap() {
            Looked::Lagging { answered, seen } => assert_eq!((answered, seen), (None, 10)),
            Looked::Observed(_) => panic!("a node behind was believed"),
        }
        // Caught up: the next look is taken.
        {
            let mut node = node.lock().unwrap();
            node.behind = false;
            node.slot = 12;
        }
        let look = observed(observer.look(&client, &[]).await.unwrap());
        assert_eq!(look.slot, 12);
    }
}
