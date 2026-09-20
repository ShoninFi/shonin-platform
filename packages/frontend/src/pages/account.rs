//! One agent: what it does, its record from attested reads, the flow that
//! connects an account to it, and what an owner sees once theirs is running.

use std::sync::OnceLock;

use dominator::{Dom, clone, events, html};
use futures_signals::signal::{Mutable, SignalExt};
use shared::{Connected, Strategy};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::pages::parts;
use crate::pages::strategy::fee_table;
use crate::state;
use crate::ui::chart::{self, Series};
use crate::ui::form;
use crate::ui::overlay::{modal, modal_actions};
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
use crate::ui::table::{Column, Row, table};
use crate::wallet::{self, Account};

/// Which step of the connect flow is on screen, if it is open at all.
fn step() -> &'static Mutable<Option<u8>> {
    static STEP: OnceLock<Mutable<Option<u8>>> = OnceLock::new();
    STEP.get_or_init(|| Mutable::new(None))
}

fn account_value() -> &'static Mutable<String> {
    static VALUE: OnceLock<Mutable<String>> = OnceLock::new();
    VALUE.get_or_init(|| Mutable::new("40000".to_string()))
}

fn tank_amount() -> &'static Mutable<String> {
    static TANK: OnceLock<Mutable<String>> = OnceLock::new();
    TANK.get_or_init(|| Mutable::new("120".to_string()))
}

pub fn render(slug: &str) -> Dom {
    let Some(agent) = data::strategy(slug) else {
        return crate::pages::not_found::render();
    };
    let Some(connected) = agent.connected() else {
        return crate::pages::not_found::render();
    };

    html!("div", {
        .class("stack-lg")
        .child(header(agent))
        .child(html!("div", {
            .class(["grid", "grid--aside"])
            .child(html!("div", {
                .class("stack-lg")
                .child(what_it_does(agent))
                .child(record(agent, connected))
                .child(custody(connected))
                .child(requirements(connected))
                .child(fees(connected))
                .child(limits(agent))
            }))
            .child(html!("div", {
                .class("stack")
                .child(owner_panel(agent, connected))
                .child(identity(agent))
            }))
        }))
        .child(connect_flow(agent, connected))
    })
}

fn header(agent: &'static Strategy) -> Dom {
    html!("div", {
        .class(["row", "row--wrap", "row--between", "row--top"])
        .child(html!("div", {
            .child(html!("h1", { .class("title").text(&agent.name()) }))
            .child(parts::byline(agent))
        }))
        .child(ui::badges(vec![
            parts::standing_badge(agent.standing),
            parts::source_badge(agent),
            ui::badge("broker-enforced", Tone::Ok),
        ]))
    })
}

fn what_it_does(agent: &'static Strategy) -> Dom {
    ui::section(
        "What it does",
        vec![
            html!("p", { .text(&agent.description) }),
            ui::note("The author's own words. The review reads the code, not the description."),
        ],
    )
}

fn record(agent: &'static Strategy, connected: &'static Connected) -> Dom {
    let Some(record) = &agent.record else {
        return html!("span");
    };
    let performance = data::performance(agent);

    ui::section(
        "The record, from attested reads",
        vec![
            ui::stats(vec![
                ui::stat_toned(
                    "Normalised return",
                    &fmt::annualised(performance.normalised_annualised),
                    Some("across every account running it"),
                    parts::tone_of(performance.normalised_annualised),
                ),
                ui::stat_toned(
                    "Max drawdown",
                    &fmt::percent_signed(performance.max_drawdown, 1),
                    Some("deepest fall from a peak"),
                    Tone::Neutral,
                ),
                ui::stat(
                    "Accounts",
                    &fmt::count(connected.accounts),
                    Some(&format!(
                        "{} between them",
                        fmt::money_compact(connected.total_value)
                    )),
                ),
                ui::stat(
                    "Live",
                    &fmt::days(agent.days_live),
                    Some(&format!("read {}", connected.read_cadence)),
                ),
            ]),
            chart::line_chart(
                vec![
                    Series::new("aggregate value", record.value_per_share.clone()),
                    Series::reference("normalised", record.normalised.clone()),
                ],
                |value| fmt::number(value, 4),
            ),
            ui::notice(
                Tone::Info,
                "What this record is",
                copy::CONNECTED_RECORD_NOTE,
            ),
            if agent.days_live < Strategy::DAYS_TO_RANK {
                ui::notice(
                    Tone::Warn,
                    "Labelled new",
                    &format!(
                        "It has run for {}. The new label comes off, and it is ranked, at {} live \
                         days — the same rule vaults follow.",
                        fmt::days(agent.days_live),
                        Strategy::DAYS_TO_RANK
                    ),
                )
            } else {
                html!("span")
            },
        ],
    )
}

fn custody(connected: &'static Connected) -> Dom {
    ui::section(
        "What the broker will not let it do",
        vec![
            html!("div", {
                .class(["grid", "grid--2"])
                .child(ui::card(vec![
                    ui::subheading("It can"),
                    ui::bullets(
                        connected
                            .broker_limits
                            .iter()
                            .filter(|limit| limit.contains("can "))
                            .cloned(),
                    ),
                ]))
                .child(ui::card(vec![
                    ui::subheading("It cannot"),
                    ui::bullets(
                        connected
                            .broker_limits
                            .iter()
                            .filter(|limit| limit.contains("cannot"))
                            .cloned(),
                    ),
                ]))
            }),
            ui::note(
                "This is the same shape as a trade-only key at a venue, enforced by a broker \
                 instead. Your protection is the broker's own rule and your ability to revoke, \
                 not a contract of ours — and that is worth knowing rather than glossing over.",
            ),
        ],
    )
}

fn requirements(connected: &'static Connected) -> Dom {
    ui::section(
        "What you need to connect",
        vec![ui::bullets(connected.requirements.iter().cloned())],
    )
}

fn fees(connected: &'static Connected) -> Dom {
    ui::section(
        "What it costs",
        vec![
            fee_table(),
            ui::note(&format!(
                "Charged to the tank on your account's entry rather than minted as shares, from \
                 each daily statement posted there. Running costs for this strategy come to \
                 about {} a month; a broker account needs no gas. Deposits and withdrawals you \
                 make at the broker are read from its transfer history and netted out of the \
                 gain, because they aren't performance.",
                fmt::money_exact(connected.monthly_cost)
            )),
        ],
    )
}

fn limits(agent: &'static Strategy) -> Dom {
    if agent.limits.is_empty() {
        return html!("span");
    }
    ui::section(
        "What it will not do",
        vec![
            ui::bullets(agent.limits.iter().cloned()),
            ui::subheading("What it depends on"),
            ui::bullets(agent.dependencies.iter().cloned()),
            ui::note(
                "One strategy runs per connected account, and the fee is on the account's whole \
                 gain — so trading the same account by hand makes the record and the fee mean \
                 something else.",
            ),
        ],
    )
}

fn identity(agent: &'static Strategy) -> Dom {
    ui::card_titled(
        "Which code would be trading",
        None,
        vec![
            ui::kv_rows(vec![
                ("Bundle".to_string(), ui::hash(&agent.bundle.bundle_hash)),
                ("Source".to_string(), ui::value(agent.bundle.source.label())),
                ("Review".to_string(), parts::review_badge(agent)),
                (
                    "Secrets it reads".to_string(),
                    ui::muted(&agent.bundle.secrets.join(", ")),
                ),
            ]),
            ui::note(
                "Your token is sealed to that bundle hash, so only an attested executor running \
                 exactly this code can read it. Nobody, including us, can.",
            ),
        ],
    )
}

// ------------------------------------------------------------ owner's view

fn owner_panel(agent: &'static Strategy, connected: &'static Connected) -> Dom {
    let slug = agent.slug.clone();

    html!("div", {
        .child_signal(wallet::holdings().signal_cloned().map(clone!(slug => move |holdings| {
            Some(match holdings.account(&slug) {
                Some(account) if !account.revoked => running(agent, account),
                Some(account) => closed(account),
                None => not_connected(connected),
            })
        })))
    })
}

fn not_connected(connected: &'static Connected) -> Dom {
    ui::card_titled(
        "Connect your account",
        None,
        vec![
            ui::para(&format!(
                "You keep your account at {}. The strategy gets a token that can place orders \
                 and nothing else.",
                connected.broker
            )),
            shell::button("Connect an account", true, || step().set(Some(0))),
            ui::note(
                "Three steps: authorise once at the broker on your own machine, seal the token \
                 to the bundle hash, then create the entry and fund its tank.",
            ),
        ],
    )
}

fn running(agent: &'static Strategy, account: &Account) -> Dom {
    let slug = agent.slug.clone();
    let months = account.tank / agent.connected().map_or(1.0, |c| c.monthly_cost.max(0.01));

    ui::card_titled(
        "Your account",
        Some(ui::badge_dot("running", Tone::Ok)),
        vec![
            ui::kv_rows(vec![
                (
                    "Value at the last read".to_string(),
                    ui::number(&fmt::money_exact(account.value)),
                ),
                ("Entry".to_string(), ui::address(&account.entry)),
                (
                    "Tank".to_string(),
                    html!("div", {
                        .child(ui::number(&fmt::money_exact(account.tank)))
                        .child(html!("div", {
                            .class(["faint", "small"])
                            .text(&format!("about {months:.0} months of running costs"))
                        }))
                    }),
                ),
                (
                    "Connected".to_string(),
                    ui::value(&fmt::days(account.connected_days)),
                ),
            ]),
            html!("div", {
                .class(["row", "row--wrap"])
                .child(shell::button("Top up the tank", false, clone!(slug => move || {
                    match wallet::fund_tank(&slug, 60.0) {
                        Ok(()) => state::notify("Tank topped up by $60.00.", Tone::Ok),
                        Err(problem) => state::notify(problem, Tone::Warn),
                    }
                })))
                .child(html!("button", {
                    .class(["btn", "btn--danger"])
                    .attr("type", "button")
                    .text("Revoke and close")
                    .event(clone!(slug => move |_: events::Click| {
                        match wallet::revoke_account(&slug) {
                            Ok(returned) => state::notify(
                                format!(
                                    "Revoked at the broker. {} came back from the tank; any open \
                                     positions are yours.",
                                    fmt::money_exact(returned)
                                ),
                                Tone::Ok,
                            ),
                            Err(problem) => state::notify(problem, Tone::Warn),
                        }
                    }))
                }))
            }),
            ui::note(
                "Revoking happens at your broker and needs nothing from the strategy. Closing \
                 the entry returns what's left of the tank; open positions stay yours to keep or \
                 close.",
            ),
            statements(account),
        ],
    )
}

fn statements(account: &Account) -> Dom {
    let daily = account.value * 0.005 / 365.0;
    let rows: Vec<Row> = (0..4)
        .map(|day| {
            Row::new(vec![
                ui::muted(&fmt::date(data::days_ago(day))),
                ui::number(&fmt::money_exact(account.value)),
                ui::number(&fmt::money_exact(daily)),
            ])
        })
        .collect();

    html!("div", {
        .class("stack-sm")
        .child(ui::subheading("Posted to your entry"))
        .child(table(
            vec![
                Column::new("Day"),
                Column::num("Attested read"),
                Column::num("Fees charged"),
            ],
            rows,
        ))
        .child(ui::note(
            "An hourly attested read of the account is posted to its entry, and a fee statement \
             is posted daily and charged to the tank. That series is what the record and the \
             high-water mark are computed from.",
        ))
    })
}

fn closed(account: &Account) -> Dom {
    let slug = account.slug.clone();
    ui::card_titled(
        "Your account",
        Some(ui::badge("revoked", Tone::Neutral)),
        vec![
            ui::para(
                "The strategy's access was revoked at your broker and the entry closed. Nothing \
                 of ours can reach the account, and whatever it holds is yours.",
            ),
            shell::button("Remove this entry", false, move || {
                wallet::close_account(&slug);
            }),
        ],
    )
}

// ----------------------------------------------------------- connect flow

fn connect_flow(agent: &'static Strategy, connected: &'static Connected) -> Dom {
    let open = Mutable::new(false);
    let watcher = step().signal().for_each(clone!(open => move |current| {
        open.set_neq(current.is_some());
        async {}
    }));

    html!("div", {
        .future(watcher)
        .child(modal(&open, "Connect your account", true, move || {
            let current = step().get().unwrap_or(0);
            vec![
                steps_list(current),
                step_body(agent, connected, current),
            ]
        }))
    })
}

fn steps_list(current: u8) -> Dom {
    let labels = [
        "Authorise at the broker",
        "Seal the token to the bundle",
        "Create the entry and fund its tank",
    ];

    html!("ol", {
        .class("steps")
        .children(labels.into_iter().enumerate().map(|(index, label)| {
            let index = index as u8;
            html!("li", {
                .class("steps__item")
                .apply_if(index < current, |dom| dom.class("is-done"))
                .apply_if(index == current, |dom| dom.class("is-current"))
                .child(html!("div", {
                    .class("steps__title")
                    .apply_if(index > current, |dom| dom.class("faint"))
                    .text(label)
                }))
            })
        }))
    })
}

fn step_body(agent: &'static Strategy, connected: &'static Connected, current: u8) -> Dom {
    match current {
        0 => html!("div", {
            .class("stack")
            .child(ui::para(&format!(
                "You complete {}'s authorisation once, on your own machine. It grants a token \
                 that can place orders in your agentic account and cannot withdraw, transfer, \
                 stake or lend.",
                connected.broker
            )))
            .child(ui::notice(
                Tone::Info,
                "This part never touches us",
                "The authorisation happens between you and your broker. What reaches Shonin is a \
                 token you then seal to a specific bundle hash — and a sealed value can only be \
                 read inside an attested enclave running exactly that bundle.",
            ))
            .child(modal_actions(vec![
                shell::button("Cancel", false, || step().set(None)),
                shell::button("I've authorised at the broker", true, || step().set(Some(1))),
            ]))
        }),
        1 => html!("div", {
            .class("stack")
            .child(ui::para(
                "Your browser encrypts the token to the key published for this bundle, so the \
                 plaintext never leaves your machine.",
            ))
            .child(ui::kv(vec![
                ("Strategy".to_string(), ui::value(&agent.name())),
                (
                    "Sealing to".to_string(),
                    ui::hash(&agent.bundle.bundle_hash),
                ),
                (
                    "Secret name".to_string(),
                    ui::muted(
                        &agent
                            .bundle
                            .secrets
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "broker_refresh_token".to_string()),
                    ),
                ),
            ]))
            .child(ui::note(
                "You can rotate this token later without a new version of the strategy: a \
                 secret is not part of the bundle hash, only sealed to it.",
            ))
            .child(modal_actions(vec![
                shell::button("Back", false, || step().set(Some(0))),
                shell::button("Seal the token", true, || step().set(Some(2))),
            ]))
        }),
        _ => html!("div", {
            .class("stack")
            .child(ui::para(
                "Your account becomes an entry on the registry: an opaque account id, the \
                 strategy's bundle hash, your wallet, and a tank for running costs. Every hourly \
                 attested read and daily fee statement is posted there, so the record and the \
                 ledger are on-chain.",
            ))
            .child(form::field(
                "What your account is worth",
                Some("Read from the broker at the first attested read"),
                form::amount(account_value(), "USD", 250_000.0),
            ))
            .child(form::field(
                "Fund the tank",
                Some(&format!(
                    "About {} a month for this strategy; a broker account needs no gas",
                    fmt::money_exact(connected.monthly_cost)
                )),
                form::amount(tank_amount(), "USDC", 1_000.0),
            ))
            .child(ui::note(
                "A tank that runs dry stops the strategy at the end of the day. That is a halt \
                 for one account and nothing more: you can trade or revoke at any time.",
            ))
            .child(modal_actions(vec![
                shell::button("Back", false, || step().set(Some(1))),
                shell::button("Create the entry", true, move || {
                    let value = form::parse_amount(&account_value().get_cloned());
                    let tank = form::parse_amount(&tank_amount().get_cloned());
                    match wallet::connect_account(&agent.slug, value, tank) {
                        Ok(()) => {
                            step().set(None);
                            state::notify(
                                "Connected. The first attested read is posted within the hour.",
                                Tone::Ok,
                            );
                        }
                        Err(problem) => state::notify(problem, Tone::Warn),
                    }
                }),
            ]))
        }),
    }
}
