//! The portfolio's Solana part, and a Solana vault's claim page: the same
//! sections as the EVM portfolio (positions, booked withdrawals, waiting
//! requests, units and held payouts), read from each vault's accounts on the
//! cluster for the connected Solana wallet. Each row's value is rounded down
//! to the cent once, and the total is the sum of those cents.

use std::rc::Rc;

use alloy_primitives::U256;
use chain::solana_ix::Pubkey;
use chain::solana_ix::accounts::{
    Claim, ClaimState, Holder, Request, RequestKind, Vault, VaultState,
};
use futures_signals::signal::{Mutable, SignalExt};
use ui::icons::Icon;

use super::exit::{short, sig_link, sol_error_words};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::wallet::open_connect;
use crate::money::format as money;
use crate::money::{Rounding, units::pow10};
use crate::pages::Page;
use crate::tx::solana::flow::{Fallback, HeldAt, Outcome, Step};
use crate::tx::solana::run::{SolAction, SolRunning, SolVault};
use crate::view::{El, View, el, text};
use crate::wallet::solana::SolNetwork;
use crate::web::app_link;

/// One Solana vault the wallet has something in.
#[derive(Clone)]
struct Row {
    vault: SolVault,
    account: Rc<Vault>,
    holder: Option<Holder>,
    value_cents: u128,
    /// This wallet's requests still waiting, with their addresses.
    waiting: Vec<(Pubkey, Request)>,
    /// Payouts held for this wallet on requests.
    held: Vec<(Pubkey, Request)>,
    now: u64,
}

#[derive(Clone)]
struct ClaimRow {
    vault: SolVault,
    claim: Claim,
}

#[derive(Clone, Default)]
struct Loaded {
    rows: Vec<Row>,
    claims: Vec<ClaimRow>,
    network: Option<Rc<SolNetwork>>,
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

/// The portfolio's Solana section: nothing while no Solana wallet is
/// connected.
pub fn section() -> View {
    let session = crate::wallet::solana::get();
    let loaded: Mutable<Option<Loaded>> = Mutable::new(None);
    el("section")
        .class("sh-portfolio__section sh-portfolio__solana")
        .attr("aria-label", "On Solana")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let account = session.connection.signal_ref(|c| c.account()),
                let loaded = loaded.signal_cloned() => (*account, loaded.clone())
            }
            .map(|(account, loaded)| {
                let account = account?;
                Some(match loaded {
                    None => el("p")
                        .class("sh-muted")
                        .text("Reading your Solana wallet…")
                        .into(),
                    Some(loaded) => holdings_view(&account, &loaded),
                })
            }),
        )
        .hook(move |dom| dom.future(follow(loaded)))
        .into()
}

async fn follow(loaded: Mutable<Option<Loaded>>) {
    #[cfg(target_arch = "wasm32")]
    {
        let session = crate::wallet::solana::get();
        loop {
            let account = session.account();
            match account {
                Some(account) => {
                    if let Some(read) = read(account).await {
                        loaded.set(Some(read));
                    }
                }
                None => loaded.set(None),
            }
            let seen = (session.changed.get(), session.account());
            for _ in 0..15 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (session.changed.get(), session.account()) != seen {
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = loaded;
}

/// Every Solana vault the store knows.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn solana_vaults() -> Vec<SolVault> {
    let store = crate::store::get();
    store
        .all()
        .iter()
        .filter_map(|agent| crate::tx::solana::run::vault_of(&agent.slug))
        .collect()
}

#[cfg(target_arch = "wasm32")]
async fn read(owner: Pubkey) -> Option<Loaded> {
    use chain::solana_ix::accounts::{self, discriminator as d};
    use chain::solana_ix::{address, ids};

    use crate::tx::solana::port::SolReader;
    use crate::wallet::solana::SolSession;

    let session = crate::wallet::solana::get();
    let network = session.load_network().await?;
    let reader = SolSession::reader(&network);
    let vaults = solana_vaults();
    // Each vault and this wallet's holding in it, as of one slot.
    let mut keys = Vec::new();
    for vault in &vaults {
        keys.push(vault.address);
        keys.push(address::holder(&vault.address, &owner));
    }
    let accounts = reader.accounts(&keys).await.ok()?;
    let now = reader.chain_time().await.ok()?;
    // This wallet's requests, and payouts held for it, across vaults.
    let mine = reader
        .program_accounts(
            &ids::VAULT,
            &[(0, d::REQUEST.to_vec()), (48, owner.to_bytes().to_vec())],
        )
        .await
        .ok()?;
    let held_for = reader
        .program_accounts(
            &ids::VAULT,
            &[
                (0, d::REQUEST.to_vec()),
                (accounts::request_at::HELD_FOR, owner.to_bytes().to_vec()),
            ],
        )
        .await
        .ok()?;
    let claims = reader
        .program_accounts(
            &ids::VAULT,
            &[
                (0, d::CLAIM.to_vec()),
                (accounts::CLAIM_OWNER_AT, owner.to_bytes().to_vec()),
            ],
        )
        .await
        .ok()?;
    let mut loaded = Loaded {
        network: Some(network.clone()),
        ..Loaded::default()
    };
    for (i, vault) in vaults.iter().enumerate() {
        let Some(Some(data)) = accounts.get(2 * i) else {
            continue;
        };
        if data.owner != ids::VAULT {
            continue;
        }
        let Some(account) = Vault::read(&data.data) else {
            continue;
        };
        let holder = accounts
            .get(2 * i + 1)
            .cloned()
            .flatten()
            .filter(|a| a.owner == ids::VAULT)
            .and_then(|a| Holder::read(&a.data))
            .filter(|h| h.vault == vault.address && h.owner == owner);
        let figures = crate::tx::solana::read::figures(&account);
        let shares = U256::from(holder.as_ref().map_or(0, |h| h.shares));
        let value = figures
            .share_price()
            .and_then(|price| crate::money::quote::value_of(shares, price))
            .unwrap_or(U256::ZERO);
        let waiting: Vec<(Pubkey, Request)> = mine
            .iter()
            .filter_map(|(at, a)| Request::read(&a.data).map(|r| (*at, r)))
            .filter(|(_, r)| r.vault == vault.address && !r.done && r.owner == owner)
            .collect();
        let held: Vec<(Pubkey, Request)> = held_for
            .iter()
            .filter_map(|(at, a)| Request::read(&a.data).map(|r| (*at, r)))
            .filter(|(_, r)| r.vault == vault.address && r.held_for == owner && r.held > 0)
            .collect();
        for (at, data) in &claims {
            if let Some(claim) = Claim::read(&data.data)
                && *at == address::claim(&vault.address, claim.id)
                && claim.owner == owner
            {
                loaded.claims.push(ClaimRow {
                    vault: vault.clone(),
                    claim,
                });
            }
        }
        let units = holder
            .as_ref()
            .is_some_and(|h| h.units > 0 || h.units_claimable(account.per_unit).unwrap_or(0) > 0);
        if shares.is_zero() && waiting.is_empty() && held.is_empty() && !units {
            continue;
        }
        loaded.rows.push(Row {
            vault: vault.clone(),
            account: Rc::new(account),
            holder,
            value_cents: cents(value, network.asset_decimals),
            waiting,
            held,
            now,
        });
    }
    Some(loaded)
}

fn claim_badge(state: ClaimState) -> El {
    match state {
        ClaimState::Pending => badge(Tone::Neutral, Some(Icon::Clock), "Pending"),
        ClaimState::Waiting => badge(Tone::Caution, Some(Icon::Clock), "Waiting"),
        ClaimState::Queued => badge(Tone::Brand, Some(Icon::Clock), "Queued"),
        ClaimState::Paid => badge(Tone::Gain, Some(Icon::Check), "Paid"),
        ClaimState::Fallback => badge(Tone::Neutral, None, "Cash share taken"),
        ClaimState::None => badge(Tone::Neutral, None, "Unknown"),
    }
}

fn holdings_view(account: &Pubkey, loaded: &Loaded) -> View {
    let decimals = loaded.network.as_ref().map_or(6, |n| n.asset_decimals);
    let positions: Vec<&Row> = loaded
        .rows
        .iter()
        .filter(|row| row.holder.as_ref().is_some_and(|h| h.shares > 0))
        .collect();
    let open_claims: Vec<&ClaimRow> = loaded
        .claims
        .iter()
        .filter(|row| !matches!(row.claim.state, ClaimState::Paid | ClaimState::Fallback))
        .collect();
    let held_claims: Vec<&ClaimRow> = loaded
        .claims
        .iter()
        .filter(|row| row.claim.held > 0)
        .collect();
    let extras: Vec<&Row> = loaded
        .rows
        .iter()
        .filter(|row| {
            !row.waiting.is_empty()
                || !row.held.is_empty()
                || row.holder.as_ref().is_some_and(|h| h.units > 0)
        })
        .collect();
    let head = el("div")
        .class("sh-portfolio__head")
        .child(el("h2").class("sh-portfolio__h2").text("On Solana"))
        .child(el("p").class("sh-muted").text(format!(
            "Wallet {} · figures read from each vault’s account",
            short(&account.to_string())
        )));
    if positions.is_empty() && open_claims.is_empty() && extras.is_empty() && held_claims.is_empty()
    {
        return el("div")
            .child(head)
            .child(
                el("div")
                    .class("sh-portfolio__empty")
                    .child(el("p").text("You haven’t deposited from this Solana wallet yet."))
                    .child(
                        app_link("/agents")
                            .class("sh-btn sh-btn-primary")
                            .text("Browse agents"),
                    ),
            )
            .into();
    }
    let total: u128 = positions.iter().map(|row| row.value_cents).sum();
    let mut body = el("div").class("sh-portfolio__body").child(head).child(
        el("div").class("sh-portfolio__tiles").child(
            el("div")
                .class("sh-stat sh-stat--tile")
                .child(el("p").class("sh-label").text("Total value on Solana"))
                .child(
                    el("p")
                        .class("sh-stat__value sh-num")
                        .text(dollars_from_cents(total)),
                )
                .child(el("p").class("sh-stat__period").text(format!(
                    "{} {}, at today’s share prices",
                    positions.len(),
                    if positions.len() == 1 {
                        "agent"
                    } else {
                        "agents"
                    }
                ))),
        ),
    );
    if !positions.is_empty() {
        body = body.child(
            el("div").class("sh-table-wrap").child(
                el("table")
                    .class("sh-table")
                    .child(
                        el("caption")
                            .class("sh-vh")
                            .text("Your positions on Solana"),
                    )
                    .child(
                        el("thead").child(
                            el("tr")
                                .child(el("th").attr("scope", "col").text("Agent"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Shares"))
                                .child(el("th").attr("scope", "col").class("sh-r").text("Value")),
                        ),
                    )
                    .child(el("tbody").children(positions.iter().map(|row| {
                        el("tr")
                            .class("sh-row")
                            .child(
                                el("th").attr("scope", "row").child(
                                    app_link(format!("/agents/{}", row.vault.slug))
                                        .text(row.vault.title.clone()),
                                ),
                            )
                            .child(el("td").class("sh-r sh-fig").text(money::shares(
                                U256::from(row.holder.as_ref().map_or(0, |h| h.shares)),
                                Rounding::Down,
                            )))
                            .child(
                                el("td")
                                    .class("sh-r sh-fig")
                                    .text(dollars_from_cents(row.value_cents)),
                            )
                    })))
                    .child(
                        el("tfoot").child(
                            el("tr")
                                .child(el("th").attr("scope", "row").text("Total"))
                                .child(el("td"))
                                .child(
                                    el("td")
                                        .class("sh-r sh-fig")
                                        .text(dollars_from_cents(total)),
                                ),
                        ),
                    ),
            ),
        );
    }
    if !open_claims.is_empty() {
        body = body.child(
            el("section")
                .class("sh-portfolio__section")
                .child(
                    el("h3")
                        .class("sh-portfolio__h2")
                        .text("Booked withdrawals"),
                )
                .child(
                    el("p")
                        .class("sh-muted")
                        .text("Pending shares aren’t in the total above."),
                )
                .child(
                    el("ul")
                        .class("sh-portfolio__cards")
                        .children(open_claims.iter().map(|row| {
                            el("li")
                                .class("sh-card sh-card--pad sh-portfolio__claim")
                                .child(
                                    el("p")
                                        .class("sh-portfolio__claimtitle")
                                        .text(row.vault.title.clone())
                                        .child(claim_badge(row.claim.state)),
                                )
                                .child(el("p").class("sh-num").text(format!(
                                    "{} shares",
                                    money::shares(U256::from(row.claim.shares), Rounding::Down)
                                )))
                                .child(
                                    app_link(format!(
                                        "/portfolio/claims/{}-{}",
                                        row.vault.slug, row.claim.id
                                    ))
                                    .class("sh-link")
                                    .text("Track your claim"),
                                )
                        })),
                ),
        );
    }
    if !extras.is_empty() || !held_claims.is_empty() {
        let network = loaded.network.clone();
        let mut cards: Vec<El> = extras
            .iter()
            .map(|row| extra_card(row, network.clone(), decimals))
            .collect();
        for row in held_claims {
            cards.push(
                el("li")
                    .class("sh-card sh-card--pad sh-portfolio__claim")
                    .child(el("p").class("sh-portfolio__claimtitle").text(row.vault.title.clone()))
                    .child(el("p").text(format!(
                        "{} held for you on claim {}: your dollar account couldn’t take the payment, so the vault kept it.",
                        money::dollars(U256::from(row.claim.held), decimals, Rounding::Down),
                        row.claim.id
                    )))
                    .child(act(
                        &row.vault,
                        network.clone(),
                        SolAction::CollectHeld(HeldAt::Claim { id: row.claim.id }),
                        "Collect",
                        Kind::Primary,
                    )),
            );
        }
        body = body.child(
            el("section")
                .class("sh-portfolio__section")
                .child(
                    el("h3")
                        .class("sh-portfolio__h2")
                        .text("Waiting, units and held payouts"),
                )
                .child(el("ul").class("sh-portfolio__cards").children(cards)),
        );
    }
    body.into()
}

fn extra_card(row: &Row, network: Option<Rc<SolNetwork>>, decimals: u8) -> El {
    let mut card = el("li")
        .class("sh-card sh-card--pad sh-portfolio__claim")
        .child(
            el("p")
                .class("sh-portfolio__claimtitle")
                .text(row.vault.title.clone()),
        );
    let closed = row.account.state == VaultState::Closed;
    for (_, request) in &row.waiting {
        let due = request.earliest + row.account.timers.fallback_after;
        let what = match request.kind {
            RequestKind::Deposit => format!(
                "Deposit of {} waiting for a price",
                money::dollars(U256::from(request.amount), decimals, Rounding::Down)
            ),
            _ => format!(
                "Withdrawal of {} shares waiting for a price",
                money::shares(U256::from(request.amount), Rounding::Down)
            ),
        };
        card = card.child(el("p").text(what));
        if closed {
            card = card.child(act(
                &row.vault,
                network.clone(),
                SolAction::SettleClosed(request.id),
                if request.kind == RequestKind::Deposit {
                    "Take your deposit back"
                } else {
                    "Settle at the closed vault’s price"
                },
                Kind::Primary,
            ));
        } else if row.now >= due {
            if request.kind == RequestKind::Deposit {
                // The fallback on a deposit sends it back.
                card = card
                    .child(el("p").class("sh-muted").text(
                        "Nothing has priced it for a day. You can take your deposit back now.",
                    ))
                    .child(act(
                        &row.vault,
                        network.clone(),
                        SolAction::TakeCash(Fallback::Request(request.id)),
                        "Take your deposit back",
                        Kind::Primary,
                    ));
            } else {
                card = card
                    .child(el("p").class("sh-muted").text("Nothing has priced it for a day. You can take your exact share of the vault’s cash now; the rest becomes units, paid as the money comes home."))
                    .child(act(
                        &row.vault,
                        network.clone(),
                        SolAction::TakeCash(Fallback::Request(request.id)),
                        "Take cash share now",
                        Kind::Primary,
                    ))
                    .child(act(
                        &row.vault,
                        network.clone(),
                        SolAction::ClaimInstead(request.id),
                        "Make it a claim instead",
                        Kind::Secondary,
                    ));
            }
        }
    }
    if let Some(holder) = &row.holder {
        let claimable = holder.units_claimable(row.account.per_unit).unwrap_or(0);
        if holder.units > 0 || claimable > 0 {
            card = card.child(el("p").text(format!(
                "Units from the fallback: {} ready to collect.",
                money::dollars(U256::from(claimable), decimals, Rounding::Down)
            )));
            if claimable > 0 {
                card = card.child(act(
                    &row.vault,
                    network.clone(),
                    SolAction::CollectUnits,
                    "Collect",
                    Kind::Primary,
                ));
            }
        }
    }
    for (address, request) in &row.held {
        card = card
            .child(el("p").text(format!(
                "{} held for you: your dollar account couldn’t take a payment, so the vault kept it.",
                money::dollars(U256::from(request.held), decimals, Rounding::Down)
            )))
            .child(act(
                &row.vault,
                network.clone(),
                SolAction::CollectHeld(HeldAt::Request {
                    id: request.id,
                    address: *address,
                }),
                "Collect",
                Kind::Primary,
            ));
    }
    card
}

/// A button that runs one Solana flow, with its status line in place.
fn act(
    vault: &SolVault,
    network: Option<Rc<SolNetwork>>,
    action: SolAction,
    label: &'static str,
    kind: Kind,
) -> El {
    let running: Mutable<Option<SolRunning>> = Mutable::new(None);
    let session = crate::wallet::solana::get();
    let vault = vault.clone();
    el("div").class("sh-act").child_signal(
        None,
        futures_signals::map_ref! {
            let account = session.connection.signal_ref(|c| c.account()),
            let current = running.signal_cloned() => (*account, current.clone())
        }
        .map(move |(account, current)| {
            if account.is_none() {
                return Some(
                    on_click(
                        Button::new(kind, "Connect a Solana wallet")
                            .size(Size::Small)
                            .build(),
                        open_connect,
                    )
                    .into(),
                );
            }
            if let Some(current) = current {
                return Some(status(&current, network.clone(), &running));
            }
            let running = running.clone();
            let address = vault.address;
            let action = action.clone();
            Some(
                on_click(
                    Button::new(kind, label).size(Size::Small).build(),
                    move || {
                        let flow = SolRunning::new();
                        crate::tx::solana::run::start(address, action.clone(), &flow);
                        running.set(Some(flow));
                    },
                )
                .into(),
            )
        }),
    )
}

pub(super) fn status(
    current: &SolRunning,
    network: Option<Rc<SolNetwork>>,
    running: &Mutable<Option<SolRunning>>,
) -> View {
    let running = running.clone();
    el("div")
        .class("sh-act__status")
        .child_signal(
            None,
            current.step.signal_cloned().map(move |step| {
                let wallet = crate::wallet::solana::get().wallet_name.get_cloned();
                let decimals = network.as_ref().map_or(6, |n| n.asset_decimals);
                Some(match step? {
                    Step::Checking => el("p").attr("aria-live", "polite").text("Checking…").into(),
                    Step::Signing(_) => el("p")
                        .attr("aria-live", "polite")
                        .text(format!("Approve in {wallet}…"))
                        .into(),
                    Step::Sent { .. } | Step::Requested(_) | Step::Priced { .. } => el("p")
                        .attr("aria-live", "polite")
                        .text("Sent. Waiting for the network…")
                        .into(),
                    Step::Done(outcome) => {
                        let (line, signature) = match &outcome {
                            Outcome::Collected { assets, signature }
                            | Outcome::TookCash {
                                assets, signature, ..
                            }
                            | Outcome::Refunded { assets, signature }
                            | Outcome::Paid {
                                assets, signature, ..
                            } => (
                                format!(
                                    "{} is in your wallet.",
                                    money::dollars(U256::from(*assets), decimals, Rounding::Down)
                                ),
                                *signature,
                            ),
                            Outcome::Claim {
                                state,
                                paid,
                                signature,
                                ..
                            } => (
                                match state {
                                    ClaimState::Paid => format!(
                                        "{} is in your wallet.",
                                        money::dollars(U256::from(*paid), decimals, Rounding::Down)
                                    ),
                                    ClaimState::Queued => {
                                        "Queued for the next valuation.".to_string()
                                    }
                                    _ => "Tried: still waiting for the market.".to_string(),
                                },
                                *signature,
                            ),
                            Outcome::ClaimMade { claim, signature } => {
                                (format!("Claim {claim} booked."), *signature)
                            }
                            Outcome::Booked {
                                claim, signature, ..
                            } => (format!("Claim {claim} booked."), *signature),
                            Outcome::Deposited { signature, .. }
                            | Outcome::Changed { signature } => ("Done.".to_string(), *signature),
                        };
                        el("div")
                            .attr("role", "status")
                            .child(el("p").text(line))
                            .child(sig_link(network.as_deref(), &signature, "Transaction"))
                            .into()
                    }
                    Step::Failed(error) => {
                        let words = sol_error_words(&error, network.as_deref());
                        let running = running.clone();
                        el("div")
                            .attr("role", if words.alert { "alert" } else { "status" })
                            .child(el("p").class("sh-exit__title").text(words.title))
                            .child(el("p").text(words.text))
                            .child(match error.signature() {
                                Some(signature) => {
                                    sig_link(network.as_deref(), &signature, "Transaction")
                                }
                                None => View::Empty,
                            })
                            .child(on_click(
                                Button::new(Kind::Secondary, "OK").size(Size::Small).build(),
                                move || running.set(None),
                            ))
                            .into()
                    }
                })
            }),
        )
        .into()
}

// The claim page ----------------------------------------------------------------------------

#[derive(Clone)]
struct ClaimView {
    vault: SolVault,
    network: Rc<SolNetwork>,
    account: Rc<Vault>,
    claim: Claim,
    now: u64,
}

/// `/portfolio/claims/<slug>-<id>` for a Solana vault.
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
                None => el("p").class("sh-muted").text("Reading the claim…").into(),
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
        use chain::solana_ix::ids;

        use crate::tx::solana::flow;
        use crate::tx::solana::port::SolReader;
        use crate::wallet::solana::SolSession;

        let session = crate::wallet::solana::get();
        loop {
            let vault = crate::tx::solana::run::vault_of(&slug);
            let result = match (vault, session.load_network().await) {
                (Some(vault), Some(network)) => {
                    let reader = SolSession::reader(&network);
                    let account = reader.account(&vault.address).await.ok().flatten();
                    let claim = flow::claim(&reader, &vault.address, id).await;
                    let now = reader.chain_time().await.unwrap_or(0);
                    match (account, claim) {
                        (Some(account), Ok(Some(claim))) if account.owner == ids::VAULT => {
                            match Vault::read(&account.data) {
                                Some(account) => Ok(ClaimView {
                                    vault,
                                    network,
                                    account: Rc::new(account),
                                    claim,
                                    now,
                                }),
                                None => Err("This vault can’t be read right now.".to_string()),
                            }
                        }
                        (_, Ok(None)) => {
                            Err("There’s no claim with this number on this vault.".to_string())
                        }
                        _ => Err("The claim can’t be read right now. This page keeps trying."
                            .to_string()),
                    }
                }
                (None, _) => Err("This vault isn’t one the app knows yet.".to_string()),
                (_, None) => Err("The Solana cluster can’t be reached right now.".to_string()),
            };
            loaded.set(Some(result));
            let seen = session.changed.get();
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
    super::super::exit::utc(seconds)
}

fn claim_view(view: &ClaimView) -> View {
    let decimals = view.network.asset_decimals;
    let claim = &view.claim;
    let state = claim.state;
    let closed = view.account.state == VaultState::Closed;
    let figures = crate::tx::solana::read::figures(&view.account);
    let estimate = figures
        .exit_quote(U256::from(claim.shares))
        .map(|(quote, _)| quote.payout);
    let open = matches!(state, ClaimState::Pending | ClaimState::Waiting);
    let headline = match state {
        ClaimState::Paid => format!(
            "{} is in your wallet",
            money::dollars(U256::from(claim.paid), decimals, Rounding::Down)
        ),
        ClaimState::Fallback => "Your cash share was taken".to_string(),
        _ => format!(
            "Your withdrawal of {} shares is booked.",
            money::shares(U256::from(claim.shares), Rounding::Down)
        ),
    };
    let explain = match state {
        ClaimState::Pending => {
            "Booked. Settling closes your share of each position within the exit band and pays you in the same transaction."
        }
        ClaimState::Waiting => {
            "The market was outside the exit band when it was last tried, so nothing closed. It is tried again; this protects you from selling at a bad price."
        }
        ClaimState::Queued => {
            "The cash at home covers it: it is priced at the next valuation, like any withdrawal."
        }
        ClaimState::Paid => "The cash is home and in the wallet it was booked for.",
        ClaimState::Fallback => {
            "It took the fallback: its share of the cash then, and units for the rest of what the station holds."
        }
        ClaimState::None => "",
    };
    let mine = crate::wallet::solana::get().account() == Some(claim.owner);
    let fallback_due = claim.booked_at + view.account.timers.fallback_after;
    let mut actions = el("div").class("sh-claim__actions");
    if closed
        && matches!(
            state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        )
    {
        // A closed vault pays every claim still waiting from its cash, a
        // queued one included.
        actions = actions
            .child(el("p").text(
                "This vault has closed. Its claims are paid their share of its cash, with no fee.",
            ))
            .child(act(
                &view.vault,
                Some(view.network.clone()),
                SolAction::SettleClaim(claim.id),
                "Collect from the closed vault",
                Kind::Primary,
            ));
    } else if open {
        actions = actions
            .child(el("h2").class("sh-portfolio__h2").text("Push it through yourself"))
            .child(el("p").text("Settling is public. It doesn’t depend on us: anyone can send it, including you. You pay its network fee. If the market is still outside the exit band, nothing closes and the claim keeps waiting."))
            .child(act(
                &view.vault,
                Some(view.network.clone()),
                SolAction::SettleClaim(claim.id),
                "Settle now",
                Kind::Secondary,
            ));
    }
    if mine
        && !closed
        && view.now >= fallback_due
        && matches!(
            state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        )
    {
        actions = actions
            .child(el("h2").class("sh-portfolio__h2").text("No price for 24 hours"))
            .child(el("p").text("You can take your exact share of the vault’s cash at once. The rest becomes units on the station’s holdings, paid as the money comes home."))
            .child(act(
                &view.vault,
                Some(view.network.clone()),
                SolAction::TakeCash(Fallback::Claim(claim.id)),
                "Take cash share now",
                Kind::Primary,
            ));
    }
    if mine && claim.held > 0 {
        actions = actions
            .child(el("p").text(format!(
                "{} held for you: your dollar account couldn’t take the payment, so the vault kept it.",
                money::dollars(U256::from(claim.held), decimals, Rounding::Down)
            )))
            .child(act(
                &view.vault,
                Some(view.network.clone()),
                SolAction::CollectHeld(HeldAt::Claim { id: claim.id }),
                "Collect",
                Kind::Primary,
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
            el("aside")
                .class("sh-card sh-card--pad sh-claim__facts")
                .child(el("h2").class("sh-portfolio__h2").text("Claim facts"))
                .child(
                    el("dl")
                        .class("sh-exit__rows")
                        .child(fact("Vault", format!("{} · {}", view.vault.title, view.network.label)))
                        .child(fact(
                            "Shares pending",
                            if open {
                                money::shares(U256::from(claim.shares), Rounding::Down)
                            } else {
                                "—".to_string()
                            },
                        ))
                        .child(fact("Booked at", utc_day(claim.booked_at)))
                        .child(fact(
                            "Last tried",
                            if claim.last_attempt_at == 0 {
                                if open { "Not yet".to_string() } else { "—".to_string() }
                            } else {
                                utc_day(claim.last_attempt_at)
                            },
                        ))
                        .child(fact(
                            "Exit band",
                            format!(
                                "{} of the venue’s price",
                                crate::format::bps(u32::from(view.account.exit_band_bps))
                            ),
                        ))
                        .child(fact("Pays to", short(&claim.owner.to_string())))
                        .child(fact("Claim", format!("{} #{}", view.vault.slug, claim.id))),
                ),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rows_value_is_rounded_down_to_the_cent_once_and_the_total_is_their_sum() {
        let rows = [
            cents(U256::from(10_001_259_999u64), 6),
            cents(U256::from(5_001_581_000u64), 6),
        ];
        assert_eq!(rows, [1_000_125, 500_158]);
        assert_eq!(dollars_from_cents(rows.iter().sum()), "$15,002.83");
    }
}
