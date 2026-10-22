//! Running a flow from a page: the vault it acts on, the steps it reaches
//! as a signal, and a cancel that the session also pulls when the account
//! changes. In the browser it reads through the vault chain's public endpoint
//! and signs with the connected wallet.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use alloy_primitives::{Address, U256};
use futures_signals::signal::Mutable;
use shared::{Chain, Request, VaultState};

use crate::tx::flow::{Cancel, Fallback, Nudge, Step};
use crate::tx::port::Clock;

/// What a flow does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Deposit `amount`, stating for the vault's restricted list as the
    /// review showed it (`shown`, as the vault writes it).
    Deposit {
        amount: U256,
        shown: String,
    },
    Withdraw(U256),
    SettleClaim(U256),
    TakeCash(Fallback),
    ClaimInstead(U256),
    CollectUnits,
    CollectHeld,
    SettleClosed(U256),
    /// The second step for a priced request, now.
    Settle(U256),
}

/// A vault as a page needs it to act on it: from the snapshot, for its
/// address and chain only. Money figures are read from the chain.
#[derive(Clone, Debug)]
pub struct VaultRef {
    pub slug: String,
    pub title: String,
    pub chain: Chain,
    pub address: Address,
    pub served: bool,
    pub state: VaultState,
    /// Requests waiting for a price, as the indexer last read them.
    pub requests: Vec<Request>,
}

impl Action {
    /// Where a page shows this flow: the Exit panel's deposit and withdrawal
    /// share one place (one at a time per vault and account); every other
    /// action is its own, by kind and id.
    pub fn slot(&self) -> String {
        match self {
            Self::Deposit { .. } | Self::Withdraw(_) => EXIT.to_string(),
            other => format!("{other:?}"),
        }
    }
}

/// The Exit panel's slot.
pub const EXIT: &str = "exit";

/// A flow in progress (or ended and not yet dismissed): what it does, its
/// latest step, its cancel, and "Settle now". The session keeps it per
/// vault and account ([`super::Session::flow`]), so a panel or card built
/// while it runs shows where it is.
#[derive(Clone)]
pub struct Running {
    /// This flow, among every flow this page has started.
    pub id: u64,
    pub action: Action,
    /// The network its review was computed with (its amounts are scaled
    /// with this network's decimals): the flow signs with this config or
    /// not at all.
    pub reviewed: Rc<super::Network>,
    pub step: Mutable<Option<Step>>,
    pub cancel: Cancel,
    pub settle_now: Nudge,
    /// "Skip" on the optional second step's wallet prompt.
    pub skip_second: Nudge,
    /// When its countdown started on the reader's clock, for the seconds
    /// left; set once per request.
    pub requested_local: Rc<Cell<Option<(f64, u64)>>>,
    /// The wallet that signs and sends for this flow, recorded when the
    /// flow starts and again with each signature and send: its words name
    /// that wallet, not whichever is connected when they are drawn.
    pub sender: Rc<std::cell::RefCell<Option<super::Sender>>>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("id", &self.id)
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Running {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Running {
    /// The wallet that signed and sent for this flow, once recorded.
    pub fn sender(&self) -> Option<super::Sender> {
        self.sender.borrow().clone()
    }

    pub fn new(action: Action, reviewed: Rc<super::Network>) -> Self {
        thread_local! {
            static NEXT: Cell<u64> = const { Cell::new(1) };
        }
        Self {
            id: NEXT.with(|next| next.replace(next.get() + 1)),
            action,
            reviewed,
            step: Mutable::new(None),
            cancel: Cancel::new(),
            settle_now: Nudge::new(),
            skip_second: Nudge::new(),
            requested_local: Rc::new(Cell::new(None)),
            sender: Rc::default(),
        }
    }
}

/// Which flow a page shows: a vault, an account, a slot ([`Action::slot`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowKey {
    pub slug: String,
    pub account: Address,
    pub slot: String,
}

/// The browser's clock. Natively, where nothing is sent, it doesn't wait.
pub struct Timer;

impl Clock for Timer {
    async fn sleep(&self, millis: u32) {
        #[cfg(target_arch = "wasm32")]
        gloo_timers::future::TimeoutFuture::new(millis).await;
        #[cfg(not(target_arch = "wasm32"))]
        let _ = millis;
    }
}

/// What this browser keeps of a send, from the step that sent it: only a
/// deposit, a withdrawal or a claim's own transaction (never an approval,
/// or the optional second step).
pub fn recent_of(
    prompt: &crate::tx::flow::Prompt,
    vault: &VaultRef,
    account: Address,
    chain_id: u64,
    decimals: u8,
    hash: alloy_primitives::B256,
    at_ms: f64,
) -> Option<super::recent::Entry> {
    use super::recent::{Entry, Family, Kind};
    use crate::money::{Rounding, format as money};
    use crate::tx::flow::Prompt;
    let (kind, amount) = match prompt {
        Prompt::Deposit { amount } => (
            Kind::Deposit,
            money::dollars(*amount, decimals, Rounding::Down),
        ),
        Prompt::Withdraw { shares } => (
            Kind::Withdrawal,
            format!("{} shares", money::shares(*shares, Rounding::Down)),
        ),
        Prompt::SettleClaim { claim } => (Kind::Claim, format!("Settling claim {claim}")),
        Prompt::TakeCash => (Kind::Claim, "Taking the cash share".to_string()),
        Prompt::ClaimInstead => (Kind::Claim, "Making a claim instead".to_string()),
        _ => return None,
    };
    Some(Entry {
        tx: format!("{hash:?}"),
        tx_url: super::networks::tx_url(chain_id, hash),
        family: Family::Evm,
        account: format!("{account:?}"),
        slug: vault.slug.clone(),
        title: vault.title.clone(),
        kind,
        amount,
        at_ms,
        outcome: None,
    })
}

/// What a deposit's approval seen mined (`known`) becomes after `step` of
/// a deposit to the same vault from the same account: kept once the deposit
/// stops after it ([`TxError::ApprovalStands`]), so the next deposit takes it
/// as known rather than asking again on a node's short read; let go once the
/// deposit that uses it is sent, or once another approval is sent (the flow
/// asks for one only when a read at or past the known approval's block shows
/// it used, or when it doesn't cover the amount); kept through anything else,
/// a refusal from a node that hasn't caught up with it included.
///
/// [`TxError::ApprovalStands`]: crate::tx::TxError::ApprovalStands
pub fn approval_after(
    step: &Step,
    known: Option<crate::tx::flow::Approval>,
) -> Option<crate::tx::flow::Approval> {
    use crate::tx::TxError;
    use crate::tx::flow::{Approval, Prompt};
    match step {
        Step::Failed(TxError::ApprovalStands { amount, block, .. }) => Some(Approval {
            amount: *amount,
            block: *block,
        }),
        Step::Sent {
            prompt: Prompt::Deposit { .. } | Prompt::Approve { .. },
            ..
        } => None,
        _ => known,
    }
}

/// How a flow ended, in a few words for this browser's record.
pub fn outcome_words(outcome: &crate::tx::flow::Outcome, decimals: u8) -> String {
    use crate::money::{Rounding, format as money};
    use crate::tx::flow::Outcome;
    use crate::tx::read::ClaimState;
    let dollars = |value: &U256| money::dollars(*value, decimals, Rounding::Down);
    match outcome {
        Outcome::Deposited { shares, .. } => {
            format!("Bought {} shares", money::shares(*shares, Rounding::Down))
        }
        Outcome::Refunded { assets, .. } => format!("Sent back: {}", dollars(assets)),
        Outcome::Paid { assets, .. } => format!("Paid {}", dollars(assets)),
        Outcome::Booked { claim, .. } => format!("Booked as claim {claim}"),
        Outcome::SettledBefore(how) | Outcome::SentAfterSettled { how, .. } => {
            outcome_words(how, decimals)
        }
        Outcome::Claim {
            state: ClaimState::Paid,
            paid,
            ..
        } => format!("Paid {}", dollars(paid)),
        Outcome::Claim { .. } => "Still booked".to_string(),
        Outcome::TookCash { assets, .. } => format!("Paid {} in cash", dollars(assets)),
        Outcome::ClaimMade { claim, .. } => format!("Made claim {claim}"),
        Outcome::Collected { assets, .. } => format!("Collected {}", dollars(assets)),
    }
}

/// Start `action` on `vault`, reporting into `running`.
#[cfg(target_arch = "wasm32")]
pub fn start(vault: &VaultRef, action: Action, running: &Running) {
    use crate::tx::flow::{self, Ctx, Pace};
    use crate::wallet::session::{ApprovalKey, Session};

    let session = crate::wallet::get();
    let vault = vault.clone();
    let running = running.clone();
    running.sender.replace(Some(session.sender()));
    running.step.set(Some(Step::Checking));
    session.track(running.cancel.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let step = running.step.clone();
        let Some(account) = session.account() else {
            step.set(Some(Step::Failed(crate::tx::TxError::NotConnected)));
            return;
        };
        // This browser keeps the send once it is on its way, and how it
        // ended once the flow says.
        let kept: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
        let decimals = Rc::new(std::cell::Cell::new(6u8));
        // Where a deposit's approval is kept, once the network is known.
        let approval_key: Rc<std::cell::Cell<Option<ApprovalKey>>> = Default::default();
        let on_step = {
            let vault = vault.clone();
            let decimals = decimals.clone();
            let session = session.clone();
            let approval_key = approval_key.clone();
            let sender = running.sender.clone();
            move |next: Step| {
                if matches!(next, Step::Signing(_) | Step::Sent { .. }) {
                    sender.replace(Some(session.sender()));
                }
                if let Some(key) = approval_key.get() {
                    let known = session.approval(&key);
                    session.set_approval(key, approval_after(&next, known));
                }
                match &next {
                    Step::Sent { prompt, hash, .. } if kept.borrow().is_none() => {
                        let chain_id = crate::wallet::get()
                            .networks
                            .lock_ref()
                            .get(&vault.chain)
                            .map_or(0, |network| network.chain_id);
                        if let Some(entry) = recent_of(
                            prompt,
                            &vault,
                            account,
                            chain_id,
                            decimals.get(),
                            *hash,
                            js_sys::Date::now(),
                        ) {
                            *kept.borrow_mut() = Some(entry.tx.clone());
                            super::recent::record(entry);
                        }
                    }
                    Step::Done(outcome) => {
                        if let Some(tx) = kept.borrow().as_deref() {
                            super::recent::settle(tx, &outcome_words(outcome, decimals.get()));
                        }
                    }
                    Step::Failed(error) if !error.may_have_moved() => {
                        if let Some(tx) = kept.borrow().as_deref() {
                            super::recent::settle(tx, "Didn’t go through");
                        }
                    }
                    _ => {}
                }
                step.set(Some(next));
            }
        };
        // Never a config remembered from an earlier visit alone: one the
        // API answered in this page's life, and the one the review was
        // computed with; any other stops here, before a wallet is asked.
        let network = match session
            .network_to_sign(vault.chain, running.reviewed.config)
            .await
        {
            Ok(network) => network,
            Err(error) => {
                on_step(Step::Failed(error));
                return;
            }
        };
        decimals.set(network.asset_decimals);
        let key = ApprovalKey {
            chain_id: network.chain_id,
            config: network.config,
            asset: network.asset,
            vault: vault.address,
            account,
        };
        if matches!(action, Action::Deposit { .. }) {
            approval_key.set(Some(key));
        }
        let reader = Session::reader(&network);
        let signer = session.signer(network.clone());
        let ctx = Ctx {
            reader: &reader,
            signer: &signer,
            clock: &Timer,
            chain_id: network.chain_id,
            vault: vault.address,
            asset: network.asset,
            account,
            cancel: running.cancel.clone(),
            settle_now: running.settle_now.clone(),
            skip_second: running.skip_second.clone(),
            pace: Pace::LIVE,
            on_step: &on_step,
        };
        let outcome = match action {
            Action::Deposit { amount, shown } => {
                let known = session.approval(&key);
                flow::report(&on_step, flow::deposit_after(&ctx, amount, &shown, known)).await
            }
            Action::Withdraw(shares) => flow::report(&on_step, flow::withdraw(&ctx, shares)).await,
            Action::SettleClaim(id) => flow::report(&on_step, flow::settle_claim(&ctx, id)).await,
            Action::TakeCash(of) => flow::report(&on_step, flow::take_cash(&ctx, of)).await,
            Action::ClaimInstead(id) => flow::report(&on_step, flow::claim_instead(&ctx, id)).await,
            Action::CollectUnits => flow::report(&on_step, flow::collect_units(&ctx)).await,
            Action::CollectHeld => flow::report(&on_step, flow::collect_held(&ctx)).await,
            Action::SettleClosed(id) => flow::report(&on_step, flow::settle_closed(&ctx, id)).await,
            Action::Settle(id) => flow::report(&on_step, flow::settle_request(&ctx, id)).await,
        };
        let moved = matches!(
            running.step.get_cloned(),
            Some(Step::Failed(error)) if error.may_have_moved()
        );
        if outcome.is_some() || moved {
            // What changed is on the chain now; pages read it again.
            session.changed.replace_with(|n| *n + 1);
        }
    });
}

/// Without a browser nothing runs.
#[cfg(not(target_arch = "wasm32"))]
pub fn start(_vault: &VaultRef, _action: Action, running: &Running) {
    running
        .step
        .set(Some(Step::Failed(crate::tx::TxError::NotConnected)));
}

/// The vaults in a snapshot, by slug.
pub fn directory(snapshot: &shared::Snapshot) -> BTreeMap<String, Rc<VaultRef>> {
    snapshot
        .strategies
        .iter()
        .filter_map(|strategy| {
            let address = strategy.vault.address.parse().ok()?;
            Some((
                strategy.slug.clone(),
                Rc::new(VaultRef {
                    slug: strategy.slug.clone(),
                    title: strategy.title.clone(),
                    chain: strategy.vault.chain,
                    address,
                    served: matches!(strategy.vault.exit_class, shared::ExitClass::Served { .. }),
                    state: strategy.vault.state,
                    requests: strategy.vault.requests.clone(),
                }),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::flow::{Outcome, Prompt, Step};
    use alloy_primitives::B256;

    fn vault() -> VaultRef {
        VaultRef {
            slug: "base-2".to_string(),
            title: "Funding basis".to_string(),
            chain: Chain::Base,
            address: Address::repeat_byte(0x5f),
            served: false,
            state: VaultState::Open,
            requests: Vec::new(),
        }
    }

    #[test]
    fn this_browser_keeps_a_deposit_withdrawal_or_claim_never_an_approval_or_the_second_step() {
        let keep = |prompt: Prompt| {
            recent_of(
                &prompt,
                &vault(),
                Address::repeat_byte(0x7e),
                84532,
                6,
                B256::repeat_byte(0xab),
                1.0,
            )
        };
        assert!(
            keep(Prompt::Approve {
                amount: U256::from(1u8)
            })
            .is_none()
        );
        assert!(keep(Prompt::Finish).is_none());
        assert!(keep(Prompt::Statement).is_none());
        let deposit = keep(Prompt::Deposit {
            amount: U256::from(250_000_000u64),
        })
        .unwrap();
        assert_eq!(deposit.kind, super::super::recent::Kind::Deposit);
        assert_eq!(deposit.amount, "$250.00");
        assert_eq!(
            deposit.tx_url.as_deref(),
            Some(format!("https://sepolia.basescan.org/tx/0x{}", "ab".repeat(32)).as_str())
        );
        let withdrawal = keep(Prompt::Withdraw {
            shares: U256::from(12_500_000_000_000_000_000u128),
        })
        .unwrap();
        assert_eq!(
            withdrawal.what(),
            "Withdrawal of 12.5 shares from Funding basis"
        );
        assert_eq!(withdrawal.status(), "Pending, priced at the next valuation");
        let claim = keep(Prompt::SettleClaim {
            claim: U256::from(3u8),
        })
        .unwrap();
        assert_eq!(claim.kind, super::super::recent::Kind::Claim);
    }

    #[test]
    fn an_approval_seen_mined_is_kept_for_the_next_deposit_until_a_deposit_or_another_approval_is_sent()
     {
        use crate::tx::TxError;
        use crate::tx::flow::Approval;
        let amount = U256::from(1_000_000_000u64);
        let approved = Approval { amount, block: 101 };
        let stands = |then: TxError| {
            Step::Failed(TxError::ApprovalStands {
                amount,
                block: 101,
                sent: false,
                then: Box::new(then),
            })
        };
        // Kept, with its block, once a deposit stops after its approval was
        // mined.
        assert_eq!(
            approval_after(&stands(TxError::Rejected), None),
            Some(approved)
        );
        // Kept through steps that change nothing about it, a deposit
        // stopped before it got that far, a refusal from a node that hasn't
        // caught up with it, and another approval asked for but not sent.
        for step in [
            Step::Checking,
            Step::Signing(Prompt::Statement),
            Step::Signing(Prompt::Approve { amount }),
            Step::Failed(TxError::Rejected),
            Step::Failed(TxError::ListChanged { now: String::new() }),
            Step::Failed(TxError::WouldRevert {
                reason: crate::tx::Revert::Unknown,
            }),
            stands(TxError::NotCaughtUp),
        ] {
            assert_eq!(
                approval_after(&step, Some(approved)),
                Some(approved),
                "{step:?}"
            );
        }
        // Let go once the deposit that uses it is sent, or once another
        // approval is sent.
        for prompt in [Prompt::Deposit { amount }, Prompt::Approve { amount }] {
            let step = Step::Sent {
                prompt,
                hash: B256::ZERO,
                unseen: false,
            };
            assert_eq!(approval_after(&step, Some(approved)), None, "{step:?}");
        }
    }

    #[test]
    fn a_withdrawal_paid_from_cash_is_recorded_as_paid_with_its_dollars() {
        let paid = Outcome::Paid {
            shares: U256::from(1u8),
            assets: U256::from(251_200_000u64),
            at: None,
            hash: B256::ZERO,
        };
        assert_eq!(outcome_words(&paid, 6), "Paid $251.20");
        assert_eq!(
            outcome_words(&Outcome::SettledBefore(Box::new(paid)), 6),
            "Paid $251.20"
        );
        assert_eq!(
            outcome_words(
                &Outcome::Booked {
                    claim: U256::from(4u8),
                    shares: U256::from(1u8),
                    hash: B256::ZERO
                },
                6
            ),
            "Booked as claim 4"
        );
    }
}
