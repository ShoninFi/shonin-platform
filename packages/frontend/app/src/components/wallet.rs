//! The wallet in the page: the header's network chip and wallet button (or
//! "Connect wallet"), the connect dialog (a sheet on phone) with every state
//! (choosing, connecting, rejected, not installed, locked, the recovery
//! phrase), the wallet menu, and the wrong-network bar.

use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::format::short_hash;
use crate::route::{DOCS, TERMS};
use crate::view::{El, View, el, text};
use crate::wallet::machine::{Connection, WalletId};
use crate::wallet::{WalletOption, dev, networks};

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

/// The header's wallet control: "Connect wallet" signed out ("Connect" on
/// phone). Connected, one account button whatever is connected (an EVM
/// wallet, a Solana wallet or both), beside the network chip of a browser
/// EVM wallet. The button opens the wallet sheet, which lists both kinds
/// and offers to connect the one that isn't.
pub fn header_control() -> View {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    let now = control(&session.connection.lock_ref(), solana.account());
    el("div")
        .class("sh-walletctl")
        .child_signal(
            Some(now),
            futures_signals::map_ref! {
                let evm = session.connection.signal_cloned(),
                let solana = solana.connection.signal_cloned() => control(evm, solana.account())
            }
            .map(Some),
        )
        .into()
}

/// What the header holds for a pair of connections.
fn control(evm: &Connection, solana: Option<chain::solana_ix::Pubkey>) -> View {
    let evm = match evm {
        Connection::Connected {
            account, chain_id, ..
        } => Some((*account, *chain_id)),
        _ => None,
    };
    match (evm, solana) {
        (None, None) => connect_button().into(),
        (Some((account, chain_id)), solana) => View::Many(vec![
            network_chip(chain_id),
            account_button(Some(account), solana).into(),
        ]),
        (None, Some(solana)) => account_button(None, Some(solana)).into(),
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

/// The chain a browser EVM wallet is on; a local signer has none to show.
/// A network no vault is on is marked as such, in the caution style, and
/// offers the switch to one that is.
fn network_chip(chain_id: Option<u64>) -> View {
    let session = crate::wallet::get();
    match chain_id.filter(|_| session.is_browser()) {
        Some(chain_id) => {
            let now = chip(chain_id, ours(&session.app_chains(), chain_id));
            el("span")
                .class("sh-contents")
                .child_signal(
                    Some(now),
                    session
                        .networks
                        .signal_ref(move |networks| {
                            let chains: Vec<u64> =
                                networks.values().map(|network| network.chain_id).collect();
                            ours(&chains, chain_id)
                        })
                        .dedupe()
                        .map(move |ours| Some(chip(chain_id, ours))),
                )
                .into()
        }
        None => View::Empty,
    }
}

/// Whether `chain_id` is one of the app's vault chains; `None` until the
/// app has read which those are.
fn ours(app_chains: &[u64], chain_id: u64) -> Option<bool> {
    (!app_chains.is_empty()).then(|| app_chains.contains(&chain_id))
}

fn chip(chain_id: u64, ours: Option<bool>) -> View {
    let name = networks::network_name(chain_id);
    if ours != Some(false) {
        return el("span")
            .class("sh-netchip")
            .attr("aria-label", format!("Your wallet is on {name}"))
            .child(tile(&name))
            .child(el("span").class("sh-netchip__name").text(name))
            .into();
    }
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-netchip sh-netchip--other")
            .attr("aria-haspopup", "dialog")
            .attr(
                "aria-label",
                format!(
                    "Your wallet is on {name}, which isn’t one of Shonin’s networks. Switch network"
                ),
            )
            .child(icon(Icon::Concern, 16))
            .child(el("span").class("sh-netchip__name").text(name))
            .child(el("span").class("sh-netchip__flag").text("Not on Shonin")),
        move || open_switch(chain_id),
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

/// The vault chain of the agent the page is about, if it is about one.
fn page_chain() -> Option<u64> {
    use crate::route::Route;
    let location = crate::web::window()?.location();
    let route = Route::from_url(&location.pathname().ok()?, &location.search().ok()?);
    let slug = match route {
        Route::Agent { slug, .. }
        | Route::Claim { slug, .. }
        | Route::Manage { slug }
        | Route::Activate { slug } => slug,
        _ => return None,
    };
    let chain = crate::store::get()
        .agent(&slug)?
        .strategy
        .lock_ref()
        .vault
        .chain;
    crate::wallet::get()
        .networks
        .lock_ref()
        .get(&chain)
        .map(|network| network.chain_id)
}

/// The wallet is on a network no vault is on: offer the page's vault chain
/// first, then the app's others, through the usual switch (adding the chain
/// if the wallet doesn't know it). Closes once the wallet is on one.
fn open_switch(on: u64) {
    let session = crate::wallet::get();
    let app_chains = session.app_chains();
    let page = page_chain();
    let targets = switch_targets(page, &app_chains);
    let name = networks::network_name(on);
    let lead = match page {
        Some(page) => format!(
            "Your wallet is on {name}, which isn’t one of Shonin’s networks. This agent’s vault is on {}.",
            networks::chain_name(page)
        ),
        None => format!(
            "Your wallet is on {name}, which isn’t one of Shonin’s networks. Switch to one of them to deposit, withdraw or publish."
        ),
    };
    let watch = {
        let chains = app_chains.clone();
        session
            .connection
            .signal_ref(|connection| match connection {
                Connection::Connected { chain_id, .. } => *chain_id,
                _ => None,
            })
            .dedupe()
            .for_each(move |chain_id| {
                if chain_id.is_some_and(|id| chains.contains(&id)) {
                    dialog::close();
                }
                async {}
            })
    };
    let buttons = targets.into_iter().enumerate().map(|(index, chain_id)| {
        let kind = if index == 0 {
            Kind::Primary
        } else {
            Kind::Secondary
        };
        switch_button_as(chain_id, Size::Medium, kind).class("sh-btn-block")
    });
    dialog::open(Dialog {
        title: "Switch network".to_string(),
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

/// The one account button: the EVM address when there is one, else the
/// Solana address; with both, a small Solana mark says the sheet holds two.
fn account_button(
    evm: Option<alloy_primitives::Address>,
    solana: Option<chain::solana_ix::Pubkey>,
) -> El {
    let evm = evm.map(|account| format!("{account:?}"));
    let solana = solana.map(|account| account.to_string());
    let label = match (&evm, &solana) {
        (Some(evm), Some(solana)) => format!("Your wallets: {evm}, and Solana {solana}"),
        (Some(evm), None) => format!("Wallet {evm}"),
        (None, Some(solana)) => format!("Solana wallet {solana}"),
        (None, None) => "Your wallets".to_string(),
    };
    let lead: View = match (&evm, &solana) {
        (Some(evm), _) => View::Many(vec![
            icon(Icon::Wallet, 16).into(),
            el("span").class("sh-mono").text(short_hash(evm)).into(),
        ]),
        (None, Some(solana)) => View::Many(vec![
            tile("S").into(),
            el("span")
                .class("sh-mono")
                .text(super::solana::exit::short(solana))
                .into(),
        ]),
        (None, None) => View::Empty,
    };
    let both = evm.is_some() && solana.is_some();
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-btn sh-btn-secondary sh-btn-sm sh-walletbtn")
            .attr("aria-label", label)
            .attr("aria-haspopup", "dialog")
            .child(lead)
            .child(View::from(
                both.then(|| tile("S").class("sh-walletbtn__also").into()),
            )),
        wallet_sheet,
    )
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
                let solana = solana.connection.signal_cloned() => (evm.clone(), solana.account())
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

/// The sheet's two sections, as the connections are now.
fn sheet_body(evm: &Connection, solana: Option<chain::solana_ix::Pubkey>) -> View {
    let session = crate::wallet::get();
    let section = |title: &'static str, body: View| -> View {
        el("section")
            .class("sh-walletsheet__kind")
            .child(el("h3").class("sh-label").text(title))
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
    let evm_part = match evm {
        Connection::Connected { account, .. } => {
            let name = session.wallet_name.get_cloned();
            View::Many(vec![
                el("p")
                    .class("sh-muted")
                    .text(if name.is_empty() {
                        "Connected".to_string()
                    } else {
                        format!("Connected with {name}")
                    })
                    .into(),
                crate::atoms::hash::hash(&format!("{account:?}"), None, "address").into(),
                disconnect("EVM", || crate::wallet::get().disconnect()),
            ])
        }
        _ => View::Many(vec![
            el("p")
                .class("sh-muted")
                .text("For agents whose vault is on an EVM chain.")
                .into(),
            add("Connect an EVM wallet"),
        ]),
    };
    let solana_part = match solana {
        Some(account) => {
            let name = crate::wallet::solana::get().wallet_name.get_cloned();
            View::Many(vec![
                el("p")
                    .class("sh-muted")
                    .text(if name.is_empty() {
                        "Connected".to_string()
                    } else {
                        format!("Connected with {name}")
                    })
                    .into(),
                crate::atoms::hash::hash(&account.to_string(), None, "address").into(),
                disconnect("Solana", || crate::wallet::solana::get().disconnect()),
            ])
        }
        None => View::Many(vec![
            el("p")
                .class("sh-muted")
                .text("For agents whose vault is on Solana.")
                .into(),
            add("Connect a Solana wallet"),
        ]),
    };
    View::Many(vec![
        section("EVM wallet", evm_part),
        section("Solana wallet", solana_part),
    ])
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
            WalletId::Phrase => "your recovery phrase wallet".to_string(),
            WalletId::Dev => "the development wallet".to_string(),
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
        labels.join(", ")
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
                    let context = crate::tx::error::Context {
                        wallet: &session.wallet_name.get_cloned(),
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
        header_control().html()
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
        let both = header_with(true, true);
        assert!(both.contains("aria-label=\"Your wallets: 0x7e7e"), "{both}");
        assert!(both.contains("sh-walletbtn__also"), "{both}");
        let solana = header_with(false, true);
        assert!(solana.contains("aria-label=\"Solana wallet "), "{solana}");
        let none = header_with(false, false);
        assert!(none.contains("Connect wallet"), "{none}");
        assert!(!none.contains("sh-walletbtn"), "{none}");
    }

    /// The header's chip for a browser wallet on `chain_id`, where the app's
    /// one vault chain is Base Sepolia.
    fn chip_on(chain_id: u64) -> String {
        let session = crate::wallet::get();
        session.networks.lock_mut().insert(
            shared::Chain::Base,
            Rc::new(crate::wallet::Network {
                chain: shared::Chain::Base,
                chain_id: 84532,
                label: "Base Sepolia".to_string(),
                rpc: "https://sepolia.base.org".to_string(),
                asset: alloy_primitives::Address::repeat_byte(0xdd),
                asset_symbol: "USDC".to_string(),
                asset_decimals: 6,
                dev_account: None,
            }),
        );
        session.connection.set(Connection::Connected {
            wallet: WalletId::Injected,
            account: alloy_primitives::Address::repeat_byte(0x7e),
            chain_id: Some(chain_id),
        });
        crate::wallet::solana::get()
            .connection
            .set(crate::wallet::solana::SolConnection::SignedOut);
        header_control().html()
    }

    #[test]
    fn a_network_shonin_doesnt_use_is_named_and_marked_with_a_way_to_switch() {
        let arbitrum = chip_on(42161);
        assert!(arbitrum.contains(">Arbitrum One<"), "{arbitrum}");
        assert!(!arbitrum.contains("chain 42161"), "{arbitrum}");
        assert!(arbitrum.contains("sh-netchip--other"), "{arbitrum}");
        assert!(arbitrum.contains("Not on Shonin"), "{arbitrum}");
        assert!(arbitrum.contains("aria-haspopup=\"dialog\""), "{arbitrum}");
        assert!(
            arbitrum.contains("which isn’t one of Shonin’s networks"),
            "{arbitrum}"
        );
        let unknown = chip_on(999_999);
        assert!(unknown.contains(">Network 999999<"), "{unknown}");
        assert!(unknown.contains("sh-netchip--other"), "{unknown}");
        for (id, name) in [
            (1, "Ethereum"),
            (10, "Optimism"),
            (137, "Polygon"),
            (8453, "Base"),
        ] {
            assert_eq!(networks::network_name(id), name);
        }
        // Base mainnet is a vault chain, but not this app's: still marked.
        assert!(chip_on(8453).contains("sh-netchip--other"));
        let ours = chip_on(84532);
        assert!(ours.contains(">Base Sepolia<"), "{ours}");
        assert!(!ours.contains("sh-netchip--other"), "{ours}");
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
        assert_eq!(ours(&[], 42161), None, "unknown until the chains are read");
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
