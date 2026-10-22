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

/// A small mark naming a wallet's chain family: "EVM", "SOL". It is read
/// out too, so a control's name holds the words it shows.
pub fn chain_mark(family: Family) -> El {
    el("span").class("sh-chainmark").text(match family {
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
    /// The route the header was drawn for, so its sheets can name the
    /// page's agent.
    static PAGE: std::cell::RefCell<Option<Route>> = const { std::cell::RefCell::new(None) };
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

/// The chain the screen signs on: the newest element on it that signs (a
/// panel, a gate, a claim's actions), else, on an agent's page (which takes
/// deposits) or its manage page, its vault's. A screen with nothing to sign
/// (a claim paid or taken, an activation done) has none.
fn signs_signal(route: &Route) -> impl futures_signals::signal::Signal<Item = Option<u64>> + use<> {
    let slug = matches!(route, Route::Agent { .. } | Route::Manage { .. })
        .then(|| route_slug(route))
        .flatten();
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

/// A chain's name as the app says it in sentences, rows and buttons alike:
/// the API config's label for one of Shonin's chains, else its known name.
pub fn chain_label(chain_id: u64) -> String {
    crate::wallet::get()
        .networks
        .lock_ref()
        .values()
        .find(|network| network.chain_id == chain_id)
        .map(|network| network.label.clone())
        .unwrap_or_else(|| networks::chain_name(chain_id))
}

/// `(on, want)` while a browser EVM wallet is on `on` and the page acts on
/// a vault on another chain `want`; `None` otherwise.
pub fn wrong_signal(
    want: impl futures_signals::signal::Signal<Item = Option<u64>>,
) -> impl futures_signals::signal::Signal<Item = Option<(u64, u64)>> {
    let session = crate::wallet::get();
    futures_signals::map_ref! {
        let connection = session.connection.signal_cloned(),
        let want = want => match connection {
            Connection::Connected { chain_id: Some(on), .. }
                if crate::wallet::get().is_browser() =>
            {
                want.filter(|want| want != on).map(|want| (*on, want))
            }
            _ => None,
        }
    }
    .dedupe()
}

/// `(on, want)` while a browser EVM wallet is on another chain than
/// `slug`'s vault.
pub fn vault_wrong_signal(
    slug: &str,
) -> impl futures_signals::signal::Signal<Item = Option<(u64, u64)>> + use<> {
    let slug = slug.to_string();
    let store = crate::store::get();
    let session = crate::wallet::get();
    wrong_signal(
        futures_signals::map_ref! {
            let _loaded = store.loaded.signal(),
            let networks = session.networks.signal_cloned() => vault_chain_id(&slug, networks)
        }
        .dedupe(),
    )
}

/// The name of the agent a route is about, once the store has it.
fn route_agent_title(route: &Route) -> Option<String> {
    let slug = route_slug(route)?;
    let title = crate::store::get().agent(&slug)?.title.get_cloned();
    (!title.is_empty()).then_some(title)
}

// The header ------------------------------------------------------------------

/// The header's wallet control: "Connect wallet" signed out ("Connect" on
/// phone). Connected, one account button whatever is connected (an EVM
/// wallet, a Solana wallet or both), each with its chain's mark, beside the
/// network chip of a browser EVM wallet. The button opens the wallet sheet,
/// which lists both kinds and offers to connect the one that isn't.
pub fn header_control(route: &Route) -> View {
    PAGE.with(|page| *page.borrow_mut() = Some(route.clone()));
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    let want = Mutable::new(None::<u64>);
    // While a remembered wallet reconnects, the header says so, neutrally,
    // rather than offering to connect one.
    let remembered = crate::wallet::session::remembers_wallet();
    let shown =
        move |evm: &Connection, solana: Option<Pubkey>, want: Option<u64>, restoring: bool| {
            if restoring && remembered && matches!(evm, Connection::SignedOut) && solana.is_none() {
                reconnecting().into()
            } else {
                control(evm, solana, want)
            }
        };
    let now = shown(
        &session.connection.lock_ref(),
        solana.account(),
        None,
        session.restoring.get(),
    );
    el("div")
        .class("sh-walletctl")
        .child_signal(
            Some(now),
            futures_signals::map_ref! {
                let evm = session.connection.signal_cloned(),
                let solana = solana.connection.signal_cloned(),
                let _evm_name = session.wallet_name.signal_cloned(),
                let _solana_name = solana.wallet_name.signal_cloned(),
                let restoring = session.restoring.signal(),
                let want = want.signal() => shown(evm, solana.account(), *want, *restoring)
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
        (Some(account), solana) => {
            let browser_chain = chain_id.filter(|_| session.is_browser());
            View::Many(vec![
                match browser_chain {
                    Some(chain_id) => chip(chain_id, want, solana.is_some()),
                    None => View::Empty,
                },
                account_button(
                    Some((account, session.app_name())),
                    solana.map(|account| (account, crate::wallet::solana::get().app_name())),
                    want,
                    browser_chain.zip(want).is_some_and(|(on, want)| on != want),
                )
                .into(),
            ])
        }
        (None, Some(solana)) => account_button(
            None,
            Some((solana, crate::wallet::solana::get().app_name())),
            want,
            false,
        )
        .into(),
    }
}

/// The header while a remembered wallet reconnects: a quiet, busy button
/// that does nothing yet.
fn reconnecting() -> El {
    el("button")
        .attr("type", "button")
        .class("sh-btn sh-btn-secondary sh-btn-sm sh-walletbtn")
        .attr("aria-disabled", "true")
        .attr("aria-busy", "true")
        .text("Connecting…")
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
/// opens the chain switcher. It cautions ("Wrong network") only where the
/// page acts on a vault on another chain (`want`); a network no vault is on
/// is otherwise just named. With a Solana wallet connected too, it says it
/// is the EVM wallet's.
fn chip(chain_id: u64, want: Option<u64>, both: bool) -> View {
    // One of Shonin's chains goes by the name the API's config gives it.
    let name = crate::wallet::get()
        .networks
        .lock_ref()
        .values()
        .find(|network| network.chain_id == chain_id)
        .map(|network| network.label.clone())
        .unwrap_or_else(|| networks::network_name(chain_id));
    let caution = want.filter(|want| *want != chain_id);
    let label = match caution {
        Some(want) => format!(
            "Wrong network: your EVM wallet is on {name}; this vault is on {}. Switch network",
            chain_label(want)
        ),
        None => format!("Your EVM wallet is on {name}. Change network"),
    };
    on_click(
        el("button")
            .attr("type", "button")
            .class("sh-btn sh-btn-ghost sh-btn-sm sh-netchip")
            .class_if(caution.is_some(), "sh-netchip--other")
            .attr("aria-haspopup", "dialog")
            .attr("aria-label", label)
            // The words, on hover, where a narrow header shows the mark alone.
            .attr(
                "title",
                match caution {
                    Some(_) => "Wrong network".to_string(),
                    None => name.clone(),
                },
            )
            // The triangle where it cautions, else the network's initial;
            // beside a Solana wallet, the EVM mark after it (shown only
            // where the chip names its network).
            .child(match caution {
                Some(_) => icon(Icon::Concern, 16).into(),
                None => View::from(tile(&name)),
            })
            .child(View::from(both.then(|| chain_mark(Family::Evm).into())))
            .child(
                el("span")
                    .class("sh-netchip__name")
                    .text(if caution.is_some() {
                        "Wrong network".to_string()
                    } else {
                        name
                    }),
            )
            .child(icon(Icon::ChevronDown, 16)),
        move || open_switch(chain_id, want),
    )
    .into()
}

/// One of Shonin's chains, as the switcher lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChainChoice {
    chain_id: u64,
    label: String,
    /// The wallet is on it now.
    current: bool,
    /// The vault the page acts on is on it.
    vault: bool,
}

/// The chains to list, from the API's chain config (`app_chains`, each with
/// its label): the page's vault chain first, then the others in order.
fn chain_choices(on: u64, want: Option<u64>, app_chains: &[(u64, String)]) -> Vec<ChainChoice> {
    let ids: Vec<u64> = app_chains.iter().map(|(id, _)| *id).collect();
    switch_targets(want, &ids)
        .into_iter()
        .filter_map(|chain_id| {
            let (_, label) = app_chains.iter().find(|(id, _)| *id == chain_id)?;
            Some(ChainChoice {
                chain_id,
                label: label.clone(),
                current: chain_id == on,
                vault: want == Some(chain_id),
            })
        })
        .collect()
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

/// The switcher's list: one row per chain; the wallet's own is marked and
/// does nothing, any other asks the wallet to switch to it.
fn chain_list(choices: Vec<ChainChoice>) -> El {
    el("ul")
        .class("sh-connect__list sh-chainlist")
        .attr("aria-label", "Shonin’s networks")
        .children(choices.into_iter().map(|choice| {
            let detail = match (choice.current, choice.vault) {
                (true, true) => Some("Your wallet is on it · this vault’s network"),
                (true, false) => Some("Your wallet is on it"),
                (false, true) => Some("This vault’s network"),
                (false, false) => None,
            };
            let end = if choice.current {
                Icon::Check
            } else {
                Icon::ChevronRight
            };
            let row = option_row_ending(
                tile(&choice.label).into(),
                &choice.label,
                detail,
                false,
                end,
            );
            let row = if choice.current {
                row.attr("aria-current", "true")
                    .attr("aria-disabled", "true")
                    .class("sh-walletopt--current")
            } else {
                let busy = Mutable::new(false);
                let chain_id = choice.chain_id;
                row.attr("aria-label", format!("Switch to {}", choice.label))
                    .attr_from("aria-busy", &busy, |busy| busy.then(|| "true".to_string()))
                    .hook(move |dom| {
                        dom.event(move |_: dominator::events::Click| switch_wallet(chain_id, &busy))
                    })
            };
            el("li").child(row)
        }))
}

/// The switch, from the chip: every chain Shonin's vaults are on (from the
/// API's chain config), the page's vault chain first. Closes once the
/// wallet moves.
fn open_switch(on: u64, want: Option<u64>) {
    let session = crate::wallet::get();
    let app_chains: Vec<(u64, String)> = session
        .networks
        .lock_ref()
        .values()
        .map(|network| (network.chain_id, network.label.clone()))
        .collect();
    let name = chain_label(on);
    let caution = want.filter(|want| *want != on);
    let lead = match caution {
        Some(want) => format!(
            "Your EVM wallet is on {name}. This vault is on {}.",
            chain_label(want)
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
    dialog::open(Dialog {
        title: if caution.is_some() {
            "Switch network".to_string()
        } else {
            "Network".to_string()
        },
        body: el("div")
            .class("sh-walletmenu")
            .child(el("p").text(lead))
            .child(chain_list(chain_choices(on, want, &app_chains)))
            .hook(move |dom| dom.future(watch))
            .into(),
        foot: None,
        wide: false,
        alert: None,
    });
}

/// The one account button: each connected wallet's chain mark and short
/// address, the EVM one first ("EVM 0xb0b7…b448 · SOL B16P…r9Kp"), with
/// each wallet's app named beside it on a wide screen and always in its
/// label. Two wallets stack below desktop. `wrong`: the EVM wallet is on
/// another chain than the vault the page acts on (`want`); where the header
/// has no room for the network chip's words (below 900) the button says
/// "Wrong network" itself, and on a phone shows the triangle, the words
/// kept in its name.
fn account_button(
    evm: Option<(Address, String)>,
    solana: Option<(Pubkey, String)>,
    want: Option<u64>,
    wrong: bool,
) -> El {
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
            .class_if(wrong, "sh-walletbtn--wrong")
            // The name is what it shows, then the full words, so it holds
            // the visible text at every width.
            .attr("title", label.clone())
            .attr("aria-haspopup", "dialog")
            .children(parts)
            .child(View::from(wrong.then(|| {
                el("span")
                    .class("sh-walletbtn__wrong")
                    .child(icon(Icon::Concern, 14))
                    .child(
                        el("span")
                            .class("sh-walletbtn__wrongword")
                            .text(" Wrong network"),
                    )
                    .into()
            })))
            .child(el("span").class("sh-vh").text(format!(", {label}"))),
        move || wallet_sheet(want),
    )
}

/// The wallet's wrong-network state, an alert: the "Wrong network" chip,
/// "Switch network", the vault's chain and the wallet's, the move from one
/// to the other, one "Switch to <chain>" and Cancel, then a small
/// Disconnect, and a Solana wallet connected too with its address and
/// Disconnect. `who`: the agent whose vault it is, when the page is about
/// one. Closes once the wallet moves.
fn open_wrong_network(on: u64, want: u64, who: Option<String>) {
    let session = crate::wallet::get();
    let watch = session
        .connection
        .signal_ref(|connection| match connection {
            Connection::Connected { chain_id, .. } => *chain_id,
            _ => None,
        })
        .dedupe()
        .for_each(move |chain_id| {
            // The header's account button that opened the sheet is redrawn
            // for the new chain, so focus goes to the one in its place.
            if chain_id != Some(on) && dialog::is_open() {
                dialog::close();
                refocus_wallet_control(None);
            }
            async {}
        });
    let solana = crate::wallet::solana::get();
    let solana_part = solana.account().map(|account| {
        let cluster = solana
            .network
            .get_cloned()
            .map(|network| network.cluster.clone());
        other_wallet(&solana.app_name(), cluster.as_deref(), &account.to_string())
    });
    dialog::open(Dialog {
        title: "Switch network".to_string(),
        body: wrong_network_body(on, want, who.as_deref(), solana_part)
            .hook(move |dom| dom.future(watch))
            .into(),
        foot: None,
        wide: false,
        alert: Some(
            crate::atoms::badge::badge(
                crate::atoms::badge::Tone::Caution,
                Some(Icon::Concern),
                "Wrong network",
            )
            .into(),
        ),
    });
}

/// The Solana wallet in the wrong-network state: its app and cluster, its
/// address and Disconnect.
fn other_wallet(app: &str, cluster: Option<&str>, address: &str) -> El {
    el("section")
        .class("sh-wrongsheet__other")
        .child(el("h3").class("sh-walletsheet__title").text(sheet_heading(
            Family::Solana,
            Some(app),
            cluster,
        )))
        .child(crate::atoms::hash::hash(address, None, "address"))
        .child(on_click(disconnect_button(Family::Solana), || {
            crate::wallet::solana::get().disconnect();
            dialog::close();
            refocus_wallet_control(None);
        }))
}

/// A wallet's Disconnect, the same in every sheet: a small secondary
/// button, named for the kind of wallet it disconnects.
fn disconnect_button(family: Family) -> El {
    let what = match family {
        Family::Evm => "EVM",
        Family::Solana => "Solana",
    };
    Button::new(Kind::Secondary, "Disconnect")
        .size(Size::Small)
        .build()
        .attr("aria-label", format!("Disconnect the {what} wallet"))
}

/// After a Disconnect or a switch of network, whose button is gone once the
/// sheet or the header redraws: focus moves to the control in its place,
/// the first in the sheet's `section` (its wallet kind's section, now
/// offering to connect one) while the sheet is open, else the header's
/// account button.
fn refocus_wallet_control(section: Option<u32>) {
    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(0).await;
        let Some(document) = crate::web::document() else {
            return;
        };
        let in_sheet = section.and_then(|index| {
            let sections = document
                .query_selector_all(".sh-dialog .sh-walletsheet__kind")
                .ok()?;
            let section =
                wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(sections.item(index)?).ok()?;
            crate::web::focusables(&section).into_iter().next()
        });
        let target = in_sheet.or_else(|| {
            let control = document.query_selector(".sh-walletctl").ok()??;
            crate::web::focusables(&control).into_iter().next()
        });
        if let Some(target) = target {
            crate::web::focus(&target);
        }
    });
}

/// The wrong-network state's body: whose vault is on which chain, the
/// wallet's chain, the move from one to the other, the switch and Cancel.
fn wrong_network_body(on: u64, want: u64, who: Option<&str>, solana: Option<El>) -> El {
    let (from, to) = (chain_label(on), chain_label(want));
    let vault = match who {
        Some(who) => format!("{who}’s vault"),
        None => "This vault".to_string(),
    };
    let place = |name: &str, strong: bool| -> View {
        el("span")
            .class("sh-wrongsheet__chain")
            .class_if(strong, "sh-wrongsheet__chain--to")
            .child(tile(name))
            .text(name.to_string())
            .into()
    };
    el("div")
        .class("sh-walletmenu sh-wrongsheet")
        .child(
            el("p")
                .class("sh-wrongsheet__lead")
                .attr("id", dialog::DESCRIPTION_ID)
                .text(format!(
                    "{vault} is on {to}. Your wallet is on {from} right now."
                )),
        )
        .child(
            el("p")
                .class("sh-wrongsheet__move")
                .attr("aria-label", format!("From {from} to {to}"))
                .child(place(&from, false))
                .child(icon(Icon::ArrowRight, 16))
                .child(place(&to, true)),
        )
        .child(
            el("div")
                .class("sh-wrongsheet__actions")
                .child(switch_button(want, Size::Medium).class("sh-wrongsheet__switch"))
                .child(on_click(
                    Button::new(Kind::Secondary, "Cancel").build(),
                    dialog::close,
                )),
        )
        .child(on_click(
            disconnect_button(Family::Evm).class("sh-wrongsheet__leave"),
            || {
                crate::wallet::get().disconnect();
                dialog::close();
                refocus_wallet_control(None);
            },
        ))
        .child(View::from(solana.map(View::from)))
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
fn wallet_sheet(want: Option<u64>) {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    // A browser wallet on another chain than the page's vault: the sheet is
    // the wrong-network state, one switch to the vault's chain.
    let on = match &*session.connection.lock_ref() {
        Connection::Connected { chain_id, .. } => *chain_id,
        _ => None,
    };
    if let (Some(on), Some(want)) = (on, want)
        && on != want
        && session.is_browser()
    {
        let page = PAGE.with(|page| page.borrow().clone());
        let who = page.as_ref().and_then(route_agent_title);
        open_wrong_network(on, want, who);
        return;
    }
    // Drawn now as well, so the dialog's first focus lands in the first
    // section rather than past it.
    let now = sheet_body(&session.connection.lock_ref(), solana.account(), want);
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
            .map(move |(evm, solana)| {
                // Nothing left connected: the sheet has nothing to list.
                if !matches!(evm, Connection::Connected { .. }) && solana.is_none() {
                    dialog::close();
                }
                Some(sheet_body(&evm, solana, want))
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
        alert: None,
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

/// The sheet's two sections, as the connections are now. A browser EVM
/// wallet's section offers the chain switcher too (as the network chip
/// does), saying "Switch network" where the page acts on a vault on
/// another chain (`want`).
fn sheet_body(evm: &Connection, solana: Option<Pubkey>, want: Option<u64>) -> View {
    let session = crate::wallet::get();
    let solana_session = crate::wallet::solana::get();
    let section = |title: String, body: View| -> View {
        el("section")
            .class("sh-walletsheet__kind")
            .child(el("h3").class("sh-walletsheet__title").text(title))
            .child(body)
            .into()
    };
    let disconnect = |family: Family, section: u32, act: fn()| -> View {
        on_click(disconnect_button(family), move || {
            act();
            refocus_wallet_control(Some(section));
        })
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
            let browser_chain = chain_id.filter(|_| session.is_browser());
            let on = browser_chain.map(chain_label);
            let switch = browser_chain.map(|on| {
                let wrong = want.is_some_and(|want| want != on);
                View::from(on_click(
                    Button::new(
                        if wrong {
                            Kind::Primary
                        } else {
                            Kind::Secondary
                        },
                        if wrong {
                            "Switch network"
                        } else {
                            "Change network"
                        },
                    )
                    .size(Size::Small)
                    .build()
                    .attr("aria-haspopup", "dialog"),
                    move || {
                        dialog::close();
                        open_switch(on, want);
                    },
                ))
            });
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
                        .child(View::from(switch))
                        .child(disconnect(Family::Evm, 0, || {
                            crate::wallet::get().disconnect()
                        }))
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
                    disconnect(Family::Solana, 1, || {
                        crate::wallet::solana::get().disconnect()
                    }),
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
        alert: None,
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
    option_row_ending(mark, name, detail, detected, Icon::ChevronRight)
}

/// A row of a list of choices, ending in `end` (a chevron, or a check on
/// the one already chosen).
fn option_row_ending(
    mark: View,
    name: &str,
    detail: Option<&str>,
    detected: bool,
    end: Icon,
) -> El {
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
        .child(icon(end, 18))
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
    let phrase = Rc::new(crate::wallet::phrase::HeldPhrase::default());
    let error = Mutable::new(None::<String>);
    let connect = {
        let phrase = phrase.clone();
        let error = error.clone();
        move || {
            let tried = phrase.submit(|words| {
                crate::wallet::get().connect_phrase(words)?;
                super::solana::wallet::also_connect_phrase(words);
                Ok(())
            });
            match tried {
                Ok(()) => error.set(None),
                Err(bad) => {
                    clear_phrase_field();
                    error.set(Some(bad.words().to_string()))
                }
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
                                                phrase.set(input.value());
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

/// Empty the recovery-phrase field, so a refused phrase isn't left on the
/// page.
fn clear_phrase_field() {
    let field = crate::web::document()
        .and_then(|document| document.get_element_by_id("sh-phrase"))
        .and_then(|element| {
            wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlInputElement>(element).ok()
        });
    if let Some(field) = field {
        field.set_value("");
    }
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

/// The wrong-network bar: under the header, only on screens that sign
/// something now (the agent page, a claim with an action, a gate on manage
/// or activate, or a panel that signs). Nothing shows while the wallet is
/// on the vault's chain, or isn't a browser wallet.
pub fn page_wrong_network_bar(route: &Route) -> View {
    let route = route.clone();
    el("div")
        .class("sh-wrongnet-slot")
        .class_if(signs_only_on_phone(&route), "sh-wrongnet-slot--phone")
        .child_signal(
            None,
            wrong_signal(signs_signal(&route)).map(move |wrong| {
                let (on, want) = wrong?;
                let what = match route_agent_title(&route) {
                    Some(title) if matches!(route, Route::Agent { .. }) => {
                        format!("{title} takes deposits")
                    }
                    Some(title) => format!("{title}’s vault is"),
                    None => "This page signs".to_string(),
                };
                Some(wrong_network(on, want, &what).into())
            }),
        )
        .into()
}

/// Whether the screen signs only through the phone's action bar: an
/// agent's Source tab has no Exit panel or rail, so from the width the
/// action bar gives way, nothing on it signs and the bar is not shown.
fn signs_only_on_phone(route: &Route) -> bool {
    matches!(
        route,
        Route::Agent {
            tab: crate::route::AgentTab::Source,
            ..
        }
    )
}

/// The bar itself: the wallet is on `on`, and `what` needs `want`. Its
/// switch is a small secondary button, so the screen's own action stays
/// its one primary; on a phone the second sentence gives way to the
/// button, which names the chain.
pub(crate) fn wrong_network(on: u64, want: u64, what: &str) -> El {
    el("div").class("sh-wrongnet").attr("role", "status").child(
        el("div")
            .class("sh-container sh-wrongnet__inner")
            .child(icon(Icon::Concern, 20))
            .child(
                el("p")
                    .child(el("strong").text(format!("Your wallet is on {}.", chain_label(on))))
                    .child(
                        el("span")
                            .class("sh-wrongnet__more")
                            .text(format!(" {what} on {}.", chain_label(want))),
                    ),
            )
            .child(switch_button_as(want, Size::Small, Kind::Secondary)),
    )
}

/// "Switch to Hyperliquid": asks the wallet to switch, adding the chain if it
/// doesn't know it.
pub fn switch_button(chain_id: u64, size: Size) -> El {
    switch_button_as(chain_id, size, Kind::Primary)
}

fn switch_button_as(chain_id: u64, size: Size, kind: Kind) -> El {
    let busy = Mutable::new(false);
    let label = format!("Switch to {}", chain_label(chain_id));
    on_click(
        Button::new(kind, label).size(size).busy(&busy).build(),
        move || switch_wallet(chain_id, &busy),
    )
}

/// Asks the browser wallet to move to `chain_id` (one of the API's chains),
/// adding it if the wallet doesn't know it; says why in a toast if it can't.
/// `busy` holds while the wallet is asked.
fn switch_wallet(chain_id: u64, busy: &Mutable<bool>) {
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
                chain: &chain_label(chain_id),
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
    {
        let _ = chain_id;
        busy.set(false);
    }
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
            solana.contains(">, Your Solana wallet: Development wallet, "),
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
    fn the_chain_switcher_lists_exactly_the_chains_the_api_config_names_and_marks_the_wallets_own()
    {
        // Whatever chains the API's config names, under its labels: none
        // added from the app's own table, none left out.
        let api = vec![
            (84532, "Base Sepolia (from the API)".to_string()),
            (998, "Hyperliquid testnet".to_string()),
            (777_777, "A chain the app has never heard of".to_string()),
        ];
        let choices = chain_choices(84532, Some(998), &api);
        let ids: Vec<u64> = choices.iter().map(|choice| choice.chain_id).collect();
        assert_eq!(ids, vec![998, 84532, 777_777], "the vault's chain first");
        assert!(
            choices
                .iter()
                .all(|choice| api.iter().any(|(_, label)| *label == choice.label))
        );
        let current: Vec<u64> = choices
            .iter()
            .filter(|c| c.current)
            .map(|c| c.chain_id)
            .collect();
        assert_eq!(current, vec![84532]);
        let html = View::from(chain_list(choices)).html();
        assert_eq!(html.matches("<li").count(), 3, "{html}");
        assert!(html.contains("Base Sepolia (from the API)"), "{html}");
        assert!(
            html.contains("A chain the app has never heard of"),
            "{html}"
        );
        assert!(!html.contains("Robinhood"), "{html}");
        // The wallet's own chain is marked and isn't offered as a switch;
        // every other one is.
        assert_eq!(html.matches("aria-current=\"true\"").count(), 1, "{html}");
        assert!(
            html.contains("aria-label=\"Switch to Hyperliquid testnet\""),
            "{html}"
        );
        assert!(
            !html.contains("Switch to Base Sepolia (from the API)"),
            "{html}"
        );
        // No chains read yet: an empty list, never a made-up one.
        assert!(chain_choices(84532, None, &[]).is_empty());
    }

    #[test]
    fn a_chip_on_another_chain_than_the_vaults_says_wrong_network_in_words() {
        let wrong = View::Many(vec![chip(11155111, Some(84532), false)]).html();
        assert!(wrong.contains(">Wrong network<"), "{wrong}");
        assert!(wrong.contains("sh-netchip--other"), "{wrong}");
        let fine = View::Many(vec![chip(84532, Some(84532), false)]).html();
        assert!(!fine.contains("Wrong network"), "{fine}");
        assert!(fine.contains(">Base Sepolia<"), "{fine}");
    }

    #[test]
    fn the_account_button_says_wrong_network_in_words_and_in_its_name_only_when_the_evm_wallet_is_elsewhere()
     {
        let account = Address::repeat_byte(0xb0);
        let html = |wrong: bool| {
            View::Many(vec![
                account_button(
                    Some((account, "MetaMask".to_string())),
                    None,
                    Some(998),
                    wrong,
                )
                .into(),
            ])
            .html()
        };
        let wrong = html(true);
        assert!(wrong.contains("sh-walletbtn--wrong"), "{wrong}");
        assert!(wrong.contains(" Wrong network<"), "{wrong}");
        assert!(
            !wrong.contains("aria-label"),
            "the name is the button's own words: {wrong}"
        );
        let fine = html(false);
        assert!(!fine.contains("Wrong network"), "{fine}");
    }

    /// Shonin's chains as the API config names them, for these tests.
    fn with_networks(chains: &[(u64, &str)]) {
        let session = crate::wallet::get();
        let mut networks = session.networks.get_cloned();
        networks.clear();
        for (i, (chain_id, label)) in chains.iter().enumerate() {
            let chain = [shared::Chain::Hyperliquid, shared::Chain::Base][i];
            networks.insert(
                chain,
                Rc::new(Network {
                    chain,
                    chain_id: *chain_id,
                    label: label.to_string(),
                    rpc: String::new(),
                    asset: Address::repeat_byte(0xdd),
                    asset_symbol: "USDC".to_string(),
                    asset_decimals: 6,
                    dev_account: None,
                    config: Default::default(),
                }),
            );
        }
        session.networks.set(networks);
    }

    #[test]
    fn the_wrong_network_sheet_says_whose_vault_is_where_and_offers_one_switch_and_cancel() {
        with_networks(&[(998, "Hyperliquid (API)"), (84_532, "Base (API)")]);
        let html = View::Many(vec![
            wrong_network_body(84_532, 998, Some("Market maker"), None).into(),
        ])
        .html();
        assert!(
            html.contains("Market maker’s vault is on Hyperliquid (API). Your wallet is on Base (API) right now."),
            "{html}"
        );
        assert!(html.contains(">Switch to Hyperliquid (API)<"), "{html}");
        assert!(html.contains(">Cancel<"), "{html}");
        assert!(html.contains("Disconnect the EVM wallet"), "{html}");
        assert!(
            html.contains(&format!("id=\"{}\"", dialog::DESCRIPTION_ID)),
            "the lead describes the alert: {html}"
        );
        assert_eq!(
            html.matches("<button").count(),
            3,
            "one switch, Cancel and the small Disconnect: {html}"
        );
        assert!(
            html.contains("From Base (API) to Hyperliquid (API)"),
            "{html}"
        );
    }

    #[test]
    fn the_wrong_network_sheet_keeps_a_connected_solana_wallets_address_and_disconnect() {
        with_networks(&[(998, "Hyperliquid (API)"), (84_532, "Base (API)")]);
        let solana = other_wallet("Phantom", Some("devnet"), "7abc");
        let html = View::Many(vec![
            wrong_network_body(84_532, 998, None, Some(solana)).into(),
        ])
        .html();
        assert!(html.contains("7abc"), "{html}");
        assert!(html.contains("Disconnect the Solana wallet"), "{html}");
        assert!(html.contains("Disconnect the EVM wallet"), "{html}");
        let alike = |what: &str| {
            let at = html
                .find(&format!("aria-label=\"Disconnect the {what} wallet\""))
                .unwrap();
            let start = html[..at].rfind("<button").unwrap();
            let class = &html[start..at];
            class.contains("sh-btn-secondary") && class.contains("sh-btn-sm")
        };
        assert!(
            alike("EVM") && alike("Solana"),
            "both Disconnects are the same small button: {html}"
        );
    }

    #[test]
    fn the_wrong_network_bar_on_an_agents_source_tab_is_kept_to_phone_widths_and_elsewhere_shown_at_all()
     {
        use crate::route::AgentTab;
        let slot = |tab| {
            View::Many(vec![page_wrong_network_bar(&Route::Agent {
                slug: "base-1".to_string(),
                tab,
            })])
            .html()
        };
        assert!(slot(AgentTab::Source).contains("sh-wrongnet-slot--phone"));
        for tab in [
            AgentTab::Overview,
            AgentTab::Review,
            AgentTab::Verify,
            AgentTab::Activity,
        ] {
            assert!(!slot(tab).contains("sh-wrongnet-slot--phone"), "{tab:?}");
        }
    }

    #[test]
    fn the_wrong_network_bar_names_chains_as_the_api_config_does() {
        with_networks(&[(998, "Hyperliquid (API)"), (84_532, "Base (API)")]);
        let html = View::Many(vec![
            wrong_network(84_532, 998, "Market maker takes deposits").into(),
        ])
        .html();
        assert!(
            html.contains("Your wallet is on Base (API).</strong>"),
            "{html}"
        );
        assert!(
            html.contains(" Market maker takes deposits on Hyperliquid (API).</span>"),
            "{html}"
        );
        assert!(html.contains(">Switch to Hyperliquid (API)<"), "{html}");
        assert!(
            html.contains("sh-btn-secondary"),
            "the bar's switch is secondary: {html}"
        );
        assert!(
            !html.contains("sh-btn-primary"),
            "the bar's switch is secondary: {html}"
        );
    }

    #[test]
    fn the_wallet_sheet_lists_both_kinds_and_offers_the_one_not_connected() {
        let html = View::Many(vec![sheet_body(
            &Connection::SignedOut,
            Some(chain::solana_ix::Pubkey::new_from_array([7; 32])),
            None,
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
