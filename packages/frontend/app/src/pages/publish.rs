//! Publishing an agent: submit it for $1, follow the job, and activate it from
//! the job's page once it is reviewed and built.

use std::sync::OnceLock;

use dominator::{Dom, html};
use futures_signals::signal::{Mutable, SignalExt};
use shared::Job;

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::state;
use crate::ui::form;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
use crate::wallet;

struct Draft {
    sealed: Mutable<bool>,
    archive: Mutable<Option<form::Picked>>,
    /// Activating a ready agent.
    tank: Mutable<String>,
    allowance: Mutable<String>,
}

fn state_of() -> &'static Draft {
    static DRAFT: OnceLock<Draft> = OnceLock::new();
    DRAFT.get_or_init(|| Draft {
        sealed: Mutable::new(false),
        archive: Mutable::new(None),
        tank: Mutable::new(String::new()),
        allowance: Mutable::new("modest".to_string()),
    })
}

pub fn render() -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(header())
        .child(how_it_goes())
        .child(html!("div", {
            .class(["grid", "grid--aside"])
            .child(html!("div", {
                .class("stack-lg")
                .child(submit())
            }))
            .child(html!("div", {
                .class("stack")
                .child(costs())
                .child(sealed_note())
            }))
        }))
    })
}

fn header() -> Dom {
    html!("div", {
        .class("stack")
        .child(html!("h1", { .class("title").text("Publish an agent") }))
        .child(html!("p", {
            .class("lede")
            .text(
                "You submit a Rust project for $1; the reviewer reads it, then we compile it. \
                 Every agent has source behind it that the reviewer read before it was built."
            )
        }))
    })
}

fn how_it_goes() -> Dom {
    ui::section(
        "How it goes",
        vec![html!("ol", {
            .class("steps")
            .children(copy::publish_steps().into_iter().map(|(title, body)| {
                html!("li", {
                    .class(["steps__item", "is-done"])
                    .child(html!("div", { .class("steps__title").text(title) }))
                    .child(html!("p", { .class("muted").text(&body) }))
                })
            }))
        })],
    )
}

fn submit() -> Dom {
    let draft = state_of();

    ui::section(
        "Submit a project",
        vec![
            html!("div", {
                .class("stack")
                .child(ui::note(
                    "A submission is your committed project, packed with git so it holds exactly \
                     what you committed. Pack it from the root of your repository, then drop the \
                     archive here.",
                ))
                .child(ui::code_block("git archive --format=tar -o agent.tar HEAD"))
                .child(form::file_drop(
                    "Drop the packed project, or choose it",
                    &draft.archive,
                ))
                .child(html!("div", {
                    .child_signal(draft.archive.signal_cloned().map(|picked| {
                        Some(declared(picked.as_ref()))
                    }))
                }))
                .child(form::toggle("Seal the source", &draft.sealed))
            }),
            shell::button("Upload and pay $1", true, submit_it),
            ui::action_link("/queue", "Watch the queue"),
        ],
    )
}

/// Upload the project and pay to publish it, from the connected wallet.
fn submit_it() {
    let draft = state_of();
    let Some(archive) = draft.archive.get_cloned() else {
        state::notify(
            "Pack the project and drop the archive here first.",
            Tone::Warn,
        );
        return;
    };
    let Some(manifest) = shared::archive::manifest_in(&archive.bytes) else {
        state::notify("There is no valid shonin.toml in that archive.", Tone::Warn);
        return;
    };
    let Ok(chain) = manifest.vault_chain() else {
        state::notify(
            "shonin.toml names a vault chain the platform doesn't have.",
            Tone::Warn,
        );
        return;
    };
    let sealed = draft.sealed.get();
    wasm_bindgen_futures::spawn_local(async move {
        match wallet::submit(chain, &archive.bytes, sealed).await {
            Ok(job) => {
                state::notify(
                    format!(
                        "Submitted as job {job}. The reviewer reads it first, then it is built; \
                         its page shows how it ends."
                    ),
                    Tone::Ok,
                );
                dominator::routing::go_to_url(&format!("/queue/{job}"));
            }
            Err(problem) => state::notify(problem, Tone::Warn),
        }
    });
}

/// What the dropped project declares about itself, read back so the author
/// sees what the reviewer will check the code against.
fn declared(picked: Option<&form::Picked>) -> Dom {
    let Some(picked) = picked else {
        return html!("div");
    };
    let Some(manifest) = shared::archive::manifest_in(&picked.bytes) else {
        return ui::notice(
            Tone::Warn,
            "No valid shonin.toml in that archive",
            "A submission is a project with a shonin.toml naming its vault chain, its description \
             and claims, and the drivers it trades through.",
        );
    };
    ui::note(&format!(
        "This project publishes \"{}\", with its vault on {}, trading through {}.",
        manifest.description.title,
        manifest.vault.chain,
        manifest
            .drivers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Activating a ready agent, from its job's page. Only its author can; anyone
/// else is told so by the contract.
pub fn activate_panel(job: &Job) -> Dom {
    let draft = state_of();
    let Some(id) = job.strategy_id else {
        return html!("span");
    };
    let chain = job.chain;
    let hint = match data::terms() {
        Some(terms) => format!(
            "At least {}; a week at the allowances is a sensible start",
            fmt::money_exact(terms.minimum_tank)
        ),
        None => copy::REACHING.to_string(),
    };
    ui::section(
        "Activate it",
        vec![
            ui::para(
                "Read the review below first. Activating opens the vault for deposits: choose \
                 what the agent may use, which is the most its tank can be charged, and fund the \
                 tank. You can change the allowances and top up the tank later.",
            ),
            form::field(
                "Allowances",
                Some("What the agent may use, metered to its vault's tank"),
                form::select(
                    shared::Allowances::SHAPES
                        .iter()
                        .map(|shape| ((*shape).to_string(), label(shape)))
                        .collect(),
                    &draft.allowance,
                ),
            ),
            form::field(
                "Starting tank",
                Some(&hint),
                form::amount(&draft.tank, chain.dollar(), 5_000.0),
            ),
            html!("div", {
                .child_signal(draft.allowance.signal_cloned().map(|allowance| {
                    Some(ui::muted(&format!(
                        "A week at these allowances: {}",
                        fmt::money_exact(daily(&allowance) * 7.0)
                    )))
                }))
            }),
            shell::button("Activate", true, move || {
                let draft = state_of();
                let tank = form::parse_amount(&draft.tank.get_cloned());
                let allowance = draft.allowance.get_cloned();
                if let Some(terms) = data::terms()
                    && tank < terms.minimum_tank
                {
                    state::notify(
                        format!(
                            "The tank needs at least {}.",
                            fmt::money_exact(terms.minimum_tank)
                        ),
                        Tone::Warn,
                    );
                    return;
                }
                wasm_bindgen_futures::spawn_local(async move {
                    match wallet::activate(chain, id, &allowance, tank).await {
                        Ok(slug) => {
                            state::notify("Activated. The vault is open for deposits.", Tone::Ok);
                            dominator::routing::go_to_url(&format!("/agents/{slug}"));
                        }
                        Err(problem) => state::notify(problem, Tone::Warn),
                    }
                });
            }),
        ],
    )
}

/// What each shape is called on the page, and what a day of it costs.
fn label(shape: &str) -> String {
    let what = match shape {
        "quoting" => "Quoting: fuel and bandwidth",
        "thinking" => "With models: inference and bandwidth",
        _ => "Modest: a slow carry agent",
    };
    format!("{what} ({} a day)", fmt::money_exact(daily(shape)))
}

/// What a day at a shape of allowances costs, at the platform's published
/// prices: the same arithmetic the vault's contract caps a bill with.
fn daily(allowance: &str) -> f64 {
    shared::Prices::published().daily_cap(&shared::Allowances::shape(allowance))
}

fn costs() -> Dom {
    ui::card_titled(
        "What publishing costs",
        None,
        vec![
            ui::para(
                "A flat $1 to submit, covering the review and the build. It is spent whatever the \
                 outcome, so a refusal or a failed build costs the same, and nothing earns a free \
                 retry.",
            ),
            ui::para(
                "When you activate, a starting tank pays the agent's running costs from then on.",
            ),
            ui::note(copy::TANK_NOTE),
            ui::kv_rows(vec![(
                "Jobs in the queue".to_string(),
                ui::number(&data::jobs().len().to_string()),
            )]),
        ],
    )
}

fn sealed_note() -> Dom {
    ui::card_titled(
        "Published or sealed",
        None,
        vec![ui::para(
            "Source is public by default. Sealing it changes who can read it, not whether the \
             reviewer read it or the builder compiled it. Its report names no file and quotes no \
             code.",
        )],
    )
}
