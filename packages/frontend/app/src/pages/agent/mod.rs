//! `/agents/:id` and its tabs, each its own URL: the overview, `/review`,
//! `/verify` and `/activity`.
//!
//! The page is drawn once per visit. Each section follows the agent's record
//! in the store and is redrawn only when what it shows has changed, so a
//! figure that moves changes in place and the rest of the page stays still.

mod activity;
mod chart;
pub mod facts;
mod overview;
mod parts;
mod review;
mod side;
mod verify;

use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::SignalExt;
use shared::{Rating, Strategy};
use ui::icons::Icon;

use self::facts::{Banner, ChipTone};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, on_click};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::rating::review_chip;
use crate::atoms::skeleton::{skeleton, skeleton_block};
use crate::components::exit::{self, Tab};
use crate::format;
use crate::model::{TankState, rating_word};
use crate::route::{AgentTab, DOCS};
use crate::store::{self, Agent};
use crate::view::{El, View, el, text};
use crate::web::app_link;

use super::Page;

/// A part of the page that follows the agent's record: drawn now from the
/// record as it is, and redrawn only when what it draws has changed.
pub(crate) fn live(agent: &Rc<Agent>, draw: impl Fn(&Strategy) -> View + 'static) -> El {
    let draw = Rc::new(draw);
    let now = draw(&agent.strategy.lock_ref());
    let key = draw.clone();
    let strategy = agent.strategy.clone();
    let signal = agent
        .strategy
        .signal_ref(move |strategy| key(strategy).html())
        .dedupe_cloned()
        .map(move |_| Some(draw(&strategy.lock_ref())));
    el("div")
        .class("sh-contents")
        .child_signal(Some(now), signal)
}

/// What the page knows of the agent right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    /// No snapshot yet: the skeleton.
    Loading,
    /// The snapshot has no such agent: the 404's words.
    Missing,
    Here,
}

fn known(slug: &str) -> Known {
    let store = store::get();
    if store.agent(slug).is_some() {
        Known::Here
    } else if store.loaded.get() {
        Known::Missing
    } else {
        Known::Loading
    }
}

pub fn page(slug: &str, tab: AgentTab) -> Page {
    let store = store::get();
    let slug = slug.to_string();
    let now = content(&slug, tab, known(&slug));
    let signal = {
        let slug = slug.clone();
        let asked = slug.clone();
        map_ref! {
            let _membership = store.membership.signal(),
            let _loaded = store.loaded.signal() => known(&asked)
        }
        .dedupe()
        .map(move |state| Some(content(&slug, tab, state)))
    };
    Page {
        top: None,
        body: el("div")
            .class("sh-contents")
            .child_signal(Some(now), signal)
            .into(),
    }
}

fn content(slug: &str, tab: AgentTab, state: Known) -> View {
    match state {
        Known::Missing => el("div")
            .class("sh-contents")
            .child(super::not_found::page().body)
            .hook(titled(crate::route::Route::NotFound.page().title))
            .into(),
        Known::Loading => loading(slug, tab).into(),
        Known::Here => match store::get().agent(slug) {
            Some(agent) => agent_page(&agent, tab)
                .hook(titled(
                    crate::route::Route::Agent {
                        slug: slug.to_string(),
                        tab,
                    }
                    .page()
                    .title,
                ))
                .into(),
            None => loading(slug, tab).into(),
        },
    }
}

/// The tab's title once the page knows what it shows: the agent's name, or
/// "Page not found" (the app named the tab before the record arrived).
fn titled(
    title: String,
) -> impl FnOnce(dominator::DomBuilder<web_sys::Element>) -> dominator::DomBuilder<web_sys::Element>
{
    move |dom| {
        dom.after_inserted(move |_| {
            if let Some(document) = crate::web::document() {
                document.set_title(&title);
            }
        })
    }
}

// ---- The header -----------------------------------------------------------

fn crumb(agent: &Rc<Agent>) -> El {
    el("nav")
        .class("ag-crumb")
        .attr("aria-label", "Breadcrumb")
        .child(
            el("ol")
                .child(
                    el("li")
                        .child(app_link("/agents").text("Agents"))
                        .child(el("span").attr("aria-hidden", "true").text("/")),
                )
                .child(
                    el("li")
                        .attr("aria-current", "page")
                        .text_from(&agent.title, Clone::clone),
                ),
        )
}

fn tone(chip: &ChipTone) -> Tone {
    match chip {
        ChipTone::Gain => Tone::Gain,
        ChipTone::Neutral => Tone::Neutral,
        ChipTone::Caution => Tone::Caution,
        ChipTone::Loss => Tone::Loss,
    }
}

/// The chips under the name: the vault's state, the tank when it needs
/// saying, the review in one chip, how you'd get out, and the source.
fn facts_row(strategy: &Strategy, now: Option<time::PrimitiveDateTime>) -> View {
    let (words, chip) = facts::state_chip(strategy, now);
    let glyph = match chip {
        ChipTone::Gain => Some(Icon::Check),
        ChipTone::Caution => Some(Icon::Clock),
        ChipTone::Loss => Some(Icon::Pause),
        ChipTone::Neutral => None,
    };
    let mut chips: Vec<View> = vec![badge(tone(&chip), glyph, words).into()];
    let tank = TankState::of(strategy);
    let running = matches!(
        strategy.vault.state,
        shared::VaultState::Open | shared::VaultState::WindingDown
    );
    if running && tank.is_empty() {
        chips.push(badge(Tone::Loss, Some(Icon::Tank), "Stopped · tank empty").into());
    } else if running && tank.is_low() {
        chips.push(
            badge(
                Tone::Caution,
                Some(Icon::Tank),
                format!("Tank low · {}", format::days(tank.days.floor() as u32)),
            )
            .into(),
        );
    }
    let summary = crate::model::ReviewSummary::of(strategy.review.as_ref());
    if summary != crate::model::ReviewSummary::AllFine {
        chips.push(review_chip(&summary).into());
    }
    chips.push(
        match strategy.vault.exit_class {
            shared::ExitClass::Direct => badge(Tone::Neutral, Some(Icon::Exit), "Direct exit"),
            shared::ExitClass::Served { .. } => {
                badge(Tone::Neutral, Some(Icon::Exit), "Served exit")
            }
        }
        .into(),
    );
    chips.push(
        if strategy.bundle.source.is_published() {
            badge(Tone::Neutral, Some(Icon::Code), "Source public")
        } else {
            badge(Tone::Neutral, Some(Icon::Lock), "Source sealed")
        }
        .into(),
    );
    View::Many(chips)
}

fn chain_tile(chain: shared::Chain) -> El {
    el("span")
        .class("ag-tile")
        .attr("aria-hidden", "true")
        .text(chain.label().chars().next().unwrap_or('?').to_string())
}

fn header(agent: &Rc<Agent>, tab: AgentTab) -> El {
    let author = el("span")
        .class("ag-by")
        .text("by ")
        .child_from(&agent.author_key, {
            let strategy = agent.strategy.clone();
            move |key| {
                let address = strategy.lock_ref().author_address.clone();
                let short = format::short_hash(&address);
                Some(match key {
                    Some(key) => app_link(format!("/agents?author={}", crate::route::encode(key)))
                        .class("sh-mono")
                        .attr("aria-label", format!("Agents by {address}"))
                        .text(short)
                        .into(),
                    None => el("span").class("sh-mono").text(short).into(),
                })
            }
        });
    let chain = agent.chain;
    let opened = {
        let registered = agent.registered.clone();
        el("span")
            .class("ag-chain")
            .child(chain_tile(chain))
            .text_from(&agent.asset, move |asset| {
                format!("{} ({asset})", chain.label())
            })
            .child(
                el("span")
                    .class("ag-opened")
                    .text_from(&registered, |date| {
                        format!("Opened {}", facts::full_day(*date))
                    }),
            )
    };
    let store = store::get();
    let as_of = store.as_of.clone();
    let main = el("div")
        .class("ag-head__main")
        .child(
            el("h1")
                .class("ag-name")
                .text_from(&agent.title, Clone::clone)
                .hook(transition_name(format!("agent-{}-name", agent.slug))),
        )
        .child(
            el("div")
                .class("ag-facts")
                .child(author)
                .child(live(agent, move |strategy| {
                    facts_row(strategy, as_of.get())
                }))
                .child(opened),
        );
    let main = if tab == AgentTab::Overview {
        main.child(
            el("p")
                .class("ag-tagline")
                .text_from(&agent.strategy, |strategy| strategy.description.clone()),
        )
        .child(live(agent, accuracy))
    } else {
        main
    };
    let aside = if tab == AgentTab::Overview {
        read_from(agent)
    } else {
        head_figures(agent)
    };
    el("div").class("ag-head").child(main).child(aside)
}

/// The line under the author's description: what the reviewer made of it.
fn accuracy(strategy: &Strategy) -> View {
    let Some(review) = &strategy.review else {
        return View::Empty;
    };
    let rating = review.rating(shared::Area::AccurateDescription);
    let glyph = crate::atoms::rating::rating_icon(rating);
    el("p")
        .class("ag-accuracy")
        .class(match rating {
            Rating::LooksFine => "ag-accuracy--fine",
            Rating::SomeConcerns => "ag-accuracy--some",
            Rating::SeriousConcerns => "ag-accuracy--serious",
            Rating::CouldntTell => "ag-accuracy--unknown",
        })
        .child(icon(glyph, 16))
        .text(format!(
            "The Author's description. The Reviewer rated it Accurate description: {}.",
            rating_word(rating).to_lowercase()
        ))
        .into()
}

/// "Read from its contracts" and when: the vault's own chain's latest read,
/// or the moment its figures stopped being current.
fn read_from(agent: &Rc<Agent>) -> El {
    let store = store::get();
    let chain = agent.chain;
    let stale = agent.stale_as_of.clone();
    let words = move |chains: &Vec<store::ChainRead>| {
        if let Some(at) = stale.get() {
            return (
                true,
                format!("Figures as of {}: its latest read failed", day_time(at)),
            );
        }
        let at = chains
            .iter()
            .find(|read| read.chain == chain)
            .and_then(|read| read.as_of);
        (
            false,
            match at {
                Some(at) => format!("Read from its contracts · {}", day_time(at)),
                None => "Read from its contracts".to_string(),
            },
        )
    };
    let now = words(&store.chains.lock_ref());
    let signal = map_ref! {
        let chains = store.chains.signal_cloned(),
        let _stale = agent.stale_as_of.signal() => chains.clone()
    }
    .map({
        let words = words.clone();
        move |chains| {
            let (stale, text) = words(&chains);
            Some(read_line(stale, text).into())
        }
    });
    el("div")
        .class("ag-read")
        .child_signal(Some(read_line(now.0, now.1).into()), signal)
}

fn read_line(stale: bool, words: String) -> El {
    el("p")
        .class("ag-read__line")
        .class_if(stale, "ag-read__line--stale")
        .child(icon(if stale { Icon::Concern } else { Icon::Clock }, 16))
        .text(words)
}

fn day_time(at: time::PrimitiveDateTime) -> String {
    format!("{}, {}", facts::full_day(at.date()), format::time_short(at))
}

/// On the other tabs the header carries the share price and the money in
/// the vault, each with its period.
fn head_figures(agent: &Rc<Agent>) -> El {
    el("dl")
        .class("ag-head__figures")
        .child(
            el("div")
                .child(el("dt").class("sh-label").text("Share price"))
                .child(
                    el("dd")
                        .class("ag-head__figure")
                        .child(live(agent, |strategy| {
                            text(format::price(strategy.vault.share_price))
                        })),
                )
                .child(
                    el("dd")
                        .class("ag-head__period")
                        .child(live(agent, |strategy| {
                            match crate::model::figures(strategy).since_opening {
                                Some(since) => el("span")
                                    .class_if(
                                        !crate::atoms::delta::tone_class(since).is_empty(),
                                        crate::atoms::delta::tone_class(since),
                                    )
                                    .text(format!("{} since opening", format::ret(since)))
                                    .into(),
                                None => text("No record yet"),
                            }
                        })),
                ),
        )
        .child(
            el("div")
                .child(el("dt").class("sh-label").text("In vault"))
                .child(
                    el("dd")
                        .class("ag-head__figure")
                        .text_from(&agent.in_vault, |value| format::money_whole(*value)),
                )
                .child(
                    el("dd")
                        .class("ag-head__period")
                        .text_from(&agent.figures, |figures| {
                            format!("{} open", format::days(figures.days))
                        }),
                ),
        )
}

/// The name of the element that morphs between the leaderboard and this
/// page (Motion M5), set through the CSS object model: the page's policy
/// allows no inline styles in markup.
pub(crate) fn transition_name(
    name: String,
) -> impl FnOnce(dominator::DomBuilder<web_sys::Element>) -> dominator::DomBuilder<web_sys::Element>
{
    move |dom| {
        dom.after_inserted(move |element| {
            use wasm_bindgen::JsCast;
            if let Some(element) = element.dyn_ref::<web_sys::HtmlElement>() {
                let _ = element.style().set_property("view-transition-name", &name);
            }
        })
    }
}

// ---- Banners --------------------------------------------------------------

fn docs(path: &str) -> String {
    format!("{DOCS}{path}")
}

fn link_out(href: String, words: &'static str) -> El {
    el("a")
        .attr("href", href)
        .text(words)
        .child(icon(Icon::ArrowRight, 16))
}

fn banner(banner: &Banner, strategy: &Strategy, this_year: i32) -> El {
    let when = |at: Option<time::PrimitiveDateTime>| {
        at.map(|at| format::day(at.date(), this_year))
            .unwrap_or_else(|| "a recent day".to_string())
    };
    let status_link = || app_link("/status").text("See it on the status page");
    match banner {
        Banner::Killed {
            at,
            reason,
            outcome,
            ..
        } => {
            let at_words = at
                .map(|at| format!("on {} at {}", format::day(at.date(), this_year), format::time_short(at)))
                .unwrap_or_else(|| "recently".to_string());
            let mut rest = String::new();
            if !reason.is_empty() {
                rest.push_str(&format!(" Reason: “{}”.", facts::sentence(reason)));
            }
            if let Some(outcome) = outcome {
                rest.push_str(&format!(" {}.", facts::sentence(outcome.trim_end_matches('.'))));
            }
            rest.push_str(" A halt, never a theft: the money can only go back to Depositors.");
            notice(
                NoticeTone::Danger,
                View::Many(vec![
                    el("strong").text(format!("Shonin killed this vault {at_words}.")).into(),
                    text(rest),
                    el("span").class("sh-notice__actions").child(status_link()).into(),
                ]),
            )
            .class("ag-banner")
            .class("ag-banner--loud")
        }
        Banner::Frozen { at } => notice(
            NoticeTone::Danger,
            View::Many(vec![
                el("strong").text("This vault is frozen.").into(),
                text(format!(
                    " A booked withdrawal went unpaid for {}, so on {} it was frozen. No new deposits. Its trading keys have lost their approval, and the only thing left is winding down its positions. You can still withdraw.",
                    format::days(strategy.vault.freeze_after_days),
                    when(*at)
                )),
                el("span")
                    .class("sh-notice__actions")
                    .child(link_out(docs("depositors/"), "What a freeze means"))
                    .child(status_link())
                    .into(),
            ]),
        )
        .class("ag-banner"),
        Banner::Closed => notice(
            NoticeTone::Info,
            View::Many(vec![
                el("strong").text("This agent has stopped for good.").into(),
                text(" Everything is in the vault's cash now. Withdraw whenever you like: there's no deadline, and nobody needs to act for you to be paid."),
            ]),
        )
        .class("ag-banner"),
        Banner::Cancelled => notice(
            NoticeTone::Info,
            View::Many(vec![
                el("strong").text("This agent was cancelled before it opened.").into(),
                text(" It never took a deposit and never started. Its tank went back to the Author."),
                el("span")
                    .class("sh-notice__actions")
                    .child(app_link("/agents").text("Browse other agents"))
                    .into(),
            ]),
        )
        .class("ag-banner"),
        Banner::Activating => notice(
            NoticeTone::Info,
            View::Many(vec![
                el("strong").text("This agent is being activated.").into(),
                text(" Its vault is being set up, with a station on each chain it trades on. Deposits open when that's done. The agent hasn't started yet."),
            ]),
        )
        .class("ag-banner"),
        Banner::TankEmpty => notice(
            NoticeTone::Danger,
            View::Many(vec![
                el("strong").text("The agent is stopped: its tank is empty.").into(),
                text(" Nothing is managing this vault's positions right now. They stay open, unmanaged, until someone tops up the tank. Anyone can, and the agent starts again once it has enough to run. You can still withdraw."),
                el("span")
                    .class("sh-notice__actions")
                    .child(link_out(docs("tank/"), "How the tank works"))
                    .into(),
            ]),
        )
        .class("ag-banner")
        .class("ag-banner--loud"),
        Banner::Serious { areas, refused } => {
            let words = if *refused {
                "The Reviewer refused this agent's code as written to do harm.".to_string()
            } else {
                let names: Vec<&str> = areas.iter().map(|area| area.label()).collect();
                match areas.len() {
                    6 => "The review found serious concerns in all six areas.".to_string(),
                    count => format!(
                        "The review found serious concerns in {}: {}.",
                        if count == 1 { "1 area".to_string() } else { format!("{count} areas") },
                        join_and(&names)
                    ),
                }
            };
            notice(
                NoticeTone::Danger,
                View::Many(vec![
                    el("strong").text(words).into(),
                    text(" You can still deposit, but read what the Reviewer found first. A review isn't a prediction either way."),
                    el("span")
                        .class("sh-notice__actions")
                        .child(app_link(format!("/agents/{}/review", strategy.slug)).text("Read the review"))
                        .into(),
                ]),
            )
            .class("ag-banner")
        }
        Banner::Retiring { since, ends } => {
            let until = ends
                .map(|end| format!("until {} at {}", format::day(end.date(), this_year), format::time_short(end)))
                .unwrap_or_else(|| "for seven days".to_string());
            notice(
                NoticeTone::Caution,
                View::Many(vec![
                    el("strong").text("The Author is retiring this agent.").into(),
                    text(format!(
                        " Deposits stopped on {}. It keeps trading {until}, then closes its positions at market and stops for good. Closing at market can lock in a loss. You can withdraw as usual until then, and after that from the vault's cash.",
                        when(*since)
                    )),
                    el("span")
                        .class("sh-notice__actions")
                        .child(link_out(docs("depositors/"), "What retiring means"))
                        .into(),
                ]),
            )
            .class("ag-banner")
        }
        Banner::TankLow { days } => notice(
            NoticeTone::Caution,
            View::Many(vec![
                el("strong")
                    .text(format!(
                        "The tank is running low: {} left.",
                        if *days < 1 { "under a day".to_string() } else { format!("about {}", format::days(*days)) }
                    ))
                    .into(),
                text(" The tank pays to run the agent, never from your money. If it empties, the agent stops and its positions sit unmanaged until someone tops it up. Anyone can top it up."),
                el("span")
                    .class("sh-notice__actions")
                    .child(link_out(docs("tank/"), "How the tank works"))
                    .into(),
            ]),
        )
        .class("ag-banner"),
        Banner::New { days } => notice(
            NoticeTone::Info,
            View::Many(vec![
                el("strong")
                    .text(format!("New agent: {} of history.", format::days(*days)))
                    .into(),
                text(" It joins the main leaderboard once it has 30 days. A short record says very little about what comes next."),
            ]),
        )
        .class("ag-banner"),
        Banner::Sealed => notice_with_icon(
            Icon::Lock,
            View::Many(vec![
                el("strong").text("Source sealed.").into(),
                text(" Only the Reviewer and the build machine have seen this agent's code. You can check that what runs is what was reviewed, but you can't read the code yourself."),
                el("span")
                    .class("sh-notice__actions")
                    .child(app_link(format!("/agents/{}/verify", strategy.slug)).text("How sealed agents are checked"))
                    .into(),
            ]),
        ),
        Banner::Served => notice_with_icon(
            Icon::Exit,
            View::Many(vec![
                el("strong").text("Served exit.").into(),
                text(format!(
                    " This vault trades on other chains, so a booked withdrawal needs an unwinder to finish it. If one goes unpaid for {}, anyone can freeze the vault.",
                    format::days(strategy.vault.freeze_after_days)
                )),
                el("span")
                    .class("sh-notice__actions")
                    .child(link_out(docs("depositors/"), "How exits work"))
                    .into(),
            ]),
        ),
    }
}

fn notice_with_icon(glyph: Icon, body: View) -> El {
    crate::atoms::notice::notice_with(NoticeTone::Info, glyph, body).class("ag-banner")
}

pub(crate) fn join_and(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn banners(agent: &Rc<Agent>) -> El {
    let store = store::get();
    let as_of = store.as_of.clone();
    el("div")
        .class("ag-banners")
        .child(live(agent, move |strategy| {
            let this_year = as_of.get().map(|at| at.year()).unwrap_or(2026);
            View::Many(
                facts::banners(strategy)
                    .iter()
                    .map(|item| banner(item, strategy, this_year).into())
                    .collect(),
            )
        }))
}

// ---- Tabs and layout ------------------------------------------------------

fn tabs(slug: &str, current: AgentTab) -> El {
    el("nav")
        .class("sh-tabs")
        .class("ag-tabs")
        .attr("aria-label", "Agent sections")
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

/// The phone's action bar: Withdraw and Deposit, which open the Exit sheet
/// on the matching side. It takes the tab bar's place on this page.
fn action_bar(slug: &str) -> El {
    let deposit = {
        let slug = slug.to_string();
        on_click(Button::new(Kind::Primary, "Deposit").build(), move || {
            exit::open_sheet(&slug, Tab::Deposit)
        })
    };
    let withdraw = {
        let slug = slug.to_string();
        on_click(
            Button::new(Kind::Secondary, "Withdraw").build(),
            move || exit::open_sheet(&slug, Tab::Withdraw),
        )
    };
    el("div")
        .class("ag-actionbar")
        .attr("role", "region")
        .attr("aria-label", "Deposit or withdraw")
        .child(
            el("p")
                .class("ag-risk")
                .child(icon(Icon::Concern, 16))
                .text("The agent can lose money. Nothing here stops that."),
        )
        .child(
            el("div")
                .class("ag-actionbar__buttons")
                .child(withdraw)
                .child(deposit),
        )
}

fn agent_page(agent: &Rc<Agent>, tab: AgentTab) -> El {
    let body = match tab {
        AgentTab::Overview => overview::body(agent),
        AgentTab::Review => review::body(agent),
        AgentTab::Verify => verify::body(agent),
        AgentTab::Activity => activity::body(agent),
    };
    el("div")
        .class("ag")
        .class(match tab {
            AgentTab::Overview => "ag--overview",
            AgentTab::Review => "ag--review",
            AgentTab::Verify => "ag--verify",
            AgentTab::Activity => "ag--activity",
        })
        .child(crumb(agent))
        .child(header(agent, tab))
        .child(banners(agent))
        .child(tabs(&agent.slug, tab))
        .child(body)
        .child(action_bar(&agent.slug))
}

// ---- Loading --------------------------------------------------------------

fn stat_skeleton(label: &'static str) -> El {
    el("div")
        .class("ag-stat")
        .child(el("p").class("sh-label").text(label))
        .child(skeleton("110", 32))
        .child(skeleton("72", 12))
}

/// The skeleton the States board draws while the first snapshot is on its
/// way: the page's shape, nothing pretending to be a figure.
fn loading(slug: &str, tab: AgentTab) -> El {
    el("div")
        .class("ag")
        .attr("aria-busy", "true")
        .child(
            el("div").class("ag-head").child(
                el("div")
                    .class("ag-head__main")
                    .child(
                        el("h1")
                            .class("ag-name")
                            .child(skeleton("300", 44))
                            .child(el("span").class("sh-vh").text("Loading agent")),
                    )
                    .child(skeleton("420", 16)),
            ),
        )
        .child(tabs(slug, tab))
        .child(
            el("div")
                .class("ag-layout")
                .child(
                    el("div")
                        .class("ag-main")
                        .child(
                            el("div")
                                .class("sh-card")
                                .class("ag-stats")
                                .child(stat_skeleton("30 days"))
                                .child(stat_skeleton("Since opening"))
                                .child(stat_skeleton("Deepest drop"))
                                .child(stat_skeleton("In vault")),
                        )
                        .child(
                            el("div")
                                .class("sh-card")
                                .class("ag-chart")
                                .child(skeleton("150", 40))
                                .child(skeleton_block("100%", 220)),
                        ),
                )
                .child(
                    el("aside")
                        .class("ag-side")
                        .attr("aria-label", "Deposit, withdraw and running costs")
                        .child(
                            el("div")
                                .class("sh-card")
                                .class("ag-card")
                                .child(skeleton_block("100%", 38))
                                .child(skeleton_block("100%", 56))
                                .child(skeleton("100%", 14))
                                .child(skeleton("70%", 14))
                                .child(skeleton_block("100%", 52)),
                        ),
                ),
        )
}
