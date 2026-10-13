//! `/you/agents/:id` (Manage-Desktop, Manage-Mobile): an agent's controls,
//! for its Author only. Whether the connected wallet is the Author is read
//! from the vault itself; any other wallet is sent to the public agent page.
//! Everything a control acts on is read from the chain (the tank, auto-fill,
//! the allowances, the idle floor and any lowering waiting, the Author's
//! shares) and read again after every change and every 15 seconds.

pub mod list;
pub mod retire;

use std::rc::Rc;

use alloy_primitives::{Address, U256};
use futures_signals::signal::{Mutable, SignalExt};
use shared::Chain;
use ui::icons::Icon;

use super::Page;
use super::publish::activate::{allowance_grid, allowance_label};
use super::publish::parts::{self, Names, field};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::wallet::open_connect;
use crate::money::format as money;
use crate::money::{Rounding, mul_div, units::pow10};
use crate::tx::author::error::utc;
use crate::tx::author::form::{self, AllowanceField, AllowanceText, Allowances};
use crate::tx::author::read::{AuthorVault, Escape};
use crate::tx::author::run::{Action, Running};
use crate::tx::read::State;
use crate::view::{El, View, el, text};
use crate::wallet::Network;
use crate::web::app_link;

/// The vault as read for its Author, with what it costs a day.
#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct Read {
    pub vault: Address,
    pub chain: Chain,
    pub title: String,
    pub network: Rc<Network>,
    pub at: AuthorVault,
    /// The most a day at the allowances costs, at today's prices.
    pub daily: Option<U256>,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl Read {
    /// Whether two reads show the same: the chain's clock aside, unless a
    /// waiting lowering of the floor took effect between them.
    pub fn same(&self, other: &Read) -> bool {
        let quiet = |at: &AuthorVault| AuthorVault {
            now: 0,
            ..at.clone()
        };
        quiet(&self.at) == quiet(&other.at)
            && self.daily == other.daily
            && self.title == other.title
            && self.at.floor.waiting(self.at.now).is_some()
                == other.at.floor.waiting(other.at.now).is_some()
            && self.at.escape() == other.at.escape()
    }
}

#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum Load {
    /// Waiting for the agent to appear in the store, the wallet, or the chain.
    Loading,
    SignedOut,
    Failed(String),
    Ready(Rc<Read>),
}

/// Each control's own fields and flow, so each status shows in place.
struct Controls {
    top_up: Mutable<String>,
    take: Mutable<String>,
    shares: Mutable<String>,
    keep: Mutable<String>,
    floor: Mutable<String>,
    editing_floor: Mutable<bool>,
    editing_allowances: Mutable<bool>,
    allowance_text: [Mutable<String>; 7],
    tank: Mutable<Option<Running>>,
    autofill: Mutable<Option<Running>>,
    shares_flow: Mutable<Option<Running>>,
    allowances: Mutable<Option<Running>>,
    idle: Mutable<Option<Running>>,
    escape: Mutable<Option<Running>>,
    retire: Mutable<Option<Running>>,
    /// A refusal under a control before anything is asked.
    keep_error: Mutable<Option<String>>,
}

impl Controls {
    fn new() -> Self {
        Self {
            top_up: Mutable::new(String::new()),
            take: Mutable::new(String::new()),
            shares: Mutable::new(String::new()),
            keep: Mutable::new(String::new()),
            floor: Mutable::new(String::new()),
            editing_floor: Mutable::new(false),
            editing_allowances: Mutable::new(false),
            allowance_text: Default::default(),
            tank: Mutable::new(None),
            autofill: Mutable::new(None),
            shares_flow: Mutable::new(None),
            allowances: Mutable::new(None),
            idle: Mutable::new(None),
            escape: Mutable::new(None),
            retire: Mutable::new(None),
            keep_error: Mutable::new(None),
        }
    }

    fn fill_allowances(&self, allowances: &Allowances, decimals: u8) {
        let text = AllowanceText::of(allowances, decimals);
        for (slot, value) in self.allowance_text.iter().zip([
            text.fuel_millions,
            text.memory_mib,
            text.requests_per_min,
            text.data_mb,
            text.streams,
            text.inference_ms,
            text.gas_dollars,
        ]) {
            slot.set(value);
        }
    }

    fn typed_allowances(&self) -> AllowanceText {
        let get = |at: usize| self.allowance_text[at].get_cloned();
        AllowanceText {
            fuel_millions: get(0),
            memory_mib: get(1),
            requests_per_min: get(2),
            data_mb: get(3),
            streams: get(4),
            inference_ms: get(5),
            gas_dollars: get(6),
        }
    }
}

pub fn page(slug: &str) -> Page {
    // A Solana vault is managed with a Solana wallet, from its own account.
    if crate::tx::solana::run::is_solana(slug) {
        // The title once the store has the agent: a direct load arrives
        // before the first snapshot.
        let title_of = |slug: &str| {
            crate::store::get()
                .agent(slug)
                .map(|agent| agent.strategy.lock_ref().title.clone())
                .unwrap_or_else(|| "Manage your agent".to_string())
        };
        let store = crate::store::get();
        let asked = slug.to_string();
        let title = futures_signals::map_ref! {
            let _membership = store.membership.signal(),
            let _loaded = store.loaded.signal() => title_of(&asked)
        }
        .dedupe_cloned();
        let body = el("div")
            .class("sh-author sh-manage")
            .child(
                el("h1")
                    .class("sh-author__title")
                    .text_signal(title_of(slug), title),
            )
            .child(crate::components::solana::manage::panel(slug));
        return Page {
            top: None,
            body: body.into(),
        };
    }
    let load = Mutable::new(Load::Loading);
    let controls = Rc::new(Controls::new());
    let follow = load.clone();
    let follow_slug = slug.to_string();
    let view_slug = slug.to_string();
    let body = el("div")
        .class("sh-author sh-manage")
        .child_signal(
            Some(loading()),
            load.signal_cloned().map(move |load| {
                Some(match load {
                    Load::Loading => loading(),
                    Load::SignedOut => signed_out(&view_slug),
                    Load::Failed(why) => notice(NoticeTone::Danger, text(why))
                        .attr("role", "alert")
                        .into(),
                    Load::Ready(read) => manage(&view_slug, &read, &controls),
                })
            }),
        )
        .hook(move |dom| dom.future(watch(follow_slug, follow)));
    Page {
        top: None,
        body: body.into(),
    }
}

fn loading() -> View {
    el("div")
        .class("sh-job__loading")
        .attr("aria-busy", "true")
        .child(crate::atoms::skeleton::skeleton_block("50%", 40))
        .child(crate::atoms::skeleton::skeleton_block("100%", 240))
        .into()
}

fn signed_out(slug: &str) -> View {
    el("div")
        .class("sh-card sh-card--pad sh-job__card")
        .child(el("h1").class("sh-author__title").text("Manage your agent"))
        .child(el("p").text(
            "These controls are for the agent's Author. Connect the wallet that published it.",
        ))
        .child(
            el("div")
                .class("sh-job__actions")
                .child(
                    on_click(
                        Button::new(Kind::Primary, "Connect wallet").build(),
                        open_connect,
                    )
                    .attr("aria-haspopup", "dialog"),
                )
                .child(
                    Button::new(Kind::Secondary, "View public page")
                        .href(format!("/agents/{slug}"))
                        .build(),
                ),
        )
        .into()
}

/// Read the vault for its Author now, whenever the account changes or a
/// flow lands, and every 15 s. Anyone but the Author goes to the public page.
async fn watch(slug: String, load: Mutable<Load>) {
    #[cfg(target_arch = "wasm32")]
    {
        let session = crate::wallet::get();
        loop {
            let seen = (session.changed.get(), session.account());
            match seen.1 {
                None => load.set(Load::SignedOut),
                Some(account) => match read(&slug, account).await {
                    Ok(Some(read)) if read.at.author == account => {
                        // Redrawn only when something shown changed, so a
                        // field being typed in keeps its focus.
                        let changed = match &*load.lock_ref() {
                            Load::Ready(seen) => !seen.same(&read),
                            _ => true,
                        };
                        if session.account() == Some(account) && changed {
                            load.set(Load::Ready(Rc::new(read)));
                        }
                    }
                    Ok(Some(_)) => {
                        // Not the Author: the public page is theirs.
                        dominator::routing::go_to_url(&format!("/agents/{slug}"));
                        return;
                    }
                    Ok(None) => {}
                    Err(why) => {
                        if !matches!(load.get_cloned(), Load::Ready(_)) {
                            load.set(Load::Failed(why));
                        }
                    }
                },
            }
            for _ in 0..15 {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
                if (session.changed.get(), session.account()) != seen {
                    break;
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (slug, load);
}

/// The vault for `account`; `None` while the agent isn't in the store yet.
#[cfg(target_arch = "wasm32")]
async fn read(slug: &str, account: Address) -> Result<Option<Read>, String> {
    use crate::tx::author::read as author;
    use crate::wallet::session::Session;
    let Some(agent) = crate::store::get().agent(slug) else {
        return Ok(None);
    };
    let strategy = agent.strategy.get_cloned();
    let chain = strategy.vault.chain;
    let vault: Address = strategy
        .vault
        .address
        .parse()
        .map_err(|_| "This agent's vault isn't an address this app can act on.".to_string())?;
    let session = crate::wallet::get();
    let (Some(network), Some(platform)) = (
        session.network(chain).await,
        crate::tx::author::run::platform(chain).await,
    ) else {
        return Err(format!(
            "{} isn't reachable from the app right now.",
            chain.label()
        ));
    };
    let reader = Session::reader(&network);
    let at = author::author_vault(&reader, vault, account)
        .await
        .map_err(|_| "The chain didn't answer. This page keeps asking.".to_string())?;
    let daily = author::daily_cost(&reader, platform.registries.prices, &at.allowances)
        .await
        .ok();
    Ok(Some(Read {
        vault,
        chain,
        title: agent.title.get_cloned(),
        network,
        at,
        daily,
    }))
}

fn state_chip(state: State) -> El {
    match state {
        State::Open => badge(Tone::Gain, Some(Icon::Check), "Live"),
        State::Activating => badge(Tone::Neutral, Some(Icon::Clock), "Opening"),
        State::WindingDown => badge(Tone::Caution, Some(Icon::Clock), "Winding down"),
        State::Frozen => badge(Tone::Caution, Some(Icon::Pause), "Frozen"),
        State::Killed => badge(Tone::Loss, Some(Icon::Serious), "Stopped"),
        State::Closed => badge(Tone::Neutral, None, "Closed"),
        State::Cancelled => badge(Tone::Neutral, None, "Cancelled"),
        State::None => badge(Tone::Neutral, None, "Not open"),
    }
}

fn manage(slug: &str, read: &Rc<Read>, controls: &Rc<Controls>) -> View {
    let names = Rc::new(Names::of(&read.network));
    let decimals = read.network.asset_decimals;
    let head = el("div")
        .class("sh-author__head")
        .child(
            el("div")
                .class("sh-author__headtext")
                .child(
                    el("div")
                        .class("sh-job__titlerow")
                        .child(el("h1").class("sh-author__title").text(read.title.clone()))
                        .child(state_chip(read.at.state)),
                )
                .child(el("p").class("sh-muted").text(format!(
                    "Vault on {} ({})",
                    read.chain.label(),
                    read.network.asset_symbol
                ))),
        )
        .child(
            el("div")
                .class("sh-author__links")
                .child(
                    Button::new(Kind::Quiet, "Publish another")
                        .size(Size::Small)
                        .icon_before(Icon::Plus)
                        .href("/publish")
                        .build(),
                )
                .child(
                    Button::new(Kind::Secondary, "View public page")
                        .size(Size::Small)
                        .href(format!("/agents/{slug}"))
                        .build(),
                ),
        );
    let vault = el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .attr("aria-labelledby", "m-vault")
        .child(
            el("h2")
                .attr("id", "m-vault")
                .class("sh-publish__h2")
                .text("Vault"),
        )
        .child(
            el("dl")
                .class("sh-publish__dl")
                .child(parts::row(
                    "Share price",
                    parts::words(money::price(read.at.share_price, decimals)),
                ))
                .child(parts::row(
                    "Cash at home",
                    parts::words(money::dollars(read.at.cash, decimals, Rounding::Down)),
                )),
        );
    el("div")
        .class("sh-manage__body")
        .child(head)
        .child(
            el("div")
                .class("sh-manage__grid")
                .child(
                    el("div")
                        .class("sh-manage__main")
                        .child(tank_card(read, controls, &names))
                        .child(allowances_card(read, controls, &names)),
                )
                .child(
                    el("div")
                        .class("sh-manage__side")
                        .child(shares_card(slug, read, controls, &names))
                        .child(floor_card(read, controls, &names))
                        .child(vault)
                        .child(retire_card(read, controls, &names)),
                ),
        )
        .into()
}

/// Start `action` for this vault into `slot`, unless a flow there is live.
fn start(read: &Read, slot: &Mutable<Option<Running>>, action: Action) {
    if slot.lock_ref().as_ref().is_some_and(Running::is_live) {
        return;
    }
    let running = Running::new();
    slot.set(Some(running.clone()));
    crate::tx::author::run::start(read.chain, action, &running, |_| {});
}

/// A refusal before anything is asked, said where the flow's status goes.
fn refuse(slot: &Mutable<Option<Running>>, why: String) {
    let running = Running::new();
    running.step.set(Some(crate::tx::author::Step::Failed(
        crate::tx::TxError::Network(why).into(),
    )));
    slot.set(Some(running));
}

fn status_slot(slot: &Mutable<Option<Running>>, names: &Rc<Names>) -> El {
    let names = names.clone();
    el("div").child_signal(
        None,
        slot.signal_cloned().map(move |running| {
            running.map(|running| parts::status(&running, names.clone()).into())
        }),
    )
}

/// A field and its button on one row.
fn action_row(input: El, button: El) -> El {
    el("div")
        .class("sh-manage__action")
        .child(input)
        .child(button)
}

fn tank_card(read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> El {
    let decimals = read.network.asset_decimals;
    let tank = read.at.tank;
    let runway = match read.daily {
        Some(daily) => match form::runway_days(tank.balance, daily) {
            Some(days) => format!(
                "At least {days} days of running left: the allowances can cost at most {} a day.",
                money::dollars_exact(daily, decimals)
            ),
            None => "Nothing is charged at these allowances.".to_string(),
        },
        None => String::new(),
    };
    let low = read
        .daily
        .is_some_and(|daily| form::covers_less_than(tank.balance, daily, 7));
    let keeping = !tank.keep_at.is_zero();
    if controls.keep.get_cloned().is_empty() && keeping {
        controls
            .keep
            .set(money::plain_exact(tank.keep_at, decimals));
    }

    // Auto-fill: a switch that sends the change, and shows the chain's state.
    let switch_read = read.clone();
    let switch_controls = controls.clone();
    let autofill = el("button")
        .attr("type", "button")
        .class("sh-switch sh-manage__switch")
        .attr("role", "switch")
        .attr("aria-checked", keeping.to_string())
        .attr("aria-describedby", "m-keep-hint")
        .attr_signal(
            "disabled",
            None,
            controls
                .autofill
                .signal_cloned()
                .map(|running| running.filter(Running::is_live).map(|_| String::new())),
        )
        .child(
            el("span")
                .class("sh-switch__text")
                .child(el("span").class("sh-switch__title").text("Auto-fill"))
                .child(
                    el("span")
                        .class("sh-switch__detail")
                        .text("Tops the tank back up from your shares once a day."),
                ),
        )
        .child(
            el("span")
                .class("sh-switch__state")
                .attr("aria-hidden", "true")
                .text(if keeping { "On" } else { "Off" }),
        )
        .child(
            el("span")
                .class("sh-switch__track")
                .attr("aria-hidden", "true")
                .child(el("span").class("sh-switch__knob")),
        )
        .hook(move |dom| {
            dom.event(move |_: dominator::events::Click| {
                let read = &switch_read;
                let controls = &switch_controls;
                let amount = if keeping {
                    Some(U256::ZERO)
                } else {
                    match form::positive_up_to(&controls.keep.get_cloned(), decimals, None, "") {
                        Ok(level) => Some(level),
                        Err(why) => {
                            controls
                                .keep_error
                                .set(Some(format!("Set a level first. {why}")));
                            None
                        }
                    }
                };
                if let Some(amount) = amount {
                    controls.keep_error.set(None);
                    start(
                        read,
                        &controls.autofill,
                        Action::SetKeep {
                            vault: read.vault,
                            amount,
                        },
                    );
                }
            })
        });

    let top_read = read.clone();
    let top_controls = controls.clone();
    let take_read = read.clone();
    let take_controls = controls.clone();
    el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .attr("aria-labelledby", "m-tank")
        .child(el("h2").attr("id", "m-tank").class("sh-publish__h2").text("Tank"))
        .child(
            el("p")
                .class("sh-manage__figure")
                .text(money::dollars(tank.balance, decimals, Rounding::Down)),
        )
        .child({
            let line: View = if low {
            notice(
                NoticeTone::Caution,
                text(format!("{runway} Top it up so your agent keeps running.")),
            )
            .into()
        } else {
            el("p").class("sh-muted").text(runway).into()
        };
            line
        })
        .child(el("p").class("sh-muted sh-author__small").text(
            "Computing and gas, collected once a day.",
        ))
        .child(autofill)
        .child(field(
            "m-keep",
            "Keep my tank at",
            Some("$"),
            Some(read.network.asset_symbol.clone()),
            &controls.keep,
            if keeping {
                format!(
                    "On: the tank is topped back up to {} once a day, the first fill with the next daily collection.",
                    money::dollars_exact(tank.keep_at, decimals)
                )
            } else {
                "Type a level, then turn auto-fill on.".to_string()
            },
            controls.keep_error.signal_cloned(),
        ))
        .child(if keeping {
            let read = read.clone();
            let controls = controls.clone();
            on_click(
                Button::new(Kind::Secondary, "Save level").size(Size::Small).build(),
                move || match form::positive_up_to(&controls.keep.get_cloned(), decimals, None, "") {
                    Ok(amount) => {
                        controls.keep_error.set(None);
                        start(&read, &controls.autofill, Action::SetKeep {
                            vault: read.vault,
                            amount,
                        });
                    }
                    Err(why) => controls.keep_error.set(Some(why)),
                },
            )
            .into()
        } else {
            View::Empty
        })
        .child(status_slot(&controls.autofill, names))
        .child(
            el("div")
                .class("sh-manage__split")
                .child(action_row(
                    field(
                        "m-topup",
                        "Top up tank",
                        Some("$"),
                        Some(read.network.asset_symbol.clone()),
                        &controls.top_up,
                        "From your wallet. An approval for exactly this, if needed.".to_string(),
                        futures_signals::signal::always(None),
                    ),
                    on_click(
                        Button::new(Kind::Primary, "Top up").build(),
                        move || match form::positive_up_to(
                            &top_controls.top_up.get_cloned(),
                            decimals,
                            None,
                            "",
                        ) {
                            Ok(amount) => start(&top_read, &top_controls.tank, Action::FundTank {
                                vault: top_read.vault,
                                amount,
                            }),
                            Err(why) => refuse(&top_controls.tank, why),
                        },
                    ),
                ))
                .child(action_row(
                    field(
                        "m-take",
                        "Take money out",
                        Some("$"),
                        Some(read.network.asset_symbol.clone()),
                        &controls.take,
                        "The bill so far is paid from the tank first. You're asked for no more than what it leaves, and the wallet names what you'll get.".to_string(),
                        futures_signals::signal::always(None),
                    ),
                    on_click(
                        Button::new(Kind::Secondary, "Withdraw").build(),
                        move || match form::positive_up_to(
                            &take_controls.take.get_cloned(),
                            decimals,
                            Some(take_read.at.tank.balance),
                            "That's more than the tank holds.",
                        ) {
                            Ok(amount) => start(&take_read, &take_controls.tank, Action::WithdrawTank {
                                vault: take_read.vault,
                                amount,
                            }),
                            Err(why) => refuse(&take_controls.tank, why),
                        },
                    ),
                )),
        )
        .child(status_slot(&controls.tank, names))
        .child(escape_box(read, controls, names))
}

/// The unsigned tank escape: for when no executor is running to sign a bill.
fn escape_box(read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> El {
    let decimals = read.network.asset_decimals;
    let escape = read.at.escape();
    let line = match escape {
        Escape::NotAsked => "If your agent's executor stops, nobody can sign a bill. Ask, wait, then withdraw without one; the wait lets anyone collect a final bill first.".to_string(),
        Escape::Waiting { ready_at } => format!("Asked. You can withdraw without a signed bill from {}.", utc(ready_at)),
        Escape::Open { until } => format!("Open: you can withdraw without a signed bill until {}.", utc(until)),
        Escape::Lapsed => "Your last request lapsed. Ask again, then wait once more.".to_string(),
        Escape::Free => "The vault has closed: its tank comes back to you without a bill.".to_string(),
    };
    let ask_read = read.clone();
    let ask_controls = controls.clone();
    let out_read = read.clone();
    let out_controls = controls.clone();
    let balance = read.at.tank.balance;
    let action: View = match escape {
        Escape::NotAsked | Escape::Lapsed => on_click(
            Button::new(Kind::Secondary, "Ask to withdraw without a bill")
                .size(Size::Small)
                .build(),
            move || {
                start(
                    &ask_read,
                    &ask_controls.escape,
                    Action::RequestEscape {
                        vault: ask_read.vault,
                    },
                )
            },
        )
        .into(),
        Escape::Open { .. } | Escape::Free => on_click(
            Button::new(
                Kind::Secondary,
                format!(
                    "Withdraw {} without a bill",
                    money::dollars_exact(balance, decimals)
                ),
            )
            .size(Size::Small)
            .disabled(balance.is_zero())
            .build(),
            move || {
                start(
                    &out_read,
                    &out_controls.escape,
                    Action::WithdrawUnsigned {
                        vault: out_read.vault,
                        amount: out_read.at.tank.balance,
                    },
                )
            },
        )
        .into(),
        Escape::Waiting { .. } => View::Empty,
    };
    el("details")
        .class("sh-manage__escape")
        .attr_if(!matches!(escape, Escape::NotAsked), "open", "")
        .child(el("summary").text("If the executor stops: withdraw without a signed bill"))
        .child(el("p").class("sh-muted").text(line))
        .child(action)
        .child(status_slot(&controls.escape, names))
}

/// What the shares card says of where the author's shares come from: their
/// profit fee is paid as new shares, which join any they deposited for.
const SHARES_NOTE: &str = "Your share of new profit is paid every Monday as new shares, and they join these. Any shares you deposited for count here too: the vault doesn't keep the two apart.";

/// Every share the author holds. The vault keeps no count of which came as
/// their profit fee and which they bought with a deposit, so neither part is
/// shown, and none of it is called earnings.
fn shares_card(slug: &str, read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> El {
    let decimals = read.network.asset_decimals;
    let shares = read.at.shares;
    let worth = mul_div(shares, read.at.share_price, pow10(18)).unwrap_or_default();
    let pct = form::percent(read.at.author_bps);
    let fill_read = read.clone();
    let fill_controls = controls.clone();
    el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .attr("aria-labelledby", "m-own-shares")
        .child(
            el("h2")
                .attr("id", "m-own-shares")
                .class("sh-publish__h2")
                .text("Your shares"),
        )
        .child(
            el("p")
                .class("sh-manage__figure")
                .text(format!("{} shares", money::shares(shares, Rounding::Down))),
        )
        .child(el("p").class("sh-muted").text(format!(
            "≈ {} at {} a share",
            money::dollars(worth, decimals, Rounding::Nearest),
            money::price(read.at.share_price, decimals)
        )))
        .child(el("dl").class("sh-publish__dl").child(parts::row(
            "Your share",
            parts::words(format!("{pct} of new profit")),
        )))
        .child(el("p").class("sh-muted sh-author__small").text(SHARES_NOTE))
        .child(action_row(
            field(
                "m-shares",
                "Turn shares into tank money",
                None,
                Some("shares".to_string()),
                &controls.shares,
                "Priced at the next valuation, like any withdrawal.".to_string(),
                futures_signals::signal::always(None),
            ),
            on_click(
                Button::new(Kind::Secondary, "Fill tank").build(),
                move || match form::shares(&fill_controls.shares.get_cloned(), fill_read.at.shares)
                {
                    Ok(shares) => start(
                        &fill_read,
                        &fill_controls.shares_flow,
                        Action::TopUpFromShares {
                            vault: fill_read.vault,
                            shares,
                        },
                    ),
                    Err(why) => refuse(&fill_controls.shares_flow, why),
                },
            ),
        ))
        .child(status_slot(&controls.shares_flow, names))
        .child(
            app_link(format!("/agents/{slug}"))
                .class("sh-link")
                .text("Withdraw shares from the agent's page"),
        )
}

fn allowances_card(read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> El {
    let decimals = read.network.asset_decimals;
    let a = read.at.allowances;
    let shown = AllowanceText::of(&a, decimals);
    let values = [
        shown.fuel_millions.clone(),
        shown.memory_mib.clone(),
        shown.requests_per_min.clone(),
        shown.data_mb.clone(),
        shown.streams.clone(),
        shown.inference_ms.clone(),
        shown.gas_dollars.clone(),
    ];
    let fields = [
        AllowanceField::Fuel,
        AllowanceField::Memory,
        AllowanceField::Requests,
        AllowanceField::Data,
        AllowanceField::Streams,
        AllowanceField::Inference,
        AllowanceField::Gas,
    ];
    let list = el("dl")
        .class("sh-publish__dl")
        .children(fields.iter().zip(values.iter()).map(|(which, value)| {
            let (_, label, before, after, _) = allowance_label(*which);
            parts::row(
                label,
                parts::words(format!("{}{value} {after}", before.unwrap_or(""))),
            )
        }));
    let edit_controls = controls.clone();
    let editing = controls.editing_allowances.clone();
    let form_read = read.clone();
    let form_controls = controls.clone();
    let form_names = names.clone();
    el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .attr("aria-labelledby", "m-allowances")
        .child(
            el("div")
                .class("sh-manage__cardhead")
                .child(
                    el("h2")
                        .attr("id", "m-allowances")
                        .class("sh-publish__h2")
                        .text("Allowances"),
                )
                .child(on_click(
                    el("button")
                        .attr("type", "button")
                        .class("sh-btn sh-btn-ghost sh-btn-sm")
                        .attr_from("aria-expanded", &editing, |open| Some(open.to_string()))
                        .attr("aria-controls", "m-allowances-form")
                        .text_from(&editing, |open| {
                            if *open { "Close" } else { "Edit" }.to_string()
                        }),
                    move || {
                        if !edit_controls.editing_allowances.get() {
                            edit_controls.fill_allowances(&a, decimals);
                        }
                        edit_controls.editing_allowances.replace_with(|open| !*open);
                    },
                )),
        )
        .child(list)
        .child(match read.daily {
            Some(daily) => el("p")
                .class("sh-muted")
                .text(format!(
                    "At most {} a day at today's published prices.",
                    money::dollars_exact(daily, decimals)
                ))
                .into(),
            None => View::Empty,
        })
        .child(
            el("p")
                .class("sh-muted sh-author__small")
                .text("The vault's page shows every change."),
        )
        .child_from(&controls.editing_allowances, move |open| {
            open.then(|| {
                let controls = form_controls.clone();
                let read = form_read.clone();
                let typed = controls.clone();
                let grid = allowance_grid(
                    [
                        (&controls.allowance_text[0], AllowanceField::Fuel),
                        (&controls.allowance_text[1], AllowanceField::Memory),
                        (&controls.allowance_text[2], AllowanceField::Requests),
                        (&controls.allowance_text[3], AllowanceField::Data),
                        (&controls.allowance_text[4], AllowanceField::Streams),
                        (&controls.allowance_text[5], AllowanceField::Inference),
                        (&controls.allowance_text[6], AllowanceField::Gas),
                    ],
                    move |which| {
                        let typed = typed.clone();
                        allowance_error_signal(&typed, which, decimals)
                    },
                );
                el("div")
                    .attr("id", "m-allowances-form")
                    .class("sh-manage__edit")
                    .child(grid)
                    .child(on_click(
                        Button::new(Kind::Primary, "Save allowances").build(),
                        move || match controls.typed_allowances().parse(decimals) {
                            Ok(allowances) => start(
                                &read,
                                &controls.allowances,
                                Action::SetAllowances {
                                    vault: read.vault,
                                    allowances,
                                },
                            ),
                            Err(_) => refuse(
                                &controls.allowances,
                                "Fix the fields marked above".to_string(),
                            ),
                        },
                    ))
                    .into()
            })
        })
        .child(status_slot(&controls.allowances, &form_names))
}

fn allowance_error_signal(
    controls: &Rc<Controls>,
    which: AllowanceField,
    decimals: u8,
) -> std::pin::Pin<Box<dyn futures_signals::signal::Signal<Item = Option<String>>>> {
    let signals: Vec<_> = controls
        .allowance_text
        .iter()
        .map(|text| text.signal_cloned().boxed_local())
        .collect();
    let mut signals = signals.into_iter();
    let first = signals.next().expect("seven fields");
    let all = signals.fold(first.map(|text| vec![text]).boxed_local(), |acc, next| {
        futures_signals::map_ref! {
            let acc = acc,
            let next = next => {
                let mut all = acc.clone();
                all.push(next.clone());
                all
            }
        }
        .boxed_local()
    });
    Box::pin(all.map(move |texts| {
        let text = AllowanceText {
            fuel_millions: texts[0].clone(),
            memory_mib: texts[1].clone(),
            requests_per_min: texts[2].clone(),
            data_mb: texts[3].clone(),
            streams: texts[4].clone(),
            inference_ms: texts[5].clone(),
            gas_dollars: texts[6].clone(),
        };
        text.parse(decimals)
            .err()
            .and_then(|errors| errors.into_iter().find(|(field, _)| *field == which))
            .map(|(_, why)| why)
    }))
}

fn floor_card(read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> El {
    let decimals = read.network.asset_decimals;
    let floor = read.at.floor;
    let now = read.at.now;
    let in_force = floor.now(now);
    let kept = read.at.cash;
    let pending = floor.waiting(now);
    let cancel_read = read.clone();
    let cancel_controls = controls.clone();
    let save_read = read.clone();
    let save_controls = controls.clone();
    let editing = controls.editing_floor.clone();
    let open_controls = controls.clone();
    el("section")
        .class("sh-card sh-card--pad sh-manage__card")
        .attr("aria-labelledby", "m-floor")
        .child(
            el("div")
                .class("sh-manage__cardhead")
                .child(
                    el("h2")
                        .attr("id", "m-floor")
                        .class("sh-publish__h2")
                        .text("Idle floor"),
                )
                .child(on_click(
                    el("button")
                        .attr("type", "button")
                        .class("sh-btn sh-btn-ghost sh-btn-sm")
                        .attr_from("aria-expanded", &editing, |open| Some(open.to_string()))
                        .attr("aria-controls", "m-floor-form")
                        .text_from(&editing, |open| {
                            if *open { "Close" } else { "Change" }.to_string()
                        }),
                    move || {
                        if !open_controls.editing_floor.get() {
                            open_controls
                                .floor
                                .set(form::percent(in_force).trim_end_matches('%').to_string());
                        }
                        open_controls.editing_floor.replace_with(|open| !*open);
                    },
                )),
        )
        .child(
            el("p")
                .class("sh-manage__figure")
                .text(form::percent(in_force)),
        )
        .child(el("p").class("sh-muted").text(format!(
            "Cash at home now: {}",
            money::dollars(kept, decimals, Rounding::Down)
        )))
        .child(match pending {
            Some((bps, at)) => el("div")
                .class("sh-manage__pending")
                .child(badge(Tone::Caution, Some(Icon::Clock), "Pending change"))
                .child(el("p").text(format!(
                    "Lowering to {} — takes effect {}",
                    form::percent(bps),
                    utc(at)
                )))
                .child(on_click(
                    Button::new(Kind::Secondary, "Cancel change")
                        .size(Size::Small)
                        .build(),
                    move || {
                        start(
                            &cancel_read,
                            &cancel_controls.idle,
                            Action::CancelFloorChange {
                                vault: cancel_read.vault,
                            },
                        )
                    },
                ))
                .into(),
            None => View::Empty,
        })
        .child_from(&controls.editing_floor, move |open| {
            open.then(|| {
                let controls = save_controls.clone();
                let read = save_read.clone();
                el("div")
                    .attr("id", "m-floor-form")
                    .class("sh-manage__edit")
                    .child(field(
                        "m-floor-input",
                        "New idle floor",
                        None,
                        Some("%".to_string()),
                        &controls.floor,
                        "0% to 100%, two decimals at most.".to_string(),
                        controls
                            .floor
                            .signal_cloned()
                            .map(|text| form::floor_bps(&text).err()),
                    ))
                    .child(on_click(
                        Button::new(Kind::Primary, "Save").build(),
                        move || match form::floor_bps(&controls.floor.get_cloned()) {
                            Ok(bps) => start(
                                &read,
                                &controls.idle,
                                Action::SetIdleFloor {
                                    vault: read.vault,
                                    bps,
                                },
                            ),
                            Err(why) => refuse(&controls.idle, why),
                        },
                    ))
                    .into()
            })
        })
        .child(status_slot(&controls.idle, names))
        .child(el("p").class("sh-muted sh-author__small").text(
            "Raising takes effect at once. Lowering takes a day, so Depositors see it coming.",
        ))
}

fn retire_card(read: &Rc<Read>, controls: &Rc<Controls>, names: &Rc<Names>) -> View {
    if read.at.state != State::Open {
        return View::Empty;
    }
    let open_read = read.clone();
    let open_controls = controls.clone();
    let open_names = names.clone();
    el("section")
        .class("sh-card sh-card--pad sh-manage__card sh-manage__retire")
        .attr("aria-labelledby", "m-retire")
        .child(el("h2").attr("id", "m-retire").class("sh-publish__h2").text("Retire this agent"))
        .child(el("p").class("sh-muted").text(
            "Retiring takes a week, on purpose. Deposits stop at once, your agent keeps running and withdrawals keep working. After seven days it stops for good and the money comes home. A retired agent can't come back.",
        ))
        .child(
            on_click(
                Button::new(Kind::Danger, "Retire agent…").build(),
                move || retire::open(&open_read, &open_controls.retire, &open_names),
            )
            .attr("aria-haspopup", "dialog"),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::author::read::{Floor, Tank};

    #[test]
    fn the_authors_shares_are_never_called_earnings() {
        let network = Rc::new(Network {
            chain: Chain::Base,
            chain_id: 8453,
            label: "Base".to_string(),
            rpc: String::new(),
            asset: Address::repeat_byte(0xdd),
            asset_symbol: "USDC".to_string(),
            asset_decimals: 6,
            dev_account: None,
        });
        let read = Rc::new(Read {
            vault: Address::repeat_byte(0x11),
            chain: Chain::Base,
            title: "Funding basis".to_string(),
            network: network.clone(),
            at: AuthorVault {
                state: State::Open,
                author: Address::repeat_byte(0x7e),
                tank: Tank::default(),
                allowances: Allowances::default(),
                floor: Floor::default(),
                // Shares bought with a deposit and shares paid as a fee
                // look the same on the chain.
                shares: U256::from(29_952_002_564_591_237_459u128),
                share_price: U256::from(1_000_850u64),
                cash: U256::ZERO,
                author_bps: 1_000,
                escape_delay: 0,
                usage_freshness: 0,
                wind_down: 0,
                ready_to_close: false,
                now: 0,
            },
            daily: None,
        });
        let names = Rc::new(Names::of(&network));
        let html = View::from(shares_card(
            "base-3",
            &read,
            &Rc::new(Controls::new()),
            &names,
        ))
        .html();
        assert!(html.contains("Your shares"), "{html}");
        assert!(html.contains("29.9 shares"), "{html}");
        assert!(html.contains("keep the two apart"), "{html}");
        assert!(!html.to_lowercase().contains("earning"), "{html}");
    }
}
