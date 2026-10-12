//! Saying how old the figures are the moment they fall behind (States-System,
//! "Data freshness"): a 36px bar under the header once they are more than a
//! minute behind the chain, and the "can't reach the indexer" card at the top
//! of the page after two minutes without an answer. Both clear themselves.

use futures_signals::map_ref;
use futures_signals::signal::{Signal, SignalExt};
use ui::icons::Icon;

use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::format;
use crate::freshness::Freshness;
use crate::route::DOCS;
use crate::store;
use crate::view::{View, el, text};
use crate::web::app_link;

/// Freshness, as it changes.
pub fn signal() -> impl Signal<Item = Freshness> {
    let store = store::get();
    map_ref! {
        let now = crate::clock::signal(),
        let started = store.sync.started.signal(),
        let last_ok = store.sync.last_ok.signal(),
        let as_of = store.as_of.signal() =>
        Freshness::of(*now, *started, *last_ok, *as_of)
    }
    .dedupe()
}

/// Freshness as it is now (the pre-render's view: loading, so nothing shows).
pub fn now() -> Freshness {
    let store = store::get();
    Freshness::of(
        crate::clock::now().get(),
        store.sync.started.get(),
        store.sync.last_ok.get(),
        store.as_of.get(),
    )
}

/// The slim bar under the header.
pub fn bar() -> View {
    let failing = store::get().sync.failing.clone();
    el("div")
        .class("sh-contents")
        .child_signal(
            behind_bar(now(), false),
            map_ref! {
                let freshness = signal(),
                let failing = failing.signal() => behind_bar(*freshness, *failing)
            },
        )
        .into()
}

fn behind_bar(freshness: Freshness, failing: bool) -> Option<View> {
    let Freshness::Behind { seconds } = freshness else {
        return None;
    };
    Some(
        el("div")
            .class("sh-stalebar")
            .attr("role", "status")
            .child(icon(Icon::Clock, 16))
            .text(format!(
                "Figures are {} behind the chain.{}",
                format::ago(seconds),
                if failing { " Reconnecting…" } else { "" }
            ))
            .into(),
    )
}

/// The card at the top of the main column.
pub fn card() -> View {
    el("div")
        .class("sh-contents")
        .child_signal(unreachable_card(now()), signal().map(unreachable_card))
        .into()
}

fn unreachable_card(freshness: Freshness) -> Option<View> {
    let Freshness::Unreachable { since } = freshness else {
        return None;
    };
    let lead = match since {
        Some(at) => format!(
            "We can’t reach the indexer. Numbers below are from {}.",
            format::time_short(at)
        ),
        None => "We can’t reach the indexer.".to_string(),
    };
    Some(
        notice(
            NoticeTone::Caution,
            View::Many(vec![
                el("p")
                    .child(el("strong").text(lead))
                    .text(" Nothing about your money has changed. Deposits and withdrawals are priced by the vault contract, not by these numbers.")
                    .into(),
                el("span")
                    .class("sh-notice__actions")
                    .child(
                        el("a")
                            .attr("href", format!("{DOCS}verify/"))
                            .text("Check on-chain")
                            .child(icon(Icon::External, 14)),
                    )
                    .child(
                        app_link("/status")
                            .text("Status page")
                            .child(icon(Icon::ArrowRight, 16)),
                    )
                    .into(),
            ]),
        )
        .class("sh-freshness")
        .attr("role", "status")
        .into(),
    )
}

/// "As of 14:02 UTC" for a figure from a vault the indexer couldn't read
/// whole: stale money is never shown as live.
pub fn as_of_words(at: time::PrimitiveDateTime) -> View {
    text(format!("As of {}", format::time_short(at)))
}
