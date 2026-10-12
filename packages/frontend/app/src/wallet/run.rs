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
#[cfg(target_arch = "wasm32")]
use crate::tx::port::Clock;

/// What a flow does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Deposit(U256),
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
            Self::Deposit(_) | Self::Withdraw(_) => EXIT.to_string(),
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
    pub step: Mutable<Option<Step>>,
    pub cancel: Cancel,
    pub settle_now: Nudge,
    /// When its countdown started on the reader's clock, for the seconds
    /// left; set once per request.
    pub requested_local: Rc<Cell<Option<(f64, u64)>>>,
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
    pub fn new(action: Action) -> Self {
        thread_local! {
            static NEXT: Cell<u64> = const { Cell::new(1) };
        }
        Self {
            id: NEXT.with(|next| next.replace(next.get() + 1)),
            action,
            step: Mutable::new(None),
            cancel: Cancel::new(),
            settle_now: Nudge::new(),
            requested_local: Rc::new(Cell::new(None)),
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

/// The browser's clock.
#[cfg(target_arch = "wasm32")]
pub struct Timer;

#[cfg(target_arch = "wasm32")]
impl Clock for Timer {
    async fn sleep(&self, millis: u32) {
        gloo_timers::future::TimeoutFuture::new(millis).await;
    }
}

/// Start `action` on `vault`, reporting into `running`.
#[cfg(target_arch = "wasm32")]
pub fn start(vault: &VaultRef, action: Action, running: &Running) {
    use crate::tx::flow::{self, Ctx, Pace};
    use crate::wallet::session::Session;

    let session = crate::wallet::get();
    let vault = vault.clone();
    let running = running.clone();
    running.step.set(Some(Step::Checking));
    session.track(running.cancel.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let step = running.step.clone();
        let on_step = move |next: Step| step.set(Some(next));
        let Some(account) = session.account() else {
            on_step(Step::Failed(crate::tx::TxError::NotConnected));
            return;
        };
        let Some(network) = session.network(vault.chain).await else {
            on_step(Step::Failed(crate::tx::TxError::Network(
                "this vault's chain isn't reachable from this app".to_string(),
            )));
            return;
        };
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
            pace: Pace::LIVE,
            on_step: &on_step,
        };
        let outcome = match action {
            Action::Deposit(amount) => flow::report(&on_step, flow::deposit(&ctx, amount)).await,
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
