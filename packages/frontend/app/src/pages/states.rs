//! The states today's data can't show, each drawn by the very renderer the
//! app uses, from synthetic values (dev builds only, at `/_states`): every
//! agent banner, the data-freshness bar and card, the wrong-network bar,
//! the toasts, the agent page's skeleton and every publishing job's state.

use futures_signals::signal::SignalExt;
use shared::{Area, Failure, Job, JobState, Strategy};
use time::PrimitiveDateTime;

use super::Page;
use super::agent::facts::Banner;
use crate::atoms::button::{Button, Kind, on_click};
use crate::atoms::toast::{self, ToastKind};
use crate::freshness::Freshness;
use crate::route::AgentTab;
use crate::store;
use crate::tx::author::read::PaymentRecord;
use crate::view::{El, View, el};

fn section(id: &'static str, title: &'static str, body: impl IntoIterator<Item = View>) -> El {
    el("section")
        .class("at-section")
        .attr("aria-labelledby", id)
        .child(el("h2").attr("id", id).class("at-h2").text(title))
        .children(body)
}

fn labelled(label: impl Into<String>, body: impl Into<View>) -> View {
    el("div")
        .class("st-state")
        .child(el("p").class("at-row__label").text(label.into()))
        .child(body.into())
        .into()
}

fn at(day: u16, hour: u8) -> PrimitiveDateTime {
    PrimitiveDateTime::new(
        time::Date::from_ordinal_date(2026, day).expect("a day of the year"),
        time::Time::from_hms(hour, 0, 0).expect("an hour"),
    )
}

/// Every banner the agent page can carry, most serious first, drawn over
/// `strategy` (an agent from the snapshot; the banners' own facts are
/// synthetic).
pub fn banners(strategy: &Strategy) -> View {
    let every = [
        (
            "Killed",
            Banner::Killed {
                at: Some(at(280, 14)),
                reason: "The executor stopped answering and the positions were closed.".into(),
                outcome: Some("Positions closed; the cash is home.".into()),
                reference: Some("incident-1".into()),
            },
        ),
        (
            "Frozen",
            Banner::Frozen {
                at: Some(at(281, 9)),
            },
        ),
        ("Tank empty", Banner::TankEmpty),
        (
            "Serious concerns",
            Banner::Serious {
                areas: vec![Area::ALL[0], Area::ALL[4]],
                unlisted: vec![],
                refused: false,
            },
        ),
        (
            "Retiring",
            Banner::Retiring {
                since: Some(at(268, 18)),
                ends: Some(at(275, 18)),
            },
        ),
        ("Tank low", Banner::TankLow { days: 5 }),
        ("New", Banner::New { days: 12 }),
        ("Sealed", Banner::Sealed),
        ("Served exit", Banner::Served),
        ("Activating", Banner::Activating),
        ("Closed", Banner::Closed),
        ("Cancelled", Banner::Cancelled),
    ];
    View::Many(
        every
            .into_iter()
            .map(|(label, banner)| {
                labelled(
                    label,
                    el("div")
                        .class("ag-banners")
                        .child(super::agent::banner(&banner, strategy, 2026)),
                )
            })
            .collect(),
    )
}

/// The banners, over the first agent the store holds once it has one.
fn banners_from_store() -> View {
    let store = store::get();
    el("div")
        .class("sh-contents")
        .child_signal(
            Some(
                el("p")
                    .class("sh-muted")
                    .text("Waiting for an agent from the API…")
                    .into(),
            ),
            store.membership.signal().map(|_| {
                let first = store::get()
                    .agents
                    .lock_ref()
                    .values()
                    .next()
                    .map(|agent| agent.strategy.get_cloned());
                first.map(|strategy| banners(&strategy))
            }),
        )
        .into()
}

fn freshness() -> View {
    use crate::components::freshness::{behind_bar, unreachable_card};
    View::Many(vec![
        labelled(
            "Figures behind the chain, reconnecting",
            behind_bar(Freshness::Behind { seconds: 180.0 }, true).unwrap_or(View::Empty),
        ),
        labelled(
            "Can't reach the indexer, with figures from earlier",
            unreachable_card(
                Freshness::Unreachable {
                    since: Some(at(285, 14)),
                },
                true,
            )
            .unwrap_or(View::Empty),
        ),
        labelled(
            "Can't reach the indexer, nothing arrived yet (offline)",
            unreachable_card(Freshness::Unreachable { since: None }, true).unwrap_or(View::Empty),
        ),
    ])
}

fn wallet() -> View {
    labelled(
        "Wrong network: the wallet is on Base Sepolia, the vault on another chain",
        crate::components::wallet::wrong_network(84_532, 31_337, "Funding basis takes deposits"),
    )
}

fn toasts() -> View {
    labelled(
        "The board's five toasts (errors stay until closed)",
        on_click(
            Button::new(Kind::Secondary, "Show the toasts").build(),
            || {
                toast::push(
                    ToastKind::Info,
                    "Deposit requested",
                    "Priced at 14:32:40 UTC.",
                );
                toast::push(ToastKind::Success, "Paid", "$2,277.12 is in your wallet.");
                toast::push(
                    ToastKind::Neutral,
                    "Rejected in wallet",
                    "Nothing was sent.",
                );
                toast::push(
                    ToastKind::Error,
                    "Transaction failed",
                    "The network didn't accept it. Nothing moved.",
                );
            },
        ),
    )
}

fn job(state: JobState) -> Job {
    Job {
        id: "8801".to_string(),
        chain: shared::Chain::Base,
        strategy_id: Some(7),
        title: "Funding basis".to_string(),
        author: "0x2222222222222222222222222222222222222222".to_string(),
        state,
        submitted: at(281, 12),
        duration_seconds: 0,
        source_hash: String::new(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: Some("base-7".to_string()),
    }
}

/// Every state a publishing job can be in, each as its page draws it.
pub fn jobs() -> View {
    use crate::pages::publish::job::job_view;
    let mut every: Vec<(&str, JobState, Option<PaymentRecord>)> = vec![
        ("Awaiting payment", JobState::AwaitingPayment, None),
        (
            "Awaiting payment, paid and waiting for confirmations",
            JobState::AwaitingPayment,
            Some(PaymentRecord::ByUploader {
                strategy_id: alloy_primitives::U256::from(7u8),
            }),
        ),
        ("Queued", JobState::Queued { position: 3 }, None),
        ("Reviewing", JobState::Reviewing, None),
        ("Building", JobState::Building, None),
        ("Ready", JobState::Ready, None),
        ("Activated", JobState::Activated, None),
        ("Refused", JobState::Refused, None),
    ];
    for (label, outcome) in [
        ("Failed: manifest", Failure::Manifest),
        ("Failed: too large", Failure::TooLarge),
        ("Failed: build", Failure::Build),
        ("Failed: timed out", Failure::TimedOut),
        ("Failed: our side", Failure::Service),
    ] {
        every.push((label, JobState::Failed { outcome }, None));
    }
    View::Many(
        every
            .into_iter()
            .map(|(label, state, paid)| labelled(label, job_view(&job(state), paid)))
            .collect(),
    )
}

pub fn page() -> Page {
    Page {
        top: None,
        body: el("div")
            .class("at")
            .child(el("h1").class("at-title").text("States"))
            .child(el("p").class("sh-muted").text(
                "Every state today's data can't show, drawn by the app's own renderers from synthetic values. Dev builds only.",
            ))
            .child(section("st-banners", "Agent banners", [banners_from_store()]))
            .child(section("st-freshness", "Data freshness", [freshness()]))
            .child(section("st-wallet", "Wallet", [wallet()]))
            .child(section("st-toasts", "Toasts", [toasts()]))
            .child(section(
                "st-loading",
                "Loading",
                [labelled(
                    "The agent page before its data arrives",
                    super::agent::loading("base-1", AgentTab::Overview),
                )],
            ))
            .child(section("st-jobs", "Publishing job", [jobs()]))
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Strategy {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../../tests/snapshot.json"))
                .expect("the fixture decodes");
        snapshot
            .strategies
            .into_iter()
            .find(|strategy| strategy.review.is_some())
            .expect("a reviewed agent")
    }

    #[test]
    fn every_agent_banner_is_drawn_from_synthetic_facts() {
        let html = banners(&fixture()).html();
        for words in [
            "Killed",
            "Frozen",
            "tank is empty",
            "serious concerns",
            "retiring",
            "Tank",
            "Sealed",
        ] {
            assert!(
                html.to_lowercase().contains(&words.to_lowercase()),
                "{words}: {html}"
            );
        }
        assert!(html.matches("sh-notice").count() >= 12, "{html}");
    }

    #[test]
    fn every_publishing_job_state_is_drawn() {
        let html = jobs().html();
        for words in [
            "Awaiting payment",
            "Queued",
            "Reviewing",
            "Building",
            "Ready",
            "Activated",
            "Refused",
            "Build failed",
        ] {
            assert!(html.contains(words), "{words}");
        }
    }

    #[test]
    fn the_freshness_bar_and_both_indexer_cards_are_drawn() {
        let html = freshness().html();
        assert!(html.contains("behind the chain. Reconnecting"), "{html}");
        assert_eq!(
            html.matches("We can’t reach the indexer.").count(),
            2,
            "{html}"
        );
        assert!(html.contains("Numbers below are from"), "{html}");
    }
}
