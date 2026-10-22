//! `/publish/:agentId/activate` (Activate-Desktop): set the agent's
//! allowances and idle floor, fill its starting tank, sign the eligibility
//! statement and open its vault. Everything that decides the activation is
//! read from the chain when the page opens and again by the flow right
//! before it signs: the strategy's record and review, the factory's terms
//! and minimum tank, the vault's list, and what the allowances cost at the
//! published prices (an upper bound, recomputed as they change).

use std::rc::Rc;

use alloy_primitives::U256;
use futures_signals::signal::{Mutable, Signal, SignalExt};
use shared::Chain;
use ui::icons::Icon;

use super::parts::{self, Names, field, wallet_gate};
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::money::format as money;
use crate::pages::Page;
use crate::tx::author::form::{self, AllowanceField, AllowanceText, Allowances};
use crate::tx::author::read::{Readiness, Terms};
use crate::tx::author::run::{self, Action, Running};
use crate::tx::author::{Activation, Prompt, Step};
use crate::view::{El, View, el, text};
use crate::wallet::Network;

/// What the page read when it opened.
#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
struct Info {
    chain: Chain,
    id: u64,
    /// Its name as its publishing job gave it, when the jobs list answered.
    title: Option<String>,
    network: Rc<Network>,
    readiness: Readiness,
    terms: Terms,
    /// The list the vault will refuse, as the statement carries it.
    restricted: String,
}

#[derive(Clone)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum Load {
    Loading,
    Failed(String),
    Ready(Rc<Info>),
}

/// The form's fields.
struct Form {
    fuel: Mutable<String>,
    memory: Mutable<String>,
    requests: Mutable<String>,
    data: Mutable<String>,
    streams: Mutable<String>,
    inference: Mutable<String>,
    gas: Mutable<String>,
    floor: Mutable<String>,
    tank: Mutable<String>,
    /// The upper bound for a day at the allowances as typed.
    daily: Mutable<Option<U256>>,
    /// The connected wallet's dollars on the vault's chain.
    balance: Mutable<Option<U256>>,
    running: Mutable<Option<Running>>,
}

impl Form {
    fn new(decimals: u8) -> Self {
        let text = AllowanceText::of(&form::shape("modest"), decimals);
        Self {
            fuel: Mutable::new(text.fuel_millions),
            memory: Mutable::new(text.memory_mib),
            requests: Mutable::new(text.requests_per_min),
            data: Mutable::new(text.data_mb),
            streams: Mutable::new(text.streams),
            inference: Mutable::new(text.inference_ms),
            gas: Mutable::new(text.gas_dollars),
            floor: Mutable::new("5".to_string()),
            tank: Mutable::new(String::new()),
            daily: Mutable::new(None),
            balance: Mutable::new(None),
            running: Mutable::new(None),
        }
    }

    fn fill(&self, allowances: &Allowances, decimals: u8) {
        let text = AllowanceText::of(allowances, decimals);
        self.fuel.set(text.fuel_millions);
        self.memory.set(text.memory_mib);
        self.requests.set(text.requests_per_min);
        self.data.set(text.data_mb);
        self.streams.set(text.streams);
        self.inference.set(text.inference_ms);
        self.gas.set(text.gas_dollars);
    }

    /// The allowances as typed, as a signal.
    fn text_signal(&self) -> impl Signal<Item = AllowanceText> + 'static {
        futures_signals::map_ref! {
            let fuel = self.fuel.signal_cloned(),
            let memory = self.memory.signal_cloned(),
            let requests = self.requests.signal_cloned(),
            let data = self.data.signal_cloned(),
            let streams = self.streams.signal_cloned(),
            let inference = self.inference.signal_cloned(),
            let gas = self.gas.signal_cloned() => AllowanceText {
                fuel_millions: fuel.clone(),
                memory_mib: memory.clone(),
                requests_per_min: requests.clone(),
                data_mb: data.clone(),
                streams: streams.clone(),
                inference_ms: inference.clone(),
                gas_dollars: gas.clone(),
            }
        }
    }

    fn text(&self) -> AllowanceText {
        AllowanceText {
            fuel_millions: self.fuel.get_cloned(),
            memory_mib: self.memory.get_cloned(),
            requests_per_min: self.requests.get_cloned(),
            data_mb: self.data.get_cloned(),
            streams: self.streams.get_cloned(),
            inference_ms: self.inference.get_cloned(),
            gas_dollars: self.gas.get_cloned(),
        }
    }
}

pub fn page(slug: &str) -> Page {
    let Some((chain, id)) = run::parse_slug(slug) else {
        return Page {
            top: None,
            body: el("div")
                .class("sh-author")
                .child(
                    el("h1")
                        .class("sh-author__title")
                        .text("No agent by that name"),
                )
                .child(
                    crate::web::app_link("/publish")
                        .class("sh-link")
                        .text("Publish an agent"),
                )
                .into(),
        };
    };
    let load = Mutable::new(Load::Loading);
    let follow = load.clone();
    // One form for the page's life: what was typed, and the flow it
    // started, outlive a read again (the chain's settings changing).
    let form = Kept::default();
    let watched = form.clone();
    let body = el("div")
        .class("sh-author sh-activate")
        .child_signal(
            Some(loading()),
            load.signal_cloned().map(move |load| {
                Some(match load {
                    Load::Loading => loading(),
                    Load::Failed(why) => notice(NoticeTone::Danger, text(why))
                        .attr("role", "alert")
                        .into(),
                    Load::Ready(info) => view(&info, &form),
                })
            }),
        )
        .hook(move |dom| dom.future(read_page(chain, id, follow, watched)));
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
        .child(crate::atoms::skeleton::skeleton_block("100%", 320))
        .into()
}

/// The page's form, made on its first read and kept from then on.
type Kept = Rc<std::cell::OnceCell<Rc<Form>>>;

/// Whether the page's form has a flow under way: the wallet asked, or a
/// transaction on its way.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn live(form: &Kept) -> bool {
    form.get()
        .and_then(|form| form.running.get_cloned())
        .is_some_and(|running| running.is_live())
}

/// Whether a new read may be drawn now: always before the page is first
/// drawn; once drawn, only while no flow is under way, so a wallet prompt
/// and the status beside it stay still.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn may_redraw(load: &Mutable<Load>, form: &Kept) -> bool {
    !matches!(&*load.lock_ref(), Load::Ready(_)) || !live(form)
}

/// A read that failed (`why`): said in place of the page on the first read.
/// Once the page has been drawn, what it shows (the form, and a flow's
/// status and links, finished or not) stays, and the read is tried again
/// shortly. Whether to stop reading.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn read_failed(load: &Mutable<Load>, why: String) -> bool {
    if matches!(&*load.lock_ref(), Load::Ready(_)) {
        return false;
    }
    load.set(Load::Failed(why));
    true
}

async fn read_page(chain: Chain, id: u64, load: Mutable<Load>, form: Kept) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::tx::author::read;
        use crate::tx::read::read as call;
        use crate::wallet::session::Session;
        use chain::bindings::factory::IVaultFactory;

        let session = crate::wallet::get();
        let failed = |why: String| read_failed(&load, why);
        loop {
            let (Some(network), Some(platform)) = run::platform(chain).await.unzip() else {
                if failed(format!(
                    "{} isn't reachable from the app right now. Reload to try again.",
                    chain.label()
                )) {
                    return;
                }
                gloo_timers::future::TimeoutFuture::new(5_000).await;
                continue;
            };
            let reader = Session::reader(&network);
            let strategy_id = U256::from(id);
            let read_all = async {
                let readiness =
                    read::readiness(&reader, &platform.registries, network.chain_id, strategy_id)
                        .await?;
                let terms = read::terms(&reader, platform.registries.factory).await?;
                let restricted = call(
                    &reader,
                    platform.registries.factory,
                    IVaultFactory::restrictedForCall {
                        strategyId: strategy_id,
                    },
                )
                .await?
                .text;
                Ok::<_, crate::tx::TxError>((readiness, terms, restricted))
            };
            let (readiness, terms, restricted) = match read_all.await {
                Ok(read) => read,
                Err(_) => {
                    if failed("The chain didn't answer. Reload to try again.".to_string()) {
                        return;
                    }
                    gloo_timers::future::TimeoutFuture::new(5_000).await;
                    continue;
                }
            };
            let title = run::jobs()
                .await
                .ok()
                .and_then(|jobs| {
                    jobs.into_iter()
                        .find(|job| job.chain == chain && job.strategy_id == Some(id))
                        .map(|job| job.title)
                })
                .filter(|title| !title.trim().is_empty());
            let config = network.config;
            // A read again (after a failed one, or a settings change) never
            // redraws the page while the wallet is asked or a transaction is
            // on its way: it waits for the flow to end.
            while !may_redraw(&load, &form) {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
            }
            load.set(Load::Ready(Rc::new(Info {
                chain,
                id,
                title,
                network,
                readiness,
                terms,
                restricted,
            })));
            // Read again once the chain's settings change: the form (kept,
            // with what was typed) is then drawn, and the activation
            // computed, with the network it would sign with. Never while a
            // flow is under way: its status stays, and it can't be started
            // twice.
            while live(&form)
                || session
                    .networks
                    .lock_ref()
                    .get(&chain)
                    .map(|network| network.config)
                    == Some(config)
            {
                gloo_timers::future::TimeoutFuture::new(1_000).await;
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (chain, id, load, form);
}

/// Why the strategy can't be activated by this page, if it can't.
fn blocker(info: &Info) -> Option<View> {
    let ready = &info.readiness;
    if !ready.registered || !ready.reviewed {
        return Some(
            notice(
                NoticeTone::Caution,
                text("It isn't ready to activate: its review and its build aren't both on the chain yet. Its job page shows where it is."),
            )
            .into(),
        );
    }
    if !ready.other_chains.is_empty() {
        return Some(
            notice(
                NoticeTone::Caution,
                text("It trades on chains besides its vault's. Vaults with stations on other chains can't be opened yet."),
            )
            .into(),
        );
    }
    None
}

/// The agent's name: its publishing job's, else the snapshot's, else its
/// number.
fn title_now(info: &Info) -> String {
    let slug = shared::Strategy::slug_for(info.chain, info.id);
    info.title
        .clone()
        .or_else(|| {
            crate::store::get()
                .agent(&slug)
                .map(|agent| agent.title.get_cloned())
                .filter(|title| !title.trim().is_empty())
        })
        .unwrap_or_else(|| format!("agent {}", info.id))
}

/// The name as the snapshot arrives.
fn title_signal(info: &Rc<Info>) -> impl Signal<Item = String> + 'static {
    let info = info.clone();
    crate::store::get()
        .loaded
        .signal()
        .map(move |_| title_now(&info))
}

/// The agent's page, once the snapshot has it.
fn agent_href(info: &Info) -> Option<String> {
    let slug = shared::Strategy::slug_for(info.chain, info.id);
    crate::store::get().agent(&slug).map(|agent| agent.href())
}

/// "Your agents / {agent} / Activate".
fn crumb(info: &Rc<Info>) -> El {
    el("nav")
        .class("ag-crumb")
        .attr("aria-label", "Breadcrumb")
        .child(
            el("ol")
                .child(
                    el("li")
                        .child(crate::web::app_link("/you").text("Your agents"))
                        .child(el("span").attr("aria-hidden", "true").text("/")),
                )
                .child(
                    el("li")
                        .text_signal(title_now(info), title_signal(info))
                        .child(el("span").attr("aria-hidden", "true").text("/")),
                )
                .child(el("li").attr("aria-current", "page").text("Activate")),
        )
}

fn head(info: &Rc<Info>, lede: &'static str) -> El {
    let title = title_signal(info).map(|title| format!("Activate {title}"));
    el("div").class("sh-author__head").child(
        el("div")
            .class("sh-author__headtext")
            .child(
                el("h1")
                    .class("sh-author__title")
                    .text_signal(format!("Activate {}", title_now(info)), title),
            )
            .child(el("p").class("sh-author__lede").text(lede)),
    )
}

/// An agent whose vault is open: nothing to activate, a way to manage it.
fn already_active(info: &Rc<Info>) -> View {
    let slug = shared::Strategy::slug_for(info.chain, info.id);
    let page_link = {
        let info = info.clone();
        crate::store::get().loaded.signal().map(move |_| {
            agent_href(&info).map(|href| {
                crate::web::app_link(href)
                    .class("sh-btn sh-btn-secondary")
                    .text("View its page")
                    .into()
            })
        })
    };
    el("div")
        .class("sh-activate__body")
        .child(crumb(info))
        .child(head(info, "Its vault is open and taking deposits. There's nothing left to activate."))
        .child(
            el("div")
                .class("sh-card sh-card--pad sh-activate__card")
                .child(
                    el("div")
                        .class("sh-activate__ready")
                        .child(crate::atoms::badge::badge(
                            crate::atoms::badge::Tone::Gain,
                            Some(Icon::Check),
                            "Active",
                        ))
                        .child(el("span").class("sh-muted").text("It's already active")),
                )
                .child(el("p").text(
                    "Change its allowances and idle floor, top up its tank or retire it from its manage page.",
                ))
                .child(
                    el("div")
                        .class("sh-activate__quick")
                        .child(
                            crate::web::app_link(format!("/you/agents/{slug}"))
                                .class("sh-btn sh-btn-primary")
                                .text("Manage it"),
                        )
                        .child_signal(None, page_link),
                ),
        )
        .into()
}

fn view(info: &Rc<Info>, kept: &Kept) -> View {
    let decimals = info.network.asset_decimals;
    let form = kept.get_or_init(|| Rc::new(Form::new(decimals))).clone();
    // A flow under way (or ended, and not left) stays in view, whatever
    // the chain now says about the vault.
    if info.readiness.vault.is_some() && form.running.lock_ref().is_none() {
        return already_active(info);
    }
    let names = Rc::new(Names::of(&info.network));
    let can_activate = blocker(info).is_none();
    let head = head(
        info,
        "Set its limits, fill its tank, and open its vault in one transaction. You can change the limits and the idle floor later; the vault's page shows every change.",
    );
    let report = {
        let info = info.clone();
        crate::store::get().loaded.signal().map(move |_| {
            agent_href(&info).map(|href| {
                crate::web::app_link(format!("{href}/review"))
                    .class("sh-link")
                    .text("Read the report")
                    .into()
            })
        })
    };
    let status: View = match blocker(info) {
        Some(blocker) => blocker,
        None => el("div")
            .class("sh-activate__ready")
            .child(crate::atoms::badge::badge(
                crate::atoms::badge::Tone::Gain,
                Some(Icon::Check),
                "Ready",
            ))
            .child(
                el("span")
                    .class("sh-muted")
                    .text("Review posted, build registered"),
            )
            .child_signal(None, report)
            .into(),
    };
    let author_note = author_notice(info);
    el("div")
        .class("sh-activate__body")
        .child(crumb(info))
        .child(head)
        .child(status)
        .child(author_note)
        .child(
            el("div")
                .class("sh-activate__grid")
                .child(
                    el("div")
                        .class("sh-activate__main")
                        .child(allowances_card(info, &form))
                        .child(floor_card(&form))
                        .child(tank_card(info, &form))
                        .child(statement_card(info)),
                )
                .child(summary(info, &form, &names, can_activate)),
        )
        .hook({
            let info = info.clone();
            let form = form.clone();
            move |dom| dom.future(follow_costs(info, form))
        })
        .into()
}

/// Only the strategy's Author can activate it: say so to anyone else.
fn author_notice(info: &Info) -> View {
    let author = info.readiness.author;
    let session = crate::wallet::get();
    el("div")
        .child_signal(
            None,
            session.connection.signal_cloned().map(move |connection| {
                let account = connection.account()?;
                (account != author).then(|| {
                    notice(
                        NoticeTone::Caution,
                        text(format!(
                            "Only its Author can activate it. Connect {} to continue.",
                            crate::format::short_hash(&format!("{author:?}"))
                        )),
                    )
                    .into()
                })
            }),
        )
        .into()
}

/// Read the upper bound again whenever the allowances change, and the
/// wallet's balance whenever the account does.
async fn follow_costs(info: Rc<Info>, form: Rc<Form>) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::tx::author::read;
        use crate::wallet::session::Session;
        let session = crate::wallet::get();
        let reader = Session::reader(&info.network);
        // The contracts of the config this page was read with.
        let Some(platform) = run::platform_of(&info.network) else {
            return;
        };
        let decimals = info.network.asset_decimals;
        let mut last: Option<(Allowances, Option<alloy_primitives::Address>)> = None;
        loop {
            let account = session.account();
            let typed = form.text().parse(decimals).ok();
            match typed {
                Some(allowances) if last.as_ref().map(|(a, _)| *a) != Some(allowances) => {
                    form.daily.set(
                        read::daily_cost(&reader, platform.registries.prices, &allowances)
                            .await
                            .ok(),
                    );
                }
                None => form.daily.set(None),
                _ => {}
            }
            if last.as_ref().map(|(_, a)| *a) != Some(account) {
                form.balance.set(match account {
                    Some(account) => {
                        crate::tx::read::asset_balance(&reader, info.network.asset, account)
                            .await
                            .ok()
                    }
                    None => None,
                });
            }
            last = typed.map(|allowances| (allowances, account));
            gloo_timers::future::TimeoutFuture::new(500).await;
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (info, form);
}

fn field_error(
    form: &Form,
    which: AllowanceField,
    decimals: u8,
) -> impl Signal<Item = Option<String>> + 'static {
    form.text_signal().map(move |text| {
        text.parse(decimals)
            .err()
            .and_then(|errors| errors.into_iter().find(|(field, _)| *field == which))
            .map(|(_, why)| why)
    })
}

fn allowances_card(info: &Info, form: &Rc<Form>) -> El {
    let decimals = info.network.asset_decimals;
    let shape = |label: &'static str, name: &'static str| {
        let form = form.clone();
        on_click(
            Button::new(Kind::Secondary, label)
                .size(Size::Small)
                .build(),
            move || form.fill(&form::shape(name), decimals),
        )
    };
    el("section")
        .class("sh-card sh-card--pad sh-activate__card")
        .attr("aria-labelledby", "act-allowances")
        .child(el("h2").attr("id", "act-allowances").class("sh-publish__h2").text("Allowances"))
        .child(el("p").class("sh-muted").text(
            "The most your agent may use. It can't go past these, and they're public on its page.",
        ))
        .child(
            el("div")
                .class("sh-activate__shapes")
                .child(el("span").class("sh-muted sh-author__small").text("Start from"))
                .child(shape("Modest", "modest"))
                .child(shape("With a model", "thinking"))
                .child(shape("Quoting", "quoting")),
        )
        .child(allowance_fields(form, decimals))
        .child(
            el("div")
                .class("sh-activate__bound")
                .child(icon(Icon::Info, 18))
                .child(el("p").text_signal(
                    "",
                    form.daily.signal_cloned().map(move |daily| match daily {
                        Some(daily) => format!(
                            "Upper bound: at these limits, running costs at most {} a day at today's published prices.",
                            money::dollars_exact(daily, decimals)
                        ),
                        None => "Upper bound: fix the limits above to see what a day can cost at most.".to_string(),
                    }),
                )),
        )
}

/// The seven allowance fields, in the contracts' own units.
fn allowance_fields(form: &Rc<Form>, decimals: u8) -> El {
    allowance_grid(
        [
            (&form.fuel, AllowanceField::Fuel),
            (&form.memory, AllowanceField::Memory),
            (&form.requests, AllowanceField::Requests),
            (&form.data, AllowanceField::Data),
            (&form.streams, AllowanceField::Streams),
            (&form.inference, AllowanceField::Inference),
            (&form.gas, AllowanceField::Gas),
        ],
        {
            let form = form.clone();
            move |which| field_error(&form, which, decimals).boxed_local()
        },
    )
}

/// A grid of the seven fields; `error` gives each field's refusal.
pub(crate) fn allowance_grid(
    fields: [(&Mutable<String>, AllowanceField); 7],
    error: impl Fn(AllowanceField) -> std::pin::Pin<Box<dyn Signal<Item = Option<String>>>>,
) -> El {
    el("div")
        .class("sh-activate__fields")
        .children(fields.into_iter().map(|(value, which)| {
            let (id, label, before, after, hint) = allowance_label(which);
            field(
                id,
                label,
                before,
                Some(after.to_string()),
                value,
                hint.to_string(),
                error(which),
            )
        }))
}

/// Each allowance's id, label, units and hint.
pub(crate) fn allowance_label(
    which: AllowanceField,
) -> (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static str,
) {
    match which {
        AllowanceField::Fuel => (
            "al-fuel",
            "Computing",
            None,
            "million fuel a second",
            "Processor time, as Wasmtime counts it. 10 million a second is a hundredth of a core.",
        ),
        AllowanceField::Memory => (
            "al-memory",
            "Memory",
            None,
            "MiB",
            "The most memory it can hold at once.",
        ),
        AllowanceField::Requests => (
            "al-requests",
            "Requests to other services",
            None,
            "a minute",
            "Calls to venues and data services.",
        ),
        AllowanceField::Data => (
            "al-data",
            "Data",
            None,
            "MB a day",
            "Data it sends and receives.",
        ),
        AllowanceField::Streams => (
            "al-streams",
            "Live connections",
            None,
            "at once",
            "Open streams, like a venue's price feed.",
        ),
        AllowanceField::Inference => (
            "al-inference",
            "Time running AI models",
            None,
            "ms a minute",
            "Leave it at 0 if your agent ships no AI model.",
        ),
        AllowanceField::Gas => (
            "al-gas",
            "Gas",
            Some("$"),
            "a day",
            "Also the most the tank can be charged for gas in a day.",
        ),
    }
}

fn floor_card(form: &Rc<Form>) -> El {
    el("section")
        .class("sh-card sh-card--pad sh-activate__card")
        .attr("aria-labelledby", "act-floor")
        .child(el("h2").attr("id", "act-floor").class("sh-publish__h2").text("Idle floor"))
        .child(field(
            "act-floor-input",
            "Kept as cash at home",
            None,
            Some("%".to_string()),
            &form.floor,
            "Default 5%. 0% is allowed.".to_string(),
            form.floor
                .signal_cloned()
                .map(|text| form::floor_bps(&text).err()),
        ))
        .child(el("p").class("sh-muted").text(
            "The share of the vault kept as cash at home, so withdrawals can be paid straight from it. Raising it takes effect at once. Lowering it takes a day, so Depositors see it coming.",
        ))
}

fn tank_error(info: &Info, form: &Form) -> impl Signal<Item = Option<String>> + 'static {
    let decimals = info.network.asset_decimals;
    let minimum = info.terms.minimum_tank;
    futures_signals::map_ref! {
        let text = form.tank.signal_cloned(),
        let balance = form.balance.signal() => {
            if text.trim().is_empty() {
                None
            } else {
                form::starting_tank(text, decimals, minimum, U256::ZERO, *balance).err()
            }
        }
    }
}

fn tank_card(info: &Info, form: &Rc<Form>) -> El {
    let decimals = info.network.asset_decimals;
    let minimum = info.terms.minimum_tank;
    let quick_form = form.clone();
    el("section")
        .class("sh-card sh-card--pad sh-activate__card")
        .attr("aria-labelledby", "act-tank")
        .child(el("h2").attr("id", "act-tank").class("sh-publish__h2").text("Starting tank"))
        .child(field(
            "act-tank-input",
            "Starting tank",
            Some("$"),
            Some(info.network.asset_symbol.clone()),
            &form.tank,
            format!(
                "At least {}. About a week of running is a sensible start.",
                money::dollars_exact(minimum, decimals)
            ),
            tank_error(info, form),
        ))
        .child(
            el("div")
                .class("sh-activate__quick")
                .attr("role", "group")
                .attr("aria-label", "Quick amounts")
                .child_signal(
                    None,
                    form.daily.signal().map(move |daily| {
                        let form = quick_form.clone();
                        let amounts = form::quick_amounts(minimum, daily.unwrap_or_default(), decimals);
                        Some(
                            View::Many(
                                amounts
                                    .into_iter()
                                    .filter(|(name, _)| *name == "minimum" || daily.is_some())
                                    .map(|(name, amount)| {
                                        let form = form.clone();
                                        on_click(
                                            Button::new(
                                                Kind::Secondary,
                                                format!(
                                                    "{} · {}",
                                                    money::dollars_exact(amount, decimals),
                                                    name
                                                ),
                                            )
                                            .size(Size::Small)
                                            .build(),
                                            move || {
                                                form.tank.set(money::plain_exact(amount, decimals))
                                            },
                                        )
                                        .into()
                                    })
                                    .collect(),
                            ),
                        )
                    }),
                ),
        )
        .child(el("p").class("sh-muted").text(format!(
            "Stations on other chains: none needed — your agent trades only on {}.",
            info.chain.label()
        )))
        .child(el("p").class("sh-muted").text(
            "A running tab. The tank pays for computing and gas, collected once a day. It stops when it's empty: your agent stops the moment the tab reaches what's in the tank. Anyone can top it up to restart it.",
        ))
}

fn statement_card(info: &Info) -> El {
    let refused = if info.restricted.trim().is_empty() {
        "None listed.".to_string()
    } else {
        info.restricted.replace(',', ", ")
    };
    el("section")
        .class("sh-card sh-card--pad sh-activate__card")
        .attr("aria-labelledby", "act-statement")
        .child(
            el("h2")
                .attr("id", "act-statement")
                .class("sh-publish__h2")
                .text("Your eligibility statement"),
        )
        .child(el("p").text(
            "You're the vault's first holder, so before it opens you sign the same statement every Depositor signs. It's a signature in your wallet, not a transaction:",
        ))
        .child(el("blockquote").class("sh-activate__quote").text(format!("“{}”", chain::statement::TEXT)))
        .child(
            el("dl")
                .class("sh-publish__dl")
                .child(parts::row("Restricted for this vault", parts::words(refused))),
        )
}

/// The activation's steps, as the flow reaches them.
fn progress(running: &Running) -> El {
    let steps = ["Sign the statement", "Approve the tank", "Open the vault"];
    el("ol").class("sh-activate__progress").child_signal(
        None,
        running.step.signal_cloned().map(move |step| {
            let at = match &step {
                Some(Step::Signing(Prompt::Statement)) => 0,
                Some(
                    Step::Signing(Prompt::Approve { .. })
                    | Step::Sent {
                        prompt: Prompt::Approve { .. },
                        ..
                    },
                ) => 1,
                Some(
                    Step::Signing(Prompt::Activate { .. })
                    | Step::Sent {
                        prompt: Prompt::Activate { .. },
                        ..
                    },
                ) => 2,
                Some(Step::Done(_)) => 3,
                _ => 0,
            };
            Some(View::Many(
                steps
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        el("li")
                            .class("sh-job__step")
                            .class_if(index < at, "sh-job__step--done")
                            .class_if(index == at, "sh-job__step--now")
                            .attr_if(index == at, "aria-current", "step")
                            .child(
                                el("span")
                                    .class("sh-job__stepmark")
                                    .attr("aria-hidden", "true")
                                    .child(if index < at {
                                        icon(Icon::Check, 14).into()
                                    } else {
                                        text((index + 1).to_string())
                                    }),
                            )
                            .child(el("span").class("sh-job__stepname").text(*name))
                            .into()
                    })
                    .collect(),
            ))
        }),
    )
}

/// The activation as chosen, if every field is valid.
fn chosen(info: &Info, form: &Form) -> Option<Activation> {
    let decimals = info.network.asset_decimals;
    let allowances = form.text().parse(decimals).ok()?;
    let floor = form::floor_bps(&form.floor.get_cloned()).ok()?;
    let tank = form::starting_tank(
        &form.tank.get_cloned(),
        decimals,
        info.terms.minimum_tank,
        U256::ZERO,
        form.balance.get(),
    )
    .ok()?;
    Some(Activation {
        strategy_id: U256::from(info.id),
        allowances,
        idle_floor_bps: floor,
        tank,
        keep: U256::ZERO,
        terms: info.terms.hash,
    })
}

fn summary(info: &Rc<Info>, form: &Rc<Form>, names: &Rc<Names>, can_activate: bool) -> El {
    let decimals = info.network.asset_decimals;
    let tank_total = form.tank.signal_cloned().map(move |text| {
        form::dollars(&text, decimals)
            .map(|tank| money::dollars_exact(tank, decimals))
            .unwrap_or_else(|_| "—".to_string())
    });
    let daily = form.daily.signal().map(move |daily| match daily {
        Some(daily) => format!("At most {} a day", money::dollars_exact(daily, decimals)),
        None => "—".to_string(),
    });
    let floor = form.floor.signal_cloned().map(|text| {
        form::floor_bps(&text)
            .map(form::percent)
            .unwrap_or_else(|_| "—".to_string())
    });
    let chain = format!("{} ({})", info.chain.label(), info.network.asset_symbol);
    let running_form = form.clone();
    let start_info = info.clone();
    let start_form = form.clone();
    let after = shared::Strategy::slug_for(info.chain, info.id);
    el("aside")
        .class("sh-card sh-card--pad sh-activate__summary")
        .child(el("h2").class("sh-publish__h2").text("Summary"))
        .child(
            el("dl")
                .class("sh-publish__dl")
                .child(parts::row_signal(
                    "Agent",
                    format!("{} · {}", title_now(info), info.id),
                    {
                        let id = info.id;
                        title_signal(info).map(move |title| format!("{title} · {id}"))
                    },
                ))
                .child(parts::row("Vault chain", parts::words(chain)))
                .child(parts::row_signal("Running costs", "—".to_string(), daily))
                .child(parts::row_signal("Idle floor", "5%".to_string(), floor))
                .child(parts::row("Stations", parts::words("None needed")))
                .child(parts::row_signal("Starting tank", "—".to_string(), tank_total)),
        )
        .child(
            el("p")
                .class("sh-activate__total")
                .text_signal(
                    "",
                    form.tank.signal_cloned().map(move |text| match form::dollars(&text, decimals) {
                        Ok(tank) => format!(
                            "Total today: {} (the starting tank; no station costs)",
                            money::dollars_exact(tank, decimals)
                        ),
                        Err(_) => "Total today: type a starting tank".to_string(),
                    }),
                ),
        )
        .child(if can_activate {
            wallet_gate(info.network.chain_id, move || {
                let info = start_info.clone();
                let form = start_form.clone();
                let busy = Mutable::new(false);
                on_click(
                    Button::new(Kind::Primary, "Activate agent")
                        .size(Size::Large)
                        .block()
                        .busy(&busy)
                        .build(),
                    move || start(&info, &form),
                )
                .into()
            })
        } else {
            Button::new(Kind::Primary, "Activate agent")
                .size(Size::Large)
                .block()
                .disabled(true)
                .build()
                .into()
        })
        .child(el("p").class("sh-muted sh-author__small").text(format!(
            "You sign the statement, approve the tank, then one transaction on {} opens the vault and fills the tank. Your vault opens straight away and is marked new until it has 30 days of history.",
            info.chain.label()
        )))
        .child_signal(None, running_form.running.signal_cloned().map({
            let names = names.clone();
            move |running| {
                running.map(|running| {
                    let done = running.step.signal_cloned().map({
                        let after = after.clone();
                        move |step| {
                            matches!(step, Some(Step::Done(_))).then(|| {
                                el("div")
                                    .class("sh-job__actions")
                                    .child(
                                        Button::new(Kind::Primary, "Manage your agent")
                                            .href(format!("/you/agents/{after}"))
                                            .build(),
                                    )
                                    .child(
                                        Button::new(Kind::Secondary, "View public page")
                                            .href(format!("/agents/{after}"))
                                            .build(),
                                    )
                                    .into()
                            })
                        }
                    });
                    el("div")
                        .class("sh-activate__flow")
                        .child(progress(&running))
                        .child(parts::status(&running, names.clone()))
                        .child_signal(None, done)
                        .into()
                })
            }
        }))
}

fn start(info: &Rc<Info>, form: &Rc<Form>) {
    // Nothing while one runs; and once one has activated the vault, its
    // Done status and links stay: there is nothing left to activate.
    if form.running.lock_ref().as_ref().is_some_and(|running| {
        running.is_live() || matches!(running.step.lock_ref().as_ref(), Some(Step::Done(_)))
    }) {
        return;
    }
    let Some(activation) = chosen(info, form) else {
        // Each field says what's wrong under it; say it once here too.
        let running = Running::new();
        running
            .step
            .set(Some(Step::Failed(crate::tx::TxError::NothingToSend.into())));
        form.running.set(Some(running));
        return;
    };
    let running = Running::new();
    form.running.set(Some(running.clone()));
    // Computed with this page's network: the flow signs with it or not at all.
    run::start(
        info.chain,
        Action::Activate(activation),
        info.network.config,
        &running,
        |_| {},
    );
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256};

    use super::*;
    use crate::tx::author::read::{Readiness, Terms};
    use crate::wallet::config_cache::ConfigId;

    /// The page as read with the network of config `config`, `decimals`
    /// decimals; `active` once its vault exists.
    fn info(config: u8, decimals: u8, active: bool) -> Rc<Info> {
        Rc::new(Info {
            chain: Chain::Base,
            id: 7,
            title: Some("Funding basis".to_string()),
            network: Rc::new(Network {
                chain: Chain::Base,
                chain_id: 84_532,
                label: "Base Sepolia".to_string(),
                rpc: String::new(),
                asset: Address::repeat_byte(config),
                asset_symbol: "USDC".to_string(),
                asset_decimals: decimals,
                dev_account: None,
                config: ConfigId(B256::repeat_byte(config)),
            }),
            readiness: Readiness {
                author: Address::repeat_byte(0x42),
                registered: true,
                reviewed: true,
                vault: active.then(|| Address::repeat_byte(0x5f)),
                other_chains: Vec::new(),
            },
            terms: Terms {
                hash: B256::repeat_byte(config),
                minimum_tank: U256::from(10u8),
                minimum_deposit: U256::from(10u8),
                author_bps: 1000,
                activation_window: 0,
                floor_delay: 0,
                wind_down: 0,
            },
            restricted: String::new(),
        })
    }

    #[test]
    fn a_read_that_fails_after_the_page_was_drawn_keeps_a_finished_activations_status() {
        // The first read fails: said in place of the page, and reading stops.
        let load = Mutable::new(Load::Loading);
        assert!(read_failed(&load, "The chain didn't answer.".to_string()));
        assert!(matches!(load.get_cloned(), Load::Failed(_)));
        // Drawn, with an activation that has ended: a later read that fails
        // keeps the page (its form, the flow's status and its links), and
        // the read is tried again.
        let shown = info(1, 6, false);
        let load = Mutable::new(Load::Ready(shown.clone()));
        assert!(!read_failed(&load, "The chain didn't answer.".to_string()));
        assert!(matches!(load.get_cloned(), Load::Ready(info) if Rc::ptr_eq(&info, &shown)));
    }

    #[test]
    fn a_config_change_never_hides_or_restarts_an_activation_under_way() {
        let kept = Kept::default();
        let _ = view(&info(1, 6, false), &kept).html();
        let form = kept
            .get()
            .expect("the form, made on the first read")
            .clone();
        form.tank.set("250".to_string());
        // The author starts it: the flow is under way.
        let running = Running::new();
        running.step.set(Some(Step::Checking));
        form.running.set(Some(running.clone()));
        assert!(live(&kept), "the page reads nothing again while it runs");
        // The chain's settings change and the page is drawn again: the same
        // form, what was typed, and the flow under way, shown.
        let again = info(2, 18, false);
        let html = view(&again, &kept).html();
        assert!(Rc::ptr_eq(kept.get().unwrap(), &form));
        assert_eq!(form.tank.get_cloned(), "250");
        assert!(html.contains("sh-activate__total"), "{html}");
        // "Activate agent" starts nothing while it runs.
        start(&again, &form);
        assert!(matches!(
            form.running
                .get_cloned()
                .and_then(|now| now.step.get_cloned()),
            Some(Step::Checking)
        ));
        // Drawn with a vault that now exists, the form and its flow stay
        // rather than the page for an agent already active.
        let html = view(&info(2, 18, true), &kept).html();
        assert!(!html.contains("nothing left to activate"), "{html}");
        assert!(html.contains("sh-activate__total"), "{html}");
        let fresh = view(&info(2, 18, true), &Kept::default()).html();
        assert!(fresh.contains("nothing left to activate"), "{fresh}");
        // Once it has ended, the page may read again.
        running.step.set(Some(Step::Failed(
            crate::tx::TxError::SettingsChanged.into(),
        )));
        assert!(!live(&kept));
    }

    #[test]
    fn a_read_again_never_redraws_the_page_while_a_wallet_prompt_is_open() {
        let kept = Kept::default();
        let load = Mutable::new(Load::Loading);
        assert!(may_redraw(&load, &kept), "the first read is drawn at once");
        let shown = info(1, 6, false);
        let _ = view(&shown, &kept).html();
        load.set(Load::Ready(shown));
        assert!(may_redraw(&load, &kept), "drawn, with nothing under way");
        let form = kept.get().expect("the form").clone();
        let running = Running::new();
        running.step.set(Some(Step::Signing(Prompt::Statement)));
        form.running.set(Some(running.clone()));
        assert!(
            !may_redraw(&load, &kept),
            "the wallet is asked: the page keeps still"
        );
        running
            .step
            .set(Some(Step::Failed(crate::tx::TxError::Rejected.into())));
        assert!(
            may_redraw(&load, &kept),
            "once the flow ends it may be drawn again"
        );
    }

    #[test]
    fn activate_after_a_finished_activation_keeps_its_done_status() {
        let kept = Kept::default();
        let shown = info(1, 6, false);
        let _ = view(&shown, &kept).html();
        let form = kept.get().expect("the form").clone();
        form.tank.set("250".to_string());
        let running = Running::new();
        let done = Step::Done(crate::tx::author::flow::Outcome::Activated {
            vault: Address::repeat_byte(0x5f),
            hash: B256::repeat_byte(0x01),
        });
        running.step.set(Some(done.clone()));
        form.running.set(Some(running.clone()));
        start(&shown, &form);
        let now = form.running.get_cloned().expect("a flow");
        assert_eq!(now.step.get_cloned(), Some(done));
        // The same flow, not a new one: they share their step.
        running.step.set(Some(Step::Checking));
        assert_eq!(
            now.step.get_cloned(),
            Some(Step::Checking),
            "a new flow was started"
        );
    }
}
