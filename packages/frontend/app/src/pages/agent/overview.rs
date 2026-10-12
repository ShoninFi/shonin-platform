//! The agent's overview: its record in four figures, the share-price chart,
//! the review in brief, what can still go wrong, where it trades, how you'd
//! get out, its fees and its latest activity; beside them the Exit panel,
//! the tank and the verified build.

use std::rc::Rc;

use shared::{Area, Custody, ExitClass, Leg, Rating, Strategy, VenueStatus};
use ui::icons::Icon;

use super::chart::{ChartState, chart};
use super::parts::{card, card_with_aside, more, more_in_app};
use super::{facts, join_and, live, side};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::delta::{missing_figure, tone_class};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::prose::prose;
use crate::atoms::rating::rating;
use crate::format;
use crate::model::rating_word;
use crate::route::DOCS_DEPOSITORS;
use crate::store::{self, Agent};
use crate::view::{El, View, el, svg, text};

pub fn body(agent: &Rc<Agent>) -> El {
    let state = ChartState::new(agent);
    el("div")
        .class("ag-layout")
        .class("ag-layout--overview")
        .child(
            el("div")
                .class("ag-main")
                .child(stats(agent))
                .child(chart(agent, &state))
                .child(live(agent, review_card))
                .child(live(agent, risks))
                .child(live(agent, where_it_trades))
                .child(live(agent, way_out))
                .child(live(agent, fees))
                .child(live(agent, recent)),
        )
        .child(
            el("aside")
                .class("ag-side")
                .attr("aria-label", "Deposit, withdraw and running costs")
                .child(side::exit_slot(agent))
                .child(side::tank(agent))
                .child(side::verified_build(agent)),
        )
}

// ---- The record in four figures ------------------------------------------

fn stat(label: &'static str, figure: View, period: String) -> El {
    el("div")
        .class("ag-stat")
        .child(el("dt").class("sh-label").text(label))
        .child(el("dd").class("ag-stat__value").child(figure))
        .child(el("dd").class("ag-stat__period").text(period))
}

fn ret_figure(value: Option<f64>, missing: &'static str) -> View {
    match value {
        Some(value) => el("span")
            .class_if(!tone_class(value).is_empty(), tone_class(value))
            .text(format::ret(value))
            .into(),
        None => missing_figure(missing).into(),
    }
}

fn stats(agent: &Rc<Agent>) -> El {
    let as_of = store::get().as_of.clone();
    // The list is drawn inside the live binding, so its groups are its own
    // children (a binding's wrappers inside a `dl` aren't allowed there).
    el("div")
        .class("sh-card")
        .class("ag-stats")
        .class("ag-o-stats")
        .child(live(agent, move |strategy| {
            let figures = crate::model::figures(strategy);
            let this_year = as_of.get().map(|at| at.year()).unwrap_or(2026);
            let stale = strategy
                .stale_as_of
                .map(|at| format!(" · as of {}", format::day(at.date(), this_year)))
                .unwrap_or_default();
            el("dl")
                .class("sh-contents")
                .attr("aria-label", "Record")
                .child(View::Many(vec![
                    stat(
                        "30 days",
                        ret_figure(figures.return_30d, "Needs 30 days"),
                        match (figures.return_30d, facts::period_30d(strategy, this_year)) {
                            (Some(_), Some(period)) => format!("{period}{stale}"),
                            _ => "Needs 30 days".to_string(),
                        },
                    )
                    .into(),
                    stat(
                        "Since opening",
                        ret_figure(figures.since_opening, "No record yet"),
                        if figures.since_opening.is_some() {
                            format!("Return over {}{stale}", format::days(figures.days))
                        } else {
                            "No record yet".to_string()
                        },
                    )
                    .into(),
                    stat(
                        "Deepest drop",
                        ret_figure(figures.deepest_drop, "No record yet"),
                        if figures.deepest_drop.is_some() {
                            "From a high, since opening".to_string()
                        } else {
                            "No record yet".to_string()
                        },
                    )
                    .into(),
                    stat(
                        "In vault",
                        text(format::money_whole(strategy.vault.value)),
                        format!(
                            "{} on {}{stale}",
                            strategy.vault.asset,
                            strategy.vault.chain.label()
                        ),
                    )
                    .into(),
                ]))
                .into()
        }))
}

// ---- The review in brief --------------------------------------------------

/// The review in brief: the report card's six ratings and its headline
/// finding. The summary, every finding and the claims live on the Review tab.
fn review_card(strategy: &Strategy) -> View {
    let Some(review) = &strategy.review else {
        return card("ag-review-title", "The review", Some(Icon::Review))
            .class("ag-o-review")
            .child(
                el("p")
                    .class("ag-muted")
                    .text("No review is on the chain for this agent yet."),
            )
            .into();
    };
    let ratings = el("ul")
        .class("ag-ratings")
        .attr("aria-label", "Ratings in six areas")
        .children(Area::ALL.into_iter().map(|area| {
            el("li")
                .class_if(
                    review.rating(area) != Rating::LooksFine,
                    "ag-ratings__item--flag",
                )
                .child(el("span").class("ag-ratings__area").text(area.label()))
                .child(rating(review.rating(area)))
        }));
    let refused = review.refused.then(|| {
        notice(
            NoticeTone::Danger,
            View::Many(vec![
                el("strong").text("Refused.").into(),
                text(" The Reviewer judged this code written to do harm, so it was never built."),
            ]),
        )
        .into()
    });
    card_with_aside(
        "ag-review-title",
        "The review",
        Some(Icon::Review),
        el("p")
            .class("ag-muted")
            .text(format!("Reviewed {}", facts::full_day(review.reviewed))),
    )
    .class("ag-o-review")
    .child(View::from(refused))
    .child(ratings)
    .child(headline(review))
    .child(
        el("div")
            .class("ag-card__foot")
            .child(
                el("div")
                    .class("ag-card__links")
                    .child(more_in_app(
                        format!("/agents/{}/review", strategy.slug),
                        "Read the full review",
                    ))
                    .child(side::source_line(strategy)),
            )
            .child(
                el("p").class("ag-note").text(
                    "A review isn't a prediction. It never rates how much an agent might make.",
                ),
            ),
    )
    .into()
}

/// How much an area's rating says, for picking what leads.
fn rating_weight(rated: Rating) -> u8 {
    match rated {
        Rating::SeriousConcerns => 3,
        Rating::SomeConcerns => 2,
        Rating::CouldntTell => 1,
        Rating::LooksFine => 0,
    }
}

/// The finding that leads the review: the most severe one, from the worst
/// rated area on a tie, the first of those.
pub fn headline_finding(review: &shared::Review) -> Option<(Area, &shared::Finding)> {
    let mut best: Option<(Area, &shared::Finding)> = None;
    for area in Area::ALL {
        for finding in review.findings_in(area) {
            let better = match best {
                None => true,
                Some((held, lead)) => {
                    (finding.severity, rating_weight(review.rating(area)))
                        > (lead.severity, rating_weight(review.rating(held)))
                }
            };
            if better {
                best = Some((area, finding));
            }
        }
    }
    best
}

/// The headline finding as one notice, or the areas rated with concerns
/// when there is no finding to quote, or nothing when all six look fine.
fn headline(review: &shared::Review) -> View {
    let block = |body: View, tone: NoticeTone, glyph: Icon| -> View {
        el("div")
            .class("ag-headline")
            .child(el("h3").class("sh-label").text("Headline finding"))
            .child(crate::atoms::notice::notice_with(tone, glyph, body))
            .into()
    };
    if let Some((area, finding)) = headline_finding(review) {
        let (tone, glyph) = tone_of(finding.severity);
        // A worse-rated area with no finding to quote still gets named.
        let worse: Vec<(Area, Rating)> = facts::concerns(review)
            .into_iter()
            .filter(|(_, rated)| rating_weight(*rated) > rating_weight(review.rating(area)))
            .collect();
        let also = worse.first().map(|(_, worst)| {
            let named: Vec<Area> = worse
                .iter()
                .filter(|(_, rated)| rated == worst)
                .map(|(area, _)| *area)
                .collect();
            let names: Vec<&str> = named.iter().map(|area| area.label()).collect();
            el("p")
                .class("ag-note")
                .text(format!(
                    "Also rated {}: {}.{}",
                    rating_word(*worst).to_lowercase(),
                    join_and(&names),
                    unlisted_words(review, &named)
                ))
                .into()
        });
        let outrank = facts::findings_outrank_rating(review, area)
            .map(|severity| format!(" {}", facts::outrank_line(severity)))
            .unwrap_or_default();
        return View::Many(vec![
            block(
                View::Many(vec![
                    el("strong")
                        .text(format!(
                            "{}: {}.{outrank}",
                            area.label(),
                            rating_word(review.rating(area)).to_lowercase()
                        ))
                        .into(),
                    text(" "),
                    View::Many(prose(&format!(
                        "{}.",
                        finding.summary.trim_end_matches('.')
                    ))),
                ]),
                tone,
                glyph,
            ),
            View::from(also),
        ]);
    }
    let concerns = facts::concerns(review);
    let Some((_, worst)) = concerns.first().copied() else {
        return View::Empty;
    };
    let named: Vec<Area> = concerns
        .iter()
        .filter(|(_, rated)| *rated == worst)
        .map(|(area, _)| *area)
        .collect();
    let areas: Vec<&str> = named.iter().map(|area| area.label()).collect();
    let (tone, glyph) = match worst {
        Rating::SeriousConcerns => (NoticeTone::Danger, Icon::Serious),
        _ => (NoticeTone::Caution, Icon::Concern),
    };
    block(
        View::Many(vec![
            el("strong")
                .text(format!(
                    "{}: {}.",
                    join_and(&areas),
                    rating_word(worst).to_lowercase()
                ))
                .into(),
            // With no finding anywhere, every area named here has none.
            text(format!(
                "{} Read the full review.",
                unlisted_words(review, &named)
            )),
        ]),
        tone,
        glyph,
    )
}

/// For areas rated with concerns: which of them the Reviewer listed no
/// finding for, said plainly so the overview never promises findings the
/// review tab doesn't hold. Empty when every one has a finding.
pub fn unlisted_words(review: &shared::Review, areas: &[Area]) -> String {
    let quiet: Vec<&str> = areas
        .iter()
        .filter(|area| facts::rated_without_finding(review, **area))
        .map(|area| area.label())
        .collect();
    if quiet.is_empty() {
        String::new()
    } else if quiet.len() == areas.len() {
        match areas.len() {
            1 => " The Reviewer listed no finding for it.".to_string(),
            _ => " The Reviewer listed no finding for any of them.".to_string(),
        }
    } else {
        format!(" The Reviewer listed no finding for {}.", join_and(&quiet))
    }
}

fn tone_of(severity: shared::Severity) -> (NoticeTone, Icon) {
    match severity {
        shared::Severity::Serious => (NoticeTone::Danger, Icon::Serious),
        shared::Severity::Concern => (NoticeTone::Caution, Icon::Concern),
        shared::Severity::Note => (NoticeTone::Info, Icon::Info),
    }
}

// ---- What can still go wrong ---------------------------------------------

fn risks(strategy: &Strategy) -> View {
    let venues: Vec<&str> = {
        let mut names: Vec<&str> = strategy.legs.iter().map(|leg| leg.venue.as_str()).collect();
        names.dedup();
        names
    };
    let mut items: Vec<(String, String)> = vec![
        (
            "It can lose money.".into(),
            "Nothing here stops that. A bad trade loses real money.".into(),
        ),
        (
            "It can stop if its tank runs dry.".into(),
            "Its positions would sit unmanaged until someone tops it up. You could still leave."
                .into(),
        ),
        (
            "It can be retired, with a week's notice.".into(),
            "You get a week to leave. After that, positions close at market, which can lock in a loss."
                .into(),
        ),
        (
            "It can be killed.".into(),
            "Our admin key can kill a failing vault. The cash comes home, but closing at market can lock in a loss."
                .into(),
        ),
    ];
    match venues.as_slice() {
        [] => {}
        [one] => items.push((
            format!("{one} can fail."),
            format!("All of this vault's money that isn't at home sits on {one}. It's only as safe as that venue."),
        )),
        many => items.push((
            "Its venues can fail.".into(),
            format!(
                "This vault's money sits on {}. It's only as safe as each of them.",
                join_and(many)
            ),
        )),
    }
    if !strategy.bridges.is_empty() {
        items.push((
            "A bridge can fail.".into(),
            format!(
                "Money moves between chains through {}. It's only as safe as the bridge.",
                join_and(
                    &bridge_names(strategy)
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                )
            ),
        ));
    }
    items.push((
        "Our own code can have bugs.".into(),
        "The contracts and machine images are open source, but a bug in them could still lose money."
            .into(),
    ));
    el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-o-risks")
        .attr("aria-labelledby", "ag-risk-title")
        .child(
            el("div")
                .class("ag-card__intro")
                .child(
                    el("h2")
                        .class("ag-card__title")
                        .attr("id", "ag-risk-title")
                        .child(icon(Icon::Concern, 22))
                        .text("What can still go wrong"),
                )
                .child(
                    el("p")
                        .class("ag-muted")
                        .text("For this agent, said out loud. A track record isn't a promise."),
                ),
        )
        .child(
            el("ul")
                .class("ag-risks")
                .children(items.into_iter().map(|(title, body)| {
                    el("li")
                        .child(el("p").class("ag-risks__title").text(title))
                        .child(el("p").class("ag-risks__body").text(body))
                })),
        )
        .child(more(
            format!("{DOCS_DEPOSITORS}#what-can-still-go-wrong"),
            "Read the full list in the docs",
        ))
        .into()
}

// ---- Where it trades ------------------------------------------------------

/// The bridges by name, as a sentence writes them: Across.
fn bridge_names(strategy: &Strategy) -> Vec<String> {
    strategy
        .bridges
        .iter()
        .map(|name| facts::sentence(name))
        .collect()
}

/// The markets a leg's key may hold, from what the venue says it can do.
fn markets(leg: &Leg) -> Option<String> {
    leg.can
        .iter()
        .find_map(|can| can.strip_prefix("hold positions in "))
        .map(str::to_string)
}

fn tile(name: &str) -> El {
    el("span")
        .class("ag-tile")
        .class("ag-tile--m")
        .attr("aria-hidden", "true")
        .text(
            name.chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string(),
        )
}

fn leg_row(leg: &Leg, drivers: &[String]) -> El {
    let custody = match leg.custody {
        Custody::VenueEnforced => "Venue-enforced",
        Custody::ChainEnforced => "Chain-enforced",
    };
    el("div")
        .class("ag-venue")
        .child(
            el("dl")
                .class("ag-venue__facts")
                .child(
                    el("div")
                        .child(el("dt").class("sh-label").text("Venue"))
                        .child(
                            el("dd")
                                .class("ag-venue__name")
                                .child(tile(&leg.venue))
                                .text(leg.venue.clone()),
                        ),
                )
                .child(
                    el("div")
                        .child(el("dt").class("sh-label").text("Trades"))
                        .child(
                            el("dd")
                                .class("ag-venue__strong")
                                .text(markets(leg).unwrap_or_else(|| format::MISSING.to_string())),
                        ),
                )
                .child(
                    el("div")
                        .child(el("dt").class("sh-label").text(if drivers.len() == 1 { "Driver" } else { "Drivers" }))
                        .child(el("dd").class("ag-venue__drivers").children(
                            drivers.iter().map(|driver| el("code").class("ag-code").text(driver.clone())),
                        )),
                )
                .child(
                    el("div")
                        .child(el("dt").class("sh-label").text("Custody"))
                        .child(el("dd").child(badge(Tone::Neutral, Some(Icon::Lock), custody))),
                ),
        )
        .child(View::from((leg.status == VenueStatus::Suspended).then(|| {
            notice(
                NoticeTone::Caution,
                View::Many(vec![
                    el("strong").text(format!("{} is suspended.", leg.venue)).into(),
                    text(format!(
                        " {}",
                        leg.note
                            .clone()
                            .unwrap_or_else(|| "The agent can't trade there until it's back.".to_string())
                    )),
                ]),
            )
            .class("sh-notice--compact")
            .into()
        })))
        .child(
            el("p")
                .class("ag-line")
                .child(icon(Icon::Lock, 18))
                .child(el("span").text(format!(
                    "The vault contract is the master of its own account on {}, and {} holds the line. Its key can {}, and nothing else.",
                    leg.venue,
                    leg.custody.enforced_by(),
                    join_and(&leg.can.iter().map(String::as_str).collect::<Vec<_>>())
                ))),
        )
}

fn where_it_trades(strategy: &Strategy) -> View {
    let chain = strategy.vault.chain.label();
    let bridges = if strategy.bridges.is_empty() {
        View::Many(vec![
            el("strong").text("Bridges: none.").into(),
            text(format!(" Your money never leaves {chain}.")),
        ])
    } else {
        View::Many(vec![
            el("strong")
                .text(format!(
                    "Bridges: {}.",
                    join_and(
                        &bridge_names(strategy)
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                    )
                ))
                .into(),
            text(" Money only moves between the vault and its own stations."),
        ])
    };
    card("ag-where-title", "Where it trades", None)
        .class("ag-o-where")
        .children(strategy.legs.iter().map(|leg| leg_row(leg, &strategy.drivers)))
        .child(View::from(strategy.legs.is_empty().then(|| {
            el("p").class("ag-muted").text("No venue yet: the vault holds only cash.").into()
        })))
        .child(
            el("p")
                .class("ag-line")
                .child(icon(Icon::Bridge, 18))
                .child(el("span").child(bridges)),
        )
        .child(
            el("p")
                .class("ag-line")
                .child(icon(Icon::Info, 18))
                .child(el("span").text(
                    "Its drivers were fixed when the vault was created. If a venue changes how it works or stops answering, money held there can stay inside that venue until it's brought home.",
                )),
        )
        .into()
}

// ---- How you'd get out ----------------------------------------------------

fn way_out(strategy: &Strategy) -> View {
    let vault = &strategy.vault;
    let served = matches!(vault.exit_class, ExitClass::Served { .. });
    let share = vault.idle_share().clamp(0.0, 1.0);
    let floor = f64::from(vault.idle_floor_bps) / 10_000.0;
    let pct = |fraction: f64| format!("{:.2}%", fraction * 100.0);
    let gauge = svg("svg")
        .class("ag-cash__gauge")
        .attr("role", "img")
        .attr(
            "aria-label",
            format!(
                "Cash at home is {} of the vault. The idle floor is {}.",
                format::number(share * 100.0, 1) + "%",
                format::bps(vault.idle_floor_bps)
            ),
        )
        .attr("width", "100%")
        .attr("height", "40")
        .child(
            svg("rect")
                .class("ag-cash__track")
                .attr("x", "0")
                .attr("y", "4")
                .attr("width", "100%")
                .attr("height", "10")
                .attr("rx", "5"),
        )
        .child(
            svg("rect")
                .class("ag-cash__fill")
                .attr("x", "0")
                .attr("y", "4")
                .attr("width", pct(share.max(0.004)))
                .attr("height", "10")
                .attr("rx", "5"),
        )
        .child(View::from((vault.idle_floor_bps > 0).then(|| {
            View::Many(vec![
                svg("line")
                    .class("ag-cash__floor")
                    .attr("x1", pct(floor))
                    .attr("x2", pct(floor))
                    .attr("y1", "0")
                    .attr("y2", "18")
                    .into(),
                svg("text")
                    .class("ag-cash__floor-label")
                    .attr("x", pct(floor))
                    .attr("y", "34")
                    .attr("text-anchor", if floor > 0.5 { "end" } else { "start" })
                    .text(format!("Idle floor {}", format::bps(vault.idle_floor_bps)))
                    .into(),
            ])
        })))
        .child(
            svg("text")
                .class("ag-cash__whole")
                .attr("x", "100%")
                .attr("y", "34")
                .attr("text-anchor", "end")
                .text(format!("Whole vault {}", format::money_whole(vault.value))),
        );
    let pending = vault.pending_floor.as_ref().map(|pending| {
        el("p")
            .class("ag-note")
            .text(format!(
                "The Author has lowered the idle floor to {}, from {}.",
                format::bps(pending.bps),
                facts::full_day(pending.from.date())
            ))
            .into()
    });
    let steps = served.then(|| {
        el("ol")
            .class("ag-steps")
            .child(
                el("li")
                    .child(el("span").class("sh-label").text("Booked"))
                    .child(el("span").text("A claim is made")),
            )
            .child(
                el("li")
                    .child(el("span").class("sh-label").text("Unwound"))
                    .child(el("span").text("Your share closed on each chain")),
            )
            .child(
                el("li")
                    .class("ag-steps__done")
                    .child(el("span").class("sh-label").text("Paid"))
                    .child(el("span").text("To your wallet")),
            )
            .into()
    });
    card_with_aside(
        "ag-out-title",
        "How you'd get out",
        None,
        badge(
            Tone::Neutral,
            Some(Icon::Exit),
            if served { "Served exit" } else { "Direct exit" },
        ),
    )
    .class("ag-o-out")
    .child(el("p").class("ag-body").text(if served {
        "This vault trades on other chains, so a booked withdrawal needs an unwinder to finish it. A withdrawal its cash covers is paid by the contract alone."
    } else {
        "Its contracts can always finish a withdrawal by themselves. Even if everything we run stopped, you could withdraw straight from the contract."
    }))
    .child(
        el("div")
            .class("ag-cash")
            .child(
                el("div")
                    .class("ag-cash__top")
                    .child(
                        el("p")
                            .class("ag-cash__figure")
                            .child(el("span").class("sh-label").text("Cash at home"))
                            .child(el("span").class("ag-cash__value").text(format::money_whole(vault.idle))),
                    )
                    .child(el("p").class("ag-note").text(format!(
                        "{} of the vault, in the contract itself",
                        format::number(share * 100.0, 1) + "%"
                    ))),
            )
            .child(gauge)
            .child(el("p").class("ag-cash__text").text(format!(
                "The agent must keep at least {} of the vault at home. Withdrawals up to about {} right now are paid from cash within seconds.",
                format::bps(vault.idle_floor_bps),
                format::money_whole(vault.idle)
            )))
            .child(View::from(pending)),
    )
    .child(View::from(steps))
    .child(
        el("p")
            .class("ag-line")
            .child(icon(Icon::Clock, 18))
            .child(el("span").text(if served {
                format!(
                    "If there isn't enough cash at home, your withdrawal is booked and an unwinder finishes it. If one isn't paid within {}, anyone can freeze the vault so it only winds down.",
                    format::days(vault.freeze_after_days)
                )
            } else {
                "If there isn't enough cash at home, your withdrawal is booked and anyone can push it through.".to_string()
            })),
    )
    .child(more(DOCS_DEPOSITORS, "How withdrawals work"))
    .into()
}

// ---- Fees ------------------------------------------------------------------

fn fees(strategy: &Strategy) -> View {
    let fees = strategy.vault.fees;
    let row = |name: &'static str, figure: String, note: String| {
        el("div").child(el("dt").text(name)).child(
            el("dd")
                .child(el("span").class("ag-fees__figure").text(figure))
                .child(el("span").class("ag-fees__note").text(note)),
        )
    };
    card("ag-fees-title", "Fees", None)
        .class("ag-o-fees")
        .child(
            el("dl")
                .class("ag-fees")
                .child(row(
                    "Profit fee",
                    format!(
                        "{} of new profit",
                        format::bps(fees.author_bps + fees.platform_bps)
                    ),
                    format!(
                        "Only above the high-water mark, {}. {} to the Author, {} to Shonin",
                        format::price(strategy.vault.high_water_mark),
                        format::bps(fees.author_bps),
                        format::bps(fees.platform_bps)
                    ),
                ))
                .child(row(
                    "Holding fee",
                    format!("{} a year", format::bps(fees.holding_bps)),
                    "Taken weekly, as shares".to_string(),
                ))
                .child(row(
                    "Deposit and withdrawal",
                    format!("{} each way", format::bps(fees.transaction_bps)),
                    "Stays in the vault".to_string(),
                )),
        )
        .child(
            el("p")
                .class("ag-fees__fixed")
                .child(icon(Icon::Lock, 16))
                .text("This vault's schedule is fixed and never changes."),
        )
        .into()
}

// ---- Recent activity ------------------------------------------------------

/// One event as a row: when, what, detail, amount.
pub struct Row {
    pub day: String,
    pub time: String,
    pub what: &'static str,
    pub detail: String,
    pub amount: Option<String>,
    pub reference: String,
}

pub fn event_rows(strategy: &Strategy, this_year: i32) -> Vec<Row> {
    let mut events: Vec<&shared::VaultEvent> = strategy.events.iter().collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.at));
    events
        .into_iter()
        .map(|event| Row {
            day: if facts::is_unknown(event.at) {
                format::MISSING.to_string()
            } else {
                format::day(event.at.date(), this_year)
            },
            time: if facts::is_unknown(event.at) {
                String::new()
            } else {
                format::time_short(event.at)
            },
            what: facts::event_word(event.kind),
            detail: facts::sentence(&event.summary),
            amount: facts::event_amount(event.kind, event.amount),
            reference: event.reference.clone(),
        })
        .collect()
}

fn recent(strategy: &Strategy) -> View {
    let this_year = store::get().as_of.get().map(|at| at.year()).unwrap_or(2026);
    let rows: Vec<Row> = event_rows(strategy, this_year)
        .into_iter()
        .take(4)
        .collect();
    let all = format!("/agents/{}/activity", strategy.slug);
    let head = el("div")
        .class("ag-card__head")
        .child(
            el("h2")
                .class("ag-card__title")
                .attr("id", "ag-recent-title")
                .text("Recent activity"),
        )
        .child(more_in_app(all, "See all activity"));
    let section = el("section")
        .class("sh-card")
        .class("ag-card")
        .class("ag-recent")
        .class("ag-o-activity")
        .attr("aria-labelledby", "ag-recent-title")
        .child(head);
    if rows.is_empty() {
        return section
            .child(el("p").class("ag-muted").text("Nothing on the record yet."))
            .into();
    }
    let amount = |row: &Row| {
        row.amount
            .clone()
            .unwrap_or_else(|| format::MISSING.to_string())
    };
    let table = el("table")
        .class("sh-table")
        .class("ag-recent__table")
        .child(
            el("caption")
                .class("sh-vh")
                .text("The vault's latest events"),
        )
        .child(
            el("thead").child(
                el("tr")
                    .child(el("th").attr("scope", "col").text("When"))
                    .child(el("th").attr("scope", "col").text("What"))
                    .child(el("th").attr("scope", "col").text("Detail"))
                    .child(el("th").attr("scope", "col").class("sh-r").text("Amount")),
            ),
        )
        .child(el("tbody").children(rows.iter().map(|row| {
            el("tr")
                .class("sh-row")
                .child(
                    el("td")
                        .class("ag-when")
                        .child(el("span").class("ag-when__day").text(row.day.clone()))
                        .child(el("span").class("ag-when__time").text(row.time.clone())),
                )
                .child(el("td").class("ag-what").text(row.what))
                .child(el("td").class("ag-detail").text(row.detail.clone()))
                .child(el("td").class("sh-r").class("ag-amount").text(amount(row)))
        })));
    let list =
        el("ul")
            .class("ag-recent__list")
            .children(rows.iter().map(|row| {
                el("li")
                    .child(
                        el("span")
                            .class("ag-recent__text")
                            .child(el("span").class("ag-what").text(row.what))
                            .child(el("span").class("ag-detail").text(row.detail.clone()))
                            .child(el("span").class("ag-when__time").text(
                                if row.time.is_empty() {
                                    row.day.clone()
                                } else {
                                    format!("{} · {}", row.day, row.time)
                                },
                            )),
                    )
                    .child(el("span").class("ag-amount").text(amount(row)))
            }));
    section.child(table).child(list).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{Finding, Review, Severity};

    fn review() -> Review {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../../../tests/snapshot.json"))
                .expect("the fixture decodes");
        let mut review = snapshot
            .strategies
            .into_iter()
            .find_map(|strategy| strategy.review)
            .expect("the fixture has a review");
        review.ratings = [Rating::LooksFine; 6];
        review.findings.clear();
        review
    }

    fn finding(area: Area, severity: Severity) -> Finding {
        Finding {
            area,
            severity,
            summary: "The peak starts at `zero`".into(),
            detail: "d".into(),
            file: None,
            line: None,
        }
    }

    #[test]
    fn the_brief_never_promises_findings_for_an_area_rated_with_none() {
        let mut review = review();
        review.ratings[Area::OutsideInfluence.index()] = Rating::SeriousConcerns;
        let html = headline(&review).html();
        assert!(
            html.contains("Outside influence: serious concerns."),
            "{html}"
        );
        assert!(
            html.contains("The Reviewer listed no finding for it."),
            "{html}"
        );
        assert!(!html.contains("what the Reviewer found"), "{html}");
        // With a finding elsewhere, the unlisted area is still named honestly.
        review.ratings[Area::Reliability.index()] = Rating::SomeConcerns;
        review
            .findings
            .push(finding(Area::Reliability, Severity::Concern));
        let html = headline(&review).html();
        assert!(
            html.contains("Also rated serious concerns: Outside influence. The Reviewer listed no finding for it."),
            "{html}"
        );
        assert!(
            html.contains("<code class=\"sh-code\">zero</code>"),
            "{html}"
        );
    }

    #[test]
    fn the_brief_says_when_the_headline_finding_outranks_its_area_rating() {
        let mut review = review();
        review.ratings[Area::Reliability.index()] = Rating::SomeConcerns;
        review
            .findings
            .push(finding(Area::Reliability, Severity::Serious));
        let html = headline(&review).html();
        assert!(
            html.contains("Reliability: some concerns. Its findings include serious concerns."),
            "{html}"
        );
    }
}
