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

use std::cell::Cell;
use std::rc::Rc;

use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_sol_types::{SolCall, SolEvent};
use chain::bindings::erc20::IERC20;
use chain::bindings::vault::IVault;

use super::error::{Revert, TxError};
use super::port::{Clock, Log, Pending, Reader, Receipt, Signer, StatementRequest, TxRequest};
use super::read::{self, ClaimState, State, Timers};

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
    /// The second step: settle what the valuation priced.
    Finish,
    SettleClaim {
        claim: U256,
    },
    /// The fallback: the cash share now, units for the rest.
    TakeCash,
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
    /// Handed to the network; waiting for it to be mined.
    Sent {
        prompt: Prompt,
        hash: B256,
    },
    /// The request is on the chain; it is priced at `slot`.
    Requested(Countdown),
    /// Priced; the second step follows.
    Priced {
        countdown: Countdown,
        at: u64,
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
    /// A deposit sent back (the vault stopped taking deposits, or its list
    /// changed, between the request and its price).
    Refunded { assets: U256, hash: B256 },
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
    /// A claim settled.
    Claim {
        claim: U256,
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
    /// Polls before the flow stops watching (the request still stands).
    pub give_up_after: u32,
}

impl Pace {
    pub const LIVE: Self = Self {
        poll_millis: 1_000,
        dropped_after: 180,
        finish_after: 8,
        give_up_after: 30 * 60,
    };
    pub const FAST: Self = Self {
        poll_millis: 0,
        dropped_after: 5,
        finish_after: 2,
        give_up_after: 200,
    };
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

    /// The one path that sends money: simulate (`eth_estimateGas`, so a
    /// send that would revert is refused before the wallet is asked), check
    /// the network fee against the coin held, [`Self::guard`] again, ask the
    /// wallet, then wait for the receipt and fail on a revert. Success is
    /// only ever a mined, successful receipt.
    pub(crate) async fn transact(
        &self,
        prompt: Prompt,
        to: Address,
        data: Vec<u8>,
    ) -> Result<Receipt, TxError> {
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
        self.step(Step::Signing(prompt.clone()));
        let hash = self.signer.send(&request).await?;
        self.step(Step::Sent { prompt, hash });
        let receipt = self.mined(hash).await?;
        if !receipt.success {
            return Err(TxError::Reverted { hash, reason: None });
        }
        Ok(receipt)
    }

    /// Wait for a sent transaction to be mined, or say what became of it:
    /// replaced, dropped, or still pending after a long wait.
    pub(crate) async fn mined(&self, hash: B256) -> Result<Receipt, TxError> {
        let mut known: Option<u64> = None;
        let mut unknown_polls = 0u32;
        for _ in 0..self.pace.give_up_after {
            if let Some(receipt) = self.reader.receipt(hash).await? {
                return Ok(receipt);
            }
            match self.reader.pending(hash).await? {
                Pending::Known { nonce } => {
                    known = Some(nonce);
                    unknown_polls = 0;
                }
                Pending::Unknown => {
                    unknown_polls += 1;
                    // Known once, gone now, and its nonce used by another.
                    if let Some(nonce) = known
                        && self.reader.mined_nonce(self.account).await? > nonce
                        && self.reader.receipt(hash).await?.is_none()
                    {
                        return Err(TxError::Replaced { hash });
                    }
                    if unknown_polls > self.pace.dropped_after {
                        return Err(TxError::Dropped { hash });
                    }
                }
            }
            self.clock.sleep(self.pace.poll_millis).await;
        }
        // Known to the network all this while and never mined: a fee too
        // low, most likely. It may still land, so this isn't a failure.
        Err(TxError::Stuck { hash })
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
    if allowed < amount {
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
        .await?;
    let countdown = requested(ctx, &receipt).await?;
    let at = priced(ctx, countdown, &figures.timers).await?;

    // How it settled, from the vault's own events for this request.
    let id = B256::from(countdown.request);
    let deposited = ctx
        .reader
        .logs(
            ctx.vault,
            IVault::Deposited::SIGNATURE_HASH,
            Some(id),
            receipt.block,
        )
        .await?;
    if let Some(event) = decode::<IVault::Deposited>(ctx.vault, &deposited)
        .into_iter()
        .next()
    {
        return Ok(Outcome::Deposited {
            assets: event.assets,
            shares: event.shares,
            at,
            hash: countdown.hash,
        });
    }
    let refunded = ctx
        .reader
        .logs(
            ctx.vault,
            IVault::Refunded::SIGNATURE_HASH,
            Some(id),
            receipt.block,
        )
        .await?;
    if let Some(event) = decode::<IVault::Refunded>(ctx.vault, &refunded)
        .into_iter()
        .next()
    {
        return Ok(Outcome::Refunded {
            assets: event.assets,
            hash: countdown.hash,
        });
    }
    Err(TxError::Network(
        "the deposit settled, but its record couldn't be read; your portfolio shows it".to_string(),
    ))
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
    Ok(signature)
}

/// Whether a 65-byte signature over `digest` is `account`'s.
pub fn recovers_to(signature: &[u8], digest: B256, account: Address) -> bool {
    Signature::try_from(signature)
        .ok()
        .and_then(|signature| signature.recover_address_from_prehash(&digest).ok())
        == Some(account)
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
async fn priced<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    mut countdown: Countdown,
    timers: &Timers,
) -> Result<u64, TxError> {
    let earliest = countdown.slot;
    let mut priced_at: Option<u64> = None;
    let mut since_priced = 0u32;
    let mut finished = false;
    for _ in 0..ctx.pace.give_up_after {
        let request = read::request(ctx.reader, ctx.vault, countdown.request).await?;
        let last = read::last_priced(ctx.reader, ctx.vault).await?;
        if let Some(last) = last.filter(|last| last.time >= earliest)
            && priced_at.is_none()
        {
            let at = pricing_time(ctx, &last, earliest).await?;
            priced_at = Some(at);
            countdown.slot = at;
            ctx.step(Step::Priced { countdown, at });
        }
        if request.done {
            return Ok(priced_at.unwrap_or(countdown.slot));
        }
        match priced_at {
            Some(_) => {
                since_priced += 1;
                // Settling is open to anyone; the keeper does it with the
                // valuation. If nobody has, the app sends it.
                if since_priced >= ctx.pace.finish_after && !finished {
                    finished = true;
                    ctx.transact(
                        Prompt::Finish,
                        ctx.vault,
                        IVault::settleCall {
                            max: U256::from(256u32),
                        }
                        .abi_encode(),
                    )
                    .await?;
                }
            }
            None => {
                let now = ctx.reader.chain_time().await?;
                let slot = read::live_slot(earliest, now, timers);
                if slot != countdown.slot {
                    countdown.moved_from = Some(countdown.slot);
                    countdown.slot = slot;
                    ctx.step(Step::Requested(countdown));
                }
            }
        }
        ctx.clock.sleep(ctx.pace.poll_millis).await;
    }
    Err(TxError::Network(
        "the request is still waiting for its price; it stands, and your portfolio shows it when it's priced"
            .to_string(),
    ))
}

/// The time of the valuation that priced a request with this `earliest`:
/// the first priced one at or after it. The newest priced valuation may be a
/// later one (on a fast chain, several can pass between two looks), so walk
/// back from it while valuations are still at or after `earliest`.
async fn pricing_time<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    last: &read::Priced,
    earliest: u64,
) -> Result<u64, TxError> {
    let mut time = last.time;
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
            time = valuation.time;
        }
        index = previous;
    }
    Ok(time)
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
    let countdown = requested(ctx, &receipt).await?;
    let at = priced(ctx, countdown, &figures.timers).await?;

    let id = B256::from(countdown.request);
    let withdrawn = ctx
        .reader
        .logs(
            ctx.vault,
            IVault::Withdrawn::SIGNATURE_HASH,
            Some(id),
            receipt.block,
        )
        .await?;
    if let Some(event) = decode::<IVault::Withdrawn>(ctx.vault, &withdrawn)
        .into_iter()
        .next()
    {
        return Ok(Outcome::Paid {
            shares: event.shares,
            assets: event.assets,
            at: Some(at),
            hash: countdown.hash,
        });
    }
    // Not paid: booked as a claim, for exactly these shares, in the
    // account's name, after the request.
    let booked = ctx
        .reader
        .logs(
            ctx.vault,
            IVault::ClaimBooked::SIGNATURE_HASH,
            None,
            receipt.block,
        )
        .await?;
    let claim = decode::<IVault::ClaimBooked>(ctx.vault, &booked)
        .into_iter()
        .filter(|event| event.owner == ctx.account && event.shares == shares)
        .map(|event| event.claimId)
        .max()
        .ok_or_else(|| {
            TxError::Network(
                "the withdrawal settled, but its record couldn't be read; your portfolio shows it"
                    .to_string(),
            )
        })?;
    Ok(Outcome::Booked {
        claim,
        shares,
        hash: countdown.hash,
    })
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
        .ok_or_else(|| TxError::Network("the payout's record wasn't in its receipt".to_string()))?;
    Ok(Outcome::Paid {
        shares: paid.shares,
        assets: paid.assets,
        at: None,
        hash: receipt.hash,
    })
}

// Claims, the fallback, collecting ------------------------------------------

/// Push a claim through (`settleClaim`): anyone may. It ends paid, waiting
/// (a market outside the band), queued for the next valuation, or closing.
pub async fn settle_claim<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    claim: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let before = read::claim(ctx.reader, ctx.vault, claim).await?;
    if !matches!(before.state, ClaimState::Pending | ClaimState::Waiting) {
        return Err(TxError::WrongState);
    }
    let state = read::state(ctx.reader, ctx.vault).await?;
    let receipt = if state == State::Closed {
        // A closed vault pays its claims from its cash.
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
    let after = read::claim(ctx.reader, ctx.vault, claim).await?;
    Ok(Outcome::Claim {
        claim,
        state: after.state,
        paid: after.paid,
        hash: receipt.hash,
    })
}

/// What the fallback is taken for: a withdrawal request, or a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Request(U256),
    Claim(U256),
}

/// The fallback, when nothing has priced for a day: the exact share of the
/// cash now, and units for the rest of what the stations hold.
pub async fn take_cash<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    of: Fallback,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    let data = match of {
        Fallback::Request(id) => IVault::takeProRataCall { requestId: id }.abi_encode(),
        Fallback::Claim(id) => IVault::takeProRataClaimCall { claimId: id }.abi_encode(),
    };
    let receipt = ctx.transact(Prompt::TakeCash, ctx.vault, data).await?;
    let taken = ctx
        .events::<IVault::ProRataTaken>(&receipt)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| {
            TxError::Network("the fallback's record wasn't in its receipt".to_string())
        })?;
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
        .ok_or_else(|| TxError::Network("the claim's record wasn't in its receipt".to_string()))?;
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
        .ok_or_else(|| {
            TxError::Network("the payment's record wasn't in its receipt".to_string())
        })?;
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

/// A request a closed vault never priced: a withdrawal paid from its cash,
/// a deposit sent back.
pub async fn settle_closed<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    request: U256,
) -> Result<Outcome, TxError> {
    ctx.step(Step::Checking);
    check_endpoint(ctx).await?;
    if read::state(ctx.reader, ctx.vault).await? != State::Closed {
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
            hash: receipt.hash,
        });
    }
    Err(TxError::Network(
        "the payout's record wasn't in its receipt".to_string(),
    ))
}
