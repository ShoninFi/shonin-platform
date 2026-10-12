//! Every author flow as a small state machine over the same ports as the
//! depositor's ([`Reader`], [`Signer`], [`Clock`]): checking → awaiting
//! signature → sent → done, or rejected or failed. The rules every flow
//! keeps are the depositor flows' own:
//!
//! - What a flow acts on is read from the chain first: the fee, the terms,
//!   the strategy's record, the vault's owner and state, the tank, the
//!   balance and the allowance.
//! - The account and the chain are checked immediately before every
//!   signature and every send, and the endpoint read through answers for the
//!   same chain.
//! - Every transaction goes through the one send path the depositor flows
//!   use ([`crate::tx::flow::Ctx::transact`]): simulated before the wallet
//!   is asked, the network fee checked against the coin the wallet holds,
//!   the account and chain checked again, and only a mined, successful
//!   receipt counts.
//! - An approval is for exactly the amount being paid, and only when the
//!   allowance doesn't already cover it.
//! - Success is reported only from a mined receipt and the contract's own
//!   event, never before.

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::SolCall;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::publishing::IPublishing;
use chain::bindings::vault::IVault;

use super::error::AuthorError;
use super::form::Allowances;
use super::read::{self, AuthorVault, Escape, Registries};
use crate::tx::TxError;
use crate::tx::flow::{Cancel, Pace, as_the_vault_takes_it, decode, recovers_to};
use crate::tx::port::{Clock, Reader, Receipt, Signer, StatementRequest};
use crate::tx::read::{State, allowance, asset_balance, read as call};

/// Who an approval lets take the dollar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spender {
    Publishing,
    Factory,
    Vault,
}

/// What the wallet is being asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// The author's own eligibility statement, for the vault about to exist.
    Statement,
    /// Let `spender` take exactly this many of the dollar's base units.
    Approve {
        amount: U256,
        spender: Spender,
    },
    /// The publishing fee.
    Pay {
        fee: U256,
    },
    /// Create the vault, with this starting tank.
    Activate {
        tank: U256,
    },
    FundTank {
        amount: U256,
    },
    SetKeep {
        amount: U256,
    },
    TopUpFromShares {
        shares: U256,
    },
    SetAllowances,
    SetIdleFloor {
        bps: u16,
    },
    CancelFloorChange {
        bps: u16,
    },
    Retire,
    RequestEscape,
    /// Pay `amount` out of the tank to the author, after at most `tab` of
    /// the bill so far is settled from it.
    WithdrawTank {
        amount: U256,
        tab: U256,
    },
}

/// How a flow ended well.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Paid: the job is admitted, and the strategy has its id.
    Paid {
        strategy_id: U256,
        hash: B256,
    },
    /// The chain already records a payment for this upload from this
    /// wallet, under this id: nothing was sent.
    AlreadyPaid {
        strategy_id: U256,
    },
    /// The vault exists.
    Activated {
        vault: Address,
        hash: B256,
    },
    TankFunded {
        amount: U256,
        hash: B256,
    },
    KeepSet {
        amount: U256,
        hash: B256,
    },
    /// A tank fill from the author's shares is queued for the next
    /// valuation.
    FillQueued {
        shares: U256,
        request: U256,
        hash: B256,
    },
    AllowancesSet {
        hash: B256,
    },
    /// The floor changes to `bps` at `effective_at` (now, for a raise).
    FloorSet {
        bps: u16,
        effective_at: u64,
        hash: B256,
    },
    /// The waiting lowering is off; the floor stays at `bps`.
    FloorKept {
        bps: u16,
        hash: B256,
    },
    /// Winding down: deposits are closed.
    Retired {
        hash: B256,
    },
    /// The unsigned withdrawal opens at `ready_at`.
    EscapeRequested {
        ready_at: u64,
        hash: B256,
    },
    /// Paid out of the tank to the author's wallet.
    TankWithdrawn {
        amount: U256,
        hash: B256,
    },
}

/// Where a flow is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Checking,
    Signing(Prompt),
    Sent { prompt: Prompt, hash: B256 },
    Done(Outcome),
    Failed(AuthorError),
}

/// Everything one flow acts on, fixed when it starts.
pub struct Ctx<'a, R, S, C> {
    pub reader: &'a R,
    pub signer: &'a S,
    pub clock: &'a C,
    pub chain_id: u64,
    /// The account the flow was built for.
    pub account: Address,
    pub cancel: Cancel,
    pub pace: Pace,
    pub on_step: &'a dyn Fn(Step),
}

type Result<T> = std::result::Result<T, AuthorError>;

impl<R: Reader, S: Signer, C: Clock> Ctx<'_, R, S, C> {
    fn step(&self, step: Step) {
        (self.on_step)(step);
    }

    /// The endpoint we read through answers for the flow's chain.
    async fn check_endpoint(&self) -> Result<()> {
        let endpoint = self.reader.chain_id().await?;
        if endpoint != self.chain_id {
            return Err(TxError::WrongEndpoint {
                endpoint,
                vault: self.chain_id,
            }
            .into());
        }
        Ok(())
    }

    /// The depositor flows' context over the same ports, for the one send
    /// path every flow shares. What it reports is translated into this
    /// flow's steps, under `prompt`.
    fn shared<'a>(
        &'a self,
        on_step: &'a dyn Fn(crate::tx::flow::Step),
    ) -> crate::tx::flow::Ctx<'a, R, S, C> {
        crate::tx::flow::Ctx {
            reader: self.reader,
            signer: self.signer,
            clock: self.clock,
            chain_id: self.chain_id,
            // Unused by the shared send path, which takes its target per call.
            vault: Address::ZERO,
            asset: Address::ZERO,
            account: self.account,
            cancel: self.cancel.clone(),
            // The author's flows have no second step to settle.
            settle_now: crate::tx::flow::Nudge::new(),
            skip_second: crate::tx::flow::Nudge::new(),
            pace: self.pace,
            on_step,
        }
    }

    /// Immediately before every signature and every send: not cancelled,
    /// the same account, and the wallet on the flow's chain (the shared
    /// guard).
    async fn guard(&self) -> Result<()> {
        let quiet = |_: crate::tx::flow::Step| {};
        Ok(self.shared(&quiet).guard().await?)
    }

    /// The shared send path (simulate, check the fee, guard, send, wait for
    /// a mined and successful receipt), its steps reported as this flow's,
    /// under `prompt`.
    async fn transact(&self, prompt: Prompt, to: Address, data: Vec<u8>) -> Result<Receipt> {
        use crate::tx::flow::Step as Shared;
        let translate = |step: Shared| match step {
            Shared::Signing(_) => self.step(Step::Signing(prompt.clone())),
            Shared::Sent { hash, .. } => self.step(Step::Sent {
                prompt: prompt.clone(),
                hash,
            }),
            _ => {}
        };
        // The shared path's own prompt is only reported, and translated
        // above; which one it carries doesn't matter.
        Ok(self
            .shared(&translate)
            .transact(crate::tx::flow::Prompt::Finish, to, data)
            .await?)
    }

    /// Approve `spender` for exactly `amount` of `asset`, unless the
    /// allowance already covers it (with `exact`, unless it already is
    /// exactly `amount`, so nothing more than the action stays approved
    /// after it); refuse first if the wallet doesn't hold it. Whether an
    /// approval was sent and mined.
    async fn pay_from_wallet(
        &self,
        asset: Address,
        spender: Address,
        who: Spender,
        amount: U256,
        exact: bool,
    ) -> Result<bool> {
        let have = asset_balance(self.reader, asset, self.account).await?;
        if have < amount {
            return Err(TxError::InsufficientBalance { have }.into());
        }
        let allowed = allowance(self.reader, asset, self.account, spender).await?;
        if allowed < amount || (exact && allowed != amount) {
            self.transact(
                Prompt::Approve {
                    amount,
                    spender: who,
                },
                asset,
                IERC20::approveCall {
                    spender,
                    value: amount,
                }
                .abi_encode(),
            )
            .await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// The vault as its author's controls act on it, read now; refused
    /// unless the connected account is its author.
    async fn authors_vault(&self, vault: Address) -> Result<AuthorVault> {
        self.check_endpoint().await?;
        let read = read::author_vault(self.reader, vault, self.account).await?;
        if read.author != self.account {
            return Err(AuthorError::NotAuthor);
        }
        Ok(read)
    }
}

/// Run a flow, reporting where it ends: done, or failed with its reason.
pub async fn report(
    on_step: &dyn Fn(Step),
    flow: impl Future<Output = Result<Outcome>>,
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

// Publishing -----------------------------------------------------------------

/// Pay the publishing fee against an upload, from the wallet that uploaded
/// it and no other: approve exactly the fee if the allowance doesn't cover
/// it, then pay. The contract takes no permit, so it is approve and pay, at
/// most two prompts.
///
/// The flow is bound to `uploader`, the account the upload named: the
/// context must be built for it, and the wallet must be on it when the flow
/// starts and immediately before every send (the shared guard compares the
/// wallet's account with the context's). A payment from any other account
/// would start nothing, so if the wallet is on another account nothing is
/// sent, and the words name the wallet to switch back to.
pub async fn pay<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    publishing: Address,
    upload: B256,
    uploader: Address,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if ctx.account != uploader || ctx.signer.account() != Some(uploader) {
        return Err(AuthorError::WrongPayer {
            uploader,
            approved: None,
        });
    }
    // The fee's approval, once mined: it stands if the payment is stopped.
    let approved = std::cell::Cell::new(None);
    let paid = pay_as_uploader(ctx, publishing, upload, &approved).await;
    let wrong_payer = AuthorError::WrongPayer {
        uploader,
        approved: approved.get(),
    };
    paid.map_err(|error| match error {
        // The wallet left the uploading account: the guard stopped it, or
        // the session cancelled the flow when the account changed.
        AuthorError::Tx(TxError::AccountChanged | TxError::NotConnected) => wrong_payer,
        AuthorError::Tx(TxError::Cancelled) if ctx.signer.account() != Some(uploader) => {
            wrong_payer
        }
        other => other,
    })
}

async fn pay_as_uploader<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    publishing: Address,
    upload: B256,
    approved: &std::cell::Cell<Option<U256>>,
) -> Result<Outcome> {
    ctx.check_endpoint().await?;
    // A payment for this upload from this wallet already on the chain (one
    // the app gave up on that landed after all): paying again would buy
    // nothing, so nothing is sent and the job is shown instead.
    if let read::PaymentRecord::ByUploader { strategy_id } =
        read::payment_after(ctx.reader, publishing, upload, ctx.account, None).await?
    {
        return Ok(Outcome::AlreadyPaid { strategy_id });
    }
    let terms = read::publishing(ctx.reader, publishing).await?;
    if ctx
        .pay_from_wallet(
            terms.asset,
            publishing,
            Spender::Publishing,
            terms.fee,
            false,
        )
        .await?
    {
        approved.set(Some(terms.fee));
    }
    // How many payments the contract has recorded before this one: a payment
    // that isn't seen mined is looked for after these.
    let since = call(ctx.reader, publishing, IPublishing::countCall {}).await?;
    let receipt = ctx
        .transact(
            Prompt::Pay { fee: terms.fee },
            publishing,
            IPublishing::submitCall { upload }.abi_encode(),
        )
        .await
        .map_err(|error| match error {
            AuthorError::Tx(then) if then.may_have_moved() => AuthorError::PaymentUnsure {
                upload,
                since,
                then: Box::new(then),
            },
            other => other,
        })?;
    // The job starts only for a payment from the uploading wallet against
    // this upload: say so if the record isn't that.
    let submitted = decode::<IPublishing::Submitted>(publishing, &receipt.logs)
        .into_iter()
        .find(|event| event.author == ctx.account && event.upload == upload)
        .ok_or(AuthorError::PaymentMismatch)?;
    Ok(Outcome::Paid {
        strategy_id: submitted.strategyId,
        hash: receipt.hash,
    })
}

// Activation -----------------------------------------------------------------

/// What an author chose on the activation page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub strategy_id: U256,
    pub allowances: Allowances,
    pub idle_floor_bps: u16,
    /// The starting tank, in the dollar's base units.
    pub tank: U256,
    /// "Keep my tank at"; zero is off.
    pub keep: U256,
    /// The terms' hash the page showed: the activation is held to them.
    pub terms: B256,
}

/// Create the strategy's vault: the author's statement for the vault about
/// to exist, an approval for exactly the starting tank, then the
/// activation, which takes the tank and opens the vault in one transaction.
pub async fn activate<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    at: &Registries,
    asset: Address,
    activation: &Activation,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    ctx.check_endpoint().await?;
    if activation.idle_floor_bps > 10_000 {
        return Err(AuthorError::FloorTooHigh);
    }
    let ready = read::readiness(ctx.reader, at, ctx.chain_id, activation.strategy_id).await?;
    if ready.vault.is_some() {
        return Err(AuthorError::AlreadyActive);
    }
    if !ready.registered || !ready.reviewed {
        return Err(AuthorError::NotReady);
    }
    if ready.author != ctx.account {
        return Err(AuthorError::NotAuthor);
    }
    if !ready.other_chains.is_empty() {
        return Err(AuthorError::OtherChains);
    }
    let terms = read::terms(ctx.reader, at.factory).await?;
    if terms.hash != activation.terms {
        return Err(AuthorError::TermsChanged);
    }
    if activation.tank < terms.minimum_tank {
        return Err(AuthorError::TankTooSmall {
            minimum: terms.minimum_tank,
        });
    }
    let have = asset_balance(ctx.reader, asset, ctx.account).await?;
    if have < activation.tank {
        return Err(TxError::InsufficientBalance { have }.into());
    }

    // The author is the vault's first holder and signs like every other,
    // for the vault at the address it will have, over the list it will keep.
    let vault = call(
        ctx.reader,
        at.factory,
        IVaultFactory::predictVaultCall {
            strategyId: activation.strategy_id,
        },
    )
    .await?;
    let restricted = call(
        ctx.reader,
        at.factory,
        IVaultFactory::restrictedForCall {
            strategyId: activation.strategy_id,
        },
    )
    .await?
    .text;
    let statement = StatementRequest {
        chain_id: ctx.chain_id,
        vault,
        depositor: ctx.account,
        restricted,
    };
    ctx.guard().await?;
    ctx.step(Step::Signing(Prompt::Statement));
    let signature = ctx.signer.sign_statement(&statement).await?;
    if !recovers_to(&signature, statement.digest(), ctx.account) {
        return Err(TxError::BadSignature.into());
    }
    // In the form the vault's check takes (`v` as 27 or 28), as every
    // statement is sent, and before the tank is approved.
    let signature = as_the_vault_takes_it(&signature).ok_or(TxError::BadSignature)?;

    // Exactly the tank: an approval left standing from an earlier try for
    // another tank is replaced, never left larger than the action.
    let approved = ctx
        .pay_from_wallet(asset, at.factory, Spender::Factory, activation.tank, true)
        .await?;
    let receipt = ctx
        .transact(
            Prompt::Activate {
                tank: activation.tank,
            },
            at.factory,
            IVaultFactory::activateCall {
                strategyId: activation.strategy_id,
                allowances: activation.allowances.to_sol(),
                idleFloorBps: activation.idle_floor_bps,
                tankFunding: activation.tank,
                tankKeep: activation.keep,
                statement: signature,
                terms: activation.terms,
            }
            .abi_encode(),
        )
        .await
        .map_err(|then| {
            // The approval was mined and the activation wasn't: it stands
            // until used, and trying again uses it. Not said when the
            // activation itself may still land and spend it.
            let landing = matches!(&then, AuthorError::Tx(error) if error.may_have_moved());
            if approved && !landing {
                AuthorError::ApprovalStands {
                    amount: activation.tank,
                    then: Box::new(then),
                }
            } else {
                then
            }
        })?;
    let activated = decode::<IVaultFactory::Activated>(at.factory, &receipt.logs)
        .into_iter()
        .find(|event| event.strategyId == activation.strategy_id && event.author == ctx.account)
        .ok_or_else(|| {
            TxError::Network("the activation's record wasn't in its receipt".to_string())
        })?;
    Ok(Outcome::Activated {
        vault: activated.vault,
        hash: receipt.hash,
    })
}

// The tank ---------------------------------------------------------------------

/// Top up the tank from the wallet: an approval for exactly the amount if
/// needed, then the funding. Only the author's controls offer it, so it is
/// refused for anyone else.
pub async fn fund_tank<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    asset: Address,
    amount: U256,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if amount.is_zero() {
        return Err(TxError::NothingToSend.into());
    }
    let read = ctx.authors_vault(vault).await?;
    if matches!(read.state, State::Closed | State::Cancelled | State::None) {
        return Err(AuthorError::NotOpen);
    }
    ctx.pay_from_wallet(asset, vault, Spender::Vault, amount, false)
        .await?;
    let receipt = ctx
        .transact(
            Prompt::FundTank { amount },
            vault,
            IVault::fundTankCall { assets: amount }.abi_encode(),
        )
        .await?;
    let funded = decode::<IVault::TankFunded>(vault, &receipt.logs)
        .into_iter()
        .find(|event| event.from == ctx.account)
        .ok_or_else(|| TxError::Network("the top-up's record wasn't in its receipt".to_string()))?;
    Ok(Outcome::TankFunded {
        amount: funded.assets,
        hash: receipt.hash,
    })
}

/// "Keep my tank at": auto-fill tops the tank back up to this from the
/// author's shares. Zero turns it off.
pub async fn set_keep<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    amount: U256,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    ctx.authors_vault(vault).await?;
    let receipt = ctx
        .transact(
            Prompt::SetKeep { amount },
            vault,
            IVault::setTankKeepCall { assets: amount }.abi_encode(),
        )
        .await?;
    let set = decode::<IVault::TankKeepSet>(vault, &receipt.logs)
        .into_iter()
        .next()
        .ok_or_else(|| TxError::Network("the change's record wasn't in its receipt".to_string()))?;
    Ok(Outcome::KeepSet {
        amount: set.assets,
        hash: receipt.hash,
    })
}

/// Turn some of the author's shares into tank money: queued, and priced at
/// the next valuation like any withdrawal.
pub async fn top_up_from_shares<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    shares: U256,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if shares.is_zero() {
        return Err(TxError::NothingToSend.into());
    }
    let read = ctx.authors_vault(vault).await?;
    if matches!(
        read.state,
        State::None | State::Activating | State::Closed | State::Cancelled
    ) || read.ready_to_close
    {
        return Err(AuthorError::NotOpen);
    }
    if read.shares < shares {
        return Err(TxError::InsufficientShares { have: read.shares }.into());
    }
    let receipt = ctx
        .transact(
            Prompt::TopUpFromShares { shares },
            vault,
            IVault::topUpFromSharesCall { shares }.abi_encode(),
        )
        .await?;
    let requested = decode::<IVault::Requested>(vault, &receipt.logs)
        .into_iter()
        .find(|event| event.owner == ctx.account)
        .ok_or_else(|| {
            TxError::Network("the request's record wasn't in its receipt".to_string())
        })?;
    Ok(Outcome::FillQueued {
        shares,
        request: requested.requestId,
        hash: receipt.hash,
    })
}

/// Ask to withdraw the tank without a signed bill: it opens after the
/// vault's delay, for as long again.
pub async fn request_escape<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    ctx.authors_vault(vault).await?;
    let receipt = ctx
        .transact(
            Prompt::RequestEscape,
            vault,
            IVault::requestTankWithdrawalCall {}.abi_encode(),
        )
        .await?;
    let asked = decode::<IVault::TankWithdrawalRequested>(vault, &receipt.logs)
        .into_iter()
        .next()
        .ok_or_else(|| {
            TxError::Network("the request's record wasn't in its receipt".to_string())
        })?;
    Ok(Outcome::EscapeRequested {
        ready_at: asked.readyAt,
        hash: receipt.hash,
    })
}

/// Withdraw from the tank without a signed bill, once the request has
/// waited and before it lapses. A closed or cancelled vault pays at once.
pub async fn withdraw_unsigned<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    amount: U256,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if amount.is_zero() {
        return Err(TxError::NothingToSend.into());
    }
    let read = ctx.authors_vault(vault).await?;
    match read.escape() {
        Escape::Open { .. } | Escape::Free => {}
        Escape::NotAsked => return Err(AuthorError::EscapeNotReady { ready_at: None }),
        Escape::Waiting { ready_at } => {
            return Err(AuthorError::EscapeNotReady {
                ready_at: Some(ready_at),
            });
        }
        Escape::Lapsed => return Err(AuthorError::EscapeLapsed),
    }
    within_tank(&read, amount)?;
    tank_out(
        ctx,
        vault,
        amount,
        U256::ZERO,
        IVault::withdrawTankUnsignedCall { assets: amount }.abi_encode(),
    )
    .await
}

/// A running total the agent's executor signed, as the API passes it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub total: U256,
    pub as_of: u64,
    pub signature: Bytes,
}

/// Withdraw from the tank, the bill so far settled first with the
/// executor's latest signed total: the tab is paid first, the rest goes to
/// the wallet.
///
/// The vault collects the bill first, as much of `total − settled` as its
/// cap and the tank allow, then pays what is asked out of what is left. So
/// the wallet is asked for no more than the tank less the whole unsettled
/// bill: what it names is then exactly what is paid, whatever the cap lets
/// the vault collect.
pub async fn withdraw_tank<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    amount: U256,
    usage: &Usage,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if amount.is_zero() {
        return Err(TxError::NothingToSend.into());
    }
    let read = ctx.authors_vault(vault).await?;
    let tab = tab_before_withdrawal(&read, usage)?;
    within_tank(&read, amount)?;
    let left = read.tank.balance - tab;
    if left.is_zero() {
        return Err(AuthorError::TabTakesTank { tab });
    }
    let amount = amount.min(left);
    let data = IVault::withdrawTankCall {
        assets: amount,
        total: usage.total,
        asOf: usage.as_of,
        signature: usage.signature.clone(),
    }
    .abi_encode();
    tank_out(ctx, vault, amount, tab, data).await
}

/// The most of the unsettled bill a signed withdrawal settles from the tank
/// first, as the vault takes the total: refused unless the vault would take
/// it (recent enough, never below what is settled, and, when it collects,
/// later than the last total it settled and not ahead of the chain).
pub fn tab_before_withdrawal(read: &AuthorVault, usage: &Usage) -> Result<U256> {
    let settled = read.tank.usage_settled;
    if usage.as_of.saturating_add(read.usage_freshness) < read.now || usage.total < settled {
        return Err(AuthorError::NoUsageTotal);
    }
    // A total the vault has already settled isn't collected again.
    let collects = usage.total != settled || usage.as_of != read.tank.usage_as_of;
    if !collects {
        return Ok(U256::ZERO);
    }
    if usage.as_of <= read.tank.usage_as_of || usage.as_of > read.now {
        return Err(AuthorError::NoUsageTotal);
    }
    Ok((usage.total - settled).min(read.tank.balance))
}

/// The vault pays at most what the tank holds; asking for more than that
/// would say one figure and pay another.
fn within_tank(read: &AuthorVault, amount: U256) -> Result<()> {
    if amount > read.tank.balance {
        return Err(TxError::InsufficientBalance {
            have: read.tank.balance,
        }
        .into());
    }
    Ok(())
}

async fn tank_out<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    amount: U256,
    tab: U256,
    data: Vec<u8>,
) -> Result<Outcome> {
    let receipt = ctx
        .transact(Prompt::WithdrawTank { amount, tab }, vault, data)
        .await?;
    let paid = decode::<IVault::TankWithdrawn>(vault, &receipt.logs)
        .into_iter()
        .next()
        .ok_or_else(|| {
            TxError::Network("the withdrawal's record wasn't in its receipt".to_string())
        })?;
    Ok(Outcome::TankWithdrawn {
        amount: paid.assets,
        hash: receipt.hash,
    })
}

// Allowances, the idle floor, retiring -------------------------------------------

pub async fn set_allowances<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    allowances: &Allowances,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    let read = ctx.authors_vault(vault).await?;
    if matches!(read.state, State::Closed | State::Cancelled | State::None) {
        return Err(AuthorError::NotOpen);
    }
    let receipt = ctx
        .transact(
            Prompt::SetAllowances,
            vault,
            IVault::setAllowancesCall {
                allowances: allowances.to_sol(),
            }
            .abi_encode(),
        )
        .await?;
    decode::<IVault::AllowancesSet>(vault, &receipt.logs)
        .into_iter()
        .next()
        .ok_or_else(|| TxError::Network("the change's record wasn't in its receipt".to_string()))?;
    Ok(Outcome::AllowancesSet { hash: receipt.hash })
}

/// Change the idle floor: a raise applies at once; a lowering waits the
/// vault's delay, with the old floor in force meanwhile.
pub async fn set_idle_floor<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
    bps: u16,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    if bps > 10_000 {
        return Err(AuthorError::FloorTooHigh);
    }
    ctx.authors_vault(vault).await?;
    let receipt = ctx
        .transact(
            Prompt::SetIdleFloor { bps },
            vault,
            IVault::setIdleFloorCall { bps }.abi_encode(),
        )
        .await?;
    let set = floor_event(vault, &receipt)?;
    Ok(Outcome::FloorSet {
        bps: set.bps,
        effective_at: set.effectiveAt,
        hash: receipt.hash,
    })
}

/// "Cancel change": keep the floor in force by setting it again, which
/// drops the lowering waiting. Refused when nothing is waiting, including
/// once its time has come (it has then taken effect).
pub async fn cancel_floor_change<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    let read = ctx.authors_vault(vault).await?;
    if read.floor.waiting(read.now).is_none() {
        return Err(AuthorError::NothingPending);
    }
    let keep = read.floor.now(read.now);
    let receipt = ctx
        .transact(
            Prompt::CancelFloorChange { bps: keep },
            vault,
            IVault::setIdleFloorCall { bps: keep }.abi_encode(),
        )
        .await?;
    let set = floor_event(vault, &receipt)?;
    Ok(Outcome::FloorKept {
        bps: set.bps,
        hash: receipt.hash,
    })
}

fn floor_event(vault: Address, receipt: &Receipt) -> Result<IVault::IdleFloorSet> {
    decode::<IVault::IdleFloorSet>(vault, &receipt.logs)
        .into_iter()
        .next()
        .ok_or_else(|| {
            TxError::Network("the change's record wasn't in its receipt".to_string()).into()
        })
}

/// Retire: deposits stop at once, and the vault winds down.
pub async fn retire<R: Reader, S: Signer, C: Clock>(
    ctx: &Ctx<'_, R, S, C>,
    vault: Address,
) -> Result<Outcome> {
    ctx.step(Step::Checking);
    let read = ctx.authors_vault(vault).await?;
    if read.state != State::Open {
        return Err(AuthorError::NotOpen);
    }
    let receipt = ctx
        .transact(Prompt::Retire, vault, IVault::retireCall {}.abi_encode())
        .await?;
    let winding = decode::<IVault::StateChanged>(vault, &receipt.logs)
        .into_iter()
        .any(|event| matches!(event.state, chain::bindings::vault::VaultState::WindingDown));
    if !winding {
        return Err(
            TxError::Network("the retirement's record wasn't in its receipt".to_string()).into(),
        );
    }
    Ok(Outcome::Retired { hash: receipt.hash })
}
