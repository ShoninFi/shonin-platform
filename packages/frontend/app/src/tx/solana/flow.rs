//! Every depositor flow on a Solana vault, as a small state machine whose
//! states are the EVM flows' and the canvas's: checking → awaiting signature
//! → sent → requested (the valuation countdown) → priced → done, or
//! rejected, failed, moved.
//!
//! Each flow is one async function over the [`SolReader`] and [`SolSigner`]
//! ports and the EVM side's [`Clock`], reporting every step it reaches.
//! Rules every flow keeps:
//!
//! - Every instruction comes from `chain::solana_ix`'s builders, which the
//!   chain crate checks byte for byte against the programs; nothing is
//!   hand-built here.
//! - The vault is read from the cluster first, and its keys (its station,
//!   its dollar accounts, its author and platform) are worked out from what
//!   its account says, then checked against the account itself: the vault's
//!   own address, its station's and its dollar account's must be the ones
//!   the program derives, or nothing is built.
//! - The endpoint is checked to be the vault's cluster, and the account and
//!   the wallet's cluster are checked immediately before every signature; a
//!   flow built for an account that changed is cancelled.
//! - Each transaction is simulated before the wallet is asked, so one that
//!   would fail is refused in words and never offered for signing.
//! - The deposit statement is the deposit: `request_deposit`'s `statement`
//!   argument names the hash of the restricted list read from the vault's
//!   own account and shown to the person, and is checked to be the list the
//!   vault holds now. Signing the transaction is making the statement.
//! - A transaction the app sends itself is checked to be exactly the one
//!   built, signed by the account, before it is sent.
//! - Success is reported only once the cluster has confirmed the
//!   transaction, and the outcome is read from the accounts and the vault's
//!   own events, never assumed. A transaction whose blockhash expired is
//!   reported as such (nothing moved); it is never sent again by itself.
//! - Once a transaction may have gone out (the node's answer to the app's
//!   own send was lost, or a look at its status went unanswered), it is
//!   followed by its signature until it lands or its blockhash can no
//!   longer land it, and an error after it landed says it went through:
//!   nothing offers to send it again while it may still land.
//! - A transaction too large for one legacy transaction is never offered:
//!   the pages measure it first ([`fits`]).

use chain::solana_ix::accounts::{
    self, Claim, ClaimState, Event, Holder, Request, RequestKind, Vault,
};
use chain::solana_ix::budget::{open_dollars, with_budget};
use chain::solana_ix::transaction::Unsigned;
use chain::solana_ix::vault::{self as ix, Groups, Held, VaultKeys};
use chain::solana_ix::{Hash, Instruction, Pubkey, address, ids, read};

use super::error::{Reason, SolError};
use super::port::{Record, Sig, Signed, Simulated, SolReader, SolSigner};
use crate::tx::flow::{Cancel, Pace};
use crate::tx::port::Clock;
use crate::tx::read::{Timers, live_slot};

/// What the wallet is being asked for, for the panel's wallet step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// The deposit, which is also the statement.
    Deposit {
        amount: u64,
    },
    Withdraw {
        shares: u128,
    },
    SettleClaim {
        claim: u64,
    },
    TakeCash,
    ClaimInstead,
    CollectUnits,
    CollectHeld,
    /// A closed vault's payout.
    Closed,
    /// One of the author's (`tx::solana::author`).
    Author(super::author::AuthorPrompt),
}

/// A request waiting for its valuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Countdown {
    pub request: u64,
    pub address: Pubkey,
    pub signature: Sig,
    /// When the request landed, on the cluster's clock.
    pub asked_at: u64,
    /// The valuation time it will be priced at now.
    pub slot: u64,
    /// The time it was moved from, when a valuation wasn't recorded in time.
    pub moved_from: Option<u64>,
}

/// Where a flow is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Checking,
    Signing(Prompt),
    Sent {
        prompt: Prompt,
        signature: Sig,
    },
    Requested(Countdown),
    /// Priced; the keeper settles it.
    Priced {
        countdown: Countdown,
        at: u64,
    },
    Done(Outcome),
    Failed(SolError),
}

/// How a flow ended well.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Deposited {
        assets: u64,
        shares: u128,
        at: u64,
        signature: Sig,
    },
    Refunded {
        assets: u64,
        signature: Sig,
    },
    Paid {
        shares: u128,
        assets: u64,
        at: Option<u64>,
        signature: Sig,
    },
    Booked {
        claim: u64,
        shares: u128,
        signature: Sig,
    },
    Claim {
        claim: u64,
        state: ClaimState,
        paid: u64,
        signature: Sig,
    },
    TookCash {
        assets: u64,
        units: u128,
        signature: Sig,
    },
    ClaimMade {
        claim: u64,
        signature: Sig,
    },
    Collected {
        assets: u64,
        signature: Sig,
    },
    /// An author's change, landed.
    Changed {
        signature: Sig,
    },
}

/// A confirmed transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Landed {
    pub signature: Sig,
    pub slot: u64,
}

/// The cluster a flow acts on, as the network config gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    /// `localnet`, `devnet`, `mainnet-beta`.
    pub name: String,
    /// The Wallet Standard's name for it: `solana:devnet`.
    pub chain: String,
    /// The dollar the config names.
    pub mint: Pubkey,
    /// The endpoint is this machine's (a local cluster has no known genesis).
    pub local_endpoint: bool,
}

/// How long past a blockhash's last valid height a transaction a wallet
/// sent itself is still watched: the wallet may have given it a newer
/// blockhash than the one it was built with.
const WALLET_SENT_MARGIN: u64 = 300;

/// The genesis hashes of the public clusters.
const GENESIS: [(&str, &str); 3] = [
    ("devnet", "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"),
    ("testnet", "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY"),
    (
        "mainnet-beta",
        "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
    ),
];

/// Everything one flow acts on, fixed when it starts.
pub struct Ctx<'a, R, S, C> {
    pub reader: &'a R,
    pub signer: &'a S,
    pub clock: &'a C,
    pub cluster: &'a Cluster,
    pub vault: Pubkey,
    /// The account the flow was built for.
    pub account: Pubkey,
    pub cancel: Cancel,
    pub pace: Pace,
    pub on_step: &'a dyn Fn(Step),
}

/// The vault as its account holds it, and its keys, checked.
pub struct Loaded {
    pub vault: Vault,
    pub keys: VaultKeys,
}

impl<R: SolReader, S: SolSigner, C: Clock> Ctx<'_, R, S, C> {
    pub(crate) fn step(&self, step: Step) {
        (self.on_step)(step);
    }

    /// The endpoint answers for the vault's cluster: a public cluster's by
    /// its genesis hash (`mainnet` and `mainnet-beta` are one cluster); a
    /// local cluster only through this machine's endpoint, and never one
    /// whose genesis is a public cluster's.
    pub(crate) async fn check_endpoint(&self) -> Result<(), SolError> {
        let name = match self.cluster.name.as_str() {
            "mainnet" => "mainnet-beta",
            other => other,
        };
        let known = GENESIS
            .iter()
            .find(|(cluster, _)| *cluster == name)
            .map(|(_, hash)| *hash);
        let genesis = self.reader.genesis().await?;
        match known {
            Some(expected) if genesis == expected => Ok(()),
            None if self.cluster.local_endpoint
                && name == "localnet"
                && !GENESIS.iter().any(|(_, hash)| *hash == genesis) =>
            {
                Ok(())
            }
            _ => Err(SolError::WrongEndpoint),
        }
    }

    /// The vault, read and checked: owned by the vault program, its keys the
    /// ones the program derives for its strategy, its dollar the config's.
    pub(crate) async fn load(&self) -> Result<Loaded, SolError> {
        let account = self
            .reader
            .account(&self.vault)
            .await?
            .ok_or(SolError::WrongAccount)?;
        if account.owner != ids::VAULT {
            return Err(SolError::WrongAccount);
        }
        let vault = Vault::read(&account.data).ok_or(SolError::WrongAccount)?;
        let keys = VaultKeys::new(vault.strategy_id, vault.mint, vault.author, vault.platform);
        if keys.vault != self.vault
            || keys.station != vault.station
            || keys.vault_dollars != vault.dollars
            || vault.mint != self.cluster.mint
        {
            return Err(SolError::WrongAccount);
        }
        Ok(Loaded { vault, keys })
    }

    /// Checked immediately before every signature: not cancelled, the same
    /// account, and the wallet offering the vault's cluster.
    pub(crate) fn guard(&self) -> Result<(), SolError> {
        if self.cancel.is_cancelled() {
            return Err(SolError::Cancelled);
        }
        match self.signer.account() {
            None => return Err(SolError::NotConnected),
            Some(account) if account != self.account => return Err(SolError::AccountChanged),
            Some(_) => {}
        }
        if !self.signer.supports(&self.cluster.chain) {
            return Err(SolError::WrongCluster);
        }
        Ok(())
    }

    /// The one path that sends: build the transaction with a fresh
    /// blockhash and the compute budget, simulate it (so one that would fail
    /// is refused before the wallet is asked), [`Self::guard`], ask the
    /// wallet, check what it hands back, send it, and wait for the cluster
    /// to confirm it. Success is only ever a confirmed transaction.
    pub(crate) async fn transact(
        &self,
        prompt: Prompt,
        ixs: Vec<Instruction>,
    ) -> Result<Landed, SolError> {
        let blockhash = self.reader.blockhash().await?;
        let unsigned = as_sent(&self.account, ixs, blockhash.hash).ok_or(SolError::TooLarge)?;
        if let Simulated::Failed(failure) = self.reader.simulate(&unsigned.wire()).await? {
            return Err(SolError::WouldFail {
                reason: Reason::from_failure(&failure),
            });
        }
        self.guard()?;
        self.step(Step::Signing(prompt.clone()));
        let signed = self.signer.sign(&unsigned, &self.cluster.chain).await?;
        // A late answer for a flow cancelled meanwhile (or built for an
        // account the wallet has since left) is never sent on.
        if self.signer.account() != Some(self.account) {
            return Err(SolError::AccountChanged);
        }
        if self.cancel.is_cancelled() {
            return Err(SolError::Cancelled);
        }
        let (signature, expiry) = match signed {
            Signed::Sent(signature) => (signature, blockhash.last_valid + WALLET_SENT_MARGIN),
            Signed::Wire(wire) => {
                let signature = check_signed(&wire, &unsigned, &self.account)?;
                match self.reader.send(&wire).await {
                    // Whatever the node names, the transaction sent is the
                    // one signed, and is known by its signature.
                    Ok(_) => {}
                    // The node ran it first and refused it: nothing was sent.
                    Err(refused @ SolError::WouldFail { .. }) => return Err(refused),
                    // The node's answer was lost (a timeout, a dropped
                    // connection) or made no sense: it may have taken the
                    // transaction all the same, so it is followed by its
                    // signature until its blockhash can no longer land it.
                    Err(_) => {}
                }
                (signature, blockhash.last_valid)
            }
        };
        self.step(Step::Sent { prompt, signature });
        self.confirmed(signature, expiry).await
    }

    /// Wait for the cluster to confirm a sent transaction, or say what
    /// became of it: failed (confirmed with an error), expired (its
    /// blockhash passed without it), or still unseen after a long wait. A
    /// look the node doesn't answer is looked again: once sent, a
    /// transaction is never left as "the network didn't answer", which
    /// would offer to send it again while it may still land.
    pub(crate) async fn confirmed(&self, signature: Sig, expiry: u64) -> Result<Landed, SolError> {
        for _ in 0..self.pace.give_up_after {
            if let Ok(Some(end)) = self.look(signature, expiry).await {
                return end;
            }
            self.clock.sleep(self.pace.poll_millis).await;
        }
        Err(SolError::Stuck { signature })
    }

    /// One look at a sent transaction: how it ended, if it has.
    async fn look(
        &self,
        signature: Sig,
        expiry: u64,
    ) -> Result<Option<Result<Landed, SolError>>, SolError> {
        let mut status = self.reader.status(&signature, false).await?;
        if status.is_none() && self.reader.block_height().await? > expiry {
            // One last look through the whole history before calling it
            // gone: past its last valid height it can never land.
            status = self.reader.status(&signature, true).await?;
            if status.is_none() {
                return Ok(Some(Err(SolError::Expired { signature })));
            }
        }
        // Only a confirmed status is judged: one seen at `processed` may be
        // on a fork the cluster drops, and land (or fail) differently.
        let Some(status) = status.filter(|status| status.confirmed) else {
            return Ok(None);
        };
        if let Some(err) = status.err {
            let logs = self
                .reader
                .transaction(&signature)
                .await
                .ok()
                .flatten()
                .map(|record| record.logs)
                .unwrap_or_default();
            return Ok(Some(Err(SolError::Failed {
                signature,
                reason: Some(Reason::from_failure(&super::port::Failure { err, logs })),
            })));
        }
        Ok(Some(Ok(Landed {
            signature,
            slot: status.slot,
        })))
    }

    /// The vault's events in a landed transaction.
    pub(crate) async fn events(&self, signature: &Sig) -> Result<Vec<Event>, SolError> {
        let record = self.record(signature).await?;
        Ok(accounts::events_in(
            &record.logs,
            &record.calls,
            crate::wallet::solana::rpc::base64_decode,
        ))
    }

    async fn record(&self, signature: &Sig) -> Result<Record, SolError> {
        // A node may index a transaction a moment after confirming it.
        for _ in 0..10 {
            if let Some(record) = self.reader.transaction(signature).await? {
                return Ok(record);
            }
            self.clock.sleep(self.pace.poll_millis).await;
        }
        Err(SolError::Network(
            "the transaction landed, but its record couldn't be read; your portfolio shows it"
                .to_string(),
        ))
    }

    /// `owner`'s dollar account, opened first (the account paying its rent)
    /// if it doesn't exist: the vault pays only into it.
    pub(crate) async fn open_if_missing(
        &self,
        owner: &Pubkey,
        ixs: &mut Vec<Instruction>,
    ) -> Result<(), SolError> {
        open_if_missing(self.reader, &self.account, owner, &self.cluster.mint, ixs).await
    }

    /// The dollars `owner` holds.
    pub(crate) async fn dollars_of(&self, owner: &Pubkey) -> Result<u64, SolError> {
        let dollars = address::dollars(owner, &self.cluster.mint);
        Ok(match self.reader.account(&dollars).await? {
            Some(account) => accounts::token_amount(&account.data).ok_or(SolError::WrongAccount)?,
            None => 0,
        })
    }

    /// A free nonce for a new request of `maker`'s: the vault's next id,
    /// which no earlier request of the maker's took, or the next free one.
    pub(crate) async fn nonce(&self, vault: &Vault, maker: &Pubkey) -> Result<u64, SolError> {
        for nonce in (vault.request_count + 1..).take(16) {
            if self
                .reader
                .account(&address::request(&self.vault, maker, nonce))
                .await?
                .is_none()
            {
                return Ok(nonce);
            }
        }
        Err(SolError::Network(
            "no free request address was found".to_string(),
        ))
    }
}

/// `owner`'s dollar account opened first, `payer` paying its rent, if it
/// doesn't exist.
async fn open_if_missing<R: SolReader>(
    reader: &R,
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    ixs: &mut Vec<Instruction>,
) -> Result<(), SolError> {
    if reader
        .account(&address::dollars(owner, mint))
        .await?
        .is_none()
    {
        ixs.push(open_dollars(payer, owner, mint));
    }
    Ok(())
}

/// `ixs` as the app sends them, from `payer`: one legacy transaction with
/// the compute budget in front, or `None` when that is larger than the wire
/// takes. The app keeps no lookup table of its own, so a transaction too
/// large is never offered: the keeper, which sends through its own tables,
/// does what it would have done.
fn as_sent(payer: &Pubkey, ixs: Vec<Instruction>, blockhash: Hash) -> Option<Unsigned> {
    Unsigned::legacy(payer, &with_budget(ixs), blockhash).ok()
}

/// Whether `ixs`, sent by `payer`, fit one transaction from the app.
pub fn fits(payer: &Pubkey, ixs: Vec<Instruction>) -> bool {
    as_sent(payer, ixs, Hash::default()).is_some()
}

/// The signature of a transaction a wallet signed and handed back, once it
/// is checked: exactly the message built, as many signatures as it needs,
/// and the account's verifying over the message.
pub fn check_signed(wire: &[u8], unsigned: &Unsigned, account: &Pubkey) -> Result<Sig, SolError> {
    let (count, mut at) = short_vec(wire).ok_or(SolError::Altered)?;
    let mut signatures = Vec::with_capacity(count);
    for _ in 0..count {
        let signature: [u8; 64] = wire
            .get(at..at + 64)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(SolError::Altered)?;
        signatures.push(signature);
        at += 64;
    }
    if &wire[at..] != unsigned.message() || count != unsigned.signers().len() {
        return Err(SolError::Altered);
    }
    let index = unsigned
        .signers()
        .iter()
        .position(|signer| signer == account)
        .ok_or(SolError::BadSignature)?;
    if !crate::wallet::solana::key::verify(account, unsigned.message(), &signatures[index]) {
        return Err(SolError::BadSignature);
    }
    // A transaction is known by its first signature: the fee payer's.
    Ok(Sig(signatures[0]))
}

/// A length as the wire writes it, and where what follows starts.
fn short_vec(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (i, byte) in bytes.iter().take(3).enumerate() {
        value |= usize::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Run a flow, reporting where it ends.
pub async fn report(
    on_step: &dyn Fn(Step),
    flow: impl Future<Output = Result<Outcome, SolError>>,
) -> Option<Outcome> {
    match flow.await {
        Ok(done) => {
            on_step(Step::Done(done.clone()));
            Some(done)
        }
        Err(error) => {
            on_step(Step::Failed(error));
            None
        }
    }
}

// Reading ---------------------------------------------------------------------------------

/// The vault's restricted list, from its own account.
pub async fn restricted<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
) -> Result<Vec<[u8; 32]>, SolError> {
    let account = reader
        .account(&address::restricted(vault))
        .await?
        .ok_or(SolError::WrongAccount)?;
    if account.owner != ids::VAULT {
        return Err(SolError::WrongAccount);
    }
    read::restricted_codes(&account.data).ok_or(SolError::WrongAccount)
}

/// `owner`'s holding, if any.
pub async fn holder<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    owner: &Pubkey,
) -> Result<Option<Holder>, SolError> {
    match reader.account(&address::holder(vault, owner)).await? {
        Some(account) if account.owner == ids::VAULT => Holder::read(&account.data)
            .filter(|holder| holder.vault == *vault && holder.owner == *owner)
            .map(Some)
            .ok_or(SolError::WrongAccount),
        Some(_) => Err(SolError::WrongAccount),
        None => Ok(None),
    }
}

/// Claim `id` of the vault, if it exists.
pub async fn claim<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    id: u64,
) -> Result<Option<Claim>, SolError> {
    match reader.account(&address::claim(vault, id)).await? {
        Some(account) if account.owner == ids::VAULT => Claim::read(&account.data)
            .filter(|claim| claim.id == id)
            .map(Some)
            .ok_or(SolError::WrongAccount),
        Some(_) => Err(SolError::WrongAccount),
        None => Ok(None),
    }
}

/// The request at `address`, if it is still there.
pub async fn request_at<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    at: &Pubkey,
) -> Result<Option<Request>, SolError> {
    match reader.account(at).await? {
        Some(account) if account.owner == ids::VAULT => Request::read(&account.data)
            .filter(|request| request.vault == *vault)
            .map(Some)
            .ok_or(SolError::WrongAccount),
        Some(_) => Err(SolError::WrongAccount),
        None => Ok(None),
    }
}

/// The vault's request `id`, wherever its maker put it: found among the
/// vault program's accounts by the vault and the id it holds.
pub async fn find_request<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    id: u64,
) -> Result<Option<(Pubkey, Request)>, SolError> {
    let found = reader
        .program_accounts(
            &ids::VAULT,
            &[
                (0, accounts::discriminator::REQUEST.to_vec()),
                (accounts::request_at::VAULT, vault.to_bytes().to_vec()),
                (accounts::request_at::ID, id.to_le_bytes().to_vec()),
            ],
        )
        .await?;
    Ok(found.into_iter().find_map(|(address, account)| {
        Request::read(&account.data)
            .filter(|request| request.vault == *vault && request.id == id)
            .map(|request| (address, request))
    }))
}

/// For a queued claim, the address of the request it is queued by: found by
/// the id the claim holds, as the program finds it, since the program made
/// it at the nonce the claim held before (see `ClaimKeys`). `None` for a
/// claim not queued; for a queued claim whose request can't be found,
/// nothing can be built.
pub async fn queued_request<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    claim: &Claim,
) -> Result<Option<Pubkey>, SolError> {
    if claim.state != ClaimState::Queued {
        return Ok(None);
    }
    match find_request(reader, vault, claim.request).await? {
        Some((address, request)) if claim.is_queued_by(vault, &request) => Ok(Some(address)),
        _ => Err(SolError::Network(
            "the request this claim is queued by couldn't be found".to_string(),
        )),
    }
}

/// Requests of this vault holding a payout for `owner`.
pub async fn held_requests<R: SolReader>(
    reader: &R,
    vault: &Pubkey,
    owner: &Pubkey,
) -> Result<Vec<(Pubkey, Request)>, SolError> {
    let found = reader
        .program_accounts(
            &ids::VAULT,
            &[
                (0, accounts::discriminator::REQUEST.to_vec()),
                (accounts::request_at::VAULT, vault.to_bytes().to_vec()),
                (accounts::request_at::HELD_FOR, owner.to_bytes().to_vec()),
            ],
        )
        .await?;
    Ok(found
        .into_iter()
        .filter_map(|(address, account)| {
            Request::read(&account.data)
                .filter(|request| {
                    request.vault == *vault && request.held_for == *owner && request.held > 0
                })
                .map(|request| (address, request))
        })
        .collect())
}

/// The driver groups the vault's station passes on when it closes or
/// values: one per pinned driver, for the stand-in venue's driver (the
/// venue's accounts and every market the station's authority holds there).
pub async fn groups<R: SolReader>(reader: &R, keys: &VaultKeys) -> Result<Groups, SolError> {
    let account = reader
        .account(&keys.station)
        .await?
        .ok_or(SolError::WrongAccount)?;
    if account.owner != ids::STATION {
        return Err(SolError::WrongAccount);
    }
    let station = accounts::Station::read(&account.data).ok_or(SolError::WrongAccount)?;
    if station.vault != keys.vault {
        return Err(SolError::WrongAccount);
    }
    let mut groups = Groups::none();
    for driver in &station.drivers {
        let pinned = &driver.pinned;
        let held = match reader
            .account(&address::venue_account(&pinned.venue, &pinned.authority))
            .await?
        {
            Some(account) if account.owner == pinned.venue => {
                accounts::MarginAccount::read(&account.data)
                    .filter(|margin| margin.master == pinned.authority)
                    .ok_or(SolError::WrongAccount)?
                    .market_accounts(&pinned.venue)
            }
            Some(_) => return Err(SolError::WrongAccount),
            None => Vec::new(),
        };
        groups.push(ix::stand_in_group(pinned, &keys.mint, &held));
    }
    Ok(groups)
}

// Deposit ---------------------------------------------------------------------------------

/// Deposit `amount` of the dollar: one transaction that is also the
/// statement for the restricted list the person was shown (its hash
/// `shown`), then its price and its settlement.
pub async fn deposit<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    amount: u64,
    shown: [u8; 32],
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    if amount == 0 {
        return Err(SolError::NothingToSend);
    }
    let Loaded { vault, keys } = ctx.load().await?;
    if vault.state != accounts::VaultState::Open || vault.ready_to_close() {
        return Err(SolError::WrongState);
    }
    if amount < vault.minimum_deposit {
        return Err(SolError::BelowMinimum {
            minimum: vault.minimum_deposit,
        });
    }
    let have = ctx.dollars_of(&ctx.account).await?;
    if have < amount {
        return Err(SolError::InsufficientBalance { have });
    }
    // The statement: the list read from the vault now, which must be the
    // list shown and the list the vault checks against.
    let codes = restricted(ctx.reader, &ctx.vault).await?;
    let statement = read::restricted_hash(&codes);
    if statement != shown || statement != vault.restricted_hash {
        return Err(SolError::ListChanged);
    }
    let nonce = ctx.nonce(&vault, &ctx.account).await?;
    let at = address::request(&ctx.vault, &ctx.account, nonce);
    let landed = ctx
        .transact(
            Prompt::Deposit { amount },
            vec![ix::request_deposit(
                &keys,
                &ctx.account,
                &ctx.account,
                amount,
                Some(statement),
                nonce,
            )],
        )
        .await?;
    let request = request_at(ctx.reader, &ctx.vault, &at)
        .await
        .map_err(|error| pending(landed.signature, error))?
        .filter(|r| {
            r.owner == ctx.account
                && r.kind == RequestKind::Deposit
                && r.amount == u128::from(amount)
        })
        .ok_or_else(|| {
            SolError::Network("the request's account wasn't there once it landed".to_string())
        })
        .map_err(|error| pending(landed.signature, error))?;
    let countdown = requested(ctx, &request, at, landed)
        .await
        .map_err(|error| pending(landed.signature, error))?;
    let priced_at = priced(ctx, countdown, &timers(&vault))
        .await
        .map_err(|error| pending(landed.signature, error))?;
    let settled = settlement(ctx, &at, request.id)
        .await
        .map_err(|error| pending(landed.signature, error))?;
    for event in settled {
        match event {
            Event::Deposited {
                request_id,
                assets,
                shares,
                ..
            } if request_id == request.id => {
                return Ok(Outcome::Deposited {
                    assets,
                    shares,
                    at: priced_at,
                    signature: countdown.signature,
                });
            }
            Event::Refunded {
                request_id, assets, ..
            } if request_id == request.id => {
                return Ok(Outcome::Refunded {
                    assets,
                    signature: countdown.signature,
                });
            }
            _ => {}
        }
    }
    Err(SolError::Pending {
        signature: landed.signature,
        why: "the deposit settled, but its record couldn't be read".to_string(),
    })
}

/// An error once a transaction has landed (not a request, which
/// [`pending`] covers): whatever it was, what it did stands.
fn went_through(signature: Sig, error: SolError) -> SolError {
    match error {
        SolError::Landed { .. } | SolError::Pending { .. } => error,
        SolError::Network(why) => SolError::Landed { signature, why },
        _ => SolError::Landed {
            signature,
            why: "the vault couldn't be read afterwards".to_string(),
        },
    }
}

/// An error once a request has landed: whatever it was, the request stands.
fn pending(signature: Sig, error: SolError) -> SolError {
    match error {
        SolError::Pending { .. } => error,
        SolError::Network(why) => SolError::Pending { signature, why },
        _ => SolError::Pending {
            signature,
            why: "the vault couldn't be read as it was followed".to_string(),
        },
    }
}

fn timers(vault: &Vault) -> Timers {
    Timers {
        slot_interval: vault.timers.slot_interval,
        slot_lead: vault.timers.slot_lead,
        slot_window: vault.timers.slot_window,
        fallback_after: vault.timers.fallback_after,
    }
}

/// The request a landed transaction made, and when.
async fn requested<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request: &Request,
    address: Pubkey,
    landed: Landed,
) -> Result<Countdown, SolError> {
    let asked_at = ctx.reader.chain_time().await?;
    let countdown = Countdown {
        request: request.id,
        address,
        signature: landed.signature,
        asked_at: asked_at.min(request.earliest),
        slot: request.earliest,
        moved_from: None,
    };
    ctx.step(Step::Requested(countdown));
    Ok(countdown)
}

/// Wait for the request to be priced and settled (the keeper settles every
/// priced request; anyone may), moving its time on when a valuation isn't
/// recorded in time. The time of the valuation it was priced at, as far as
/// the vault's account says.
async fn priced<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    mut countdown: Countdown,
    timers: &Timers,
) -> Result<u64, SolError> {
    let earliest = countdown.slot;
    let mut priced_at: Option<u64> = None;
    for _ in 0..ctx.pace.give_up_after {
        let request = request_at(ctx.reader, &ctx.vault, &countdown.address).await?;
        // Settled (or settled and closed since, which only a finished
        // request can be).
        if request.as_ref().is_none_or(|request| request.done) {
            if let Some(at) = priced_at {
                return Ok(at);
            }
            // Priced and settled between two looks: the valuation the vault
            // holds now, if it is at or after the request's.
            let Loaded { vault, .. } = ctx.load().await?;
            return Ok(if vault.priced && vault.latest.time >= earliest {
                vault.latest.time
            } else {
                countdown.slot
            });
        }
        if priced_at.is_none() {
            let Loaded { vault, .. } = ctx.load().await?;
            if vault.priced && vault.latest.time >= earliest {
                let at = vault.latest.time;
                priced_at = Some(at);
                countdown.slot = at;
                ctx.step(Step::Priced { countdown, at });
            } else {
                let now = ctx.reader.chain_time().await?;
                let slot = live_slot(earliest, now, timers);
                if slot != countdown.slot {
                    countdown.moved_from = Some(countdown.slot);
                    countdown.slot = slot;
                    ctx.step(Step::Requested(countdown));
                }
            }
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
    Err(SolError::Network(
        "the request is still waiting for its price; it stands, and your portfolio shows it when it's priced"
            .to_string(),
    ))
}

/// The vault's events about request `id` in the transactions that touched
/// its account, newest first: where its settlement says how it ended.
async fn settlement<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    at: &Pubkey,
    id: u64,
) -> Result<Vec<Event>, SolError> {
    let mut found = Vec::new();
    for _ in 0..10 {
        for signature in ctx.reader.signatures_for(at, 10).await? {
            let events = ctx.events(&signature).await?;
            let about = |event: &Event| match event {
                Event::Deposited { request_id, .. }
                | Event::Refunded { request_id, .. }
                | Event::Withdrawn { request_id, .. } => *request_id == id,
                // A claim's id is the id of the withdrawal it was booked from.
                Event::ClaimBooked { claim_id, .. } => *claim_id == id,
                _ => false,
            };
            found.extend(events.into_iter().filter(about));
            if !found.is_empty() {
                return Ok(found);
            }
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
    Ok(found)
}

// Withdraw --------------------------------------------------------------------------------

/// Withdraw `shares`: a request priced at the next valuation, paid from cash
/// or booked as a claim. A closed vault pays at once from its cash.
pub async fn withdraw<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    shares: u128,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    if shares == 0 {
        return Err(SolError::NothingToSend);
    }
    let Loaded { vault, keys } = ctx.load().await?;
    let held = holder(ctx.reader, &ctx.vault, &ctx.account)
        .await?
        .map_or(0, |holder| holder.shares);
    if held < shares {
        return Err(SolError::InsufficientShares { have: held });
    }
    let mut ixs = Vec::new();
    ctx.open_if_missing(&ctx.account, &mut ixs).await?;
    match vault.state {
        accounts::VaultState::Closed => {
            ixs.push(ix::redeem_closed(&keys, &ctx.account, shares, &ctx.account));
            let landed = ctx.transact(Prompt::Closed, ixs).await?;
            return paid_at_once(ctx, landed)
                .await
                .map_err(|error| went_through(landed.signature, error));
        }
        accounts::VaultState::None => return Err(SolError::WrongState),
        _ if vault.ready_to_close() => return Err(SolError::WrongState),
        _ => {}
    }
    let nonce = ctx.nonce(&vault, &ctx.account).await?;
    let at = address::request(&ctx.vault, &ctx.account, nonce);
    ixs.push(ix::request_redeem(
        &keys,
        &ctx.account,
        shares,
        &ctx.account,
        nonce,
    ));
    let landed = ctx.transact(Prompt::Withdraw { shares }, ixs).await?;
    let request = request_at(ctx.reader, &ctx.vault, &at)
        .await
        .map_err(|error| pending(landed.signature, error))?
        .filter(|r| r.owner == ctx.account && r.kind == RequestKind::Redeem && r.amount == shares)
        .ok_or_else(|| {
            SolError::Network("the request's account wasn't there once it landed".to_string())
        })
        .map_err(|error| pending(landed.signature, error))?;
    let countdown = requested(ctx, &request, at, landed)
        .await
        .map_err(|error| pending(landed.signature, error))?;
    let priced_at = priced(ctx, countdown, &timers(&vault))
        .await
        .map_err(|error| pending(landed.signature, error))?;
    let settled = settlement(ctx, &at, request.id)
        .await
        .map_err(|error| pending(landed.signature, error))?;
    for event in settled {
        match event {
            Event::Withdrawn { shares, assets, .. } => {
                return Ok(Outcome::Paid {
                    shares,
                    assets,
                    at: Some(priced_at),
                    signature: countdown.signature,
                });
            }
            Event::ClaimBooked {
                claim_id, shares, ..
            } => {
                return Ok(Outcome::Booked {
                    claim: claim_id,
                    shares,
                    signature: countdown.signature,
                });
            }
            _ => {}
        }
    }
    Err(SolError::Pending {
        signature: landed.signature,
        why: "the withdrawal settled, but its record couldn't be read".to_string(),
    })
}

/// A closed vault's payout, from the events of the transaction that paid it.
async fn paid_at_once<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    landed: Landed,
) -> Result<Outcome, SolError> {
    for event in ctx.events(&landed.signature).await? {
        match event {
            Event::Withdrawn { shares, assets, .. } => {
                return Ok(Outcome::Paid {
                    shares,
                    assets,
                    at: None,
                    signature: landed.signature,
                });
            }
            Event::Refunded { assets, .. } => {
                return Ok(Outcome::Refunded {
                    assets,
                    signature: landed.signature,
                });
            }
            Event::ClaimPaid { claim_id, assets } => {
                return Ok(Outcome::Claim {
                    claim: claim_id,
                    state: ClaimState::Paid,
                    paid: assets,
                    signature: landed.signature,
                });
            }
            _ => {}
        }
    }
    Err(SolError::Network(
        "the payout's record wasn't in its transaction".to_string(),
    ))
}

// Claims, the fallback, collecting ----------------------------------------------------------

/// Push a claim through (`settle_claim`): anyone may. It ends paid, waiting
/// (a market outside the band), or queued for the next valuation. A closed
/// vault pays it from its cash.
pub async fn settle_claim<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: u64,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { vault, keys } = ctx.load().await?;
    let before = claim(ctx.reader, &ctx.vault, id)
        .await?
        .ok_or(SolError::WrongState)?;
    let landed = if vault.state == accounts::VaultState::Closed {
        let mut ixs = Vec::new();
        ctx.open_if_missing(&before.owner, &mut ixs).await?;
        if !matches!(
            before.state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ) {
            return Err(SolError::WrongState);
        }
        let queued_by = queued_request(ctx.reader, &ctx.vault, &before).await?;
        ixs.push(ix::claim_closed(&keys, &before.keys(), queued_by.as_ref()));
        ctx.transact(Prompt::Closed, ixs).await?
    } else {
        if !matches!(before.state, ClaimState::Pending | ClaimState::Waiting) {
            return Err(SolError::WrongState);
        }
        let ixs = settling(ctx.reader, &ctx.cluster.mint, &keys, &ctx.account, &before).await?;
        ctx.transact(Prompt::SettleClaim { claim: id }, ixs).await?
    };
    let after = claim(ctx.reader, &ctx.vault, id)
        .await
        .map_err(|error| went_through(landed.signature, error))?;
    Ok(Outcome::Claim {
        claim: id,
        state: after.as_ref().map_or(ClaimState::Paid, |claim| claim.state),
        paid: after.as_ref().map_or(0, |claim| claim.paid),
        signature: landed.signature,
    })
}

/// What settling `claim` on an open vault sends, from `caller`: its owner's
/// dollar account opened if missing, then the claim's close through every
/// driver group of the vault's station.
pub async fn settling<R: SolReader>(
    reader: &R,
    mint: &Pubkey,
    keys: &VaultKeys,
    caller: &Pubkey,
    claim: &Claim,
) -> Result<Vec<Instruction>, SolError> {
    let mut ixs = Vec::new();
    open_if_missing(reader, caller, &claim.owner, mint, &mut ixs).await?;
    let groups = groups(reader, keys).await?;
    ixs.push(ix::settle_claim(keys, caller, &claim.keys(), &groups));
    Ok(ixs)
}

/// What taking back deposit `request` (at `at`) sends, from its owner: their
/// dollar account opened if missing, then the fallback, which asks every
/// driver group of the station whether the vault could be valued now.
pub async fn taking_back<R: SolReader>(
    reader: &R,
    mint: &Pubkey,
    keys: &VaultKeys,
    at: &Pubkey,
    request: &Request,
) -> Result<Vec<Instruction>, SolError> {
    let mut ixs = Vec::new();
    open_if_missing(reader, &request.owner, &request.owner, mint, &mut ixs).await?;
    let groups = groups(reader, keys).await?;
    ixs.push(ix::take_pro_rata(
        keys,
        &request.owner,
        at,
        request.id,
        &groups,
    ));
    Ok(ixs)
}

/// A stand-in for whoever sends, for measuring: no key the transaction
/// names otherwise, so the measure is the largest the transaction can be.
pub fn someone() -> Pubkey {
    Pubkey::new_from_array([0x5e; 32])
}

/// What the fallback is taken for: a request, or a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Request(u64),
    Claim(u64),
}

/// The fallback, when nothing has priced for a day: a withdrawal's exact
/// share of the cash now and units for the rest, or a deposit back.
pub async fn take_cash<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    of: Fallback,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { keys, .. } = ctx.load().await?;
    let mut ixs = Vec::new();
    ctx.open_if_missing(&ctx.account, &mut ixs).await?;
    match of {
        Fallback::Request(id) => {
            let (at, request) = find_request(ctx.reader, &ctx.vault, id)
                .await?
                .ok_or(SolError::WrongState)?;
            if request.owner != ctx.account || request.done {
                return Err(SolError::WrongState);
            }
            match request.kind {
                // A deposit's asks the station whether it could be valued
                // now.
                RequestKind::Deposit => {
                    ixs = taking_back(ctx.reader, &ctx.cluster.mint, &keys, &at, &request).await?;
                }
                _ => ixs.push(ix::take_pro_rata(
                    &keys,
                    &ctx.account,
                    &at,
                    id,
                    &Groups::none(),
                )),
            }
        }
        Fallback::Claim(id) => {
            let claim = claim(ctx.reader, &ctx.vault, id)
                .await?
                .ok_or(SolError::WrongState)?;
            if claim.owner != ctx.account {
                return Err(SolError::WrongState);
            }
            let queued_by = queued_request(ctx.reader, &ctx.vault, &claim).await?;
            ixs.push(ix::take_pro_rata_claim(
                &keys,
                &claim.keys(),
                queued_by.as_ref(),
            ));
        }
    }
    let landed = ctx.transact(Prompt::TakeCash, ixs).await?;
    took_cash(ctx, landed)
        .await
        .map_err(|error| went_through(landed.signature, error))
}

async fn took_cash<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    landed: Landed,
) -> Result<Outcome, SolError> {
    for event in ctx.events(&landed.signature).await? {
        match event {
            Event::ProRataTaken {
                owner,
                assets,
                units,
                ..
            } if owner == ctx.account => {
                return Ok(Outcome::TookCash {
                    assets,
                    units,
                    signature: landed.signature,
                });
            }
            Event::Refunded { assets, .. } => {
                return Ok(Outcome::Refunded {
                    assets,
                    signature: landed.signature,
                });
            }
            _ => {}
        }
    }
    Err(SolError::Network(
        "the fallback's record wasn't in its transaction".to_string(),
    ))
}

/// A withdrawal nothing has priced for a day, made a claim instead.
pub async fn claim_instead<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request_id: u64,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { keys, .. } = ctx.load().await?;
    let (at, request) = find_request(ctx.reader, &ctx.vault, request_id)
        .await?
        .ok_or(SolError::WrongState)?;
    if request.owner != ctx.account || request.done || request.kind != RequestKind::Redeem {
        return Err(SolError::WrongState);
    }
    let landed = ctx
        .transact(
            Prompt::ClaimInstead,
            vec![ix::claim_instead(&keys, &ctx.account, &at, request_id)],
        )
        .await?;
    claim_made(ctx, landed)
        .await
        .map_err(|error| went_through(landed.signature, error))
}

async fn claim_made<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    landed: Landed,
) -> Result<Outcome, SolError> {
    for event in ctx.events(&landed.signature).await? {
        if let Event::ClaimBooked {
            claim_id, owner, ..
        } = event
            && owner == ctx.account
        {
            return Ok(Outcome::ClaimMade {
                claim: claim_id,
                signature: landed.signature,
            });
        }
    }
    Err(SolError::Network(
        "the claim's record wasn't in its transaction".to_string(),
    ))
}

/// What the fallback's units have earned so far.
pub async fn collect_units<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { vault, keys } = ctx.load().await?;
    let claimable = holder(ctx.reader, &ctx.vault, &ctx.account)
        .await?
        .and_then(|holder| holder.units_claimable(vault.per_unit))
        .unwrap_or(0);
    if claimable == 0 {
        return Err(SolError::WouldFail {
            reason: Reason::Vault("Nothing"),
        });
    }
    let mut ixs = Vec::new();
    ctx.open_if_missing(&ctx.account, &mut ixs).await?;
    ixs.push(ix::collect_units(&keys, &ctx.account));
    let landed = ctx.transact(Prompt::CollectUnits, ixs).await?;
    collected(ctx, landed)
        .await
        .map_err(|error| went_through(landed.signature, error))
}

/// Where a payout is held for the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldAt {
    /// On a request, at its address.
    Request { id: u64, address: Pubkey },
    /// On a claim.
    Claim { id: u64 },
}

/// A payout the account's dollar account couldn't take, held for it.
pub async fn collect_held<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    at: HeldAt,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { keys, .. } = ctx.load().await?;
    let (held, record) = match at {
        HeldAt::Request { id, address } => {
            let request = request_at(ctx.reader, &ctx.vault, &address)
                .await?
                .filter(|r| r.id == id && r.held_for == ctx.account && r.held > 0)
                .ok_or(SolError::WouldFail {
                    reason: Reason::Vault("Nothing"),
                })?;
            let _ = request;
            (Held::Request(id), Some(address))
        }
        HeldAt::Claim { id } => {
            claim(ctx.reader, &ctx.vault, id)
                .await?
                .filter(|c| c.owner == ctx.account && c.held > 0)
                .ok_or(SolError::WouldFail {
                    reason: Reason::Vault("Nothing"),
                })?;
            (Held::Claim(id), None)
        }
    };
    let mut ixs = Vec::new();
    ctx.open_if_missing(&ctx.account, &mut ixs).await?;
    ixs.push(ix::collect_held(&keys, &ctx.account, held, record));
    let landed = ctx.transact(Prompt::CollectHeld, ixs).await?;
    collected(ctx, landed)
        .await
        .map_err(|error| went_through(landed.signature, error))
}

async fn collected<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    landed: Landed,
) -> Result<Outcome, SolError> {
    for event in ctx.events(&landed.signature).await? {
        match event {
            Event::UnitsPaid { owner, assets } | Event::HeldCollected { to: owner, assets }
                if owner == ctx.account =>
            {
                return Ok(Outcome::Collected {
                    assets,
                    signature: landed.signature,
                });
            }
            _ => {}
        }
    }
    Err(SolError::Network(
        "the payment's record wasn't in its transaction".to_string(),
    ))
}

/// A request a closed vault never priced: a withdrawal paid from its cash,
/// a deposit sent back.
pub async fn settle_closed<R: SolReader, S: SolSigner, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request_id: u64,
) -> Result<Outcome, SolError> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    let Loaded { vault, keys } = ctx.load().await?;
    if vault.state != accounts::VaultState::Closed {
        return Err(SolError::WrongState);
    }
    let (at, request) = find_request(ctx.reader, &ctx.vault, request_id)
        .await?
        .ok_or(SolError::WrongState)?;
    if request.done {
        return Err(SolError::WrongState);
    }
    let mut ixs = Vec::new();
    let to = match request.kind {
        RequestKind::Deposit => request.payer,
        RequestKind::Redeem => request.owner,
        RequestKind::Claim | RequestKind::TankFill => vault.author,
    };
    ctx.open_if_missing(&to, &mut ixs).await?;
    ixs.push(ix::settle_closed(
        &keys,
        &at,
        request_id,
        request.kind.builder(),
        &request.owner,
        &request.payer,
    ));
    let landed = ctx.transact(Prompt::Closed, ixs).await?;
    paid_at_once(ctx, landed)
        .await
        .map_err(|error| went_through(landed.signature, error))
}
