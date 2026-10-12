//! The author's controls for a vault on Solana (Manage-*): the tank (fund
//! it, top it up from the author's shares, the level auto-fill keeps, withdrawing
//! with the executor's signed total or through the unsigned escape), the
//! allowances, the idle floor, and retiring. Every figure is read from the
//! vault's account; every change is one transaction through the Solana
//! flows. `/you/agents/:id` shows [`panel`] for a Solana vault.

use std::rc::Rc;

use alloy_primitives::U256;
use chain::solana_ix::accounts::{Vault, VaultState};
use chain::solana_ix::vault::Allowances;
use futures_signals::signal::{Mutable, SignalExt};

use super::exit::short;
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::wallet::open_connect;
use crate::money::format as money;
use crate::money::{Rounding, parse_units};
use crate::tx::solana::author::AuthorAction;
use crate::tx::solana::run::{SolAction, SolRunning, SolVault};
use crate::view::{El, View, el, text};
use crate::wallet::solana::SolNetwork;

#[derive(Clone)]
struct Read {
    vault: SolVault,
    network: Rc<SolNetwork>,
    account: Rc<Vault>,
    now: u64,
}

/// The author's controls for `slug`'s Solana vault.
pub fn panel(slug: &str) -> View {
    let loaded: Mutable<Option<Result<Read, String>>> = Mutable::new(None);
    let session = crate::wallet::solana::get();
    el("div")
        .class("sh-manage sh-manage--solana")
        .child_signal(
            None,
            futures_signals::map_ref! {
                let account = session.connection.signal_ref(|c| c.account()),
                let loaded = loaded.signal_cloned() => (*account, loaded.clone())
            }
            .map(|(account, loaded)| {
                Some(match (account, loaded) {
                    (None, _) => el("div")
                        .child(el("p").text("This agent’s vault is on Solana. Connect the Solana wallet that published it to manage it."))
                        .child(on_click(
                            Button::new(Kind::Primary, "Connect a Solana wallet").build(),
                            open_connect,
                        ))
                        .into(),
                    (_, None) => el("p").class("sh-muted").text("Reading the vault…").into(),
                    (_, Some(Err(why))) => notice(NoticeTone::Info, text(why)).into(),
                    (Some(account), Some(Ok(read))) if account != read.account.author => {
                        notice(
                            NoticeTone::Info,
                            text(format!(
                                "Only this agent’s author ({}) can change its vault.",
                                short(&read.account.author.to_string())
                            )),
                        )
                        .into()
                    }
                    (Some(_), Some(Ok(read))) => controls(&Rc::new(read)),
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
                                account: Rc::new(account),
                                now,
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
/// and shows its status in place.
fn act(
    read: &Rc<Read>,
    label: &'static str,
    kind: Kind,
    make: impl Fn() -> Result<SolAction, String> + 'static,
) -> El {
    let running: Mutable<Option<SolRunning>> = Mutable::new(None);
    let refused: Mutable<Option<String>> = Mutable::new(None);
    let address = read.vault.address;
    let network = Some(read.network.clone());
    let make = Rc::new(make);
    el("div")
        .class("sh-act")
        .child_signal(
            None,
            running.signal_cloned().map({
                let running = running.clone();
                let refused = refused.clone();
                move |current| {
                    if let Some(current) = current {
                        return Some(super::portfolio::status(
                            &current,
                            network.clone(),
                            &running,
                        ));
                    }
                    let running = running.clone();
                    let refused = refused.clone();
                    let make = make.clone();
                    Some(
                        on_click(
                            Button::new(kind, label).size(Size::Small).build(),
                            move || match make() {
                                Ok(action) => {
                                    refused.set(None);
                                    let flow = SolRunning::new();
                                    crate::tx::solana::run::start(address, action, &flow);
                                    running.set(Some(flow));
                                }
                                Err(why) => refused.set(Some(why)),
                            },
                        )
                        .into(),
                    )
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

fn dollars(text: &str, decimals: u8) -> Result<u64, String> {
    let amount = parse_units(text, decimals).map_err(|error| error.words())?;
    u64::try_from(amount).map_err(|_| "That’s more than a dollar account holds.".to_string())
}

fn controls(read: &Rc<Read>) -> View {
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
    let fund = Mutable::new(String::new());
    let keep = Mutable::new(String::new());
    let shares = Mutable::new(String::new());
    let take = Mutable::new(String::new());
    let escape_at = v.tank.escape_requested_at;
    let delay = v.timers.tank_escape_delay;
    let escape_open =
        escape_at != 0 && read.now >= escape_at + delay && read.now <= escape_at + 2 * delay;
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
            move || Ok(SolAction::Author(AuthorAction::FundTank(dollars(&fund.get_cloned(), decimals)?)))
        }))
        .child(field("sol-topup", "Top up from your shares", &shares, "shares"))
        .child(act(read, "Redeem into the tank", Kind::Secondary, {
            let shares = shares.clone();
            move || {
                let amount = parse_units(&shares.get_cloned(), 18).map_err(|error| error.words())?;
                let amount = u128::try_from(amount).map_err(|_| "That’s more shares than exist.".to_string())?;
                Ok(SolAction::Author(AuthorAction::TopUpFromShares(amount)))
            }
        }))
        .child(field("sol-keep", "Keep the tank at (0 turns auto-fill off)", &keep, "USDC"))
        .child(act(read, "Set the level", Kind::Secondary, {
            let keep = keep.clone();
            move || Ok(SolAction::Author(AuthorAction::SetKeep(dollars(&keep.get_cloned(), decimals)?)))
        }))
        .child(field("sol-take", "Withdraw from the tank", &take, "USDC"))
        .child(act(read, "Withdraw (signed usage total)", Kind::Secondary, {
            let take = take.clone();
            move || Ok(SolAction::WithdrawTankSigned(dollars(&take.get_cloned(), decimals)?))
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
                    Ok(SolAction::Author(AuthorAction::WithdrawTank {
                        assets: dollars(&take.get_cloned(), decimals)?,
                        signed: None,
                    }))
                }
            })
        } else {
            act(read, "Ask to withdraw without a signed total", Kind::Secondary, || {
                Ok(SolAction::Author(AuthorAction::RequestEscape))
            })
        });

    // Allowances.
    let a = v.allowances;
    let fuel = Mutable::new(a.fuel_per_sec.to_string());
    let memory = Mutable::new(a.memory_mib.to_string());
    let requests = Mutable::new(a.requests_per_min.to_string());
    let bytes = Mutable::new(a.bytes_per_day.to_string());
    let streams = Mutable::new(a.streams.to_string());
    let inference = Mutable::new(a.inference_ms_per_min.to_string());
    let gas = Mutable::new(a.gas_per_day.to_string());
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
            Ok(SolAction::Author(AuthorAction::SetAllowances(Allowances {
                fuel_per_sec: n(&fuel, "Fuel per second")?,
                memory_mib: n(&memory, "Memory")?,
                requests_per_min: n(&requests, "Requests per minute")?,
                bytes_per_day: n(&bytes, "Bytes per day")?,
                streams: n(&streams, "Streams")?,
                inference_ms_per_min: n(&inference, "Inference per minute")?,
                gas_per_day: n(&gas, "Gas per day")?,
            })))
        }));

    // The idle floor.
    let floor = Mutable::new(String::new());
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
                Ok(SolAction::Author(AuthorAction::SetIdleFloor(bps)))
            }
        }));

    // Retiring.
    let confirm = Mutable::new(false);
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
                        Ok(SolAction::Author(AuthorAction::Retire))
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
