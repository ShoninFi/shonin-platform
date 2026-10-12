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
/// phone); connected, the network chip (a browser wallet's chain) and the
/// wallet button, which opens the wallet menu.
pub fn header_control() -> View {
    let session = crate::wallet::get();
    el("div")
        .class("sh-walletctl")
        .child_signal(
            Some(connect_button().into()),
            session.connection.signal_cloned().map(|connection| {
                Some(match connection {
                    Connection::Connected {
                        account, chain_id, ..
                    } => connected(account, chain_id),
                    _ => connect_button().into(),
                })
            }),
        )
        // The Solana wallet's button, while one is connected.
        .child(super::solana::wallet::header_button())
        .into()
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

fn connected(account: alloy_primitives::Address, chain_id: Option<u64>) -> View {
    let session = crate::wallet::get();
    let full = format!("{account:?}");
    let chip = match chain_id.filter(|_| session.is_browser()) {
        Some(chain_id) => {
            let name = networks::chain_name(chain_id);
            el("span")
                .class("sh-netchip")
                .attr("aria-label", format!("Your wallet is on {name}"))
                .child(tile(&name))
                .child(el("span").class("sh-netchip__name").text(name))
                .into()
        }
        None => View::Empty,
    };
    let button = on_click(
        el("button")
            .attr("type", "button")
            .class("sh-btn sh-btn-secondary sh-btn-sm sh-walletbtn")
            .attr("aria-label", format!("Wallet {full}"))
            .attr("aria-haspopup", "dialog")
            .child(icon(Icon::Wallet, 16))
            .child(el("span").class("sh-mono").text(short_hash(&full))),
        move || wallet_menu(account),
    );
    View::Many(vec![chip, button.into()])
}

/// The wallet menu: the full address, Portfolio, Disconnect.
fn wallet_menu(account: alloy_primitives::Address) {
    let session = crate::wallet::get();
    let full = format!("{account:?}");
    let body = el("div")
        .class("sh-walletmenu")
        .child(
            el("p")
                .class("sh-muted")
                .text_from(&session.wallet_name, |name| {
                    if name.is_empty() {
                        "Connected".to_string()
                    } else {
                        format!("Connected with {name}")
                    }
                }),
        )
        .child(crate::atoms::hash::hash(&full, None, "address"))
        .child(
            crate::web::app_link("/portfolio")
                .class("sh-link")
                .text("View your portfolio")
                .hook(|dom| dom.event(|_: dominator::events::Click| dialog::close())),
        );
    dialog::open(Dialog {
        title: "Your wallet".to_string(),
        body: body.into(),
        foot: Some(
            on_click(
                Button::new(Kind::Secondary, "Disconnect").build(),
                move || {
                    crate::wallet::get().disconnect();
                    dialog::close();
                },
            )
            .into(),
        ),
        wide: false,
    });
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
            .future(
                crate::wallet::get()
                    .connection
                    .signal_cloned()
                    .for_each(|connection| {
                        if matches!(connection, Connection::Connected { .. }) {
                            dialog::close();
                        }
                        async {}
                    }),
            )
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
    let options = session.wallets.get_cloned();
    let mut rows: Vec<View> = options.into_iter().map(wallet_row).collect();
    rows.extend(super::solana::wallet::rows());
    if session.dev_available() {
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
    if dev::PHRASE_WALLET {
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

fn wallet_row(option: WalletOption) -> View {
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
    option_row(mark, &option.name, None, option.detected)
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
    let busy = Mutable::new(false);
    let label = format!("Switch to {}", networks::chain_name(chain_id));
    on_click(
        Button::new(Kind::Primary, label)
            .size(size)
            .busy(&busy)
            .build(),
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
