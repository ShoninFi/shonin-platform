//! Every depositor flow as a small state machine whose states are the
//! canvas's (Handoff-States, "Transaction requests"): checking → awaiting
//! signature → sent → requested (the valuation countdown) → priced → done,
//! or rejected, failed, moved.
//!
//! Each flow is one async function over the [`Reader`], [`Signer`] and
//! [`Clock`] ports, reporting every step it reaches. Rules every flow keeps:
//!
//! - Money is read from the chain first: the vault's state, the balance on
//!   the vault's chain, the allowance, whether a statement is needed.
//! - The account and the chain are checked immediately before every
//!   signature and every send; a flow built for an account that has changed
//!   is cancelled, and nothing is sent on the wrong chain.
//! - Each transaction is simulated (`eth_estimateGas`) before the wallet is
//!   asked, so a send that would revert is refused in words and never
//!   offered for signing; and the network fee is checked against the coin
//!   the wallet holds.
//! - An approval is for exactly the amount being deposited, and only when
//!   the allowance doesn't already cover it.
//! - The statement is signed over the vault's own list read from the chain,
//!   and the digest the app would ask for is checked against the vault's own
//!   `statementDigest` before anything is shown; the signature is checked to
//!   recover to the account before it is sent.
//! - Success is reported only from a mined receipt and the vault's own
//!   events, never before.
//! - Once the transaction that moves the money is mined, nothing after it
//!   is reported as a flow that never happened: a request stands and is
//!   settled at its price by the keeper, or by anyone, whatever becomes of
//!   the app's own optional second step ([`TxError::Landed`]).

use std::cell::Cell;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_sol_types::{SolCall, SolEvent};
use chain::bindings::erc20::IERC20;
use chain::bindings::vault::IVault;

use super::error::{Revert, TxError};
use super::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use super::read::{self, ClaimState, RequestKind, State, Timers};

/// What the wallet is being asked for, for the panel's wallet step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// The eligibility statement (a signature, no transaction).
    Statement,
    /// Let the vault take exactly this many of the dollar's base units.
    Approve {
        amount: U256,
    },
    Deposit {
        amount: U256,
    },
    Withdraw {
        shares: U256,
    },
    /// The second step: settle what the valuation priced. Optional: the
    /// keeper sends it with the valuation, and anyone may.
    Finish,
    SettleClaim {
        claim: U256,
    },
    /// The fallback on a withdrawal or a claim: the cash share now, units
    /// for the rest.
    TakeCash,
    /// The fallback on a deposit: the money back, nothing bought.
    TakeBack {
        amount: U256,
    },
    ClaimInstead,
    CollectUnits,
    CollectHeld,
    /// A closed vault's payout.
    Closed,
}

/// A request waiting for its valuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Countdown {
    pub request: U256,
    pub hash: B256,
    /// When the request was mined, on the chain's clock.
    pub asked_at: u64,
    /// The slot it will be priced at now.
    pub slot: u64,
    /// The slot it was moved from, when a valuation wasn't recorded in time.
    pub moved_from: Option<u64>,
}

/// Where a flow is. The page draws each; the status line announces each
/// change once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Reading the chain before asking anything.
    Checking,
    /// Waiting for the person to approve in their wallet.
    Signing(Prompt),
    /// Handed to the network; waiting for it to be mined. `unseen` once
    /// the network we read through hasn't seen it for a while: it may still
    /// land, and the app keeps watching.
    Sent {
        prompt: Prompt,
        hash: B256,
        unseen: bool,
    },
    /// The request is on the chain; it is priced at `slot`.
    Requested(Countdown),
    /// Priced, waiting to be settled at that price: the keeper does it, or
    /// the app sends the second step after a few seconds. `sent` is the
    /// app's own second step while it is watched (briefly: it is optional);
    /// `unsent` says why it wasn't sent, or wasn't seen mined. The request
    /// stands either way, and the person may ask for it again
    /// ([`Ctx::settle_now`]).
    Priced {
        countdown: Countdown,
        at: u64,
        unsent: Option<TxError>,
        sent: Option<B256>,
    },
    Done(Outcome),
    Failed(TxError),
}

/// How a flow ended well.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Shares bought, exactly, at the valuation `at`.
    Deposited {
        assets: U256,
        shares: U256,
        at: u64,
        hash: B256,
    },
    /// A deposit sent back, nothing bought, to the wallet that paid it in
    /// (`to`): the vault stopped taking deposits or its list changed before
    /// the price, the vault closed, or the fallback.
    Refunded {
        assets: U256,
        to: Address,
        hash: B256,
    },
    /// A withdrawal paid from cash.
    Paid {
        shares: U256,
        assets: U256,
        at: Option<u64>,
        hash: B256,
    },
    /// A withdrawal booked as a claim.
    Booked {
        claim: U256,
        shares: U256,
        hash: B256,
    },
    /// A request someone else had settled before the app asked anything:
    /// nothing was sent, and this is how it settled.
    SettledBefore(Box<Outcome>),
    /// A claim settled: it pays `owner`, whoever sent it.
    Claim {
        claim: U256,
        owner: Address,
        state: ClaimState,
        paid: U256,
        hash: B256,
    },
    /// The fallback taken: cash now, and units for the rest.
    TookCash {
        assets: U256,
        units: U256,
        hash: B256,
    },
    /// A withdrawal made a claim instead.
    ClaimMade { claim: U256, hash: B256 },
    /// Units' or held dollars collected.
    Collected { assets: U256, hash: B256 },
}

/// How often and how long a flow waits. The browser uses [`Pace::LIVE`];
/// the tests use [`Pace::FAST`].
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    pub poll_millis: u32,
    /// Polls before a transaction the network never knew is called dropped.
    pub dropped_after: u32,
    /// Polls after the price before the app sends the second step itself.
    pub finish_after: u32,
    /// Polls the app's own second step is watched for once sent: briefly,
    /// since it is optional and the keeper settles the request anyway.
    pub finish_watch: u32,
    /// Polls before the flow stops watching (the request still stands).
    pub give_up_after: u32,
}

impl Pace {
    pub const LIVE: Self = Self {
        poll_millis: 1_000,
        dropped_after: 180,
        finish_after: 8,
        finish_watch: 60,
        give_up_after: 30 * 60,
    };
    pub const FAST: Self = Self {
        poll_millis: 0,
        dropped_after: 5,
        finish_after: 2,
        finish_watch: 8,
        give_up_after: 200,
    };
}

/// The person asking, from the page, for the second step now ("Settle
/// now"), after the app's own try wasn't sent.
#[derive(Clone, Default)]
pub struct Nudge(Rc<Cell<bool>>);

impl Nudge {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn nudge(&self) {
        self.0.set(true);
    }
    /// Whether it was asked for since the last look; clears it.
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }
}

/// Stops a flow before its next signature or send.
#[derive(Clone, Default)]
pub struct Cancel(Rc<Cell<bool>>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.set(true);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

/// Everything one flow acts on, fixed when it starts.
pub struct Ctx<'a, R, S, C> {
    pub reader: &'a R,
    pub signer: &'a S,
    pub clock: &'a C,
    pub chain_id: u64,
    pub vault: Address,
    /// The dollar the vault settles in, on its chain.
    pub asset: Address,
    /// The account the flow was built for.
    pub account: Address,
    pub cancel: Cancel,
    /// "Settle now", pressed on the page.
    pub settle_now: Nudge,
    /// "Skip", pressed on the page while the wallet is asked for the
    /// optional second step: only that step is let go.
    pub skip_second: Nudge,
    pub pace: Pace,
    pub on_step: &'a dyn Fn(Step),
}

impl<R: Reader, S: Signer, C: Clock> Ctx<'_, R, S, C> {
    /// Report a step to the page.
    pub(crate) fn step(&self, step: Step) {
        (self.on_step)(step);
    }

    /// Checked immediately before every signature and every send: not
    /// cancelled, the same account, and the wallet on the flow's chain.
    /// Every flow (the depositor's here, the author's elsewhere) signs and
    /// sends only after this.
    pub(crate) async fn guard(&self) -> Result<(), TxError> {
        if self.cancel.is_cancelled() {
            return Err(TxError::Cancelled);
        }
        match self.signer.account() {
            None => return Err(TxError::NotConnected),
            Some(account) if account != self.account => return Err(TxError::AccountChanged),
            Some(_) => {}
        }
        let wallet = self.signer.chain_id().await?;
        if wallet != self.chain_id {
            return Err(TxError::WrongNetwork {
                wallet,
                vault: self.chain_id,
            });
        }
        Ok(())
    }

    /// The one path that sends money: [`Self::send`], then wait for the
    /// receipt and fail on a revert. Success is only ever a mined, successful
    /// receipt.
    pub(crate) async fn transact(
        &self,
        prompt: Prompt,
        to: Address,
        data: Vec<u8>,
    ) -> Result<Receipt, TxError> {
        let hash = self.send(prompt.clone(), to, data).await?;
        self.step(Step::Sent {
            prompt: prompt.clone(),
            hash,
            unseen: false,
        });
        let receipt = self.mined(hash, &prompt).await?;
        if !receipt.success {
            return Err(TxError::Reverted { hash, reason: None });
        }
        Ok(receipt)
    }

    /// Hand a transaction to the wallet: simulate (`eth_estimateGas`, so a
    /// send that would revert is refused before the wallet is asked), check
    /// the network fee against the coin held, [`Self::guard`] again, then
    /// ask the wallet. Its hash, once the wallet has handed it on.
    pub(crate) async fn send(
        &self,
        prompt: Prompt,
        to: Address,
        data: Vec<u8>,
    ) -> Result<B256, TxError> {
        let mut request = TxRequest {
            chain_id: self.chain_id,
            from: self.account,
            to,
            data: Bytes::from(data),
            gas: None,
        };
        let gas = self.reader.estimate_gas(&request).await?;
        // Headroom: the state can move a little between now and the block.
        let gas = gas.saturating_add(gas / 4).saturating_add(10_000);
        let price = self.reader.gas_price().await?;
        let need = U256::from(gas).saturating_mul(price);
        let have = self.reader.native_balance(self.account).await?;
        if have < need {
            return Err(TxError::InsufficientGas { need, have });
        }
        request.gas = Some(gas);

        self.guard().await?;
        self.step(Step::Signing(prompt));
        self.signer.send(&request).await
    }

    /// Wait for a sent transaction to be mined, or say what became of it:
    /// replaced, or not mined by the end of a long wait (seen by the network
    /// or not). One endpoint not knowing a transaction proves nothing (the
    /// wallet may have sent it through another), so that is said as "not
    /// seen yet" and watched to the end; and a read that fails is one more
    /// look, never the end of a transaction that may still land.
    pub(crate) async fn mined(&self, hash: B256, prompt: &Prompt) -> Result<Receipt, TxError> {
        let mut known: Option<u64> = None;
        let mut unknown_polls = 0u32;
        let mut unseen = false;
        for _ in 0..self.pace.give_up_after {
            if let Ok(Some(receipt)) = self.reader.receipt(hash).await {
                self.reached(receipt.block).await;
                return Ok(receipt);
            }
            match self.reader.pending(hash).await {
                Ok(Pending::Known { nonce }) => {
                    known = Some(nonce);
                    unknown_polls = 0;
                    if unseen {
                        unseen = false;
                        self.step(Step::Sent {
                            prompt: prompt.clone(),
                            hash,
                            unseen,
                        });
                    }
                }
                Ok(Pending::Unknown) => {
                    unknown_polls += 1;
                    // Known once, gone now on two looks running, and its
                    // nonce used by another: never while the endpoint has
                    // any receipt for it, even one whose block isn't sealed
                    // yet (a chain confirming ahead of its blocks counts it
                    // in the nonce already), asked twice, since a pool of
                    // nodes can answer one read from a node that hasn't had
                    // it yet.
                    if let Some(nonce) = known
                        && unknown_polls >= 2
                        && matches!(self.reader.mined_nonce(self.account).await, Ok(mined) if mined > nonce)
                        && matches!(self.reader.receipt_seen(hash).await, Ok(false))
                        && matches!(self.reader.receipt_seen(hash).await, Ok(false))
                    {
                        return Err(TxError::Replaced { hash });
                    }
                    if unknown_polls > self.pace.dropped_after && !unseen {
                        unseen = true;
                        self.step(Step::Sent {
                            prompt: prompt.clone(),
                            hash,
                            unseen,
                        });
                    }
                }
                Err(_) => {}
            }
            self.clock.sleep(self.pace.poll_millis).await;
        }
        // Not mined after a long wait. Known to the network all this while:
        // a fee too low, most likely. Either way it may still land.
        if known.is_some() && !unseen {
            Err(TxError::Stuck { hash })
        } else {
            Err(TxError::Dropped { hash })
        }
    }

    /// Wait, briefly, until the endpoint's latest block is at least `block`,
    /// so the next read or simulation sees what was just mined (a pool of
    /// nodes can answer a receipt from one node and a call from another a
    /// block behind). Gives up after a few looks: the receipt stands either
    /// way, and a read that still misses it is one more look later: the
    /// wait is best effort.
    async fn reached(&self, block: u64) {
        for _ in 0..self.pace.dropped_after.clamp(1, 30) {
            match self.reader.block_number().await {
                Ok(Some(latest)) if latest < block => {
                    self.clock.sleep(self.pace.poll_millis).await;
                }
                _ => return,
            }
        }
    }

    /// The vault's own events in a receipt.
    fn events<E: SolEvent>(&self, receipt: &Receipt) -> Vec<E> {
        decode::<E>(self.vault, &receipt.logs)
    }
}

/// Every log of `E` from `address`.
pub fn decode<E: SolEvent>(address: Address, logs: &[Log]) -> Vec<E> {
    logs.iter()
        .filter(|log| log.address == address)
        .filter(|log| log.topics.first() == Some(&E::SIGNATURE_HASH))
        .filter_map(|log| E::decode_raw_log(log.topics.iter().copied(), &log.data).ok())
        .collect()
}

/// What an error after `hash` was mined becomes: the money moved, so it is
/// [`TxError::Landed`], with what stopped the app.
fn landed(hash: B256, request: Option<U256>) -> impl Fn(TxError) -> TxError {
    move |then| match then {
        TxError::Landed { .. } => then,
        then => TxError::Landed {
            hash,
            request,
            then: Some(Box::new(then)),
        },
    }
}

/// Run a flow, reporting where it ends: done, or failed with its reason.
pub async fn report(
    on_step: &dyn Fn(Step),
    flow: impl Future<Output = Result<Outcome, TxError>>,
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

// Deposit ------------------------------------------------------------------

/// Deposit `amount` of the dollar: the statement if the vault's list needs
/// one, an approval for exactly the amount if the allowance doesn't cover
/// it, the request, its price, and the second step if nobody else has sent
/// it.
pub async fn deposit<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    amount: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    if amount.is_zero() {
        return Err(TxError::NothingToSend);
    }
    let figures = read::figures(ctx.reader, ctx.vault).await?;
    if figures.state != State::Open {
        return Err(TxError::WrongState);
    }
    if amount < figures.minimum_deposit {
        return Err(TxError::BelowMinimum {
            minimum: figures.minimum_deposit,
        });
    }
    let have = read::asset_balance(ctx.reader, ctx.asset, ctx.account).await?;
    if have < amount {
        return Err(TxError::InsufficientBalance { have });
    }

    // The statement, over the vault's own list, only if it isn't recorded
    // for the list as it stands.
    let statement = if read::has_statement(ctx.reader, ctx.vault, ctx.account).await? {
        Bytes::new()
    } else {
        sign_statement(ctx).await?
    };

    // Exactly the amount, and only if needed.
    let allowed = read::allowance(ctx.reader, ctx.asset, ctx.account, ctx.vault).await?;
    let approving = allowed < amount;
    if approving {
        ctx.transact(
            Prompt::Approve { amount },
            ctx.asset,
            IERC20::approveCall {
                spender: ctx.vault,
                value: amount,
            }
            .abi_encode(),
        )
        .await?;
    }

    let receipt = ctx
        .transact(
            Prompt::Deposit { amount },
            ctx.vault,
            IVault::requestDepositCall {
                assets: amount,
                receiver: ctx.account,
                statement,
            }
            .abi_encode(),
        )
        .await
        .map_err(|then| {
            // The approval was mined and the deposit stopped before it was
            // sent: the approval stands, and depositing again uses it.
            if approving && !then.may_have_moved() {
                TxError::ApprovalStands {
                    amount,
                    then: Box::new(then),
                }
            } else {
                then
            }
        })?;
    follow(ctx, &receipt, RequestKind::Deposit, amount, &figures.timers).await
}

/// After the request's transaction is mined: its countdown, its price, the
/// second step if nobody else sends it, and how it settled. Every error from
/// here on is [`TxError::Landed`]: the money is in the vault.
async fn follow<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    receipt: &Receipt,
    kind: RequestKind,
    amount: U256,
    timers: &Timers,
) -> Result<Outcome, TxError> {
    let countdown = requested(ctx, receipt)
        .await
        .map_err(landed(receipt.hash, None))?;
    let landed = landed(countdown.hash, Some(countdown.request));
    let at = priced(ctx, countdown, timers).await.map_err(&landed)?;
    let request = read::Request {
        owner: ctx.account,
        kind,
        earliest: countdown.slot,
        done: true,
        amount,
        reference: U256::ZERO,
    };
    settled(
        ctx,
        countdown.request,
        &request,
        receipt.block,
        at,
        Some(countdown.hash),
    )
    .await
    .map_err(&landed)
}

/// How a settled request ended, from the vault's own events for that
/// request, wherever and by whomever it was settled (`from_block` is any
/// block at or before its settlement): a deposit bought shares or was sent
/// back; a withdrawal was paid or booked as a claim; a claim's own request
/// paid its claim, or left it waiting. `hash` is the transaction the
/// outcome links to; `None` links the one that settled it.
async fn settled<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
    request: &read::Request,
    from_block: u64,
    at: u64,
    hash: Option<B256>,
) -> Result<Outcome, TxError> {
    let topic = B256::from(id);
    let by_id = |event: B256| ctx.reader.logs(ctx.vault, event, Some(topic), from_block);
    let link = |log: &Log| hash.or(log.transaction).unwrap_or_default();
    match request.kind {
        RequestKind::Deposit => {
            let logs = by_id(IVault::Deposited::SIGNATURE_HASH).await?;
            if let Some((event, log)) = first::<IVault::Deposited>(ctx.vault, &logs) {
                return Ok(Outcome::Deposited {
                    assets: event.assets,
                    shares: event.shares,
                    at,
                    hash: link(log),
                });
            }
            let logs = by_id(IVault::Refunded::SIGNATURE_HASH).await?;
            if let Some((event, log)) = first::<IVault::Refunded>(ctx.vault, &logs) {
                return Ok(Outcome::Refunded {
                    assets: event.assets,
                    to: event.to,
                    hash: link(log),
                });
            }
        }
        RequestKind::Redeem => {
            let logs = by_id(IVault::Withdrawn::SIGNATURE_HASH).await?;
            if let Some((event, log)) = first::<IVault::Withdrawn>(ctx.vault, &logs) {
                return Ok(Outcome::Paid {
                    shares: event.shares,
                    assets: event.assets,
                    at: Some(at),
                    hash: link(log),
                });
            }
            if let Some((claim, transaction)) =
                booked_claim(ctx, id, request.owner, request.amount, from_block).await?
            {
                return Ok(Outcome::Booked {
                    claim,
                    shares: request.amount,
                    hash: hash.or(transaction).unwrap_or_default(),
                });
            }
        }
        RequestKind::Claim => {
            // Paid with a `Withdrawn` for this request, or, if the cash no
            // longer covered it, left waiting to be tried again.
            let logs = by_id(IVault::Withdrawn::SIGNATURE_HASH).await?;
            let paid = first::<IVault::Withdrawn>(ctx.vault, &logs).map(|(_, log)| link(log));
            let claim = read::claim(ctx.reader, ctx.vault, request.reference).await?;
            return Ok(Outcome::Claim {
                claim: request.reference,
                owner: claim.owner,
                state: claim.state,
                paid: claim.paid,
                hash: paid.or(hash).unwrap_or_default(),
            });
        }
        RequestKind::TankFill => {}
    }
    Err(TxError::Network(
        "it settled, but its record couldn't be read".to_string(),
    ))
}

/// The first log of `E` from `address`, with the log it came from.
fn first<E: SolEvent>(address: Address, logs: &[Log]) -> Option<(E, &Log)> {
    logs.iter()
        .filter(|log| log.address == address)
        .filter(|log| log.topics.first() == Some(&E::SIGNATURE_HASH))
        .find_map(|log| {
            E::decode_raw_log(log.topics.iter().copied(), &log.data)
                .ok()
                .map(|event| (event, log))
        })
}

/// The most later requests read to match a booked withdrawal to its claim.
const MATCH_READS: u64 = 256;

/// The claim a settled withdrawal (`id`) was booked as, and the transaction
/// that booked it. The vault's `ClaimBooked` names no request, so the claim
/// is found by order: it settles withdrawals in request order, so the
/// claims of the same owner for the same shares (twins) are booked in their
/// requests' order, and every twin requested after this one was booked
/// after it. Its claim is the one followed by exactly as many twin claims as
/// there are later twin requests booked (done, and not paid by id). `None`
/// when the claims read don't account for it.
async fn booked_claim<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
    owner: Address,
    shares: U256,
    from_block: u64,
) -> Result<Option<(U256, Option<B256>)>, TxError> {
    // A twin booked between reading the requests and reading the claims
    // would shift the count: read until both agree.
    for _ in 0..3 {
        let later = later_twins_booked(ctx, id, owner, shares, from_block).await?;
        let logs = ctx
            .reader
            .logs(
                ctx.vault,
                IVault::ClaimBooked::SIGNATURE_HASH,
                None,
                from_block,
            )
            .await?;
        let mut claims: Vec<(U256, Option<B256>)> = logs
            .iter()
            .filter(|log| log.address == ctx.vault)
            .filter_map(|log| {
                IVault::ClaimBooked::decode_raw_log(log.topics.iter().copied(), &log.data)
                    .ok()
                    .filter(|event| event.owner == owner && event.shares == shares)
                    .map(|event| (event.claimId, log.transaction))
            })
            .collect();
        claims.sort_by_key(|(claim, _)| *claim);
        claims.dedup_by_key(|(claim, _)| *claim);
        if later_twins_booked(ctx, id, owner, shares, from_block).await? != later {
            continue;
        }
        return Ok(claims
            .len()
            .checked_sub(later + 1)
            .map(|index| claims[index]));
    }
    Err(TxError::Network(
        "its claim couldn't be told from its twins".to_string(),
    ))
}

/// How many withdrawals made after `id`, by the same owner for the same
/// shares, have been booked as claims: settled, and not paid by id.
async fn later_twins_booked<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
    owner: Address,
    shares: U256,
    from_block: u64,
) -> Result<usize, TxError> {
    let count = read::read(ctx.reader, ctx.vault, IVault::requestCountCall {}).await?;
    if count <= id {
        return Ok(0);
    }
    if count - id > U256::from(MATCH_READS) {
        return Err(TxError::Network(
            "too many requests since to match its claim".to_string(),
        ));
    }
    let paid: Vec<U256> = decode::<IVault::Withdrawn>(
        ctx.vault,
        &ctx.reader
            .logs(
                ctx.vault,
                IVault::Withdrawn::SIGNATURE_HASH,
                None,
                from_block,
            )
            .await?,
    )
    .into_iter()
    .map(|event| event.requestId)
    .collect();
    let mut booked = 0;
    let mut later = id + U256::from(1u8);
    while later <= count {
        let request = read::request(ctx.reader, ctx.vault, later).await?;
        if request.owner == owner
            && request.kind == RequestKind::Redeem
            && request.amount == shares
            && request.done
            && !paid.contains(&later)
        {
            booked += 1;
        }
        later += U256::from(1u8);
    }
    Ok(booked)
}

/// The endpoint we read through answers for the vault's chain.
async fn check_endpoint<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
) -> Result<(), TxError> {
    let endpoint = ctx.reader.chain_id().await?;
    if endpoint != ctx.chain_id {
        return Err(TxError::WrongEndpoint {
            endpoint,
            vault: ctx.chain_id,
        });
    }
    Ok(())
}

/// Ask for the statement, after checking the digest it will produce is the
/// one the vault checks, and check the signature recovers to the account.
async fn sign_statement<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
) -> Result<Bytes, TxError> {
    let restricted = read::read(ctx.reader, ctx.vault, IVault::restrictedTextCall {}).await?;
    let statement = StatementRequest {
        chain_id: ctx.chain_id,
        vault: ctx.vault,
        depositor: ctx.account,
        restricted,
    };
    let theirs = read::read(
        ctx.reader,
        ctx.vault,
        IVault::statementDigestCall {
            depositor: ctx.account,
        },
    )
    .await?;
    let digest = statement.digest();
    if theirs != digest {
        return Err(TxError::StatementMismatch);
    }
    ctx.guard().await?;
    ctx.step(Step::Signing(Prompt::Statement));
    let signature = ctx.signer.sign_statement(&statement).await?;
    if !recovers_to(&signature, digest, ctx.account) {
        return Err(TxError::BadSignature);
    }
    as_the_vault_takes_it(&signature).ok_or(TxError::BadSignature)
}

/// Whether a 65-byte signature over `digest` is `account`'s.
pub fn recovers_to(signature: &[u8], digest: B256, account: Address) -> bool {
    Signature::try_from(signature)
        .ok()
        .and_then(|signature| signature.recover_address_from_prehash(&digest).ok())
        == Some(account)
}

/// A 65-byte signature in the form the vault's check takes: `r`, `s`, then
/// `v` as 27 or 28. Some wallets end it with the recovery id itself (0 or
/// 1), which the vault refuses; the signature is the same.
pub fn as_the_vault_takes_it(signature: &[u8]) -> Option<Bytes> {
    let parsed = Signature::try_from(signature).ok()?;
    Some(Bytes::from(parsed.as_bytes().to_vec()))
}

/// The request a receipt made, and when.
async fn requested<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    receipt: &Receipt,
) -> Result<Countdown, TxError> {
    let event = ctx
        .events::<IVault::Requested>(receipt)
        .into_iter()
        .next()
        .ok_or_else(|| {
            TxError::Network("the request's record wasn't in its receipt".to_string())
        })?;
    let asked_at = ctx.reader.block_time(receipt.block).await?;
    let countdown = Countdown {
        request: event.requestId,
        hash: receipt.hash,
        asked_at,
        slot: event.earliest,
        moved_from: None,
    };
    ctx.step(Step::Requested(countdown));
    Ok(countdown)
}

/// Wait for the request to be priced and settled, moving its slot on when a
/// valuation isn't recorded in time, and sending the second step if nobody
/// else has a few seconds after the price. The time it was priced at.
///
/// The second step is optional: the keeper settles with the valuation, and
/// anyone may. If the app's own try isn't sent (declined, another chain, no
/// fee, reverted, not seen mined in a short watch), that is reported on the
/// priced step and the flow keeps watching the request; the person may ask
/// for it again ("Settle now"). Whatever becomes of it, the flow ends as soon
/// as the vault reports the request settled, by anyone. A read that fails
/// is one more look. When the long wait runs out the request still stands:
/// [`TxError::Landed`] with nothing that stopped it.
async fn priced<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    mut countdown: Countdown,
    timers: &Timers,
) -> Result<u64, TxError> {
    let earliest = countdown.slot;
    let mut priced_at: Option<u64> = None;
    let mut since_priced = 0u32;
    let mut tried = false;
    let mut misses = 0u32;
    for _ in 0..ctx.pace.give_up_after {
        let look = async {
            let request = read::request(ctx.reader, ctx.vault, countdown.request).await?;
            let last = read::last_priced(ctx.reader, ctx.vault).await?;
            let newly = match last.filter(|last| last.time >= earliest) {
                Some(last) if priced_at.is_none() => {
                    Some(pricing_time(ctx, &last, earliest).await?.time)
                }
                _ => None,
            };
            let now = match (priced_at, newly) {
                (None, None) => Some(ctx.reader.chain_time().await?),
                _ => None,
            };
            Ok::<_, TxError>((request, newly, now))
        }
        .await;
        let (request, newly, now) = match look {
            Ok(look) => {
                misses = 0;
                look
            }
            Err(error) => {
                misses += 1;
                if misses > ctx.pace.dropped_after {
                    return Err(error);
                }
                ctx.clock.sleep(ctx.pace.poll_millis).await;
                continue;
            }
        };
        if let Some(at) = newly {
            priced_at = Some(at);
            countdown.slot = at;
            ctx.step(Step::Priced {
                countdown,
                at,
                unsent: None,
                sent: None,
            });
        }
        if request.done {
            return Ok(priced_at.unwrap_or(countdown.slot));
        }
        match priced_at {
            Some(at) => {
                since_priced += 1;
                // Settling is open to anyone; the keeper does it with the
                // valuation. If nobody has, the app sends it, once, and again
                // whenever the person asks.
                let asked = ctx.settle_now.take();
                if asked || (!tried && since_priced >= ctx.pace.finish_after) {
                    tried = true;
                    let report = |unsent: Option<TxError>, sent: Option<B256>| {
                        ctx.step(Step::Priced {
                            countdown,
                            at,
                            unsent,
                            sent,
                        })
                    };
                    match second_step(ctx, countdown.request, &report).await {
                        Second::Settled => return Ok(at),
                        Second::Open(unsent) => report(unsent, None),
                        // Sent and mined: said as sent, so the page never
                        // says the app will still send it.
                        Second::Mined(hash) => report(None, Some(hash)),
                    }
                }
            }
            None => {
                if let Some(now) = now {
                    let slot = read::live_slot(earliest, now, timers);
                    if slot != countdown.slot {
                        countdown.moved_from = Some(countdown.slot);
                        countdown.slot = slot;
                        ctx.step(Step::Requested(countdown));
                    }
                }
            }
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
    Err(TxError::Landed {
        hash: countdown.hash,
        request: Some(countdown.request),
        then: None,
    })
}

/// Where the app's own second step left a priced request.
enum Second {
    /// The vault reports it settled, by anyone.
    Settled,
    /// Not settled yet; why the app's own step wasn't sent or seen mined,
    /// if it wasn't.
    Open(Option<TxError>),
    /// The app's own step was mined without settling it: the keeper does.
    Mined(B256),
}

/// What ended the wait on the wallet for the second step, before it
/// answered.
enum Meanwhile {
    /// The vault reports the request settled, by anyone.
    Settled,
    /// The person let the step go ("Skip").
    Skipped,
}

/// While the wallet is asked for the second step: read the request at
/// every look, and end when it is settled or the step is skipped, for as
/// long as the prompt is shown. The wallet's answer ends it too.
async fn while_asked<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
) -> Meanwhile {
    loop {
        if ctx.skip_second.take() {
            return Meanwhile::Skipped;
        }
        if matches!(read::request(ctx.reader, ctx.vault, id).await, Ok(request) if request.done) {
            return Meanwhile::Settled;
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
}

/// The settling call the second step sends: open to anyone.
fn settle_call() -> Vec<u8> {
    IVault::settleCall {
        max: U256::from(256u32),
    }
    .abi_encode()
}

/// The app's own second step for a priced request: not asked for at all if
/// the request is already settled; once sent (`report`ed with its hash),
/// watched briefly while the request is read too, ending as soon as either
/// says it is settled.
async fn second_step<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
    report: &dyn Fn(Option<TxError>, Option<B256>),
) -> Second {
    if matches!(read::request(ctx.reader, ctx.vault, id).await, Ok(request) if request.done) {
        return Second::Settled;
    }
    // A "Skip" pressed before this step was asked for is not for it.
    ctx.skip_second.take();
    // The request is read while the wallet is asked: a wallet that never
    // answers doesn't hold a request the vault has settled, and "Skip" lets
    // only this step go.
    let hash = {
        let mut send = std::pin::pin!(ctx.send(Prompt::Finish, ctx.vault, settle_call()));
        let mut meanwhile = std::pin::pin!(while_asked(ctx, id));
        let first = std::future::poll_fn(|cx| {
            if let std::task::Poll::Ready(sent) = send.as_mut().poll(cx) {
                return std::task::Poll::Ready(Ok(sent));
            }
            meanwhile.as_mut().poll(cx).map(Err)
        })
        .await;
        match first {
            Ok(Ok(hash)) => hash,
            Ok(Err(why)) => return Second::Open(Some(why)),
            Err(Meanwhile::Settled) => return Second::Settled,
            Err(Meanwhile::Skipped) => return Second::Open(Some(TxError::Cancelled)),
        }
    };
    report(None, Some(hash));
    match watch_second(ctx, id, hash).await {
        Watched::Settled => Second::Settled,
        // Mined, and the request not among those it settled: the keeper
        // settles it.
        Watched::Mined => Second::Mined(hash),
        Watched::Reverted => Second::Open(Some(TxError::Reverted { hash, reason: None })),
        Watched::NotSeen(why) => Second::Open(Some(why)),
    }
}

/// What a short watch of the second step saw.
enum Watched {
    /// The request is settled (by this transaction or anyone's).
    Settled,
    /// Mined and successful, the request not settled by it.
    Mined,
    Reverted,
    /// Not seen mined within the watch: it may still land.
    NotSeen(TxError),
}

/// Watch the second step (`hash`) for [`Pace::finish_watch`] looks, reading
/// the request at every look: it ends as soon as the request is settled,
/// whoever settled it, or the step is mined.
async fn watch_second<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
    hash: B256,
) -> Watched {
    let mut known = false;
    for _ in 0..ctx.pace.finish_watch.max(1) {
        if matches!(read::request(ctx.reader, ctx.vault, id).await, Ok(request) if request.done) {
            return Watched::Settled;
        }
        if let Ok(Some(receipt)) = ctx.reader.receipt(hash).await {
            if !receipt.success {
                return Watched::Reverted;
            }
            return match read::request(ctx.reader, ctx.vault, id).await {
                Ok(request) if !request.done => Watched::Mined,
                _ => Watched::Settled,
            };
        }
        if matches!(ctx.reader.pending(hash).await, Ok(Pending::Known { .. })) {
            known = true;
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
    Watched::NotSeen(if known {
        TxError::Stuck { hash }
    } else {
        TxError::Dropped { hash }
    })
}

/// The valuation that priced a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PricedBy {
    time: u64,
    index: U256,
}

/// The valuation that priced a request with this `earliest`: the first
/// priced one at or after it. The newest priced valuation may be a later
/// one (on a fast chain, several can pass between two looks), so walk back
/// from it while valuations are still at or after `earliest`.
async fn pricing_time<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    last: &read::Priced,
    earliest: u64,
) -> Result<PricedBy, TxError> {
    let mut by = PricedBy {
        time: last.time,
        index: last.index,
    };
    let mut index = last.index;
    for _ in 0..64 {
        if index.is_zero() {
            break;
        }
        let previous = index - U256::from(1u8);
        let valuation = read::read(
            ctx.reader,
            ctx.vault,
            IVault::valuationAtCall { index: previous },
        )
        .await?;
        if valuation.time < earliest {
            break;
        }
        if matches!(
            valuation.status,
            chain::bindings::vault::ValuationStatus::Priced
        ) {
            by = PricedBy {
                time: valuation.time,
                index: previous,
            };
        }
        index = previous;
    }
    Ok(by)
}

// Withdraw -----------------------------------------------------------------

/// Withdraw `shares`: a request priced at the next valuation, paid from cash
/// or booked as a claim. A closed vault pays at once from its cash.
pub async fn withdraw<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    shares: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    if shares.is_zero() {
        return Err(TxError::NothingToSend);
    }
    // The vault's state from the chain, not the snapshot.
    let figures = read::figures(ctx.reader, ctx.vault).await?;
    let held = read::read(
        ctx.reader,
        ctx.vault,
        IVault::balanceOfCall { owner: ctx.account },
    )
    .await?;
    if held < shares {
        return Err(TxError::InsufficientShares { have: held });
    }
    match figures.state {
        State::Closed => return redeem_closed(ctx, shares).await,
        State::None | State::Cancelled | State::Activating => return Err(TxError::WrongState),
        _ if figures.ready_to_close => return Err(TxError::WrongState),
        _ => {}
    }

    let receipt = ctx
        .transact(
            Prompt::Withdraw { shares },
            ctx.vault,
            IVault::requestRedeemCall {
                shares,
                receiver: ctx.account,
            }
            .abi_encode(),
        )
        .await?;
    follow(ctx, &receipt, RequestKind::Redeem, shares, &figures.timers).await
}

/// A closed vault's payout: the shares' fraction of its cash, at once.
async fn redeem_closed<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    shares: U256,
) -> Result<Outcome, TxError> {
    let receipt = ctx
        .transact(
            Prompt::Closed,
            ctx.vault,
            IVault::redeemClosedCall {
                shares,
                receiver: ctx.account,
            }
            .abi_encode(),
        )
        .await?;
    let paid = ctx
        .events::<IVault::Withdrawn>(&receipt)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| TxError::Network("the payout's record wasn't in its receipt".to_string()))
        .map_err(landed(receipt.hash, None))?;
    Ok(Outcome::Paid {
        shares: paid.shares,
        assets: paid.assets,
        at: None,
        hash: receipt.hash,
    })
}

// Settling a request now ---------------------------------------------------

/// The second step on its own ("Settle now"), for a request already priced
/// and not yet settled: anyone may send it. A deposit, a withdrawal, or a
/// claim's own request (which pays its claim); a tank fill is the author's.
/// It ends as the request settled, read from the vault's events for it
/// wherever it was settled: if someone else settled it first, before the
/// wallet was asked, nothing is sent and that is said
/// ([`Outcome::SettledBefore`]). The step is watched briefly: it is
/// optional, and the keeper settles the request anyway.
pub async fn settle_request<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    id: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let request = read::request(ctx.reader, ctx.vault, id).await?;
    if !matches!(
        request.kind,
        RequestKind::Deposit | RequestKind::Redeem | RequestKind::Claim
    ) {
        return Err(TxError::WrongState);
    }
    let last = read::last_priced(ctx.reader, ctx.vault)
        .await?
        .filter(|last| last.time >= request.earliest)
        .ok_or(TxError::WouldRevert {
            reason: Revert::Vault("NotYet"),
        })?;
    let by = pricing_time(ctx, &last, request.earliest).await?;
    // Every settlement of the request comes after the valuation that priced
    // it was opened: its events are read from there.
    let from = read::read(
        ctx.reader,
        ctx.vault,
        IVault::openedBlockCall { index: by.index },
    )
    .await?;
    let outcome = |hash: Option<B256>| settled(ctx, id, &request, from, by.time, hash);
    if read::request(ctx.reader, ctx.vault, id).await?.done {
        return Ok(Outcome::SettledBefore(Box::new(outcome(None).await?)));
    }
    let hash = ctx.send(Prompt::Finish, ctx.vault, settle_call()).await?;
    ctx.step(Step::Sent {
        prompt: Prompt::Finish,
        hash,
        unseen: false,
    });
    let landed = landed(hash, Some(id));
    match watch_second(ctx, id, hash).await {
        // Whoever settled it, its events say how.
        Watched::Settled => outcome(None).await.map_err(&landed),
        // Mined, and this request not among those it settled: it still
        // stands, and the keeper settles it.
        Watched::Mined => Err(TxError::Landed {
            hash,
            request: Some(id),
            then: None,
        }),
        Watched::Reverted => Err(TxError::Reverted { hash, reason: None }),
        Watched::NotSeen(why) => Err(why),
    }
}

// Claims, the fallback, collecting ------------------------------------------

/// Push a claim through: anyone may. On an open vault (`settleClaim`, for a
/// claim pending or waiting) it ends paid, waiting (a market outside the
/// band), queued for the next valuation, or closing. On a closed vault
/// (`claimClosed`, for a claim pending, waiting or queued) it is paid its
/// share of the cash. A vault ready to close takes neither until it closes.
pub async fn settle_claim<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    claim: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let before = read::claim(ctx.reader, ctx.vault, claim).await?;
    let figures = read::figures(ctx.reader, ctx.vault).await?;
    let closed = figures.state == State::Closed;
    if !claim_door_open(before.state, figures.state, figures.ready_to_close) {
        return Err(TxError::WrongState);
    }
    let receipt = if closed {
        ctx.transact(
            Prompt::Closed,
            ctx.vault,
            IVault::claimClosedCall { claimId: claim }.abi_encode(),
        )
        .await?
    } else {
        ctx.transact(
            Prompt::SettleClaim { claim },
            ctx.vault,
            IVault::settleClaimCall { claimId: claim }.abi_encode(),
        )
        .await?
    };
    let after = read::claim(ctx.reader, ctx.vault, claim)
        .await
        .map_err(landed(receipt.hash, None))?;
    Ok(Outcome::Claim {
        claim,
        owner: before.owner,
        state: after.state,
        paid: after.paid,
        hash: receipt.hash,
    })
}

/// Whether a claim in `claim` can be pushed through on a vault in `vault`,
/// as the contract takes it: `settleClaim` for a claim pending or waiting on
/// a vault neither closed, cancelled nor ready to close; `claimClosed` for a
/// claim pending, waiting or queued on a closed vault.
pub fn claim_door_open(claim: ClaimState, vault: State, ready_to_close: bool) -> bool {
    match vault {
        State::Closed => matches!(
            claim,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ),
        State::Cancelled | State::None => false,
        _ => !ready_to_close && matches!(claim, ClaimState::Pending | ClaimState::Waiting),
    }
}

/// Whether the claim fallback (`takeProRataClaim`) is open to its owner:
/// a claim pending, waiting or queued, on a vault neither closed, cancelled
/// nor ready to close, a day after it was booked; a queued claim also a day
/// after the slot its own request was queued for (the vault queues it, at
/// its last attempt, for the next slot). Whether the cash would cover it is
/// left to the chain's own estimate.
pub fn claim_fallback_open(
    claim: &read::Claim,
    vault: State,
    ready_to_close: bool,
    timers: &Timers,
    now: u64,
) -> bool {
    let from = match claim.state {
        ClaimState::Pending | ClaimState::Waiting => claim.booked_at,
        ClaimState::Queued => claim
            .booked_at
            .max(read::next_slot(claim.last_attempt_at, timers)),
        _ => return false,
    };
    !matches!(vault, State::Closed | State::Cancelled | State::None)
        && !ready_to_close
        && now >= from.saturating_add(timers.fallback_after)
}

/// What the fallback is taken for: a waiting request, or a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Request(U256),
    Claim(U256),
}

/// The fallback, when nothing has priced for a day, and only by the owner.
/// On a withdrawal or a claim: the exact share of the cash now, and units
/// for the rest of what the stations hold. On a deposit: the money back to
/// the wallet that paid it in, nothing bought (the vault records a refund).
pub async fn take_cash<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    of: Fallback,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let (prompt, data, deposit) = match of {
        Fallback::Request(id) => {
            let request = read::request(ctx.reader, ctx.vault, id).await?;
            if request.done || request.owner != ctx.account {
                return Err(TxError::WouldRevert {
                    reason: Revert::Vault("NotAllowed"),
                });
            }
            let data = IVault::takeProRataCall { requestId: id }.abi_encode();
            match request.kind {
                RequestKind::Deposit => (
                    Prompt::TakeBack {
                        amount: request.amount,
                    },
                    data,
                    Some(id),
                ),
                RequestKind::Redeem => (Prompt::TakeCash, data, None),
                RequestKind::Claim | RequestKind::TankFill => {
                    return Err(TxError::WouldRevert {
                        reason: Revert::Vault("NotAllowed"),
                    });
                }
            }
        }
        Fallback::Claim(id) => (
            Prompt::TakeCash,
            IVault::takeProRataClaimCall { claimId: id }.abi_encode(),
            None,
        ),
    };
    let receipt = ctx.transact(prompt, ctx.vault, data).await?;
    let landed = landed(receipt.hash, None);
    if let Some(id) = deposit {
        let refund = ctx
            .events::<IVault::Refunded>(&receipt)
            .into_iter()
            .find(|event| event.requestId == id)
            .ok_or_else(|| {
                TxError::Network("the refund's record wasn't in its receipt".to_string())
            })
            .map_err(&landed)?;
        return Ok(Outcome::Refunded {
            assets: refund.assets,
            to: refund.to,
            hash: receipt.hash,
        });
    }
    let taken = ctx
        .events::<IVault::ProRataTaken>(&receipt)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| TxError::Network("the fallback's record wasn't in its receipt".to_string()))
        .map_err(&landed)?;
    Ok(Outcome::TookCash {
        assets: taken.assets,
        units: taken.units,
        hash: receipt.hash,
    })
}

/// A withdrawal nothing has priced for a day, made a claim instead.
pub async fn claim_instead<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let receipt = ctx
        .transact(
            Prompt::ClaimInstead,
            ctx.vault,
            IVault::claimInsteadCall { requestId: request }.abi_encode(),
        )
        .await?;
    let booked = ctx
        .events::<IVault::ClaimBooked>(&receipt)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| TxError::Network("the claim's record wasn't in its receipt".to_string()))
        .map_err(landed(receipt.hash, None))?;
    Ok(Outcome::ClaimMade {
        claim: booked.claimId,
        hash: receipt.hash,
    })
}

/// What the fallback's units have earned so far.
pub async fn collect_units<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let units = read::read(
        ctx.reader,
        ctx.vault,
        IVault::unitsOfCall {
            holder: ctx.account,
        },
    )
    .await?;
    if units.claimable.is_zero() {
        return Err(TxError::WouldRevert {
            reason: Revert::Vault("Nothing"),
        });
    }
    let receipt = ctx
        .transact(
            Prompt::CollectUnits,
            ctx.vault,
            IVault::collectUnitsCall {}.abi_encode(),
        )
        .await?;
    let paid = ctx
        .events::<IVault::UnitsPaid>(&receipt)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| TxError::Network("the payment's record wasn't in its receipt".to_string()))
        .map_err(landed(receipt.hash, None))?;
    Ok(Outcome::Collected {
        assets: paid.assets,
        hash: receipt.hash,
    })
}

/// A payout the dollar refused to deliver, held for the account.
pub async fn collect_held<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let held = read::read(
        ctx.reader,
        ctx.vault,
        IVault::heldForCall { owner: ctx.account },
    )
    .await?;
    if held.is_zero() {
        return Err(TxError::WouldRevert {
            reason: Revert::Vault("Nothing"),
        });
    }
    let receipt = ctx
        .transact(
            Prompt::CollectHeld,
            ctx.vault,
            IVault::collectHeldCall {}.abi_encode(),
        )
        .await?;
    // It emits no event of its own: the dollar's transfer is the record.
    let paid = decode::<IERC20::Transfer>(ctx.asset, &receipt.logs)
        .into_iter()
        .find(|event| event.from == ctx.vault && event.to == ctx.account)
        .map(|event| event.value)
        .unwrap_or(held);
    Ok(Outcome::Collected {
        assets: paid,
        hash: receipt.hash,
    })
}

/// A deposit or withdrawal a closed vault never priced: a withdrawal paid
/// its fraction of the cash, a deposit sent back. A claim's own request is
/// collected through its claim, and a tank fill isn't a depositor's.
pub async fn settle_closed<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    if read::state(ctx.reader, ctx.vault).await? != State::Closed {
        return Err(TxError::WrongState);
    }
    let waiting = read::request(ctx.reader, ctx.vault, request).await?;
    if waiting.done || !matches!(waiting.kind, RequestKind::Deposit | RequestKind::Redeem) {
        return Err(TxError::WrongState);
    }
    let receipt = ctx
        .transact(
            Prompt::Closed,
            ctx.vault,
            IVault::settleClosedCall { requestId: request }.abi_encode(),
        )
        .await?;
    if let Some(paid) = ctx.events::<IVault::Withdrawn>(&receipt).into_iter().next() {
        return Ok(Outcome::Paid {
            shares: paid.shares,
            assets: paid.assets,
            at: None,
            hash: receipt.hash,
        });
    }
    if let Some(refund) = ctx.events::<IVault::Refunded>(&receipt).into_iter().next() {
        return Ok(Outcome::Refunded {
            assets: refund.assets,
            to: refund.to,
            hash: receipt.hash,
        });
    }
    Err(landed(receipt.hash, Some(request))(TxError::Network(
        "the payout's record wasn't in its receipt".to_string(),
    )))
}
