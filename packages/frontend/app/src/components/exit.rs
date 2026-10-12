//! The Exit panel: deposit and withdraw on an agent page (Deposit-*,
//! Withdraw-*, States-System). The same panel is the phone's Exit sheet.
//!
//! Every money figure is read from the vault's chain (its last price, its
//! cash, the wallet's balance on that chain) and computed in base units with
//! the contract's own rounding; the form previews without a wallet. Once a
//! request is sent the panel follows the flow's steps: the wallet step, the
//! valuation countdown, the price, done; or the error, in words, in place.
//! The status line is a polite live region; errors are alerts.

use std::rc::Rc;

use alloy_primitives::U256;
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, Signal, SignalExt};
use ui::icons::Icon;

use super::wallet::{hex_ring, open_connect, switch_button};
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::check::checkbox;
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::segmented::segmented;
use crate::money::format as money;
use crate::money::quote::ExitPath;
use crate::money::{AmountError, Rounding, parse_units};
use crate::tx::error::{Context, TxError};
use crate::tx::flow::{Outcome, Prompt, Step};
use crate::tx::read::{ClaimState, Holdings, State, VaultFigures};
use crate::view::{El, View, el, text};
use crate::wallet::machine::Connection;
use crate::wallet::run::EXIT;
use crate::wallet::{Action, Network, Running, VaultRef, networks};

/// Which way the money goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Deposit,
    Withdraw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Shares,
    Dollars,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Form,
    Review,
}

/// What the panel shows, coarsely: the form is drawn once per mode, so
/// typing never redraws the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Loading,
    Unavailable,
    SignedOut(Tab),
    WrongNetwork(u64),
    Form(Tab),
    Review,
    /// A flow for this vault and account, by its id: another account's or
    /// another flow redraws.
    Flow(u64),
}

/// Everything one panel holds.
struct Panel {
    slug: String,
    vault: Mutable<Option<Rc<VaultRef>>>,
    /// The vault or its chain can't be reached from this app.
    missing: Mutable<bool>,
    network: Mutable<Option<Rc<Network>>>,
    figures: Mutable<Option<VaultFigures>>,
    holdings: Mutable<Option<Holdings>>,
    tab: Mutable<Tab>,
    deposit_text: Mutable<String>,
    withdraw_text: Mutable<String>,
    unit: Mutable<Unit>,
    stage: Mutable<Stage>,
    ack: Mutable<bool>,
    /// Whether the panel is in the phone sheet (locked while signing).
    in_sheet: bool,
}

impl Panel {
    /// The flow this vault's panel shows for the connected account: kept by
    /// the session, so a panel or sheet built while it runs shows it too.
    fn running(&self) -> Option<Running> {
        crate::wallet::get().flow(&self.slug, EXIT)
    }
}

/// The Exit panel for an agent, for the agent page's right column.
pub fn panel(slug: &str) -> View {
    build(slug, Tab::Deposit, false)
}

/// The phone's Exit sheet, opened by the action bar's Deposit or Withdraw.
pub fn open_sheet(slug: &str, tab: Tab) {
    let title = crate::store::get()
        .agent(slug)
        .map(|agent| agent.title.get_cloned())
        .unwrap_or_else(|| "this agent".to_string());
    let verb = match tab {
        Tab::Deposit => "Deposit to",
        Tab::Withdraw => "Withdraw from",
    };
    crate::atoms::dialog::open(crate::atoms::dialog::Dialog {
        title: format!("{verb} {title}"),
        body: build(slug, tab, true),
        foot: None,
        wide: false,
    });
}

fn build(slug: &str, tab: Tab, in_sheet: bool) -> View {
    let panel = Rc::new(Panel {
        slug: slug.to_string(),
        vault: Mutable::new(None),
        missing: Mutable::new(false),
        network: Mutable::new(None),
        figures: Mutable::new(None),
        holdings: Mutable::new(None),
        tab: Mutable::new(tab),
        deposit_text: Mutable::new(String::new()),
        withdraw_text: Mutable::new(String::new()),
        unit: Mutable::new(Unit::Shares),
        stage: Mutable::new(Stage::Form),
        ack: Mutable::new(false),
        in_sheet,
    });
    let session = crate::wallet::get();
    let mode = {
        let panel = panel.clone();
        map_ref! {
            let connection = session.connection.signal_cloned(),
            let network = panel.network.signal_cloned(),
            let figures = panel.figures.signal_ref(Option::is_some),
            let tab = panel.tab.signal(),
            let stage = panel.stage.signal(),
            let running = session.flow_signal(slug, EXIT).map(|running| running.map(|running| running.id)),
            let missing = panel.missing.signal() =>
            if *missing { Mode::Unavailable } else {
                mode_of(connection, network.as_deref(), *figures, *tab, *stage, *running)
            }
        }
        .dedupe()
    };
    let body = {
        let panel = panel.clone();
        mode.map(move |mode| Some(render(&panel, mode)))
    };
    let tabs = tabs(&panel);
    let loader = panel.clone();
    el("section")
        .class("sh-exit")
        .class_if(in_sheet, "sh-exit--sheet")
        .attr("aria-label", "Deposit or withdraw")
        .child(tabs)
        .child_signal(Some(placeholder()), body)
        .child(
            el("p")
                .class("sh-exit__risk")
                .text("The agent can lose money. Nothing here stops that."),
        )
        .hook(move |dom| dom.future(follow(loader)))
        .into()
}

fn mode_of(
    connection: &Connection,
    network: Option<&Network>,
    figures: bool,
    tab: Tab,
    stage: Stage,
    running: Option<u64>,
) -> Mode {
    if let Some(id) = running {
        return Mode::Flow(id);
    }
    let Some(network) = network else {
        return Mode::Loading;
    };
    if !figures {
        return Mode::Loading;
    }
    let Connection::Connected { chain_id, .. } = connection else {
        return Mode::SignedOut(tab);
    };
    if crate::wallet::get().is_browser()
        && let Some(on) = chain_id
        && *on != network.chain_id
    {
        return Mode::WrongNetwork(network.chain_id);
    }
    match (tab, stage) {
        (Tab::Deposit, Stage::Review) => Mode::Review,
        _ => Mode::Form(tab),
    }
}

/// Keep the vault's figures and the wallet's holdings fresh: on load, every
/// 10 s, and whenever the account changes or a flow lands.
async fn follow(panel: Rc<Panel>) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::tx::read;
        use crate::wallet::session::Session;
        let session = crate::wallet::get();
        let vaults = session.read_vaults().await;
        let Some(vault) = vaults.get(&panel.slug).cloned() else {
            panel.missing.set(true);
            return;
        };
        panel.vault.set(Some(vault.clone()));
        let Some(network) = session.network(vault.chain).await else {
            panel.missing.set(true);
            return;
        };
        panel.network.set(Some(network.clone()));
        let reader = Session::reader(&network);
        loop {
            if let Ok(figures) = read::figures(&reader, vault.address).await {
                panel.figures.set(Some(figures));
            }
            match session.account() {
                Some(account) => {
                    if let Ok(held) =
                        read::holdings(&reader, vault.address, network.asset, account).await
                    {
                        panel.holdings.set(Some(held));
                    }
                }
                None => panel.holdings.set(None),
            }
            // Wait 10 s, or less if the account changes or a flow lands.
            let seen = (session.changed.get(), session.account());
            for _ in 0..10 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (session.changed.get(), session.account()) != seen {
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = panel;
}

fn placeholder() -> View {
    el("div")
        .class("sh-exit__body")
        .attr("aria-busy", "true")
        .child(crate::atoms::skeleton::skeleton_block("100%", 48))
        .child(crate::atoms::skeleton::skeleton_block("60%", 20))
        .child(
            Button::new(Kind::Primary, "Deposit")
                .block()
                .disabled(true)
                .build(),
        )
        .into()
}

fn tabs(panel: &Rc<Panel>) -> El {
    let tab = |which: Tab, label: &'static str| {
        let panel = panel.clone();
        el("button")
            .attr("type", "button")
            .class("sh-tab")
            .attr("role", "tab")
            .attr_from("aria-selected", &panel.tab, move |tab| {
                Some((*tab == which).to_string())
            })
            .text(label)
            .hook(move |dom| {
                dom.event(move |_: dominator::events::Click| {
                    // Not while a flow is under way.
                    if panel.running().is_none() {
                        panel.tab.set_neq(which);
                        panel.stage.set_neq(Stage::Form);
                    }
                })
            })
    };
    el("div")
        .class("sh-tabs sh-exit__tabs")
        .attr("role", "tablist")
        .child(tab(Tab::Deposit, "Deposit"))
        .child(tab(Tab::Withdraw, "Withdraw"))
}

fn render(panel: &Rc<Panel>, mode: Mode) -> View {
    let body = el("div").class("sh-exit__body");
    match mode {
        Mode::Loading => placeholder(),
        Mode::Unavailable => body
            .child(notice(
                NoticeTone::Info,
                text("This vault can’t be reached from here right now."),
            ))
            .into(),
        Mode::SignedOut(tab) => body
            .child(form(panel, tab, false))
            .child(el("p").class("sh-muted").text(match tab {
                Tab::Deposit => "Connect a wallet to deposit. The smallest deposit is $10.",
                Tab::Withdraw => "Connect the wallet that holds your shares to withdraw.",
            }))
            .child(
                on_click(
                    Button::new(Kind::Primary, "Connect wallet").block().build(),
                    open_connect,
                )
                .attr("aria-haspopup", "dialog"),
            )
            .child(
                el("p")
                    .class("sh-muted sh-exit__small")
                    .text("Connecting doesn’t move money or sign anything."),
            )
            .into(),
        Mode::WrongNetwork(chain_id) => {
            let session = crate::wallet::get();
            let on = session
                .wallet_chain()
                .map(networks::chain_name)
                .unwrap_or_default();
            let target = networks::chain_name(chain_id);
            let title = panel
                .vault
                .lock_ref()
                .as_ref()
                .map(|vault| vault.title.clone())
                .unwrap_or_default();
            body.attr("role", "status")
                .child(
                    el("p")
                        .class("sh-exit__title")
                        .child(icon(Icon::Concern, 18))
                        .text(" Wrong network"),
                )
                .child(el("p").text(format!(
                    "{title}’s vault is on {target}. Your wallet is on {on} right now."
                )))
                .child(switch_button(chain_id, Size::Medium).class("sh-btn-block"))
                .into()
        }
        Mode::Form(tab) => body.child(form(panel, tab, true)).into(),
        Mode::Review => review(panel),
        Mode::Flow(_) => flow_view(panel),
    }
}

// The form --------------------------------------------------------------------

/// A field's verdict on what's typed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Check {
    Empty,
    Invalid(String),
    /// Under the minimum, over the balance, more shares than held.
    Over {
        line: String,
        fill: Option<String>,
    },
    Below(String),
    Ready,
}

fn decimals(panel: &Panel) -> u8 {
    panel
        .network
        .lock_ref()
        .as_ref()
        .map(|network| network.asset_decimals)
        .unwrap_or(6)
}

fn symbol(panel: &Panel) -> String {
    panel
        .network
        .lock_ref()
        .as_ref()
        .map(|network| network.asset_symbol.clone())
        .unwrap_or_else(|| "USDC".to_string())
}

fn chain_label(panel: &Panel) -> String {
    panel
        .network
        .lock_ref()
        .as_ref()
        .map(|network| network.label.clone())
        .unwrap_or_default()
}

fn deposit_check(panel: &Panel, text: &str, connected: bool) -> (Check, Option<U256>) {
    let decimals = decimals(panel);
    let amount = match parse_units(text, decimals) {
        Err(AmountError::Empty) => return (Check::Empty, None),
        Err(error) => return (Check::Invalid(error.words()), None),
        Ok(amount) if amount.is_zero() => return (Check::Empty, None),
        Ok(amount) => amount,
    };
    let figures = panel.figures.lock_ref();
    if let Some(figures) = figures.as_ref() {
        if figures.state != State::Open {
            return (
                Check::Invalid("This vault isn’t taking deposits.".to_string()),
                Some(amount),
            );
        }
        if amount < figures.minimum_deposit {
            return (
                Check::Below(format!(
                    "Below the {} minimum",
                    money::dollars_exact(figures.minimum_deposit, decimals).replace(".00", "")
                )),
                Some(amount),
            );
        }
    }
    if connected
        && let Some(held) = panel.holdings.lock_ref().as_ref()
        && amount > held.balance
    {
        return (
            Check::Over {
                line: format!(
                    "You have {} on {}.",
                    money::token_balance(held.balance, decimals, &symbol(panel)),
                    chain_label(panel)
                ),
                fill: (!held.balance.is_zero()).then(|| money::plain(held.balance, decimals)),
            },
            Some(amount),
        );
    }
    (Check::Ready, Some(amount))
}

/// The shares a withdrawal is for, from what's typed in shares or dollars.
fn withdraw_check(panel: &Panel, text: &str, unit: Unit, connected: bool) -> (Check, Option<U256>) {
    let decimals = decimals(panel);
    let figures = panel.figures.lock_ref();
    let shares = match unit {
        Unit::Shares => match parse_units(text, 18) {
            Err(AmountError::Empty) => return (Check::Empty, None),
            Err(error) => return (Check::Invalid(error.words()), None),
            Ok(shares) => shares,
        },
        Unit::Dollars => match parse_units(text, decimals) {
            Err(AmountError::Empty) => return (Check::Empty, None),
            Err(error) => return (Check::Invalid(error.words()), None),
            Ok(dollars) if dollars.is_zero() => U256::ZERO,
            Ok(dollars) => match figures
                .as_ref()
                .and_then(|figures| figures.shares_for(dollars))
            {
                Some(shares) => shares,
                None => {
                    return (
                        Check::Invalid("That can’t be priced right now.".to_string()),
                        None,
                    );
                }
            },
        },
    };
    if shares.is_zero() {
        return (Check::Empty, None);
    }
    if connected
        && let Some(held) = panel.holdings.lock_ref().as_ref()
        && shares > held.shares
    {
        return (
            Check::Over {
                line: format!(
                    "You hold {} shares.",
                    money::shares(held.shares, Rounding::Down)
                ),
                fill: (!held.shares.is_zero()).then(|| match unit {
                    Unit::Shares => money::plain_exact(held.shares, 18),
                    Unit::Dollars => figures
                        .as_ref()
                        .and_then(|figures| figures.exit_quote(held.shares))
                        .map(|(quote, _)| money::plain(quote.payout, decimals))
                        .unwrap_or_default(),
                }),
            },
            Some(shares),
        );
    }
    (Check::Ready, Some(shares))
}

/// A row of the summary: a label (with its small print) and a figure.
fn row(
    label: &'static str,
    detail: Option<&'static str>,
    figure: impl Signal<Item = String> + 'static,
) -> El {
    let label = match detail {
        Some(detail) => el("dt")
            .text(label)
            .child(el("span").class("sh-exit__detail").text(detail)),
        None => el("dt").text(label),
    };
    el("div")
        .class("sh-exit__row")
        .child(label)
        .child(el("dd").class("sh-num").text_signal("", figure))
}

fn form(panel: &Rc<Panel>, tab: Tab, connected: bool) -> View {
    match tab {
        Tab::Deposit => deposit_form(panel, connected),
        Tab::Withdraw => withdraw_form(panel, connected),
    }
}

/// The amount input: inputmode decimal, 16px, labelled, its hint or error
/// linked with aria-describedby.
fn amount_input(
    id: &'static str,
    value: &Mutable<String>,
    unit: View,
    invalid: impl Signal<Item = bool> + 'static,
) -> El {
    let typed = value.clone();
    el("div")
        .class("sh-input sh-exit__input")
        .child(el("span").class("sh-input__unit").attr("aria-hidden", "true").text("$").class("sh-exit__dollar"))
        .child(
            el("input")
                .attr("id", id)
                .attr("type", "text")
                .attr("inputmode", "decimal")
                .attr("autocomplete", "off")
                .attr("spellcheck", "false")
                .attr("placeholder", "0.00")
                .attr("aria-describedby", format!("{id}-hint"))
                .attr_signal("aria-invalid", None, invalid.map(|bad| bad.then(|| "true".to_string())))
                .hook(move |dom| {
                    dominator::with_node!(dom, input => {
                        .prop_signal("value", typed.signal_cloned())
                        .event(move |_: dominator::events::Input| {
                            if let Some(input) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input) {
                                typed.set_neq(input.value());
                            }
                        })
                    })
                }),
        )
        .child(el("span").class("sh-input__unit").child(unit))
}

fn deposit_form(panel: &Rc<Panel>, connected: bool) -> View {
    let state = panel
        .figures
        .lock_ref()
        .as_ref()
        .map(|figures| figures.state);
    if let Some(state) = state.filter(|state| *state != State::Open) {
        let line = match state {
            State::Closed => {
                "This vault is closed and won’t take deposits again. Withdraw from its cash whenever you like."
            }
            State::WindingDown => {
                "This agent is retiring: its vault takes no new deposits. Holders can withdraw as usual."
            }
            State::Frozen | State::Killed => {
                "This vault has stopped and takes no deposits. Holders can still withdraw."
            }
            _ => "This vault isn’t taking deposits yet.",
        };
        return notice(NoticeTone::Info, text(line)).into();
    }
    let check = {
        let panel = panel.clone();
        map_ref! {
            let text = panel.deposit_text.signal_cloned(),
            let _figures = panel.figures.signal_ref(|_| ()),
            let _holdings = panel.holdings.signal_ref(|_| ()) =>
            deposit_check(&panel, text, connected)
        }
        .broadcast()
    };
    let decimals = decimals(panel);
    let held_line = {
        let panel = panel.clone();
        map_ref! {
            let holdings = panel.holdings.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            held_sentence(holdings.as_ref(), figures.as_ref(), decimals)
        }
    };
    let balance = {
        let panel = panel.clone();
        let symbol = symbol(&panel);
        panel.holdings.signal_cloned().map(move |held| match held {
            Some(held) if connected => format!(
                "In your wallet: {}",
                money::token_balance(held.balance, decimals, &symbol)
            ),
            _ => String::new(),
        })
    };
    let picks = [("$100", "100"), ("$500", "500"), ("$1,000", "1000")];
    let quote = {
        let panel = panel.clone();
        map_ref! {
            let (_, amount) = check.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            amount.zip(figures.clone()).and_then(|(amount, figures)| {
                Some((figures.deposit_quote(amount)?, figures.share_price()?))
            })
        }
        .broadcast()
    };
    let symbol_unit = symbol(panel);
    let mut quick = el("div")
        .class("sh-exit__picks")
        .attr("role", "group")
        .attr("aria-label", "Quick amounts");
    for (label, value) in picks {
        let text = panel.deposit_text.clone();
        quick = quick.child(on_click(
            Button::new(Kind::Secondary, label)
                .size(Size::Small)
                .build(),
            move || text.set(value.to_string()),
        ));
    }
    if connected {
        let panel = panel.clone();
        quick = quick.child(on_click(
            Button::new(Kind::Secondary, "Max")
                .size(Size::Small)
                .build(),
            move || {
                if let Some(held) = panel.holdings.lock_ref().as_ref() {
                    panel
                        .deposit_text
                        .set(money::plain_exact(held.balance, decimals));
                }
            },
        ));
    }
    let continue_button = {
        let panel = panel.clone();
        on_click(
            Button::new(Kind::Primary, "Continue").block().build(),
            move || {
                let (check, _) = deposit_check(&panel, &panel.deposit_text.get_cloned(), true);
                if check == Check::Ready {
                    panel.stage.set(Stage::Review);
                }
            },
        )
        .attr_signal(
            "disabled",
            Some(String::new()),
            check
                .signal_cloned()
                .map(|(check, _)| (check != Check::Ready).then(String::new)),
        )
    };
    let fee_figure = quote.signal_cloned().map(move |quote| match quote {
        Some((quote, _)) => money::dollars(quote.fee, decimals, Rounding::Up),
        None => "—".to_string(),
    });
    let price_figure = quote.signal_cloned().map(move |quote| match quote {
        Some((_, price)) => money::price(price, decimals),
        None => "—".to_string(),
    });
    let shares_figure = quote.signal_cloned().map(|quote| match quote {
        Some((quote, _)) => format!("{} shares", money::shares(quote.shares, Rounding::Down)),
        None => "—".to_string(),
    });
    let fee_label = fee_label(panel, "Deposit fee");
    el("div")
        .class("sh-exit__form")
        .child(el("p").class("sh-exit__held").text_signal("", held_line))
        .child(
            el("div")
                .class("sh-field")
                .class_signal("sh-field--error", false, check.signal_cloned().map(|(check, _)| {
                    matches!(check, Check::Invalid(_) | Check::Over { .. } | Check::Below(_))
                }))
                .child(
                    el("div")
                        .class("sh-exit__fieldhead")
                        .child(el("label").class("sh-field__label").attr("for", "sh-deposit-amount").text("Amount"))
                        .child(el("span").class("sh-exit__balance").text_signal("", balance)),
                )
                .child(amount_input(
                    "sh-deposit-amount",
                    &panel.deposit_text,
                    text(symbol_unit),
                    check.signal_cloned().map(|(check, _)| {
                        matches!(check, Check::Invalid(_) | Check::Over { .. } | Check::Below(_))
                    }),
                ))
                .child(hint("sh-deposit-amount-hint", check.signal_cloned().map(|(check, _)| check), &panel.deposit_text, "The smallest deposit is $10.")),
        )
        .child(quick)
        .child(
            el("dl")
                .class("sh-exit__rows")
                .child(row_dyn(fee_label, fee_figure))
                .child(row("Share price now", None, price_figure))
                .child(row("You get about", None, shares_figure).class("sh-exit__row--total")),
        )
        .child(el("p").class("sh-muted sh-exit__small").text(
            "Priced a few seconds after you ask, at the next valuation time. The share price can move a little before then.",
        ))
        .child(if connected { continue_button.into() } else { View::Empty })
        .into()
}

/// "Deposit fee (0.1%, stays in the vault)", with the vault's own rate.
fn fee_label(panel: &Rc<Panel>, what: &'static str) -> impl Signal<Item = String> + 'static {
    panel.figures.signal_ref(move |figures| {
        let bps = figures
            .as_ref()
            .map(|f| f.fees.transaction_bps)
            .unwrap_or(10);
        format!(
            "{what} ({}, stays in the vault)",
            crate::format::bps(u32::from(bps))
        )
    })
}

fn row_dyn(
    label: impl Signal<Item = String> + 'static,
    figure: impl Signal<Item = String> + 'static,
) -> El {
    el("div")
        .class("sh-exit__row")
        .child(el("dt").text_signal("", label))
        .child(el("dd").class("sh-num").text_signal("", figure))
}

/// The line under a field: the hint, or the error with its one-tap fix.
fn hint(
    id: &'static str,
    check: impl Signal<Item = Check> + 'static,
    value: &Mutable<String>,
    idle: &'static str,
) -> El {
    let value = value.clone();
    el("div")
        .attr("id", id)
        .class("sh-field__hint")
        .child_signal(
            Some(text(idle)),
            check.map(move |check| {
                Some(match check {
                    Check::Empty | Check::Ready => text(idle),
                    Check::Invalid(line) | Check::Below(line) => text(line),
                    Check::Over { line, fill } => {
                        let value = value.clone();
                        View::Many(vec![
                            text(format!("{line} ")),
                            match fill {
                                Some(fill) => el("button")
                                    .attr("type", "button")
                                    .class("sh-exit__fill")
                                    .text(format!("Use {fill}"))
                                    .hook(move |dom| {
                                        dom.event(move |_: dominator::events::Click| {
                                            value.set(fill.clone())
                                        })
                                    })
                                    .into(),
                                None => View::Empty,
                            },
                        ])
                    }
                })
            }),
        )
}

fn held_sentence(
    holdings: Option<&Holdings>,
    figures: Option<&VaultFigures>,
    decimals: u8,
) -> String {
    let (Some(held), Some(figures)) = (holdings, figures) else {
        return String::new();
    };
    if held.shares.is_zero() {
        return String::new();
    }
    let value = figures
        .share_price()
        .and_then(|price| crate::money::quote::value_of(held.shares, price))
        .map(|value| format!(" ≈ {}", money::dollars(value, decimals, Rounding::Down)))
        .unwrap_or_default();
    format!(
        "You hold {} shares{value}",
        money::shares(held.shares, Rounding::Down)
    )
}

fn withdraw_form(panel: &Rc<Panel>, connected: bool) -> View {
    let closed = panel
        .figures
        .lock_ref()
        .as_ref()
        .is_some_and(|figures| figures.state == State::Closed);
    let decimals = decimals(panel);
    let check = {
        let panel = panel.clone();
        map_ref! {
            let text = panel.withdraw_text.signal_cloned(),
            let unit = panel.unit.signal(),
            let _figures = panel.figures.signal_ref(|_| ()),
            let _holdings = panel.holdings.signal_ref(|_| ()) =>
            withdraw_check(&panel, text, *unit, connected)
        }
        .broadcast()
    };
    let quote = {
        let panel = panel.clone();
        map_ref! {
            let (_, shares) = check.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            shares.zip(figures.clone()).and_then(|(shares, figures)| {
                let (quote, path) = figures.exit_quote(shares)?;
                Some((quote, path, figures.cash, figures.state == State::Closed))
            })
        }
        .broadcast()
    };
    let held_line = {
        let panel = panel.clone();
        map_ref! {
            let holdings = panel.holdings.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            held_sentence(holdings.as_ref(), figures.as_ref(), decimals)
        }
    };
    let served = panel
        .vault
        .lock_ref()
        .as_ref()
        .is_some_and(|vault| vault.served);
    let path_notice = quote.signal_cloned().map(move |quote| {
        let (_, path, cash, closed) = quote?;
        Some(match (closed, path) {
            (true, _) => notice(
                NoticeTone::Info,
                text("This vault is closed. You’re paid your share of its cash at once, with no fee."),
            )
            .into(),
            (false, ExitPath::Cash) => notice(
                NoticeTone::Info,
                text("Cash at home covers this. You’ll be paid within seconds."),
            )
            .attr("role", "status")
            .into(),
            (false, ExitPath::Booked) => notice(
                NoticeTone::Caution,
                text(format!(
                    "This is more than the vault’s cash at home ({}). Your withdrawal will be booked: the contract closes your share of each position and pays you once the cash is home.{}",
                    money::dollars(cash, decimals, Rounding::Down).replace(".00", ""),
                    if served {
                        " On this vault an unwinder finishes it. Usually minutes, sometimes longer."
                    } else {
                        ""
                    }
                )),
            )
            .attr("role", "status")
            .into(),
        })
    });
    let max_line = {
        let panel = panel.clone();
        panel.holdings.signal_cloned().map(move |held| match held {
            Some(held) if connected && !held.shares.is_zero() => {
                format!("Max: {} shares", money::shares(held.shares, Rounding::Down))
            }
            _ => String::new(),
        })
    };
    let gross = quote.signal_cloned().map(move |quote| match quote {
        Some((quote, ..)) => format!(
            "≈ {}",
            money::dollars(quote.gross, decimals, Rounding::Down)
        ),
        None => "—".to_string(),
    });
    let fee = quote.signal_cloned().map(move |quote| match quote {
        Some((quote, ..)) => money::minus_dollars(quote.fee, decimals, Rounding::Up),
        None => "—".to_string(),
    });
    let payout = quote.signal_cloned().map(move |quote| match quote {
        Some((quote, ..)) => format!(
            "≈ {}",
            money::dollars(quote.payout, decimals, Rounding::Down)
        ),
        None => "—".to_string(),
    });
    let max = {
        let panel = panel.clone();
        on_click(
            Button::new(Kind::Secondary, "Max")
                .size(Size::Small)
                .build(),
            move || {
                if let Some(held) = panel.holdings.lock_ref().as_ref() {
                    panel.unit.set(Unit::Shares);
                    panel.withdraw_text.set(money::plain_exact(held.shares, 18));
                }
            },
        )
    };
    let label = {
        map_ref! {
            let quote = quote.signal_cloned(),
            let (_, shares) = check.signal_cloned() =>
            match (quote, shares) {
                (Some((_, ExitPath::Booked, _, false)), _) => "Book withdrawal".to_string(),
                (_, Some(shares)) => format!("Withdraw {} shares", whole_shares(*shares)),
                _ => "Withdraw".to_string(),
            }
        }
    };
    let submit = {
        let panel = panel.clone();
        let check = check.clone();
        el("button")
            .attr("type", "button")
            .class("sh-btn sh-btn-primary sh-btn-block")
            .text_signal("Withdraw", label)
            .attr_signal(
                "disabled",
                Some(String::new()),
                check
                    .signal_cloned()
                    .map(|(check, _)| (check != Check::Ready).then(String::new)),
            )
            .hook(move |dom| {
                dom.event(move |_: dominator::events::Click| {
                    let (verdict, shares) = withdraw_check(
                        &panel,
                        &panel.withdraw_text.get_cloned(),
                        panel.unit.get(),
                        true,
                    );
                    if let (Check::Ready, Some(shares)) = (verdict, shares) {
                        start(&panel, Action::Withdraw(shares));
                    }
                })
            })
    };
    let unit_label = panel.unit.signal().map(|unit| match unit {
        Unit::Shares => "shares".to_string(),
        Unit::Dollars => "USDC".to_string(),
    });
    el("div")
        .class("sh-exit__form")
        .child(el("p").class("sh-exit__held").text_signal("", held_line))
        .child(
            el("div")
                .class("sh-field")
                .class_signal(
                    "sh-field--error",
                    false,
                    check
                        .signal_cloned()
                        .map(|(check, _)| matches!(check, Check::Invalid(_) | Check::Over { .. })),
                )
                .child(
                    el("div")
                        .class("sh-exit__fieldhead")
                        .child(
                            el("label")
                                .class("sh-field__label")
                                .attr("for", "sh-withdraw-amount")
                                .text("Amount"),
                        )
                        .child(segmented(
                            "Amount in",
                            &[(Unit::Shares, "Shares"), (Unit::Dollars, "USDC")],
                            &panel.unit,
                        )),
                )
                .child(amount_input(
                    "sh-withdraw-amount",
                    &panel.withdraw_text,
                    el("span").text_signal("shares", unit_label).into(),
                    check
                        .signal_cloned()
                        .map(|(check, _)| matches!(check, Check::Invalid(_) | Check::Over { .. })),
                ))
                .child(hint(
                    "sh-withdraw-amount-hint",
                    check.signal_cloned().map(|(check, _)| check),
                    &panel.withdraw_text,
                    "",
                )),
        )
        .child(
            el("div")
                .class("sh-exit__picks")
                .child(el("span").class("sh-muted").text_signal("", max_line))
                .child(if connected { max.into() } else { View::Empty }),
        )
        .child(el("div").child_signal(None, path_notice))
        .child(if closed {
            // A closed vault pays its fraction of the cash at once: no
            // request, no exit price, no fee.
            el("dl")
                .class("sh-exit__rows")
                .child(row("Your share of the cash", None, gross))
                .child(row("You get", None, payout).class("sh-exit__row--total"))
        } else {
            el("dl")
                .class("sh-exit__rows")
                .child(row(
                    "Valued at exit-side marks",
                    Some("what the positions would actually sell for"),
                    gross,
                ))
                .child(row_dyn(fee_label(panel, "Withdrawal fee"), fee))
                .child(row("You get", None, payout).class("sh-exit__row--total"))
        })
        .child(el("p").class("sh-muted sh-exit__small").text(if closed {
            "Paid at once from the vault’s cash, in one transaction."
        } else {
            "Priced a few seconds after you ask. A request can’t be cancelled."
        }))
        .child(if connected {
            submit.into()
        } else {
            View::Empty
        })
        .into()
}

/// Shares as a button names them: 2,000 or 2,000.5.
fn whole_shares(shares: U256) -> String {
    let shown = money::shares(shares, Rounding::Down);
    shown
        .strip_suffix(".0")
        .map(str::to_string)
        .unwrap_or(shown)
}

// Review ------------------------------------------------------------------------

fn review(panel: &Rc<Panel>) -> View {
    let decimals = decimals(panel);
    let symbol = symbol(panel);
    let (_, amount) = deposit_check(panel, &panel.deposit_text.get_cloned(), true);
    let Some(amount) = amount else {
        panel.stage.set(Stage::Form);
        return placeholder();
    };
    let figures = panel.figures.get_cloned();
    let quote = figures
        .as_ref()
        .and_then(|figures| figures.deposit_quote(amount));
    let price = figures.as_ref().and_then(VaultFigures::share_price);
    let first = panel
        .holdings
        .lock_ref()
        .as_ref()
        .is_none_or(|held| held.shares.is_zero());
    let label = format!(
        "Deposit {}",
        money::dollars_exact(amount, decimals).replace(".00", "")
    );
    let ack = panel.ack.clone();
    let go = {
        let panel = panel.clone();
        let ticked = ack.clone();
        on_click(
            Button::new(Kind::Primary, label).block().build(),
            move || {
                if first && !ticked.get() {
                    return;
                }
                start(&panel, Action::Deposit(amount));
            },
        )
        .attr_signal(
            "disabled",
            None,
            ack.signal()
                .map(move |ticked| (first && !ticked).then(String::new)),
        )
    };
    let edit = {
        let panel = panel.clone();
        on_click(
            el("button")
                .attr("type", "button")
                .class("sh-exit__edit")
                .text("Edit amount"),
            move || panel.stage.set(Stage::Form),
        )
    };
    let static_row = |label: &'static str, figure: String, detail: Option<String>| {
        el("div")
            .class("sh-exit__row")
            .child(el("dt").text(label))
            .child(el("dd").class("sh-num").text(figure).child(match detail {
                Some(detail) => el("span").class("sh-exit__detail").text(detail).into(),
                None => View::Empty,
            }))
    };
    let bps = figures
        .as_ref()
        .map(|f| f.fees.transaction_bps)
        .unwrap_or(10);
    el("div")
        .class("sh-exit__body")
        .child(
            el("div")
                .class("sh-exit__reviewhead")
                .child(el("h3").class("sh-exit__title").text("Review your deposit"))
                .child(edit),
        )
        .child(
            el("dl")
                .class("sh-exit__rows")
                .child(static_row("You pay", money::token_exact(amount, decimals, &symbol), None))
                .child(static_row(
                    "Deposit fee",
                    quote.map(|q| money::dollars(q.fee, decimals, Rounding::Up)).unwrap_or_default(),
                    Some(format!("{}, stays in the vault", crate::format::bps(u32::from(bps)))),
                ))
                .child(static_row(
                    "You get",
                    quote
                        .map(|q| format!("≈ {} shares", money::shares(q.shares, Rounding::Down)))
                        .unwrap_or_default(),
                    price.map(|price| format!("at about {} a share", money::price(price, decimals))),
                ))
                .child(static_row(
                    "Priced",
                    "At the next valuation time".to_string(),
                    Some("a few seconds after you confirm".to_string()),
                ))
                .child(static_row("Network fee", "Paid in your wallet".to_string(), None)),
        )
        .child(notice(
            NoticeTone::Info,
            text("A request can’t be cancelled once you confirm it. Nobody can see its price yet, including you."),
        ))
        .child(if first {
            checkbox(
                "sh-exit-ack".to_string(),
                "I understand the agent can lose money, and that a review isn’t a prediction.",
                &ack,
            )
            .into()
        } else {
            View::Empty
        })
        .child(go)
        .child(if first {
            el("p")
                .class("sh-muted sh-exit__small")
                .text_signal("", ack.signal().map(|ticked| if ticked { String::new() } else { "Tick the box to continue.".to_string() }))
                .into()
        } else {
            View::Empty
        })
        .into()
}

// Running a flow ------------------------------------------------------------------

fn start(panel: &Rc<Panel>, action: Action) {
    let Some(vault) = panel.vault.get_cloned() else {
        return;
    };
    // One flow at a time per vault and account: a second click, or another
    // panel for the same vault, starts nothing while one is shown.
    let running = Running::new(action.clone());
    if !crate::wallet::get().begin(&panel.slug, &running) {
        return;
    }
    crate::wallet::run::start(&vault, action, &running);
}

fn context_names(panel: &Panel) -> (String, String, Option<String>, String, String) {
    let session = crate::wallet::get();
    let network = panel.network.get_cloned();
    (
        session.wallet_name.get_cloned(),
        network
            .as_ref()
            .map(|n| n.label.clone())
            .unwrap_or_default(),
        session.wallet_chain().map(networks::chain_name),
        network
            .as_ref()
            .map(|n| n.asset_symbol.clone())
            .unwrap_or_default(),
        network
            .as_ref()
            .map(|n| n.coin().to_string())
            .unwrap_or_default(),
    )
}

/// Words for an error, with this panel's names.
pub fn error_words(
    error: &TxError,
    wallet: &str,
    chain: &str,
    wallet_chain: Option<&str>,
    symbol: &str,
    decimals: u8,
    native: &str,
) -> crate::tx::error::Words {
    error.words(&Context {
        wallet,
        chain,
        wallet_chain,
        symbol,
        decimals,
        native,
    })
}

fn flow_view(panel: &Rc<Panel>) -> View {
    let Some(running) = panel.running() else {
        return placeholder();
    };
    // A panel built while a flow runs shows it on its own tab.
    panel.tab.set_neq(match running.action {
        Action::Withdraw(_) => Tab::Withdraw,
        _ => Tab::Deposit,
    });
    let panel = panel.clone();
    let shown = running.clone();
    el("div")
        .class("sh-exit__body sh-exit__flow")
        .child_signal(
            None,
            running.step.signal_cloned().map(move |step| {
                let step = step?;
                // The sheet stays open only while the wallet is being asked.
                if panel.in_sheet {
                    crate::atoms::dialog::lock(matches!(step, Step::Signing(_)));
                }
                Some(step_view(&panel, &shown, &step))
            }),
        )
        .into()
}

fn tx_link(chain_id: u64, hash: alloy_primitives::B256, label: &'static str) -> View {
    let full = format!("{hash:?}");
    match networks::tx_url(chain_id, hash) {
        Some(url) => el("a")
            .class("sh-exit__txlink")
            .attr("href", url)
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .child(el("span").class("sh-label").text(label))
            .child(
                el("span")
                    .class("sh-mono")
                    .text(crate::format::short_hash(&full)),
            )
            .child(icon(Icon::External, 14))
            .child(
                el("span")
                    .class("sh-vh")
                    .text(format!("{full} (opens the explorer)")),
            )
            .into(),
        None => crate::atoms::hash::hash(&full, Some(label), "transaction hash").into(),
    }
}

fn utc(seconds: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(seconds).unwrap_or(0))
        .map(|at| crate::format::time(time::PrimitiveDateTime::new(at.date(), at.time())))
        .unwrap_or_default()
}

/// What the wallet is asked, in the panel's words.
fn prompt_words(
    prompt: &Prompt,
    wallet: &str,
    vault: &str,
    symbol: &str,
    decimals: u8,
) -> (String, String) {
    match prompt {
        Prompt::Statement => (
            format!("Sign the statement in {wallet}"),
            format!(
                "{wallet} shows: “{}” for this vault’s list. Signing moves no money.",
                chain::statement::TEXT
            ),
        ),
        Prompt::Approve { amount } => (
            format!(
                "Allow {} in {wallet}",
                money::token_exact(*amount, decimals, symbol)
            ),
            format!(
                "{wallet} asks to let the {vault} vault take exactly {}. Nothing moves yet.",
                money::token_exact(*amount, decimals, symbol)
            ),
        ),
        Prompt::Deposit { amount } => (
            format!("Approve the deposit in {wallet}"),
            format!(
                "{wallet} shows {} going to the {vault} vault. Nothing is sent until you approve.",
                money::token_exact(*amount, decimals, symbol)
            ),
        ),
        Prompt::Withdraw { shares } => (
            format!("Approve the withdrawal in {wallet}"),
            format!(
                "{wallet} shows a request to withdraw {} shares from the {vault} vault. Nothing is sent until you approve.",
                money::shares(*shares, Rounding::Down)
            ),
        ),
        Prompt::Finish => (
            format!("Approve the optional second step in {wallet}"),
            "Your request is priced. This settles it at that price; anyone may send it, and nobody had yet. It’s optional: your request stands either way, and the keeper settles it if you don’t."
                .to_string(),
        ),
        Prompt::SettleClaim { .. } => (
            format!("Approve settling in {wallet}"),
            "Anyone can send this, including you. You pay its network fee.".to_string(),
        ),
        Prompt::TakeCash => (
            format!("Approve taking your cash share in {wallet}"),
            "Your share of the vault’s cash now, less the profit fee your shares owed at the last price and the transaction fee, and units for the rest.".to_string(),
        ),
        Prompt::TakeBack { amount } => (
            format!("Approve taking your deposit back in {wallet}"),
            format!(
                "Nothing has priced it for a day: {} goes back to the wallet that paid it in, and nothing is bought.",
                money::token_exact(*amount, decimals, symbol)
            ),
        ),
        Prompt::ClaimInstead => (
            format!("Approve booking a claim in {wallet}"),
            "Your withdrawal becomes a claim, served by a close.".to_string(),
        ),
        Prompt::CollectUnits => (
            format!("Approve collecting in {wallet}"),
            "What your units have earned comes to your wallet.".to_string(),
        ),
        Prompt::CollectHeld => (
            format!("Approve collecting in {wallet}"),
            "A payout held for you comes to your wallet.".to_string(),
        ),
        Prompt::Closed => (
            format!("Approve the payout in {wallet}"),
            "The vault is closed: you’re paid your share of its cash at once.".to_string(),
        ),
    }
}

fn step_view(panel: &Rc<Panel>, running: &Running, step: &Step) -> View {
    let (wallet, chain_name, wallet_chain, symbol, native) = context_names(panel);
    let decimals = decimals(panel);
    let vault_title = panel
        .vault
        .lock_ref()
        .as_ref()
        .map(|vault| vault.title.clone())
        .unwrap_or_default();
    let chain_id = panel
        .network
        .lock_ref()
        .as_ref()
        .map(|network| network.chain_id)
        .unwrap_or_default();
    let live = |line: String| {
        el("p")
            .class("sh-vh")
            .attr("aria-live", "polite")
            .text(line)
    };
    match step {
        Step::Checking => el("div")
            .class("sh-exit__wait")
            .child(hex_ring())
            .child(el("p").class("sh-exit__title").text("Checking the vault"))
            .child(el("p").class("sh-muted").text(format!(
                "Reading its state and your balance on {chain_name}."
            )))
            .child(live("Checking the vault".to_string()))
            .into(),
        Step::Signing(prompt) => {
            let (title, line) = prompt_words(prompt, &wallet, &vault_title, &symbol, decimals);
            let running = running.clone();
            // The second step settles a request already on the chain: there
            // is nothing to cancel, and declining it is fine.
            let second = *prompt == Prompt::Finish;
            el("div")
                .class("sh-exit__wait")
                .child(hex_ring())
                .child(el("p").class("sh-exit__title").text(title.clone()))
                .child(el("p").text(line))
                .child(el("p").class("sh-muted sh-exit__small").text(if second {
                    format!("Don’t see it? Open {wallet}.")
                } else {
                    format!(
                        "Don’t see it? Open {wallet}. Once you approve, the request can’t be cancelled."
                    )
                }))
                .child(if second {
                    View::Empty
                } else {
                    on_click(Button::new(Kind::Secondary, "Cancel").build(), move || {
                        // Nothing further is sent; a prompt already open in the
                        // wallet is answered there.
                        running.cancel.cancel();
                    })
                    .into()
                })
                .child(live(title))
                .into()
        }
        Step::Sent { hash, unseen, .. } => {
            let (title, line) = if *unseen {
                (
                    "Sent. Not seen by the network yet",
                    format!(
                        "It may still go through: the app keeps watching. Check {wallet}’s activity before sending anything again."
                    ),
                )
            } else {
                ("Sent. Waiting for the network", String::new())
            };
            el("div")
                .class("sh-exit__wait")
                .child(hex_ring())
                .child(el("p").class("sh-exit__title").text(title))
                .child(if line.is_empty() {
                    View::Empty
                } else {
                    el("p").class("sh-muted").text(line).into()
                })
                .child(tx_link(chain_id, *hash, "Transaction"))
                .child(live(format!("{title}.")))
                .into()
        }
        Step::Requested(countdown) | Step::Priced { countdown, .. } => {
            let priced = matches!(step, Step::Priced { .. });
            // The app's own second step: why it didn't go through (and
            // whether it may still land), or the one sent and being watched.
            let (unsent, may_land, sent) = match step {
                Step::Priced { unsent, sent, .. } => (
                    unsent.as_ref().map(|why| {
                        if why.may_have_moved() {
                            "The app’s own second step hasn’t been seen mined yet; it may still land.".to_string()
                        } else {
                            format!(
                                "The app’s own second step didn’t go through: {}.",
                                why.unsent_words(&Context {
                                    wallet: &wallet,
                                    chain: &chain_name,
                                    wallet_chain: wallet_chain.as_deref(),
                                    symbol: &symbol,
                                    decimals,
                                    native: &native,
                                })
                            )
                        }
                    }),
                    unsent.as_ref().is_some_and(TxError::may_have_moved),
                    *sent,
                ),
                _ => (None, false, None),
            };
            let started = match running.requested_local.get() {
                Some(started) => started,
                None => {
                    let started = (crate::clock::now().get(), countdown.asked_at);
                    running.requested_local.set(Some(started));
                    started
                }
            };
            let slot = countdown.slot;
            let total = slot.saturating_sub(countdown.asked_at).max(1);
            let left = crate::clock::signal().map(move |now| {
                let elapsed = ((now - started.0) / 1000.0).max(0.0) as u64;
                slot.saturating_sub(started.1 + elapsed)
            });
            let left = left.broadcast();
            let circumference = 2.0 * std::f64::consts::PI * 20.0;
            let ring = crate::view::svg("svg")
                .class("sh-countdown")
                .attr("width", "56")
                .attr("height", "56")
                .attr("viewBox", "0 0 48 48")
                .attr("aria-hidden", "true")
                .child(
                    crate::view::svg("circle")
                        .class("sh-countdown__track")
                        .attr("cx", "24")
                        .attr("cy", "24")
                        .attr("r", "20"),
                )
                .child(
                    crate::view::svg("circle")
                        .class("sh-countdown__fill")
                        .attr("cx", "24")
                        .attr("cy", "24")
                        .attr("r", "20")
                        .attr("stroke-dasharray", format!("{circumference:.2}"))
                        .attr_signal(
                            "stroke-dashoffset",
                            None,
                            left.signal().map(move |left| {
                                let done = if priced {
                                    1.0
                                } else {
                                    1.0 - left as f64 / total as f64
                                };
                                Some(format!("{:.2}", circumference * done.clamp(0.0, 1.0)))
                            }),
                        ),
                )
                .child(
                    crate::view::svg("text")
                        .attr("x", "24")
                        .attr("y", "28")
                        .attr("text-anchor", "middle")
                        .class("sh-countdown__text")
                        .text_signal(
                            "",
                            left.signal().map(move |left| {
                                if priced {
                                    "✓".to_string()
                                } else {
                                    format!("{left} s")
                                }
                            }),
                        ),
                );
            let is_deposit = matches!(running.action, Action::Deposit(_));
            let when = if priced {
                el("p")
                    .class("sh-exit__title")
                    .text(format!("Priced at {}", utc(slot)))
            } else {
                el("p")
                    .class("sh-exit__title")
                    .text(format!("Priced at {} ", utc(slot)))
                    .child(el("span").class("sh-muted").text_signal(
                        "",
                        left.signal().map(|left| {
                            if left == 0 {
                                "any moment".to_string()
                            } else {
                                format!("in {left} s")
                            }
                        }),
                    ))
            };
            let moved = countdown.moved_from.map(|from| {
                notice(
                    NoticeTone::Info,
                    crate::atoms::notice::lead(
                        "Moved to the next valuation time.",
                        format!(
                            "Prices weren’t recorded in time for {}. You’ll be priced at {} instead. Your request still stands. You don’t need to do anything.",
                            utc(from),
                            utc(slot)
                        ),
                    ),
                )
            });
            let announce = if let Some(why) = &unsent {
                format!(
                    "Priced at {}. Your request stands and is settled at this price. {why}",
                    utc(slot)
                )
            } else if sent.is_some() {
                format!(
                    "Priced at {}. The app’s optional second step is sent; waiting to see your request settled.",
                    utc(slot)
                )
            } else if priced {
                format!(
                    "Priced at {}. The app sends the second step for you.",
                    utc(slot)
                )
            } else if let Some(from) = countdown.moved_from {
                format!(
                    "Moved to the next valuation time: {} instead of {}.",
                    utc(slot),
                    utc(from)
                )
            } else {
                format!("Request sent. Priced at {}.", utc(slot))
            };
            el("div")
                .class("sh-exit__countdown")
                .child(ring)
                .child(el("p").class("sh-label").text(if priced { "Priced" } else { "Request sent" }))
                .child(when)
                .child(moved.map(View::from).unwrap_or(View::Empty))
                .child(match (&unsent, sent) {
                    (Some(why), _) => settle_now(running, why, may_land),
                    (None, Some(hash)) => el("div")
                        .class("sh-exit__settle")
                        .child(el("p").text(
                            "The app’s second step is sent. It’s optional: your request is settled at this price either way, by the keeper or by anyone. Waiting to see it settled.",
                        ))
                        .child(tx_link(chain_id, hash, "Second step"))
                        .into(),
                    (None, None) => el("p")
                        .class("sh-muted")
                        .text(if priced {
                            "The app sends the second step for you if the keeper hasn’t."
                        } else {
                            "Nobody, including you, can see this price yet. That’s what keeps anyone from racing the vault."
                        })
                        .into(),
                })
                .child(
                    el("dl")
                        .class("sh-exit__rows")
                        .child(
                            el("div")
                                .class("sh-exit__row")
                                .child(el("dt").text("You asked at"))
                                .child(el("dd").class("sh-num").text(utc(countdown.asked_at))),
                        )
                        .child(request_amount_row(&running.action, decimals, &symbol)),
                )
                .child(tx_link(chain_id, countdown.hash, "Request"))
                .child(el("p").class("sh-muted sh-exit__small").text(if is_deposit {
                    "You can close this page. The app finishes the deposit either way."
                } else {
                    "You can close this page. The app finishes the withdrawal either way."
                }))
                .child(live(announce))
                .into()
        }
        Step::Done(outcome) => done_view(panel, running, outcome, chain_id, decimals),
        Step::Failed(error) => {
            let words = error_words(
                error,
                &wallet,
                &chain_name,
                wallet_chain.as_deref(),
                &symbol,
                decimals,
                &native,
            );
            let moved = error.may_have_moved();
            let back = {
                let panel = panel.clone();
                let running = running.clone();
                move || {
                    crate::wallet::get().dismiss(&running);
                    if moved {
                        // What was typed was sent, or may still land: the
                        // form starts empty rather than offer it again.
                        panel.stage.set(Stage::Form);
                        panel.deposit_text.set(String::new());
                        panel.withdraw_text.set(String::new());
                        panel.ack.set(false);
                    }
                }
            };
            let mut card = el("div")
                .class("sh-exit__error")
                .attr("role", if words.alert { "alert" } else { "status" })
                .child(el("p").class("sh-exit__title").text(words.title))
                .child(el("p").text(words.text));
            if let Some(hash) = error.hash() {
                card = card.child(tx_link(chain_id, hash, "Transaction"));
            }
            if moved {
                card.child(
                    crate::web::app_link("/portfolio")
                        .class("sh-btn sh-btn-primary sh-btn-block")
                        .text("View in portfolio"),
                )
                .child(on_click(
                    el("button")
                        .attr("type", "button")
                        .class("sh-exit__edit")
                        .text("Done"),
                    back,
                ))
                .into()
            } else {
                card.child(on_click(
                    Button::new(Kind::Primary, "Try again").block().build(),
                    back,
                ))
                .into()
            }
        }
    }
}

/// The second step, after the app's own try didn't go through: the request
/// stands either way, and the keeper settles it at this price. Offered again
/// unless the one sent may still land.
fn settle_now(running: &Running, why: &str, may_land: bool) -> View {
    let running = running.clone();
    let line = format!(
        "Your request stands and is settled at this price by the keeper, or by anyone. {why} It’s optional."
    );
    el("div")
        .class("sh-exit__settle")
        .child(el("p").text(line))
        .child(if may_land {
            View::Empty
        } else {
            on_click(
                Button::new(Kind::Secondary, "Settle now").build(),
                move || running.settle_now.nudge(),
            )
            .into()
        })
        .into()
}

/// What the request is for, exactly as it was sent.
fn request_amount_row(action: &Action, decimals: u8, symbol: &str) -> El {
    let (label, figure) = match action {
        Action::Deposit(amount) => ("You paid in", money::token_exact(*amount, decimals, symbol)),
        Action::Withdraw(shares) => (
            "Shares",
            format!("{} marked pending", money::shares(*shares, Rounding::Down)),
        ),
        _ => ("", String::new()),
    };
    el("div")
        .class("sh-exit__row")
        .child(el("dt").text(label))
        .child(el("dd").class("sh-num").text(figure))
}

fn done_view(
    panel: &Rc<Panel>,
    running: &Running,
    outcome: &Outcome,
    chain_id: u64,
    decimals: u8,
) -> View {
    let reset = {
        let panel = panel.clone();
        let running = running.clone();
        move || {
            crate::wallet::get().dismiss(&running);
            panel.stage.set(Stage::Form);
            panel.deposit_text.set(String::new());
            panel.withdraw_text.set(String::new());
            panel.ack.set(false);
        }
    };
    let portfolio = crate::web::app_link("/portfolio")
        .class("sh-btn sh-btn-secondary sh-btn-block")
        .text("View in portfolio");
    let again = on_click(
        el("button")
            .attr("type", "button")
            .class("sh-exit__edit")
            .text("Done"),
        reset,
    );
    let card = |title: String,
                rows: Vec<(String, String)>,
                hash: alloy_primitives::B256,
                extra: View|
     -> View {
        el("div")
            .class("sh-exit__done")
            .attr("role", "status")
            .child(seal())
            .child(el("p").class("sh-exit__title").text(title))
            .child(
                el("dl")
                    .class("sh-exit__rows")
                    .children(rows.into_iter().map(|(label, figure)| {
                        el("div")
                            .class("sh-exit__row")
                            .child(el("dt").text(label))
                            .child(el("dd").class("sh-num").text(figure))
                    })),
            )
            .child(tx_link(chain_id, hash, "Transaction"))
            .child(extra)
            .into()
    };
    // Read again from the chain once the flow lands, so it follows the
    // holdings rather than a figure taken before.
    let position = |label: &'static str| -> View {
        el("dl")
            .class("sh-exit__rows")
            .child(
                el("div")
                    .class("sh-exit__row")
                    .child(el("dt").text(label))
                    .child(el("dd").class("sh-num").text_from(&panel.holdings, |held| {
                        held.as_ref()
                            .map(|held| {
                                format!("{} shares", money::shares(held.shares, Rounding::Down))
                            })
                            .unwrap_or_default()
                    })),
            )
            .into()
    };
    match outcome {
        Outcome::Deposited {
            assets,
            shares,
            at,
            hash,
        } => {
            let fee = panel
                .figures
                .lock_ref()
                .as_ref()
                .and_then(|figures| figures.deposit_quote(*assets))
                .map(|quote| quote.fee);
            let mut rows = vec![("Priced at".to_string(), utc(*at))];
            if let Some(fee) = fee {
                rows.push((
                    "Deposit fee".to_string(),
                    format!(
                        "{} stays in the vault",
                        money::dollars(fee, decimals, Rounding::Up)
                    ),
                ));
            }
            card(
                format!(
                    "You now hold {} more shares",
                    money::shares(*shares, Rounding::Down)
                ),
                rows,
                *hash,
                View::Many(vec![
                    position("Your position"),
                    portfolio.into(),
                    again.into(),
                ]),
            )
        }
        Outcome::Refunded { assets, to, hash } => {
            let yours = crate::wallet::get().account() == Some(*to);
            card(
                "Your deposit was sent back".to_string(),
                vec![(
                    "Returned".to_string(),
                    if yours {
                        format!("{} to your wallet", money::dollars_exact(*assets, decimals))
                    } else {
                        format!(
                            "{} to {}, the wallet that paid it in",
                            money::dollars_exact(*assets, decimals),
                            crate::format::short_hash(&format!("{to:?}"))
                        )
                    },
                )],
                *hash,
                View::Many(vec![
                    el("p").class("sh-muted").text("Nothing was bought.").into(),
                    again.into(),
                ]),
            )
        }
        Outcome::Paid {
            shares,
            assets,
            at,
            hash,
        } => {
            let rows = vec![(
                format!(
                    "{} shares valued at",
                    money::shares(*shares, Rounding::Down)
                ),
                at.map(|at| format!("exit-side marks, {}", utc(at)))
                    .unwrap_or_else(|| "the closed vault’s cash".to_string()),
            )];
            card(
                format!(
                    "{} is in your wallet",
                    money::dollars(*assets, decimals, Rounding::Down)
                ),
                rows,
                *hash,
                View::Many(vec![
                    position("You still hold"),
                    portfolio.into(),
                    again.into(),
                ]),
            )
        }
        Outcome::Booked {
            claim,
            shares,
            hash,
        } => {
            let slug = panel.slug.clone();
            card(
                "Claim booked".to_string(),
                vec![("Shares pending".to_string(), money::shares(*shares, Rounding::Down))],
                *hash,
                View::Many(vec![
                    el("p").text(format!(
                        "Your {} shares are marked pending. The contract closes your share of each position within the exit band, brings the cash home and pays you. Settling is public. Anyone can push it through, including you.",
                        money::shares(*shares, Rounding::Down)
                    ))
                    .into(),
                    crate::web::app_link(format!("/portfolio/claims/{slug}-{claim}"))
                        .class("sh-btn sh-btn-primary sh-btn-block")
                        .text("Track your claim")
                        .into(),
                    again.into(),
                ]),
            )
        }
        Outcome::Claim {
            state,
            paid,
            hash,
            owner,
            ..
        } => card(
            match state {
                ClaimState::Paid => crate::pages::portfolio::paid_headline(
                    &money::dollars(*paid, decimals, Rounding::Down),
                    *owner,
                    crate::wallet::get().account(),
                ),
                ClaimState::Queued => "Queued for the next valuation".to_string(),
                ClaimState::Closing => "Closing at the venue".to_string(),
                _ => "Still waiting".to_string(),
            },
            vec![],
            *hash,
            again.into(),
        ),
        Outcome::TookCash {
            assets,
            units,
            hash,
        } => card(
            format!(
                "{} is in your wallet",
                money::dollars(*assets, decimals, Rounding::Down)
            ),
            vec![(
                "Units for the rest".to_string(),
                format!("{} of what the stations hold", money::units_share(*units)),
            )],
            *hash,
            again.into(),
        ),
        Outcome::ClaimMade { claim, hash } => {
            card(format!("Claim {claim} booked"), vec![], *hash, again.into())
        }
        Outcome::Collected { assets, hash } => card(
            format!(
                "{} is in your wallet",
                money::dollars(*assets, decimals, Rounding::Down)
            ),
            vec![],
            *hash,
            again.into(),
        ),
        // Someone else settled it before anything was asked: how it settled.
        Outcome::SettledBefore(how) => done_view(panel, running, how, chain_id, decimals),
    }
}

/// The seal moment (M7): the check drawn in.
fn seal() -> El {
    el("span")
        .class("sh-seal sh-seal-in")
        .attr("aria-hidden", "true")
        .child(icon(Icon::Check, 28))
}
