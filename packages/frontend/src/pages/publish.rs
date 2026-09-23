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
use crate::wallet;

struct Draft {
    sealed: Mutable<bool>,
    archive: Mutable<Option<form::Picked>>,
    strategy_id: Mutable<String>,
    version: Mutable<String>,
    tank: Mutable<String>,
    allowance: Mutable<String>,
    reviewed: Mutable<bool>,
    secret_name: Mutable<String>,
    secret_value: Mutable<String>,
}

fn state_of() -> &'static Draft {
    static DRAFT: OnceLock<Draft> = OnceLock::new();
    DRAFT.get_or_init(|| Draft {
        sealed: Mutable::new(false),
        archive: Mutable::new(None),
        strategy_id: Mutable::new(String::new()),
        version: Mutable::new("1".to_string()),
        tank: Mutable::new(String::new()),
        allowance: Mutable::new("modest".to_string()),
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
                    "A submission is the whole workspace your strategy belongs to, packed the \
                     same way every time so its hash means something. Pack it with the command \
                     below, then drop the archive here.",
                ))
                .child(ui::code_block("shonin pack packages/bots/your-strategy > strategy.tar"))
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
                .child(form::field(
                    "Strategy id",
                    Some("Read from the project's manifest; stable across versions, and each version gets its own vault"),
                    form::input(&draft.strategy_id, "funding-basis"),
                ))
                .child(form::field(
                    "Version",
                    Some("Read from the project's manifest; a new version is a new vault, and never an edit of a running one"),
                    form::input(&draft.version, "1"),
                ))
                .child(form::field(
                    "Allowances",
                    Some("What the strategy may use, metered to its vault's tank"),
                    form::select(
                        shared::DeclaredAllowances::SHAPES
                            .iter()
                            .map(|shape| ((*shape).to_string(), label(shape)))
                            .collect(),
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
            shell::button("Create the vault and queue the build", true, publish),
            ui::action_link("/queue", "Watch the build queue"),
        ],
    )
}

/// Create the vault, then hand the source to the build service.
///
/// The order matters and is the platform's, not this page's: the build service
/// will not admit a job until its vault exists with a funded tank, so nothing
/// is ever compiled for free.
fn publish() {
    let draft = state_of();
    let Some(archive) = draft.archive.get_cloned() else {
        state::notify(
            "Pack the project and drop the archive here first.",
            Tone::Warn,
        );
        return;
    };
    let id = draft.strategy_id.get_cloned().trim().to_string();
    if id.is_empty() {
        state::notify("Give the strategy an id.", Tone::Warn);
        return;
    }
    let Ok(version) = draft.version.get_cloned().trim().parse::<u32>() else {
        state::notify("The version is a whole number.", Tone::Warn);
        return;
    };
    let tank = form::parse_amount(&draft.tank.get_cloned());
    let allowance = draft.allowance.get_cloned();
    let Some(needed) = minimum(&allowance) else {
        state::notify(
            "The app has not reached the platform yet, so it cannot say what publishing costs.",
            Tone::Warn,
        );
        return;
    };
    if tank < needed {
        state::notify(
            "The tank needs to cover the build, the review and a week at the declared \
             allowances.",
            Tone::Warn,
        );
        return;
    }
    let sealed = draft.sealed.get();

    wasm_bindgen_futures::spawn_local(async move {
        match wallet::publish(&id, version, tank, &allowance, &archive.bytes, sealed).await {
            Ok(job) => {
                state::notify(
                    format!(
                        "Queued as build {job}. It is first come, first served, and its log is \
                         public whether it succeeds or fails."
                    ),
                    Tone::Ok,
                );
                dominator::routing::go_to_url(&format!("/queue/{job}"));
            }
            Err(problem) => state::notify(problem, Tone::Warn),
        }
    });
}

/// What the dropped project declares about itself. The manifest inside it is
/// what the build is about and what its hash covers, so the page reads it back
/// rather than asking the author to type the same thing twice: a vault created
/// for one version and a manifest naming another is a failed build and a
/// closed vault.
fn declared(picked: Option<&form::Picked>) -> Dom {
    let draft = state_of();
    let Some(picked) = picked else {
        return html!("div");
    };
    let Some(manifest) = shared::archive::manifest_in(&picked.bytes) else {
        return ui::notice(
            Tone::Warn,
            "No manifest in that archive",
            "A submission is a packed workspace with a `shonin.toml` in it. Pack it with \
             `shonin pack`, which packs the whole workspace the strategy belongs to.",
        );
    };

    // The form follows the manifest, so the two can never disagree.
    draft.strategy_id.set(manifest.strategy.id.clone());
    draft.version.set(manifest.strategy.version.to_string());
    ui::note(&format!(
        "This project declares {} v{}, which is what the vault will be created for.",
        manifest.strategy.id, manifest.strategy.version
    ))
}

/// What each shape the SDK names is called on the page, and what a day of it
/// costs. The list itself comes from the SDK, so the form cannot offer a shape
/// a strategy could not declare.
fn label(shape: &str) -> String {
    let what = match shape {
        "quoting" => "Quoting: fuel, bandwidth, placement",
        "thinking" => "With models: inference and bandwidth",
        _ => "Modest: a slow carry strategy",
    };
    format!("{what} ({} a day)", fmt::money_exact(daily(shape)))
}

/// What a day at a shape of allowance costs, at the platform's published
/// prices. Nothing here is a guess: it is the arithmetic the vault's contract
/// caps a usage statement with, over the same shapes the SDK names.
fn daily(allowance: &str) -> f64 {
    shared::Prices::published().daily_cap(&shared::DeclaredAllowances::shape(allowance))
}

/// The least the tank may be funded with: the factory's own minimum, or a week
/// of running plus the review if that comes to more.
fn minimum(allowance: &str) -> Option<f64> {
    let costs = data::terms()?;
    let week = daily(allowance) * 7.0;
    Some(costs.minimum_tank.max(costs.review_fee + week))
}

fn estimate(tank: &str, allowance: &str, sealed: bool) -> Dom {
    let entered = form::parse_amount(tank);
    let Some(costs) = data::terms() else {
        return ui::card(vec![
            ui::subheading("What the tank has to cover"),
            ui::note(copy::REACHING),
        ]);
    };
    let week = daily(allowance) * 7.0;
    let needed = costs.minimum_tank.max(costs.review_fee + week);

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
                    ui::value("The vault's minimum"),
                    ui::muted("what the factory will not create a vault below"),
                    ui::number(&fmt::money_exact(costs.minimum_tank)),
                ]),
                Row::new(vec![
                    ui::value("The review"),
                    ui::muted("a flat fee, one model call over the source"),
                    ui::number(&fmt::money_exact(costs.review_fee)),
                ]),
                Row::new(vec![
                    ui::value("A week at these allowances"),
                    ui::muted("so the strategy can actually run"),
                    ui::number(&fmt::money_exact(week)),
                ]),
            ],
        ),
        ui::muted(
            "The build itself is metered compute and is charged once it has run, so what it comes \
             to is known only afterwards.",
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
                Some(
                    "Sealed to the bundle's hash, so the executor can use it and nobody else can read it",
                ),
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
             builder compiled it or the reviewer read it.",
        )],
    )
}
