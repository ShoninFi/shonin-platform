//! Depositing, withdrawing, and watching a claim settle.
//!
//! The panel on a vault's page, the two modals behind it, and the wallet
//! button in the header.

use std::rc::Rc;

use std::sync::OnceLock;

use dominator::{Dom, clone, events, html, text};
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use shared::{ClaimState, Strategy};

use crate::data;
use crate::format as fmt;
use crate::state;
use crate::ui::form;
use crate::ui::overlay::{modal, modal_actions};
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
use crate::wallet::{self, Holdings};

// ------------------------------------------------------------------- state

fn connecting() -> &'static Mutable<bool> {
    static OPEN: OnceLock<Mutable<bool>> = OnceLock::new();
    OPEN.get_or_init(|| Mutable::new(false))
}

fn depositing() -> &'static Mutable<Option<String>> {
    static OPEN: OnceLock<Mutable<Option<String>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutable::new(None))
}

fn withdrawing() -> &'static Mutable<Option<String>> {
    static OPEN: OnceLock<Mutable<Option<String>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutable::new(None))
}

fn amount() -> &'static Mutable<String> {
    static AMOUNT: OnceLock<Mutable<String>> = OnceLock::new();
    AMOUNT.get_or_init(|| Mutable::new(String::new()))
}

// ------------------------------------------------------------ the header

/// The wallet control in the masthead.
pub fn wallet_button() -> Dom {
    html!("div", {
        .class("row")
        .child_signal(wallet::holdings().signal_cloned().map(|holdings| {
            Some(match &holdings.address {
                Some(address) => html!("div", {
                    .class("row")
                    .child(html!("a", {
                        .class(["btn", "btn--ghost"])
                        .attr("href", "/portfolio")
                        .child(html!("span", {
                            .class("num")
                            .text(&fmt::short_address(address))
                        }))
                    }))
                    .child(html!("span", {
                        .class(["num", "small", "faint", "nowrap"])
                        .text(&fmt::money(holdings.balance))
                    }))
                }),
                None => shell::button("Connect wallet", true, || connecting().set(true)),
            })
        }))
        .child(connect_modal())
    })
}

fn connect_modal() -> Dom {
    modal(connecting(), "Connect a wallet", false, || {
        vec![
            ui::para(
                "Connecting only says which address you are. Depositing, leaving and settling are \
                 transactions your own wallet signs, against the vault's own contract.",
            ),
            html!("div", {
                .class("stack-sm")
                .children(ways_to_connect()
                    .into_iter()
                    .map(|(how, label)| {
                        html!("button", {
                            .class(["btn", "btn--block"])
                            .attr("type", "button")
                            .text(label)
                            .event(clone!(how => move |_: events::Click| {
                                let how = how.clone();
                                connecting().set(false);
                                wasm_bindgen_futures::spawn_local(async move {
                                    let watching = how == wallet::Connection::Watching;
                                    let address = watching.then(ask_for_an_address).flatten();
                                    match wallet::connect(how, address).await {
                                        Ok(()) => state::notify(
                                            "Connected. What you hold is read from the chain.",
                                            Tone::Ok,
                                        ),
                                        Err(problem) => state::notify(problem, Tone::Warn),
                                    }
                                });
                            }))
                        })
                    }))
            }),
        ]
    })
}

/// Ask which address to watch. Watching signs nothing, so a prompt is the
/// whole of what is needed.
fn ask_for_an_address() -> Option<String> {
    web_sys::window()?
        .prompt_with_message("Which address would you like to watch?")
        .ok()
        .flatten()
        .filter(|address| !address.trim().is_empty())
        .map(|address| address.trim().to_string())
}

/// The ways a viewer can connect here. The development wallet appears only
/// when the platform says it has one to offer.
fn ways_to_connect() -> Vec<(wallet::Connection, &'static str)> {
    let mut ways = vec![(wallet::Connection::Browser, "Browser wallet")];
    if data::dev().is_some() {
        ways.push((wallet::Connection::Development, "Development wallet"));
    }
    ways.push((wallet::Connection::Watching, "Watch an address"));
    ways
}

/// Send something, and say what happened when it lands.
///
/// Every action on this page is a transaction: it takes a moment, it can be
/// turned down, and the page says which.
pub fn act<F>(what: F, said: impl FnOnce(&str) -> String + 'static)
where
    F: std::future::Future<Output = Result<String, String>> + 'static,
{
    wasm_bindgen_futures::spawn_local(async move {
        match what.await {
            Ok(hash) => state::notify(said(&hash), Tone::Ok),
            Err(problem) => state::notify(problem, Tone::Warn),
        }
    });
}

// --------------------------------------------------------- the vault panel

/// The panel on a vault's page: what you hold, and what you can do about it.
pub fn panel(strategy: &Rc<Strategy>) -> Dom {
    let slug = strategy.slug.clone();
    let held = strategy.clone();

    html!("div", {
        .class("stack")
        .child_signal(wallet::holdings().signal_cloned().map(clone!(slug, held => move |holdings| {
            Some(panel_body(&held, &holdings, &slug))
        })))
        .child(deposit_modal(strategy))
        .child(withdraw_modal(strategy))
    })
}

fn panel_body(strategy: &Rc<Strategy>, holdings: &Holdings, slug: &str) -> Dom {
    let slug = slug.to_string();
    let vault = &strategy.vault;
    let position = holdings.position(&slug);
    let claims = holdings.claims_for(&slug);
    let open = vault.state.takes_deposits();

    let mut children = vec![html!("div", {
        .class(["row", "row--between", "row--wrap"])
        .child(html!("div", {
            .child(html!("div", { .class("stat__label").text("Value per share") }))
            .child(html!("div", {
                .class("stat__value")
                .text(&fmt::share_price(vault.share_price))
            }))
        }))
        .child(ui::badge(
            vault.state.label(),
            if open { Tone::Ok } else { Tone::Warn },
        ))
    })];

    if let Some(position) = position {
        let value = position.shares * vault.share_price;
        let profit = value - position.cost;
        children.push(ui::kv_rows(vec![
            (
                "Your shares".to_string(),
                ui::number(&fmt::number(position.shares, 2)),
            ),
            ("Worth".to_string(), ui::number(&fmt::money_exact(value))),
            (
                "Profit and loss".to_string(),
                ui::signed(profit, &fmt::money_exact(profit)),
            ),
        ]));
    }

    children.push(html!("div", {
        .class(["row", "row--wrap"])
        .child(html!("button", {
            .class(["btn", "btn--primary", "grow"])
            .attr("type", "button")
            .attr("aria-disabled", if open { "false" } else { "true" })
            .text("Deposit")
            .event(clone!(slug => move |_: events::Click| {
                if !open {
                    state::notify("This vault isn't taking deposits.", Tone::Warn);
                    return;
                }
                if !wallet::holdings().lock_ref().is_connected() {
                    connecting().set(true);
                    return;
                }
                amount().set(String::new());
                depositing().set(Some(slug.clone()));
            }))
        }))
        .apply_if(position.is_some(), |dom| dom.child(html!("button", {
            .class(["btn", "grow"])
            .attr("type", "button")
            .text("Withdraw")
            .event(clone!(slug => move |_: events::Click| {
                amount().set(String::new());
                withdrawing().set(Some(slug.clone()));
            }))
        })))
    }));

    if position.is_none() {
        children.push(ui::note(&format!(
            "Shares are minted at the value the contract computes in the same transaction, after \
             a {} transaction fee that stays in the pool.",
            wallet::transaction_fee_label(vault)
        )));
    }

    for claim in claims {
        children.push(claim_card(claim, &slug));
    }

    ui::card_titled("Your position", None, children)
}

fn claim_card(claim: &shared::HeldClaim, slug: &str) -> Dom {
    let slug = slug.to_string();
    let id = claim.id;
    let state = claim.state;
    let steps = [
        (ClaimState::Pending, "Claim booked, shares pending"),
        (
            ClaimState::Waiting,
            "Waiting for a market inside the exit band",
        ),
        (
            ClaimState::Paid,
            "Closed, valued again, burned at the marks, and paid",
        ),
    ];
    let reached = steps
        .iter()
        .position(|(step, _)| *step == state)
        .unwrap_or(0);

    let items: Vec<Dom> = steps
        .iter()
        .enumerate()
        .map(|(index, (_, label))| {
            html!("li", {
                .class("steps__item")
                .apply_if(index < reached, |dom| dom.class("is-done"))
                .apply_if(index == reached, |dom| dom.class("is-current"))
                .child(html!("div", {
                    .class("steps__title")
                    .apply_if(index > reached, |dom| dom.class("faint"))
                    .text(label)
                }))
            })
        })
        .collect();

    let mut children = vec![
        ui::kv_rows(vec![
            (
                "Shares pending".to_string(),
                ui::number(&fmt::number(claim.shares, 2)),
            ),
            (
                "Fraction of the vault closed for it".to_string(),
                ui::number(&fmt::percent(claim.fraction_closed, 2)),
            ),
        ]),
        html!("ol", { .class("steps").children(items) }),
    ];

    if state == ClaimState::Paid {
        children.push(ui::notice(
            Tone::Ok,
            "Paid",
            &format!(
                "{} reached your balance, valued at exit-side marks once the cash was home.",
                fmt::money_exact(claim.settled_for.unwrap_or_default())
            ),
        ));
    } else {
        children.push(ui::note(
            "Anyone can send the next transaction. Your shares stay outstanding until the last \
             one, so the close is borne pro rata.",
        ));
        children.push(shell::button(
            "Send the next settle transaction",
            true,
            clone!(slug => move || {
                act(
                    {
                        let slug = slug.clone();
                        async move { wallet::settle(&slug, id).await }
                    },
                    |_| "Settlement sent. The claim pays once the close has filled.".to_string(),
                );
            }),
        ));
    }

    ui::card_titled(
        "A claim on this vault",
        Some(ui::badge(
            state.label(),
            if state == ClaimState::Paid {
                Tone::Ok
            } else {
                Tone::Warn
            },
        )),
        children,
    )
}

// ------------------------------------------------------------- the modals

fn deposit_modal(strategy: &Rc<Strategy>) -> Dom {
    let open = Mutable::new(false);
    let slug = strategy.slug.clone();
    let held = strategy.clone();

    // The one modal follows whichever vault asked for it.
    let watcher = depositing()
        .signal_cloned()
        .for_each(clone!(open, slug => move |current| {
            open.set_neq(current.as_deref() == Some(slug.as_str()));
            async {}
        }));

    html!("div", {
        .future(watcher)
        .child(modal(&open, "Deposit", false, clone!(held => move || {
            let vault = &held.vault;
            let balance = wallet::holdings().lock_ref().balance;

            vec![
                form::field(
                    &format!("Amount in {}", vault.asset),
                    Some(&format!("You have {}", fmt::money_exact(balance))),
                    form::amount(amount(), &vault.asset, balance),
                ),
                html!("div", {
                    .child_signal(amount().signal_cloned().map(clone!(held => move |value| {
                        let quote = wallet::quote_deposit(&held.vault, form::parse_amount(&value));
                        Some(ui::kv_rows(vec![
                            (
                                format!(
                                    "Transaction fee ({}), kept in the pool",
                                    wallet::transaction_fee_label(&held.vault)
                                ),
                                ui::number(&fmt::money_exact(quote.fee)),
                            ),
                            (
                                "Valued at".to_string(),
                                ui::number(&format!(
                                    "{} a share, at mark",
                                    fmt::share_price(quote.share_price)
                                )),
                            ),
                            (
                                "Shares minted".to_string(),
                                ui::number(&fmt::number(quote.shares, 2)),
                            ),
                        ]))
                    })))
                }),
                ui::note(
                    "The contract computes the vault's value from on-chain state in the same \
                     transaction and mints at that. The fee stays in the pool.",
                ),
                modal_actions(vec![
                    shell::button("Cancel", false, || depositing().set(None)),
                    shell::button("Deposit", true, clone!(slug => move || {
                        let value = form::parse_amount(&amount().get_cloned());
                        depositing().set(None);
                        act(
                            {
                                let slug = slug.clone();
                                async move { wallet::deposit(&slug, value).await }
                            },
                            move |_| {
                                format!(
                                    "Deposited {}. Shares were minted at the value the contract \
                                     computed in the same transaction.",
                                    fmt::money_exact(value)
                                )
                            },
                        );
                    })),
                ]),
            ]
        })))
    })
}

fn withdraw_modal(strategy: &Rc<Strategy>) -> Dom {
    let open = Mutable::new(false);
    let slug = strategy.slug.clone();
    let held = strategy.clone();

    let watcher = withdrawing()
        .signal_cloned()
        .for_each(clone!(open, slug => move |current| {
            open.set_neq(current.as_deref() == Some(slug.as_str()));
            async {}
        }));

    html!("div", {
        .future(watcher)
        .child(modal(&open, "Withdraw", false, clone!(held, slug => move || {
            let shares = wallet::holdings()
                .lock_ref()
                .position(&slug)
                .map_or(0.0, |position| position.shares);

            vec![
                form::field(
                    "Shares",
                    Some(&format!("You hold {}", fmt::number(shares, 2))),
                    form::amount(amount(), "shares", shares),
                ),
                html!("div", {
                    .child_signal(amount().signal_cloned().map(clone!(held => move |value| {
                        let quote = wallet::quote_withdraw(&held.vault, form::parse_amount(&value));
                        Some(html!("div", {
                            .class("stack-sm")
                            .child(ui::kv_rows(vec![
                                (
                                    "Valued at".to_string(),
                                    ui::number(&format!(
                                        "{} a share, at exit-side marks",
                                        fmt::share_price(quote.exit_price)
                                    )),
                                ),
                                (
                                    format!(
                                        "Transaction fee ({}), kept in the pool",
                                        wallet::transaction_fee_label(&held.vault)
                                    ),
                                    ui::number(&fmt::money_exact(quote.fee)),
                                ),
                                (
                                    "You receive".to_string(),
                                    ui::number(&fmt::money_exact(quote.assets)),
                                ),
                            ]))
                            .child(if quote.books_claim {
                                ui::notice(
                                    Tone::Warn,
                                    "This books a claim",
                                    &format!(
                                        "It is more than the vault's idle balance of {}, so your shares are pending while \
                                         the contract closes your fraction of every position. Once the \
                                         cash is home you are paid.",
                                        fmt::money(held.vault.idle)
                                    ),
                                )
                            } else {
                                ui::notice(
                                    Tone::Ok,
                                    "Paid in one transaction",
                                    &format!(
                                        "The vault's idle balance of {} covers it, so the shares \
                                         are burned and you are paid in the same transaction.",
                                        fmt::money(held.vault.idle)
                                    ),
                                )
                            })
                        }))
                    })))
                }),
                ui::note(
                    "Valued at exit-side marks, so the leaver pays the spread rather than the pool.",
                ),
                modal_actions(vec![
                    shell::button("Cancel", false, || withdrawing().set(None)),
                    shell::button("Withdraw", true, clone!(slug => move || {
                        let value = form::parse_amount(&amount().get_cloned());
                        withdrawing().set(None);
                        act(
                            {
                                let slug = slug.clone();
                                async move { wallet::withdraw(&slug, value).await }
                            },
                            |_| {
                                "Sent. The contract pays from its idle balance if it covers it, \
                                 and books a claim if it does not."
                                    .to_string()
                            },
                        );
                    })),
                ]),
            ]
        })))
    })
}

/// What the viewer holds, wherever a page needs to read it as one figure.
pub fn holdings_summary() -> Dom {
    html!("div", {
        .child_signal(map_ref! {
            let holdings = wallet::holdings().signal_cloned() => {
                let value = holdings.value();
                let cost = holdings.cost();
                Some(ui::stats(vec![
                    ui::stat("Balance", &fmt::money_exact(holdings.balance), Some("in your wallet")),
                    ui::stat("In vaults", &fmt::money_exact(value), Some(&format!(
                        "{} position{}",
                        holdings.positions.len(),
                        if holdings.positions.len() == 1 { "" } else { "s" }
                    ))),
                    ui::stat_toned(
                        "Profit and loss",
                        &fmt::money_exact(value - cost),
                        Some("against what you paid"),
                        crate::pages::parts::tone_of(value - cost),
                    ),
                    ui::stat("Claims", &holdings.claims.len().to_string(), Some("waiting to settle")),
                ]))
            }
        })
        .child(html!("span", { .child(text("")) }))
    })
}
