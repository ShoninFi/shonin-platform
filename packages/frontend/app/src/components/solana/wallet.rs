//! The Solana wallet in the page, beside the EVM one: its rows in the
//! connect dialog, its own connecting states (a dialog of the same shape:
//! connecting with the hex ring, rejected, not installed, locked), its
//! button in the header and its menu. Either kind may be connected, or both.

use futures_signals::signal::SignalExt;
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, on_click};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::icon;
use crate::components::wallet::{hex_ring, tile};
use crate::view::{El, View, el};
use crate::wallet::solana::{SolConnection, SolWalletId, SolWalletOption};

/// Ask Solana wallets to register again (the connect dialog opening).
pub fn discover() {
    #[cfg(target_arch = "wasm32")]
    crate::wallet::solana::get().start_discovery();
}

/// The Solana rows of the connect dialog's list: every Solana wallet this
/// browser has, the popular ones named either way, and the development
/// wallet on its own when the EVM side doesn't offer it (it offers none, or
/// an EVM wallet is already connected, so the list shows only Solana).
pub fn rows() -> Vec<View> {
    let session = crate::wallet::solana::get();
    let mut rows: Vec<View> = session
        .wallets
        .get_cloned()
        .into_iter()
        .map(wallet_row)
        .collect();
    let evm = crate::wallet::get();
    let evm_connected = matches!(
        *evm.connection.lock_ref(),
        crate::wallet::machine::Connection::Connected { .. }
    );
    if session.dev_available() && (!evm.dev_available() || evm_connected) {
        rows.push(
            option_row(
                tile("D").into(),
                "Development wallet",
                "Solana · a key of this development cluster",
                false,
            )
            .hook(|dom| {
                dom.event(|_: dominator::events::Click| {
                    crate::wallet::solana::get().connect_dev();
                    dialog::close();
                })
            })
            .into(),
        );
    }
    rows
}

/// When the EVM development wallet connects, the Solana one connects too
/// (both are this build's development wallet).
pub fn also_connect_dev() {
    crate::wallet::solana::get().connect_dev();
}

/// When a recovery phrase connects on the EVM side, its first Solana
/// account connects too. The phrase stays in memory only.
pub fn also_connect_phrase(phrase: &str) {
    let _ = crate::wallet::solana::get().connect_phrase(phrase);
}

fn option_row(mark: View, name: &str, detail: &str, detected: bool) -> El {
    el("button")
        .attr("type", "button")
        .class("sh-walletopt")
        .child(mark)
        .child(
            el("span")
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
        )
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

fn wallet_row(option: SolWalletOption) -> View {
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
    option_row(mark, &option.name, "Solana", option.detected)
        .attr("aria-label", format!("{} for Solana", option.name))
        .hook(move |dom| dom.event(move |_: dominator::events::Click| connect(id.clone())))
        .into()
}

fn name_of(id: &SolWalletId) -> String {
    match id {
        SolWalletId::Standard(name) => name.clone(),
        SolWalletId::Legacy => "Phantom".to_string(),
        SolWalletId::Phrase => crate::wallet::PHRASE_WALLET.to_string(),
        SolWalletId::Dev => crate::wallet::DEV_WALLET.to_string(),
    }
}

/// Connect a Solana browser wallet, in a dialog of its own that follows the
/// connection's states and closes once it is connected.
fn connect(id: SolWalletId) {
    let session = crate::wallet::solana::get();
    let body = el("div")
        .class("sh-connect")
        .child_signal(
            None,
            session
                .connection
                .signal_cloned()
                .map(|connection| Some(state_view(&connection))),
        )
        .hook(|dom| {
            dom.after_removed(|_| {
                let session = crate::wallet::solana::get();
                if session.connection.lock_ref().in_dialog() {
                    session.cancel_connecting();
                }
            })
            .future(
                crate::wallet::solana::get()
                    .connection
                    .signal_cloned()
                    .for_each(|connection| {
                        if matches!(connection, SolConnection::Connected { .. }) {
                            dialog::close();
                        }
                        async {}
                    }),
            )
        });
    dialog::open(Dialog {
        title: "Connect a Solana wallet".to_string(),
        body: body.into(),
        foot: None,
        wide: false,
    });
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move { session.connect(id).await });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (session, id);
}

fn back_to_list() {
    crate::wallet::solana::get().cancel_connecting();
    crate::components::wallet::open_connect();
}

fn state_view(connection: &SolConnection) -> View {
    let problem = |title: String, line: String, action: Option<View>| -> View {
        el("div")
            .class("sh-connect__state")
            .attr("role", "status")
            .child(el("p").class("sh-connect__title").text(title))
            .child(el("p").text(line))
            .child(
                el("div")
                    .class("sh-connect__actions")
                    .child(action.unwrap_or(View::Empty))
                    .child(on_click(
                        Button::new(Kind::Secondary, "Choose a wallet").build(),
                        back_to_list,
                    )),
            )
            .into()
    };
    match connection {
        SolConnection::Connecting { wallet } => {
            let name = name_of(wallet);
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
                    back_to_list,
                ))
                .into()
        }
        SolConnection::Rejected { wallet } => problem(
            "Rejected in wallet. Nothing was shared.".to_string(),
            format!(
                "You declined in {}. Connecting never moves money.",
                name_of(wallet)
            ),
            None,
        ),
        SolConnection::NotInstalled { wallet } => {
            let name = name_of(wallet);
            let install = crate::wallet::solana::session::POPULAR
                .iter()
                .find(|(popular, _)| *popular == name)
                .map(|(_, href)| *href);
            problem(
                format!("{name} isn’t installed in this browser"),
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
        SolConnection::Locked { wallet } => {
            let name = name_of(wallet);
            problem(
                format!("{name} is locked"),
                format!("Unlock {name}, or answer the request waiting in it, then try again."),
                None,
            )
        }
        SolConnection::SignedOut | SolConnection::Connected { .. } => View::Empty,
    }
}
