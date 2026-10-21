//! The author's controls for a vault on Solana (Manage-*): the tank (fund
//! it, top it up from the author's shares, the level auto-fill keeps, withdrawing
//! with the executor's signed total or through the unsigned escape), the
//! allowances, the idle floor, and retiring. Every figure is read from the
//! vault's account; every change is one transaction through the Solana
//! flows. `/you/agents/:id` shows [`panel`] for a Solana vault.

use std::rc::Rc;

use alloy_primitives::U256;
use chain::solana_ix::accounts::{Allowed, Vault, VaultState};
use chain::solana_ix::vault::Allowances;
use futures_signals::signal::{Mutable, SignalExt};

use super::exit::short;
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::wallet::open_connect;
use crate::money::format as money;
use crate::money::{Rounding, parse_units};
use crate::tx::solana::author::AuthorAction;
use crate::tx::solana::flow::Step;
use crate::tx::solana::run::{self, CardShows, SolAction, SolVault};
use crate::view::{El, View, el, text};
use crate::wallet::solana::SolNetwork;

#[derive(Clone, Debug, PartialEq)]
struct Read {
    vault: SolVault,
    network: Rc<SolNetwork>,
    account: Rc<Vault>,
    /// Whether the tank's escape was open on the cluster's clock when read.
    escape_open: bool,
}

/// Whether the tank's unsigned escape is open at `now`, on the cluster's
/// clock: from its delay after it was asked for, for as long again.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn escape_open(vault: &Vault, now: u64) -> bool {
    let at = vault.tank.escape_requested_at;
    let delay = vault.timers.tank_escape_delay;
    at != 0 && now >= at + delay && now <= at + 2 * delay
}

/// What the panel draws, and nothing else: it is drawn again only when this
/// changes, so a refresh that reads the same vault keeps the controls (and
/// the field being typed in) as they are.
#[derive(Clone, Debug, PartialEq)]
enum Shape {
    SignedOut,
    Reading,
    Unreadable(String),
    NotAuthor(chain::solana_ix::Pubkey),
    Controls(Rc<Read>),
}

fn shape(
    account: Option<chain::solana_ix::Pubkey>,
    loaded: &Option<Result<Read, String>>,
) -> Shape {
    match (account, loaded) {
        (None, _) => Shape::SignedOut,
        (_, None) => Shape::Reading,
        (_, Some(Err(why))) => Shape::Unreadable(why.clone()),
        (Some(account), Some(Ok(read))) if account != read.account.author => {
            Shape::NotAuthor(read.account.author)
        }
        (Some(_), Some(Ok(read))) => Shape::Controls(Rc::new(read.clone())),
    }
}

/// What the author types into the controls: kept by the panel, so the
/// controls drawn again after a read show it as typed.
#[derive(Default)]
struct Typed {
    fund: Mutable<String>,
    keep: Mutable<String>,
    shares: Mutable<String>,
    take: Mutable<String>,
    /// Fuel, memory, requests, bytes, streams, inference, gas: the vault's
    /// own values until the author types another.
    allowances: [Mutable<String>; 7],
    /// For each allowance field, the text at which it counts as untouched
    /// (it follows the vault): the vault's value it was last filled with,
    /// or, once allowances sent from it landed, what was sent.
    untouched: Untouched,
    floor: Mutable<String>,
    confirm: Mutable<bool>,
}

/// The allowance fields' untouched texts, shared with what a flow does once
/// it lands.
type Untouched = Rc<std::cell::RefCell<Option<[String; 7]>>>;

/// What to do to the fields once the flow they were sent in lands.
type Landing = Box<dyn FnOnce()>;

/// What a flow's step says of whether it landed: `None` while it runs;
/// once it is over, whether it changed (or may have changed) the vault.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn landed(step: Option<&Step>) -> Option<bool> {
    step.filter(|step| step.is_over())
        .map(|step| run::ended_changed(Some(step)))
}

impl Typed {
    /// Once the flow sent from `field` lands, empty it: what was typed was
    /// sent, and is not offered again. Text the author typed since is kept.
    fn sent_from(field: &Mutable<String>) -> Landing {
        let field = field.clone();
        let sent = field.get_cloned();
        Box::new(move || {
            if *field.lock_ref() == sent {
                field.set(String::new());
            }
        })
    }

    /// Once the allowances sent from the fields land, each field still
    /// showing what was sent follows the vault again; one the author typed
    /// into since keeps its text.
    fn sent_allowances(allowances: &[Mutable<String>; 7], untouched: &Untouched) -> Landing {
        let untouched = untouched.clone();
        let sent = allowances.clone().map(|field| field.get_cloned());
        Box::new(move || *untouched.borrow_mut() = Some(sent))
    }

    /// Once the retiring lands, the box is unticked.
    fn sent_confirm(confirm: &Mutable<bool>) -> Landing {
        let confirm = confirm.clone();
        Box::new(move || confirm.set_neq(false))
    }

    /// Fill the allowance fields from the vault's `now`: each field still
    /// at its untouched text follows the vault; one the author typed into
    /// keeps its text.
    fn fill_allowances(&self, now: Allowed) {
        let now = [
            now.fuel_per_sec.to_string(),
            now.memory_mib.to_string(),
            now.requests_per_min.to_string(),
            now.bytes_per_day.to_string(),
            now.streams.to_string(),
            now.inference_ms_per_min.to_string(),
            now.gas_per_day.to_string(),
        ];
        let mut untouched = self.untouched.borrow_mut();
        for (i, text) in now.iter().enumerate() {
            let field = &self.allowances[i];
            let follows = untouched
                .as_ref()
                .is_none_or(|untouched| field.lock_ref().as_str() == untouched[i]);
            if follows {
                field.set_neq(text.clone());
            }
        }
        *untouched = Some(now);
    }
}

/// The author's controls for `slug`'s Solana vault.
pub fn panel(slug: &str) -> View {
    let loaded: Mutable<Option<Result<Read, String>>> = Mutable::new(None);
    let session = crate::wallet::solana::get();
    let typed = Rc::new(Typed::default());
    el("div")
        .class("sh-manage sh-manage--solana")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let account = session.connection.signal_ref(|c| c.account()),
                let loaded = loaded.signal_cloned() => shape(*account, loaded)
            }
            .dedupe_cloned()
            .map(move |shape| {
                Some(match shape {
                    Shape::SignedOut => el("div")
                        .child(el("p").text("This agent’s vault is on Solana. Connect the Solana wallet that published it to manage it."))
                        .child(on_click(
                            Button::new(Kind::Primary, "Connect a Solana wallet").build(),
                            open_connect,
                        ))
                        .into(),
                    Shape::Reading => el("p").class("sh-muted").text("Reading the vault…").into(),
                    Shape::Unreadable(why) => notice(NoticeTone::Info, text(why)).into(),
                    Shape::NotAuthor(author) => notice(
                        NoticeTone::Info,
                        text(format!(
                            "Only this agent’s author ({}) can change its vault.",
                            short(&author.to_string())
                        )),
                    )
                    .into(),
                    Shape::Controls(read) => controls(&read, &typed),
                })
            }),
        )
        .hook({
            let slug = slug.to_string();
            move |dom| dom.future(follow(slug, loaded))
        })
        .into()
}

async fn follow(slug: String, loaded: Mutable<Option<Result<Read, String>>>) {
    #[cfg(target_arch = "wasm32")]
    {
        use chain::solana_ix::ids;

        use crate::tx::solana::port::SolReader;
        use crate::wallet::solana::SolSession;

        let session = crate::wallet::solana::get();
        loop {
            // Read again when a flow lands, the account changes or the
            // cluster's settings change, counting from before this read: the
            // controls are then drawn with the cluster a send would sign with.
            let seen = (
                session.changed.get(),
                session.account(),
                session.config_id(),
            );
            let result = match (
                crate::tx::solana::run::vault_of(&slug),
                session.load_network().await,
            ) {
                (Some(vault), Some(network)) => {
                    let reader = SolSession::reader(&network);
                    let now = reader.chain_time().await.unwrap_or(0);
                    match reader.account(&vault.address).await {
                        Ok(Some(data)) if data.owner == ids::VAULT => match Vault::read(&data.data)
                        {
                            Some(account) => Ok(Read {
                                vault,
                                network,
                                escape_open: escape_open(&account, now),
                                account: Rc::new(account),
                            }),
                            None => Err("This vault can’t be read.".to_string()),
                        },
                        _ => Err("The vault can’t be read right now. This page keeps trying."
                            .to_string()),
                    }
                }
                _ => Err("The Solana cluster can’t be reached right now.".to_string()),
            };
            loaded.set(Some(result));
            for _ in 0..10 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (
                    session.changed.get(),
                    session.account(),
                    session.config_id(),
                ) != seen
                {
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (slug, loaded);
}

fn field(id: &'static str, label: &'static str, value: &Mutable<String>, unit: &'static str) -> El {
    let typed = value.clone();
    el("div")
        .class("sh-field")
        .child(el("label").class("sh-field__label").attr("for", id).text(label))
        .child(
            el("div")
                .class("sh-input")
                .child(
                    el("input")
                        .attr("id", id)
                        .attr("type", "text")
                        .attr("inputmode", "decimal")
                        .attr("autocomplete", "off")
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
                .child(el("span").class("sh-input__unit").text(unit)),
        )
}

/// A button that builds its action from what's typed when pressed, runs it,
/// and shows its status in place. Once that flow (on this panel's vault,
/// started here) lands, what `make` gave to do to the fields is done.
fn act(
    read: &Rc<Read>,
    label: &'static str,
    kind: Kind,
    make: impl Fn() -> Result<(SolAction, Landing), String> + 'static,
) -> El {
    let address = read.vault.address;
    let refused: Mutable<Option<String>> = Mutable::new(None);
    let reviewed = read.network.clone();
    let network = Some(read.network.clone());
    let make = Rc::new(make);
    el("div")
        .class("sh-act")
        // Kept per account across reads: a flow under way stays shown and
        // is never offered again; one that ended shows its outcome until
        // dismissed, with the action offered again beside it.
        .child_signal(
            None,
            run::card_signal(address, label.to_string()).map({
                let refused = refused.clone();
                move |card| {
                    let shown = card.slot.get_cloned().map(|current| {
                        super::portfolio::status(&current, network.clone(), &card.slot)
                    });
                    let refused = refused.clone();
                    let make = make.clone();
                    let reviewed = reviewed.clone();
                    let card_now = card.clone();
                    let button: View = on_click(
                        Button::new(kind, label).size(Size::Small).build(),
                        move || match make() {
                            Ok((action, landing)) => {
                                refused.set(None);
                                // Built with this read's cluster: the flow
                                // signs with it or not at all.
                                if run::start_from(&card_now, address, action, reviewed.clone())
                                    && let Some(flow) = card_now.slot.get_cloned()
                                {
                                    on_landing(flow, landing);
                                }
                            }
                            Err(why) => refused.set(Some(why)),
                        },
                    )
                    .into();
                    Some(match (card.shows, shown) {
                        (CardShows::Status, Some(shown)) => shown,
                        (CardShows::StatusAndAction, Some(shown)) => {
                            View::Many(vec![shown, button])
                        }
                        _ => button,
                    })
                }
            }),
        )
        .child(
            el("p")
                .class("sh-field__hint")
                .attr("role", "status")
                .text_from(&refused, |why| why.clone().unwrap_or_default()),
        )
}

/// Do `landing` once `flow` is over having changed the vault (or maybe so);
/// nothing when it ends otherwise.
fn on_landing(flow: run::SolRunning, landing: Landing) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        use futures::StreamExt;
        let mut steps = flow.step.signal_cloned().to_stream();
        while let Some(step) = steps.next().await {
            match landed(step.as_ref()) {
                Some(true) => return landing(),
                Some(false) => return,
                None => {}
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (flow, landing);
}

fn dollars(text: &str, decimals: u8) -> Result<u64, String> {
    let amount = parse_units(text, decimals).map_err(|error| error.words())?;
    u64::try_from(amount).map_err(|_| "That’s more than a dollar account holds.".to_string())
}

fn controls(read: &Rc<Read>, typed: &Typed) -> View {
    let v = &read.account;
    let decimals = read.network.asset_decimals;
    let usd = |value: u64| money::dollars(U256::from(value), decimals, Rounding::Down);
    let row = |label: &'static str, value: String| {
        el("div")
            .class("sh-exit__row")
            .child(el("dt").text(label))
            .child(el("dd").class("sh-num").text(value))
    };

    // The tank.
    let fund = typed.fund.clone();
    let keep = typed.keep.clone();
    let shares = typed.shares.clone();
    let take = typed.take.clone();
    let escape_at = v.tank.escape_requested_at;
    let escape_open = read.escape_open;
    let tank = el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .child(el("h2").class("sh-portfolio__h2").text("Tank"))
        .child(
            el("dl")
                .class("sh-exit__rows")
                .child(row("In the tank", usd(v.tank.balance)))
                .child(row(
                    "Auto-fill keeps it at",
                    if v.tank.keep_at == 0 { "Off".to_string() } else { usd(v.tank.keep_at) },
                ))
                .child(row("Usage collected", usd(v.tank.usage_settled))),
        )
        .child(field("sol-fund", "Add to the tank", &fund, "USDC"))
        .child(act(read, "Add from your wallet", Kind::Primary, {
            let fund = fund.clone();
            move || {
                let action = SolAction::Author(AuthorAction::FundTank(dollars(&fund.get_cloned(), decimals)?));
                Ok((action, Typed::sent_from(&fund)))
            }
        }))
        .child(field("sol-topup", "Top up from your shares", &shares, "shares"))
        .child(act(read, "Redeem into the tank", Kind::Secondary, {
            let shares = shares.clone();
            move || {
                let amount = parse_units(&shares.get_cloned(), 18).map_err(|error| error.words())?;
                let amount = u128::try_from(amount).map_err(|_| "That’s more shares than exist.".to_string())?;
                Ok((SolAction::Author(AuthorAction::TopUpFromShares(amount)), Typed::sent_from(&shares)))
            }
        }))
        .child(field("sol-keep", "Keep the tank at (0 turns auto-fill off)", &keep, "USDC"))
        .child(act(read, "Set the level", Kind::Secondary, {
            let keep = keep.clone();
            move || {
                let action = SolAction::Author(AuthorAction::SetKeep(dollars(&keep.get_cloned(), decimals)?));
                Ok((action, Typed::sent_from(&keep)))
            }
        }))
        .child(field("sol-take", "Withdraw from the tank", &take, "USDC"))
        .child(act(read, "Withdraw (signed usage total)", Kind::Secondary, {
            let take = take.clone();
            move || {
                let action = SolAction::WithdrawTankSigned(dollars(&take.get_cloned(), decimals)?);
                Ok((action, Typed::sent_from(&take)))
            }
        }))
        .child(el("p").class("sh-muted").text(if escape_open {
            "The escape is open: you can withdraw without a signed usage total."
        } else if escape_at != 0 {
            "You asked to withdraw without a signed usage total. It opens after the vault’s escape delay, for as long again."
        } else {
            "If no executor signs a usage total, ask to withdraw without one. It opens after the vault’s escape delay."
        }))
        .child(if escape_open {
            act(read, "Withdraw through the escape", Kind::Secondary, {
                let take = take.clone();
                move || {
                    let action = SolAction::Author(AuthorAction::WithdrawTank {
                        assets: dollars(&take.get_cloned(), decimals)?,
                        signed: None,
                    });
                    Ok((action, Typed::sent_from(&take)))
                }
            })
        } else {
            act(read, "Ask to withdraw without a signed total", Kind::Secondary, || {
                Ok((SolAction::Author(AuthorAction::RequestEscape), Box::new(|| {}) as Landing))
            })
        });

    // Allowances.
    typed.fill_allowances(v.allowances);
    let [fuel, memory, requests, bytes, streams, inference, gas] = typed.allowances.clone();
    let sent_allowances = {
        let allowances = typed.allowances.clone();
        let untouched = typed.untouched.clone();
        move || Typed::sent_allowances(&allowances, &untouched)
    };
    let allowances = el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .child(el("h2").class("sh-portfolio__h2").text("Allowances"))
        .child(field("sol-fuel", "Fuel per second", &fuel, "fuel"))
        .child(field("sol-memory", "Memory", &memory, "MiB"))
        .child(field(
            "sol-requests",
            "Requests per minute",
            &requests,
            "/min",
        ))
        .child(field("sol-bytes", "Bytes per day", &bytes, "B"))
        .child(field("sol-streams", "Streams", &streams, ""))
        .child(field(
            "sol-inference",
            "Inference per minute",
            &inference,
            "ms",
        ))
        .child(field("sol-gas", "Gas per day", &gas, "units"))
        .child(act(read, "Set allowances", Kind::Secondary, move || {
            fn n<T: std::str::FromStr>(text: &Mutable<String>, what: &str) -> Result<T, String> {
                text.get_cloned()
                    .trim()
                    .parse()
                    .map_err(|_| format!("{what} must be a whole number."))
            }
            let action = SolAction::Author(AuthorAction::SetAllowances(Allowances {
                fuel_per_sec: n(&fuel, "Fuel per second")?,
                memory_mib: n(&memory, "Memory")?,
                requests_per_min: n(&requests, "Requests per minute")?,
                bytes_per_day: n(&bytes, "Bytes per day")?,
                streams: n(&streams, "Streams")?,
                inference_ms_per_min: n(&inference, "Inference per minute")?,
                gas_per_day: n(&gas, "Gas per day")?,
            }));
            Ok((action, sent_allowances()))
        }));

    // The idle floor.
    let floor = typed.floor.clone();
    let pending = (v.pending_floor_at != 0).then(|| {
        format!(
            "A change to {} takes effect at {}.",
            crate::format::bps(u32::from(v.pending_floor_bps)),
            crate::components::exit::utc(v.pending_floor_at)
        )
    });
    let idle = el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .child(el("h2").class("sh-portfolio__h2").text("Idle floor"))
        .child(el("dl").class("sh-exit__rows").child(row(
            "Kept as cash",
            crate::format::bps(u32::from(v.floor_bps)),
        )))
        .child(match pending {
            Some(line) => el("p").class("sh-muted").text(line).into(),
            None => View::Empty,
        })
        .child(field("sol-floor", "New floor", &floor, "%"))
        .child(act(read, "Set the floor", Kind::Secondary, {
            let floor = floor.clone();
            move || {
                // Percent with up to two decimals, as basis points.
                let bps = parse_units(&floor.get_cloned(), 2).map_err(|error| error.words())?;
                let bps = u16::try_from(bps)
                    .ok()
                    .filter(|bps| *bps <= 10_000)
                    .ok_or_else(|| {
                        "The floor is a share of the vault: at most 100%.".to_string()
                    })?;
                Ok((
                    SolAction::Author(AuthorAction::SetIdleFloor(bps)),
                    Typed::sent_from(&floor),
                ))
            }
        }));

    // Retiring.
    let confirm = typed.confirm.clone();
    let retire = el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .child(el("h2").class("sh-portfolio__h2").text("Retire"))
        .child(el("p").text("Retiring stops new deposits. The agent runs until its wind-down ends; holders withdraw as usual. It can’t be undone."))
        .child(if v.state == VaultState::Open {
            View::Many(vec![
                crate::atoms::check::checkbox(
                    "sol-retire".to_string(),
                    "I understand retiring can’t be undone.",
                    &confirm,
                )
                .into(),
                act(read, "Retire this vault", Kind::Danger, move || {
                    if confirm.get() {
                        Ok((SolAction::Author(AuthorAction::Retire), Typed::sent_confirm(&confirm)))
                    } else {
                        Err("Tick the box to retire.".to_string())
                    }
                })
                .into(),
            ])
        } else {
            el("p").class("sh-muted").text("This vault isn’t open.").into()
        });

    el("div")
        .class("sh-manage__grid")
        .child(tank)
        .child(allowances)
        .child(idle)
        .child(retire)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::config_cache::ConfigId;
    use chain::solana_ix::Pubkey;

    fn read(vault: Vault, escape_open: bool) -> Read {
        Read {
            vault: SolVault {
                slug: "solana-4".to_string(),
                title: "Basis".to_string(),
                address: Pubkey::new_from_array([3; 32]),
            },
            network: Rc::new(SolNetwork {
                cluster: "devnet".to_string(),
                label: "Solana devnet".to_string(),
                rpc: "https://api.devnet.solana.com".to_string(),
                mint: Pubkey::new_from_array([7; 32]),
                asset_symbol: "USDC".to_string(),
                asset_decimals: 6,
                dev_account: None,
                config: ConfigId(alloy_primitives::B256::repeat_byte(1)),
            }),
            account: Rc::new(vault),
            escape_open,
        }
    }

    #[test]
    fn a_refresh_that_reads_the_same_vault_keeps_the_controls_and_a_changed_one_redraws_them() {
        let vault = crate::tx::solana::sim::a_vault();
        let author = vault.author;
        let first = shape(Some(author), &Some(Ok(read(vault.clone(), false))));
        // Read again ten seconds later: the same vault, the same escape.
        let again = shape(Some(author), &Some(Ok(read(vault.clone(), false))));
        assert_eq!(first, again);
        // The tank changed, or the escape opened: drawn again.
        let mut topped = vault.clone();
        topped.tank.balance += 1;
        assert_ne!(first, shape(Some(author), &Some(Ok(read(topped, false)))));
        assert_ne!(first, shape(Some(author), &Some(Ok(read(vault, true)))));
    }

    #[test]
    fn what_is_typed_survives_a_redraw_and_an_untouched_allowance_follows_the_vault() {
        let vault = crate::tx::solana::sim::a_vault();
        let typed = Rc::new(Typed::default());
        let _ = controls(&Rc::new(read(vault.clone(), false)), &typed);
        typed.fund.set("12.5".to_string());
        typed.floor.set("7".to_string());
        let fuel = typed.allowances[0].get_cloned();
        assert_eq!(fuel, vault.allowances.fuel_per_sec.to_string());
        typed.allowances[1].set("999".to_string());
        // The vault's allowances changed (the author set them): a field
        // left as it was read follows them; one typed in keeps its text.
        let mut set = vault.clone();
        set.allowances.fuel_per_sec += 5;
        set.allowances.memory_mib += 5;
        let _ = controls(&Rc::new(read(set.clone(), false)), &typed);
        assert_eq!(typed.fund.get_cloned(), "12.5");
        assert_eq!(typed.floor.get_cloned(), "7");
        assert_eq!(
            typed.allowances[0].get_cloned(),
            set.allowances.fuel_per_sec.to_string()
        );
        assert_eq!(typed.allowances[1].get_cloned(), "999");
    }

    #[test]
    fn a_landed_flow_empties_only_the_fields_it_was_sent_from_and_keeps_what_was_typed_since() {
        let vault = crate::tx::solana::sim::a_vault();
        let typed = Rc::new(Typed::default());
        let _ = controls(&Rc::new(read(vault.clone(), false)), &typed);
        typed.fund.set("12.5".to_string());
        typed.floor.set("7".to_string());
        typed.take.set("3".to_string());
        // Funding the tank lands: its field is emptied, the others stay.
        let landing = Typed::sent_from(&typed.fund);
        landing();
        assert_eq!(typed.fund.get_cloned(), "");
        assert_eq!(typed.floor.get_cloned(), "7");
        assert_eq!(typed.take.get_cloned(), "3");
        // A withdrawal sent for 3, and 4 typed before it landed: kept.
        let landing = Typed::sent_from(&typed.take);
        typed.take.set("4".to_string());
        landing();
        assert_eq!(typed.take.get_cloned(), "4");

        // Allowances sent: fuel and memory typed and sent; memory typed
        // again before the flow landed.
        typed.allowances[0].set("111".to_string());
        typed.allowances[1].set("222".to_string());
        let landing = Typed::sent_allowances(&typed.allowances, &typed.untouched);
        typed.allowances[1].set("333".to_string());
        landing();
        // The vault read after it landed holds what was sent.
        let mut set = vault.clone();
        set.allowances.fuel_per_sec = 111;
        set.allowances.memory_mib = 222;
        set.allowances.streams += 1;
        let _ = controls(&Rc::new(read(set.clone(), false)), &typed);
        assert_eq!(typed.allowances[0].get_cloned(), "111");
        assert_eq!(typed.allowances[1].get_cloned(), "333");
        assert_eq!(
            typed.allowances[4].get_cloned(),
            set.allowances.streams.to_string()
        );
        // Sent fuel follows the vault from then on.
        let mut later = set.clone();
        later.allowances.fuel_per_sec = 150;
        let _ = controls(&Rc::new(read(later, false)), &typed);
        assert_eq!(typed.allowances[0].get_cloned(), "150");
        assert_eq!(typed.allowances[1].get_cloned(), "333");
        assert_eq!(typed.floor.get_cloned(), "7");
    }

    #[test]
    fn the_fields_are_let_go_only_once_their_flow_is_over_having_changed_the_vault() {
        use crate::tx::solana::error::SolError;
        use crate::tx::solana::flow::Outcome;
        use crate::tx::solana::port::Sig;
        let signature = Sig([3; 64]);
        assert_eq!(landed(None), None);
        assert_eq!(landed(Some(&Step::Checking)), None);
        assert_eq!(
            landed(Some(&Step::Done(Outcome::Changed { signature }))),
            Some(true)
        );
        assert_eq!(
            landed(Some(&Step::Failed(SolError::Stuck { signature }))),
            Some(true)
        );
        assert_eq!(landed(Some(&Step::Failed(SolError::Altered))), Some(false));
    }
}
