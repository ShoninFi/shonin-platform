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
use crate::tx::error::Revert;
use crate::tx::flow::{Fallback, Outcome, Prompt, Step, claim_door_open, claim_fallback_open};
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
    /// For a waiting request whose day has passed, whether the vault would
    /// take its fallback from this wallet now: the chain's own answer
    /// (`eth_estimateGas`), by request id; `Err` with why not.
    fallbacks: Vec<(u64, Result<(), Revert>)>,
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
    /// The dollars in this wallet on each chain the app has a vault on.
    balances: Vec<Balance>,
}

/// The stand-in dollar in a wallet on one chain, read from its token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balance {
    /// "Base Sepolia".
    pub chain: String,
    /// "$4,960.00".
    pub dollars: String,
}

/// What one wallet's section adds to the page's totals, once read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// What its shares are worth, in cents (each row rounded down once).
    pub value_cents: u128,
    /// Agents it holds shares in.
    pub positions: usize,
    /// Its booked withdrawals not yet paid.
    pub booked: usize,
    /// What its positions were paid in with, where every one is known.
    pub paid_cents: Option<u128>,
    /// Whether it holds anything at all: shares, a booked withdrawal, a
    /// waiting request, units or a held payout.
    pub holds: bool,
}

/// What the top of the page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Top {
    /// No wallet connected.
    SignedOut,
    /// A connected wallet is still being read.
    Reading,
    /// No connected wallet holds anything; `before` when this browser sent
    /// something from one of them, so it isn't said they never deposited.
    Empty { before: bool },
    /// The totals across every connected wallet.
    Totals {
        value_cents: u128,
        positions: usize,
        booked: usize,
        paid_cents: Option<u128>,
    },
}

/// The top of the page for each wallet: `None` if not connected,
/// `Some(None)` while it is read, `Some(Some(summary))` once read; `before`
/// when this browser sent something from one of them.
pub fn top(wallets: &[Option<Option<Summary>>], before: bool) -> Top {
    let connected: Vec<&Option<Summary>> = wallets.iter().flatten().collect();
    if connected.is_empty() {
        return Top::SignedOut;
    }
    if connected.iter().any(|summary| summary.is_none()) {
        return Top::Reading;
    }
    let read: Vec<&Summary> = connected.into_iter().flatten().collect();
    if !read.iter().any(|summary| summary.holds) {
        return Top::Empty { before };
    }
    let holding: Vec<&&Summary> = read
        .iter()
        .filter(|summary| summary.positions > 0)
        .collect();
    Top::Totals {
        value_cents: read.iter().map(|summary| summary.value_cents).sum(),
        positions: read.iter().map(|summary| summary.positions).sum(),
        booked: read.iter().map(|summary| summary.booked).sum(),
        paid_cents: holding.iter().map(|summary| summary.paid_cents).sum(),
    }
}

/// A wallet section's heading: its chain family, its app and its short
/// address. "EVM · Phantom · 0xed47…b3c0".
pub fn wallet_heading(family: crate::wallet::recent::Family, app: &str, short: &str) -> String {
    let family = match family {
        crate::wallet::recent::Family::Evm => "EVM",
        crate::wallet::recent::Family::Solana => "Solana",
    };
    format!("{family} · {app} · {short}")
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

/// The portfolio: one view across every connected wallet. The totals at
/// the top add up every wallet's; each wallet then has its own section,
/// named by its chain family, its app and its address, read on its own so
/// neither waits for the other.
pub fn page() -> Page {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    let loaded: Mutable<Option<Loaded>> = Mutable::new(None);
    let solana_summary: Mutable<Option<Summary>> = Mutable::new(None);
    let top_signal = futures_signals::map_ref! {
        let evm = session.connection.signal_ref(Connection::account),
        let solana = solana.connection.signal_ref(|c| c.account()),
        let evm_summary = loaded.signal_ref(|loaded| loaded.as_ref().map(summary_of)),
        let solana_summary = solana_summary.signal(),
        let recent = crate::wallet::recent::get().signal_cloned() => {
            use crate::wallet::recent::{Family, of};
            let before = evm.is_some_and(|evm| !of(recent, Family::Evm, &format!("{evm:?}")).is_empty())
                || solana.is_some_and(|solana| !of(recent, Family::Solana, &solana.to_string()).is_empty());
            top(&[evm.map(|_| *evm_summary), solana.map(|_| *solana_summary)], before)
        }
    }
    .dedupe_cloned();
    let body = el("div")
        .class("sh-portfolio")
        .child(
            el("div")
                .class("sh-portfolio__head")
                .child(el("h1").class("sh-portfolio__title").text("Portfolio"))
                .child(el("p").class("sh-muted").text(
                    "Your shares, what they’re worth and any booked withdrawals, read from each vault.",
                )),
        )
        .child_signal(Some(top_view(&Top::SignedOut)), top_signal.map(|top| Some(top_view(&top))))
        .child(evm_section(&loaded))
        // What the connected Solana wallet holds, beside the EVM one's.
        .child(crate::components::solana::portfolio::section(solana_summary))
        .child_signal(
            None,
            futures_signals::map_ref! {
                let evm = session.connection.signal_ref(|c| c.account().is_some()),
                let solana = solana.connection.signal_ref(|c| c.account().is_some()) => *evm || *solana
            }
            .dedupe()
            .map(|connected| {
                connected.then(|| {
                    el("p")
                        .class("sh-muted sh-portfolio__risk")
                        .text("Agents can lose money. Nothing here stops that. Values use today’s share prices; a withdrawal is valued at exit-side marks and can come to a little less.")
                        .into()
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

/// A wallet's section while it is read.
pub(crate) fn section_loading() -> View {
    el("div")
        .class("sh-portfolio__loading")
        .attr("aria-busy", "true")
        .child(el("span").class("sh-vh").text("Reading this wallet"))
        .child(crate::atoms::skeleton::skeleton_block("100%", 120))
        .into()
}

/// The top of the page: connect, the totals being read, the empty state,
/// or the totals across every connected wallet.
fn top_view(top: &Top) -> View {
    match top {
        Top::SignedOut => signed_out(),
        Top::Reading => el("div")
            .class("sh-portfolio__tiles")
            .attr("aria-busy", "true")
            .child(crate::atoms::skeleton::skeleton_block("100%", 96))
            .into(),
        Top::Empty { before } => el("div")
            .class("sh-portfolio__empty")
            .child(el("p").class("sh-portfolio__emptytitle").text(if *before {
                "Nothing in agents right now."
            } else {
                "You haven’t deposited yet."
            }))
            .child(el("p").class("sh-muted").text("When you deposit, your shares, what they’re worth and any booked withdrawals show up here."))
            .child(app_link("/agents").class("sh-btn sh-btn-primary").text("Browse agents"))
            .into(),
        Top::Totals {
            value_cents,
            positions,
            booked,
            paid_cents,
        } => {
            let mut tiles = el("div").class("sh-portfolio__tiles").child(tile_stat(
                "Total value",
                dollars_from_cents(*value_cents),
                format!(
                    "{positions} {}, at today’s share prices",
                    if *positions == 1 { "agent" } else { "agents" }
                ),
            ));
            if let Some(paid) = paid_cents.filter(|_| *positions > 0) {
                tiles = tiles
                    .child(tile_stat(
                        "Paid in",
                        dollars_from_cents(paid),
                        "into these agents".to_string(),
                    ))
                    .child(tile_stat(
                        "Gain",
                        match gain_percent(*value_cents, paid) {
                            Some(percent) => {
                                format!("{} · {percent}", signed_cents(*value_cents, paid))
                            }
                            None => signed_cents(*value_cents, paid),
                        },
                        "since you joined, after fees".to_string(),
                    ));
            }
            if *booked > 0 {
                tiles = tiles.child(tile_stat(
                    "Not counted",
                    format!(
                        "{booked} booked {}",
                        if *booked == 1 { "withdrawal" } else { "withdrawals" }
                    ),
                    "pending shares aren’t in the totals".to_string(),
                ));
            }
            tiles.into()
        }
    }
}

/// The EVM wallet's section: nothing while no EVM wallet is connected.
fn evm_section(loaded: &Mutable<Option<Loaded>>) -> View {
    let session = crate::wallet::get();
    el("div")
        .class("sh-contents")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let account = session.connection.signal_ref(Connection::account),
                let _name = session.wallet_name.signal_cloned(),
                let loaded = loaded.signal_cloned() => (*account, loaded.clone())
            }
            .map(|(account, loaded)| {
                let account = account?;
                let address = format!("{account:?}");
                let app = crate::wallet::get().app_name();
                Some(wallet_section(
                    crate::wallet::recent::Family::Evm,
                    &app,
                    &address,
                    crate::format::short_hash(&address),
                    match loaded {
                        None => section_loading(),
                        Some(loaded) => evm_content(&loaded),
                    },
                ))
            }),
        )
        .into()
}

/// One wallet's section: its heading, what was read, and what this browser
/// sent from it.
pub(crate) fn wallet_section(
    family: crate::wallet::recent::Family,
    app: &str,
    address: &str,
    short: String,
    content: View,
) -> View {
    use crate::wallet::recent::Family;
    let id = match family {
        Family::Evm => "pf-wallet-evm",
        Family::Solana => "pf-wallet-solana",
    };
    let heading = wallet_heading(family, app, &short);
    let mark = match family {
        Family::Evm => "EVM",
        Family::Solana => "Solana",
    };
    el("section")
        .class("sh-portfolio__section sh-portfolio__wallet")
        .attr(
            "data-wallet",
            match family {
                Family::Evm => "evm",
                Family::Solana => "solana",
            },
        )
        .attr("aria-labelledby", id)
        .child(
            el("h2")
                .class("sh-portfolio__h2 sh-portfolio__wallethead")
                .attr("id", id)
                .attr("aria-label", heading)
                .text(format!("{mark} · {app} · "))
                .child(el("span").class("sh-mono").text(short)),
        )
        .child(content)
        .child(recent_list(family, address.to_string()))
        .into()
}

/// The stand-in dollar in a wallet on each chain the app has a vault on.
pub(crate) fn balances_view(balances: &[Balance]) -> View {
    if balances.is_empty() {
        return View::Empty;
    }
    el("div")
        .class("sh-portfolio__balances")
        .child(el("p").class("sh-label").text("Dollars in this wallet"))
        .child(el("dl").children(balances.iter().map(|balance| {
            el("div")
                .child(el("dt").text(format!("On {}", balance.chain)))
                .child(el("dd").class("sh-num").text(balance.dollars.clone()))
        })))
        .into()
}

/// The last sends this browser made from one wallet, newest first, with
/// where each stands: nothing when there are none.
pub(crate) fn recent_list(family: crate::wallet::recent::Family, account: String) -> View {
    el("div")
        .class("sh-contents")
        .child_signal(
            None,
            crate::wallet::recent::get().signal_ref(move |list| {
                let mine = crate::wallet::recent::of(list, family, &account);
                (!mine.is_empty()).then(|| recent_view(&mine))
            }),
        )
        .into()
}

fn recent_view(entries: &[crate::wallet::recent::Entry]) -> View {
    el("div")
        .class("sh-portfolio__recent")
        .child(
            el("h3")
                .class("sh-portfolio__h3")
                .text("Recent in this browser"),
        )
        .child(el("p").class("sh-muted").text(
            "Only what was sent from this browser. Your wallet’s own history has everything.",
        ))
        .child(el("ul").children(entries.iter().map(|entry| {
            let when = utc_day((entry.at_ms / 1000.0).max(0.0) as u64);
            el("li")
                .child(el("p").class("sh-portfolio__recentwhat").text(entry.what()))
                .child(
                    el("p")
                        .class("sh-muted")
                        .text(format!("{} · {when} · ", entry.status()))
                        .child(View::from(match &entry.tx_url {
                            Some(url) => el("a")
                                .class("sh-link")
                                .attr("href", url.clone())
                                .attr("target", "_blank")
                                .attr("rel", "noopener noreferrer")
                                .text("Transaction")
                                .child(el("span").class("sh-vh").text(" (opens the explorer)")),
                            None => el("span")
                                .class("sh-mono")
                                .text(crate::format::short_hash(&entry.tx)),
                        })),
                )
        })))
        .into()
}

/// What one EVM wallet's reading adds to the page's totals.
fn summary_of(loaded: &Loaded) -> Summary {
    let (positions, open_claims, extras) = parts(loaded);
    Summary {
        value_cents: positions.iter().map(|row| row.value_cents).sum(),
        positions: positions.len(),
        booked: open_claims.len(),
        paid_cents: (!positions.is_empty())
            .then(|| positions.iter().map(|row| row.paid_cents).sum())
            .flatten(),
        holds: !positions.is_empty() || !open_claims.is_empty() || !extras.is_empty(),
    }
}

/// Read the wallet's holdings: now, whenever the account changes or a flow
/// lands, and every 15 s.
async fn follow(loaded: Mutable<Option<Loaded>>) {
    #[cfg(target_arch = "wasm32")]
    {
        let session = crate::wallet::get();
        loop {
            // Read again when a flow lands, the account changes or a
            // chain's settings change: every card is then drawn with the
            // network a send would sign with.
            let seen = (session.changed.get(), session.account(), session.configs());
            if let Some(account) = seen.1 {
                // A reading that reached no chain at all keeps what was
                // shown (or the skeleton), and is tried again.
                if let Some(result) = read(account).await
                    && session.account() == Some(account)
                {
                    loaded.set(Some(result));
                }
            } else {
                loaded.set(None);
            }
            for _ in 0..15 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (session.changed.get(), session.account(), session.configs()) != seen {
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

/// Every EVM chain's holdings for `account`, the chains read side by side;
/// `None` if no chain could be read at all.
#[cfg(target_arch = "wasm32")]
async fn read(account: alloy_primitives::Address) -> Option<Loaded> {
    let session = crate::wallet::get();
    let vaults = session.read_vaults().await;
    let chains: std::collections::BTreeSet<shared::Chain> = vaults
        .values()
        .map(|vault| vault.chain)
        .filter(|chain| *chain != shared::Chain::Solana)
        .collect();
    let parts = futures::future::join_all(
        chains
            .into_iter()
            .map(|chain| read_chain(chain, &vaults, account)),
    )
    .await;
    if !parts.is_empty() && parts.iter().all(Option::is_none) {
        return None;
    }
    let mut loaded = Loaded::default();
    for part in parts.into_iter().flatten() {
        loaded.rows.extend(part.rows);
        loaded.claims.extend(part.claims);
        loaded.balances.extend(part.balances);
    }
    Some(loaded)
}

/// One chain's holdings for `account`: its dollars, then each vault, read
/// side by side; `None` if the chain couldn't be reached.
#[cfg(target_arch = "wasm32")]
async fn read_chain(
    chain: shared::Chain,
    vaults: &std::collections::BTreeMap<String, Rc<VaultRef>>,
    account: alloy_primitives::Address,
) -> Option<Loaded> {
    use crate::tx::port::Reader;
    use crate::tx::read;
    use crate::wallet::session::Session;
    let session = crate::wallet::get();
    let mut loaded = Loaded::default();
    let address = format!("{account:?}");
    let network = session.network(chain).await?;
    let reply =
        gloo_net::http::Request::get(&format!("{}/{chain}/portfolio/{address}", crate::api::BASE))
            .send()
            .await
            .ok()
            .filter(|reply| reply.ok());
    let portfolio: Option<shared::Portfolio> = match reply {
        Some(reply) => reply.json().await.ok(),
        None => None,
    };
    let reader = Session::reader(&network);
    // The chain's own answer for the wallet's dollars; a chain that doesn't
    // answer it isn't read.
    let balance = read::asset_balance(&reader, network.asset, account)
        .await
        .ok()?;
    loaded.balances.push(Balance {
        chain: networks::chain_name(network.chain_id),
        dollars: money::dollars(balance, network.asset_decimals, Rounding::Down),
    });
    let now = reader.chain_time().await.unwrap_or_default();
    let rows = futures::future::join_all(
        vaults
            .values()
            .filter(|vault| vault.chain == chain)
            .map(|vault| read_row(&reader, &network, vault, account, now, portfolio.as_ref())),
    )
    .await;
    loaded.rows.extend(rows.into_iter().flatten());
    if let Some(portfolio) = portfolio {
        for claim in portfolio.claims {
            if let Some(vault) = vaults.get(&claim.slug) {
                let closed = matches!(read::state(&reader, vault.address).await, Ok(State::Closed));
                loaded.claims.push(ClaimRow {
                    vault: vault.clone(),
                    network: network.clone(),
                    claim,
                    closed,
                });
            }
        }
    }
    Some(loaded)
}

/// One vault's row for `account`, if it holds anything there.
#[cfg(target_arch = "wasm32")]
async fn read_row(
    reader: &crate::wallet::rpc::RpcReader<crate::wallet::rpc::Endpoint>,
    network: &Rc<Network>,
    vault: &Rc<VaultRef>,
    account: alloy_primitives::Address,
    now: u64,
    portfolio: Option<&shared::Portfolio>,
) -> Option<Row> {
    use crate::tx::port::Reader;
    use crate::tx::read;
    let address = format!("{account:?}");
    let held = read::holdings(reader, vault.address, network.asset, account)
        .await
        .ok()?;
    let waiting: Vec<shared::Request> = vault
        .requests
        .iter()
        .filter(|request| request.owner.eq_ignore_ascii_case(&address))
        .cloned()
        .collect();
    if held.shares.is_zero() && held.units.is_zero() && held.held.is_zero() && waiting.is_empty() {
        return None;
    }
    let figures = read::figures(reader, vault.address).await.ok()?;
    // The fallback's conditions are the contract's (a deposit goes back
    // only while nothing could price it): ask the chain whether it would
    // take it, for each request whose day has passed.
    let mut fallbacks = Vec::new();
    for request in &waiting {
        let earliest = request.earliest.assume_utc().unix_timestamp().max(0) as u64;
        let priced = figures
            .last
            .as_ref()
            .is_some_and(|last| last.time >= earliest);
        if priced
            || figures.state == State::Closed
            || now < earliest.saturating_add(figures.timers.fallback_after)
            || !matches!(
                request.kind,
                shared::RequestKind::Deposit | shared::RequestKind::Withdrawal
            )
        {
            continue;
        }
        let attempt = crate::tx::port::TxRequest {
            chain_id: network.chain_id,
            from: account,
            to: vault.address,
            data: alloy_sol_types::SolCall::abi_encode(
                &chain::bindings::vault::IVault::takeProRataCall {
                    requestId: U256::from(request.id),
                },
            )
            .into(),
            gas: None,
        };
        let answer = match reader.estimate_gas(&attempt).await {
            Ok(_) => Ok(()),
            Err(crate::tx::TxError::WouldRevert { reason }) => Err(reason),
            // Unread: offered, and the flow's own estimate decides.
            Err(_) => Ok(()),
        };
        fallbacks.push((request.id, answer));
    }
    let value = figures
        .share_price()
        .and_then(|price| crate::money::quote::value_of(held.shares, price))
        .unwrap_or_default();
    let paid_cents = portfolio
        .and_then(|p| p.positions.iter().find(|h| h.slug == vault.slug))
        .filter(|holding| holding.cost.is_finite() && holding.cost >= 0.0)
        .map(|holding| (holding.cost * 100.0).round() as u128);
    Some(Row {
        vault: vault.clone(),
        network: network.clone(),
        value_cents: cents(value, network.asset_decimals),
        figures,
        held,
        paid_cents,
        waiting,
        fallbacks,
        now,
    })
}

/// A reading's positions, booked withdrawals not yet paid, and waiting
/// requests, units or held payouts.
fn parts(loaded: &Loaded) -> (Vec<&Row>, Vec<&ClaimRow>, Vec<&Row>) {
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
    (positions, open_claims, extras)
}

/// The EVM wallet's section, once read: its dollars, then its positions,
/// booked withdrawals and anything waiting; one quiet line if it holds
/// nothing in agents.
fn evm_content(loaded: &Loaded) -> View {
    let (positions, open_claims, extras) = parts(loaded);
    let body = el("div")
        .class("sh-portfolio__body")
        .child(balances_view(&loaded.balances));
    if positions.is_empty() && open_claims.is_empty() && extras.is_empty() {
        return body
            .child(
                el("p")
                    .class("sh-muted")
                    .text("Nothing in agents from this wallet."),
            )
            .into();
    }
    let total: u128 = positions.iter().map(|row| row.value_cents).sum();
    let paid: Option<u128> = positions.iter().map(|row| row.paid_cents).sum();
    body.child(tank_warnings(&positions))
        .child(if positions.is_empty() {
            View::Empty
        } else {
            View::Many(vec![
                positions_table(&positions, total, paid),
                positions_cards(&positions),
            ])
        })
        .child(if open_claims.is_empty() {
            View::Empty
        } else {
            claims_list(&open_claims)
        })
        .child(if extras.is_empty() {
            View::Empty
        } else {
            extras_list(&extras)
        })
        .into()
}

fn signed_cents(value: u128, paid: u128) -> String {
    if value >= paid {
        format!("+{}", dollars_from_cents(value - paid))
    } else {
        format!("{}{}", money::MINUS, dollars_from_cents(paid - value))
    }
}

/// Gain as a share of what was paid in: "+4.2%".
fn gain_percent(value: u128, paid: u128) -> Option<String> {
    (paid > 0).then(|| crate::format::ret((value as f64 - paid as f64) / paid as f64))
}

/// For each held agent still running whose tank is low, the plain warning:
/// read from the agent's own record, as its page reads it.
fn tank_warnings(rows: &[&Row]) -> View {
    let store = crate::store::get();
    let notices: Vec<View> = rows
        .iter()
        .filter(|row| matches!(row.figures.state, State::Open | State::WindingDown))
        .filter_map(|row| {
            let agent = store.agent(&row.vault.slug)?;
            let tank = crate::model::TankState::of(&agent.strategy.get_cloned());
            tank.is_low().then(|| {
                let left = if tank.days < 1.0 {
                    "less than a day".to_string()
                } else {
                    format!("about {}", crate::format::days(tank.days.floor() as u32))
                };
                notice(
                    NoticeTone::Caution,
                    text(format!(
                        "{}'s tank has {left} left. If it runs dry, the agent stops and its positions sit unmanaged. You can still leave whenever you like.",
                        row.vault.title
                    )),
                )
                .into()
            })
        })
        .collect();
    View::Many(notices)
}

/// Phone: each position as a card (Portfolio-Mobile): the agent, its chain
/// and status; Value and Gain first, then Shares and Share price; what was
/// paid in as a footnote; Deposit and Withdraw, each opening its sheet.
fn positions_cards(rows: &[&Row]) -> View {
    let cards = rows.iter().map(|row| {
        let decimals = row.network.asset_decimals;
        let price = row.figures.share_price();
        let open = row.figures.state == State::Open;
        let figure = |label: &'static str, value: String| {
            el("div")
                .child(el("dt").class("sh-label").text(label))
                .child(el("dd").class("sh-num").text(value))
        };
        let gain = row
            .paid_cents
            .map(|paid| {
                let amount = signed_cents(row.value_cents, paid);
                match gain_percent(row.value_cents, paid) {
                    Some(percent) => format!("{amount} · {percent}"),
                    None => amount,
                }
            })
            .unwrap_or_else(|| "—".to_string());
        let slug = row.vault.slug.clone();
        let deposit = {
            let slug = slug.clone();
            on_click(
                Button::new(Kind::Secondary, "Deposit")
                    .size(Size::Small)
                    .build(),
                move || {
                    crate::components::exit::open_sheet(
                        &slug,
                        crate::components::exit::Tab::Deposit,
                    )
                },
            )
        };
        let withdraw = on_click(
            Button::new(Kind::Secondary, "Withdraw")
                .size(Size::Small)
                .build(),
            move || {
                crate::components::exit::open_sheet(&slug, crate::components::exit::Tab::Withdraw)
            },
        );
        el("li")
            .class("sh-card sh-poscard")
            .child(
                el("div")
                    .class("sh-poscard__head")
                    .child(
                        el("div")
                            .child(
                                el("h3").class("sh-poscard__name").child(
                                    app_link(format!("/agents/{}", row.vault.slug))
                                        .text(row.vault.title.clone()),
                                ),
                            )
                            .child(
                                el("p")
                                    .class("sh-portfolio__chain sh-muted")
                                    .child(tile(&row.network.label))
                                    .text(format!(
                                        " {} · {}",
                                        row.network.label,
                                        if row.vault.served { "Served" } else { "Direct" }
                                    )),
                            ),
                    )
                    .child(status_badge(row.figures.state)),
            )
            .child(
                el("dl")
                    .class("sh-poscard__figures")
                    .child(figure("Value", dollars_from_cents(row.value_cents)))
                    .child(figure("Gain", gain))
                    .child(figure(
                        "Shares",
                        money::shares(row.held.shares, Rounding::Down),
                    ))
                    .child(figure(
                        "Share price",
                        price
                            .map(|p| money::price(p, decimals))
                            .unwrap_or_else(|| "—".to_string()),
                    )),
            )
            .child(View::from(row.paid_cents.map(|paid| {
                el("p")
                    .class("sh-poscard__paid sh-muted")
                    .text(format!("Paid in {}", dollars_from_cents(paid)))
                    .into()
            })))
            .child(
                el("div")
                    .class("sh-poscard__actions")
                    .child(View::from(open.then(|| deposit.into())))
                    .child(withdraw),
            )
    });
    el("ul")
        .class("sh-poscards")
        .attr("aria-label", "Your positions")
        .children(cards)
        .into()
}

fn status_badge(state: State) -> El {
    match state {
        State::Open => badge(Tone::Gain, None, "Live"),
        State::WindingDown => badge(Tone::Caution, None, "Winding down"),
        State::Frozen => badge(Tone::Caution, None, "Frozen"),
        State::Killed => badge(Tone::Loss, None, "Killed"),
        State::Closed => badge(Tone::Neutral, None, "Closed"),
        _ => badge(Tone::Neutral, None, "Not open"),
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
        let status = status_badge(row.figures.state);
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
                            .map(|paid| match gain_percent(row.value_cents, paid) {
                                Some(percent) => {
                                    format!("{} · {percent}", signed_cents(row.value_cents, paid))
                                }
                                None => signed_cents(row.value_cents, paid),
                            })
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
        .child(el("h3").class("sh-portfolio__h3").text("Your positions"))
        .child(
            el("div").class("sh-table-wrap").child(
                el("table")
                    .class("sh-table")
                    .child(el("caption").class("sh-vh").text("Your positions"))
                    .child(
                        el("thead").child(el("tr").children(head.iter().enumerate().map(
                            |(i, label)| {
                                let th = el("th")
                                    .attr("scope", "col")
                                    .class_if((1..=5).contains(&i), "sh-r");
                                // The actions column: named for a screen
                                // reader, blank on screen.
                                if label.is_empty() {
                                    th.child(el("span").class("sh-vh").text("Actions"))
                                } else {
                                    th.text(*label)
                                }
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

pub(crate) fn claim_badge(state: ClaimState) -> El {
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

pub(crate) fn shared_state(state: shared::ClaimState) -> ClaimState {
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
            el("h3")
                .class("sh-portfolio__h3")
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
            el("h3")
                .class("sh-portfolio__h3")
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
        let refused = row
            .fallbacks
            .iter()
            .find(|(id, _)| *id == request.id)
            .and_then(|(_, answer)| answer.as_ref().err());
        let offer = waiting_offer(
            request.kind,
            earliest,
            row.figures.state,
            row.figures.ready_to_close,
            row.figures.last.as_ref().map(|last| last.time),
            row.figures.timers.fallback_after,
            row.now,
            refused,
        );
        let fee_bps = row.figures.fees.transaction_bps;
        card = card.child(el("p").text(waiting_words(request, &offer)));
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
                .child(el("p").class("sh-muted").text(format!("Nothing has priced it for a day. {}", cash_share_words(fee_bps))))
                .child(act(vault, network, Action::TakeCash(Fallback::Request(id)), "Take cash share now", Kind::Primary))
                .child(act(vault, network, Action::ClaimInstead(id), "Make it a claim instead", Kind::Secondary)),
            Offer::NotNow(why) => card.child(el("p").class("sh-muted").text(why)),
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// The day has passed, but the vault wouldn't take the fallback now:
    /// why, in words.
    NotNow(String),
}

/// The one door the contract opens for a waiting request of this kind on a
/// vault in this state. Settling once priced is open for a deposit, a
/// withdrawal and a claim's own request (which pays its claim); a tank fill
/// is the author's, settled with the vault's own. The fallback a day after
/// the request's slot is offered only when the chain says it would take it
/// (`refused` is its refusal): a deposit goes back only while nothing could
/// price it, so a vault whose prices can still be read keeps it waiting.
#[allow(clippy::too_many_arguments)]
fn waiting_offer(
    kind: shared::RequestKind,
    earliest: u64,
    state: State,
    ready_to_close: bool,
    last_priced: Option<u64>,
    fallback_after: u64,
    now: u64,
    refused: Option<&Revert>,
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
        return match kind {
            K::Deposit | K::Withdrawal | K::Claim => Offer::SettleNow,
            K::TankFill => Offer::Nothing,
        };
    }
    if now < earliest.saturating_add(fallback_after)
        || matches!(state, State::Cancelled | State::None)
    {
        return Offer::Nothing;
    }
    let offer = match kind {
        K::Deposit => Offer::TakeBack,
        K::Withdrawal if !ready_to_close => Offer::ProRata,
        _ => return Offer::Nothing,
    };
    match refused {
        None => offer,
        Some(Revert::Vault("NotAllowed")) if kind == K::Deposit => Offer::NotNow(
            "Nothing has priced it for a day, but the vault’s prices can still be read, so the next valuation can price it: it waits for that rather than going back. A deposit can be taken back only while nothing could price it."
                .to_string(),
        ),
        // A withdrawal priced or settled since this page last read it.
        Some(Revert::Vault("NotAllowed")) => Offer::NotNow(
            "It has been priced or settled since this page last looked, so the fallback isn’t open for it. Reload to see where it stands."
                .to_string(),
        ),
        Some(Revert::Vault("NotYet")) => Offer::NotNow(
            "Nothing has priced it for a day. The vault is settling a valuation just now, so the fallback isn’t open this moment; it may be priced instead."
                .to_string(),
        ),
        Some(reason) => Offer::NotNow(format!(
            "Nothing has priced it for a day, but the vault won’t take the fallback now: {}",
            reason.words()
        )),
    }
}

/// The fallback on a withdrawal or a claim, with the fees the contract
/// takes from it: the profit fee the shares owed at the last priced
/// valuation (as a slice of them) and the transaction fee on the cash paid.
fn cash_share_words(transaction_bps: u16) -> String {
    format!(
        "You can take your share of the vault’s cash now, less the profit fee your shares owed at the last price and the {} transaction fee. The rest becomes units on what the stations hold, paid as the money comes home.",
        crate::format::bps(u32::from(transaction_bps))
    )
}

/// A waiting request in words.
fn waiting_words(request: &shared::Request, offer: &Offer) -> String {
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
    if *offer == Offer::SettleNow {
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
/// where needed. The flow is kept by the session per vault, account and
/// action, so the card shows it again when the page is redrawn while it
/// runs, and never offers to start it twice.
fn act(
    vault: &Rc<VaultRef>,
    network: &Rc<Network>,
    action: Action,
    label: &'static str,
    kind: Kind,
) -> El {
    let session = crate::wallet::get();
    let vault = vault.clone();
    let network = network.clone();
    let chain_id = network.chain_id;
    let slot = action.slot();
    el("div").class("sh-act").child_signal(
        None,
        futures_signals::map_ref! {
            let connection = session.connection.signal_cloned(),
            let current = session.flow_signal(&vault.slug, &slot) => (connection.clone(), current.clone())
        }
        .map(move |(connection, current)| {
            let wrong = matches!(connection, Connection::Connected { chain_id: Some(on), .. } if on != chain_id)
                && crate::wallet::get().is_browser();
            if connection.account().is_none() {
                return Some(on_click(Button::new(kind, "Connect wallet").size(Size::Small).build(), open_connect).into());
            }
            if let Some(current) = current {
                return Some(status(&current, &network));
            }
            if wrong {
                return Some(switch_button(chain_id, Size::Small).into());
            }
            let vault = vault.clone();
            let action = action.clone();
            let reviewed = network.clone();
            Some(
                on_click(Button::new(kind, label).size(Size::Small).build(), move || {
                    // One flow at a time per action: a second click starts
                    // nothing. It signs with the network this card was
                    // read and drawn with, or not at all.
                    let flow = Running::new(action.clone(), reviewed.clone());
                    if crate::wallet::get().begin(&vault.slug, &flow) {
                        crate::wallet::run::start(&vault, action.clone(), &flow);
                    }
                })
                .into(),
            )
        }),
    )
}

/// What a flow from the portfolio ended as, in a line.
fn done_line(outcome: &Outcome, network: &Network) -> String {
    let session = crate::wallet::get();
    let decimals = network.asset_decimals;
    match outcome {
        Outcome::Collected { assets, .. } | Outcome::TookCash { assets, .. } => {
            format!(
                "{} is in your wallet.",
                money::dollars(*assets, decimals, Rounding::Down)
            )
        }
        Outcome::Claim {
            state: ClaimState::Paid,
            paid,
            owner,
            ..
        } => format!(
            "Paid: {}.",
            paid_headline(
                &money::dollars(*paid, decimals, Rounding::Down),
                *owner,
                session.account()
            )
        ),
        Outcome::Claim {
            state: ClaimState::Waiting,
            ..
        } => "The market was outside the exit band, so nothing closed. It can be tried again."
            .to_string(),
        // From its own request: the cash no longer covered it when it was
        // tried, so it stays booked for a close.
        Outcome::Claim {
            state: ClaimState::Pending,
            ..
        } => "The cash at home no longer covers it, so it stays booked: settling closes your share of the positions."
            .to_string(),
        Outcome::Claim {
            state: ClaimState::Queued,
            ..
        } => "The cash covers it: it’s priced at the next valuation.".to_string(),
        Outcome::ClaimMade { claim, .. } => format!("Claim {claim} booked."),
        Outcome::Refunded { assets, to, .. } => {
            let assets = money::dollars(*assets, decimals, Rounding::Down);
            if session.account() == Some(*to) {
                format!("Your deposit is back: {assets} is in your wallet.")
            } else {
                format!("Your deposit went back: {assets} to the wallet that paid it in.")
            }
        }
        Outcome::Deposited { shares, .. } => {
            format!(
                "Settled: you now hold {} more shares.",
                money::shares(*shares, Rounding::Down)
            )
        }
        Outcome::Booked { claim, .. } => format!("Settled: booked as claim {claim}."),
        Outcome::Paid { assets, .. } => {
            format!(
                "{} is in your wallet.",
                money::dollars(*assets, decimals, Rounding::Down)
            )
        }
        Outcome::SettledBefore(how) => format!(
            "It was already settled, so nothing was sent. {}",
            done_line(how, network)
        ),
        _ => "Done.".to_string(),
    }
}

fn status(current: &Running, network: &Rc<Network>) -> View {
    let network = network.clone();
    let shown = current.clone();
    el("div")
        .class("sh-act__status")
        .child_signal(
            None,
            current.step.signal_cloned().map(move |step| {
                let session = crate::wallet::get();
                let wallet = session.wallet_name.get_cloned();
                let dismiss = |label: &'static str| {
                    let shown = shown.clone();
                    on_click(
                        el("button").attr("type", "button").class("sh-link").text(label),
                        move || crate::wallet::get().dismiss(&shown),
                    )
                };
                Some(match step? {
                    Step::Checking => el("p").attr("aria-live", "polite").text("Checking…").into(),
                    Step::Signing(_) => el("p").attr("aria-live", "polite").text(format!("Approve in {wallet}…")).into(),
                    Step::Sent { prompt: Prompt::Finish, .. } => el("p")
                        .attr("aria-live", "polite")
                        .text("Second step sent. It’s optional: the request is settled at its price either way. Watching for it…")
                        .into(),
                    Step::Sent { unseen: true, .. } => el("p")
                        .attr("aria-live", "polite")
                        .text(format!("Sent. Not seen by the network yet; it may still go through. Check {wallet}’s activity before sending anything again."))
                        .into(),
                    Step::Sent { .. } | Step::Requested(_) | Step::Priced { .. } => {
                        el("p").attr("aria-live", "polite").text("Sent. Waiting for the network…").into()
                    }
                    Step::Done(outcome) => el("div")
                        .attr("role", "status")
                        .child(
                            el("p")
                                .class("sh-gain")
                                .child(icon(Icon::Check, 16))
                                .text(format!(" {}", done_line(&outcome, &network))),
                        )
                        .child(dismiss("Done"))
                        .into(),
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
                            .child(dismiss(again))
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
    if crate::tx::solana::run::is_solana(slug) {
        return crate::components::solana::portfolio::claim(slug, id);
    }
    let loaded: Mutable<Option<Result<ClaimView, String>>> = Mutable::new(None);
    let body = el("div")
        .class("sh-portfolio sh-claim")
        .child(
            el("nav")
                .class("sh-claim__crumbs")
                .attr("aria-label", "Breadcrumb")
                .child(app_link("/portfolio").text("Portfolio"))
                .text(" / ")
                .child(
                    el("span").attr("aria-current", "page").text_from(
                        &loaded,
                        |loaded| match loaded {
                            Some(Ok(view)) => format!("Claim on {}", view.vault.title),
                            _ => "Claim".to_string(),
                        },
                    ),
                ),
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
            let seen = (session.changed.get(), session.configs());
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
                if (session.changed.get(), session.configs()) != seen {
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
            &view.figures.timers,
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
                .child(
                    el("h2")
                        .class("sh-portfolio__h2")
                        .text("No price for 24 hours"),
                )
                .child(el("p").text(cash_share_words(view.figures.fees.transaction_bps)))
                .child(act(
                    &view.vault,
                    &view.network,
                    Action::TakeCash(Fallback::Claim(U256::from(view.id))),
                    "Take cash share now",
                    Kind::Primary,
                ));
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
    // Nothing left to do here once it's paid or taken: back to the rest.
    if matches!(state, ClaimState::Paid | ClaimState::Fallback) {
        actions = actions.child(
            app_link("/portfolio")
                .class("sh-btn sh-btn-primary sh-claim__back")
                .text("View in portfolio"),
        );
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
                .child(match (state, viewer == Some(claim.owner)) {
                    // An address keeps its mono face, even in a headline.
                    (ClaimState::Paid, false) => el("h1")
                        .class("sh-portfolio__title")
                        .text(format!(
                            "{} was paid to ",
                            money::dollars(claim.paid, decimals, Rounding::Down)
                        ))
                        .child(
                            el("span")
                                .class("sh-mono")
                                .text(crate::format::short_hash(&format!("{:?}", claim.owner))),
                        ),
                    _ => el("h1").class("sh-portfolio__title").text(headline),
                })
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

    fn holding(value_cents: u128) -> Summary {
        Summary {
            value_cents,
            positions: 1,
            booked: 0,
            paid_cents: None,
            holds: true,
        }
    }

    #[test]
    fn the_empty_state_shows_only_when_no_connected_wallet_holds_anything() {
        let nothing = Summary::default();
        // The EVM side of a wallet holds nothing, its Solana side holds
        // shares: the totals, never "You haven't deposited yet".
        assert_eq!(
            top(&[Some(Some(nothing)), Some(Some(holding(148_000)))], false),
            Top::Totals {
                value_cents: 148_000,
                positions: 1,
                booked: 0,
                paid_cents: None
            }
        );
        // Every connected wallet read and holding nothing: the empty state.
        assert_eq!(
            top(&[Some(Some(nothing)), Some(Some(nothing))], false),
            Top::Empty { before: false }
        );
        assert_eq!(
            top(&[Some(Some(nothing)), None], false),
            Top::Empty { before: false }
        );
        // Withdrawn from since: never "haven't deposited".
        assert_eq!(
            top(&[Some(Some(nothing)), None], true),
            Top::Empty { before: true }
        );
        // A wallet still being read: never the empty state yet.
        assert_eq!(top(&[Some(Some(nothing)), Some(None)], false), Top::Reading);
        assert_eq!(top(&[None, None], false), Top::SignedOut);
        // Across wallets the totals add up; what was paid in only where
        // every holding wallet knows it.
        let mut evm = holding(10_000);
        evm.paid_cents = Some(9_000);
        evm.booked = 1;
        assert_eq!(
            top(&[Some(Some(evm)), Some(Some(holding(5_000)))], false),
            Top::Totals {
                value_cents: 15_000,
                positions: 2,
                booked: 1,
                paid_cents: None
            }
        );
        assert_eq!(
            top(&[Some(Some(evm)), Some(Some(nothing))], false),
            Top::Totals {
                value_cents: 10_000,
                positions: 1,
                booked: 1,
                paid_cents: Some(9_000)
            }
        );
    }

    #[test]
    fn each_wallet_section_is_headed_by_its_chain_family_app_and_short_address() {
        use crate::wallet::recent::Family;
        assert_eq!(
            wallet_heading(Family::Evm, "Phantom", "0xed47…b3c0"),
            "EVM · Phantom · 0xed47…b3c0"
        );
        assert_eq!(
            wallet_heading(Family::Solana, "Phantom", "B16P…r9Kp"),
            "Solana · Phantom · B16P…r9Kp"
        );
        let html = View::Many(vec![wallet_section(
            Family::Evm,
            "MetaMask",
            "0xb0b7",
            "0xb0b7…b448".to_string(),
            View::Empty,
        )])
        .html();
        assert!(
            html.contains("aria-label=\"EVM · MetaMask · 0xb0b7…b448\""),
            "{html}"
        );
        assert!(html.contains("data-wallet=\"evm\""), "{html}");
    }

    #[test]
    fn an_evm_wallet_holding_nothing_says_so_in_one_quiet_line_beside_its_dollars() {
        let loaded = Loaded {
            balances: vec![Balance {
                chain: "Base Sepolia".to_string(),
                dollars: "$4,960.00".to_string(),
            }],
            ..Loaded::default()
        };
        let html = View::Many(vec![evm_content(&loaded)]).html();
        assert!(
            html.contains("Nothing in agents from this wallet."),
            "{html}"
        );
        assert!(!html.contains("haven’t deposited"), "{html}");
        assert!(!html.contains("Browse agents"), "{html}");
        assert!(html.contains("Dollars in this wallet"), "{html}");
        assert!(html.contains(">On Base Sepolia<"), "{html}");
        assert!(html.contains(">$4,960.00<"), "{html}");
        assert_eq!(summary_of(&loaded), Summary::default());
    }

    #[test]
    fn this_browsers_recent_sends_say_paid_or_pending_and_that_they_are_this_browsers_only() {
        use crate::wallet::recent::{Entry, Family, Kind};
        let entry = |tx: &str, kind, outcome: Option<&str>| Entry {
            tx: tx.to_string(),
            tx_url: Some(format!("https://sepolia.basescan.org/tx/{tx}")),
            family: Family::Evm,
            account: "0x7e".to_string(),
            slug: "base-2".to_string(),
            title: "Funding basis".to_string(),
            kind,
            amount: "12.5 shares".to_string(),
            at_ms: 1_760_000_000_000.0,
            outcome: outcome.map(str::to_string),
        };
        let html = recent_view(&[
            entry("0xaa", Kind::Withdrawal, Some("Paid $251.20")),
            entry("0xbb", Kind::Withdrawal, None),
        ])
        .html();
        assert!(html.contains("Recent in this browser"), "{html}");
        assert!(
            html.contains("Only what was sent from this browser."),
            "{html}"
        );
        assert!(
            html.contains("Withdrawal of 12.5 shares from Funding basis"),
            "{html}"
        );
        assert!(html.contains("Paid $251.20"), "{html}");
        assert!(
            html.contains("Pending, priced at the next valuation"),
            "{html}"
        );
        assert!(
            html.contains("href=\"https://sepolia.basescan.org/tx/0xaa\""),
            "{html}"
        );
    }

    #[test]
    fn a_gain_reads_as_a_share_of_what_was_paid_in() {
        assert_eq!(gain_percent(104_200, 100_000).as_deref(), Some("+4.2%"));
        assert_eq!(
            gain_percent(99_000, 100_000).as_deref(),
            Some("\u{2212}1.0%")
        );
        assert_eq!(
            gain_percent(500, 0),
            None,
            "nothing paid in has no percentage"
        );
    }

    #[test]
    fn a_claim_the_cash_no_longer_covered_is_never_said_to_be_outside_the_exit_band() {
        let network = Network {
            chain: shared::Chain::Base,
            chain_id: 8453,
            label: "Base".to_string(),
            rpc: String::new(),
            asset: alloy_primitives::Address::repeat_byte(0xdd),
            asset_symbol: "USDC".to_string(),
            asset_decimals: 6,
            dev_account: None,
            config: crate::wallet::config_cache::ConfigId::default(),
        };
        let claim = |state| Outcome::Claim {
            claim: U256::from(1u8),
            owner: alloy_primitives::Address::repeat_byte(0x42),
            state,
            paid: U256::ZERO,
            hash: alloy_primitives::B256::ZERO,
        };
        let pending = done_line(&claim(ClaimState::Pending), &network);
        assert!(!pending.contains("exit band"), "{pending}");
        assert!(pending.contains("no longer covers"), "{pending}");
        let waiting = done_line(&claim(ClaimState::Waiting), &network);
        assert!(waiting.contains("exit band"), "{waiting}");
    }

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
            waiting_offer(kind, 1_000, state, ready, priced, day, now, None)
        };
        // Priced and not yet settled: the second step, for a deposit, a
        // withdrawal and a claim's own request (it pays the claim); a tank
        // fill is the author's.
        for kind in [K::Deposit, K::Withdrawal, K::Claim] {
            assert_eq!(
                offer(kind, State::Open, false, Some(1_000), 1_001),
                Offer::SettleNow
            );
        }
        assert_eq!(
            offer(K::TankFill, State::Open, false, Some(1_000), 1_001),
            Offer::Nothing
        );
        // Nothing priced for a day, and the chain would take the fallback:
        // a deposit goes back, a withdrawal takes its cash share or becomes
        // a claim; a claim's own request and a tank fill are offered nothing.
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
    fn a_deposit_is_offered_back_only_when_the_chain_would_take_it_and_says_why_not_otherwise() {
        use shared::RequestKind as K;
        let day = 86_400;
        let late = 1_000 + day;
        let offer = |kind, refused: Option<Revert>| {
            waiting_offer(
                kind,
                1_000,
                State::Open,
                false,
                None,
                day,
                late,
                refused.as_ref(),
            )
        };
        // The day passed only because nobody recorded a valuation: the
        // vault's prices can still be read, so it refuses to send the
        // deposit back. Nothing is offered; the card says why, and never
        // that the owner isn't the owner.
        let Offer::NotNow(why) = offer(K::Deposit, Some(Revert::Vault("NotAllowed"))) else {
            panic!("a refused fallback was offered");
        };
        assert!(why.contains("can still be read"), "{why}");
        assert!(!why.contains("Only the owner"), "{why}");
        // A withdrawal priced since the page looked: never "Only the owner".
        let Offer::NotNow(why) = offer(K::Withdrawal, Some(Revert::Vault("NotAllowed"))) else {
            panic!("a refused fallback was offered");
        };
        assert!(!why.contains("Only the owner"), "{why}");
        // A valuation settling just now: not this moment.
        let Offer::NotNow(why) = offer(K::Withdrawal, Some(Revert::Vault("NotYet"))) else {
            panic!("a refused fallback was offered");
        };
        assert!(why.contains("settling a valuation"), "{why}");
        // The chain would take it: offered.
        assert_eq!(offer(K::Deposit, None), Offer::TakeBack);
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
    fn the_cash_share_fallback_names_the_fees_the_contract_takes() {
        let said = cash_share_words(10);
        assert!(said.contains("profit fee"), "{said}");
        assert!(said.contains("0.1% transaction fee"), "{said}");
        assert!(!said.contains("exact share"), "{said}");
    }

    #[test]
    fn a_gain_is_signed_with_a_true_minus() {
        assert_eq!(signed_cents(1_500_283, 1_455_000), "+$452.83");
        assert_eq!(signed_cents(100, 250), "\u{2212}$1.50");
        assert_eq!(signed_cents(100, 100), "+$0.00");
    }
}
