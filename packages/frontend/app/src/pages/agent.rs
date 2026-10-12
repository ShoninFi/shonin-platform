//! `/agents/:id` and its tabs (review, verify, activity). The full page
//! arrives with the next update; until then the route shows the agent's name,
//! its facts and the loading skeleton the States board draws, so every link
//! to an agent lands somewhere real.

use futures_signals::signal::SignalExt;

use crate::atoms::review_chip;
use crate::atoms::skeleton::{skeleton, skeleton_block};
use crate::route::AgentTab;
use crate::store;
use crate::view::{El, View, el};
use crate::web::app_link;

use super::Page;

fn stat_skeleton(label: &'static str) -> El {
    el("div")
        .class("ag-stat")
        .child(el("p").class("sh-label").text(label))
        .child(skeleton("110", 28))
        .child(skeleton("72", 12))
}

fn tabs(slug: &str, current: AgentTab) -> El {
    el("nav")
        .class("sh-tabs")
        .class("ag-tabs")
        .attr("aria-label", "Agent")
        .children(AgentTab::ALL.iter().map(|tab| {
            let href = match tab.segment() {
                Some(segment) => format!("/agents/{slug}/{segment}"),
                None => format!("/agents/{slug}"),
            };
            app_link(href)
                .class("sh-tab")
                .attr_if(*tab == current, "aria-current", "page")
                .text(tab.label())
        }))
}

fn loading() -> El {
    el("div")
        .class("ag-loading")
        .attr("aria-busy", "true")
        .attr("aria-label", "Loading agent")
        .child(
            el("div")
                .class("ag-main")
                .child(
                    el("div")
                        .class("ag-stats")
                        .child(stat_skeleton("Share price"))
                        .child(stat_skeleton("30 days"))
                        .child(stat_skeleton("Since opening"))
                        .child(stat_skeleton("In vault")),
                )
                .child(
                    el("div")
                        .class("sh-card")
                        .class("sh-card--pad")
                        .class("ag-chart")
                        .child(
                            el("div")
                                .class("ag-chart__head")
                                .child(
                                    el("div")
                                        .class("ag-stat")
                                        .child(el("p").class("sh-label").text("Share price"))
                                        .child(skeleton("150", 36)),
                                )
                                .child(
                                    el("div")
                                        .class("sh-seg")
                                        .attr("aria-hidden", "true")
                                        .children(["1W", "1M", "3M", "All"].into_iter().map(
                                            |range| {
                                                el("button")
                                                    .attr("type", "button")
                                                    .attr("tabindex", "-1")
                                                    .attr(
                                                        "aria-pressed",
                                                        if range == "3M" {
                                                            "true"
                                                        } else {
                                                            "false"
                                                        },
                                                    )
                                                    .text(range)
                                            },
                                        )),
                                ),
                        )
                        .child(skeleton_block("100%", 180).class("ag-chart__plot")),
                ),
        )
        .child(
            el("div")
                .class("sh-card")
                .class("ag-side")
                .child(
                    el("div")
                        .class("sh-tabs")
                        .class("ag-side__tabs")
                        .attr("aria-hidden", "true")
                        .child(
                            el("span")
                                .class("sh-tab")
                                .attr("aria-selected", "true")
                                .text("Deposit"),
                        )
                        .child(el("span").class("sh-tab").text("Withdraw")),
                )
                .child(
                    el("div")
                        .class("ag-side__body")
                        .child(el("p").class("ag-side__label").text("Amount"))
                        .child(skeleton_block("100%", 48))
                        .child(skeleton("100%", 14))
                        .child(skeleton("100%", 14))
                        .child(skeleton("70%", 14))
                        .child(
                            el("button")
                                .attr("type", "button")
                                .class("sh-btn")
                                .class("sh-btn-primary")
                                .class("sh-btn-lg")
                                .class("sh-btn-block")
                                .attr("disabled", "")
                                .text("Deposit"),
                        )
                        .child(
                            el("p")
                                .class("ag-risk")
                                .text("The agent can lose money. Nothing here stops that."),
                        ),
                )
                .child(
                    el("div")
                        .class("ag-side__tank")
                        .child(el("p").class("ag-side__tank-title").text("Tank"))
                        .child(skeleton("100%", 8))
                        .child(skeleton("60%", 12)),
                ),
        )
}

/// The page as the store has it now: the agent, the skeleton while it hasn't
/// arrived, or the 404's words if the store has loaded and doesn't know it.
fn content(slug: &str, tab: AgentTab) -> View {
    let store = store::get();
    let agent = store.agent(slug);
    if agent.is_none() && store.loaded.get() {
        return super::not_found::page().body;
    }
    let head = match &agent {
        Some(agent) => el("div")
            .class("ag-head")
            .child(
                el("h1")
                    .class("ag-name")
                    .text_from(&agent.title, Clone::clone),
            )
            .child(
                el("p")
                    .class("ag-meta")
                    .text_from(&agent.meta, Clone::clone),
            )
            .child(
                el("div")
                    .class("sh-chips")
                    .child_from(&agent.review, |review| Some(review_chip(review).into())),
            ),
        None => el("div")
            .class("ag-head")
            .child(
                el("h1")
                    .class("ag-name")
                    .child(skeleton("300", 40))
                    .child(el("span").class("sh-vh").text("Loading agent")),
            )
            .child(skeleton("420", 14)),
    };
    el("div")
        .class("ag")
        .child(head)
        .child(tabs(slug, tab))
        .child(loading())
        .into()
}

pub fn page(slug: &str, tab: AgentTab) -> Page {
    let store = store::get();
    let slug = slug.to_string();
    let now = content(&slug, tab);
    // The agent may arrive after the page is drawn: redraw once it does.
    let arrived = store
        .membership
        .signal()
        .map(move |_| Some(content(&slug, tab)));
    Page {
        top: None,
        body: el("div")
            .class("sh-contents")
            .child_signal(Some(now), arrived)
            .into(),
    }
}
