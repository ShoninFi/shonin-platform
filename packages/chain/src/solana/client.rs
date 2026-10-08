//! A connection to a Solana cluster: which cluster it is, the platform's
//! programs on it, reading accounts, and sending transactions signed by a
//! role's key or any other (a vault's own executor key) until they land.
//!
//! Every send and simulation goes through one place, which names every
//! account by address when that fits the wire and otherwise finds them
//! through the paying key's own lookup table ([`super::tables`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anchor_lang::AccountDeserialize;
use anchor_lang::solana_program::instruction::Instruction;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_message::VersionedMessage;
use solana_transaction::versioned::VersionedTransaction;

use super::events::TransactionRecord;
use super::keys::{RoleKeys, Signing};
use super::rpc::{Account, Rpc, RpcError, Simulated};
use super::tables::{self, Table};
use super::{Keypair, Pubkey, Signer, programs};
use crate::client::Role;
use crate::config::{ChainConfig, ChainKey, ChainKind, NetworkName};
use crate::deployments::SolanaDeployments;

/// The most a transaction may weigh on the wire.
pub const PACKET_DATA_SIZE: usize = 1232;
/// The compute every transaction asks for: the most one may use. A vault's
/// calls through its station need much of it (a close at three venues is
/// over a million units), and asking costs nothing while the price is zero.
pub const COMPUTE_UNITS: u32 = 1_400_000;

/// How long a send is watched while the node won't answer about it.
pub const LANDING_TIMEOUT: Duration = Duration::from_secs(90);
/// How often an unconfirmed send is asked after, and sent again.
const POLL: Duration = Duration::from_millis(400);
const RESEND: Duration = Duration::from_secs(2);
/// How long to wait for a cluster to answer at all before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a lookup table just grown is waited on to be usable.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(60);
/// How many times a known lookup table the node answers nothing for is read
/// before the key's tables are searched for again: a node behind the
/// cluster answers nothing for an account that is there.
const TABLE_READS: usize = 5;

/// The public clusters' genesis hashes: a key naming one of them must be
/// answered by it, and a key naming a local cluster must not be answered by
/// any of them.
const PUBLIC_CLUSTERS: [(&str, &str); 3] = [
    (
        "mainnet-beta",
        "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
    ),
    ("devnet", "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"),
    ("testnet", "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY"),
];

/// A transaction the cluster refused or ran and failed, with what it said.
#[derive(Clone, Debug)]
pub struct TransactionFailed {
    pub signature: Option<String>,
    /// The error, as the node wrote it.
    pub err: Value,
    /// The program's own error code, when the failure was one.
    pub custom: Option<u32>,
    pub logs: Vec<String>,
}

impl std::fmt::Display for TransactionFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.custom {
            Some(code) => write!(f, "the transaction failed with program error {code}")?,
            None => write!(f, "the transaction failed: {}", self.err)?,
        }
        // The last lines say where it failed; the whole log can run long.
        let tail = self.logs.len().saturating_sub(6);
        for line in &self.logs[tail..] {
            write!(f, "\n  {line}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TransactionFailed {}

impl TransactionFailed {
    fn from_err(signature: Option<String>, err: Value, logs: Vec<String>) -> Self {
        let custom = err["InstructionError"][1]["Custom"]
            .as_u64()
            .map(|code| code as u32);
        Self {
            signature,
            err,
            custom,
            logs,
        }
    }
}

/// A send that never landed: its blockhash expired with the cluster holding
/// no trace of it, so it can never land later, and sending again is safe.
#[derive(Clone, Debug)]
pub struct NeverLanded {
    pub signature: String,
}

impl std::fmt::Display for NeverLanded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transaction {} never landed", self.signature)
    }
}

impl std::error::Error for NeverLanded {}

/// A send the node stopped answering about before it landed or its
/// blockhash expired: it may still land, and its fee be paid, later.
#[derive(Clone, Debug)]
pub struct MayStillLand {
    pub signature: String,
}

impl std::fmt::Display for MayStillLand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "transaction {} may still land: the node stopped answering",
            self.signature
        )
    }
}

enum Watch {
    Landed,
    Waiting,
    Expired,
}

/// A transaction that landed, and what it cost.
#[derive(Clone, Debug)]
pub struct Landed {
    pub record: TransactionRecord,
    pub payer: Pubkey,
    /// The transactions that made or grew the payer's lookup table for it
    /// first, if it needed one: part of what it cost.
    pub setup: Vec<TransactionRecord>,
}

impl Landed {
    pub fn signature(&self) -> &str {
        &self.record.signature
    }

    /// The fees, in lamports: its own and its table set-up's.
    pub fn fee(&self) -> u64 {
        self.record.fee + self.setup.iter().map(|record| record.fee).sum::<u64>()
    }

    /// Everything the payer's lamports fell by: the fees and the rent of
    /// every account it paid for (a lookup table among them), less rent
    /// refunded to it.
    pub fn payer_spent(&self) -> i128 {
        self.record.spent_by(&self.payer)
            + self
                .setup
                .iter()
                .map(|record| record.spent_by(&self.payer))
                .sum::<i128>()
    }

    /// The rent the payer put into accounts (negative: took back).
    pub fn rent(&self) -> i128 {
        self.payer_spent() - self.record.fee as i128
    }
}

/// A connection to one Solana cluster.
#[derive(Clone)]
pub struct SolanaClient {
    pub rpc: Rpc,
    pub key: ChainKey,
    /// The cluster's name: the chain key's id (`localnet`).
    pub cluster: String,
    pub genesis_hash: String,
    signing: Arc<Signing>,
    /// Each paying key's lookup tables, as last read: `None` until they have
    /// been looked for on the cluster. One key's tables are set up one send
    /// at a time.
    tables: Arc<Mutex<HashMap<Pubkey, KeyTables>>>,
}

type KeyTables = Arc<tokio::sync::Mutex<Option<Vec<Table>>>>;

impl SolanaClient {
    /// Connect, waiting for the cluster to answer, and check it is the
    /// cluster `key` names. The keys are the network's
    /// ([`Signing::for_network`]): every role's locally and with the team's
    /// mnemonic, the keeper's alone with its own key, none on mainnet, where
    /// the client only reads.
    pub async fn connect(
        network: NetworkName,
        key: &ChainKey,
        config: &ChainConfig,
    ) -> Result<Self> {
        Self::connect_signing(key, config, Signing::for_network(network)?).await
    }

    /// The same, signing with these role keys (or none).
    pub async fn connect_with(
        key: &ChainKey,
        config: &ChainConfig,
        keys: Option<Arc<RoleKeys>>,
    ) -> Result<Self> {
        let signing = keys.map_or(Signing::ReadOnly, Signing::Roles);
        Self::connect_signing(key, config, signing).await
    }

    /// The same, signing as `signing` says.
    pub async fn connect_signing(
        key: &ChainKey,
        config: &ChainConfig,
        signing: Signing,
    ) -> Result<Self> {
        if key.kind != ChainKind::Solana {
            bail!("{key} is not a Solana cluster");
        }
        let rpc = Rpc::new(&config.rpc, &config.label)?;
        let started = Instant::now();
        let genesis_hash = loop {
            match rpc.genesis_hash().await {
                Ok(hash) => break hash,
                Err(error) if started.elapsed() < CONNECT_TIMEOUT => {
                    tracing::debug!(chain = %key, error = format!("{error:#}"), "waiting for the cluster");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(error) => {
                    return Err(error.context(format!("{} never answered", config.label)));
                }
            }
        };
        check_cluster(&key.id, &genesis_hash, &config.label)?;
        Ok(Self {
            rpc,
            key: key.clone(),
            cluster: key.id.clone(),
            genesis_hash,
            signing: Arc::new(signing),
            tables: Arc::default(),
        })
    }

    /// The platform's role keys, where this client signs as every role.
    pub fn keys(&self) -> Result<&RoleKeys> {
        match &*self.signing {
            Signing::Roles(keys) => Ok(keys),
            Signing::Keeper(_) => bail!(
                "{} signs as the keeper alone here: the other roles' keys come from {}",
                self.key,
                super::keys::TESTNET_SOLANA_MNEMONIC
            ),
            Signing::ReadOnly => bail!("{} is read-only here: no role keys", self.key),
        }
    }

    /// Who signs on this cluster.
    pub fn signing(&self) -> &Signing {
        &self.signing
    }

    pub fn role(&self, role: Role) -> Result<&Keypair> {
        self.signing.get(role).ok_or_else(|| {
            anyhow!(
                "this client holds no key for {} on {}: on devnet a role signs with {}, or the keeper alone with {}",
                role.label(),
                self.key,
                super::keys::TESTNET_SOLANA_MNEMONIC,
                super::keys::TESTNET_KEEPER_SOLANA_KEY
            )
        })
    }

    pub fn address(&self, role: Role) -> Result<Pubkey> {
        Ok(self.role(role)?.pubkey())
    }

    /// The value digests on this cluster are bound to.
    pub fn cluster_value(&self) -> [u8; 32] {
        shonin_common::digest::cluster_value(&self.cluster)
    }

    // Deployments -------------------------------------------------------------

    /// Check a deployment is of this cluster and of the programs this client
    /// was built with: the same cluster, the same genesis (a local cluster
    /// started again is a new one), and every program it names at the id
    /// compiled in here and present on the cluster.
    pub async fn check_deployments(&self, deployments: &SolanaDeployments) -> Result<()> {
        if !deployments.cluster.is_empty() && deployments.cluster != self.cluster {
            bail!(
                "the deployment is for cluster {}, and this is {}",
                deployments.cluster,
                self.cluster
            );
        }
        if !deployments.genesis_hash.is_empty() && deployments.genesis_hash != self.genesis_hash {
            bail!(
                "the deployment is for an earlier {} (another genesis): deploy again",
                self.rpc.label()
            );
        }
        let compiled = programs::compiled();
        let mut named = Vec::new();
        for (name, id) in &deployments.programs {
            let Some((_, expected)) = compiled.iter().find(|(known, _)| known == name) else {
                continue;
            };
            let id: Pubkey = super::rpc::parse_pubkey(id)?;
            if id != *expected {
                bail!(
                    "{name} is deployed at {id}, and this build expects it at {expected}: \
                     build against the program keypairs the cluster was deployed with"
                );
            }
            named.push(id);
        }
        for (id, account) in named.iter().zip(self.rpc.accounts(&named).await?) {
            if !account.is_some_and(|account| account.executable) {
                bail!("program {id} is not on {}", self.rpc.label());
            }
        }
        Ok(())
    }

    /// The deployment in `file`, once it is there and checks out against
    /// this cluster.
    pub async fn wait_for_deployments(&self, file: &Path) -> Result<SolanaDeployments> {
        let mut said = false;
        loop {
            match std::fs::read_to_string(file) {
                Ok(text) => {
                    let deployments = SolanaDeployments::from_json(&text)
                        .with_context(|| format!("reading {}", file.display()))?;
                    match self.check_deployments(&deployments).await {
                        Ok(()) if deployments.roles.is_some() => return Ok(deployments),
                        Ok(()) => {}
                        Err(error) if !said => {
                            tracing::info!(file = %file.display(), error = format!("{error:#}"), "waiting for a deployment of this cluster");
                            said = true;
                        }
                        Err(_) => {}
                    }
                }
                Err(_) if !said => {
                    tracing::info!(file = %file.display(), "waiting for the deployment file");
                    said = true;
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    // Reading -----------------------------------------------------------------

    pub async fn account(&self, address: &Pubkey) -> Result<Option<Account>> {
        self.rpc.account(address).await
    }

    /// An Anchor account of type `T` at `address`, if one is there.
    pub async fn load<T: AccountDeserialize>(&self, address: &Pubkey) -> Result<Option<T>> {
        let Some(account) = self.rpc.account(address).await? else {
            return Ok(None);
        };
        if account.data.is_empty() {
            return Ok(None);
        }
        T::try_deserialize(&mut &account.data[..])
            .map(Some)
            .map_err(|error| anyhow!("the account at {address} is not what was expected: {error}"))
    }

    /// The cluster's clock, in seconds: what every program reads time from.
    pub async fn now(&self) -> Result<u64> {
        let clock = self
            .rpc
            .account(&anchor_lang::solana_program::sysvar::clock::ID)
            .await?
            .ok_or_else(|| anyhow!("the cluster has no clock"))?;
        let bytes: [u8; 8] = clock
            .data
            .get(32..40)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| anyhow!("the clock is not a clock"))?;
        Ok(i64::from_le_bytes(bytes).max(0) as u64)
    }

    pub async fn lamports(&self, address: &Pubkey) -> Result<u64> {
        self.rpc.balance(address).await
    }

    /// A token account's balance, or zero where there is none.
    pub async fn token_balance(&self, account: &Pubkey) -> Result<u64> {
        Ok(self
            .rpc
            .account(account)
            .await?
            .filter(|account| account.data.len() >= 72)
            .map_or(0, |account| {
                u64::from_le_bytes(account.data[64..72].try_into().expect("8 bytes"))
            }))
    }

    /// A landed transaction, whole, if the node still has it.
    pub async fn transaction(&self, signature: &str) -> Result<Option<TransactionRecord>> {
        match self.rpc.transaction(signature).await? {
            Some(value) if !value.is_null() => {
                Ok(Some(TransactionRecord::from_json(signature, &value)?))
            }
            _ => Ok(None),
        }
    }

    // Sending -----------------------------------------------------------------

    /// Send `ixs` and wait until they land, the first signer paying. A
    /// transaction the cluster refuses or that fails comes back as
    /// [`TransactionFailed`], with its logs. One too large for the wire by
    /// address goes through the payer's lookup table, made or grown first
    /// (and paid for by the payer) when it doesn't hold the accounts yet.
    pub async fn send(&self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> Result<Landed> {
        let payer = *signers
            .first()
            .ok_or_else(|| anyhow!("a transaction needs someone to pay for it"))?;
        let ixs = with_budget(ixs);
        let (table, setup) = self.table_for(&ixs, &payer.pubkey(), Some(payer)).await?;
        let (blockhash, last_valid) = self.blockhash().await?;
        let message = tables::compile(&ixs, &payer.pubkey(), table.as_ref(), blockhash)?;
        let mut landed = self.land(signed(message, signers)?, last_valid).await?;
        landed.setup = setup;
        Ok(landed)
    }

    async fn blockhash(&self) -> Result<(Hash, u64)> {
        let (blockhash, last_valid) = self.rpc.latest_blockhash().await?;
        let blockhash: Hash = blockhash
            .parse()
            .map_err(|_| anyhow!("the node's blockhash is not a hash"))?;
        Ok((blockhash, last_valid))
    }

    /// Hand a signed transaction to the node and watch it until it lands,
    /// its blockhash (valid until block `last_valid`) expires, or the node
    /// stops answering for too long.
    async fn land(&self, tx: VersionedTransaction, last_valid: u64) -> Result<Landed> {
        let wire = bincode::serialize(&tx).context("encoding the transaction")?;
        if wire.len() > PACKET_DATA_SIZE {
            bail!(
                "a transaction of {} bytes is over the wire's {PACKET_DATA_SIZE}",
                wire.len()
            );
        }
        let payer = tx.message.static_account_keys()[0];
        let signature = tx.signatures[0].to_string();
        self.submit(&wire).await?;
        let started = Instant::now();
        let mut last_sent = Instant::now();
        loop {
            tokio::time::sleep(POLL).await;
            match self.status_of(&signature, last_valid).await {
                Ok(Watch::Landed) => {
                    let record = self.landed_record(&signature).await?;
                    if let Some(err) = record.err.clone() {
                        return Err(
                            TransactionFailed::from_err(Some(signature), err, record.logs).into(),
                        );
                    }
                    return Ok(Landed {
                        record,
                        payer,
                        setup: Vec::new(),
                    });
                }
                Ok(Watch::Expired) => return Err(NeverLanded { signature }.into()),
                Ok(Watch::Waiting) => {}
                // The node didn't answer: the send may still land, so it is
                // watched until it does, its blockhash expires, or the
                // node stays silent too long to say.
                Err(error) if started.elapsed() > LANDING_TIMEOUT => {
                    return Err(error.context(MayStillLand { signature }));
                }
                Err(_) => {}
            }
            if last_sent.elapsed() > RESEND {
                // The same bytes, so the same signature: it lands once.
                let _ = self.submit(&wire).await;
                last_sent = Instant::now();
            }
        }
    }

    async fn status_of(&self, signature: &str, last_valid: u64) -> Result<Watch> {
        let status = self
            .rpc
            .statuses(&[signature.to_string()])
            .await?
            .pop()
            .flatten();
        if status.is_some_and(|status| {
            status
                .confirmation_status
                .as_deref()
                .is_some_and(|level| level == "confirmed" || level == "finalized")
        }) {
            return Ok(Watch::Landed);
        }
        if self.rpc.block_height().await? > last_valid {
            // Asked once more after the expiry, so a send that landed in the
            // last valid block is not called lost.
            let late = self
                .rpc
                .statuses(&[signature.to_string()])
                .await?
                .pop()
                .flatten();
            return Ok(if late.is_some() {
                Watch::Waiting
            } else {
                Watch::Expired
            });
        }
        Ok(Watch::Waiting)
    }

    async fn submit(&self, wire: &[u8]) -> Result<()> {
        match self.rpc.send(wire).await {
            Ok(_) => Ok(()),
            Err(error) => match error.downcast_ref::<RpcError>() {
                // The node's preflight ran it and it failed.
                Some(rpc) if !rpc.data["err"].is_null() => {
                    let logs = rpc.data["logs"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|line| line.as_str().map(str::to_string))
                        .collect();
                    Err(TransactionFailed::from_err(None, rpc.data["err"].clone(), logs).into())
                }
                // Already held: sending again is how an unconfirmed send is
                // kept alive, and this one is on its way.
                Some(rpc) if rpc.message.contains("already been processed") => Ok(()),
                _ => Err(error),
            },
        }
    }

    /// A confirmed transaction's record, which the node may take a moment to
    /// have after reporting its status.
    async fn landed_record(&self, signature: &str) -> Result<TransactionRecord> {
        for _ in 0..50 {
            if let Some(record) = self.transaction(signature).await? {
                return Ok(record);
            }
            tokio::time::sleep(POLL).await;
        }
        bail!("transaction {signature} landed, and the node never gave its record")
    }

    /// How many bytes `ixs` take on the wire as one transaction `payer`
    /// pays for, with its signatures, as it would go: by address, or through
    /// a lookup table when that doesn't fit. Over [`PACKET_DATA_SIZE`] it
    /// can't be sent.
    pub fn wire_size(&self, ixs: Vec<Instruction>, payer: &Pubkey) -> Result<usize> {
        tables::sent_size(&with_budget(ixs), payer)
    }

    /// Run `ixs` without keeping anything, for what they return and log.
    pub async fn simulate(&self, ixs: Vec<Instruction>, payer: &Pubkey) -> Result<Simulated> {
        self.simulate_reading(ixs, payer, &[]).await
    }

    /// The same, with `addresses` as the transaction would leave them.
    ///
    /// It is compiled as the send would be: through the payer's lookup table
    /// when it doesn't fit by address. Where this client holds the payer's
    /// key (a role's), a table it needs is made or grown first, as the send
    /// would; for any other payer it must already hold the accounts.
    pub async fn simulate_reading(
        &self,
        ixs: Vec<Instruction>,
        payer: &Pubkey,
        addresses: &[Pubkey],
    ) -> Result<Simulated> {
        let ixs = with_budget(ixs);
        let own = self.own_key(payer);
        let (table, _) = self.table_for(&ixs, payer, own).await?;
        let message = tables::compile(&ixs, payer, table.as_ref(), Hash::default())?;
        let unsigned = VersionedTransaction {
            signatures: vec![Default::default(); message.header().num_required_signatures as usize],
            message,
        };
        let wire = bincode::serialize(&unsigned).context("encoding the transaction")?;
        let simulated = self.rpc.simulate_reading(&wire, addresses).await?;
        if let Some(err) = simulated.err.clone() {
            return Err(TransactionFailed::from_err(None, err, simulated.logs).into());
        }
        Ok(simulated)
    }

    /// The role key at `address`, if this client holds it.
    fn own_key(&self, address: &Pubkey) -> Option<&Keypair> {
        self.signing.find(address)
    }

    // Lookup tables -------------------------------------------------------------

    /// The table `payer`'s transaction of `ixs` goes through, if it doesn't
    /// fit by address, read and checked just now and ready to use; and the
    /// transactions that made or grew it, if `builder` (the payer's key) had
    /// to. Without the key, a table not ready is an error.
    async fn table_for(
        &self,
        ixs: &[Instruction],
        payer: &Pubkey,
        builder: Option<&Keypair>,
    ) -> Result<(Option<Table>, Vec<TransactionRecord>)> {
        if tables::fits(&tables::by_address(ixs, payer, Hash::default())) {
            return Ok((None, Vec::new()));
        }
        let wanted = tables::tableable(ixs, payer);
        if wanted.len() > tables::MOST_ADDRESSES {
            bail!(
                "a transaction naming {} accounts is more than one lookup table holds",
                wanted.len()
            );
        }
        let held = {
            let mut all = self.tables.lock().map_err(|_| anyhow!("a poisoned lock"))?;
            all.entry(*payer).or_default().clone()
        };
        let mut held = held.lock().await;
        if held.is_none() {
            *held = Some(self.find_tables(payer).await);
        }
        let known = held.get_or_insert_with(Vec::new);
        let (readable, unread) = self.read_tables(payer, known).await?;
        *known = readable.iter().chain(&unread).cloned().collect();

        if let Some(table) = readable.iter().find(|table| table.ready_for(&wanted)) {
            return Ok((Some(table.clone()), Vec::new()));
        }
        if let Some(table) = readable.iter().find(|table| table.holds(&wanted)).cloned() {
            let table = self.warmed_up(payer, &table, table.extended_in).await?;
            replace(known, &table);
            return Ok((Some(table), Vec::new()));
        }
        if !unread.is_empty() {
            let unread: Vec<String> = unread
                .iter()
                .map(|table| table.address.to_string())
                .collect();
            bail!(
                "{payer}'s lookup tables {} could not be read just now: no new one is made while they may still be there",
                unread.join(", ")
            );
        }
        let Some(builder) = builder else {
            bail!("the transaction needs a lookup table {payer} has not made yet");
        };

        let mut setup = Vec::new();
        let (address, before, adding) = match tables::to_extend(&readable, &wanted) {
            Some(table) => (
                table.address,
                table.addresses.clone(),
                table.missing(&wanted),
            ),
            None => {
                let recent = self.rpc.slot().await?;
                let (create, address) = tables::create_lookup_table(*payer, *payer, recent);
                let first: Vec<Pubkey> =
                    wanted.iter().take(tables::ADDED_AT_ONCE).copied().collect();
                let extend = tables::extend_lookup_table(address, *payer, Some(*payer), first);
                setup.push(self.set_up(vec![create, extend], builder).await?);
                (
                    address,
                    Vec::new(),
                    wanted[wanted.len().min(tables::ADDED_AT_ONCE)..].to_vec(),
                )
            }
        };
        for chunk in adding.chunks(tables::ADDED_AT_ONCE) {
            let extend = tables::extend_lookup_table(address, *payer, Some(*payer), chunk.to_vec());
            setup.push(self.set_up(vec![extend], builder).await?);
        }
        let last = setup.iter().map(|record| record.slot).max().unwrap_or(0);
        let mut expected = before;
        expected.extend(
            wanted
                .iter()
                .filter(|key| !expected.contains(key))
                .copied()
                .collect::<Vec<_>>(),
        );
        // Known before it is waited on, so a wait that ends without reading
        // it leaves the next send to read it rather than make another.
        let grown = Table {
            address,
            addresses: expected,
            usable: 0,
            extended_in: last,
        };
        replace(known, &grown);
        let table = self.warmed_up(payer, &grown, last).await?;
        if !table.ready_for(&wanted) {
            bail!(
                "the lookup table {address} was grown and does not hold the transaction's accounts"
            );
        }
        replace(known, &table);
        tracing::info!(%payer, table = %address, addresses = table.addresses.len(), "a lookup table grown");
        Ok((Some(table), setup))
    }

    /// Send a lookup table's set-up, `payer` paying.
    async fn set_up(&self, ixs: Vec<Instruction>, payer: &Keypair) -> Result<TransactionRecord> {
        let (blockhash, last_valid) = self.blockhash().await?;
        let message = tables::by_address(&ixs, &payer.pubkey(), blockhash);
        let landed = self
            .land(signed(message, &[payer])?, last_valid)
            .await
            .context("setting up a lookup table")?;
        Ok(landed.record)
    }

    /// `payer`'s tables on the cluster, found by their authority: none if
    /// the node won't search (some won't), and the first send that needs
    /// one makes a new one.
    async fn find_tables(&self, payer: &Pubkey) -> Vec<Table> {
        let found = match self
            .rpc
            .program_accounts(&tables::PROGRAM, &tables::owned_by(payer))
            .await
        {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(%payer, error = format!("{error:#}"), "the node would not find the key's lookup tables: a new one will be made");
                return Vec::new();
            }
        };
        let slot = self.rpc.slot().await.unwrap_or(0);
        found
            .into_iter()
            .filter_map(|(address, account)| Table::read(address, &account, payer, slot).ok())
            .collect()
    }

    /// `known` as the cluster holds each table now, with every address it
    /// held before still at its index; and those that could not be read,
    /// as they were. One that is not `payer`'s, deactivated or changed is
    /// dropped and never used again. One the node answers nothing for is
    /// read again a few times, and then looked for among `payer`'s tables:
    /// it is dropped only if that search finds it gone, and kept unread if
    /// the node won't search.
    async fn read_tables(
        &self,
        payer: &Pubkey,
        known: &[Table],
    ) -> Result<(Vec<Table>, Vec<Table>)> {
        if known.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let addresses: Vec<Pubkey> = known.iter().map(|table| table.address).collect();
        let mut accounts = self.rpc.accounts(&addresses).await?;
        for _ in 1..TABLE_READS {
            let missing: Vec<usize> = (0..accounts.len())
                .filter(|at| accounts[*at].is_none())
                .collect();
            if missing.is_empty() {
                break;
            }
            tokio::time::sleep(POLL).await;
            let again: Vec<Pubkey> = missing.iter().map(|at| addresses[*at]).collect();
            for (at, account) in missing.into_iter().zip(self.rpc.accounts(&again).await?) {
                accounts[at] = account;
            }
        }
        let mut searched = false;
        if accounts.iter().any(Option::is_none) {
            match self
                .rpc
                .program_accounts(&tables::PROGRAM, &tables::owned_by(payer))
                .await
            {
                Ok(found) => {
                    searched = true;
                    for (address, account) in found {
                        if let Some(at) = addresses.iter().position(|known| *known == address)
                            && accounts[at].is_none()
                        {
                            accounts[at] = Some(account);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%payer, error = format!("{error:#}"), "the node would not find the key's lookup tables")
                }
            }
        }
        let slot = self.rpc.slot().await?;
        let (mut readable, mut unread) = (Vec::new(), Vec::new());
        for (was, account) in known.iter().zip(accounts) {
            let Some(account) = account else {
                if searched {
                    tracing::warn!(%payer, table = %was.address, "a lookup table is gone: neither read nor found");
                } else {
                    tracing::warn!(%payer, table = %was.address, "a lookup table could not be read: kept");
                    unread.push(was.clone());
                }
                continue;
            };
            let read = Table::read(was.address, &account, payer, slot)
                .and_then(|table| table.agrees_with(&was.addresses).map(|()| table));
            match read {
                Ok(table) => readable.push(table),
                Err(error) => {
                    tracing::warn!(%payer, error = format!("{error:#}"), "a lookup table refused")
                }
            }
        }
        Ok((readable, unread))
    }

    /// `table` read again once the cluster is past `slot`, the slot it was
    /// last grown in, so everything in it can be looked up; checked to hold
    /// what it held.
    async fn warmed_up(&self, payer: &Pubkey, table: &Table, slot: u64) -> Result<Table> {
        let started = Instant::now();
        loop {
            let now = self.rpc.slot().await?;
            // A node behind the cluster answers nothing for a table that
            // is there: it is asked again until the wait is over.
            if now > slot
                && let Some(account) = self.rpc.account(&table.address).await?
            {
                let read = Table::read(table.address, &account, payer, now)?;
                read.agrees_with(&table.addresses)?;
                if read.usable >= table.addresses.len() {
                    return Ok(read);
                }
            }
            if started.elapsed() > WARM_UP_TIMEOUT {
                bail!(
                    "the lookup table {} never became usable: the cluster stayed at slot {now}, or the node never read it",
                    table.address
                );
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Coin from the cluster's own faucet, waited for: development clusters
    /// only.
    pub async fn airdrop(&self, to: &Pubkey, lamports: u64) -> Result<()> {
        let signature = self.rpc.airdrop(to, lamports).await?;
        for _ in 0..100 {
            if let Some(Some(status)) = self
                .rpc
                .statuses(std::slice::from_ref(&signature))
                .await?
                .pop()
                && status.confirmation_status.is_some()
            {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        bail!("an airdrop to {to} never landed")
    }
}

/// `message` signed by `signers`, each once, the payer first among them.
fn signed(message: VersionedMessage, signers: &[&Keypair]) -> Result<VersionedTransaction> {
    let mut unique: Vec<&Keypair> = Vec::new();
    for signer in signers {
        if !unique.iter().any(|seen| seen.pubkey() == signer.pubkey()) {
            unique.push(signer);
        }
    }
    VersionedTransaction::try_new(message, &unique)
        .map_err(|error| anyhow!("signing the transaction: {error}"))
}

/// `table` in place of the one of the same address in `known`.
fn replace(known: &mut Vec<Table>, table: &Table) {
    match known.iter_mut().find(|held| held.address == table.address) {
        Some(held) => *held = table.clone(),
        None => known.push(table.clone()),
    }
}

/// Every transaction asks for the most compute it may use, and one calling
/// a vault or a station the heap their allocator grows into.
fn with_budget(ixs: Vec<Instruction>) -> Vec<Instruction> {
    let mut all = vec![ComputeBudgetInstruction::set_compute_unit_limit(
        COMPUTE_UNITS,
    )];
    if ixs
        .iter()
        .any(|ix| ix.program_id == shonin_vault::ID || ix.program_id == shonin_station::ID)
    {
        all.push(ComputeBudgetInstruction::request_heap_frame(
            shonin_station::heap::MAX_HEAP as u32,
        ));
    }
    all.extend(ixs);
    all
}

/// Whether a cluster answering with `genesis` may be the cluster named
/// `cluster`: a public cluster by its genesis, and a local one by not being
/// any public cluster, so a local key pointed at a public node is refused.
pub fn check_cluster(cluster: &str, genesis: &str, label: &str) -> Result<()> {
    match PUBLIC_CLUSTERS.iter().find(|(name, _)| *name == cluster) {
        Some((_, expected)) if *expected != genesis => {
            bail!("{label} is not {cluster}: its genesis is {genesis}")
        }
        Some(_) => Ok(()),
        None => match PUBLIC_CLUSTERS.iter().find(|(_, hash)| *hash == genesis) {
            Some((public, _)) => {
                bail!("{label} is the public cluster {public}, not the local {cluster}")
            }
            None => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_cluster_name_refuses_a_public_genesis() {
        assert!(
            check_cluster(
                "localnet",
                "AbfHL7AeBtDMe7HtEA3cXJgzY8cdpa4MTW7ZXk9byEJf",
                "l"
            )
            .is_ok()
        );
        assert!(
            check_cluster(
                "localnet",
                "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
                "l"
            )
            .is_err()
        );
        assert!(
            check_cluster(
                "devnet",
                "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
                "d"
            )
            .is_ok()
        );
        assert!(
            check_cluster(
                "devnet",
                "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
                "d"
            )
            .is_err()
        );
    }

    #[test]
    fn a_program_error_code_is_read_from_the_node_s_error() {
        let err = serde_json::json!({"InstructionError": [2, {"Custom": 6023}]});
        let failed = TransactionFailed::from_err(None, err, vec!["one".into()]);
        assert_eq!(failed.custom, Some(6023));
        assert!(failed.to_string().contains("6023"));
    }
}
