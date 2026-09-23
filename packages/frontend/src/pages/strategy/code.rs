//! The code tab: the bundle, the project it was compiled from, and the log of
//! the build that made it.

use std::rc::Rc;

use std::sync::OnceLock;

use dominator::{Dom, clone, events, html};
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use shared::{LogLevel, Source, Strategy};

use crate::format as fmt;
use crate::ui::primitives as ui;
use crate::ui::primitives::Tone;
use crate::ui::table::{Column, Row, table};

/// Which file the source browser is showing.
fn open_file() -> &'static Mutable<String> {
    static OPEN: OnceLock<Mutable<String>> = OnceLock::new();
    OPEN.get_or_init(|| Mutable::new(String::new()))
}

pub fn render(strategy: &Rc<Strategy>) -> Dom {
    html!("div", {
        .class("stack-lg")
        .child(identity(strategy))
        .child(source(strategy))
        .child(models(strategy))
        .child(allowances(strategy))
        .child(build_log(strategy))
        .child(rebuilding(strategy))
    })
}

fn identity(strategy: &Rc<Strategy>) -> Dom {
    let bundle = &strategy.bundle;

    ui::section(
        "The bundle",
        vec![
            ui::note(
                "One WASM component plus any models it calls, compiled by the build service from the \
                 submitted project. An executor refuses anything whose hash differs.",
            ),
            ui::kv(vec![
                ("Bundle hash".to_string(), ui::hash(&bundle.bundle_hash)),
                (
                    "Component hash".to_string(),
                    ui::hash(&bundle.component_hash),
                ),
                ("Source hash".to_string(), ui::hash(&bundle.source_hash)),
                ("Manifest hash".to_string(), ui::hash(&bundle.manifest_hash)),
                ("Interface".to_string(), ui::value(&bundle.world)),
                ("Toolchain".to_string(), ui::value(&bundle.toolchain)),
                ("Builder image".to_string(), ui::hash(&bundle.builder_image)),
                (
                    "Built in".to_string(),
                    ui::value(&fmt::duration(bundle.built_in_seconds)),
                ),
            ]),
        ],
    )
}

fn source(strategy: &Rc<Strategy>) -> Dom {
    let held = strategy.clone();
    let bundle = &strategy.bundle;

    match &bundle.source {
        Source::Sealed => ui::section(
            "The source",
            vec![ui::notice(
                Tone::Info,
                "This strategy's source is sealed",
                "The project is stored encrypted and released only to attested builders, reviewers \
                 and executors. The build, the hashes and the review are the same as for \
                 published source; only who else can read it changes.",
            )],
        ),
        Source::Published { repo, commit } => {
            if bundle.source_files.is_empty() {
                return ui::section("The source", vec![ui::empty("No files were served.")]);
            }

            let first = bundle.source_files[0].path.clone();
            if open_file().get_cloned().is_empty()
                || !bundle
                    .source_files
                    .iter()
                    .any(|file| file.path == open_file().get_cloned())
            {
                open_file().set(
                    bundle
                        .source_files
                        .iter()
                        .find(|file| file.path == "src/lib.rs")
                        .map_or(first, |file| file.path.clone()),
                );
            }

            ui::section(
                "The source",
                vec![
                    ui::kv(vec![
                        ("Repository".to_string(), ui::value(repo)),
                        ("Commit".to_string(), ui::copyable(commit, commit)),
                    ]),
                    html!("div", {
                        .class(["grid", "grid--aside"])
                        .child(html!("div", {
                            .child_signal(map_ref! {
                                let path = open_file().signal_cloned() => {
                                    Some(file_body(&held, path))
                                }
                            })
                        }))
                        .child(html!("div", {
                            .class(["card", "filetree"])
                            .children(bundle.source_files.iter().map(|file| {
                                let path = file.path.clone();
                                html!("button", {
                                    .class("filetree__item")
                                    .attr("type", "button")
                                    .attr_signal("aria-current", open_file().signal_cloned().map(
                                        clone!(path => move |open| {
                                            if open == path { "true" } else { "false" }
                                        })
                                    ))
                                    .child(html!("span", { .text(&file.path) }))
                                    .child(html!("span", {
                                        .class("filetree__size")
                                        .text(&fmt::bytes(file.bytes))
                                    }))
                                    .event(clone!(path => move |_: events::Click| {
                                        open_file().set(path.clone());
                                    }))
                                })
                            }))
                        }))
                    }),
                ],
            )
        }
    }
}

fn file_body(strategy: &Rc<Strategy>, path: &str) -> Dom {
    let Some(file) = strategy
        .bundle
        .source_files
        .iter()
        .find(|file| file.path == path)
    else {
        return ui::empty("Pick a file.");
    };

    html!("div", {
        .class("stack-sm")
        .child(html!("div", {
            .class(["row", "row--between", "row--wrap"])
            .child(html!("span", { .class("num").text(&file.path) }))
            .child(ui::muted(&fmt::bytes(file.bytes)))
        }))
        .child(html!("pre", { .class("code").text(&file.body) }))
    })
}

fn models(strategy: &Rc<Strategy>) -> Dom {
    let models = &strategy.bundle.models;
    if models.is_empty() {
        return html!("span");
    }

    let rows: Vec<Row> = models
        .iter()
        .map(|model| {
            Row::new(vec![
                html!("span", { .class("table__primary").text(&model.name) }),
                ui::number(&model.opset.to_string()),
                ui::number(&format!("{:.0} MiB", model.size_mib)),
                ui::muted(&model.purpose),
                ui::hash(&model.hash),
            ])
        })
        .collect();

    ui::section(
        "Models in the bundle",
        vec![
            table(
                vec![
                    Column::new("Name"),
                    Column::num("Opset"),
                    Column::num("Size"),
                    Column::new("What it is for").wide(),
                    Column::new("Hash"),
                ],
                rows,
            ),
            ui::notice(
                Tone::Warn,
                "Weights are data, not source",
                "Nobody can read a model the way they read code. The review judges what the code \
                 does with its output, and says the model's behaviour is unverified.",
            ),
        ],
    )
}

fn allowances(strategy: &Rc<Strategy>) -> Dom {
    let allowances = &strategy.bundle.allowances;
    let secrets = &strategy.bundle.secrets;

    let mut children = vec![
        ui::kv(vec![
            (
                "Fuel".to_string(),
                ui::number(&format!(
                    "{} a second",
                    fmt::number(allowances.fuel_per_sec as f64, 0)
                )),
            ),
            (
                "Memory".to_string(),
                ui::number(&format!("{} MiB", allowances.memory_mib)),
            ),
            (
                "Bandwidth".to_string(),
                ui::number(&format!(
                    "{} requests a minute · {} a day · {} streams",
                    fmt::count(allowances.requests_per_min),
                    fmt::bytes(u32::try_from(allowances.bytes_per_day).unwrap_or(u32::MAX)),
                    allowances.streams
                )),
            ),
            (
                "Inference".to_string(),
                ui::number(&format!("{} ms a minute", allowances.inference_ms_per_min)),
            ),
            ("Placement".to_string(), ui::value(&allowances.placement)),
        ]),
        ui::note(
            "The compute this strategy declared: each one a cap, metered to the vault's tank at \
             published prices.",
        ),
    ];

    if !secrets.is_empty() {
        children.push(ui::subheading("Sealed secrets"));
        children.push(ui::bullets(secrets.iter().cloned()));
        children.push(ui::note(
            "Sealed to this bundle's hash, so only an attested executor running exactly this \
             bundle can read them.",
        ));
    }

    ui::section("What it may use", children)
}

fn build_log(strategy: &Rc<Strategy>) -> Dom {
    let lines: Vec<Dom> = strategy
        .bundle
        .build_log
        .iter()
        .enumerate()
        .map(|(index, line)| {
            html!("div", {
                .class("log__line")
                .apply(|dom| match line.level {
                    LogLevel::Error => dom.class("log__line--err"),
                    LogLevel::Warn => dom.class("log__line--warn"),
                    LogLevel::Plain => dom,
                })
                .child(html!("span", { .class("log__num").text(&(index + 1).to_string()) }))
                .child(html!("span", { .text(&line.text) }))
            })
        })
        .collect();

    ui::section(
        "The build log",
        vec![
            html!("div", { .class("log").children(lines) }),
            ui::note("Builds run offline on a pinned recipe, twice, and the bytes must match."),
        ],
    )
}

fn rebuilding(strategy: &Rc<Strategy>) -> Dom {
    if !strategy.bundle.source.is_published() {
        return html!("span");
    }

    ui::section(
        "Rebuilding it yourself",
        vec![
            ui::para(
                "The builder image and the recipe are public, so anyone can rebuild this strategy \
                 from its source and compare the result with the hash the registry accepted.",
            ),
            html!("pre", {
                .class("code")
                .text(&format!(
                    "# fetch the source the builder was given\nshonin-source {}\n\n\
                     # rebuild it with the image that signed the bundle\nbuilder {}\n\n\
                     # compare with what the registry holds\nexpect {}",
                    fmt::short_hash(&strategy.bundle.source_hash),
                    fmt::short_hash(&strategy.bundle.builder_image),
                    fmt::short_hash(&strategy.bundle.bundle_hash),
                ))
            }),
            ui::action_link("/verify", "Everything else anyone can check"),
        ],
    )
}
