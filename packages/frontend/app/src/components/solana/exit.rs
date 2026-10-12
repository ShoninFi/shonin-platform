//! The Exit panel for a vault on Solana: the same panel as the EVM one
//! (Deposit-*, Withdraw-*, States-System), drawn with its pieces, over the
//! Solana session and flows. Every money figure is read from the vault's
//! own account on the cluster and computed in base units with the
//! program's rounding. The deposit's review shows the vault's restricted
//! list as read from its account: approving the deposit is the statement.
//! A wallet of the other kind says plainly which kind this vault needs.

use std::cell::Cell;
use std::rc::Rc;

use alloy_primitives::U256;
use chain::solana_ix::accounts::Vault;
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, Signal, SignalExt};
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::check::checkbox;
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::segmented::segmented;
use crate::components::exit::{
    Check, Tab, amount_input, held_sentence, hint, placeholder, row, row_dyn, seal, utc,
    whole_shares,
};
use crate::components::wallet::{hex_ring, open_connect};
use crate::money::format as money;
use crate::money::quote::ExitPath;
use crate::money::{AmountError, Rounding, parse_units};
use crate::tx::error::{Context, Words};
use crate::tx::read::{Holdings, State, VaultFigures};
use crate::tx::solana::flow::{Outcome, Prompt, Step};
use crate::tx::solana::port::Sig;
use crate::tx::solana::run::{SolAction, SolRunning, SolVault};
use crate::tx::solana::{SolError, author::AuthorPrompt};
use crate::view::{El, View, el, text};
use crate::wallet::solana::{SolConnection, SolNetwork};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Loading,
    Unavailable,
    SignedOut(Tab),
    /// Only an EVM wallet is connected: this vault needs a Solana one.
    OtherKind(Tab),
    Form(Tab),
    Review,
    Flow,
}

struct Panel {
    slug: String,
    vault: Mutable<Option<SolVault>>,
    missing: Mutable<bool>,
    network: Mutable<Option<Rc<SolNetwork>>>,
    /// The vault's account as last read.
    account: Mutable<Option<Rc<Vault>>>,
    figures: Mutable<Option<VaultFigures>>,
    holdings: Mutable<Option<Holdings>>,
    /// The vault's restricted list, read from its own account.
    codes: Mutable<Option<Vec<[u8; 32]>>>,
    tab: Mutable<Tab>,
    deposit_text: Mutable<String>,
    withdraw_text: Mutable<String>,
    unit: Mutable<Unit>,
    stage: Mutable<Stage>,
    ack: Mutable<bool>,
    running: Mutable<Option<SolRunning>>,
    /// The list shown on the review, which the deposit states.
    shown: Mutable<Option<[u8; 32]>>,
    requested_local: Rc<Cell<Option<(f64, u64)>>>,
    in_sheet: bool,
}

/// The Exit panel for a Solana vault.
pub fn panel(slug: &str) -> View {
    build(slug, Tab::Deposit, false)
}

/// The phone's Exit sheet for a Solana vault.
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
        account: Mutable::new(None),
        figures: Mutable::new(None),
        holdings: Mutable::new(None),
        codes: Mutable::new(None),
        tab: Mutable::new(tab),
        deposit_text: Mutable::new(String::new()),
        withdraw_text: Mutable::new(String::new()),
        unit: Mutable::new(Unit::Shares),
        stage: Mutable::new(Stage::Form),
        ack: Mutable::new(false),
        running: Mutable::new(None),
        shown: Mutable::new(None),
        requested_local: Rc::new(Cell::new(None)),
        in_sheet,
    });
    let solana = crate::wallet::solana::get();
    let evm = crate::wallet::get();
    let mode = {
        let panel = panel.clone();
        map_ref! {
            let connection = solana.connection.signal_cloned(),
            let evm = evm.connection.signal_ref(|c| c.account().is_some()),
            let network = panel.network.signal_ref(Option::is_some),
            let figures = panel.figures.signal_ref(Option::is_some),
            let tab = panel.tab.signal(),
            let stage = panel.stage.signal(),
            let running = panel.running.signal_ref(Option::is_some),
            let missing = panel.missing.signal() =>
            mode_of(connection, *evm, *network, *figures, *tab, *stage, *running, *missing)
        }
        .dedupe()
    };
    let body = {
        let panel = panel.clone();
        mode.map(move |mode| Some(render(&panel, mode)))
    };
    let loader = panel.clone();
    el("section")
        .class("sh-exit")
        .class_if(in_sheet, "sh-exit--sheet")
        .attr("aria-label", "Deposit or withdraw")
        .child(tabs(&panel))
        .child_signal(Some(placeholder()), body)
        .child(
            el("p")
                .class("sh-exit__risk")
                .text("The agent can lose money. Nothing here stops that."),
        )
        .hook(move |dom| dom.future(follow(loader)))
        .into()
}

#[allow(clippy::too_many_arguments)]
fn mode_of(
    connection: &SolConnection,
    evm: bool,
    network: bool,
    figures: bool,
    tab: Tab,
    stage: Stage,
    running: bool,
    missing: bool,
) -> Mode {
    if missing {
        return Mode::Unavailable;
    }
    if running {
        return Mode::Flow;
    }
    if !network || !figures {
        return Mode::Loading;
    }
    if connection.account().is_none() {
        return if evm {
            Mode::OtherKind(tab)
        } else {
            Mode::SignedOut(tab)
        };
    }
    match (tab, stage) {
        (Tab::Deposit, Stage::Review) => Mode::Review,
        _ => Mode::Form(tab),
    }
}

/// Keep the vault's figures, its list and the wallet's holdings fresh: on
/// load, every 10 s, and whenever the account changes or a flow lands.
async fn follow(panel: Rc<Panel>) {
    #[cfg(target_arch = "wasm32")]
    {
        use chain::solana_ix::{address, ids};

        use crate::tx::solana::flow;
        use crate::tx::solana::port::SolReader;
        use crate::wallet::solana::SolSession;

        let session = crate::wallet::solana::get();
        let mut vault = None;
        for _ in 0..40 {
            vault = crate::tx::solana::run::vault_of(&panel.slug);
            if vault.is_some() {
                break;
            }
            gloo_timers::future::TimeoutFuture::new(250).await;
        }
        let Some(vault) = vault else {
            panel.missing.set(true);
            return;
        };
        panel.vault.set(Some(vault.clone()));
        let Some(network) = session.load_network().await else {
            panel.missing.set(true);
            return;
        };
        panel.network.set(Some(network.clone()));
        let reader = SolSession::reader(&network);
        loop {
            if let Ok(Some(account)) = reader.account(&vault.address).await
                && account.owner == ids::VAULT
                && let Some(read) = Vault::read(&account.data)
            {
                panel
                    .figures
                    .set(Some(crate::tx::solana::read::figures(&read)));
                let read = Rc::new(read);
                panel.account.set(Some(read.clone()));
                if let Ok(codes) = flow::restricted(&reader, &vault.address).await {
                    panel.codes.set_neq(Some(codes));
                }
                match session.account() {
                    Some(owner) => {
                        let holder = flow::holder(&reader, &vault.address, &owner)
                            .await
                            .ok()
                            .flatten();
                        let balance = match reader
                            .account(&address::dollars(&owner, &network.mint))
                            .await
                        {
                            Ok(Some(account)) => {
                                chain::solana_ix::accounts::token_amount(&account.data)
                            }
                            Ok(None) => Some(0),
                            Err(_) => None,
                        };
                        if let Some(balance) = balance {
                            panel.holdings.set(Some(crate::tx::solana::read::holdings(
                                &read,
                                holder.as_ref(),
                                balance,
                            )));
                        }
                    }
                    None => panel.holdings.set(None),
                }
            } else if panel.figures.lock_ref().is_none() {
                panel.missing.set(true);
            }
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
                    if panel.running.lock_ref().is_none() {
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
                Tab::Deposit => {
                    "This vault is on Solana. Connect a Solana wallet, such as Phantom, to deposit. The smallest deposit is $10."
                }
                Tab::Withdraw => "Connect the Solana wallet that holds your shares to withdraw.",
            }))
            .child(
                on_click(
                    Button::new(Kind::Primary, "Connect a Solana wallet")
                        .block()
                        .build(),
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
        Mode::OtherKind(tab) => body
            .attr("role", "status")
            .child(form(panel, tab, false))
            .child(
                el("p")
                    .class("sh-exit__title")
                    .child(icon(Icon::Concern, 18))
                    .text(" This vault needs a Solana wallet"),
            )
            .child(el("p").text(
                "It’s on Solana, and the wallet you connected is for the EVM chains. Connect a Solana wallet, such as Phantom, as well: both can stay connected.",
            ))
            .child(
                on_click(
                    Button::new(Kind::Primary, "Connect a Solana wallet")
                        .block()
                        .build(),
                    open_connect,
                )
                .attr("aria-haspopup", "dialog"),
            )
            .into(),
        Mode::Form(tab) => body.child(form(panel, tab, true)).into(),
        Mode::Review => review(panel),
        Mode::Flow => flow_view(panel),
    }
}

// The form ---------------------------------------------------------------------------------

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
        .unwrap_or_else(|| "Solana".to_string())
}

fn deposit_check(panel: &Panel, text: &str, connected: bool) -> (Check, Option<U256>) {
    let decimals = decimals(panel);
    let amount = match parse_units(text, decimals) {
        Err(AmountError::Empty) => return (Check::Empty, None),
        Err(error) => return (Check::Invalid(error.words()), None),
        Ok(amount) if amount.is_zero() => return (Check::Empty, None),
        Ok(amount) => amount,
    };
    if u64::try_from(amount).is_err() {
        return (
            Check::Invalid("That’s more than a Solana dollar account can hold.".to_string()),
            None,
        );
    }
    if let Some(figures) = panel.figures.lock_ref().as_ref() {
        if figures.state != State::Open || figures.ready_to_close {
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
            Ok(dollars) => match figures.as_ref().and_then(|f| f.shares_for(dollars)) {
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
    if u128::try_from(shares).is_err() {
        return (
            Check::Invalid("That’s more shares than exist.".to_string()),
            None,
        );
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

fn form(panel: &Rc<Panel>, tab: Tab, connected: bool) -> View {
    match tab {
        Tab::Deposit => deposit_form(panel, connected),
        Tab::Withdraw => withdraw_form(panel, connected),
    }
}

/// What a deposit buys at the last price, as the program settles one.
fn deposit_quote(panel: &Panel, amount: U256) -> Option<crate::money::quote::DepositQuote> {
    let account = panel.account.get_cloned()?;
    crate::tx::solana::read::deposit_quote(&account, amount)
}

fn deposit_form(panel: &Rc<Panel>, connected: bool) -> View {
    let figures = panel.figures.get_cloned();
    if let Some(figures) = figures.filter(|f| f.state != State::Open || f.ready_to_close) {
        let line = match figures.state {
            State::Closed => {
                "This vault is closed and won’t take deposits again. Withdraw from its cash whenever you like."
            }
            State::WindingDown => {
                "This agent is retiring: its vault takes no new deposits. Holders can withdraw as usual."
            }
            State::Frozen | State::Killed => {
                "This vault has stopped and takes no deposits. Holders can still withdraw."
            }
            _ => "This vault isn’t taking deposits.",
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
        map_ref! {
            let holdings = panel.holdings.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            held_sentence(holdings.as_ref(), figures.as_ref(), decimals)
        }
    };
    let balance = {
        let symbol = symbol(panel);
        panel.holdings.signal_cloned().map(move |held| match held {
            Some(held) if connected => format!(
                "In your wallet: {}",
                money::token_balance(held.balance, decimals, &symbol)
            ),
            _ => String::new(),
        })
    };
    let quote = {
        let panel = panel.clone();
        map_ref! {
            let (_, amount) = check.signal_cloned(),
            let figures = panel.figures.signal_cloned(),
            let _account = panel.account.signal_ref(|_| ()) =>
            amount.zip(figures.clone()).and_then(|(amount, figures)| {
                Some((deposit_quote(&panel, amount)?, figures.share_price()?))
            })
        }
        .broadcast()
    };
    let mut quick = el("div")
        .class("sh-exit__picks")
        .attr("role", "group")
        .attr("aria-label", "Quick amounts");
    for (label, value) in [("$100", "100"), ("$500", "500"), ("$1,000", "1000")] {
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
                if check == Check::Ready && panel.codes.lock_ref().is_some() {
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
    let invalid = |check: &(Check, Option<U256>)| {
        matches!(
            check.0,
            Check::Invalid(_) | Check::Over { .. } | Check::Below(_)
        )
    };
    el("div")
        .class("sh-exit__form")
        .child(el("p").class("sh-exit__held").text_signal("", held_line))
        .child(
            el("div")
                .class("sh-field")
                .class_signal("sh-field--error", false, check.signal_ref(invalid))
                .child(
                    el("div")
                        .class("sh-exit__fieldhead")
                        .child(
                            el("label")
                                .class("sh-field__label")
                                .attr("for", "sh-deposit-amount")
                                .text("Amount"),
                        )
                        .child(el("span").class("sh-exit__balance").text_signal("", balance)),
                )
                .child(amount_input(
                    "sh-deposit-amount",
                    &panel.deposit_text,
                    text(symbol(panel)),
                    check.signal_ref(invalid),
                ))
                .child(hint(
                    "sh-deposit-amount-hint",
                    check.signal_cloned().map(|(check, _)| check),
                    &panel.deposit_text,
                    "The smallest deposit is $10.",
                )),
        )
        .child(quick)
        .child(
            el("dl")
                .class("sh-exit__rows")
                .child(row_dyn(fee_label(panel, "Deposit fee"), fee_figure))
                .child(row("Share price now", None, price_figure))
                .child(row("You get about", None, shares_figure).class("sh-exit__row--total")),
        )
        .child(el("p").class("sh-muted sh-exit__small").text(
            "Priced a few seconds after you ask, at the next valuation time. The share price can move a little before then.",
        ))
        .child(if connected {
            continue_button.into()
        } else {
            View::Empty
        })
        .into()
}

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
        map_ref! {
            let holdings = panel.holdings.signal_cloned(),
            let figures = panel.figures.signal_cloned() =>
            held_sentence(holdings.as_ref(), figures.as_ref(), decimals)
        }
    };
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
                    "This is more than the vault’s cash at home ({}). Your withdrawal will be booked: the program closes your share of each position and pays you once the cash is home.",
                    money::dollars(cash, decimals, Rounding::Down).replace(".00", ""),
                )),
            )
            .attr("role", "status")
            .into(),
        })
    });
    let max_line = panel.holdings.signal_cloned().map(move |held| match held {
        Some(held) if connected && !held.shares.is_zero() => {
            format!("Max: {} shares", money::shares(held.shares, Rounding::Down))
        }
        _ => String::new(),
    });
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
    let label = map_ref! {
        let quote = quote.signal_cloned(),
        let (_, shares) = check.signal_cloned() =>
        match (quote, shares) {
            (Some((_, ExitPath::Booked, _, false)), _) => "Book withdrawal".to_string(),
            (_, Some(shares)) => format!("Withdraw {} shares", whole_shares(*shares)),
            _ => "Withdraw".to_string(),
        }
    };
    let submit = {
        let panel = panel.clone();
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
                    if let (Check::Ready, Some(shares)) = (verdict, shares)
                        && let Ok(shares) = u128::try_from(shares)
                    {
                        start(&panel, SolAction::Withdraw(shares));
                    }
                })
            })
    };
    let unit_label = panel.unit.signal().map(|unit| match unit {
        Unit::Shares => "shares".to_string(),
        Unit::Dollars => "USDC".to_string(),
    });
    let invalid =
        |check: &(Check, Option<U256>)| matches!(check.0, Check::Invalid(_) | Check::Over { .. });
    el("div")
        .class("sh-exit__form")
        .child(el("p").class("sh-exit__held").text_signal("", held_line))
        .child(
            el("div")
                .class("sh-field")
                .class_signal("sh-field--error", false, check.signal_ref(invalid))
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
                    check.signal_ref(invalid),
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

// Review: the deposit and its statement ---------------------------------------------------------

fn review(panel: &Rc<Panel>) -> View {
    let decimals = decimals(panel);
    let symbol = symbol(panel);
    let (_, amount) = deposit_check(panel, &panel.deposit_text.get_cloned(), true);
    let (Some(amount), Some(codes)) = (amount, panel.codes.get_cloned()) else {
        panel.stage.set(Stage::Form);
        return placeholder();
    };
    // What this review shows is what the deposit states, and nothing else.
    let shown = chain::solana_ix::read::restricted_hash(&codes);
    panel.shown.set(Some(shown));
    let quote = deposit_quote(panel, amount);
    let price = panel
        .figures
        .lock_ref()
        .as_ref()
        .and_then(VaultFigures::share_price);
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
                let Ok(amount) = u64::try_from(amount) else {
                    return;
                };
                start(&panel, SolAction::Deposit { amount, shown });
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
    let bps = panel
        .figures
        .lock_ref()
        .as_ref()
        .map(|f| f.fees.transaction_bps)
        .unwrap_or(10);
    let list = codes
        .iter()
        .map(chain::solana_ix::read::code_text)
        .collect::<Vec<_>>();
    let list = if list.is_empty() {
        "no one".to_string()
    } else {
        list.join(", ")
    };
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
                    quote
                        .map(|q| money::dollars(q.fee, decimals, Rounding::Up))
                        .unwrap_or_default(),
                    Some(format!(
                        "{}, stays in the vault",
                        crate::format::bps(u32::from(bps))
                    )),
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
                .child(static_row("Network fee", "Paid in SOL by your wallet".to_string(), None)),
        )
        .child(
            el("div")
                .class("sh-notice sh-notice--info")
                .child(
                    el("div")
                        .class("sh-notice__body")
                        .child(el("p").class("sh-notice__lead").text("Your statement"))
                        .child(el("p").text(format!("“{}”", chain::statement::TEXT)))
                        .child(el("p").text(format!(
                            "This vault’s restricted list, read from its account on Solana: {list}."
                        )))
                        .child(el("p").class("sh-muted").text(
                            "On Solana the deposit itself is the statement: approving it in your wallet states this for the list above. There’s no separate signature.",
                        )),
                ),
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
        .into()
}

// Running a flow ---------------------------------------------------------------------------

fn start(panel: &Rc<Panel>, action: SolAction) {
    let Some(vault) = panel.vault.get_cloned() else {
        return;
    };
    let running = SolRunning::new();
    panel.requested_local.set(None);
    if panel.in_sheet {
        crate::atoms::dialog::lock(true);
    }
    crate::tx::solana::run::start(vault.address, action, &running);
    panel.running.set(Some(running));
}

/// Words for a Solana error, with this network's names.
pub fn sol_error_words(error: &SolError, network: Option<&SolNetwork>) -> Words {
    let session = crate::wallet::solana::get();
    let wallet = session.wallet_name.get_cloned();
    let chain = network.map_or_else(|| "Solana".to_string(), |n| n.label.clone());
    let symbol = network.map_or_else(|| "USDC".to_string(), |n| n.asset_symbol.clone());
    let decimals = network.map_or(6, |n| n.asset_decimals);
    error.words(&Context {
        wallet: if wallet.is_empty() {
            "your wallet"
        } else {
            &wallet
        },
        chain: &chain,
        wallet_chain: None,
        symbol: &symbol,
        decimals,
        native: "SOL",
    })
}

/// A transaction's link on the explorer.
pub fn sig_link(network: Option<&SolNetwork>, signature: &Sig, label: &'static str) -> View {
    let full = signature.to_string();
    match network {
        Some(network) => el("a")
            .class("sh-exit__txlink")
            .attr("href", network.tx_url(&full))
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .child(el("span").class("sh-label").text(label))
            .child(el("span").class("sh-mono").text(short(&full)))
            .child(icon(Icon::External, 14))
            .child(
                el("span")
                    .class("sh-vh")
                    .text(format!("{full} (opens the explorer)")),
            )
            .into(),
        None => crate::atoms::hash::hash(&full, Some(label), "transaction signature").into(),
    }
}

/// A base58 key or signature, shortened: `4fNN…FnV5`.
pub fn short(full: &str) -> String {
    if full.chars().count() <= 12 {
        return full.to_string();
    }
    let head: String = full.chars().take(4).collect();
    let tail: String = full
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// What the wallet is asked, in the panel's words.
pub fn prompt_words(
    prompt: &Prompt,
    wallet: &str,
    vault: &str,
    symbol: &str,
    decimals: u8,
) -> (String, String) {
    match prompt {
        Prompt::Deposit { amount } => (
            format!("Approve the deposit in {wallet}"),
            format!(
                "{wallet} shows {} going to the {vault} vault. Approving it is also your statement for the vault’s list. Nothing is sent until you approve.",
                money::token_exact(U256::from(*amount), decimals, symbol)
            ),
        ),
        Prompt::Withdraw { shares } => (
            format!("Approve the withdrawal in {wallet}"),
            format!(
                "{wallet} shows a request to withdraw {} shares from the {vault} vault. Nothing is sent until you approve.",
                money::shares(U256::from(*shares), Rounding::Down)
            ),
        ),
        Prompt::SettleClaim { .. } => (
            format!("Approve settling in {wallet}"),
            "Anyone can send this, including you. You pay its network fee.".to_string(),
        ),
        Prompt::TakeCash => (
            format!("Approve taking your cash share in {wallet}"),
            "Your exact share of the vault’s cash now, and units for the rest.".to_string(),
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
        Prompt::Author(prompt) => (
            format!("Approve in {wallet}"),
            match prompt {
                AuthorPrompt::FundTank { assets } => format!(
                    "{} from your wallet into the tank.",
                    money::token_exact(U256::from(*assets), decimals, symbol)
                ),
                AuthorPrompt::TopUpFromShares { shares } => format!(
                    "Redeem {} of your shares into the tank, at the next valuation.",
                    money::shares(U256::from(*shares), Rounding::Down)
                ),
                AuthorPrompt::SetKeep { .. } => {
                    "The level auto-fill keeps the tank at.".to_string()
                }
                AuthorPrompt::RequestEscape => {
                    "Ask to withdraw the tank without a signed usage total.".to_string()
                }
                AuthorPrompt::WithdrawTank { assets } => format!(
                    "{} from the tank to your wallet.",
                    money::token_exact(U256::from(*assets), decimals, symbol)
                ),
                AuthorPrompt::Allowances => "The agent’s new allowances.".to_string(),
                AuthorPrompt::IdleFloor { .. } => "The vault’s new idle floor.".to_string(),
                AuthorPrompt::Retire => "Retire the vault: no new deposits.".to_string(),
            },
        ),
    }
}

fn flow_view(panel: &Rc<Panel>) -> View {
    let Some(running) = panel.running.get_cloned() else {
        return placeholder();
    };
    let panel = panel.clone();
    el("div")
        .class("sh-exit__body sh-exit__flow")
        .child_signal(
            None,
            running.step.signal_cloned().map(move |step| {
                let step = step?;
                let unlocked = matches!(
                    step,
                    Step::Done(_) | Step::Failed(_) | Step::Requested(_) | Step::Priced { .. }
                );
                if panel.in_sheet && unlocked {
                    crate::atoms::dialog::lock(false);
                }
                Some(step_view(&panel, &step))
            }),
        )
        .into()
}

fn live(line: String) -> El {
    el("p")
        .class("sh-vh")
        .attr("aria-live", "polite")
        .text(line)
}

fn step_view(panel: &Rc<Panel>, step: &Step) -> View {
    let session = crate::wallet::solana::get();
    let wallet = session.wallet_name.get_cloned();
    let network = panel.network.get_cloned();
    let decimals = decimals(panel);
    let symbol = symbol(panel);
    let vault_title = panel
        .vault
        .lock_ref()
        .as_ref()
        .map(|vault| vault.title.clone())
        .unwrap_or_default();
    match step {
        Step::Checking => el("div")
            .class("sh-exit__wait")
            .child(hex_ring())
            .child(el("p").class("sh-exit__title").text("Checking the vault"))
            .child(el("p").class("sh-muted").text(format!(
                "Reading its account and your balance on {}.",
                chain_label(panel)
            )))
            .child(live("Checking the vault".to_string()))
            .into(),
        Step::Signing(prompt) => {
            let (title, line) = prompt_words(prompt, &wallet, &vault_title, &symbol, decimals);
            let running = panel.running.get_cloned();
            el("div")
                .class("sh-exit__wait")
                .child(hex_ring())
                .child(el("p").class("sh-exit__title").text(title.clone()))
                .child(el("p").text(line))
                .child(el("p").class("sh-muted sh-exit__small").text(format!(
                    "Don’t see it? Open {wallet}. Once you approve, the request can’t be cancelled."
                )))
                .child(on_click(
                    Button::new(Kind::Secondary, "Cancel").build(),
                    move || {
                        if let Some(running) = &running {
                            running.cancel.cancel();
                        }
                    },
                ))
                .child(live(title))
                .into()
        }
        Step::Sent { signature, .. } => el("div")
            .class("sh-exit__wait")
            .child(hex_ring())
            .child(
                el("p")
                    .class("sh-exit__title")
                    .text("Sent. Waiting for the network"),
            )
            .child(sig_link(network.as_deref(), signature, "Transaction"))
            .child(live("Sent. Waiting for the network.".to_string()))
            .into(),
        Step::Requested(countdown) | Step::Priced { countdown, .. } => {
            let priced = matches!(step, Step::Priced { .. });
            let started = match panel.requested_local.get() {
                Some(started) => started,
                None => {
                    let started = (crate::clock::now().get(), countdown.asked_at);
                    panel.requested_local.set(Some(started));
                    started
                }
            };
            let slot = countdown.slot;
            let total = slot.saturating_sub(countdown.asked_at).max(1);
            let left = crate::clock::signal()
                .map(move |now| {
                    let elapsed = ((now - started.0) / 1000.0).max(0.0) as u64;
                    slot.saturating_sub(started.1 + elapsed)
                })
                .broadcast();
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
            let announce = if priced {
                format!("Priced at {}. Settling now.", utc(slot))
            } else if let Some(from) = countdown.moved_from {
                format!(
                    "Moved to the next valuation time: {} instead of {}.",
                    utc(slot),
                    utc(from)
                )
            } else {
                format!("Request sent. Priced at {}.", utc(slot))
            };
            let is_deposit = panel.tab.get() == Tab::Deposit;
            el("div")
                .class("sh-exit__countdown")
                .child(ring)
                .child(el("p").class("sh-label").text(if priced {
                    "Priced"
                } else {
                    "Request sent"
                }))
                .child(when)
                .child(moved.map(View::from).unwrap_or(View::Empty))
                .child(el("p").class("sh-muted").text(if priced {
                    "The platform’s keeper settles it now. Anyone may."
                } else {
                    "Nobody, including you, can see this price yet. That’s what keeps anyone from racing the vault."
                }))
                .child(
                    el("dl").class("sh-exit__rows").child(
                        el("div")
                            .class("sh-exit__row")
                            .child(el("dt").text("You asked at"))
                            .child(el("dd").class("sh-num").text(utc(countdown.asked_at))),
                    ),
                )
                .child(sig_link(network.as_deref(), &countdown.signature, "Request"))
                .child(el("p").class("sh-muted sh-exit__small").text(if is_deposit {
                    "You can close this page. The deposit finishes either way."
                } else {
                    "You can close this page. The withdrawal finishes either way."
                }))
                .child(live(announce))
                .into()
        }
        Step::Done(outcome) => done_view(panel, outcome, network.as_deref(), decimals),
        Step::Failed(error) => {
            let words = sol_error_words(error, network.as_deref());
            let back = {
                let panel = panel.clone();
                move || {
                    panel.running.set(None);
                }
            };
            let mut card = el("div")
                .class("sh-exit__error")
                .attr("role", if words.alert { "alert" } else { "status" })
                .child(el("p").class("sh-exit__title").text(words.title))
                .child(el("p").text(words.text));
            if let Some(signature) = error.signature() {
                card = card.child(sig_link(network.as_deref(), &signature, "Transaction"));
            }
            if matches!(error, SolError::Pending { .. }) {
                // The request stands: sending it again would make a second.
                return card
                    .child(
                        crate::web::app_link("/portfolio")
                            .class("sh-btn sh-btn-secondary sh-btn-block")
                            .text("View in portfolio"),
                    )
                    .child(on_click(
                        el("button")
                            .attr("type", "button")
                            .class("sh-exit__edit")
                            .text("Done"),
                        back,
                    ))
                    .into();
            }
            card.child(on_click(
                Button::new(Kind::Primary, "Try again").block().build(),
                back,
            ))
            .into()
        }
    }
}

fn done_view(
    panel: &Rc<Panel>,
    outcome: &Outcome,
    network: Option<&SolNetwork>,
    decimals: u8,
) -> View {
    let reset = {
        let panel = panel.clone();
        move || {
            panel.running.set(None);
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
    let card = |title: String, rows: Vec<(String, String)>, signature: Sig, extra: View| -> View {
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
            .child(sig_link(network, &signature, "Transaction"))
            .child(extra)
            .into()
    };
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
    let dollars = |assets: u64| money::dollars(U256::from(assets), decimals, Rounding::Down);
    match outcome {
        Outcome::Deposited { shares, at, signature, .. } => card(
            format!(
                "You now hold {} more shares",
                money::shares(U256::from(*shares), Rounding::Down)
            ),
            vec![("Priced at".to_string(), utc(*at))],
            *signature,
            View::Many(vec![position("Your position"), portfolio.into(), again.into()]),
        ),
        Outcome::Refunded { assets, signature } => card(
            "Your deposit was sent back".to_string(),
            vec![(
                "Returned".to_string(),
                money::dollars_exact(U256::from(*assets), decimals),
            )],
            *signature,
            View::Many(vec![
                el("p").class("sh-muted").text("The vault stopped taking deposits, or its list changed, before your request was priced. Nothing was bought.").into(),
                again.into(),
            ]),
        ),
        Outcome::Paid { shares, assets, at, signature } => card(
            format!("{} is in your wallet", dollars(*assets)),
            vec![(
                format!(
                    "{} shares valued at",
                    money::shares(U256::from(*shares), Rounding::Down)
                ),
                at.map(|at| format!("exit-side marks, {}", utc(at)))
                    .unwrap_or_else(|| "the closed vault’s cash".to_string()),
            )],
            *signature,
            View::Many(vec![position("You still hold"), portfolio.into(), again.into()]),
        ),
        Outcome::Booked { claim, shares, signature } => {
            let slug = panel.slug.clone();
            card(
                "Claim booked".to_string(),
                vec![(
                    "Shares pending".to_string(),
                    money::shares(U256::from(*shares), Rounding::Down),
                )],
                *signature,
                View::Many(vec![
                    el("p").text("Your shares are marked pending. The program closes your share of each position within the exit band, brings the cash home and pays you. Settling is public. Anyone can push it through, including you.").into(),
                    crate::web::app_link(format!("/portfolio/claims/{slug}-{claim}"))
                        .class("sh-btn sh-btn-primary sh-btn-block")
                        .text("Track your claim")
                        .into(),
                    again.into(),
                ]),
            )
        }
        Outcome::Claim { state, paid, signature, .. } => card(
            match state {
                chain::solana_ix::accounts::ClaimState::Paid => {
                    format!("{} is in your wallet", dollars(*paid))
                }
                chain::solana_ix::accounts::ClaimState::Queued => {
                    "Queued for the next valuation".to_string()
                }
                _ => "Still waiting".to_string(),
            },
            vec![],
            *signature,
            again.into(),
        ),
        Outcome::TookCash { assets, signature, .. } => card(
            format!("{} is in your wallet", dollars(*assets)),
            vec![],
            *signature,
            again.into(),
        ),
        Outcome::ClaimMade { claim, signature } => {
            card(format!("Claim {claim} booked"), vec![], *signature, again.into())
        }
        Outcome::Collected { assets, signature } => card(
            format!("{} is in your wallet", dollars(*assets)),
            vec![],
            *signature,
            again.into(),
        ),
        Outcome::Changed { signature } => card("Done".to_string(), vec![], *signature, again.into()),
    }
}
