//! Running a Solana flow from a page: the vault it acts on (from the
//! store, for its address only; money is read from the cluster), the steps
//! it reaches as a signal, and a cancel the session also pulls when the
//! account changes. In the browser it reads through the cluster's public
//! endpoint and signs with the connected Solana wallet.

use chain::solana_ix::Pubkey;
use futures_signals::signal::Mutable;
use shared::Chain;

use super::author::AuthorAction;
use super::flow::{Cluster, Fallback, HeldAt, Step};
use crate::tx::flow::Cancel;
use crate::wallet::solana::network::SolNetwork;

/// What a flow does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolAction {
    /// Deposit, stating for the restricted list whose hash was shown.
    Deposit {
        amount: u64,
        shown: [u8; 32],
    },
    Withdraw(u128),
    SettleClaim(u64),
    TakeCash(Fallback),
    ClaimInstead(u64),
    CollectUnits,
    CollectHeld(HeldAt),
    SettleClosed(u64),
    Author(AuthorAction),
    /// Withdraw from the tank with the executor's signed usage total, read
    /// from the API when the flow starts.
    WithdrawTankSigned(u64),
}

/// A Solana vault as a page acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolVault {
    pub slug: String,
    pub title: String,
    pub address: Pubkey,
}

/// The vault of `slug`, if it lives on Solana: from the store.
pub fn vault_of(slug: &str) -> Option<SolVault> {
    let agent = crate::store::get().agent(slug)?;
    let strategy = agent.strategy.get_cloned();
    if strategy.vault.chain != Chain::Solana {
        return None;
    }
    Some(SolVault {
        slug: slug.to_string(),
        title: strategy.title.clone(),
        address: strategy.vault.address.parse().ok()?,
    })
}

/// Whether `slug`'s vault lives on Solana: as the store knows it, or before
/// the store has it, as its slug names its chain (`solana-4`).
pub fn is_solana(slug: &str) -> bool {
    match crate::store::get().agent(slug) {
        Some(agent) => agent.strategy.lock_ref().vault.chain == Chain::Solana,
        None => slug.starts_with("solana-"),
    }
}

/// The cluster a flow acts on.
pub fn cluster(network: &SolNetwork) -> Cluster {
    Cluster {
        name: network.cluster.clone(),
        chain: network.wallet_chain(),
        mint: network.mint,
        local_endpoint: network.rpc.starts_with("http://127.0.0.1:")
            || network.rpc.starts_with("http://localhost:"),
    }
}

/// A flow in progress: its latest step, and its cancel.
#[derive(Clone, Default)]
pub struct SolRunning {
    pub step: Mutable<Option<Step>>,
    pub cancel: Cancel,
}

impl SolRunning {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Start `action` on the vault at `vault`, reporting into `running`.
#[cfg(target_arch = "wasm32")]
pub fn start(vault: Pubkey, action: SolAction, running: &SolRunning) {
    use super::error::SolError;
    use super::flow::{self, Ctx};
    use crate::tx::flow::Pace;
    use crate::wallet::run::Timer;
    use crate::wallet::solana::session::SolSession;

    let session = crate::wallet::solana::get();
    let running = running.clone();
    running.step.set(Some(Step::Checking));
    session.track(running.cancel.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let step = running.step.clone();
        let on_step = move |next: Step| step.set(Some(next));
        let Some(account) = session.account() else {
            on_step(Step::Failed(SolError::NotConnected));
            return;
        };
        let Some(network) = session.load_network().await else {
            on_step(Step::Failed(SolError::Network(
                "the Solana cluster isn't reachable from this app".to_string(),
            )));
            return;
        };
        let cluster = cluster(&network);
        let reader = SolSession::reader(&network);
        let signer = session.signer();
        let ctx = Ctx {
            reader: &reader,
            signer: &signer,
            clock: &Timer,
            cluster: &cluster,
            vault,
            account,
            cancel: running.cancel.clone(),
            pace: Pace::LIVE,
            on_step: &on_step,
        };
        let outcome = match action {
            SolAction::Deposit { amount, shown } => {
                flow::report(&on_step, flow::deposit(&ctx, amount, shown)).await
            }
            SolAction::Withdraw(shares) => {
                flow::report(&on_step, flow::withdraw(&ctx, shares)).await
            }
            SolAction::SettleClaim(id) => {
                flow::report(&on_step, flow::settle_claim(&ctx, id)).await
            }
            SolAction::TakeCash(of) => flow::report(&on_step, flow::take_cash(&ctx, of)).await,
            SolAction::ClaimInstead(id) => {
                flow::report(&on_step, flow::claim_instead(&ctx, id)).await
            }
            SolAction::CollectUnits => flow::report(&on_step, flow::collect_units(&ctx)).await,
            SolAction::CollectHeld(at) => {
                flow::report(&on_step, flow::collect_held(&ctx, at)).await
            }
            SolAction::SettleClosed(id) => {
                flow::report(&on_step, flow::settle_closed(&ctx, id)).await
            }
            SolAction::Author(action) => {
                flow::report(&on_step, super::author::run(&ctx, action)).await
            }
            SolAction::WithdrawTankSigned(assets) => {
                let signed = usage(&vault).await;
                match signed {
                    Some(signed) => {
                        flow::report(
                            &on_step,
                            super::author::run(
                                &ctx,
                                AuthorAction::WithdrawTank {
                                    assets,
                                    signed: Some(signed),
                                },
                            ),
                        )
                        .await
                    }
                    None => {
                        on_step(Step::Failed(SolError::Network(
                            "the executor's signed usage total isn't available".to_string(),
                        )));
                        None
                    }
                }
            }
        };
        if outcome.is_some() {
            session.changed.replace_with(|n| *n + 1);
        }
    });
}

/// The executor's signed usage total for `vault`, from the API.
#[cfg(target_arch = "wasm32")]
async fn usage(vault: &Pubkey) -> Option<chain::solana_ix::vault::SignedUsage> {
    let reply = gloo_net::http::Request::get(&format!("{}/solana/usage/{vault}", crate::api::BASE))
        .send()
        .await
        .ok()?;
    if !reply.ok() {
        return None;
    }
    let value: serde_json::Value = reply.json().await.ok()?;
    super::author::signed_usage(&value, vault)
}

/// Without a browser nothing runs.
#[cfg(not(target_arch = "wasm32"))]
pub fn start(_vault: Pubkey, _action: SolAction, running: &SolRunning) {
    running
        .step
        .set(Some(Step::Failed(super::error::SolError::NotConnected)));
}
