//! `/portfolio` and `/portfolio/claims/<slug>-<id>` (Portfolio-*, Claim-Desktop).
//!
//! Everything a wallet holds is read from each vault's contract on its own
//! chain: shares, the share price, units and held payouts, waiting requests
//! and claims. What was paid in is the API's reading of the vault's own logs.
//! Each row's value is rounded down to the cent once, and the totals are the
//! sums of those cents, so they match the rows exactly.

use std::rc::Rc;

use alloy_primitives::U256;
use futures_signals::signal::{Mutable, SignalExt};
use ui::icons::Icon;

use super::Page;
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::exit::error_words;
use crate::components::wallet::{open_connect, switch_button, tile};
use crate::money::format as money;
use crate::money::{Rounding, units::pow10};
use crate::tx::flow::{Fallback, Outcome, Step, claim_door_open, claim_fallback_open};
use crate::tx::read::{ClaimState, Holdings, State, VaultFigures};
use crate::view::{El, View, el, text};
use crate::wallet::machine::Connection;
use crate::wallet::{Action, Network, Running, VaultRef, networks};
use crate::web::app_link;

/// One vault the wallet has something in.
#[derive(Clone)]
struct Row {
    vault: Rc<VaultRef>,
    network: Rc<Network>,
    figures: VaultFigures,
    held: Holdings,
    /// What its shares are worth now, rounded down to the cent, in cents.
    value_cents: u128,
    /// What was paid in, as the API read it from the vault's logs, in cents.
    paid_cents: Option<u128>,
    /// Requests of this wallet still waiting for a price.
    waiting: Vec<shared::Request>,
    /// The chain's clock when read, for the fallback's day.
    now: u64,
}

/// A claim of this wallet, as the API lists it.
#[derive(Clone)]
struct ClaimRow {
    vault: Rc<VaultRef>,
    network: Rc<Network>,
    claim: shared::HeldClaim,
    /// The vault has closed (read from the chain): its claims are collected
    /// from its cash.
    closed: bool,
}

#[derive(Clone, Default)]
struct Loaded {
    rows: Vec<Row>,
    claims: Vec<ClaimRow>,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn cents(value: U256, decimals: u8) -> u128 {
    let per_cent = if decimals >= 2 {
        pow10(decimals - 2)
    } else {
        U256::from(1u8)
    };
    u128::try_from(value / per_cent).unwrap_or(u128::MAX)
}

fn dollars_from_cents(cents: u128) -> String {
    money::dollars(U256::from(cents), 2, Rounding::Down)
}

/// The portfolio.
pub fn page() -> Page {
    let session = crate::wallet::get();
    let loaded: Mutable<Option<Loaded>> = Mutable::new(None);
    let body = el("div")
        .class("sh-portfolio")
        .child(
            el("div")
                .class("sh-portfolio__head")
                .child(el("h1").class("sh-portfolio__title").text("Portfolio"))
                .child(
                    el("p")
                        .class("sh-muted")
                        .text_from(&session.connection, |connection| {
                            match connection.account() {
                                Some(account) => format!(
                                    "Wallet {} · figures read from each vault’s contract",
                                    crate::format::short_hash(&format!("{account:?}"))
                                ),
                                None => {
                                    "Your shares, what they’re worth and any booked withdrawals."
                                        .to_string()
                                }
                            }
                        }),
                ),
        )
        .child_signal(
            Some(signed_out()),
            futures_signals::map_ref! {
                let connection = session.connection.signal_cloned(),
                let loaded = loaded.signal_cloned() => (connection.account(), loaded.clone())
            }
            .map(|(account, loaded)| {
                Some(match (account, loaded) {
                    (None, _) => signed_out(),
                    (Some(_), None) => loading(),
                    (Some(_), Some(loaded)) => holdings_view(&loaded),
                })
            }),
        )
        .hook(move |dom| dom.future(follow(loaded)));
    Page {
        top: None,
        body: body.into(),
    }
}

fn signed_out() -> View {
    el("div")
        .class("sh-portfolio__empty")
        .child(el("p").text(
            "Connect a wallet to see your shares, what they’re worth and any booked withdrawals.",
        ))
        .child(
            on_click(
                Button::new(Kind::Primary, "Connect wallet").build(),
                open_connect,
            )
            .attr("aria-haspopup", "dialog"),
        )
        .into()
}

fn loading() -> View {
    el("div")
        .class("sh-portfolio__loading")
        .attr("aria-busy", "true")
        .child(crate::atoms::skeleton::skeleton_block("100%", 96))
        .child(crate::atoms::skeleton::skeleton_block("100%", 160))
        .into()
}

/// Read the wallet's holdings: now, whenever the account changes or a flow
/// lands, and every 15 s.
async fn follow(loaded: Mutable<Option<Loaded>>) {
    #[cfg(target_arch = "wasm32")]
    {
        let session = crate::wallet::get();
        loop {
            let seen = (session.changed.get(), session.account());
            if let Some(account) = seen.1 {
                let result = read(account).await;
                if session.account() == Some(account) {
                    loaded.set(Some(result));
                }
            } else {
                loaded.set(None);
            }
            for _ in 0..15 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (session.changed.get(), session.account()) != seen {
                    if session.account() != seen.1 {
                        loaded.set(None);
                    }
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = loaded;
}

#[cfg(target_arch = "wasm32")]
async fn read(account: alloy_primitives::Address) -> Loaded {
    use crate::tx::port::Reader;
    use crate::tx::read;
    use crate::wallet::session::Session;
    let session = crate::wallet::get();
    let vaults = session.read_vaults().await;
    let mut loaded = Loaded::default();
    let address = format!("{account:?}");
    let chains: std::collections::BTreeSet<shared::Chain> =
        vaults.values().map(|vault| vault.chain).collect();
    for chain in chains {
        let Some(network) = session.network(chain).await else {
            continue;
        };
        let reply = gloo_net::http::Request::get(&format!(
            "{}/{chain}/portfolio/{address}",
            crate::api::BASE
        ))
        .send()
        .await
        .ok()
        .filter(|reply| reply.ok());
        let portfolio: Option<shared::Portfolio> = match reply {
            Some(reply) => reply.json().await.ok(),
            None => None,
        };
        let reader = Session::reader(&network);
        let now = reader.chain_time().await.unwrap_or_default();
        for vault in vaults.values().filter(|vault| vault.chain == chain) {
            let Ok(held) = read::holdings(&reader, vault.address, network.asset, account).await
            else {
                continue;
            };
            let waiting: Vec<shared::Request> = vault
                .requests
                .iter()
                .filter(|request| request.owner.eq_ignore_ascii_case(&address))
                .cloned()
                .collect();
            if held.shares.is_zero()
                && held.units.is_zero()
                && held.held.is_zero()
                && waiting.is_empty()
            {
                continue;
            }
            let Ok(figures) = read::figures(&reader, vault.address).await else {
                continue;
            };
            let value = figures
                .share_price()
                .and_then(|price| crate::money::quote::value_of(held.shares, price))
                .unwrap_or_default();
            let paid_cents = portfolio
                .as_ref()
                .and_then(|p: &shared::Portfolio| p.positions.iter().find(|h| h.slug == vault.slug))
                .filter(|holding| holding.cost.is_finite() && holding.cost >= 0.0)
                .map(|holding| (holding.cost * 100.0).round() as u128);
            loaded.rows.push(Row {
                vault: vault.clone(),
                network: network.clone(),
                value_cents: cents(value, network.asset_decimals),
                figures,
                held,
                paid_cents,
                waiting,
                now,
            });
        }
        if let Some(portfolio) = portfolio {
            for claim in portfolio.claims {
                if let Some(vault) = vaults.get(&claim.slug) {
                    let closed =
                        matches!(read::state(&reader, vault.address).await, Ok(State::Closed));
                    loaded.claims.push(ClaimRow {
                        vault: vault.clone(),
                        network: network.clone(),
                        claim,
                        closed,
                    });
                }
            }
        }
    }
    loaded
}

fn holdings_view(loaded: &Loaded) -> View {
    let positions: Vec<&Row> = loaded
        .rows
        .iter()
        .filter(|row| !row.held.shares.is_zero())
        .collect();
    let open_claims: Vec<&ClaimRow> = loaded
        .claims
        .iter()
        .filter(|row| {
            !matches!(
                row.claim.state,
                shared::ClaimState::Paid | shared::ClaimState::Fallback
            )
        })
        .collect();
    let extras: Vec<&Row> = loaded
        .rows
        .iter()
        .filter(|row| {
            !row.held.units.is_zero()
                || !row.held.held.is_zero()
                || !row.held.units_claimable.is_zero()
                || !row.waiting.is_empty()
        })
        .collect();
    if positions.is_empty() && open_claims.is_empty() && extras.is_empty() {
        return el("div")
            .class("sh-portfolio__empty")
            .child(el("p").class("sh-portfolio__emptytitle").text("You haven’t deposited yet."))
            .child(el("p").class("sh-muted").text("When you deposit, your shares, what they’re worth and any booked withdrawals show up here."))
            .child(app_link("/agents").class("sh-btn sh-btn-primary").text("Browse agents"))
            .into();
    }
    let total: u128 = positions.iter().map(|row| row.value_cents).sum();
    let paid: Option<u128> = positions.iter().map(|row| row.paid_cents).sum();
    let mut tiles = el("div").class("sh-portfolio__tiles").child(tile_stat(
        "Total value",
        dollars_from_cents(total),
        format!(
            "{} {}, at today’s share prices",
            positions.len(),
            if positions.len() == 1 {
                "agent"
            } else {
                "agents"
            }
        ),
    ));
    if let Some(paid) = paid {
        tiles = tiles.child(tile_stat(
            "Paid in",
            dollars_from_cents(paid),
            "into these agents".to_string(),
        ));
        tiles = tiles.child(tile_stat(
            "Gain",
            signed_cents(total, paid),
            "since you joined, after fees".to_string(),
        ));
    }
    if !open_claims.is_empty() {
        tiles = tiles.child(tile_stat(
            "Not counted",
            format!(
                "{} booked {}",
                open_claims.len(),
                if open_claims.len() == 1 {
                    "withdrawal"
                } else {
                    "withdrawals"
                }
            ),
            "pending shares aren’t in the totals".to_string(),
        ));
    }
    el("div")
        .class("sh-portfolio__body")
        .child(tiles)
        .child(if positions.is_empty() {
            View::Empty
        } else {
            positions_table(&positions, total, paid)
        })
        .child(if open_claims.is_empty() {
            View::Empty
        } else {
            claims_list(&open_claims)
        })
        .child(if extras.is_empty() { View::Empty } else { extras_list(&extras) })
        .child(
            el("p")
                .class("sh-muted sh-portfolio__risk")
                .text("Agents can lose money. Nothing here stops that. Values use today’s share prices; a withdrawal is valued at exit-side marks and can come to a little less."),
        )
        .into()
}

fn signed_cents(value: u128, paid: u128) -> String {
    if value >= paid {
        format!("+{}", dollars_from_cents(value - paid))
    } else {
        format!("{}{}", money::MINUS, dollars_from_cents(paid - value))
    }
}

fn tile_stat(label: &'static str, figure: String, period: String) -> El {
    el("div")
        .class("sh-stat sh-stat--tile")
        .child(el("p").class("sh-label").text(label))
        .child(el("p").class("sh-stat__value sh-num").text(figure))
        .child(el("p").class("sh-stat__period").text(period))
}

fn positions_table(rows: &[&Row], total: u128, paid: Option<u128>) -> View {
    let head = [
        "Agent",
        "Shares",
        "Share price",
        "Value",
        "Paid in",
        "Gain",
        "Status",
        "",
    ];
    let body = rows.iter().map(|row| {
        let decimals = row.network.asset_decimals;
        let price = row.figures.share_price();
        let status = match row.figures.state {
            State::Open => badge(Tone::Gain, None, "Live"),
            State::WindingDown => badge(Tone::Caution, None, "Winding down"),
            State::Frozen => badge(Tone::Caution, None, "Frozen"),
            State::Killed => badge(Tone::Loss, None, "Killed"),
            State::Closed => badge(Tone::Neutral, None, "Closed"),
            _ => badge(Tone::Neutral, None, "Not open"),
        };
        el("tr")
            .class("sh-row")
            .child(
                el("th")
                    .attr("scope", "row")
                    .child(
                        app_link(format!("/agents/{}", row.vault.slug))
                            .text(row.vault.title.clone()),
                    )
                    .child(
                        el("span")
                            .class("sh-portfolio__chain")
                            .child(tile(&row.network.label))
                            .text(format!(
                                " {} · {}",
                                row.network.label,
                                if row.vault.served { "Served" } else { "Direct" }
                            )),
                    ),
            )
            .child(
                el("td")
                    .attr("data-label", "Shares")
                    .class("sh-r sh-num")
                    .text(money::shares(row.held.shares, Rounding::Down)),
            )
            .child(
                el("td")
                    .attr("data-label", "Share price")
                    .class("sh-r sh-num")
                    .text(price.map(|p| money::price(p, decimals)).unwrap_or_default()),
            )
            .child(
                el("td")
                    .attr("data-label", "Value")
                    .class("sh-r sh-num")
                    .text(dollars_from_cents(row.value_cents)),
            )
            .child(
                el("td")
                    .attr("data-label", "Paid in")
                    .class("sh-r sh-num")
                    .text(
                        row.paid_cents
                            .map(dollars_from_cents)
                            .unwrap_or_else(|| "—".to_string()),
                    ),
            )
            .child(
                el("td")
                    .attr("data-label", "Gain")
                    .class("sh-r sh-num")
                    .text(
                        row.paid_cents
                            .map(|paid| signed_cents(row.value_cents, paid))
                            .unwrap_or_else(|| "—".to_string()),
                    ),
            )
            .child(el("td").attr("data-label", "Status").child(status))
            .child(
                el("td").class("sh-r").child(
                    app_link(format!("/agents/{}", row.vault.slug))
                        .class("sh-btn sh-btn-secondary sh-btn-sm")
                        .text(if row.figures.state == State::Open {
                            "Deposit or withdraw"
                        } else {
                            "Withdraw"
                        }),
                ),
            )
    });
    el("section")
        .class("sh-portfolio__section")
        .child(el("h2").class("sh-portfolio__h2").text("Your positions"))
        .child(
            el("div").class("sh-table-wrap").child(
                el("table")
                    .class("sh-table")
                    .child(el("caption").class("sh-vh").text("Your positions"))
                    .child(
                        el("thead").child(el("tr").children(head.iter().enumerate().map(
                            |(i, label)| {
                                el("th")
                                    .attr("scope", "col")
                                    .class_if((1..=5).contains(&i), "sh-r")
                                    .text(*label)
                            },
                        ))),
                    )
                    .child(el("tbody").children(body))
                    .child(
                        el("tfoot").child(
                            el("tr")
                                .child(el("th").attr("scope", "row").text("Total"))
                                .child(el("td"))
                                .child(el("td"))
                                .child(
                                    el("td")
                                        .attr("data-label", "Value")
                                        .class("sh-r sh-num")
                                        .text(dollars_from_cents(total)),
                                )
                                .child(
                                    el("td")
                                        .attr("data-label", "Paid in")
                                        .class("sh-r sh-num")
                                        .text(
                                            paid.map(dollars_from_cents)
                                                .unwrap_or_else(|| "—".to_string()),
                                        ),
                                )
                                .child(
                                    el("td")
                                        .attr("data-label", "Gain")
                                        .class("sh-r sh-num")
                                        .text(
                                            paid.map(|paid| signed_cents(total, paid))
                                                .unwrap_or_else(|| "—".to_string()),
                                        ),
                                )
                                .child(
                                    el("td")
                                        .attr("colspan", "2")
                                        .class("sh-muted")
                                        .text("Gain after fees"),
                                ),
                        ),
                    ),
            ),
        )
        .into()
}

fn claim_badge(state: ClaimState) -> El {
    match state {
        ClaimState::Pending => badge(Tone::Neutral, Some(Icon::Clock), "Pending"),
        ClaimState::Waiting => badge(Tone::Caution, Some(Icon::Clock), "Waiting"),
        ClaimState::Queued => badge(Tone::Brand, Some(Icon::Clock), "Queued"),
        ClaimState::Closing => badge(Tone::Brand, Some(Icon::Clock), "Closing"),
        ClaimState::Paid => badge(Tone::Gain, Some(Icon::Check), "Paid"),
        ClaimState::Fallback => badge(Tone::Neutral, None, "Cash share taken"),
        ClaimState::None => badge(Tone::Neutral, None, "Unknown"),
    }
}

fn shared_state(state: shared::ClaimState) -> ClaimState {
    match state {
        shared::ClaimState::Pending => ClaimState::Pending,
        shared::ClaimState::Waiting => ClaimState::Waiting,
        shared::ClaimState::Queued => ClaimState::Queued,
        shared::ClaimState::Paid => ClaimState::Paid,
        shared::ClaimState::Fallback => ClaimState::Fallback,
        #[allow(unreachable_patterns)]
        _ => ClaimState::Closing,
    }
}

fn claims_list(claims: &[&ClaimRow]) -> View {
    el("section")
        .class("sh-portfolio__section")
        .child(
            el("h2")
                .class("sh-portfolio__h2")
                .text("Booked withdrawals"),
        )
        .child(
            el("p")
                .class("sh-muted")
                .text("Pending shares aren’t in the totals above."),
        )
        .child(
            el("ul")
                .class("sh-portfolio__cards")
                .children(claims.iter().map(|row| {
                    let year = row.claim.booked.year();
                    el("li")
                        .class("sh-card sh-card--pad sh-portfolio__claim")
                        .child(
                            el("p")
                                .class("sh-portfolio__claimtitle")
                                .text(row.vault.title.clone())
                                .child(claim_badge(shared_state(row.claim.state))),
                        )
                        .child(el("p").class("sh-num").text(format!(
                            "{} shares",
                            crate::format::number(row.claim.shares, 1)
                        )))
                        .child(el("p").class("sh-muted").text(format!(
                            "Booked {}",
                            crate::format::day_time(row.claim.booked, year)
                        )))
                        .child({
                            let state = shared_state(row.claim.state);
                            if row.closed && claim_door_open(state, State::Closed, false) {
                                View::Many(vec![
                                    el("p").class("sh-muted").text("The vault has closed: collect your share of its cash, with no fee.").into(),
                                    act(
                                        &row.vault,
                                        &row.network,
                                        Action::SettleClaim(U256::from(row.claim.id)),
                                        "Collect from the closed vault",
                                        Kind::Primary,
                                    )
                                    .into(),
                                ])
                            } else {
                                View::Empty
                            }
                        })
                        .child(
                            app_link(format!(
                                "/portfolio/claims/{}-{}",
                                row.vault.slug, row.claim.id
                            ))
                            .class("sh-link")
                            .text("Track your claim"),
                        )
                })),
        )
        .into()
}

fn extras_list(rows: &[&Row]) -> View {
    el("section")
        .class("sh-portfolio__section")
        .child(
            el("h2")
                .class("sh-portfolio__h2")
                .text("Waiting, units and held payouts"),
        )
        .child(
            el("ul")
                .class("sh-portfolio__cards")
                .children(rows.iter().map(|row| extra_card(row))),
        )
        .into()
}

fn extra_card(row: &Row) -> El {
    let decimals = row.network.asset_decimals;
    let mut card = el("li")
        .class("sh-card sh-card--pad sh-portfolio__claim")
        .child(
            el("p")
                .class("sh-portfolio__claimtitle")
                .text(row.vault.title.clone()),
        );
    for request in &row.waiting {
        let earliest = request.earliest.assume_utc().unix_timestamp().max(0) as u64;
        let id = U256::from(request.id);
        let offer = waiting_offer(
            request.kind,
            earliest,
            row.figures.state,
            row.figures.ready_to_close,
            row.figures.last.as_ref().map(|last| last.time),
            row.figures.timers.fallback_after,
            row.now,
        );
        card = card.child(el("p").text(waiting_words(request, offer)));
        let vault = &row.vault;
        let network = &row.network;
        card = match offer {
            Offer::Nothing => card,
            Offer::SettleNow => card
                .child(el("p").class("sh-muted").text("Priced. The keeper settles it at that price; anyone may, including you."))
                .child(act(vault, network, Action::Settle(id), "Settle now", Kind::Secondary)),
            Offer::ClosedRefund => card
                .child(el("p").class("sh-muted").text("The vault closed before it was priced: the deposit goes back to the wallet that paid it in."))
                .child(act(vault, network, Action::SettleClosed(id), "Take your deposit back", Kind::Primary)),
            Offer::ClosedPayout => card
                .child(el("p").class("sh-muted").text("The vault closed before it was priced: the shares are paid their fraction of its cash, with no fee."))
                .child(act(vault, network, Action::SettleClosed(id), "Settle at the closed vault’s price", Kind::Primary)),
            Offer::TakeBack => card
                .child(el("p").class("sh-muted").text("Nothing has priced it for a day. You can take your deposit back: it returns to the wallet that paid it in, and nothing is bought."))
                .child(act(vault, network, Action::TakeCash(Fallback::Request(id)), "Take your deposit back", Kind::Primary)),
            Offer::ProRata => card
                .child(el("p").class("sh-muted").text("Nothing has priced it for a day. You can take your exact share of the vault’s cash now; the rest becomes units, paid as the money comes home."))
                .child(act(vault, network, Action::TakeCash(Fallback::Request(id)), "Take cash share now", Kind::Primary))
                .child(act(vault, network, Action::ClaimInstead(id), "Make it a claim instead", Kind::Secondary)),
        };
    }
    if !row.held.units.is_zero() || !row.held.units_claimable.is_zero() {
        card = card.child(el("p").text(format!(
            "Units from the fallback: {} ready to collect.",
            money::dollars(row.held.units_claimable, decimals, Rounding::Down)
        )));
        if !row.held.units_claimable.is_zero() {
            card = card.child(act(
                &row.vault,
                &row.network,
                Action::CollectUnits,
                "Collect",
                Kind::Primary,
            ));
        }
    }
    if !row.held.held.is_zero() {
        card = card
            .child(el("p").text(format!(
                "{} held for you: the dollar refused a payment to your wallet, so the vault kept it.",
                money::dollars(row.held.held, decimals, Rounding::Down)
            )))
            .child(act(&row.vault, &row.network, Action::CollectHeld, "Collect", Kind::Primary));
    }
    card
}

/// What the portfolio offers for one waiting request, as the contract
/// takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offer {
    Nothing,
    /// Priced and not yet settled: the second step (`settle`), anyone's.
    SettleNow,
    /// A closed vault sends a deposit back (`settleClosed`).
    ClosedRefund,
    /// A closed vault pays a withdrawal its fraction of the cash
    /// (`settleClosed`).
    ClosedPayout,
    /// Nothing priced for a day: a deposit goes back (`takeProRata`, which
    /// records a refund).
    TakeBack,
    /// Nothing priced for a day: a withdrawal takes the cash share or
    /// becomes a claim (`takeProRata`, `claimInstead`).
    ProRata,
}

/// The one door the contract opens for a waiting request of this kind on a
/// vault in this state. A claim's own request is collected through its claim
/// and a tank fill isn't a depositor's, so neither is offered anything here
/// but settling once priced.
fn waiting_offer(
    kind: shared::RequestKind,
    earliest: u64,
    state: State,
    ready_to_close: bool,
    last_priced: Option<u64>,
    fallback_after: u64,
    now: u64,
) -> Offer {
    use shared::RequestKind as K;
    let priced = last_priced.is_some_and(|time| time >= earliest);
    if state == State::Closed {
        return match kind {
            K::Deposit => Offer::ClosedRefund,
            K::Withdrawal => Offer::ClosedPayout,
            K::Claim | K::TankFill => Offer::Nothing,
        };
    }
    if priced {
        return Offer::SettleNow;
    }
    if now < earliest.saturating_add(fallback_after)
        || matches!(state, State::Cancelled | State::None)
    {
        return Offer::Nothing;
    }
    match kind {
        K::Deposit => Offer::TakeBack,
        K::Withdrawal if !ready_to_close => Offer::ProRata,
        _ => Offer::Nothing,
    }
}

/// A waiting request in words.
fn waiting_words(request: &shared::Request, offer: Offer) -> String {
    let what = match request.kind {
        shared::RequestKind::Deposit => {
            format!("Deposit of {}", crate::format::money(request.amount))
        }
        shared::RequestKind::Withdrawal => format!(
            "Withdrawal of {} shares",
            crate::format::number(request.amount, 1)
        ),
        shared::RequestKind::Claim => format!(
            "Claim of {} shares, queued",
            crate::format::number(request.amount, 1)
        ),
        shared::RequestKind::TankFill => "Tank fill".to_string(),
    };
    if offer == Offer::SettleNow {
        format!("{what}, priced and waiting to be settled")
    } else {
        format!("{what} waiting for a price")
    }
}

/// The headline for a paid claim: "in your wallet" only to the wallet it
/// was paid to.
pub fn paid_headline(
    paid: &str,
    owner: alloy_primitives::Address,
    viewer: Option<alloy_primitives::Address>,
) -> String {
    if viewer == Some(owner) {
        format!("{paid} is in your wallet")
    } else {
        format!(
            "{paid} was paid to {}",
            crate::format::short_hash(&format!("{owner:?}"))
        )
    }
}

/// A button that runs one flow, with its status line in place: the wallet
/// step, then done or the error in words. Switches the wallet's chain first
/// where needed.
fn act(
    vault: &Rc<VaultRef>,
    network: &Rc<Network>,
    action: Action,
    label: &'static str,
    kind: Kind,
) -> El {
    let running: Mutable<Option<Running>> = Mutable::new(None);
    let session = crate::wallet::get();
    let vault = vault.clone();
    let network = network.clone();
    let chain_id = network.chain_id;
    el("div").class("sh-act").child_signal(
        None,
        futures_signals::map_ref! {
            let connection = session.connection.signal_cloned(),
            let current = running.signal_cloned() => (connection.clone(), current.clone())
        }
        .map(move |(connection, current)| {
            let wrong = matches!(connection, Connection::Connected { chain_id: Some(on), .. } if on != chain_id)
                && crate::wallet::get().is_browser();
            if connection.account().is_none() {
                return Some(on_click(Button::new(kind, "Connect wallet").size(Size::Small).build(), open_connect).into());
            }
            if let Some(current) = current {
                return Some(status(&current, &network, &running));
            }
            if wrong {
                return Some(switch_button(chain_id, Size::Small).into());
            }
            let running = running.clone();
            let vault = vault.clone();
            let action = action.clone();
            Some(
                on_click(Button::new(kind, label).size(Size::Small).build(), move || {
                    // One flow at a time: a second click starts nothing.
                    if running.lock_ref().is_some() {
                        return;
                    }
                    let flow = Running::new();
                    running.set(Some(flow.clone()));
                    crate::wallet::run::start(&vault, action.clone(), &flow);
                })
                .into(),
            )
        }),
    )
}

fn status(current: &Running, network: &Rc<Network>, running: &Mutable<Option<Running>>) -> View {
    let network = network.clone();
    let running = running.clone();
    el("div")
        .class("sh-act__status")
        .child_signal(
            None,
            current.step.signal_cloned().map(move |step| {
                let session = crate::wallet::get();
                let wallet = session.wallet_name.get_cloned();
                Some(match step? {
                    Step::Checking => el("p").attr("aria-live", "polite").text("Checking…").into(),
                    Step::Signing(_) => el("p").attr("aria-live", "polite").text(format!("Approve in {wallet}…")).into(),
                    Step::Sent { unseen: true, .. } => el("p")
                        .attr("aria-live", "polite")
                        .text(format!("Sent. Not seen by the network yet; it may still go through. Check {wallet}’s activity before sending anything again."))
                        .into(),
                    Step::Sent { .. } | Step::Requested(_) | Step::Priced { .. } => {
                        el("p").attr("aria-live", "polite").text("Sent. Waiting for the network…").into()
                    }
                    Step::Done(outcome) => {
                        let line = match outcome {
                            Outcome::Collected { assets, .. } | Outcome::TookCash { assets, .. } => {
                                format!("{} is in your wallet.", money::dollars(assets, network.asset_decimals, Rounding::Down))
                            }
                            Outcome::Claim { state: ClaimState::Paid, paid, owner, .. } => {
                                format!(
                                    "Paid: {}.",
                                    paid_headline(
                                        &money::dollars(paid, network.asset_decimals, Rounding::Down),
                                        owner,
                                        session.account(),
                                    )
                                )
                            }
                            Outcome::Claim { state: ClaimState::Waiting, .. } => {
                                "The market was outside the exit band, so nothing closed. It can be tried again.".to_string()
                            }
                            Outcome::Claim { state: ClaimState::Queued, .. } => {
                                "The cash covers it: it’s priced at the next valuation.".to_string()
                            }
                            Outcome::ClaimMade { claim, .. } => format!("Claim {claim} booked."),
                            Outcome::Refunded { assets, to, .. } => {
                                let assets = money::dollars(assets, network.asset_decimals, Rounding::Down);
                                if session.account() == Some(to) {
                                    format!("Your deposit is back: {assets} is in your wallet.")
                                } else {
                                    format!("Your deposit went back: {assets} to the wallet that paid it in.")
                                }
                            }
                            Outcome::Deposited { shares, .. } => {
                                format!("Settled: you now hold {} more shares.", money::shares(shares, Rounding::Down))
                            }
                            Outcome::Booked { claim, .. } => format!("Settled: booked as claim {claim}."),
                            Outcome::Paid { assets, .. } => {
                                format!("{} is in your wallet.", money::dollars(assets, network.asset_decimals, Rounding::Down))
                            }
                            _ => "Done.".to_string(),
                        };
                        el("p").attr("role", "status").class("sh-gain").child(icon(Icon::Check, 16)).text(format!(" {line}")).into()
                    }
                    Step::Failed(error) => {
                        let words = error_words(
                            &error,
                            &wallet,
                            &network.label,
                            session.wallet_chain().map(networks::chain_name).as_deref(),
                            &network.asset_symbol,
                            network.asset_decimals,
                            network.coin(),
                        );
                        let running = running.clone();
                        // Money that moved, or may still, is never offered
                        // again from here: the page reads where it stands.
                        let again = if error.may_have_moved() { "Dismiss" } else { "Try again" };
                        let hash = error.hash().map(|hash| {
                            crate::atoms::hash::hash(&format!("{hash:?}"), Some("Transaction"), "transaction hash")
                        });
                        el("div")
                            .attr("role", if words.alert { "alert" } else { "status" })
                            .child(el("p").class("sh-act__error").text(words.title))
                            .child(el("p").class("sh-muted").text(words.text))
                            .child(hash.map(View::from).unwrap_or(View::Empty))
                            .child(on_click(
                                el("button").attr("type", "button").class("sh-link").text(again),
                                move || running.set(None),
                            ))
                            .into()
                    }
                })
            }),
        )
        .into()
}

// The claim page ---------------------------------------------------------------

#[derive(Clone)]
struct ClaimView {
    vault: Rc<VaultRef>,
    network: Rc<Network>,
    id: u64,
    claim: crate::tx::read::Claim,
    figures: VaultFigures,
    exit_band_bps: u16,
    now: u64,
}

/// One claim: where it is, what it's estimated at, and how to push it
/// through.
pub fn claim(slug: &str, id: u64) -> Page {
    let loaded: Mutable<Option<Result<ClaimView, String>>> = Mutable::new(None);
    let body = el("div")
        .class("sh-portfolio sh-claim")
        .child(
            el("nav")
                .class("sh-claim__crumbs")
                .attr("aria-label", "Breadcrumb")
                .child(app_link("/portfolio").text("Portfolio"))
                .text(" / ")
                .child(el("span").attr("aria-current", "page").text("Claim")),
        )
        .child_from(&loaded, |loaded| {
            Some(match loaded {
                None => loading(),
                Some(Err(why)) => notice(NoticeTone::Info, text(why.clone())).into(),
                Some(Ok(view)) => claim_view(view),
            })
        })
        .hook({
            let slug = slug.to_string();
            move |dom| dom.future(follow_claim(slug, id, loaded))
        });
    Page {
        top: None,
        body: body.into(),
    }
}

async fn follow_claim(slug: String, id: u64, loaded: Mutable<Option<Result<ClaimView, String>>>) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::tx::port::Reader;
        use crate::tx::read;
        use crate::wallet::session::Session;
        let session = crate::wallet::get();
        loop {
            let seen = session.changed.get();
            let vaults = session.read_vaults().await;
            let result = async {
                let vault = vaults
                    .get(&slug)
                    .cloned()
                    .ok_or("This agent isn’t on this network.".to_string())?;
                let network = session
                    .network(vault.chain)
                    .await
                    .ok_or("This vault’s chain can’t be reached from here.".to_string())?;
                let reader = Session::reader(&network);
                let count = read::read(
                    &reader,
                    vault.address,
                    chain::bindings::vault::IVault::claimCountCall {},
                )
                .await
                .map_err(|_| "The vault couldn’t be read. Try again in a moment.".to_string())?;
                if U256::from(id) > count {
                    return Err("This vault has no claim with that number.".to_string());
                }
                let claim = read::claim(&reader, vault.address, U256::from(id))
                    .await
                    .map_err(|_| {
                        "The claim couldn’t be read. Try again in a moment.".to_string()
                    })?;
                let figures = read::figures(&reader, vault.address).await.map_err(|_| {
                    "The vault couldn’t be read. Try again in a moment.".to_string()
                })?;
                let exit_band_bps = read::read(
                    &reader,
                    vault.address,
                    chain::bindings::vault::IVault::exitBandBpsCall {},
                )
                .await
                .unwrap_or_default();
                let now = reader.chain_time().await.unwrap_or_default();
                Ok(ClaimView {
                    vault,
                    network,
                    id,
                    claim,
                    figures,
                    exit_band_bps,
                    now,
                })
            }
            .await;
            loaded.set(Some(result));
            for _ in 0..10 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if session.changed.get() != seen {
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (slug, id, loaded);
}

fn utc_day(seconds: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(seconds).unwrap_or(0))
        .map(|at| {
            let at = time::PrimitiveDateTime::new(at.date(), at.time());
            crate::format::day_time(at, at.year())
        })
        .unwrap_or_default()
}

fn claim_view(view: &ClaimView) -> View {
    let decimals = view.network.asset_decimals;
    let claim = &view.claim;
    let state = claim.state;
    let closed = view.figures.state == State::Closed;
    let estimate = view
        .figures
        .exit_quote(claim.shares)
        .map(|(quote, _)| quote.payout);
    let open = matches!(
        state,
        ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued | ClaimState::Closing
    );
    let door = claim_door_open(state, view.figures.state, view.figures.ready_to_close);
    let viewer = crate::wallet::get().account();
    let headline = match state {
        ClaimState::Paid => paid_headline(
            &money::dollars(claim.paid, decimals, Rounding::Down),
            claim.owner,
            viewer,
        ),
        ClaimState::Fallback => "Your cash share was taken".to_string(),
        _ => format!(
            "Your withdrawal of {} shares is booked.",
            money::shares(claim.shares, Rounding::Down)
        ),
    };
    let explain = match state {
        ClaimState::Pending => {
            "Booked. Settling closes your share of each position with reduce-only orders, within the exit band, and pays you in the same transaction."
        }
        ClaimState::Waiting => {
            "The market was outside the exit band when it was last tried, so nothing closed. It is tried again; this protects you from selling at a bad price."
        }
        ClaimState::Queued if closed => {
            "The cash at home covered it, and the vault closed before the valuation that would have priced it."
        }
        ClaimState::Queued => {
            "The cash at home covers it: it is priced at the next valuation, like any withdrawal."
        }
        ClaimState::Closing => {
            "A close is under way at the venue. It is paid when the close is delivered."
        }
        ClaimState::Paid => "The cash is home and in the wallet it was booked for.",
        ClaimState::Fallback => {
            "It took the fallback: its share of the cash then, and units for the rest of what the stations hold."
        }
        ClaimState::None => "",
    };
    let fallback = viewer == Some(claim.owner)
        && claim_fallback_open(
            claim,
            view.figures.state,
            view.figures.ready_to_close,
            view.figures.timers.fallback_after,
            view.now,
        );
    let mut actions = el("div").class("sh-claim__actions");
    if door && closed {
        actions = actions
            .child(el("p").text(
                "This vault has closed. Its claims are paid their share of its cash, with no fee.",
            ))
            .child(act(
                &view.vault,
                &view.network,
                Action::SettleClaim(U256::from(view.id)),
                "Collect from the closed vault",
                Kind::Primary,
            ));
    } else {
        if door {
            actions = actions
                .child(el("h2").class("sh-portfolio__h2").text("Push it through yourself"))
                .child(el("p").text("Settling is public. It doesn’t depend on us: anyone can send it, including you. You pay its network fee. If the market is still outside the exit band, nothing closes and the claim keeps waiting."))
                .child(act(&view.vault, &view.network, Action::SettleClaim(U256::from(view.id)), "Settle now", Kind::Secondary));
        }
        if fallback {
            actions = actions
                .child(el("h2").class("sh-portfolio__h2").text("No price for 24 hours"))
                .child(el("p").text("You can take your exact share of the vault’s cash at once. The rest becomes units on each station’s holdings, paid as the money comes home."))
                .child(act(&view.vault, &view.network, Action::TakeCash(Fallback::Claim(U256::from(view.id))), "Take cash share now", Kind::Primary));
        }
    }
    if view.vault.served && open {
        actions = actions.child(notice(
            NoticeTone::Info,
            text(format!(
                "{} is a served vault. An unwinder finishes the parts on other chains.",
                view.vault.title
            )),
        ));
    }
    let fact = |label: &'static str, value: String| {
        el("div")
            .class("sh-exit__row")
            .child(el("dt").text(label))
            .child(el("dd").class("sh-num").text(value))
    };
    el("div")
        .class("sh-claim__grid")
        .child(
            el("div")
                .class("sh-claim__main")
                .child(el("p").attr("aria-live", "polite").child(claim_badge(state)))
                .child(el("h1").class("sh-portfolio__title").text(headline))
                .child(el("p").text(explain))
                .child(match (estimate, open) {
                    (Some(estimate), true) => el("p").class("sh-muted").text(format!(
                        "Estimated {} from {}, at the last exit price. What you finally get is what your share of each position sells for.",
                        money::dollars(estimate, decimals, Rounding::Down),
                        view.vault.title
                    )).into(),
                    _ => View::Empty,
                })
                .child(actions),
        )
        .child(
            el("aside").class("sh-card sh-card--pad sh-claim__facts").child(el("h2").class("sh-portfolio__h2").text("Claim facts")).child(
                el("dl")
                    .class("sh-exit__rows")
                    .child(fact("Vault", format!("{} · {}", view.vault.title, view.network.label)))
                    .child(fact("Shares pending", if open { money::shares(claim.shares, Rounding::Down) } else { "—".to_string() }))
                    .child(fact("Booked at", utc_day(claim.booked_at)))
                    .child(fact("Last tried", if claim.last_attempt_at == 0 { if open { "Not yet".to_string() } else { "—".to_string() } } else { utc_day(claim.last_attempt_at) }))
                    .child(fact("Exit kind", if view.vault.served { "Served".to_string() } else { "Direct".to_string() }))
                    .child(fact("Exit band", format!("{} of the venue’s price", crate::format::bps(u32::from(view.exit_band_bps)))))
                    .child(fact("Pays to", crate::format::short_hash(&format!("{:?}", claim.owner))))
                    .child(fact("Claim", format!("{} #{}", view.vault.slug, view.id))),
            ),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rows_value_is_rounded_down_to_the_cent_once_and_the_total_is_their_sum() {
        // $10,001.259999 and $5,001.581 in six-decimal units.
        let rows = [
            cents(U256::from(10_001_259_999u64), 6),
            cents(U256::from(5_001_581_000u64), 6),
        ];
        assert_eq!(rows, [1_000_125, 500_158]);
        let total: u128 = rows.iter().sum();
        assert_eq!(dollars_from_cents(total), "$15,002.83");
        assert_eq!(dollars_from_cents(rows[0]), "$10,001.25");
        assert_eq!(dollars_from_cents(rows[1]), "$5,001.58");
    }

    #[test]
    fn each_waiting_request_is_offered_only_the_door_the_contract_opens_for_its_kind() {
        use shared::RequestKind as K;
        let day = 86_400;
        let offer = |kind, state, ready, priced, now| {
            waiting_offer(kind, 1_000, state, ready, priced, day, now)
        };
        // Priced and not yet settled: the second step, for any kind.
        for kind in [K::Deposit, K::Withdrawal, K::Claim, K::TankFill] {
            assert_eq!(
                offer(kind, State::Open, false, Some(1_000), 1_001),
                Offer::SettleNow
            );
        }
        // Nothing priced for a day: a deposit goes back, a withdrawal takes
        // its cash share or becomes a claim; a claim's own request and a
        // tank fill are offered nothing.
        let late = 1_000 + day;
        assert_eq!(
            offer(K::Deposit, State::Open, false, None, late),
            Offer::TakeBack
        );
        assert_eq!(
            offer(K::Deposit, State::Open, false, Some(999), late),
            Offer::TakeBack
        );
        assert_eq!(
            offer(K::Withdrawal, State::Open, false, None, late),
            Offer::ProRata
        );
        assert_eq!(
            offer(K::Withdrawal, State::Open, true, None, late),
            Offer::Nothing
        );
        assert_eq!(
            offer(K::Claim, State::Open, false, None, late),
            Offer::Nothing
        );
        assert_eq!(
            offer(K::TankFill, State::Open, false, None, late),
            Offer::Nothing
        );
        // Before the day is out, nothing.
        assert_eq!(
            offer(K::Deposit, State::Open, false, None, late - 1),
            Offer::Nothing
        );
        // A closed vault: a deposit back, a withdrawal paid from its cash.
        assert_eq!(
            offer(K::Deposit, State::Closed, false, None, 0),
            Offer::ClosedRefund
        );
        assert_eq!(
            offer(K::Withdrawal, State::Closed, false, None, 0),
            Offer::ClosedPayout
        );
        assert_eq!(
            offer(K::Claim, State::Closed, false, None, 0),
            Offer::Nothing
        );
        assert_eq!(
            offer(K::TankFill, State::Closed, false, None, 0),
            Offer::Nothing
        );
    }

    #[test]
    fn a_paid_claim_is_said_to_be_in_your_wallet_only_to_the_wallet_it_was_paid_to() {
        let owner = alloy_primitives::Address::repeat_byte(0x11);
        let other = alloy_primitives::Address::repeat_byte(0x22);
        assert_eq!(
            paid_headline("$24,968.05", owner, Some(owner)),
            "$24,968.05 is in your wallet"
        );
        for viewer in [Some(other), None] {
            let said = paid_headline("$24,968.05", owner, viewer);
            assert!(!said.contains("your wallet"), "{said}");
            assert!(said.starts_with("$24,968.05 was paid to "), "{said}");
        }
    }

    #[test]
    fn a_gain_is_signed_with_a_true_minus() {
        assert_eq!(signed_cents(1_500_283, 1_455_000), "+$452.83");
        assert_eq!(signed_cents(100, 250), "\u{2212}$1.50");
        assert_eq!(signed_cents(100, 100), "+$0.00");
    }
}
