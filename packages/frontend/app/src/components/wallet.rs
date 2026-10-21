//! The wallet in the page: the header's network chip and wallet button (or
//! "Connect wallet"), the connect dialog (a sheet on phone) with every state
//! (choosing, connecting, rejected, not installed, locked, the recovery
//! phrase), the wallet menu, and the wrong-network bar.

use std::collections::BTreeMap;
use std::rc::Rc;

use alloy_primitives::Address;
use chain::solana_ix::Pubkey;
use futures_signals::signal::{Mutable, SignalExt};
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::format::short_hash;
use crate::route::{DOCS, Route, TERMS};
use crate::view::{El, View, el, text};
use crate::wallet::machine::{Connection, WalletId};
use crate::wallet::recent::Family;
use crate::wallet::{Network, WalletOption, dev, networks};

/// A one-letter tile for a chain or a wallet without an icon.
pub fn tile(name: &str) -> El {
    el("span")
        .class("sh-tile")
        .attr("aria-hidden", "true")
        .text(
            name.chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string(),
        )
}

/// A small mark naming a wallet's chain family: "EVM", "SOL".
pub fn chain_mark(family: Family) -> El {
    el("span")
        .class("sh-chainmark")
        .attr("aria-hidden", "true")
        .text(match family {
            Family::Evm => "EVM",
            Family::Solana => "SOL",
        })
}

// The vault the page acts on -------------------------------------------------

thread_local! {
    /// The EVM chains that elements now on the page act on (a deposit or
    /// withdraw panel or sheet, a claim, the publish flow once its chain is
    /// chosen), newest last, by a token per element.
    static ACTING: Mutable<Vec<(u64, u64)>> = Mutable::new(Vec::new());
    static NEXT_TOKEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Drops an element's entry when the element goes.
struct Acting(u64);

impl Drop for Acting {
    fn drop(&mut self) {
        let token = self.0;
        ACTING.with(|acting| acting.lock_mut().retain(|(held, _)| *held != token));
    }
}

/// For as long as the returned future runs (an element that acts on a vault
/// holds it with `.future(...)`), the header's network chip compares the
/// wallet's chain with `chain_id`'s, and cautions when they differ.
pub async fn acting_on(chain_id: impl futures_signals::signal::Signal<Item = Option<u64>>) {
    let token = NEXT_TOKEN.with(|next| {
        let token = next.get();
        next.set(token + 1);
        token
    });
    let _held = Acting(token);
    chain_id
        .for_each(move |chain_id| {
            ACTING.with(|acting| {
                let mut acting = acting.lock_mut();
                acting.retain(|(held, _)| *held != token);
                if let Some(chain_id) = chain_id {
                    acting.push((token, chain_id));
                }
            });
            async {}
        })
        .await;
    // A chain that never changes still holds for as long as the element is
    // on the page.
    std::future::pending::<()>().await;
}

/// The chain the newest element on the page acts on.
fn acting_signal() -> impl futures_signals::signal::Signal<Item = Option<u64>> {
    ACTING.with(|acting| acting.signal_ref(|acting| acting.last().map(|(_, chain)| *chain)))
}

/// The agent a route is about, when the page acts on its vault: the agent
/// page, a claim, manage and activate.
fn route_slug(route: &Route) -> Option<String> {
    match route {
        Route::Agent { slug, .. }
        | Route::Claim { slug, .. }
        | Route::Manage { slug }
        | Route::Activate { slug } => (!slug.is_empty()).then(|| slug.clone()),
        _ => None,
    }
}

/// The EVM chain id of `slug`'s vault, once the store and the chain's
/// config have been read; `None` for a Solana vault.
fn vault_chain_id(slug: &str, networks: &BTreeMap<shared::Chain, Rc<Network>>) -> Option<u64> {
    let chain = crate::store::get()
        .agent(slug)?
        .strategy
        .lock_ref()
        .vault
        .chain;
    networks.get(&chain).map(|network| network.chain_id)
}

/// The chain the page acts on: the newest element's, else the route's.
fn want_signal(route: &Route) -> impl futures_signals::signal::Signal<Item = Option<u64>> + use<> {
    let slug = route_slug(route);
    let store = crate::store::get();
    let session = crate::wallet::get();
    futures_signals::map_ref! {
        let acting = acting_signal(),
        let _loaded = store.loaded.signal(),
        let networks = session.networks.signal_cloned() =>
        acting.or_else(|| slug.as_deref().and_then(|slug| vault_chain_id(slug, networks)))
    }
    .dedupe()
}

// The header ------------------------------------------------------------------

/// The header's wallet control: "Connect wallet" signed out ("Connect" on
/// phone). Connected, one account button whatever is connected (an EVM
/// wallet, a Solana wallet or both), each with its chain's mark, beside the
/// network chip of a browser EVM wallet. The button opens the wallet sheet,
/// which lists both kinds and offers to connect the one that isn't.
pub fn header_control(route: &Route) -> View {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    let want = Mutable::new(None::<u64>);
    let now = control(&session.connection.lock_ref(), solana.account(), None);
    el("div")
        .class("sh-walletctl")
        .child_signal(
            Some(now),
            futures_signals::map_ref! {
                let evm = session.connection.signal_cloned(),
                let solana = solana.connection.signal_cloned(),
                let _evm_name = session.wallet_name.signal_cloned(),
                let _solana_name = solana.wallet_name.signal_cloned(),
                let want = want.signal() => control(evm, solana.account(), *want)
            }
            .map(Some),
        )
        .hook({
            let route = route.clone();
            move |dom| {
                dom.future(want_signal(&route).for_each(move |chain| {
                    want.set_neq(chain);
                    async {}
                }))
            }
        })
        .into()
}

/// What the header holds for a pair of connections, where the page acts on
/// a vault on `want` (an EVM chain id), if it does.
fn control(evm: &Connection, solana: Option<Pubkey>, want: Option<u64>) -> View {
    let session = crate::wallet::get();
    let evm_account = evm.account();
    let chain_id = match evm {
        Connection::Connected { chain_id, .. } => *chain_id,
        _ => None,
    };
    match (evm_account, solana) {
        (None, None) => connect_button().into(),
        (Some(account), solana) => View::Many(vec![
            match chain_id.filter(|_| session.is_browser()) {
                Some(chain_id) => chip(chain_id, want, solana.is_some()),
                None => View::Empty,
            },
            account_button(
                Some((account, session.app_name())),
                solana.map(|account| (account, crate::wallet::solana::get().app_name())),
            )
            .into(),
        ]),
        (None, Some(solana)) => account_button(
            None,
            Some((solana, crate::wallet::solana::get().app_name())),
        )
        .into(),
    }
}

fn connect_button() -> El {
    let label = View::Many(vec![
        el("span")
            .class("sh-appheader__long")
            .text("Connect wallet")
            .into(),
        el("span")
            .class("sh-appheader__short")
            .text("Connect")
            .into(),
    ]);
    on_click(
        Button::new(Kind::Primary, label)
            .size(Size::Small)
            .build()
            .attr("aria-haspopup", "dialog"),
        open_connect,
    )
}

/// The chain a browser EVM wallet is on, by name, as a quiet button that
/// offers the switch. It cautions only where the page acts on a vault on
/// another chain (`want`); a network no vault is on is otherwise just named.
/// With a Solana wallet connected too, it says it is the EVM wallet's.
fn chip(chain_id: u64, want: Option<u64>, both: bool) -> View {
    let name = networks::network_name(chain_id);
    let caution = want.filter(|want| *want != chain_id);
    let label = match caution {
        Some(want) => format!(
            "Your EVM wallet is on {name}; this vault is on {}. Switch network",
            networks::chain_name(want)
        ),
        None => format!("Your EVM wallet is on {name}. Change network"),
    };
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-netchip")
            .class_if(caution.is_some(), "sh-netchip--other")
            .attr("aria-haspopup", "dialog")
            .attr("aria-label", label)
            // The triangle where it cautions; else the EVM mark beside a
            // Solana wallet, or the network's initial on its own.
            .child(match (caution, both) {
                (Some(_), _) => icon(Icon::Concern, 16).into(),
                (None, true) => View::from(chain_mark(Family::Evm)),
                (None, false) => View::from(tile(&name)),
            })
            .child(View::from(
                (caution.is_some() && both).then(|| chain_mark(Family::Evm).into()),
            ))
            .child(el("span").class("sh-netchip__name").text(name)),
        move || open_switch(chain_id, want),
    )
    .into()
}

/// The networks to offer, in order: the page's vault chain first, then the
/// app's other chains.
fn switch_targets(page: Option<u64>, app_chains: &[u64]) -> Vec<u64> {
    let mut targets: Vec<u64> = page
        .into_iter()
        .filter(|id| app_chains.contains(id))
        .collect();
    for id in app_chains {
        if !targets.contains(id) {
            targets.push(*id);
        }
    }
    targets
}

/// The switch, from the chip: the vault chain the page acts on first (the
/// one to switch to, where the wallet is elsewhere), then the app's others.
/// Closes once the wallet moves.
fn open_switch(on: u64, want: Option<u64>) {
    let session = crate::wallet::get();
    let app_chains = session.app_chains();
    let targets: Vec<u64> = switch_targets(want, &app_chains)
        .into_iter()
        .filter(|id| *id != on)
        .collect();
    let name = networks::network_name(on);
    let caution = want.filter(|want| *want != on);
    let lead = match caution {
        Some(want) => format!(
            "Your EVM wallet is on {name}. This vault is on {}.",
            networks::chain_name(want)
        ),
        None => format!("Your EVM wallet is on {name}."),
    };
    let watch = session
        .connection
        .signal_ref(|connection| match connection {
            Connection::Connected { chain_id, .. } => *chain_id,
            _ => None,
        })
        .dedupe()
        .for_each(move |chain_id| {
            if chain_id != Some(on) {
                dialog::close();
            }
            async {}
        });
    let buttons = targets.into_iter().enumerate().map(|(index, chain_id)| {
        let kind = if index == 0 && caution.is_some() {
            Kind::Primary
        } else {
            Kind::Secondary
        };
        switch_button_as(chain_id, Size::Medium, kind).class("sh-btn-block")
    });
    dialog::open(Dialog {
        title: if caution.is_some() {
            "Switch network".to_string()
        } else {
            "Network".to_string()
        },
        body: el("div")
            .class("sh-walletmenu")
            .child(el("p").text(lead))
            .children(buttons)
            .hook(move |dom| dom.future(watch))
            .into(),
        foot: None,
        wide: false,
    });
}

/// The one account button: each connected wallet's chain mark and short
/// address, the EVM one first ("EVM 0xb0b7…b448 · SOL B16P…r9Kp"), with
/// each wallet's app named beside it on a wide screen and always in its
/// label. Two wallets stack on a phone.
fn account_button(evm: Option<(Address, String)>, solana: Option<(Pubkey, String)>) -> El {
    let evm = evm.map(|(account, app)| (format!("{account:?}"), app));
    let solana = solana.map(|(account, app)| (account.to_string(), app));
    let label = account_label(
        evm.as_ref().map(|(a, n)| (a.as_str(), n.as_str())),
        solana.as_ref().map(|(a, n)| (a.as_str(), n.as_str())),
    );
    let one = |family: Family, app: &str, short: String| -> View {
        el("span")
            .class("sh-walletbtn__one")
            .child(chain_mark(family))
            .child(el("span").class("sh-walletbtn__app").text(app.to_string()))
            .child(el("span").class("sh-mono").text(short))
            .into()
    };
    let both = evm.is_some() && solana.is_some();
    let mut parts: Vec<View> = Vec::new();
    if let Some((account, app)) = &evm {
        parts.push(one(Family::Evm, app, short_hash(account)));
    }
    if both {
        parts.push(
            el("span")
                .class("sh-walletbtn__sep")
                .attr("aria-hidden", "true")
                .text("·")
                .into(),
        );
    }
    if let Some((account, app)) = &solana {
        parts.push(one(
            Family::Solana,
            app,
            super::solana::exit::short(account),
        ));
    }
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-btn sh-btn-secondary sh-btn-sm sh-walletbtn")
            .class_if(both, "sh-walletbtn--both")
            .attr("aria-label", label.clone())
            .attr("title", label)
            .attr("aria-haspopup", "dialog")
            .children(parts),
        wallet_sheet,
    )
}

/// The account button's label: every connected wallet, by chain family,
/// app and full address.
fn account_label(evm: Option<(&str, &str)>, solana: Option<(&str, &str)>) -> String {
    match (evm, solana) {
        (Some((evm, evm_app)), Some((solana, solana_app))) => {
            format!("Your wallets: EVM, {evm_app}, {evm}; Solana, {solana_app}, {solana}")
        }
        (Some((evm, app)), None) => format!("Your EVM wallet: {app}, {evm}"),
        (None, Some((solana, app))) => format!("Your Solana wallet: {app}, {solana}"),
        (None, None) => "Your wallets".to_string(),
    }
}

/// The wallet sheet: each kind of wallet, its full address and Disconnect,
/// or, for a kind that isn't connected, a button to connect one; then
/// Portfolio and Your agents. It follows the connections while open.
fn wallet_sheet() {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    // Drawn now as well, so the dialog's first focus lands in the first
    // section rather than past it.
    let now = sheet_body(&session.connection.lock_ref(), solana.account());
    let body = el("div")
        .class("sh-walletmenu")
        .class("sh-walletsheet")
        .child_signal(
            Some(now),
            futures_signals::map_ref! {
                let evm = session.connection.signal_cloned(),
                let solana = solana.connection.signal_cloned(),
                let _cluster = solana.network.signal_ref(Option::is_some) => (evm.clone(), solana.account())
            }
            .map(|(evm, solana)| {
                // Nothing left connected: the sheet has nothing to list.
                if !matches!(evm, Connection::Connected { .. }) && solana.is_none() {
                    dialog::close();
                }
                Some(sheet_body(&evm, solana))
            }),
        )
        .child(
            el("div")
                .class("sh-walletsheet__links")
                .child(
                    crate::web::app_link("/portfolio")
                        .class("sh-link")
                        .text("View your portfolio")
                        .hook(|dom| dom.event(|_: dominator::events::Click| dialog::close())),
                )
                .child(
                    crate::web::app_link("/you")
                        .class("sh-link")
                        .text("Your agents")
                        .hook(|dom| dom.event(|_: dominator::events::Click| dialog::close())),
                ),
        );
    dialog::open(Dialog {
        title: "Your wallets".to_string(),
        body: body.into(),
        foot: None,
        wide: false,
    });
}

/// A wallet section's heading: its chain family, its app and where it is.
/// "EVM: MetaMask on Base Sepolia", "Solana: Phantom on devnet".
pub fn sheet_heading(family: Family, app: Option<&str>, on: Option<&str>) -> String {
    let family = match family {
        Family::Evm => "EVM",
        Family::Solana => "Solana",
    };
    match (app, on) {
        (Some(app), Some(on)) => format!("{family}: {app} on {on}"),
        (Some(app), None) => format!("{family}: {app}"),
        (None, _) => format!("{family} wallet"),
    }
}

/// The browser wallet to offer instead of the one connected for EVM: the
/// one this browser used before it, if it is installed; or, where the EVM
/// side of the connected Solana wallet's app took the slot, the first other
/// EVM wallet installed.
pub fn instead(
    current: &WalletId,
    current_app: &str,
    solana_app: Option<&str>,
    previous: Option<&WalletId>,
    wallets: &[WalletOption],
) -> Option<WalletOption> {
    let installed = |id: &WalletId| {
        wallets
            .iter()
            .find(|option| &option.id == id && option.detected)
            .cloned()
    };
    if let Some(previous) = previous.filter(|previous| *previous != current)
        && let Some(option) = installed(previous)
    {
        return Some(option);
    }
    if current.is_browser() && solana_app == Some(current_app) {
        return wallets
            .iter()
            .find(|option| option.detected && &option.id != current && option.name != current_app)
            .cloned();
    }
    None
}

/// The sheet's two sections, as the connections are now.
fn sheet_body(evm: &Connection, solana: Option<Pubkey>) -> View {
    let session = crate::wallet::get();
    let solana_session = crate::wallet::solana::get();
    let section = |title: String, body: View| -> View {
        el("section")
            .class("sh-walletsheet__kind")
            .child(el("h3").class("sh-walletsheet__title").text(title))
            .child(body)
            .into()
    };
    let disconnect = |what: &'static str, act: fn()| -> View {
        on_click(
            Button::new(Kind::Secondary, "Disconnect")
                .size(Size::Small)
                .build()
                .attr("aria-label", format!("Disconnect the {what} wallet")),
            act,
        )
        .into()
    };
    let add = |words: &'static str| -> View {
        on_click(
            Button::new(Kind::Secondary, words)
                .size(Size::Small)
                .build(),
            || {
                dialog::close();
                open_connect();
            },
        )
        .into()
    };
    let solana_app = solana.map(|_| solana_session.app_name());
    let evm_part = match evm {
        Connection::Connected {
            account,
            chain_id,
            wallet,
        } => {
            let app = session.app_name();
            let on = chain_id
                .filter(|_| session.is_browser())
                .map(networks::network_name);
            let other = instead(
                wallet,
                &app,
                solana_app.as_deref(),
                crate::wallet::session::previous_wallet().as_ref(),
                &session.wallets.lock_ref(),
            );
            section(
                sheet_heading(Family::Evm, Some(&app), on.as_deref()),
                View::Many(vec![
                    crate::atoms::hash::hash(&format!("{account:?}"), None, "address").into(),
                    el("div")
                        .class("sh-walletsheet__actions")
                        .child(disconnect("EVM", || crate::wallet::get().disconnect()))
                        .child(View::from(other.map(|other| {
                            on_click(
                                Button::new(Kind::Secondary, format!("Use {} instead", other.name))
                                    .size(Size::Small)
                                    .build(),
                                || {
                                    crate::wallet::get().disconnect();
                                    dialog::close();
                                    open_connect();
                                },
                            )
                            .into()
                        })))
                        .into(),
                ]),
            )
        }
        _ => section(
            sheet_heading(Family::Evm, None, None),
            View::Many(vec![
                el("p")
                    .class("sh-muted")
                    .text("For agents whose vault is on an EVM chain.")
                    .into(),
                add("Connect an EVM wallet"),
            ]),
        ),
    };
    let solana_part = match (solana, solana_app) {
        (Some(account), Some(app)) => {
            let cluster = solana_session
                .network
                .get_cloned()
                .map(|network| network.cluster.clone());
            section(
                sheet_heading(Family::Solana, Some(&app), cluster.as_deref()),
                View::Many(vec![
                    crate::atoms::hash::hash(&account.to_string(), None, "address").into(),
                    disconnect("Solana", || crate::wallet::solana::get().disconnect()),
                ]),
            )
        }
        _ => section(
            sheet_heading(Family::Solana, None, None),
            View::Many(vec![
                el("p")
                    .class("sh-muted")
                    .text("For agents whose vault is on Solana.")
                    .into(),
                add("Connect a Solana wallet"),
            ]),
        ),
    };
    View::Many(vec![evm_part, solana_part])
}

/// Open the connect dialog (a sheet on phone).
pub fn open_connect() {
    let session = crate::wallet::get();
    session.open();
    // Wallets that load after the page announce themselves now.
    #[cfg(target_arch = "wasm32")]
    session.start_discovery();
    super::solana::wallet::discover();
    let phrase_mode = Mutable::new(false);
    let body = el("div")
        .class("sh-connect")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let connection = session.connection.signal_cloned(),
                let _wallets = session.wallets.signal_cloned(),
                let phrase = phrase_mode.signal() => (connection.clone(), *phrase)
            }
            .map({
                let phrase_mode = phrase_mode.clone();
                move |(connection, phrase)| Some(state_view(&connection, phrase, &phrase_mode))
            }),
        )
        .hook(|dom| {
            dom.after_removed(|_| {
                // Closed by Esc, the scrim or the close button.
                let session = crate::wallet::get();
                if !matches!(*session.connection.lock_ref(), Connection::Connected { .. }) {
                    session.close();
                }
            })
            .future({
                // Close once an EVM wallet connects; one connected before the
                // dialog opened (adding a Solana wallet) doesn't close it.
                let was = std::cell::Cell::new(matches!(
                    *crate::wallet::get().connection.lock_ref(),
                    Connection::Connected { .. }
                ));
                crate::wallet::get()
                    .connection
                    .signal_cloned()
                    .for_each(move |connection| {
                        let now = matches!(connection, Connection::Connected { .. });
                        if now && !was.get() {
                            dialog::close();
                        }
                        was.set(now);
                        async {}
                    })
            })
        });
    dialog::open(Dialog {
        title: "Connect a wallet".to_string(),
        body: body.into(),
        foot: None,
        wide: false,
    });
}

fn state_view(connection: &Connection, phrase: bool, phrase_mode: &Mutable<bool>) -> View {
    let session = crate::wallet::get();
    if phrase {
        return phrase_entry(phrase_mode);
    }
    let wallet_name = |id: &WalletId| -> String {
        match id {
            WalletId::Phrase => crate::wallet::PHRASE_WALLET.to_string(),
            WalletId::Dev => crate::wallet::DEV_WALLET.to_string(),
            other => session
                .wallets
                .lock_ref()
                .iter()
                .find(|option| &option.id == other)
                .map(|option| option.name.clone())
                .unwrap_or_else(|| "your wallet".to_string()),
        }
    };
    match connection {
        Connection::Connecting { wallet } => {
            let name = wallet_name(wallet);
            el("div")
                .class("sh-connect__state")
                .attr("role", "status")
                .child(hex_ring())
                .child(
                    el("p")
                        .class("sh-connect__title")
                        .text(format!("Approve in {name}")),
                )
                .child(el("p").text(format!(
                    "Request sent to {name}. Waiting for your approval."
                )))
                .child(el("p").class("sh-muted").text(format!(
                    "Don’t see it? Open the {name} extension. Connecting never moves money."
                )))
                .child(on_click(
                    Button::new(Kind::Secondary, "Cancel").build(),
                    || crate::wallet::get().cancel_connecting(),
                ))
                .into()
        }
        Connection::Rejected { wallet } => problem(
            "Rejected in wallet. Nothing was shared.",
            format!(
                "You declined in {}. Connecting never moves money.",
                wallet_name(wallet)
            ),
            None,
        ),
        Connection::NotInstalled { wallet } => {
            let name = wallet_name(wallet);
            let install = session
                .wallets
                .lock_ref()
                .iter()
                .find(|option| &option.id == wallet)
                .and_then(|option| option.install);
            problem(
                &format!("{name} isn’t installed in this browser"),
                "Install it, then come back to connect. Or choose another wallet.".to_string(),
                install.map(|href| {
                    el("a")
                        .class("sh-btn sh-btn-primary")
                        .attr("href", href)
                        .attr("target", "_blank")
                        .attr("rel", "noopener noreferrer")
                        .text(format!("Get {name}"))
                        .child(icon(Icon::External, 16))
                        .into()
                }),
            )
        }
        Connection::Locked { wallet } => {
            let name = wallet_name(wallet);
            problem(
                &format!("{name} is locked"),
                format!("Unlock {name}, or answer the request waiting in it, then try again."),
                None,
            )
        }
        _ => choosing(phrase_mode),
    }
}

fn problem(title: &str, line: String, action: Option<View>) -> View {
    el("div")
        .class("sh-connect__state")
        .attr("role", "status")
        .child(el("p").class("sh-connect__title").text(title.to_string()))
        .child(el("p").text(line))
        .child(
            el("div")
                .class("sh-connect__actions")
                .child(action.unwrap_or(View::Empty))
                .child(on_click(
                    Button::new(Kind::Secondary, "Choose a wallet").build(),
                    || crate::wallet::get().open(),
                )),
        )
        .into()
}

fn choosing(phrase_mode: &Mutable<bool>) -> View {
    let session = crate::wallet::get();
    // The list offers only what isn't connected yet: with an EVM wallet
    // connected, the Solana wallets; with a Solana one, the EVM wallets.
    let evm_connected = matches!(*session.connection.lock_ref(), Connection::Connected { .. });
    let solana_connected = crate::wallet::solana::get().account().is_some();
    let options = if evm_connected {
        Vec::new()
    } else {
        session.wallets.get_cloned()
    };
    let chains = evm_chains(&session.networks.lock_ref());
    let mut rows: Vec<View> = options
        .into_iter()
        .map(|option| wallet_row(option, &chains))
        .collect();
    if !solana_connected {
        rows.extend(super::solana::wallet::rows());
    }
    if session.dev_available() && !evm_connected {
        rows.push(
            option_row(
                tile("D").into(),
                "Development wallet",
                Some("An account this development chain holds the key for"),
                false,
            )
            .hook(|dom| {
                dom.event(|_: dominator::events::Click| {
                    crate::wallet::get().connect_dev();
                    super::solana::wallet::also_connect_dev();
                })
            })
            .into(),
        );
    }
    if dev::PHRASE_WALLET && !evm_connected {
        let phrase_mode = phrase_mode.clone();
        rows.push(
            option_row(
                icon(Icon::Lock, 20).class("sh-tile").into(),
                "Use a recovery phrase",
                Some("For test wallets only. Kept in this page’s memory"),
                false,
            )
            .hook(move |dom| dom.event(move |_: dominator::events::Click| phrase_mode.set(true)))
            .into(),
        );
    }
    el("div")
        .child(el("p").class("sh-muted").text(
            "Shonin never holds your money or your keys. Connecting lets you see your shares and send deposits and withdrawals yourself.",
        ))
        .child(
            el("ul")
                .class("sh-connect__list")
                .attr("aria-label", "Wallets")
                .children(rows.into_iter().map(|row| el("li").child(row))),
        )
        .child(
            el("div")
                .class("sh-connect__foot")
                .child(
                    el("p")
                        .text("New to wallets? ")
                        .child(
                            el("a")
                                .attr("href", DOCS)
                                .attr("target", "_blank")
                                .attr("rel", "noopener")
                                .text("Read how connecting works"),
                        ),
                )
                .child(
                    el("p")
                        .class("sh-connect__terms")
                        .text("By connecting you agree to the ")
                        .child(
                            el("a")
                                .attr("href", TERMS)
                                .attr("target", "_blank")
                                .attr("rel", "noopener")
                                .text("Terms of use"),
                        )
                        .text("."),
                ),
        )
        .into()
}

fn option_row(mark: View, name: &str, detail: Option<&str>, detected: bool) -> El {
    let label = match detail {
        Some(detail) => el("span")
            .class("sh-walletopt__text")
            .child(
                el("span")
                    .class("sh-walletopt__name")
                    .text(name.to_string()),
            )
            .child(
                el("span")
                    .class("sh-walletopt__detail")
                    .text(detail.to_string()),
            ),
        None => el("span").class("sh-walletopt__text").child(
            el("span")
                .class("sh-walletopt__name")
                .text(name.to_string()),
        ),
    };
    el("button")
        .attr("type", "button")
        .class("sh-walletopt")
        .child(mark)
        .child(label)
        .child(if detected {
            el("span")
                .class("sh-chip sh-chip-neutral")
                .text("Detected")
                .into()
        } else {
            View::Empty
        })
        .child(icon(Icon::ChevronRight, 18))
}

/// What an EVM wallet's row says it connects to: the vault chains the app
/// reads, by name, so a wallet that is on the list twice (Phantom announces
/// an EVM side and a Solana side) reads as two different things.
fn evm_chains(
    networks: &std::collections::BTreeMap<shared::Chain, Rc<crate::wallet::Network>>,
) -> String {
    let labels: Vec<&str> = networks
        .values()
        .map(|network| network.label.as_str())
        .collect();
    if labels.is_empty() {
        "EVM chains".to_string()
    } else {
        format!("EVM: {}", labels.join(", "))
    }
}

fn wallet_row(option: WalletOption, chains: &str) -> View {
    let mark: View = match &option.icon {
        Some(src) => el("img")
            .class("sh-tile sh-tile--img")
            .attr("src", src.clone())
            .attr("alt", "")
            .attr("width", "36")
            .attr("height", "36")
            .into(),
        None => tile(&option.name).into(),
    };
    let id = option.id.clone();
    option_row(mark, &option.name, Some(chains), option.detected)
        .hook(move |dom| {
            dom.event(move |_: dominator::events::Click| {
                let id = id.clone();
                let session = crate::wallet::get();
                #[cfg(target_arch = "wasm32")]
                wasm_bindgen_futures::spawn_local(async move { session.connect(id).await });
                #[cfg(not(target_arch = "wasm32"))]
                let _ = (session, id);
            })
        })
        .into()
}

fn phrase_entry(phrase_mode: &Mutable<bool>) -> View {
    let phrase = Rc::new(std::cell::RefCell::new(String::new()));
    let error = Mutable::new(None::<String>);
    let connect = {
        let phrase = phrase.clone();
        let error = error.clone();
        move || {
            let words = std::mem::take(&mut *phrase.borrow_mut());
            match crate::wallet::get().connect_phrase(&words) {
                Ok(()) => {
                    super::solana::wallet::also_connect_phrase(&words);
                    error.set(None)
                }
                Err(bad) => error.set(Some(bad.words().to_string())),
            }
        }
    };
    let back = phrase_mode.clone();
    el("div")
        .class("sh-connect__phrase")
        .child(notice(
            NoticeTone::Caution,
            text("Anyone with this phrase controls its wallet. Use a test wallet only. The phrase stays in this page’s memory and is forgotten when you reload; it is never stored or sent."),
        ))
        .child(
            el("div")
                .class("sh-field")
                .class_from("sh-field--error", &error, Option::is_some)
                .child(
                    el("label")
                        .class("sh-field__label")
                        .attr("for", "sh-phrase")
                        .text("Recovery phrase"),
                )
                .child(
                    el("div").class("sh-input").child(
                        // A text field shown as dots, not a password field:
                        // password managers offer to save a password field
                        // whatever it asks, and the phrase lives only in
                        // memory.
                        el("input")
                            .attr("id", "sh-phrase")
                            .class("sh-input__secret")
                            .attr("type", "text")
                            .attr("autocomplete", "off")
                            .attr("autocorrect", "off")
                            .attr("data-1p-ignore", "true")
                            .attr("data-lpignore", "true")
                            .attr("data-bwignore", "true")
                            .attr("data-form-type", "other")
                            .attr("autocapitalize", "off")
                            .attr("spellcheck", "false")
                            .attr("aria-describedby", "sh-phrase-hint")
                            .attr_from("aria-invalid", &error, |e| e.as_ref().map(|_| "true".to_string()))
                            .hook({
                                let phrase = phrase.clone();
                                move |dom| {
                                    dominator::with_node!(dom, input => {
                                        .event(move |_: dominator::events::Input| {
                                            if let Some(input) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input) {
                                                *phrase.borrow_mut() = input.value();
                                            }
                                        })
                                    })
                                }
                            }),
                    ),
                )
                .child(
                    el("p")
                        .class("sh-field__hint")
                        .attr("id", "sh-phrase-hint")
                        .text_from(&error, |error| {
                            error.clone().unwrap_or_else(|| {
                                "12 or 24 words, separated by spaces.".to_string()
                            })
                        }),
                ),
        )
        .child(
            el("div")
                .class("sh-connect__actions")
                .child(on_click(Button::new(Kind::Secondary, "Back").build(), move || {
                    back.set(false)
                }))
                .child(on_click(Button::new(Kind::Primary, "Connect").build(), connect)),
        )
        .into()
}

/// The hexagon traced while a wallet is asked (M10).
pub fn hex_ring() -> El {
    el("span")
        .class("sh-hexring")
        .attr("aria-hidden", "true")
        .child(
            crate::atoms::button::hex()
                .attr("width", "48")
                .attr("height", "48"),
        )
}

/// The wrong-network bar: under the header, only where something is signed.
/// `vault_chain` is the chain the action needs; nothing shows while the
/// wallet is on it, or isn't a browser wallet.
pub fn wrong_network_bar(vault_chain_id: Mutable<Option<u64>>, what: String) -> View {
    let session = crate::wallet::get();
    el("div")
        .class("sh-wrongnet-slot")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let connection = session.connection.signal_cloned(),
                let want = vault_chain_id.signal() => (connection.clone(), *want)
            }
            .map(move |(connection, want)| {
                let Connection::Connected {
                    chain_id: Some(on), ..
                } = connection
                else {
                    return None;
                };
                let want = want?;
                if on == want || !crate::wallet::get().is_browser() {
                    return None;
                }
                Some(wrong_network(on, want, &what).into())
            }),
        )
        .into()
}

/// The bar itself: the wallet is on `on`, and `what` needs `want`.
pub(crate) fn wrong_network(on: u64, want: u64, what: &str) -> El {
    el("div").class("sh-wrongnet").attr("role", "status").child(
        el("div")
            .class("sh-container sh-wrongnet__inner")
            .child(icon(Icon::Concern, 18))
            .child(el("p").text(format!(
                "Your wallet is on {}. {what} on {}.",
                networks::chain_name(on),
                networks::chain_name(want)
            )))
            .child(switch_button(want, Size::Small)),
    )
}

/// "Switch to Hyperliquid": asks the wallet to switch, adding the chain if it
/// doesn't know it.
pub fn switch_button(chain_id: u64, size: Size) -> El {
    switch_button_as(chain_id, size, Kind::Primary)
}

fn switch_button_as(chain_id: u64, size: Size, kind: Kind) -> El {
    let busy = Mutable::new(false);
    let label = format!("Switch to {}", networks::chain_name(chain_id));
    on_click(
        Button::new(kind, label).size(size).busy(&busy).build(),
        move || {
            if busy.get() {
                return;
            }
            busy.set(true);
            let busy = busy.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                let session = crate::wallet::get();
                let network = session
                    .networks
                    .lock_ref()
                    .values()
                    .find(|network| network.chain_id == chain_id)
                    .cloned();
                let result = match network {
                    Some(network) => session.switch_to(&network).await,
                    None => Err(crate::tx::TxError::Network(
                        "this chain isn't one the app reads".to_string(),
                    )),
                };
                if let Err(error) = result {
                    let sender = session.sender();
                    let context = crate::tx::error::Context {
                        wallet: &sender.name,
                        keeps_activity: sender.keeps_activity,
                        chain: &networks::chain_name(chain_id),
                        wallet_chain: None,
                        symbol: "",
                        decimals: 6,
                        native: "",
                    };
                    let words = error.words(&context);
                    crate::atoms::toast::push(
                        if words.alert {
                            crate::atoms::toast::ToastKind::Error
                        } else {
                            crate::atoms::toast::ToastKind::Neutral
                        },
                        words.title,
                        words.text,
                    );
                }
                busy.set(false);
            });
            #[cfg(not(target_arch = "wasm32"))]
            busy.set(false);
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::solana::{SolConnection, SolWalletId};

    /// The header's control with the given wallets connected, as HTML.
    fn header_with(evm: bool, solana: bool) -> String {
        let session = crate::wallet::get();
        session.connection.set(if evm {
            Connection::Connected {
                wallet: WalletId::Dev,
                account: alloy_primitives::Address::repeat_byte(0x7e),
                chain_id: Some(84532),
            }
        } else {
            Connection::SignedOut
        });
        crate::wallet::solana::get().connection.set(if solana {
            SolConnection::Connected {
                wallet: SolWalletId::Dev,
                account: chain::solana_ix::Pubkey::new_from_array([7; 32]),
            }
        } else {
            SolConnection::SignedOut
        });
        header_control(&Route::Portfolio).html()
    }

    #[test]
    fn the_header_holds_one_account_button_whichever_wallets_are_connected() {
        for (evm, solana) in [(true, false), (false, true), (true, true)] {
            let html = header_with(evm, solana);
            assert_eq!(
                html.matches("sh-walletbtn\"").count() + html.matches("sh-walletbtn ").count(),
                1,
                "evm {evm}, solana {solana}: {html}"
            );
            assert!(
                !html.contains("Connect"),
                "connected, the header never offers Connect beside an address: {html}"
            );
        }
        let solana = header_with(false, true);
        assert!(
            solana.contains("aria-label=\"Your Solana wallet: Development wallet, "),
            "{solana}"
        );
        assert!(solana.contains(">SOL<"), "{solana}");
        let none = header_with(false, false);
        assert!(none.contains("Connect wallet"), "{none}");
        assert!(!none.contains("sh-walletbtn"), "{none}");
    }

    #[test]
    fn the_account_button_names_both_wallets_by_chain_app_and_address() {
        let both = header_with(true, true);
        let solana = chain::solana_ix::Pubkey::new_from_array([7; 32]).to_string();
        // Both short addresses, each after its chain's mark, the EVM one
        // first.
        let evm_at = both.find(">EVM<").expect("the EVM mark");
        let evm_short = both.find(">0x7e7e…7e7e<").expect("the EVM address");
        let sol_at = both.find(">SOL<").expect("the Solana mark");
        let sol_short = both
            .find(&format!(
                ">{}<",
                crate::components::solana::exit::short(&solana)
            ))
            .expect("the Solana address");
        assert!(
            evm_at < evm_short && evm_short < sol_at && sol_at < sol_short,
            "{both}"
        );
        assert!(both.contains("sh-walletbtn--both"), "{both}");
        // Its label names each wallet's family, app and full address.
        assert_eq!(
            account_label(Some(("0xb0b7", "MetaMask")), Some(("B16P", "Phantom"))),
            "Your wallets: EVM, MetaMask, 0xb0b7; Solana, Phantom, B16P"
        );
        assert!(
            both.contains(&format!(
                "Your wallets: EVM, Development wallet, 0x{}; Solana, Development wallet, {solana}",
                "7e".repeat(20)
            )),
            "{both}"
        );
        // The network chip of a local signer isn't drawn; a browser
        // wallet's says it is the EVM wallet's when both are connected.
        let chip = View::Many(vec![chip(84532, None, true)]).html();
        assert!(chip.contains(">EVM<"), "{chip}");
        assert!(
            chip.contains("Your EVM wallet is on Base Sepolia"),
            "{chip}"
        );
    }

    #[test]
    fn the_chip_is_neutral_where_the_page_acts_on_no_vault_and_cautions_only_on_another_vaults_chain()
     {
        let html = |on: u64, want: Option<u64>| View::Many(vec![chip(on, want, false)]).html();
        // No vault on the page: a network Shonin doesn't use is just named.
        for (id, name) in [
            (11155111, "Ethereum Sepolia"),
            (42161, "Arbitrum One"),
            (84532, "Base Sepolia"),
            (999_999, "Network 999999"),
        ] {
            let chip = html(id, None);
            assert!(chip.contains(&format!(">{name}<")), "{chip}");
            assert!(!chip.contains("sh-netchip--other"), "{chip}");
            assert!(!chip.contains("Not on Shonin"), "{chip}");
            assert!(!chip.contains("isn’t one of"), "{chip}");
            // The switch stays, quietly.
            assert!(chip.contains("aria-haspopup=\"dialog\""), "{chip}");
        }
        // A vault on Base Sepolia: a wallet there is fine, one elsewhere is
        // cautioned.
        assert!(!html(84532, Some(84532)).contains("sh-netchip--other"));
        let elsewhere = html(11155111, Some(84532));
        assert!(elsewhere.contains("sh-netchip--other"), "{elsewhere}");
        assert!(
            elsewhere.contains("this vault is on Base Sepolia"),
            "{elsewhere}"
        );
        // Which pages act on a vault: the agent page, claims, manage and
        // activate (and, by registering, the deposit and withdraw sheets and
        // the publish flow once its chain is chosen). None of the others.
        for route in [
            Route::Agents { author: None },
            Route::Portfolio,
            Route::You,
            Route::Status,
            Route::Publish,
        ] {
            assert_eq!(route_slug(&route), None, "{route:?}");
        }
        assert_eq!(
            route_slug(&Route::Claim {
                slug: "base-2".to_string(),
                id: 1
            })
            .as_deref(),
            Some("base-2")
        );
        assert_eq!(
            route_slug(&Route::Manage {
                slug: "base-2".to_string()
            })
            .as_deref(),
            Some("base-2")
        );
        // "Sepolia" is Ethereum's; Base Sepolia is Shonin's own.
        assert_eq!(networks::network_name(11155111), "Ethereum Sepolia");
        assert_eq!(networks::network_name(84532), "Base Sepolia");
    }

    #[test]
    fn a_sheet_or_panel_acting_on_a_vault_sets_the_chips_chain_only_while_it_is_open() {
        use std::task::{Context, Poll};
        let last = || ACTING.with(|acting| acting.lock_ref().last().map(|(_, chain)| *chain));
        assert_eq!(last(), None);
        let mut held = Box::pin(acting_on(futures_signals::signal::always(Some(998))));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(held.as_mut().poll(&mut cx), Poll::Pending));
        assert!(matches!(held.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(last(), Some(998), "held while the element is on the page");
        drop(held);
        assert_eq!(last(), None, "gone with the element");
    }

    #[test]
    fn the_switch_offers_the_pages_vault_chain_first_then_the_apps_others() {
        assert_eq!(
            switch_targets(Some(998), &[84532, 998, 46630]),
            vec![998, 84532, 46630]
        );
        assert_eq!(switch_targets(None, &[84532, 998]), vec![84532, 998]);
        // A page chain the app doesn't read isn't offered.
        assert_eq!(switch_targets(Some(1), &[84532]), vec![84532]);
    }

    #[test]
    fn the_wallet_sheet_lists_both_kinds_and_offers_the_one_not_connected() {
        let html = View::Many(vec![sheet_body(
            &Connection::SignedOut,
            Some(chain::solana_ix::Pubkey::new_from_array([7; 32])),
        )])
        .html();
        assert!(html.contains("Connect an EVM wallet"), "{html}");
        assert!(html.contains("Disconnect the Solana wallet"), "{html}");
        assert!(!html.contains("Connect a Solana wallet"), "{html}");
    }

    #[test]
    fn each_wallet_section_of_the_sheet_names_its_chain_family_app_and_network() {
        assert_eq!(
            sheet_heading(Family::Evm, Some("MetaMask"), Some("Base Sepolia")),
            "EVM: MetaMask on Base Sepolia"
        );
        assert_eq!(
            sheet_heading(Family::Solana, Some("Phantom"), Some("devnet")),
            "Solana: Phantom on devnet"
        );
        assert_eq!(
            sheet_heading(Family::Evm, Some("Development wallet"), None),
            "EVM: Development wallet"
        );
        assert_eq!(sheet_heading(Family::Solana, None, None), "Solana wallet");
    }

    fn option(rdns: &str, name: &str, detected: bool) -> WalletOption {
        WalletOption {
            id: WalletId::Announced(rdns.to_string()),
            name: name.to_string(),
            icon: None,
            detected,
            install: None,
        }
    }

    #[test]
    fn another_app_in_the_evm_slot_is_offered_back_as_use_metamask_instead() {
        let phantom = WalletId::Announced("app.phantom".to_string());
        let metamask = WalletId::Announced("io.metamask".to_string());
        let wallets = [
            option("io.metamask", "MetaMask", true),
            option("app.phantom", "Phantom", true),
            option("io.rabby", "Rabby", false),
        ];
        // The one used before, installed: offered back.
        assert_eq!(
            instead(&phantom, "Phantom", None, Some(&metamask), &wallets).map(|o| o.name),
            Some("MetaMask".to_string())
        );
        // Phantom's EVM side took the slot beside Phantom on Solana, with
        // no record of what came before: the other installed EVM wallet.
        assert_eq!(
            instead(&phantom, "Phantom", Some("Phantom"), None, &wallets).map(|o| o.name),
            Some("MetaMask".to_string())
        );
        // The same wallet as before, or one not installed: nothing offered.
        assert_eq!(
            instead(
                &metamask,
                "MetaMask",
                Some("Phantom"),
                Some(&metamask),
                &wallets
            ),
            None
        );
        let rabby = WalletId::Announced("io.rabby".to_string());
        assert_eq!(
            instead(&metamask, "MetaMask", None, Some(&rabby), &wallets),
            None
        );
        // A development or phrase wallet is never second-guessed.
        assert_eq!(
            instead(
                &WalletId::Dev,
                "Development wallet",
                Some("Development wallet"),
                None,
                &wallets
            ),
            None
        );
    }

    #[test]
    fn the_recovery_phrase_field_is_never_offered_to_a_password_manager() {
        let html = phrase_entry(&Mutable::new(true)).html();
        let start = html.find("id=\"sh-phrase\"").expect("the phrase field");
        let open = html[..start].rfind('<').expect("its tag");
        let tag = &html[open..open + html[open..].find('>').expect("its tag's end")];
        assert!(!tag.contains("type=\"password\""), "{tag}");
        assert!(tag.contains("type=\"text\""), "{tag}");
        assert!(tag.contains("sh-input__secret"), "{tag}");
        assert!(tag.contains("autocomplete=\"off\""), "{tag}");
        assert!(tag.contains("data-1p-ignore"), "{tag}");
    }
}
