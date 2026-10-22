//! `/status`: platform events (Status-Desktop). Every kill, freeze and
//! allowlist change as the feed posts it, newest first, by day; a machine
//! image's two-day public wait as a Pending → Counted stepper; how fresh the
//! indexer's read of each chain is; and why the page exists. The RSS and JSON
//! feeds stay hidden until their addresses exist.

use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::SignalVecExt;
use shared::FeedKind;
use time::{Date, Duration, PrimitiveDateTime};
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::hash::hash;
use crate::atoms::{Tone, badge, icon};
use crate::format;
use crate::freshness::Freshness;
use crate::route::DOCS;
use crate::store::{self, ChainRead, FeedItem};
use crate::view::{El, View, el};
use crate::web::app_link;

use super::Page;

/// The feed's filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Kills,
    Freezes,
    Images,
    Venues,
    Agents,
}

impl Filter {
    pub const ALL: [Filter; 6] = [
        Filter::All,
        Filter::Kills,
        Filter::Freezes,
        Filter::Images,
        Filter::Venues,
        Filter::Agents,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Kills => "Kills",
            Filter::Freezes => "Freezes",
            Filter::Images => "Machine images",
            Filter::Venues => "Venues",
            Filter::Agents => "Agents",
        }
    }

    pub fn keeps(self, kind: FeedKind) -> bool {
        match self {
            Filter::All => true,
            Filter::Kills => kind == FeedKind::Kill,
            Filter::Freezes => kind == FeedKind::VaultFrozen,
            Filter::Images => matches!(kind, FeedKind::ImageProposed | FeedKind::ImageRevoked),
            Filter::Venues => matches!(kind, FeedKind::VenueSuspended | FeedKind::VenueRestored),
            Filter::Agents => matches!(
                kind,
                FeedKind::StrategyRegistered | FeedKind::ReviewPublished | FeedKind::ClaimNote
            ),
        }
    }
}

/// What kind of event, as its chip says it.
fn kind_chip(kind: FeedKind) -> El {
    match kind {
        FeedKind::Kill => badge(Tone::Loss, Some(Icon::Pause), "Kill"),
        FeedKind::VaultFrozen => badge(Tone::Loss, Some(Icon::Pause), "Freeze"),
        FeedKind::ImageProposed | FeedKind::ImageRevoked => {
            badge(Tone::Neutral, Some(Icon::Machine), "Machine image")
        }
        FeedKind::VenueSuspended | FeedKind::VenueRestored => {
            badge(Tone::Neutral, Some(Icon::Bridge), "Venue")
        }
        FeedKind::StrategyRegistered | FeedKind::ReviewPublished => {
            badge(Tone::Neutral, Some(Icon::Review), "Agent")
        }
        FeedKind::ClaimNote => badge(Tone::Neutral, Some(Icon::Clock), "Claim"),
    }
}

/// When a proposed image counts: its timelock after it was proposed.
pub(crate) fn counts_at(item: &FeedItem, timelock_days: Option<f64>) -> Option<PrimitiveDateTime> {
    if item.kind != FeedKind::ImageProposed {
        return None;
    }
    let seconds = (timelock_days? * 86_400.0).round() as i64;
    Some(item.at + Duration::seconds(seconds))
}

/// The state chip on the right of an event.
fn state_chip(item: &FeedItem, counted: Option<bool>) -> El {
    match item.kind {
        FeedKind::Kill => badge(Tone::Loss, Some(Icon::Pause), "Killed"),
        FeedKind::VaultFrozen => badge(Tone::Loss, Some(Icon::Pause), "Frozen"),
        FeedKind::ImageProposed => match counted {
            Some(true) => badge(Tone::Gain, Some(Icon::Check), "Counted"),
            Some(false) => badge(Tone::Neutral, Some(Icon::Clock), "Pending"),
            None => badge(Tone::Neutral, Some(Icon::Clock), "Proposed"),
        },
        FeedKind::ImageRevoked => badge(Tone::Caution, Some(Icon::Concern), "Revoked"),
        FeedKind::VenueSuspended => badge(Tone::Caution, Some(Icon::Concern), "Suspended"),
        FeedKind::VenueRestored => badge(Tone::Gain, Some(Icon::Check), "Restored"),
        FeedKind::StrategyRegistered => badge(Tone::Neutral, None, "Registered"),
        FeedKind::ReviewPublished => badge(Tone::Neutral, None, "Published"),
        FeedKind::ClaimNote => badge(Tone::Caution, Some(Icon::Clock), "Waiting"),
    }
}

/// What an event means for Depositors, where the design says it.
fn meaning(kind: FeedKind) -> Option<&'static str> {
    match kind {
        FeedKind::Kill => {
            Some("Every Depositor can withdraw once the positions are closed and the cash is home.")
        }
        FeedKind::VaultFrozen => {
            Some("You can still withdraw. Withdrawals are paid as the positions are unwound.")
        }
        FeedKind::ImageProposed => Some(
            "Nothing that runs today changes. Once it counts, the image can be used for new work.",
        ),
        _ => None,
    }
}

/// A link inside the app the event points at, if it names one.
fn app_link_for(link: &Option<String>) -> Option<(String, &'static str)> {
    let link = link.as_ref()?;
    link.starts_with("/agents/")
        .then(|| (link.clone(), "Vault page"))
}

/// Whether a reference is a transaction hash (32 bytes of hex).
fn is_transaction(reference: &str) -> bool {
    reference.len() == 66 && reference.starts_with("0x")
}

fn event_card(item: &Rc<FeedItem>, now: Option<PrimitiveDateTime>, timelock: Option<f64>) -> El {
    let this_year = now.map_or(item.at.year(), |now| now.year());
    let counts = counts_at(item, timelock);
    let counted = counts.zip(now).map(|(counts, now)| now >= counts);
    // A network with no public wait counts a proposal at once.
    let instant = counts == Some(item.at);
    let timing = match counts {
        _ if undated(item.at.date()) => "The feed didn’t record when this happened".to_string(),
        Some(_) if instant => format!(
            "Proposed {} · no public wait on this network",
            format::day_time(item.at, this_year)
        ),
        Some(counts) => format!(
            "Proposed {} · {} {}",
            format::day_time(item.at, this_year),
            if counted == Some(true) {
                "counted"
            } else {
                "counts"
            },
            format::day_time(counts, this_year)
        ),
        None => format!("Posted {}", format::day_time(item.at, this_year)),
    };
    let mut card = el("article")
        .class("sh-card")
        .class("st-event")
        .child(
            el("div")
                .class("st-event__chips")
                .child(kind_chip(item.kind))
                .child(state_chip(item, counted)),
        )
        .child(
            el("div")
                .class("st-event__head")
                .child(el("h3").class("st-event__title").text(item.title.clone()))
                .child(
                    el("p")
                        .class("st-event__time")
                        .child(el("time").text(timing)),
                ),
        )
        .child(el("p").class("st-event__detail").text(item.detail.clone()));
    if let Some(counts) = counts.filter(|_| !instant) {
        card = card.child(
            el("ol")
                .class("st-wait")
                .attr("aria-label", "Public wait")
                .child(
                    el("li")
                        .class("st-wait__step")
                        .class_if(counted != Some(true), "st-wait__step--now")
                        .child(
                            el("span")
                                .class("st-wait__row")
                                .child(badge(Tone::Neutral, Some(Icon::Clock), "Pending"))
                                .child(el("span").class("st-wait__note").text("Public wait")),
                        )
                        .child(el("span").class("st-wait__note").text(format!(
                            "{} to {}",
                            format::day_time(item.at, this_year),
                            format::day_time(counts, this_year)
                        ))),
                )
                .child(
                    el("li")
                        .class("st-wait__step")
                        .class_if(counted == Some(true), "st-wait__step--now")
                        .child(
                            el("span")
                                .class("st-wait__row")
                                .child(badge(
                                    if counted == Some(true) {
                                        Tone::Gain
                                    } else {
                                        Tone::Neutral
                                    },
                                    Some(Icon::Check),
                                    "Counted",
                                ))
                                .child(el("span").class("st-wait__note").text("On the allowlist")),
                        )
                        .child(el("span").class("st-wait__note").text(format!(
                            "{} {}",
                            if counted == Some(true) {
                                "Since"
                            } else {
                                "From"
                            },
                            format::day_time(counts, this_year)
                        ))),
                ),
        );
    }
    if let Some(meaning) = meaning(item.kind) {
        card = card.child(
            el("div")
                .class("sh-well")
                .class("st-means")
                .child(
                    el("p")
                        .class("sh-label")
                        .text("What this means for Depositors"),
                )
                .child(el("p").class("st-means__text").text(meaning)),
        );
    }
    let foot = el("div").class("st-event__foot").child(
        el("span")
            .class("st-event__ref")
            .text(if is_transaction(&item.reference) {
                "Transaction "
            } else {
                "Reference "
            })
            .child(hash(
                &item.reference,
                None,
                if is_transaction(&item.reference) {
                    "transaction hash"
                } else {
                    "reference"
                },
            )),
    );
    let foot = match app_link_for(&item.link) {
        Some((href, label)) => foot.child(
            app_link(href)
                .class("st-event__link")
                .text(label)
                .child(icon(Icon::ArrowRight, 16)),
        ),
        None => foot,
    };
    card.child(foot)
}

/// Group the feed by UTC day, newest first.
fn days(feed: &[Rc<FeedItem>], filter: Filter, from: Option<Date>) -> Vec<Day> {
    let mut out: Vec<Day> = Vec::new();
    for item in feed.iter().filter(|item| filter.keeps(item.kind)) {
        let day = item.at.date();
        if from.is_some_and(|from| day < from) {
            continue;
        }
        match out.last_mut() {
            Some((last, items)) if *last == day => items.push(item.clone()),
            _ => out.push((day, vec![item.clone()])),
        }
    }
    out
}

/// The feed writes an event whose time it didn't record as the Unix epoch:
/// it is shown, last, as undated, never as an event from 1970.
fn undated(day: Date) -> bool {
    day.year() <= 1970
}

/// How many days with events show at first, and how many more each
/// "Show earlier events" adds.
const DAYS_PER_PAGE: usize = 7;

/// One day's events.
type Day = (Date, Vec<Rc<FeedItem>>);

/// The days shown after `pages` presses of "Show earlier events", and how
/// many days have events in all.
fn shown_days(feed: &[Rc<FeedItem>], filter: Filter, pages: u32) -> (Vec<Day>, usize) {
    let all = days(feed, filter, None);
    let total = all.len();
    let shown = all
        .into_iter()
        .take(DAYS_PER_PAGE * (pages as usize + 1))
        .collect();
    (shown, total)
}

fn timeline(
    feed: &[Rc<FeedItem>],
    filter: Filter,
    months: u32,
    now: Option<PrimitiveDateTime>,
    timelock: Option<f64>,
    failing: bool,
) -> View {
    let newest = feed.first().map(|item| item.at.date());
    let (groups, _) = shown_days(feed, filter, months);
    if now.is_none() && failing {
        // Nothing has arrived and the API isn't answering: say so, never an
        // endless shimmer.
        return el("div")
            .class("st-offline")
            .attr("role", "status")
            .child(icon(Icon::Concern, 20))
            .child(
                el("p")
                    .child(el("strong").text("Events can’t be shown right now."))
                    .text(" We can’t reach the indexer. We’ll keep trying."),
            )
            .child(on_click(
                Button::new(Kind::Secondary, "Try again").build(),
                || {
                    store::get().sync.retry.replace_with(|n| n.wrapping_add(1));
                },
            ))
            .into();
    }
    if now.is_none() {
        // Nothing has arrived yet: the shape of an event, shimmering.
        return el("div")
            .class("st-loading")
            .attr("aria-busy", "true")
            .attr("aria-label", "Loading events")
            .child(crate::atoms::skeleton::skeleton_block("100%", 180))
            .child(crate::atoms::skeleton::skeleton_block("100%", 180))
            .into();
    }
    if groups.is_empty() {
        return el("p")
            .class("st-none")
            .text(if feed.is_empty() {
                "Nothing has been posted yet."
            } else {
                "No events of this kind in this period."
            })
            .into();
    }
    let this_year = now.map_or_else(|| newest.map_or(2026, |day| day.year()), |now| now.year());
    el("div")
        .class("st-timeline")
        .child(
            el("span")
                .class("st-timeline__rail")
                .attr("aria-hidden", "true"),
        )
        .child(
            el("ol")
                .class("st-days")
                .children(groups.into_iter().map(|(day, items)| {
                    let alarming = items.iter().any(|item| item.kind.is_alarming());
                    el("li")
                        .class("st-day")
                        .child(
                            el("div")
                                .class("st-day__date")
                                .child(el("span").class("st-day__day").text(if undated(day) {
                                    "Undated".to_string()
                                } else {
                                    format::day(day, this_year)
                                }))
                                .child(el("span").class("st-day__weekday").text(if undated(day) {
                                    "Time not recorded"
                                } else {
                                    format::weekday(day)
                                }))
                                .child(
                                    el("span")
                                        .class("st-day__dot")
                                        .class_if(alarming, "st-day__dot--alarm")
                                        .attr("aria-hidden", "true"),
                                ),
                        )
                        .child(
                            el("div")
                                .class("st-day__events")
                                .children(items.iter().map(|item| event_card(item, now, timelock))),
                        )
                })),
        )
        .into()
}

/// The overall line and one tile per thing that can fall behind.
fn system(freshness: Freshness, chains: &[ChainRead], now: Option<PrimitiveDateTime>) -> View {
    let chains_tile = chains_state(chains);
    let chains_behind = chains_tile.as_ref().is_some_and(|(normal, _)| !normal);
    let (ok, headline) = match freshness {
        // The summary never says normal while a tile under it doesn't.
        Freshness::Fresh if chains_behind => (false, "A vault chain is behind"),
        Freshness::Fresh => (true, "All systems normal"),
        Freshness::Loading => (true, "Checking"),
        Freshness::Behind { .. } => (false, "Figures are behind"),
        Freshness::Unreachable { .. } => (false, "We can’t reach the indexer"),
    };
    let checked = match (now, freshness) {
        (Some(now), _) => format!("Checked {}", format::day_time(now, now.year())),
        (None, Freshness::Unreachable { .. }) => "No answer since this page opened".to_string(),
        (None, _) => "Not checked yet".to_string(),
    };
    let indexer_chip = match freshness {
        Freshness::Fresh => badge(Tone::Gain, Some(Icon::Check), "Normal"),
        Freshness::Loading => badge(Tone::Neutral, Some(Icon::Clock), "Checking"),
        Freshness::Behind { .. } => badge(Tone::Caution, Some(Icon::Clock), "Behind"),
        Freshness::Unreachable { .. } => badge(Tone::Loss, Some(Icon::Serious), "Unreachable"),
    };
    let tile = |name: String, chip: El, words: String| {
        el("div")
            .class("st-system__tile")
            .child(
                el("div")
                    .class("st-system__tilehead")
                    .child(el("h3").class("st-system__name").text(name))
                    .child(chip),
            )
            .child(el("p").class("st-system__words").text(words))
    };
    let mut section = el("section")
        .class("sh-card")
        .class("st-system")
        .attr("aria-labelledby", "st-system-title")
        .child(
            el("div")
                .class("st-system__summary")
                .child(
                    el("h2")
                        .attr("id", "st-system-title")
                        .class("st-system__title")
                        .child(
                            el("span")
                                .class("st-system__seal")
                                .class_if(!ok, "st-system__seal--warn")
                                .class_if(freshness == Freshness::Loading, "st-system__seal--wait")
                                .child(icon(
                                    match (ok, freshness) {
                                        (_, Freshness::Loading) => Icon::Clock,
                                        (true, _) => Icon::Check,
                                        (false, _) => Icon::Concern,
                                    },
                                    20,
                                )),
                        )
                        .text(headline),
                )
                .child(el("p").class("st-system__words").text(checked)),
        )
        .child(tile(
            "Indexer".to_string(),
            indexer_chip,
            "Reads the chain so these pages load quickly.".to_string(),
        ));
    if let Some((normal, words)) = chains_tile {
        let chip = if normal {
            badge(Tone::Gain, Some(Icon::Check), "Normal")
        } else {
            badge(Tone::Caution, Some(Icon::Clock), "Behind")
        };
        section = section.child(tile("Vault chains".to_string(), chip, words));
    }
    section
        .child(tile(
            "Web app".to_string(),
            badge(Tone::Gain, Some(Icon::Check), "Normal"),
            "This site.".to_string(),
        ))
        .into()
}

/// The vault chains' tile: whether every chain was read within a minute of
/// the latest, and its words; none when no chain is listed.
fn chains_state(chains: &[ChainRead]) -> Option<(bool, String)> {
    if chains.is_empty() {
        return None;
    }
    let latest = chains.iter().filter_map(|read| read.as_of).max();
    let behind: Vec<&str> = chains
        .iter()
        .filter(|read| match (read.as_of, latest) {
            (Some(at), Some(latest)) => (latest - at).whole_seconds() > 60,
            _ => true,
        })
        .map(|read| read.chain.label())
        .collect();
    let names: Vec<&str> = chains.iter().map(|read| read.chain.label()).collect();
    let words = match latest {
        Some(at) if behind.is_empty() => format!(
            "{}: each read at {}.",
            names.join(", "),
            format::time_short(at)
        ),
        Some(_) => format!("{} behind the others.", behind.join(", ")),
        None => "Not read yet.".to_string(),
    };
    Some((latest.is_some() && behind.is_empty(), words))
}

pub fn page() -> Page {
    let store = store::get();
    let filter = Mutable::new(Filter::All);
    let months = Mutable::new(0u32);

    let feed_now: Vec<Rc<FeedItem>> = store.feed.lock_ref().to_vec();
    let timeline_now = timeline(
        &feed_now,
        filter.get(),
        months.get(),
        store.as_of.get(),
        store.image_timelock_days.get(),
        store.sync.failing.get(),
    );
    let timeline_signal = map_ref! {
        let feed = store.feed.signal_vec_cloned().to_signal_cloned(),
        let filter = filter.signal(),
        let months = months.signal(),
        let now = store.as_of.signal(),
        let timelock = store.image_timelock_days.signal(),
        let failing = store.sync.failing.signal() =>
        Some(timeline(feed, *filter, *months, *now, *timelock, *failing))
    };

    let system_now = system(
        crate::components::freshness::now(),
        &store.chains.get_cloned(),
        store.as_of.get(),
    );
    let system_signal = map_ref! {
        let freshness = crate::components::freshness::signal(),
        let chains = store.chains.signal_cloned(),
        let now = store.as_of.signal() => Some(system(*freshness, chains, *now))
    };

    let counts = |feed: &[Rc<FeedItem>], filter: Filter| {
        feed.iter().filter(|item| filter.keeps(item.kind)).count()
    };
    let filters = el("div")
        .class("st-filters")
        .attr("role", "group")
        .attr("aria-label", "Filter events")
        .children(Filter::ALL.iter().map(|option| {
            let option = *option;
            let pick = filter.clone();
            let now_count = counts(&feed_now, option);
            el("button")
                .attr("type", "button")
                .class("st-filter")
                .attr_from("aria-pressed", &filter, move |current| {
                    Some((*current == option).to_string())
                })
                .text(option.label())
                .child(
                    el("span").class("st-filter__n").text_signal(
                        now_count.to_string(),
                        store
                            .feed
                            .signal_vec_cloned()
                            .to_signal_cloned()
                            .map(move |feed| counts(&feed, option).to_string()),
                    ),
                )
                .hook(move |dom| dom.event(move |_: dominator::events::Click| pick.set_neq(option)))
        }));

    let pending = |feed: &[Rc<FeedItem>],
                   now: Option<PrimitiveDateTime>,
                   timelock: Option<f64>|
     -> View {
        // Nothing read yet: nothing to say about what's waiting.
        if now.is_none() {
            return View::Empty;
        }
        let waiting = feed
            .iter()
            .filter(|item| {
                counts_at(item, timelock)
                    .zip(now)
                    .is_some_and(|(counts, now)| now < counts)
            })
            .count();
        el("div")
            .class("st-pending")
            .child(icon(Icon::Clock, 20))
            .child(match waiting {
                0 => el("p")
                    .child(el("strong").text("Nothing is waiting to count."))
                    .child(el("span").class("sh-muted").text(" A proposed change shows here with a Pending chip and the time it counts, for its whole two-day wait.")),
                n => el("p")
                    .child(el("strong").text(if n == 1 {
                        "1 change is waiting to count.".to_string()
                    } else {
                        format!("{n} changes are waiting to count.")
                    }))
                    .child(el("span").class("sh-muted").text(" Each shows a Pending chip and the time it counts, for its whole public wait.")),
            })
            .into()
    };
    let pending_now = pending(
        &feed_now,
        store.as_of.get(),
        store.image_timelock_days.get(),
    );
    let pending_signal = map_ref! {
        let feed = store.feed.signal_vec_cloned().to_signal_cloned(),
        let now = store.as_of.signal(),
        let timelock = store.image_timelock_days.signal() => Some(pending(feed, *now, *timelock))
    };

    let more_now = more_line(&feed_now, filter.get(), months.get());
    let more_signal = {
        let months = months.clone();
        map_ref! {
            let feed = store.feed.signal_vec_cloned().to_signal_cloned(),
            let filter = filter.signal(),
            let shown = months.signal() => (feed.clone(), *filter, *shown)
        }
        .map(move |(feed, filter, shown)| more_line(&feed, filter, shown))
    };
    let older = months.clone();

    let events = el("section")
        .class("st-events")
        .attr("aria-labelledby", "st-events-title")
        .child(
            el("div")
                .class("st-events__head")
                .child(
                    el("h2")
                        .attr("id", "st-events-title")
                        .class("st-h2")
                        .text("Events"),
                )
                .child(
                    el("p")
                        .class("sh-muted")
                        .class("st-small")
                        .text("Newest first · times in UTC"),
                ),
        )
        .child(filters)
        .child(
            el("div")
                .class("sh-contents")
                .child_signal(Some(pending_now), pending_signal),
        )
        .child(
            el("div")
                .class("sh-contents")
                .child_signal(Some(timeline_now), timeline_signal),
        )
        .child(el("div").class("st-more").child_signal(
            more_view(more_now, &months),
            more_signal.map(move |line| more_view(line, &older)),
        ));

    let aside = el("aside")
        .class("st-aside")
        .child(
            el("section")
                .class("sh-card")
                .class("st-why")
                .attr("aria-labelledby", "st-why-title")
                .child(el("h2").attr("id", "st-why-title").class("st-h3").text("Why this page exists"))
                .child(el("p").class("sh-muted").text("Shonin keeps one admin key. Its powers are narrow, and everything it does is posted here as it happens."))
                .child(
                    el("ul")
                        .class("st-why__list")
                        .child(why(Icon::Machine, "Propose or revoke", " machine images, operators, drivers and attestation checkers. A change only counts after a public wait, so you see it coming."))
                        .child(why(Icon::Pause, "Kill a vault,", " typically one whose health report shows it failing. The agent halts, its positions close, and the cash goes home."))
                        .child(why(Icon::Lock, "Nothing else.", " It can’t move money anywhere but back to Depositors.")),
                )
                .child(el("p").class("st-why__note").text("Freezes aren’t ours. Anyone can freeze a vault when a booked withdrawal has gone unpaid for seven days."))
                .child(
                    el("a")
                        .class("sh-link")
                        .attr("href", format!("{DOCS}verify/"))
                        .text("Watch the allowlist")
                        .child(icon(Icon::External, 14)),
                ),
        )
        .child(
            el("p")
                .class("st-check")
                .child(icon(Icon::Info, 16))
                .child(el("span").text("Every event carries its on-chain reference, so you can check it without trusting this page.")),
        );

    let body = el("div")
        .class("st")
        .child(
            el("div")
                .class("st-head")
                .child(el("h1").class("st-title").text("Status"))
                .child(
                    el("p")
                        .class("st-lede")
                        .text("Every kill, freeze and allowlist change, as it happens."),
                ),
        )
        .child(
            el("div")
                .class("sh-contents")
                .child_signal(Some(system_now), system_signal),
        )
        .child(el("div").class("st-grid").child(events).child(aside));

    Page {
        top: None,
        body: body.into(),
    }
}

fn why(glyph: Icon, lead: &'static str, rest: &'static str) -> El {
    el("li")
        .class("st-why__item")
        .child(icon(glyph, 20))
        .child(el("p").child(el("strong").text(lead)).text(rest))
}

/// "That's everything since Sep 1." and, when there are older events, a
/// button to show more of them.
fn more_line(
    feed: &[Rc<FeedItem>],
    filter: Filter,
    pages: u32,
) -> Option<(String, Option<String>)> {
    let newest = feed.first()?.at.date();
    let (shown, total) = shown_days(feed, filter, pages);
    let oldest = shown
        .iter()
        .rev()
        .map(|(day, _)| *day)
        .find(|day| !undated(*day))?;
    let words = format!(
        "That’s everything since {}.",
        format::day(oldest, newest.year())
    );
    let button = (shown.len() < total).then(|| "Show earlier events".to_string());
    Some((words, button))
}

fn more_view(line: Option<(String, Option<String>)>, older: &Mutable<u32>) -> Option<View> {
    let (words, button) = line?;
    let row = el("div")
        .class("st-more__row")
        .child(el("p").class("sh-muted").text(words));
    Some(match button {
        Some(label) => {
            let older = older.clone();
            row.child(on_click(
                Button::new(Kind::Secondary, label)
                    .size(Size::Small)
                    .build(),
                move || {
                    older.replace_with(|months| *months + 1);
                },
            ))
            .into()
        }
        None => row.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(chain: shared::Chain, at: PrimitiveDateTime) -> ChainRead {
        ChainRead {
            chain,
            key: String::new(),
            block: 1,
            as_of: Some(at),
        }
    }

    #[test]
    fn the_summary_never_says_normal_while_a_vault_chain_is_behind() {
        let now = time::macros::datetime!(2026-10-12 22:00);
        let behind = [
            read(shared::Chain::Base, now),
            read(shared::Chain::Solana, now - Duration::minutes(5)),
        ];
        let html = system(Freshness::Fresh, &behind, Some(now)).html();
        assert!(!html.contains("All systems normal"), "{html}");
        assert!(html.contains("A vault chain is behind"), "{html}");
        let level = [
            read(shared::Chain::Base, now),
            read(shared::Chain::Solana, now),
        ];
        let html = system(Freshness::Fresh, &level, Some(now)).html();
        assert!(html.contains("All systems normal"), "{html}");
    }
}
