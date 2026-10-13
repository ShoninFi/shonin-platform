//! `/agents/:id` and its tabs, each its own URL: the overview, `/review`,
//! `/source`, `/verify` and `/activity`.
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
mod source;
mod verify;

use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use shared::{Area, Rating, Strategy};
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
            // The badge is the way in to the code: a link that looks like
            // the fact it states.
            app_link(format!("/agents/{}/source", strategy.slug))
                .class("sh-chip")
                .class(Tone::Neutral.class())
                .class("ag-chip-link")
                .child(icon(Icon::Code, 14))
                .text("Source public")
                .child(el("span").class("sh-vh").text(": read the code"))
                .child(icon(Icon::ChevronRight, 14))
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
    let head = el("div")
        .class("ag-head")
        .child(main)
        .child(price(agent, tab));
    if tab == AgentTab::Overview {
        head.child(about(agent))
    } else {
        head
    }
}

/// The Author's description, held to three lines with a button that opens
/// the rest, and what the Reviewer made of it. The whole text is always in
/// the page: the clamp is drawn only where scripts run (CSS `scripting`), and
/// the button shows only when the app has measured that the text is longer
/// than its three lines.
fn about(agent: &Rc<Agent>) -> El {
    const ID: &str = "ag-description";
    let expanded = Mutable::new(false);
    let overflows = Mutable::new(false);
    let paragraph = el("p")
        .class("ag-tagline")
        .attr("id", ID)
        .class_signal(
            "ag-tagline--clamp",
            true,
            expanded.signal().map(|expanded| !expanded),
        )
        .text_from(&agent.strategy, |strategy| strategy.description.clone())
        .hook({
            let expanded = expanded.clone();
            let overflows = overflows.clone();
            let description = agent
                .strategy
                .signal_ref(|strategy| strategy.description.clone())
                .dedupe_cloned();
            move |dom| {
                dominator::with_node!(dom, element => {
                    .future(measure_clamp(element, description, expanded, overflows))
                })
            }
        });
    let label = |expanded: bool| if expanded { "Show less" } else { "Read more" };
    let toggle = el("button")
        .attr("type", "button")
        .class("ag-more")
        .attr("aria-controls", ID)
        .attr_signal(
            "hidden",
            Some(String::new()),
            map_ref! {
                let overflows = overflows.signal(),
                let expanded = expanded.signal() => (!*overflows && !*expanded).then(String::new)
            },
        )
        .attr_signal(
            "aria-expanded",
            Some("false".to_string()),
            expanded.signal().map(|expanded| Some(expanded.to_string())),
        )
        .child(
            el("span").text_signal(
                label(false),
                expanded
                    .signal()
                    .map(move |expanded| label(expanded).to_string()),
            ),
        )
        .child(icon(Icon::ChevronDown, 16));
    let toggle = on_click(toggle, {
        let expanded = expanded.clone();
        move || {
            expanded.replace_with(|expanded| !*expanded);
        }
    });
    el("div")
        .class("ag-about")
        .child(paragraph)
        .child(toggle)
        .child(live(agent, accuracy))
}

/// Whether the description runs past its three lines, measured again when
/// the window changes size, the web fonts arrive or the text changes, and
/// only while it is held to three lines.
async fn measure_clamp(
    element: web_sys::Element,
    description: impl futures_signals::signal::Signal<Item = String>,
    expanded: Mutable<bool>,
    overflows: Mutable<bool>,
) {
    let fonts = async {
        let ready = crate::web::document()
            .and_then(|document| js_sys::Reflect::get(&document, &"fonts".into()).ok())
            .and_then(|fonts| js_sys::Reflect::get(&fonts, &"ready".into()).ok())
            .and_then(|ready| wasm_bindgen::JsCast::dyn_into::<js_sys::Promise>(ready).ok());
        if let Some(ready) = ready {
            let _ = wasm_bindgen_futures::JsFuture::from(ready).await;
        }
    };
    let changes = map_ref! {
        let _size = dominator::window_size(),
        let _text = description,
        let _fonts = futures_signals::signal::from_future(Box::pin(fonts)),
        let expanded = expanded.signal() => *expanded
    };
    changes
        .for_each(move |expanded| {
            let element = element.clone();
            let overflows = overflows.clone();
            async move {
                if expanded {
                    return;
                }
                // Once the text and the clamp are in place.
                gloo_timers::future::TimeoutFuture::new(0).await;
                overflows.set_neq(element.scroll_height() > element.client_height() + 1);
            }
        })
        .await;
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

/// The header's figures, in the same column as the Exit panel below them:
/// the share price as the page's headline number, its change since opening,
/// the high-water mark (and on the tabs without the record's four figures,
/// the money in the vault), and when they were read from the contracts.
fn price(agent: &Rc<Agent>, tab: AgentTab) -> El {
    let row = |label: &'static str, value: View| -> View {
        el("div")
            .class("ag-price__row")
            .child(el("dt").text(label))
            .child(el("dd").child(value))
            .into()
    };
    let figures = el("dl").class("ag-price__figures").child(
        el("div")
            .class("ag-price__main")
            .child(el("dt").class("sh-label").text("Share price"))
            .child(
                el("dd")
                    .class("ag-price__value")
                    .child(live(agent, |strategy| {
                        text(format::price(strategy.vault.share_price))
                    })),
            )
            .child(
                el("dd")
                    .class("ag-price__change")
                    .child(live(agent, |strategy| {
                        match crate::model::figures(strategy).since_opening {
                            Some(since) => chart::since_opening(since).into(),
                            None => el("span")
                                .class("ag-price__none")
                                .text("No record yet")
                                .into(),
                        }
                    })),
            ),
    );
    let facts = live(agent, move |strategy| {
        let mark = strategy.vault.high_water_mark;
        let mut rows = Vec::new();
        if mark > 0.0 {
            rows.push(row("High-water mark", text(format::price(mark))));
        }
        if tab != AgentTab::Overview {
            rows.push(row(
                "In vault",
                text(format::money_whole(crate::model::in_vault(strategy))),
            ));
        }
        if rows.is_empty() {
            View::Empty
        } else {
            el("dl").class("ag-price__facts").children(rows).into()
        }
    });
    el("div")
        .class("ag-price")
        .child(figures)
        .child(facts)
        .child(read_from(agent))
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

pub(crate) fn banner(banner: &Banner, strategy: &Strategy, this_year: i32) -> El {
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
                rest.push_str(&format!(" Reason: “{}”.", facts::sentence(reason.trim_end_matches(['.', ' ']))));
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
        Banner::Serious { areas, unlisted, refused } => {
            let (words, rest) = serious_words(areas, unlisted, *refused);
            notice(
                NoticeTone::Danger,
                View::Many(vec![
                    el("strong").text(words).into(),
                    text(rest),
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
                text(" You can't read this agent's code. The Reviewer did, and the chain records that it read the same source the running build came from. You're relying on its reading."),
                el("span")
                    .class("sh-notice__actions")
                    .child(link_out(docs("verify/#what-does-it-do"), "What sealed means"))
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

/// The serious-concerns banner's words: the ratings as recorded, and, for
/// an area rated serious with no finding under it, that nothing was listed,
/// so the banner never promises findings the review tab doesn't hold.
pub(crate) fn serious_words(areas: &[Area], unlisted: &[Area], refused: bool) -> (String, String) {
    if refused {
        return (
            "The Reviewer refused this agent's code as written to do harm.".to_string(),
            " You can still deposit, but read what the Reviewer found first. A review isn't a prediction either way.".to_string(),
        );
    }
    let names: Vec<&str> = areas.iter().map(|area| area.label()).collect();
    let lead = match areas.len() {
        6 => "The Reviewer rated all six areas serious concerns.".to_string(),
        1 => format!("The Reviewer rated 1 area serious concerns: {}.", names[0]),
        count => format!(
            "The Reviewer rated {count} areas serious concerns: {}.",
            join_and(&names)
        ),
    };
    let mut rest = String::new();
    if !unlisted.is_empty() {
        let quiet: Vec<&str> = unlisted.iter().map(|area| area.label()).collect();
        rest.push_str(&if unlisted.len() == areas.len() {
            match areas.len() {
                1 => " It listed no finding for that area.".to_string(),
                _ => " It listed no finding for any of them.".to_string(),
            }
        } else {
            format!(" It listed no finding for {}.", join_and(&quiet))
        });
    }
    rest.push_str(" You can still deposit, but read the review first. A review isn't a prediction either way.");
    (lead, rest)
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
        AgentTab::Source => source::body(agent),
        AgentTab::Verify => verify::body(agent),
        AgentTab::Activity => activity::body(agent),
    };
    el("div")
        .class("ag")
        .class(match tab {
            AgentTab::Overview => "ag--overview",
            AgentTab::Review => "ag--review",
            AgentTab::Source => "ag--source",
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
pub(crate) fn loading(slug: &str, tab: AgentTab) -> El {
    el("div")
        .class("ag")
        .attr("aria-busy", "true")
        .child(
            el("div")
                .class("ag-head")
                .child(
                    el("div")
                        .class("ag-head__main")
                        .child(
                            el("h1")
                                .class("ag-name")
                                .child(skeleton("300", 44))
                                .child(el("span").class("sh-vh").text("Loading agent")),
                        )
                        .child(skeleton("420", 16)),
                )
                .child(
                    el("div").class("ag-price").child(
                        el("div")
                            .class("ag-price__figures")
                            .child(skeleton("90", 12))
                            .child(skeleton("150", 40))
                            .child(skeleton("180", 14)),
                    ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(slug: &str) -> Rc<Agent> {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../../../tests/snapshot.json"))
                .expect("the captured snapshot decodes");
        let store = store::get();
        store.apply(&snapshot);
        store.agent(slug).expect("the snapshot has the agent")
    }

    fn header_html(slug: &str, tab: AgentTab) -> String {
        View::from(header(&agent(slug), tab)).html()
    }

    #[test]
    fn the_prerendered_header_carries_the_whole_description_and_a_hidden_toggle() {
        let agent = agent("base-1");
        let description = agent.strategy.lock_ref().description.clone();
        assert!(description.len() > 300, "a paragraph, not a line");
        let html = header_html("base-1", AgentTab::Overview);
        assert!(html.contains(&crate::view::escape(&description)));
        let toggle = &html[html.find("class=\"ag-more\"").expect("the toggle is drawn")..];
        let toggle = &toggle[..toggle.find('>').expect("the tag closes")];
        assert!(
            toggle.contains("hidden=\"\""),
            "hidden until the app has measured"
        );
        assert!(toggle.contains("aria-expanded=\"false\""));
        assert!(toggle.contains("aria-controls=\"ag-description\""));
    }

    #[test]
    fn the_header_leads_with_the_share_price_its_change_and_high_water_mark() {
        let html = header_html("base-1", AgentTab::Overview);
        let price = &html[html
            .find("class=\"ag-price\"")
            .expect("the price block is drawn")..];
        assert!(price.contains("Share price"));
        // A share price, whatever the fixture holds: dollars to four decimals.
        let figure = price.split('$').skip(1).find_map(|rest| {
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
                .collect();
            let (_, decimals) = digits.rsplit_once('.')?;
            (decimals.len() == 4 && decimals.chars().all(|c| c.is_ascii_digit())).then_some(digits)
        });
        assert!(
            figure.is_some(),
            "no share price to four decimals in {price}"
        );
        assert!(price.contains("since opening"));
        assert!(price.contains("High-water mark"));
        assert!(price.contains("Read from its contracts"));
        assert!(!price.contains("In vault"), "the overview's record says it");
    }

    #[test]
    fn the_other_tabs_add_the_money_in_the_vault_and_leave_out_the_description() {
        let html = header_html("base-1", AgentTab::Review);
        assert!(html.contains("In vault"));
        assert!(!html.contains("ag-description"));
    }

    fn tab_html(slug: &str, tab: AgentTab) -> String {
        View::from(agent_page(&agent(slug), tab)).html()
    }

    #[test]
    fn no_page_the_app_renders_carries_a_placeholder() {
        agent("base-1");
        for route in crate::route::Route::all_pages() {
            let html = View::from(crate::shell::shell(&route, false)).html();
            assert!(
                !html.contains("[CONFIRM"),
                "{route:?} carries a placeholder"
            );
            assert!(
                !html.contains("CONFIRM:"),
                "{route:?} carries a placeholder"
            );
        }
    }

    #[test]
    fn the_source_page_lists_every_file_and_holds_each_one_whole() {
        let agent = agent("base-1");
        let strategy = agent.strategy.lock_ref().clone();
        assert!(!strategy.bundle.source_files.is_empty());
        let html = tab_html("base-1", AgentTab::Source);
        let nav = &html[html.find("ag-src__nav").expect("the file list is drawn")..];
        for file in &strategy.bundle.source_files {
            let id = source::file_id(&file.path);
            assert!(
                nav.contains(&format!("href=\"#{id}\"")),
                "{} is listed",
                file.path
            );
            assert!(
                html.contains(&format!("id=\"{id}\"")),
                "{} is in the page",
                file.path
            );
            let last = file.body.lines().rev().find(|line| !line.trim().is_empty());
            if let Some(last) = last {
                assert!(
                    html.contains(&crate::view::escape(last)),
                    "{} is whole",
                    file.path
                );
            }
        }
        assert!(html.contains(&strategy.bundle.source_hash));
        assert!(html.contains(&format!(
            "/store/{}/source.tar",
            strategy.bundle.source_hash
        )));
        assert!(html.contains("download=\"source.tar\""));
        // Exactly one file is shown first without scripts.
        assert_eq!(html.matches("ag-src__file--first").count(), 1);
        // Without scripts the address picks the file, so the list marks none.
        assert!(!nav.contains("aria-current=\"true\""));
    }

    #[test]
    fn a_sealed_agents_source_page_says_so_and_offers_nothing_to_open() {
        let html = tab_html("base-6", AgentTab::Source);
        assert!(html.contains("Source sealed: only the Reviewer read it"));
        assert!(!html.contains("source.tar"));
        assert!(!html.contains("ag-src__file"));
        let header = header_html("base-6", AgentTab::Source);
        assert!(header.contains("Source sealed"));
        assert!(
            !header.contains("/agents/base-6/source"),
            "the badge links nowhere"
        );
    }

    #[test]
    fn the_source_public_badge_links_to_the_code() {
        let html = header_html("base-1", AgentTab::Overview);
        let badge = &html[..html.find("Source public").expect("the badge is drawn")];
        let open = badge.rfind("<a ").expect("the badge is a link");
        assert!(badge[open..].contains("href=\"/agents/base-1/source\""));
    }

    #[test]
    fn the_review_tab_leads_with_the_report_card() {
        let html = tab_html("base-1", AgentTab::Review);
        let body = &html[html.find("ag-layout").expect("the tab is drawn")..];
        let card = body.find("Report card").expect("the report card is drawn");
        let found = body
            .find("What the Reviewer found")
            .expect("the summary is drawn");
        let areas = body.find("Area by area").expect("the areas are drawn");
        assert!(card < found && found < areas);
        let first = body.find("<section").expect("a section");
        assert!(
            body[first..]
                .starts_with("<section class=\"sh-card ag-card ag-card--flush ag-report-card\"")
        );
    }

    #[test]
    fn the_reviewers_summary_is_whole_in_the_page_behind_a_real_toggle() {
        let agent = agent("base-1");
        let summary = agent.strategy.lock_ref().review.clone().unwrap().summary;
        let html = tab_html("base-1", AgentTab::Review);
        for paragraph in review::paragraphs(&summary) {
            let words: String = paragraph.chars().take(60).collect();
            assert!(html.contains(&crate::view::escape(&words)), "{words}");
        }
        let at = html.find("ag-summary__more").expect("the toggle is drawn");
        let open = html[..at].rfind('<').unwrap();
        let tag = &html[open..open + html[open..].find('>').unwrap()];
        assert!(tag.starts_with("<button"), "a real button: {tag}");
        assert!(tag.contains("aria-controls=\"ag-summary\""));
        assert!(tag.contains("aria-expanded=\"false\""));
        assert!(
            tag.contains("hidden=\"\""),
            "hidden until the app has measured"
        );
        assert!(
            html.contains("Read the Reviewer&#39;s full summary")
                || html.contains("Read the Reviewer's full summary")
        );
        assert!(html.contains("id=\"ag-summary\""));
    }

    #[test]
    fn the_overview_shows_the_review_in_brief_and_leaves_the_detail_to_its_tab() {
        let agent = agent("base-1");
        let summary = agent.strategy.lock_ref().review.clone().unwrap().summary;
        let html = tab_html("base-1", AgentTab::Overview);
        let card = &html[html.find("ag-o-review").expect("the review card is drawn")..];
        let card = &card[..card.find("</section>").unwrap()];
        for area in shared::Area::ALL {
            assert!(card.contains(area.label()), "{}", area.label());
        }
        assert!(card.contains("Headline finding"));
        assert!(card.contains("Read the full review"));
        let opening: String = review::paragraphs(&summary)[0].chars().take(60).collect();
        assert!(
            !html.contains(&crate::view::escape(&opening)),
            "the summary lives on Review"
        );
        assert!(!card.contains("What its Author promises"));
    }

    #[test]
    fn the_headline_finding_is_the_most_severe_one() {
        let agent = agent("base-5");
        let review = agent.strategy.lock_ref().review.clone().unwrap();
        let (area, finding) = overview::headline_finding(&review).expect("findings");
        assert_eq!(finding.severity, shared::Severity::Serious);
        assert_eq!(area, shared::Area::Reliability);
    }

    #[test]
    fn what_the_page_calls_the_build_is_the_hash_the_chain_holds_and_the_component_is_named_apart()
    {
        let agent = agent("base-1");
        let bundle = agent.strategy.lock_ref().bundle.clone();
        assert_ne!(bundle.bundle_hash, bundle.component_hash);
        let short = |hash: &str| crate::format::short_hash(hash);
        let html = tab_html("base-1", AgentTab::Overview);
        let card = &html[html.find("ag-o-build").expect("the build card is drawn")..];
        let card = &card[..card.find("</section>").unwrap()];
        assert!(card.contains(&short(&bundle.bundle_hash)), "{card}");
        assert!(!card.contains(&short(&bundle.component_hash)), "{card}");
        let rows = side::facts_hashes(&agent.strategy.lock_ref());
        assert!(rows.contains(&("Build", bundle.bundle_hash.clone())));
        assert!(rows.contains(&("Component", bundle.component_hash.clone())));
        let html = tab_html("base-1", AgentTab::Verify);
        let on_chain = &html[html
            .find("Build on chain")
            .expect("the build on chain is named")..];
        assert!(on_chain[..400.min(on_chain.len())].contains(&short(&bundle.bundle_hash)));
        assert!(
            !html.contains("The chain holds"),
            "the chain holds the build, not the component"
        );
        assert!(
            html.contains("The build&#39;s component") || html.contains("The build's component")
        );
    }

    #[test]
    fn the_verify_tab_checks_the_source_and_build_with_real_commands() {
        let agent = agent("base-1");
        let bundle = agent.strategy.lock_ref().bundle.clone();
        let html = tab_html("base-1", AgentTab::Verify);
        let digits = |hash: &str| hash.trim_start_matches("sha256:").to_string();
        assert!(html.contains("shasum -a 256 source.tar"));
        assert!(html.contains(&format!("# expect {}", digits(&bundle.source_hash))));
        assert!(html.contains(&format!("# expect {}", digits(&bundle.component_hash))));
        assert!(html.contains(&format!("/store/{}/component.wasm", bundle.bundle_hash)));
        assert!(html.contains("ag-copy"), "a copy button");
        assert!(
            html.contains("href=\"/agents/base-1/source\""),
            "the source record links to it"
        );
    }
}
