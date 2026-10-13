//! Running a Solana flow from a page: the vault it acts on (from the
//! store, for its address only; money is read from the cluster), the steps
//! it reaches as a signal, and a cancel the session also pulls when the
//! account changes. In the browser it reads through the cluster's public
//! endpoint and signs with the connected Solana wallet.

use std::rc::Rc;

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

/// The Solana vault at `address`, from the store.
pub fn vault_at(address: &Pubkey) -> Option<SolVault> {
    crate::store::get()
        .all()
        .iter()
        .filter_map(|agent| vault_of(&agent.slug))
        .find(|vault| vault.address == *address)
}

/// What this browser keeps of a send, from the step that sent it: only a
/// deposit, a withdrawal or a claim's own transaction.
pub fn recent_of(
    prompt: &super::flow::Prompt,
    vault: &SolVault,
    account: &Pubkey,
    network: &SolNetwork,
    signature: &super::port::Sig,
    at_ms: f64,
) -> Option<crate::wallet::recent::Entry> {
    use super::flow::Prompt;
    use crate::money::{Rounding, format as money};
    use crate::wallet::recent::{Entry, Family, Kind};
    use alloy_primitives::U256;
    let (kind, amount) = match prompt {
        Prompt::Deposit { amount } => (
            Kind::Deposit,
            money::dollars(U256::from(*amount), network.asset_decimals, Rounding::Down),
        ),
        Prompt::Withdraw { shares } => (
            Kind::Withdrawal,
            format!(
                "{} shares",
                money::shares(U256::from(*shares), Rounding::Down)
            ),
        ),
        Prompt::SettleClaim { claim } => (Kind::Claim, format!("Settling claim {claim}")),
        Prompt::TakeCash => (Kind::Claim, "Taking the cash share".to_string()),
        Prompt::ClaimInstead => (Kind::Claim, "Making a claim instead".to_string()),
        _ => return None,
    };
    let tx = signature.to_string();
    Some(Entry {
        tx_url: Some(network.tx_url(&tx)),
        tx,
        family: Family::Solana,
        account: account.to_string(),
        slug: vault.slug.clone(),
        title: vault.title.clone(),
        kind,
        amount,
        at_ms,
        outcome: None,
    })
}

/// How a Solana flow ended, in a few words for this browser's record.
pub fn outcome_words(outcome: &super::flow::Outcome, decimals: u8) -> String {
    use super::flow::Outcome;
    use crate::money::{Rounding, format as money};
    use alloy_primitives::U256;
    use chain::solana_ix::accounts::ClaimState;
    let dollars = |value: u64| money::dollars(U256::from(value), decimals, Rounding::Down);
    match outcome {
        Outcome::Deposited { shares, .. } => format!(
            "Bought {} shares",
            money::shares(U256::from(*shares), Rounding::Down)
        ),
        Outcome::Refunded { assets, .. } => format!("Sent back: {}", dollars(*assets)),
        Outcome::Paid { assets, .. } => format!("Paid {}", dollars(*assets)),
        Outcome::Booked { claim, .. } => format!("Booked as claim {claim}"),
        Outcome::Claim {
            state: ClaimState::Paid,
            paid,
            ..
        } => format!("Paid {}", dollars(*paid)),
        Outcome::Claim { .. } => "Still booked".to_string(),
        Outcome::TookCash { assets, .. } => format!("Paid {} in cash", dollars(*assets)),
        Outcome::ClaimMade { claim, .. } => format!("Made claim {claim}"),
        Outcome::Collected { assets, .. } => format!("Collected {}", dollars(*assets)),
        Outcome::Changed { .. } => "Done".to_string(),
    }
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

/// A flow in progress: its latest step, its cancel, and the cluster its
/// review was computed with.
#[derive(Clone)]
pub struct SolRunning {
    pub step: Mutable<Option<Step>>,
    pub cancel: Cancel,
    /// The cluster the review was computed with (its amounts are scaled
    /// with this cluster's decimals): the flow signs with this config or
    /// not at all.
    pub reviewed: Rc<SolNetwork>,
}

impl SolRunning {
    pub fn new(reviewed: Rc<SolNetwork>) -> Self {
        Self {
            step: Mutable::new(None),
            cancel: Cancel::default(),
            reviewed,
        }
    }
}

/// Where a card shows the flow it started, kept for the page's life by the
/// connected account, the vault and what the card does: a card drawn again
/// (its page reading again, the cluster's settings changing) shows the flow
/// under way rather than offering to start it a second time.
pub fn card_slot(
    account: Option<Pubkey>,
    vault: &Pubkey,
    what: &str,
) -> Mutable<Option<SolRunning>> {
    thread_local! {
        static CARDS: std::cell::RefCell<std::collections::BTreeMap<String, Mutable<Option<SolRunning>>>> =
            const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    }
    let key = format!("{account:?}|{vault}|{what}");
    CARDS.with(|cards| {
        cards
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Mutable::new(None))
            .clone()
    })
}

/// What a card shows for the flow its slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardShows {
    /// No flow: the action.
    Action,
    /// A flow under way, or one whose money may still move: its status
    /// only, never the action again (the latter with a way to dismiss it).
    Status,
    /// A flow that ended and moves nothing more: its outcome, with a way to
    /// dismiss it, and the action offered again beside it.
    StatusAndAction,
}

/// What a card shows for a flow at `step`.
pub fn card_shows(step: Option<&Step>) -> CardShows {
    match step {
        Some(Step::Done(_)) => CardShows::StatusAndAction,
        Some(Step::Failed(error)) if !error.may_have_moved() => CardShows::StatusAndAction,
        _ => CardShows::Status,
    }
}

/// What a card shows for the flow `slot` holds, if any.
pub fn card_shows_slot(slot: &Mutable<Option<SolRunning>>) -> CardShows {
    match slot.get_cloned() {
        None => CardShows::Action,
        Some(flow) => card_shows(flow.step.lock_ref().as_ref()),
    }
}

/// One card as it stands: the account connected, the slot of its flow for
/// that account, and what it shows.
#[derive(Clone)]
pub struct Card {
    pub account: Option<Pubkey>,
    pub slot: Mutable<Option<SolRunning>>,
    pub shows: CardShows,
}

/// The card for `what` on `vault`, as it changes: with the account
/// connected (its slot is that account's: a flow is kept under the account
/// it starts for), and with its flow's steps.
pub fn card_signal(
    vault: Pubkey,
    what: String,
) -> impl futures_signals::signal::Signal<Item = Card> + use<> {
    use futures_signals::signal::{SignalExt, always};
    crate::wallet::solana::get()
        .connection
        .signal_ref(|connection| connection.account())
        .dedupe()
        .switch(move |account| {
            let slot = card_slot(account, &vault, &what);
            let held = slot.clone();
            slot.signal_cloned()
                .switch(|current| match current {
                    None => always(CardShows::Action).boxed_local(),
                    Some(flow) => flow
                        .step
                        .signal_ref(|step| card_shows(step.as_ref()))
                        .boxed_local(),
                })
                .dedupe()
                .map(move |shows| Card {
                    account,
                    slot: held.clone(),
                    shows,
                })
        })
}

/// Start `action` from a card: only for the account the card was drawn for
/// (still the one connected), and never while its flow is under way or its
/// money may still move. Whether it started.
pub fn start_from(card: &Card, vault: Pubkey, action: SolAction, reviewed: Rc<SolNetwork>) -> bool {
    if card.account.is_none() || crate::wallet::solana::get().account() != card.account {
        return false;
    }
    if card_shows_slot(&card.slot) == CardShows::Status {
        return false;
    }
    let flow = SolRunning::new(reviewed);
    start(vault, action, &flow);
    card.slot.set(Some(flow));
    true
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
        let Some(account) = session.account() else {
            step.set(Some(Step::Failed(SolError::NotConnected)));
            return;
        };
        // Never a config remembered from an earlier visit alone: one the
        // API answered in this page's life, and the one the review was
        // computed with; any other stops here, before a wallet is asked.
        let network = match session.network_to_sign(running.reviewed.config).await {
            Ok(network) => network,
            Err(error) => {
                step.set(Some(Step::Failed(error)));
                return;
            }
        };
        // This browser keeps the send once it is on its way, and how it
        // ended once the flow says.
        let on_step = {
            let network = network.clone();
            let kept: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
            let shown = vault_at(&vault);
            move |next: Step| {
                match &next {
                    Step::Sent { prompt, signature } if kept.borrow().is_none() => {
                        if let Some(entry) = shown.as_ref().and_then(|shown| {
                            recent_of(
                                prompt,
                                shown,
                                &account,
                                &network,
                                signature,
                                js_sys::Date::now(),
                            )
                        }) {
                            *kept.borrow_mut() = Some(entry.tx.clone());
                            crate::wallet::recent::record(entry);
                        }
                    }
                    Step::Done(outcome) => {
                        if let Some(tx) = kept.borrow().as_deref() {
                            crate::wallet::recent::settle(
                                tx,
                                &outcome_words(outcome, network.asset_decimals),
                            );
                        }
                    }
                    Step::Failed(error) if !error.may_have_moved() => {
                        if let Some(tx) = kept.borrow().as_deref() {
                            crate::wallet::recent::settle(tx, "Didn’t go through");
                        }
                    }
                    _ => {}
                }
                step.set(Some(next));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::solana::SolError;
    use crate::tx::solana::flow::Outcome;
    use crate::tx::solana::port::Sig;

    fn cluster() -> Rc<SolNetwork> {
        Rc::new(SolNetwork {
            cluster: "devnet".to_string(),
            label: "Solana devnet".to_string(),
            rpc: "https://api.devnet.solana.com".to_string(),
            mint: Pubkey::new_from_array([7; 32]),
            asset_symbol: "USDC".to_string(),
            asset_decimals: 6,
            dev_account: None,
            config: crate::wallet::config_cache::ConfigId::default(),
        })
    }

    #[test]
    fn a_card_starts_only_for_the_account_it_was_drawn_for_and_never_over_a_flow_under_way() {
        let session = crate::wallet::solana::get();
        session
            .connect_phrase("test test test test test test test test test test test junk")
            .unwrap();
        let connected = session.account();
        let vault = Pubkey::new_from_array([4; 32]);
        // Drawn for another account (the wallet moved before the card was
        // drawn again): nothing starts, and no slot is filled.
        let other = Pubkey::new_from_array([6; 32]);
        let stale = Card {
            account: Some(other),
            slot: card_slot(Some(other), &vault, "Collect"),
            shows: CardShows::Action,
        };
        assert!(!start_from(
            &stale,
            vault,
            SolAction::CollectUnits,
            cluster()
        ));
        assert!(stale.slot.lock_ref().is_none());
        // Drawn for the account connected: it starts, into that account's
        // slot.
        let card = Card {
            account: connected,
            slot: card_slot(connected, &vault, "Collect"),
            shows: CardShows::Action,
        };
        assert!(start_from(&card, vault, SolAction::CollectUnits, cluster()));
        assert!(card_slot(connected, &vault, "Collect").lock_ref().is_some());
        // Under way: a second start is refused.
        let flow = card.slot.get_cloned().unwrap();
        flow.step.set(Some(Step::Checking));
        assert_eq!(card_shows_slot(&card.slot), CardShows::Status);
        assert!(!start_from(
            &card,
            vault,
            SolAction::CollectUnits,
            cluster()
        ));
        // Its money may still move: refused until it is dismissed.
        flow.step.set(Some(Step::Failed(SolError::Pending {
            signature: Sig([1; 64]),
            why: "the cluster stopped answering".to_string(),
        })));
        assert_eq!(card_shows_slot(&card.slot), CardShows::Status);
        assert!(!start_from(
            &card,
            vault,
            SolAction::CollectUnits,
            cluster()
        ));
        card.slot.set(None);
        assert_eq!(card_shows_slot(&card.slot), CardShows::Action);
        // Ended: offered again beside its outcome.
        assert!(start_from(&card, vault, SolAction::CollectUnits, cluster()));
        let flow = card.slot.get_cloned().unwrap();
        flow.step.set(Some(Step::Done(Outcome::Changed {
            signature: Sig([2; 64]),
        })));
        assert_eq!(card_shows_slot(&card.slot), CardShows::StatusAndAction);
        assert!(start_from(&card, vault, SolAction::CollectUnits, cluster()));
        session.disconnect();
    }
}
