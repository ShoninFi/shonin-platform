//! Publishing a strategy: what you submit, what it costs, the pre-review, and
//! sealing a secret to a bundle.

use std::sync::OnceLock;

use dominator::{Dom, html};
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};

use crate::copy;
use crate::data;
use crate::format as fmt;
use crate::state;
use crate::ui::form;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::shell;
use crate::ui::table::{Column, Row, table};

struct Draft {
    repo: Mutable<String>,
    commit: Mutable<String>,
    sealed: Mutable<bool>,
    upload: Mutable<Option<String>>,
    strategy_id: Mutable<String>,
    tank: Mutable<String>,
    allowance: Mutable<String>,
    reviewed: Mutable<bool>,
    secret_name: Mutable<String>,
    secret_value: Mutable<String>,
}

fn state_of() -> &'static Draft {
    static DRAFT: OnceLock<Draft> = OnceLock::new();
    DRAFT.get_or_init(|| Draft {
        repo: Mutable::new(String::new()),
        commit: Mutable::new(String::new()),
        sealed: Mutable::new(false),
        upload: Mutable::new(None),
        strategy_id: Mutable::new(String::new()),
        tank: Mutable::new(String::new()),
        allowance: Mutable::new("light".to_string()),
        reviewed: Mutable::new(false),
        secret_name: Mutable::new(String::new()),
        secret_value: Mutable::new(String::new()),
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
                .child(pre_review())
                .child(secrets())
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
        .child(html!("h1", { .class("title").text("Publish a strategy") }))
        .child(html!("p", {
            .class("lede")
            .text(
                "You submit a Rust project; we compile it. Every strategy has source behind it that \
                 the reviewer read."
            )
        }))
    })
}

fn how_it_goes() -> Dom {
    ui::section(
        "How it goes",
        vec![html!("ol", {
            .class("steps")
            .children(copy::PUBLISH_STEPS.iter().map(|(title, body)| {
                html!("li", {
                    .class(["steps__item", "is-done"])
                    .child(html!("div", { .class("steps__title").text(title) }))
                    .child(html!("p", { .class("muted").text(body) }))
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
                .child(form::toggle("Seal the source", &draft.sealed))
                .child(html!("div", {
                    .child_signal(draft.sealed.signal().map(|sealed| {
                        Some(if sealed {
                            html!("div", {
                                .class("stack")
                                .child(ui::note(
                                    "Upload the project encrypted instead of pointing us at a repository. Only who can \
                                     read it changes."
                                ))
                                .child(form::dropzone(
                                    "Drop an encrypted project archive, or choose one",
                                    &state_of().upload,
                                ))
                            })
                        } else {
                            html!("div", {
                                .class("stack")
                                .child(form::field(
                                    "Public repository",
                                    Some("We clone it and hash the tree"),
                                    form::input(&state_of().repo, "github.com/you/your-strategy"),
                                ))
                                .child(form::field(
                                    "Commit",
                                    Some("The exact commit to build"),
                                    form::input(&state_of().commit, "9f2ac1…"),
                                ))
                            })
                        })
                    }))
                }))
                .child(form::field(
                    "Strategy id",
                    Some("Stable across versions; each version gets its own vault"),
                    form::input(&draft.strategy_id, "funding-basis"),
                ))
                .child(form::field(
                    "Allowances",
                    Some("What the strategy may use, metered to its vault's tank"),
                    form::select(
                        vec![
                            ("light".to_string(), "Light: a slow carry strategy".to_string()),
                            ("thinking".to_string(), "With models: inference and bandwidth".to_string()),
                            ("quoting".to_string(), "Quoting: fuel, bandwidth, placement".to_string()),
                        ],
                        &draft.allowance,
                    ),
                ))
                .child(form::field(
                    "Fund the vault's tank",
                    Some("The build and the review are its first charges"),
                    form::amount(&draft.tank, "USDC", 5_000.0),
                ))
            }),
            html!("div", {
                .child_signal(map_ref! {
                    let tank = draft.tank.signal_cloned(),
                    let allowance = draft.allowance.signal_cloned(),
                    let sealed = draft.sealed.signal() => {
                        Some(estimate(tank, allowance, *sealed))
                    }
                })
            }),
            shell::button("Create the vault and queue the build", true, || {
                let tank = form::parse_amount(&state_of().tank.get_cloned());
                if tank < minimum(&state_of().allowance.get_cloned()) {
                    state::notify(
                        "The tank needs to cover the build, the review and a week at the \
                         declared allowances.",
                        Tone::Warn,
                    );
                    return;
                }
                state::notify(
                    "Queued. The build is first come, first served, and its log is public \
                     whether it succeeds or fails.",
                    Tone::Ok,
                );
            }),
            ui::action_link("/queue", "Watch the build queue"),
        ],
    )
}

fn minimum(allowance: &str) -> f64 {
    let week = match allowance {
        "quoting" => 34.60 * 7.0,
        "thinking" => 12.40 * 7.0,
        _ => 1.15 * 7.0,
    };
    // The metered build, the flat review fee, and a week of running costs.
    38.0 + 1.0 + week
}

fn estimate(tank: &str, allowance: &str, sealed: bool) -> Dom {
    let entered = form::parse_amount(tank);
    let needed = minimum(allowance);
    let week = needed - 39.0;

    ui::card(vec![
        ui::subheading("What the tank has to cover"),
        table(
            vec![
                Column::new("Charge").wide(),
                Column::new("Why"),
                Column::num("Amount"),
            ],
            vec![
                Row::new(vec![
                    ui::value("The build"),
                    ui::muted("metered compute, charged because it ran"),
                    ui::number(&fmt::money_exact(38.0)),
                ]),
                Row::new(vec![
                    ui::value("The review"),
                    ui::muted("a flat fee, one model call over the source"),
                    ui::number(&fmt::money_exact(1.0)),
                ]),
                Row::new(vec![
                    ui::value("A week at full allowances"),
                    ui::muted("so the strategy can actually run"),
                    ui::number(&fmt::money_exact(week)),
                ]),
            ],
        ),
        html!("div", {
            .class(["row", "row--between", "row--wrap"])
            .child(ui::muted("Minimum"))
            .child(ui::number(&fmt::money_exact(needed)))
        }),
        if entered >= needed {
            ui::notice(
                Tone::Ok,
                "That covers it",
                "Creating the vault and funding its tank happen in one transaction. If the build \
                 fails, the vault closes and you reclaim the rest.",
            )
        } else {
            ui::notice(
                Tone::Warn,
                "Not enough yet",
                &format!(
                    "The build service admits a job once its vault exists and the tank covers \
                     the minimum of {}.",
                    fmt::money_exact(needed)
                ),
            )
        },
        if sealed {
            ui::note(
                "A sealed project is uploaded encrypted. The attested builder and the reviewer still \
                 read it; nobody else does.",
            )
        } else {
            ui::note(
                "Anyone can rebuild published source with the builder image and compare the hash.",
            )
        },
    ])
}

fn pre_review() -> Dom {
    let draft = state_of();

    ui::section(
        "Run the review before you publish",
        vec![
            ui::para(
                "The reviewer's prompt and output schema are published, so you can run the same review \
                 yourself before anyone sees your strategy. It is unsigned and binds nobody.",
            ),
            html!("div", {
                .child_signal(draft.reviewed.signal().map(|reviewed| {
                    Some(if reviewed {
                        ui::card(vec![
                            html!("div", {
                                .class(["row", "row--wrap"])
                                .child(ui::score(78, shared::ScoreBand::Middling))
                                .child(ui::muted("2 findings · unsigned"))
                            }),
                            ui::bullets(vec![
                                "Unbounded risk — order size scales with the vault's value with \
                                 no ceiling (src/limits.rs:24).".to_string(),
                                "Missing failure handling — a rejected order is retried without \
                                 back-off (src/lib.rs:71).".to_string(),
                            ]),
                            ui::note(
                                "The same categories the signed review uses.",
                            ),
                        ])
                    } else {
                        ui::empty("Run it to see what a reviewer would say about your code.")
                    })
                }))
            }),
            shell::button("Run a pre-review", false, || {
                state_of().reviewed.set(true);
                state::notify(
                    "Pre-review finished. It is unsigned and binds nobody.",
                    Tone::Ok,
                );
            }),
        ],
    )
}

fn secrets() -> Dom {
    let draft = state_of();

    ui::section(
        "Seal a secret to your bundle",
        vec![
            ui::para(
                "An API key your strategy needs, sealed so that only an attested executor running \
                 exactly your bundle can read it. Your browser encrypts it, so the plaintext \
                 never leaves your machine.",
            ),
            form::field(
                "Name",
                Some("The name your code passes to the secrets import"),
                form::input(&draft.secret_name, "wire_api_key"),
            ),
            form::field(
                "Value",
                Some("Encrypted in your browser before anything is sent"),
                form::input(&draft.secret_value, "…"),
            ),
            shell::button("Seal it", false, || {
                let name = state_of().secret_name.get_cloned();
                if name.trim().is_empty() {
                    state::notify("Give the secret a name.", Tone::Warn);
                    return;
                }
                state_of().secret_value.set(String::new());
                state::notify(
                    format!("Sealed {name}. You can rotate it later without a new version."),
                    Tone::Ok,
                );
            }),
            ui::note(
                "Rotating a secret doesn't need a new version: it is sealed to the bundle hash, not \
                 part of it.",
            ),
        ],
    )
}

fn costs() -> Dom {
    ui::card_titled(
        "What publishing costs",
        None,
        vec![
            ui::para(
                "There is no publishing fee. Creating the vault funds its tank, and the build \
                 and the review are its first charges.",
            ),
            ui::note(copy::TANK_NOTE),
            ui::kv_rows(vec![(
                "Builds in the queue".to_string(),
                ui::number(&data::get().jobs.len().to_string()),
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
             builder compiled it or the reviewer read it.",
        )],
    )
}
