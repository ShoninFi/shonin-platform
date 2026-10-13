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
    /// How many flows had landed in this session (its `changed`) when the
    /// holdings were last read: a Done card shows a position only once it
    /// was read after its flow landed.
    holdings_at: Mutable<u64>,
    /// The vault's restricted list as it writes it (`SANCTIONED,US`), read
    /// as the deposit statement reads it: what the review shows.
    restricted: Mutable<Option<String>>,
    tab: Mutable<Tab>,
    deposit_text: Mutable<String>,
    withdraw_text: Mutable<String>,
    unit: Mutable<Unit>,
    stage: Mutable<Stage>,
    ack: Mutable<bool>,
    /// Bumped by "Try again" on the unreachable state: read again now.
    retry: Mutable<u32>,
    /// Whether the panel is in the phone sheet (locked while signing).
    in_sheet: bool,
}

impl Panel {
    fn new(slug: &str, tab: Tab, in_sheet: bool) -> Self {
        Self {
            slug: slug.to_string(),
            vault: Mutable::new(None),
            missing: Mutable::new(false),
            network: Mutable::new(None),
            figures: Mutable::new(None),
            holdings: Mutable::new(None),
            holdings_at: Mutable::new(0),
            restricted: Mutable::new(None),
            tab: Mutable::new(tab),
            deposit_text: Mutable::new(String::new()),
            withdraw_text: Mutable::new(String::new()),
            unit: Mutable::new(Unit::Shares),
            stage: Mutable::new(Stage::Form),
            ack: Mutable::new(false),
            retry: Mutable::new(0),
            in_sheet,
        }
    }

    /// The empty form: nothing typed, no review, nothing acknowledged. A
    /// flow starting takes what was typed with it, and another account
    /// never sees what this one typed or reviewed.
    fn clear_form(&self) {
        self.stage.set(Stage::Form);
        self.deposit_text.set(String::new());
        self.withdraw_text.set(String::new());
        self.ack.set(false);
    }

    /// The flow this vault's panel shows for the connected account: kept by
    /// the session, so a panel or sheet built while it runs shows it too.
    fn running(&self) -> Option<Running> {
        crate::wallet::get().flow(&self.slug, EXIT)
    }
}

/// The Exit panel for an agent, for the agent page's right column.
pub fn panel(slug: &str) -> View {
    // A Solana vault's panel is signed by a Solana wallet.
    if crate::tx::solana::run::is_solana(slug) {
        return super::solana::exit::panel(slug);
    }
    build(slug, Tab::Deposit, false)
}

/// The phone's Exit sheet, opened by the action bar's Deposit or Withdraw.
pub fn open_sheet(slug: &str, tab: Tab) {
    if crate::tx::solana::run::is_solana(slug) {
        return super::solana::exit::open_sheet(slug, tab);
    }
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
    let panel = Rc::new(Panel::new(slug, tab, in_sheet));
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
    let (tabs, why) = tabs(&panel);
    let loader = panel.clone();
    let acting = panel.network.clone();
    // Another account (or none) gets the empty form.
    let account_change = {
        let panel = panel.clone();
        let mut seen = None;
        session
            .connection
            .signal_ref(Connection::account)
            .dedupe()
            .for_each(move |account| {
                if account_moved(&mut seen, account) {
                    panel.clear_form();
                }
                async {}
            })
    };
    el("section")
        .class("sh-exit")
        .class_if(in_sheet, "sh-exit--sheet")
        .attr("aria-label", "Deposit or withdraw")
        .child(
            el("div")
                .class("sh-exit__head")
                .child(
                    el("h2")
                        .class("sh-exit__heading")
                        .text("Deposit or withdraw"),
                )
                .child(tabs),
        )
        .child(why)
        .child_signal(Some(placeholder()), body)
        .child(
            el("p")
                .class("sh-exit__risk sh-exit__note")
                .child(icon(Icon::Concern, 16).class("sh-exit__warn"))
                .text("The agent can lose money. Nothing here stops that."),
        )
        .hook(move |dom| {
            dom.future(follow(loader))
                .future(account_change)
                // While open, the header's chip compares the wallet's chain
                // with this vault's.
                .future(super::wallet::acting_on(acting.signal_ref(|network| {
                    network.as_ref().map(|network| network.chain_id)
                })))
        })
        .into()
}

/// Whether the connected account (or none) is another than the one the
/// panel last saw; the first look moves nothing.
fn account_moved(
    seen: &mut Option<Option<alloy_primitives::Address>>,
    now: Option<alloy_primitives::Address>,
) -> bool {
    seen.replace(now).is_some_and(|before| before != now)
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
/// 10 s, and whenever the account changes, a flow lands or the reader asks
/// to try again. Until the vault has been read once, a failed read shows
/// the unreachable state with "Try again", never an endless skeleton.
async fn follow(panel: Rc<Panel>) {
    #[cfg(target_arch = "wasm32")]
    {
        use futures_signals::map_ref;

        use crate::tx::read;
        use crate::wallet::session::Session;
        let session = crate::wallet::get();
        // The page's own snapshot reader fills the vault directory: wait for
        // its first answer (or its first failure) rather than fetch the
        // snapshot a second time.
        let store = crate::store::get();
        map_ref! {
            let loaded = store.loaded.signal(),
            let failing = store.sync.failing.signal() =>
            *loaded || *failing
        }
        .wait_for(true)
        .await;
        let (vault, network) = loop {
            let vaults = session.read_vaults().await;
            let found = match vaults.get(&panel.slug).cloned() {
                Some(vault) => session
                    .network(vault.chain)
                    .await
                    .map(|network| (vault, network)),
                None => None,
            };
            if let Some(found) = found {
                panel.missing.set_neq(false);
                break found;
            }
            panel.missing.set_neq(true);
            wait_or_retry(&panel, 15).await;
        };
        panel.vault.set(Some(vault.clone()));
        panel.network.set(Some(network.clone()));
        let reader = Session::reader(&network);
        loop {
            // Before the figures, so the review never opens without it.
            if let Ok(restricted) = read::restricted_text(&reader, vault.address).await {
                panel.restricted.set_neq(Some(restricted));
            }
            match read::figures(&reader, vault.address).await {
                Ok(figures) => {
                    panel.figures.set(Some(figures));
                    panel.missing.set_neq(false);
                }
                // Never read yet: say so, with a way to try again.
                Err(_) if panel.figures.lock_ref().is_none() => panel.missing.set_neq(true),
                Err(_) => {}
            }
            // What had landed before this read: the read shows it.
            let at = session.changed.get();
            match session.account() {
                Some(account) => {
                    if let Ok(held) =
                        read::holdings(&reader, vault.address, network.asset, account).await
                    {
                        panel.holdings.set(Some(held));
                        panel.holdings_at.set(at);
                    }
                }
                None => panel.holdings.set(None),
            }
            // A flow that landed while reading is read again at once.
            wait_or_retry_since(&panel, 10, at).await;
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = panel;
}

/// Wait `seconds`, or less if the account changes, a flow lands or the
/// reader presses "Try again".
#[cfg(target_arch = "wasm32")]
async fn wait_or_retry(panel: &Panel, seconds: u32) {
    wait_or_retry_since(panel, seconds, crate::wallet::get().changed.get()).await;
}

/// The same, counting flows landed since `changed` (the session's count
/// when the last read began).
#[cfg(target_arch = "wasm32")]
async fn wait_or_retry_since(panel: &Panel, seconds: u32, changed: u64) {
    let session = crate::wallet::get();
    let seen = (changed, session.account(), panel.retry.get());
    for _ in 0..seconds {
        gloo_timers::future::TimeoutFuture::new(1_000).await;
        if (session.changed.get(), session.account(), panel.retry.get()) != seen {
            break;
        }
    }
}

/// The vault couldn't be read: say so, and offer to read again now.
pub(crate) fn unreachable(retry: &Mutable<u32>) -> View {
    let retry = retry.clone();
    el("div")
        .class("sh-exit__body")
        .attr("role", "status")
        .child(notice(
            NoticeTone::Info,
            text("This vault can’t be reached from here right now. We’ll keep trying."),
        ))
        .child(on_click(
            Button::new(Kind::Secondary, "Try again").block().build(),
            move || {
                retry.replace_with(|n| n.wrapping_add(1));
            },
        ))
        .into()
}

pub(crate) fn placeholder() -> View {
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

/// Where the flow a panel shows stands, as its tabs see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// No flow: the form.
    Nothing,
    /// A flow still under way, on the tab it was started from.
    Live,
    /// A flow that has ended, done or failed: its card stays until left.
    Ended,
}

impl Shown {
    pub(crate) fn of(step: Option<Option<bool>>) -> Self {
        match step {
            None => Self::Nothing,
            Some(Some(true)) => Self::Ended,
            Some(_) => Self::Live,
        }
    }
}

/// What a click on the Deposit or Withdraw tab does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabClick {
    /// Open the tab's form (and leave a review for it).
    Open,
    /// Leave the ended flow and open the tab's empty form.
    Leave,
    /// Nothing: a flow is under way, or its card is already this tab's.
    Stay,
}

pub(crate) fn tab_click(shown: Shown, current: Tab, clicked: Tab) -> TabClick {
    match shown {
        Shown::Nothing => TabClick::Open,
        Shown::Ended if clicked != current => TabClick::Leave,
        Shown::Ended | Shown::Live => TabClick::Stay,
    }
}

/// Why `which` can't be opened now, if it can't: a flow is under way on
/// the `current` tab, and a flow is never abandoned half-way.
pub(crate) fn tab_blocked(shown: Shown, current: Tab, which: Tab) -> Option<&'static str> {
    match (shown, current) {
        (Shown::Live, current) if current == which => None,
        (Shown::Live, Tab::Deposit) => Some("Withdraw opens once this deposit has finished."),
        (Shown::Live, Tab::Withdraw) => Some("Deposit opens once this withdrawal has finished."),
        _ => None,
    }
}

/// The Deposit and Withdraw tabs and, while a flow is under way, the line
/// that says why the other tab is closed. `shown` follows the panel's flow;
/// `click` is handed the tab clicked.
pub(crate) fn tab_bar(
    tab: &Mutable<Tab>,
    shown: impl Signal<Item = Shown> + 'static,
    in_sheet: bool,
    click: impl Fn(Tab) + 'static,
) -> (El, El) {
    let why_id = if in_sheet {
        "sh-exit-tabs-why-sheet"
    } else {
        "sh-exit-tabs-why"
    };
    let state = Mutable::new(Shown::Nothing);
    let both = {
        let tab = tab.clone();
        let state = state.clone();
        move || {
            map_ref! {
                let shown = state.signal(),
                let current = tab.signal() => (*shown, *current)
            }
        }
    };
    let click = Rc::new(click);
    let button = |which: Tab, label: &'static str| {
        let click = click.clone();
        let blocked = both().map(move |(shown, current)| tab_blocked(shown, current, which));
        let described = both().map(move |(shown, current)| {
            tab_blocked(shown, current, which).map(|_| why_id.to_string())
        });
        el("button")
            .attr("type", "button")
            .class("sh-tab")
            .attr("role", "tab")
            .attr_from("aria-selected", tab, move |tab| {
                Some((*tab == which).to_string())
            })
            .attr_signal(
                "aria-disabled",
                None,
                blocked.map(|why| why.map(|_| "true".to_string())),
            )
            .attr_signal("aria-describedby", None, described)
            .text(label)
            .hook(move |dom| dom.event(move |_: dominator::events::Click| click(which)))
    };
    let tabs = el("div")
        .class("sh-tabs sh-exit__tabs")
        .attr("role", "tablist")
        .child(button(Tab::Deposit, "Deposit"))
        .child(button(Tab::Withdraw, "Withdraw"))
        .hook(move |dom| {
            dom.future(shown.for_each(move |shown| {
                state.set_neq(shown);
                async {}
            }))
        });
    let why = el("p")
        .class("sh-muted sh-exit__small sh-exit__why")
        .attr("id", why_id)
        .child_signal(
            None,
            both().map(|(shown, current)| {
                let other = match current {
                    Tab::Deposit => Tab::Withdraw,
                    Tab::Withdraw => Tab::Deposit,
                };
                tab_blocked(shown, current, other).map(text)
            }),
        );
    (tabs, why)
}

fn tabs(panel: &Rc<Panel>) -> (El, El) {
    let session = crate::wallet::get();
    let shown = session
        .flow_signal(&panel.slug, EXIT)
        .map(|running| running.map(|running| running.step))
        .switch(|step| match step {
            Some(step) => step
                .signal_ref(|step| Shown::of(Some(step.as_ref().map(Step::is_over))))
                .boxed_local(),
            None => futures_signals::signal::always(Shown::Nothing).boxed_local(),
        });
    let clicked = panel.clone();
    tab_bar(&panel.tab, shown, panel.in_sheet, move |which| {
        click_tab(&clicked, which)
    })
}

/// A click on a tab: from a flow that has ended, leave it for the tab's
/// empty form; while one is under way, stay with it.
fn click_tab(panel: &Panel, which: Tab) {
    let running = panel.running();
    let shown = Shown::of(
        running
            .as_ref()
            .map(|running| running.step.lock_ref().as_ref().map(Step::is_over)),
    );
    match tab_click(shown, panel.tab.get(), which) {
        TabClick::Open => {
            panel.tab.set_neq(which);
            panel.stage.set_neq(Stage::Form);
        }
        TabClick::Leave => {
            if let Some(running) = &running {
                crate::wallet::get().dismiss(running);
            }
            panel.clear_form();
            panel.tab.set_neq(which);
        }
        TabClick::Stay => {}
    }
}

fn render(panel: &Rc<Panel>, mode: Mode) -> View {
    let body = el("div").class("sh-exit__body");
    match mode {
        Mode::Loading => placeholder(),
        Mode::Unavailable => unreachable(&panel.retry),
        Mode::SignedOut(tab) => body
            .child(form(
                panel,
                tab,
                false,
                View::Many(vec![
                    el("p")
                        .class("sh-muted")
                        .text(match tab {
                            Tab::Deposit => "Connect a wallet to deposit.",
                            Tab::Withdraw => {
                                "Connect the wallet that holds your shares to withdraw."
                            }
                        })
                        .into(),
                    on_click(
                        Button::new(Kind::Primary, "Connect wallet").block().build(),
                        open_connect,
                    )
                    .attr("aria-haspopup", "dialog")
                    .into(),
                    el("p")
                        .class("sh-muted sh-exit__small")
                        .text("Connecting doesn’t move money or sign anything.")
                        .into(),
                ]),
            ))
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
        Mode::Form(tab) => body.child(form(panel, tab, true, View::Empty)).into(),
        Mode::Review => review(panel),
        Mode::Flow(_) => flow_view(panel),
    }
}

// The form --------------------------------------------------------------------

/// A field's verdict on what's typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Check {
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
pub(crate) fn row(
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

/// The form for `tab`: what's typed on the left (the lead), what it comes to
/// and what to do on the right (the trail) on a tablet, one column
/// elsewhere. `tail` ends the trail: the signed-out lines, say.
fn form(panel: &Rc<Panel>, tab: Tab, connected: bool, tail: View) -> View {
    match tab {
        Tab::Deposit => deposit_form(panel, connected, tail),
        Tab::Withdraw => withdraw_form(panel, connected, tail),
    }
}

/// The unit after a withdrawal amount.
fn unit_word(unit: Unit) -> &'static str {
    match unit {
        Unit::Shares => "shares",
        Unit::Dollars => "USDC",
    }
}

/// The "$" in front of a money amount; a share count has none.
fn dollar_sign() -> View {
    el("span")
        .class("sh-input__unit sh-exit__dollar")
        .attr("aria-hidden", "true")
        .text("$")
        .into()
}

/// The amount input: inputmode decimal, 16px, labelled, its hint or error
/// linked with aria-describedby. `dollar` says whether the amount is money
/// (a "$" in front) or a share count (no sign, only its unit after).
pub(crate) fn amount_input(
    id: &'static str,
    value: &Mutable<String>,
    unit: View,
    dollar: (bool, impl Signal<Item = bool> + 'static),
    invalid: impl Signal<Item = bool> + 'static,
) -> El {
    let typed = value.clone();
    let (dollar_now, dollar) = dollar;
    el("div")
        .class("sh-input sh-exit__input")
        .child_signal(
            dollar_now.then(dollar_sign),
            dollar.map(|money| money.then(dollar_sign)),
        )
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

/// Whether the deposit's review may open: the amount is ready, and the
/// vault's list has been read (`list_read`), so the review always shows the
/// list the statement is checked against before it is signed.
pub(crate) fn review_ready(check: &Check, list_read: bool) -> bool {
    *check == Check::Ready && list_read
}

fn deposit_form(panel: &Rc<Panel>, connected: bool, tail: View) -> View {
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
        let ready = {
            let panel = panel.clone();
            map_ref! {
                let (check, _) = check.signal_cloned(),
                let read = panel.restricted.signal_ref(Option::is_some) =>
                review_ready(check, *read)
            }
        };
        on_click(
            Button::new(Kind::Primary, "Continue").block().build(),
            move || {
                let (check, _) = deposit_check(&panel, &panel.deposit_text.get_cloned(), true);
                if review_ready(&check, panel.restricted.lock_ref().is_some()) {
                    panel.stage.set(Stage::Review);
                }
            },
        )
        .attr_signal(
            "disabled",
            Some(String::new()),
            ready.map(|ready| (!ready).then(String::new)),
        )
    };
    // Why Continue waits, while the vault's list hasn't been read.
    let list_wait = panel.restricted.signal_ref(|restricted| match restricted {
        None => "Reading who this vault is open to. You can review once it’s read.".to_string(),
        Some(_) => String::new(),
    });
    let fee_figure = quote.signal_cloned().map(move |quote| match quote {
        Some((quote, _)) => money::dollars(quote.fee, decimals, Rounding::Up),
        None => "—".to_string(),
    });
    let price_figure = quote.signal_cloned().map(move |quote| match quote {
        Some((_, price)) => money::price(price, decimals),
        None => "—".to_string(),
    });
    let shares_figure = quote.signal_cloned().map(|quote| match quote {
        Some((quote, _)) => format!("{} shares", money::shares(quote.shares, Rounding::Nearest)),
        None => "—".to_string(),
    });
    let fee_label = fee_label(panel, "Deposit fee");
    el("div")
        .class("sh-exit__form")
        .child(
            el("div")
                .class("sh-exit__lead")
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
                        .child(el("label").class("sh-field__label").attr("for", field_id("sh-deposit-amount", panel.in_sheet)).text("Amount"))
                        .child(el("span").class("sh-exit__balance").text_signal("", balance)),
                )
                .child(amount_input(
                    field_id("sh-deposit-amount", panel.in_sheet),
                    &panel.deposit_text,
                    text(symbol_unit),
                    (true, futures_signals::signal::always(true)),
                    check.signal_cloned().map(|(check, _)| {
                        matches!(check, Check::Invalid(_) | Check::Over { .. } | Check::Below(_))
                    }),
                ))
                // The chips sit between the field and its hint, as drawn.
                .child(quick)
                .child(hint(field_id("sh-deposit-amount-hint", panel.in_sheet), check.signal_cloned().map(|(check, _)| check), &panel.deposit_text, "The smallest deposit is $10.")),
        )
        )
        .child(
            el("div")
                .class("sh-exit__trail")
        .child(
            el("dl")
                .class("sh-exit__rows sh-exit__preview")
                .child(row_dyn(fee_label, fee_figure))
                .child(row("Share price now", None, price_figure))
                .child(row("You get about", None, shares_figure).class("sh-exit__row--total")),
        )
        .child(
            el("p")
                .class("sh-muted sh-exit__small sh-exit__note")
                .child(icon(Icon::Clock, 16))
                .text("Priced a few seconds after you ask, at the next valuation time. The share price can move a little before then."),
        )
        .child(if connected { continue_button.into() } else { View::Empty })
        .child(if connected {
            el("p")
                .class("sh-muted sh-exit__small")
                .attr("aria-live", "polite")
                .text_signal("", list_wait)
                .into()
        } else {
            View::Empty
        })
        .child(tail),
        )
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

pub(crate) fn row_dyn(
    label: impl Signal<Item = String> + 'static,
    figure: impl Signal<Item = String> + 'static,
) -> El {
    el("div")
        .class("sh-exit__row")
        .child(el("dt").text_signal("", label))
        .child(el("dd").class("sh-num").text_signal("", figure))
}

/// The line under a field: the hint, or the error with its one-tap fix.
/// A field's id: the phone sheet's copy of the panel sits in the same page
/// as the panel itself, so each takes its own id and its label and hint
/// stay tied to it.
pub(crate) fn field_id(base: &'static str, in_sheet: bool) -> &'static str {
    match (base, in_sheet) {
        ("sh-deposit-amount", true) => "sh-deposit-amount-sheet",
        ("sh-deposit-amount-hint", true) => "sh-deposit-amount-sheet-hint",
        ("sh-withdraw-amount", true) => "sh-withdraw-amount-sheet",
        ("sh-withdraw-amount-hint", true) => "sh-withdraw-amount-sheet-hint",
        (base, _) => base,
    }
}

pub(crate) fn hint(
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

pub(crate) fn held_sentence(
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
        .map(|value| format!(" ≈ {}", money::dollars(value, decimals, Rounding::Nearest)))
        .unwrap_or_default();
    format!(
        "You hold {} shares{value}",
        money::shares(held.shares, Rounding::Down)
    )
}

fn withdraw_form(panel: &Rc<Panel>, connected: bool, tail: View) -> View {
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
            money::dollars(quote.gross, decimals, Rounding::Nearest)
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
            money::dollars(quote.payout, decimals, Rounding::Nearest)
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
    el("div")
        .class("sh-exit__form")
        .child(
            el("div")
                .class("sh-exit__lead")
                .child(el("p").class("sh-exit__held").text_signal("", held_line))
                .child(
                    el("div")
                        .class("sh-field")
                        .class_signal(
                            "sh-field--error",
                            false,
                            check.signal_cloned().map(|(check, _)| {
                                matches!(check, Check::Invalid(_) | Check::Over { .. })
                            }),
                        )
                        .child(
                            el("div")
                                .class("sh-exit__fieldhead")
                                .child(
                                    el("label")
                                        .class("sh-field__label")
                                        .attr("for", field_id("sh-withdraw-amount", panel.in_sheet))
                                        .text("Amount"),
                                )
                                .child(segmented(
                                    "Amount in",
                                    &[(Unit::Shares, "Shares"), (Unit::Dollars, "USDC")],
                                    &panel.unit,
                                )),
                        )
                        .child(amount_input(
                            field_id("sh-withdraw-amount", panel.in_sheet),
                            &panel.withdraw_text,
                            el("span")
                                .text_from(&panel.unit, |unit| unit_word(*unit).to_string())
                                .into(),
                            (
                                panel.unit.get() == Unit::Dollars,
                                panel.unit.signal().map(|unit| unit == Unit::Dollars),
                            ),
                            check.signal_cloned().map(|(check, _)| {
                                matches!(check, Check::Invalid(_) | Check::Over { .. })
                            }),
                        ))
                        .child(hint(
                            field_id("sh-withdraw-amount-hint", panel.in_sheet),
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
                .child(el("div").child_signal(None, path_notice)),
        )
        .child(
            el("div")
                .class("sh-exit__trail")
                .child(if closed {
                    // A closed vault pays its fraction of the cash at once: no
                    // request, no exit price, no fee.
                    el("dl")
                        .class("sh-exit__rows sh-exit__preview")
                        .child(row("Your share of the cash", None, gross))
                        .child(row("You get", None, payout).class("sh-exit__row--total"))
                } else {
                    el("dl")
                        .class("sh-exit__rows sh-exit__preview")
                        .child(row(
                            "Valued at exit-side marks",
                            Some("what the positions would actually sell for"),
                            gross,
                        ))
                        .child(row_dyn(fee_label(panel, "Withdrawal fee"), fee))
                        .child(row("You get", None, payout).class("sh-exit__row--total"))
                })
                .child(
                    el("p")
                        .class("sh-muted sh-exit__small sh-exit__note")
                        .child(icon(Icon::Clock, 16))
                        .text(if closed {
                            "Paid at once from the vault’s cash, in one transaction."
                        } else {
                            "Priced a few seconds after you ask. A request can’t be cancelled."
                        }),
                )
                .child(if connected {
                    submit.into()
                } else {
                    View::Empty
                })
                .child(tail),
        )
        .into()
}

/// Shares as a button names them: 2,000 or 2,000.5.
pub(crate) fn whole_shares(shares: U256) -> String {
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
    // Never without the vault's list: the review shows what is signed.
    let (Some(amount), true) = (amount, panel.restricted.lock_ref().is_some()) else {
        panel.stage.set(Stage::Form);
        return placeholder();
    };
    // The list as this review last drew it: the deposit carries it, and is
    // stopped before signing if the vault's list is another by then.
    let shown: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
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
        let shown = shown.clone();
        on_click(
            Button::new(Kind::Primary, label).block().build(),
            move || {
                if first && !ticked.get() {
                    return;
                }
                let Some(shown) = shown.borrow().clone() else {
                    return;
                };
                start(&panel, Action::Deposit { amount, shown });
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
                        .map(|q| format!("≈ {} shares", money::shares(q.shares, Rounding::Nearest)))
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
        .child_from(&panel.restricted, move |restricted| {
            if restricted.is_some() {
                shown.replace(restricted.clone());
            }
            Some(statement_notice(Statement::Evm(restricted.as_deref())))
        })
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

// The statement ---------------------------------------------------------------------

/// The list a deposit's statement is made for, as its review shows it.
pub(crate) enum Statement<'a> {
    /// An EVM vault's list as it writes it (`SANCTIONED,US`), read as the
    /// statement reads it before it is signed; `None` until it is read.
    Evm(Option<&'a str>),
    /// A Solana vault's codes, read from its account: approving the
    /// deposit is the statement.
    Solana(&'a [String]),
}

/// Who a vault's list keeps out, in plain words: "people in the US,
/// sanctioned persons". `None` for an empty list.
pub(crate) fn not_open_to(codes: &[&str]) -> Option<String> {
    let places: Vec<String> = codes
        .iter()
        .filter(|code| **code != "SANCTIONED")
        .map(|code| place_name(code))
        .collect();
    let mut parts = Vec::new();
    if !places.is_empty() {
        parts.push(format!("people in {}", and_list(&places)));
    }
    if codes.contains(&"SANCTIONED") {
        parts.push("sanctioned persons".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// A list as the wallet's prompt names it, to compare with that prompt:
/// "Restricted: SANCTIONED,US", or "Restricted: (none)" for an empty one.
pub(crate) fn restricted_line(restricted: &str) -> String {
    if restricted.is_empty() {
        "Restricted: (none)".to_string()
    } else {
        format!("Restricted: {restricted}")
    }
}

/// The codes of a list as the vault writes it: `SANCTIONED,US`.
pub(crate) fn list_codes(restricted: &str) -> Vec<&str> {
    restricted
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .collect()
}

fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A country (ISO 3166) or subdivision (ISO 3166-2) code by its name where
/// it is a familiar one, else as written.
fn place_name(code: &str) -> String {
    const PLACES: [(&str, &str); 30] = [
        ("US", "the US"),
        ("GB", "the UK"),
        ("CA", "Canada"),
        ("CA-ON", "Ontario (Canada)"),
        ("CA-QC", "Quebec (Canada)"),
        ("AU", "Australia"),
        ("NZ", "New Zealand"),
        ("FR", "France"),
        ("DE", "Germany"),
        ("NL", "the Netherlands"),
        ("IT", "Italy"),
        ("ES", "Spain"),
        ("CH", "Switzerland"),
        ("JP", "Japan"),
        ("KR", "South Korea"),
        ("CN", "China"),
        ("HK", "Hong Kong"),
        ("SG", "Singapore"),
        ("IN", "India"),
        ("AE", "the UAE"),
        ("RU", "Russia"),
        ("BY", "Belarus"),
        ("UA", "Ukraine"),
        ("KP", "North Korea"),
        ("IR", "Iran"),
        ("CU", "Cuba"),
        ("SY", "Syria"),
        ("MM", "Myanmar"),
        ("VE", "Venezuela"),
        ("AF", "Afghanistan"),
    ];
    PLACES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// The review's statement: who the vault is not open to, in plain words,
/// and the words the depositor states, before the acknowledgement. Drawn
/// from the list the statement is made for, and nothing else.
pub(crate) fn statement_notice(statement: Statement<'_>) -> View {
    let codes: Option<Vec<&str>> = match &statement {
        Statement::Evm(restricted) => restricted.map(list_codes),
        Statement::Solana(codes) => Some(codes.iter().map(String::as_str).collect()),
    };
    let who = match codes.as_deref() {
        None => "This vault’s list couldn’t be read just now. Your wallet shows it when it asks you to sign.".to_string(),
        Some(codes) => match not_open_to(codes) {
            Some(words) => format!("Not open to: {words}."),
            None => "Open to everyone: this vault restricts no one.".to_string(),
        },
    };
    let how = match &statement {
        Statement::Evm(Some(restricted)) if restricted.is_empty() => format!(
            "Your wallet asks you to sign this, with the list as “{}” (an empty list, so the field is blank in your wallet), \
             the first time you deposit and again if the list changes. It’s a signature, not a transaction.",
            restricted_line(restricted)
        ),
        Statement::Evm(Some(restricted)) => format!(
            "Your wallet asks you to sign this, with the list as “{}”, \
             the first time you deposit and again if the list changes. It’s a signature, not a transaction.",
            restricted_line(restricted)
        ),
        Statement::Evm(None) => {
            "Your wallet asks you to sign this the first time you deposit. It’s a signature, not a transaction.".to_string()
        }
        Statement::Solana(codes) => format!(
            "On Solana the deposit itself is the statement: approving it in your wallet states this for the list read from the vault’s account ({}). There’s no separate signature.",
            if codes.is_empty() { "empty".to_string() } else { codes.join(",") }
        ),
    };
    notice(
        NoticeTone::Info,
        View::Many(vec![
            el("p")
                .class("sh-notice__lead")
                .text("Who can deposit")
                .into(),
            el("p").text(who).into(),
            el("p")
                .text(format!(
                    "By depositing you state: “{}”",
                    chain::statement::TEXT
                ))
                .into(),
            el("p").class("sh-exit__small").text(how).into(),
        ]),
    )
    .class("sh-exit__statement")
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
    // The flow carries its amount; the form behind it starts empty, so no
    // review is left standing for whoever comes back to it.
    panel.clear_form();
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

pub(crate) fn utc(seconds: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(seconds).unwrap_or(0))
        .map(|at| crate::format::time(time::PrimitiveDateTime::new(at.date(), at.time())))
        .unwrap_or_default()
}

/// What the wallet is asked, in the panel's words, each line a sentence
/// (a line may start with the wallet's name: "The development wallet
/// shows…").
fn prompt_words(
    prompt: &Prompt,
    wallet: &str,
    vault: &str,
    symbol: &str,
    decimals: u8,
) -> (String, String) {
    let (title, line) = prompt_lines(prompt, wallet, vault, symbol, decimals);
    (
        crate::format::sentence(&title),
        crate::format::sentence(&line),
    )
}

/// What the wallet is asked, in the panel's words.
fn prompt_lines(
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
            // is nothing to cancel, and declining or skipping it is fine.
            let second = *prompt == Prompt::Finish;
            el("div")
                .class("sh-exit__wait")
                .child(hex_ring())
                .child(el("p").class("sh-exit__title").text(title.clone()))
                .child(el("p").text(line))
                .child(el("p").class("sh-muted sh-exit__small").text(if second {
                    format!(
                        "Don’t see it? Open {wallet}, or skip it: your request stands either way."
                    )
                } else {
                    format!(
                        "Don’t see it? Open {wallet}. Once you approve, the request can’t be cancelled."
                    )
                }))
                .child(if second {
                    // Lets only this step go; the flow keeps watching the
                    // request, and "Settle now" asks for it again.
                    View::from(on_click(
                        Button::new(Kind::Secondary, "Skip").build(),
                        move || running.skip_second.nudge(),
                    ))
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
            let is_deposit = matches!(running.action, Action::Deposit { .. });
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
                let error = error.clone();
                move || {
                    if let TxError::ListChanged { now } = &error {
                        // Back to the review, with the vault's list as it
                        // is now; the person confirms again, nothing more.
                        review_again(&panel, &running, now);
                        return;
                    }
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
                .child(back_to(&panel_title(panel), panel.in_sheet, back))
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

/// After the vault's list changed between the review and signing: leave the
/// failed flow for the review of the same deposit, showing the list as it is
/// now (`now`). Nothing is started: the person reads it and confirms again.
fn review_again(panel: &Panel, running: &Running, now: &str) {
    crate::wallet::get().dismiss(running);
    panel.restricted.set(Some(now.to_string()));
    if let Action::Deposit { amount, .. } = &running.action {
        panel
            .deposit_text
            .set(money::plain_exact(*amount, decimals(panel)));
        panel.tab.set(Tab::Deposit);
        panel.ack.set(false);
        panel.stage.set(Stage::Review);
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
        Action::Deposit { amount, .. } => {
            ("You paid in", money::token_exact(*amount, decimals, symbol))
        }
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

/// What a withdrawal paid for, as one phrase: "19.9 shares, valued at the
/// exit-side marks of 05:47:30 UTC", or at a closed vault's cash.
pub(crate) fn valued_words(shares: U256, at: Option<u64>) -> String {
    let shares = money::shares(shares, Rounding::Down);
    match at {
        Some(at) => format!(
            "{shares} shares, valued at the exit-side marks of {}.",
            utc(at)
        ),
        None => format!("{shares} shares, valued at the closed vault’s cash."),
    }
}

/// The position a Done card shows: from holdings read once its flow had
/// landed (`read_at`, the session's count of landed flows when they were
/// read, against `landed`, its count once this flow landed). Before that,
/// "Updating…": never the figure from before the flow.
pub(crate) fn position_words(held: Option<&Holdings>, read_at: u64, landed: u64) -> String {
    match held {
        Some(held) if read_at >= landed => {
            format!("{} shares", money::shares(held.shares, Rounding::Down))
        }
        _ => "Updating…".to_string(),
    }
}

/// A Done card's position row, following the holdings as they are read.
pub(crate) fn position_row(
    label: &'static str,
    holdings: &Mutable<Option<Holdings>>,
    holdings_at: &Mutable<u64>,
    landed: u64,
) -> View {
    let now = position_words(holdings.lock_ref().as_ref(), holdings_at.get(), landed);
    let words = map_ref! {
        let held = holdings.signal_cloned(),
        let at = holdings_at.signal() => position_words(held.as_ref(), *at, landed)
    }
    .broadcast();
    el("dl")
        .class("sh-exit__rows")
        .child(
            el("div")
                .class("sh-exit__row")
                .child(el("dt").text(label))
                .child(
                    el("dd")
                        .class("sh-num")
                        .class_signal(
                            "sh-exit__updating",
                            now == "Updating…",
                            words.signal_ref(|words| words == "Updating…"),
                        )
                        .text_signal(now, words.signal_cloned()),
                ),
        )
        .into()
}

/// A withdrawal's and a deposit's Done card, before and after the position
/// is read again, drawn by the panel's own renderer from synthetic values,
/// for the dev-only states page.
pub fn done_states() -> Vec<(String, View)> {
    // As if a flow had just landed (nothing else here reads holdings).
    let session = crate::wallet::get();
    session.changed.replace_with(|n| *n + 1);
    let landed = session.changed.get();
    let withdraw = Outcome::Paid {
        shares: U256::from(19_950_000_000_000_000_000u128),
        assets: U256::from(19_998_497u64),
        at: Some(1_791_870_450),
        hash: alloy_primitives::B256::repeat_byte(0x11),
    };
    let deposit = Outcome::Deposited {
        assets: U256::from(50_000_000u64),
        shares: U256::from(49_950_000_000_000_000_000u128),
        at: 1_791_870_450,
        hash: alloy_primitives::B256::repeat_byte(0x12),
    };
    let held = Holdings {
        balance: U256::ZERO,
        shares: U256::from(49_950_000_000_000_000_000u128),
        units: U256::ZERO,
        units_claimable: U256::ZERO,
        held: U256::ZERO,
    };
    [
        (
            "Deposit done, position not read since",
            deposit.clone(),
            false,
        ),
        ("Deposit done, position read", deposit, true),
        ("Withdrawal done", withdraw, true),
    ]
    .into_iter()
    .map(|(label, outcome, read)| {
        let panel = Rc::new(Panel::new("base-0", Tab::Deposit, false));
        panel.holdings.set(Some(held));
        panel
            .holdings_at
            .set(if read { landed } else { landed - 1 });
        let running = Running::new(Action::Withdraw(U256::ZERO));
        (
            label.to_string(),
            done_view(&panel, &running, &outcome, 84_532, 6),
        )
    })
    .collect()
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
        .class("sh-btn sh-btn-primary sh-btn-block")
        .text("View in portfolio");
    let again = back_to(&panel_title(panel), panel.in_sheet, reset);
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
    // holdings rather than a figure taken before: until then, "Updating…".
    let landed = crate::wallet::get().changed.get();
    let position = |label: &'static str| -> View {
        position_row(label, &panel.holdings, &panel.holdings_at, landed)
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
        } => el("div")
            .class("sh-exit__done")
            .attr("role", "status")
            .child(seal())
            .child(el("p").class("sh-exit__title").text(format!(
                "{} is in your wallet",
                money::dollars(*assets, decimals, Rounding::Down)
            )))
            // One phrase, never split across a label and a figure.
            .child(
                el("p")
                    .class("sh-exit__valued")
                    .text(valued_words(*shares, *at)),
            )
            .child(tx_link(chain_id, *hash, "Transaction"))
            .child(position("You still hold"))
            .child(portfolio)
            .child(again)
            .into(),
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

/// The agent's name, as the vault directory or the store has it.
fn panel_title(panel: &Panel) -> String {
    panel
        .vault
        .lock_ref()
        .as_ref()
        .map(|vault| vault.title.clone())
        .or_else(|| {
            crate::store::get()
                .agent(&panel.slug)
                .map(|agent| agent.title.get_cloned())
        })
        .unwrap_or_default()
}

/// "Back to {agent}": the finished step's way back to the agent's page. In
/// the phone sheet it closes the sheet, which is the page underneath.
pub(crate) fn back_to(title: &str, in_sheet: bool, reset: impl Fn() + 'static) -> El {
    let label = if title.trim().is_empty() {
        "Back to the agent".to_string()
    } else {
        format!("Back to {title}")
    };
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-exit__edit")
            .text(label),
        move || {
            reset();
            if in_sheet {
                crate::atoms::dialog::close();
            }
        },
    )
}

/// The seal moment (M7): the check drawn in.
pub(crate) fn seal() -> El {
    el("span")
        .class("sh-seal sh-seal-in")
        .attr("aria-hidden", "true")
        .child(icon(Icon::Check, 28))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;

    use super::*;
    use crate::wallet::machine::WalletId;

    fn network() -> Network {
        Network {
            chain: shared::Chain::Base,
            chain_id: 8453,
            label: "Base".to_string(),
            rpc: String::new(),
            asset: Address::repeat_byte(0xdd),
            asset_symbol: "USDC".to_string(),
            asset_decimals: 6,
            dev_account: None,
        }
    }

    fn connected(account: Address) -> Connection {
        Connection::Connected {
            wallet: WalletId::Injected,
            account,
            chain_id: Some(8453),
        }
    }

    /// The amount field of a drawn withdraw form, from its opening tag to
    /// the end of its unit.
    fn withdraw_field(panel: &Rc<Panel>) -> String {
        let html = withdraw_form(panel, false, View::Empty).html();
        let start = html.find("sh-exit__input").expect("an amount field");
        let field = &html[start..];
        let end = field.find("</div>").expect("the field closes");
        field[..end].to_string()
    }

    #[test]
    fn a_withdrawal_in_shares_shows_no_dollar_sign_and_one_in_usdc_does() {
        let panel = Rc::new(Panel::new("base-1", Tab::Withdraw, false));
        panel.unit.set(Unit::Shares);
        let shares = withdraw_field(&panel);
        assert!(
            !shares.contains('$'),
            "a share count carries no $: {shares}"
        );
        assert!(shares.contains(">shares<"), "the unit is shares: {shares}");
        panel.unit.set(Unit::Dollars);
        let dollars = withdraw_field(&panel);
        assert!(
            dollars.contains('$'),
            "a dollar amount keeps its $: {dollars}"
        );
        assert!(dollars.contains(">USDC<"), "the unit is USDC: {dollars}");
    }

    #[test]
    fn another_account_sees_the_empty_form_never_the_previous_accounts_review() {
        let first = Address::repeat_byte(0xa0);
        let second = Address::repeat_byte(0x23);
        let panel = Panel::new("base-1", Tab::Deposit, false);
        let mut seen = None;
        assert!(!account_moved(&mut seen, Some(first)), "the first look");
        // The first account typed an amount and reached its review.
        panel.deposit_text.set("11".to_string());
        panel.withdraw_text.set("3".to_string());
        panel.ack.set(true);
        panel.stage.set(Stage::Review);
        assert!(!account_moved(&mut seen, Some(first)));
        // The wallet moves to another account.
        if account_moved(&mut seen, Some(second)) {
            panel.clear_form();
        }
        let mode = mode_of(
            &connected(second),
            Some(&network()),
            true,
            panel.tab.get(),
            panel.stage.get(),
            None,
        );
        assert_eq!(mode, Mode::Form(Tab::Deposit));
        assert!(panel.deposit_text.get_cloned().is_empty());
        assert!(panel.withdraw_text.get_cloned().is_empty());
        assert!(!panel.ack.get());
        // Disconnecting is a change too.
        assert!(account_moved(&mut seen, None));
    }

    #[test]
    fn the_review_names_who_the_vault_is_not_open_to_before_the_acknowledgement() {
        let panel = Rc::new(Panel::new("base-1", Tab::Deposit, false));
        panel.deposit_text.set("100".to_string());
        panel.restricted.set(Some("SANCTIONED,US".to_string()));
        panel.stage.set(Stage::Review);
        let html = review(&panel).html();
        let list = html
            .find("Not open to: people in the US, sanctioned persons.")
            .expect("the list in plain words");
        let words = html
            .find(chain::statement::TEXT)
            .expect("the statement's words");
        let signed = html
            .find("Restricted: SANCTIONED,US")
            .expect("the list as the wallet shows it");
        let ack = html.find("sh-exit-ack").expect("the acknowledgement");
        assert!(list < ack && words < ack && signed < ack, "{html}");
    }

    #[test]
    fn the_review_cant_start_with_no_list_read() {
        let panel = Rc::new(Panel::new("base-1", Tab::Deposit, false));
        panel.deposit_text.set("100".to_string());
        let (check, _) = deposit_check(&panel, "100", true);
        assert_eq!(check, Check::Ready, "the amount itself is fine");
        // The vault's list hasn't been read: Continue stays shut.
        assert!(!review_ready(&check, panel.restricted.lock_ref().is_some()));
        // And a review asked for anyway goes back to the form, showing no
        // deposit button.
        panel.stage.set(Stage::Review);
        let html = review(&panel).html();
        assert_eq!(panel.stage.get(), Stage::Form);
        assert!(!html.contains("Deposit $100"), "{html}");
        // Once the list is read, the review opens and shows it.
        panel.restricted.set(Some("SANCTIONED,US".to_string()));
        assert!(review_ready(&check, panel.restricted.lock_ref().is_some()));
        panel.stage.set(Stage::Review);
        let html = review(&panel).html();
        assert_eq!(panel.stage.get(), Stage::Review);
        assert!(html.contains("Restricted: SANCTIONED,US"), "{html}");
        assert!(html.contains("Deposit $100"), "{html}");
    }

    #[test]
    fn an_empty_list_is_shown_as_restricted_none_to_compare_with_the_wallet() {
        let html = statement_notice(Statement::Evm(Some(""))).html();
        assert!(html.contains("Restricted: (none)"), "{html}");
        assert!(html.contains("Open to everyone"), "{html}");
    }

    #[test]
    fn trying_again_after_the_list_changed_reopens_the_review_with_the_new_list_and_signs_nothing()
    {
        let panel = Rc::new(Panel::new("base-4", Tab::Deposit, false));
        panel.network.set(Some(Rc::new(network())));
        panel.restricted.set(Some("SANCTIONED".to_string()));
        let account = Address::repeat_byte(0x5c);
        let running = deposit_shown(
            &panel,
            account,
            Step::Failed(TxError::ListChanged {
                now: "SANCTIONED,US".to_string(),
            }),
        );
        review_again(&panel, &running, "SANCTIONED,US");
        // The failed flow is left and nothing new is started.
        assert!(panel.running().is_none());
        let mode = mode_of(
            &connected(account),
            Some(&network()),
            true,
            panel.tab.get(),
            panel.stage.get(),
            None,
        );
        assert_eq!(mode, Mode::Review);
        assert_eq!(panel.deposit_text.get_cloned(), "11");
        assert!(!panel.ack.get(), "acknowledged again on the new review");
        // The review shows the new list.
        let html = review(&panel).html();
        assert!(html.contains("Restricted: SANCTIONED,US"), "{html}");
        assert!(
            html.contains("Not open to: people in the US, sanctioned persons."),
            "{html}"
        );
        assert!(
            panel.running().is_none(),
            "drawing the review starts nothing"
        );
    }

    #[test]
    fn a_solana_review_names_the_list_read_from_the_vaults_account_in_plain_words() {
        let codes = ["SANCTIONED".to_string(), "US".to_string()];
        let html = statement_notice(Statement::Solana(&codes)).html();
        assert!(
            html.contains("Not open to: people in the US, sanctioned persons."),
            "{html}"
        );
        assert!(html.contains(chain::statement::TEXT), "{html}");
        assert!(html.contains("(SANCTIONED,US)"), "{html}");
        let open = statement_notice(Statement::Solana(&[])).html();
        assert!(open.contains("Open to everyone"), "{open}");
    }

    /// A deposit flow shown on the panel for `account`, at `step`.
    fn deposit_shown(panel: &Panel, account: Address, step: Step) -> Running {
        let session = crate::wallet::get();
        session.connection.set(connected(account));
        let running = Running::new(Action::Deposit {
            amount: U256::from(11_000_000u64),
            shown: "SANCTIONED,US".to_string(),
        });
        running.step.set(Some(step));
        assert!(session.begin(&panel.slug, &running));
        running
    }

    fn deposited() -> Step {
        Step::Done(Outcome::Deposited {
            assets: U256::from(11_000_000u64),
            shares: U256::from(10_000_000u64),
            at: 1_700_000_000,
            hash: alloy_primitives::B256::repeat_byte(0x11),
        })
    }

    #[test]
    fn the_other_tab_leaves_a_finished_or_failed_flow_for_its_empty_form() {
        for (slug, step) in [
            ("base-1", deposited()),
            ("base-2", Step::Failed(TxError::Rejected)),
        ] {
            let panel = Panel::new(slug, Tab::Deposit, false);
            panel.deposit_text.set("11".to_string());
            panel.ack.set(true);
            let account = Address::repeat_byte(0x5a);
            deposit_shown(&panel, account, step);
            click_tab(&panel, Tab::Withdraw);
            assert!(panel.running().is_none(), "{slug}: the ended flow is left");
            assert_eq!(panel.tab.get(), Tab::Withdraw);
            assert!(panel.deposit_text.get_cloned().is_empty());
            assert!(!panel.ack.get());
            let mode = mode_of(
                &connected(account),
                Some(&network()),
                true,
                panel.tab.get(),
                panel.stage.get(),
                None,
            );
            assert_eq!(mode, Mode::Form(Tab::Withdraw), "{slug}");
        }
    }

    #[test]
    fn a_flow_under_way_keeps_its_tab_and_says_why_the_other_is_shut() {
        let panel = Panel::new("base-3", Tab::Deposit, false);
        let account = Address::repeat_byte(0x5b);
        let running = deposit_shown(&panel, account, Step::Signing(Prompt::Statement));
        click_tab(&panel, Tab::Withdraw);
        assert_eq!(
            panel.running(),
            Some(running),
            "a live flow is never abandoned"
        );
        assert_eq!(panel.tab.get(), Tab::Deposit);
        assert_eq!(
            tab_blocked(Shown::Live, Tab::Deposit, Tab::Withdraw),
            Some("Withdraw opens once this deposit has finished.")
        );
        assert_eq!(tab_blocked(Shown::Live, Tab::Deposit, Tab::Deposit), None);
        assert_eq!(tab_blocked(Shown::Ended, Tab::Deposit, Tab::Withdraw), None);
        assert_eq!(
            tab_blocked(Shown::Nothing, Tab::Deposit, Tab::Withdraw),
            None
        );
        // A flow whose first step hasn't come yet is under way too.
        assert_eq!(Shown::of(Some(None)), Shown::Live);
        assert_eq!(
            tab_click(Shown::Ended, Tab::Deposit, Tab::Deposit),
            TabClick::Stay,
            "the finished card's own tab keeps it"
        );
    }
    #[test]
    fn the_done_card_never_shows_a_position_read_before_its_flow_landed() {
        let held = Holdings {
            balance: U256::ZERO,
            shares: U256::from(49_950_000_000_000_000_000u128),
            units: U256::ZERO,
            units_claimable: U256::ZERO,
            held: U256::ZERO,
        };
        // Read before the flow landed (the figure from before it): never
        // shown, "Updating…" instead.
        assert_eq!(position_words(Some(&held), 3, 4), "Updating…");
        assert_eq!(position_words(None, 4, 4), "Updating…");
        // Read once it landed: the position.
        assert_eq!(position_words(Some(&held), 4, 4), "49.9 shares");
        assert_eq!(position_words(Some(&held), 5, 4), "49.9 shares");
        // As the card draws it: the old holdings of 0.0 shares, read before.
        let session = crate::wallet::get();
        session.changed.replace_with(|n| *n + 1);
        let landed = session.changed.get();
        let panel = Rc::new(Panel::new("base-9", Tab::Deposit, false));
        panel.holdings.set(Some(Holdings {
            shares: U256::ZERO,
            ..held
        }));
        panel.holdings_at.set(landed - 1);
        let running = Running::new(Action::Deposit {
            amount: U256::from(50_000_000u64),
            shown: String::new(),
        });
        let outcome = Outcome::Deposited {
            assets: U256::from(50_000_000u64),
            shares: held.shares,
            at: 1_700_000_000,
            hash: alloy_primitives::B256::repeat_byte(0x11),
        };
        let html = done_view(&panel, &running, &outcome, 8453, 6).html();
        assert!(html.contains("Updating…"), "{html}");
        assert!(!html.contains("0.0 shares"), "{html}");
        // Read again after it landed.
        panel.holdings.set(Some(held));
        panel.holdings_at.set(landed);
        let html = done_view(&panel, &running, &outcome, 8453, 6).html();
        assert!(html.contains("Your position"), "{html}");
        assert!(!html.contains("Updating…"), "{html}");
    }

    #[test]
    fn a_withdrawals_done_card_says_what_was_paid_for_in_one_phrase() {
        let shares = U256::from(19_950_000_000_000_000_000u128);
        let at = 5 * 3600 + 47 * 60 + 30;
        assert_eq!(
            valued_words(shares, Some(at)),
            "19.9 shares, valued at the exit-side marks of 05:47:30 UTC."
        );
        assert_eq!(
            valued_words(shares, None),
            "19.9 shares, valued at the closed vault’s cash."
        );
    }

    #[test]
    fn a_wallet_prompt_line_starts_with_a_capital_even_when_it_names_the_wallet_first() {
        for prompt in [
            Prompt::Statement,
            Prompt::Approve {
                amount: U256::from(50_000_000u64),
            },
            Prompt::Deposit {
                amount: U256::from(50_000_000u64),
            },
            Prompt::Withdraw {
                shares: U256::from(10u64),
            },
        ] {
            let (title, line) = prompt_words(
                &prompt,
                "the development wallet",
                "Funding basis",
                "USDC",
                6,
            );
            for words in [&title, &line] {
                assert!(
                    words.chars().next().is_some_and(char::is_uppercase),
                    "{words}"
                );
            }
        }
        let (_, line) = prompt_words(
            &Prompt::Deposit {
                amount: U256::from(50_000_000u64),
            },
            "the development wallet",
            "Funding basis",
            "USDC",
            6,
        );
        assert!(
            line.starts_with(
                "The development wallet shows 50.00 USDC going to the Funding basis vault."
            ),
            "{line}"
        );
        // And an error's words.
        let words = TxError::BadSignature.words(&Context {
            wallet: "the development wallet",
            chain: "Base",
            wallet_chain: None,
            symbol: "USDC",
            decimals: 6,
            native: "ETH",
        });
        assert!(
            words.text.starts_with("The development wallet"),
            "{}",
            words.text
        );
    }
}
