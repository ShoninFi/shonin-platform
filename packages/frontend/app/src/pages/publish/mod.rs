//! `/publish` (Publish-Desktop, Publish-Mobile): package a project, upload
//! it, choose public or sealed source, and pay $1 from the wallet that
//! uploaded it. The `shonin.toml` summary is read from the archive itself,
//! read-only, before anything is uploaded; a project the build service
//! would refuse for its size is refused here, in an alert, with nothing
//! uploaded or charged.

pub mod activate;
pub mod job;
pub mod parts;

use std::rc::Rc;

use alloy_primitives::B256;
use futures_signals::signal::{Mutable, Signal, SignalExt};
use shared::Chain;
use ui::icons::Icon;

use super::Page;
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::check::{checkbox, radio};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::route::DOCS;
use crate::tx::author::archive::{self, Refusal, Summary};
use crate::tx::author::run::Running;
use crate::view::{El, View, el, text};
use parts::{Names, wallet_gate};

/// The checks an author ticks before submitting. Nobody checks the ticks.
const CHECKS: [&str; 8] = [
    "cargo test passes.",
    "cargo build --release --locked --target wasm32-wasip2 finishes.",
    "Cargo.lock is committed.",
    "The toolchain is pinned in rust-toolchain.toml.",
    "It builds without the network, the clock or randomness.",
    "The source is 2 MB or less unpacked, every file counted.",
    "The build takes 20 minutes or less.",
    "I re-read shonin.toml. Its description and claims are what the Reviewer checks and what Depositors read.",
];

/// How to package a project the way the build service takes it.
const PACKAGE: &str = "git archive --format=tar -o agent.tar HEAD";

/// The file chosen, and what it holds.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone)]
struct Picked {
    name: String,
    bytes: Rc<Vec<u8>>,
    summary: Result<Summary, Refusal>,
}

/// Where the upload and payment are.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum Sending {
    Idle,
    /// Offering the project to the build service.
    Uploading {
        note: String,
    },
    /// Taken: pay against it from the same account.
    Uploaded {
        job: String,
        upload: B256,
        account: String,
        chain: Chain,
        /// What was offered: the archive's hash, sealed or not, and the
        /// package. Anything else chosen since is uploaded again.
        offered: String,
    },
    /// Refused or failed: nothing was charged.
    Refused(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Public,
    Sealed,
}

struct Draft {
    picked: Mutable<Option<Picked>>,
    /// Which of the project's agents, when it holds several.
    agent: Mutable<usize>,
    source: Mutable<Source>,
    checks: Vec<Mutable<bool>>,
    checks_open: Mutable<bool>,
    sending: Mutable<Sending>,
    running: Mutable<Option<Running>>,
    dragging: Mutable<bool>,
}

pub fn page() -> Page {
    let draft = Rc::new(Draft {
        picked: Mutable::new(None),
        agent: Mutable::new(0),
        source: Mutable::new(Source::Public),
        checks: CHECKS.iter().map(|_| Mutable::new(false)).collect(),
        checks_open: Mutable::new(false),
        sending: Mutable::new(Sending::Idle),
        running: Mutable::new(None),
        dragging: Mutable::new(false),
    });
    let body = el("div")
        .class("sh-author sh-publish")
        .child(
            el("div")
                .class("sh-author__head")
                .child(
                    el("div")
                        .class("sh-author__headtext")
                        .child(el("h1").class("sh-author__title").text("Publish an agent"))
                        .child(el("p").class("sh-author__lede").text(
                            "Submit your code for $1. We review it, build it once and seal it. You activate it when you’re happy.",
                        )),
                )
                .child(
                    el("div").class("sh-author__links").child(
                        Button::new(Kind::Quiet, "Publishing guide")
                            .size(Size::Small)
                            .icon_after(Icon::External)
                            .href(DOCS)
                            .build()
                            .attr("target", "_blank")
                            .attr("rel", "noopener noreferrer"),
                    ),
                ),
        )
        .child(
            el("div")
                .class("sh-publish__grid")
                .child(stepper(&draft))
                .child(
                    el("div")
                        .class("sh-card sh-publish__main")
                        .child(checklist(&draft))
                        .child(upload_section(&draft))
                        .child(seal_section(&draft))
                        .child(pay_section(&draft)),
                )
                .child(aside()),
        );
    Page {
        top: None,
        body: body.into(),
    }
}

// The stepper ------------------------------------------------------------------

fn stepper(draft: &Rc<Draft>) -> El {
    let draft = draft.clone();
    let state = futures_signals::map_ref! {
        let picked = draft.picked.signal_cloned(),
        let sending = draft.sending.signal_cloned(),
        let source = draft.source.signal() => {
            let uploaded = picked.as_ref().is_some_and(|picked| picked.summary.is_ok());
            let paying = matches!(sending, Sending::Uploading { .. } | Sending::Uploaded { .. });
            (uploaded, *source, paying, picked.as_ref().map(|picked| picked.name.clone()))
        }
    };
    let now = (false, Source::Public, false, None::<String>);
    el("nav")
        .class("sh-publish__steps")
        .attr("aria-label", "Publishing steps")
        .child_signal(
            Some(steps_list(&now)),
            state.map(|state| Some(steps_list(&state))),
        )
}

fn steps_list(state: &(bool, Source, bool, Option<String>)) -> View {
    let (uploaded, source, paying, name) = state;
    let current = if *paying {
        3
    } else if *uploaded {
        2
    } else {
        1
    };
    let steps = [
        ("Before you submit", "You tick these yourself".to_string()),
        (
            "Upload",
            name.clone()
                .unwrap_or_else(|| "A packed project".to_string()),
        ),
        (
            "Seal",
            match source {
                Source::Public => "Public source".to_string(),
                Source::Sealed => "Sealed source".to_string(),
            },
        ),
        ("Pay $1", "One transaction, or two".to_string()),
    ];
    el("ol")
        .class("sh-publish__steplist")
        .children(
            steps
                .into_iter()
                .enumerate()
                .map(|(index, (title, detail))| {
                    let done = index < current && index > 0 || (index == 0 && current > 1);
                    el("li")
                        .class("sh-publish__step")
                        .class_if(index == current, "sh-publish__step--now")
                        .class_if(done, "sh-publish__step--done")
                        .attr_if(index == current, "aria-current", "step")
                        .child(
                            el("span")
                                .class("sh-publish__stepmark")
                                .attr("aria-hidden", "true")
                                .child(if done {
                                    icon(Icon::Check, 14).into()
                                } else {
                                    text((index + 1).to_string())
                                }),
                        )
                        .child(
                            el("span")
                                .class("sh-publish__steptext")
                                .child(el("span").class("sh-publish__steptitle").text(title))
                                .child(el("span").class("sh-publish__stepdetail").text(detail)),
                        )
                }),
        )
        .into()
}

// 1 · Before you submit ---------------------------------------------------------

fn checklist(draft: &Rc<Draft>) -> El {
    let open = draft.checks_open.clone();
    let toggle = draft.checks_open.clone();
    let tally = tally_signal(&draft.checks);
    el("section")
        .class("sh-publish__section")
        .attr("aria-labelledby", "st-check")
        .child(
            el("div")
                .class("sh-publish__sectionhead")
                .child(section_mark(1))
                .child(
                    el("div")
                        .class("sh-publish__sectiontext")
                        .child(
                            el("h2")
                                .attr("id", "st-check")
                                .class("sh-publish__h2")
                                .text("Before you submit"),
                        )
                        .child(el("p").class("sh-muted").text_signal(
                            "0 of 8 checks ticked. You tick these yourself; nobody checks your ticks.",
                            tally.map(|n| {
                                format!(
                                    "{n} of 8 checks ticked. You tick these yourself; nobody checks your ticks."
                                )
                            }),
                        )),
                )
                .child(on_click(
                    el("button")
                        .attr("type", "button")
                        .class("sh-btn sh-btn-ghost sh-btn-sm")
                        .attr("aria-controls", "checklist")
                        .attr_from("aria-expanded", &open, |open| Some(open.to_string()))
                        .text_from(&open, |open| {
                            if *open { "Hide list" } else { "Show list" }.to_string()
                        }),
                    move || {
                        toggle.replace_with(|open| !*open);
                    },
                )),
        )
        .child(
            el("div")
                .attr("id", "checklist")
                .class("sh-publish__checklist")
                .attr_from("hidden", &open, |open| (!*open).then(String::new))
                .children(CHECKS.iter().enumerate().map(|(index, words)| {
                    checkbox(format!("check-{index}"), *words, &draft.checks[index])
                }))
                .child(el("p").class("sh-muted").text("Then package it:"))
                .child(el("code").class("sh-publish__code").text(PACKAGE)),
        )
}

/// How many of the checks are ticked, as they change.
fn tally_signal(checks: &[Mutable<bool>]) -> impl Signal<Item = usize> + 'static {
    checks.iter().fold(
        futures_signals::signal::always(0usize).boxed_local(),
        |sum, check| {
            futures_signals::map_ref! {
                let sum = sum,
                let ticked = check.signal() => *sum + usize::from(*ticked)
            }
            .boxed_local()
        },
    )
}

fn section_mark(n: u8) -> El {
    el("span")
        .class("sh-publish__mark")
        .attr("aria-hidden", "true")
        .text(n.to_string())
}

// 2 · Upload ---------------------------------------------------------------------

/// The agent the author is publishing: the one the project holds, or the
/// one they chose.
fn chosen(picked: &Picked, index: usize) -> Option<&archive::Agent> {
    let summary = picked.summary.as_ref().ok()?;
    summary.agents.get(index).or_else(|| summary.agents.first())
}

/// What stops this project being published as it is, in words.
fn problem(picked: &Picked, index: usize) -> Option<String> {
    let summary = picked.summary.as_ref().ok()?;
    let agent = chosen(picked, index)?;
    if summary.agents.len() > 1 && agent.package.is_none() {
        return Some(format!(
            "There's no Cargo.toml with a [package] beside {}. Put shonin.toml beside your agent's own Cargo.toml.",
            agent.path
        ));
    }
    let manifest = match &agent.manifest {
        Ok(manifest) => manifest,
        Err(why) => return Some(format!("{why}.")),
    };
    match manifest.vault_chain() {
        Err(_) => Some(format!(
            "shonin.toml puts the vault on “{}”, which isn't a chain the platform runs on.",
            manifest.vault.chain
        )),
        Ok(Chain::Solana) => {
            Some("Publishing an agent with its vault on Solana isn't in this app yet.".to_string())
        }
        Ok(_) => None,
    }
}

fn upload_section(draft: &Rc<Draft>) -> El {
    let view_draft = draft.clone();
    el("section")
        .class("sh-publish__section")
        .attr("aria-labelledby", "st-upload")
        .child(
            el("div")
                .class("sh-publish__sectionhead")
                .child(section_mark(2))
                .child(
                    el("div").class("sh-publish__sectiontext").child(
                        el("h2")
                            .attr("id", "st-upload")
                            .class("sh-publish__h2")
                            .text("Upload"),
                    ),
                )
                .child_from(&draft.picked, |picked| {
                    picked
                        .as_ref()
                        .filter(|picked| picked.summary.is_ok())
                        .map(|_| {
                            crate::atoms::badge::badge(
                                crate::atoms::badge::Tone::Gain,
                                Some(Icon::Check),
                                "Read and understood",
                            )
                            .into()
                        })
                }),
        )
        .child_signal(
            Some(dropzone(draft, None)),
            futures_signals::map_ref! {
                let picked = draft.picked.signal_cloned(),
                let agent = draft.agent.signal() => (picked.clone(), *agent)
            }
            .map(move |(picked, agent)| {
                Some(match picked {
                    None => dropzone(&view_draft, None),
                    Some(picked) => match &picked.summary {
                        Err(refusal) => dropzone(&view_draft, Some((&picked.name, refusal))),
                        Ok(summary) => read_back(&view_draft, &picked, summary, agent),
                    },
                })
            }),
        )
}

/// The drop zone: drop a file, or choose one. With a refusal, the refusal
/// first, as an alert: nothing was uploaded and nothing was charged.
fn dropzone(draft: &Rc<Draft>, refused: Option<(&str, &Refusal)>) -> View {
    let draft_drop = draft.clone();
    let draft_pick = draft.clone();
    let dragging = draft.dragging.clone();
    let (enter, leave) = (dragging.clone(), dragging.clone());
    el("div")
        .class("sh-publish__upload")
        .child(match refused {
            Some((name, refusal)) => el("div")
                .class("sh-publish__refused")
                .attr("role", "alert")
                .child(icon(Icon::Serious, 20))
                .child(
                    el("div")
                        .child(el("p").class("sh-publish__refusedtext").text(refusal.words(name)))
                        .child(el("p").text("Nothing was uploaded and nothing was charged.")),
                )
                .into(),
            None => View::Empty,
        })
        .child(
            el("div")
                .class("sh-publish__drop")
                .class_from("sh-publish__drop--over", &dragging, |over| *over)
                .child(icon(Icon::Upload, 24))
                .child(el("p").class("sh-publish__droptitle").text("Drop agent.tar here"))
                .child(
                    el("input")
                        .attr("type", "file")
                        .attr("id", "archive-file")
                        .attr("accept", ".tar,application/x-tar")
                        .attr("tabindex", "-1")
                        .attr("aria-hidden", "true")
                        .class("sh-publish__file")
                        .hook(move |dom| {
                            dominator::with_node!(dom, input => {
                                .event(move |_: dominator::events::Change| {
                                    #[cfg(target_arch = "wasm32")]
                                    browser::take_input(&draft_pick, &input);
                                    #[cfg(not(target_arch = "wasm32"))]
                                    let _ = (&draft_pick, &input);
                                })
                            })
                        }),
                )
                .child(
                    el("button")
                        .attr("type", "button")
                        .class("sh-btn sh-btn-secondary")
                        .text(if refused.is_some() {
                            "Choose another file"
                        } else {
                            "Choose file"
                        })
                        .hook(|dom| {
                            dom.event(|event: dominator::events::Click| {
                                // The file input sits beside the button.
                                if let Some(input) = event
                                    .dyn_target::<web_sys::Element>()
                                    .and_then(|button| button.parent_element())
                                    .and_then(|zone| zone.query_selector("#archive-file").ok().flatten())
                                    .and_then(|input| {
                                        wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(input).ok()
                                    })
                                {
                                    input.click();
                                }
                            })
                        }),
                )
                .child(el("p").class("sh-muted sh-publish__small").text(
                    "Up to 2 MB unpacked, every file counted. A project carries no .cargo directory.",
                ))
                .hook(move |dom| {
                    dom.event_with_options(
                        &dominator::EventOptions::preventable(),
                        move |event: dominator::events::DragOver| {
                            event.prevent_default();
                            enter.set_neq(true);
                        },
                    )
                    .event(move |_: dominator::events::DragLeave| leave.set_neq(false))
                    .event_with_options(
                        &dominator::EventOptions::preventable(),
                        move |event: dominator::events::Drop| {
                            event.prevent_default();
                            draft_drop.dragging.set_neq(false);
                            #[cfg(target_arch = "wasm32")]
                            if let Some(transfer) = event.data_transfer() {
                                browser::take_files(&draft_drop, transfer.as_ref());
                            }
                        },
                    )
                }),
        )
        .child(el("p").class("sh-muted").text("Package it from the root of your repository:"))
        .child(el("code").class("sh-publish__code").text(PACKAGE))
        .into()
}

/// The chosen file and what its `shonin.toml` says, read-only.
fn read_back(draft: &Rc<Draft>, picked: &Picked, summary: &Summary, index: usize) -> View {
    let replace = draft.clone();
    let agent = chosen(picked, index);
    let file = el("div")
        .class("sh-publish__filerow")
        .child(
            el("span")
                .class("sh-publish__fileicon")
                .attr("aria-hidden", "true")
                .child(icon(Icon::Code, 20)),
        )
        .child(
            el("div")
                .class("sh-publish__filetext")
                .child(
                    el("span")
                        .class("sh-mono sh-publish__filename")
                        .text(picked.name.clone()),
                )
                .child(el("span").class("sh-muted sh-publish__small").text(format!(
                    "{} · {} files · {} unpacked",
                    archive::size(summary.bytes as u64),
                    summary.files,
                    archive::size(summary.unpacked)
                ))),
        )
        .child(on_click(
            Button::new(Kind::Secondary, "Replace")
                .size(Size::Small)
                .icon_before(Icon::Upload)
                .build(),
            move || {
                replace.picked.set(None);
                replace.sending.set(Sending::Idle);
                replace.running.set(None);
            },
        ));
    let several = if summary.agents.len() > 1 {
        el("fieldset")
            .class("sh-fieldset sh-publish__agents")
            .child(
                el("legend")
                    .text("This project holds several agents. Which one are you publishing?"),
            )
            .children(summary.agents.iter().enumerate().map(|(at, agent)| {
                let title = agent
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.description.title.clone())
                    .unwrap_or_else(|_| "shonin.toml can't be read".to_string());
                let label = match &agent.package {
                    Some(package) => format!("{package} · {title}"),
                    None => format!("{} · {title}", agent.path),
                };
                radio("agent", format!("agent-{at}"), label, at, &draft.agent)
            }))
            .into()
    } else {
        View::Empty
    };
    let details = match agent.map(|agent| &agent.manifest) {
        Some(Ok(manifest)) => manifest_list(manifest),
        _ => View::Empty,
    };
    let problem = match problem(picked, index) {
        Some(why) => notice(NoticeTone::Danger, text(why))
            .attr("role", "alert")
            .into(),
        None => View::Empty,
    };
    el("div")
        .class("sh-publish__readback")
        .child(file)
        .child(several)
        .child(problem)
        .child(details)
        .into()
}

fn manifest_list(manifest: &shared::Manifest) -> View {
    let chain = manifest
        .vault_chain()
        .map(|chain| format!("{} ({})", chain.label(), chain.dollar()))
        .unwrap_or_else(|_| manifest.vault.chain.clone());
    let names =
        |map: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>| {
            if map.is_empty() {
                "None".to_string()
            } else {
                map.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        };
    let claims: View = if manifest.description.claims.is_empty() {
        el("span").text("None").into()
    } else {
        el("ol")
            .class("sh-publish__claims")
            .children(
                manifest
                    .description
                    .claims
                    .iter()
                    .map(|claim| el("li").text(format!("“{claim}”"))),
            )
            .into()
    };
    el("div")
        .class("sh-publish__manifest")
        .child(
            el("div")
                .class("sh-publish__manifesthead")
                .child(
                    el("h3")
                        .class("sh-publish__h3")
                        .text("What we read from ")
                        .child(el("span").class("sh-mono").text("shonin.toml")),
                )
                .child(el("span").class("sh-muted sh-publish__small").text("The Reviewer checks your code against this")),
        )
        .child(
            el("dl")
                .class("sh-publish__dl")
                .child(parts::row("Title", parts::words(manifest.description.title.clone())))
                .child(parts::row("Vault chain", parts::words(chain)))
                .child(parts::row("Description", parts::words(manifest.description.text.clone())))
                .child(parts::row(
                    &format!("Claims ({})", manifest.description.claims.len()),
                    claims,
                ))
                .child(parts::row("Drivers", parts::words(names(&manifest.drivers))))
                .child(parts::row("Bridges", parts::words(names(&manifest.bridges)))),
        )
        .child(el("p").class("sh-muted sh-publish__small").text(
            "This is fixed forever once published. Depositors see exactly this. To change it, edit shonin.toml, package again and replace the file.",
        ))
        .into()
}

// 3 · Seal -------------------------------------------------------------------------

fn seal_section(draft: &Rc<Draft>) -> El {
    let choice = |value: Source, id: &str, label: &str, lines: &[&str]| {
        el("div")
            .class("sh-publish__choice")
            .child(radio("source", id.to_string(), label, value, &draft.source))
            .children(lines.iter().map(|line| {
                el("p")
                    .class("sh-muted sh-publish__choiceline")
                    .text(line.to_string())
            }))
    };
    el("section")
        .class("sh-publish__section")
        .attr("aria-labelledby", "st-seal")
        .child(
            el("div")
                .class("sh-publish__sectionhead")
                .child(section_mark(3))
                .child(
                    el("div").class("sh-publish__sectiontext").child(
                        el("h2")
                            .attr("id", "st-seal")
                            .class("sh-publish__h2")
                            .text("Seal"),
                    ),
                ),
        )
        .child(
            el("fieldset")
                .class("sh-fieldset sh-publish__seal")
                .child(el("legend").text("Who can read your source code?"))
                .child(choice(
                    Source::Public,
                    "source-public",
                    "Public source · recommended for trust",
                    &[
                        "Anyone can read it and rebuild it to check.",
                        "Depositors can compare your code with what the Reviewer said about it.",
                    ],
                ))
                .child(choice(
                    Source::Sealed,
                    "source-sealed",
                    "Sealed",
                    &[
                        "Only our attested machines and the Reviewer's model can read it. Depositors rely on the review instead, and your page says so.",
                        "It's still reviewed and built the same way; it's just never published.",
                    ],
                )),
        )
}

// 4 · Pay $1 ---------------------------------------------------------------------------

/// The chain the chosen agent's vault is on, and that chain's network,
/// once the project can be published.
fn target(draft: &Draft) -> Option<Chain> {
    let picked = draft.picked.get_cloned()?;
    if problem(&picked, draft.agent.get()).is_some() {
        return None;
    }
    chosen(&picked, draft.agent.get())?
        .manifest
        .as_ref()
        .ok()?
        .vault_chain()
        .ok()
}

fn pay_section(draft: &Rc<Draft>) -> El {
    let session = crate::wallet::get();
    let view_draft = draft.clone();
    el("section")
        .class("sh-publish__section")
        .attr("aria-labelledby", "st-pay")
        .child(
            el("div")
                .class("sh-publish__sectionhead")
                .child(section_mark(4))
                .child(
                    el("div").class("sh-publish__sectiontext").child(
                        el("h2")
                            .attr("id", "st-pay")
                            .class("sh-publish__h2")
                            .text("Pay $1"),
                    ),
                ),
        )
        .child_signal(
            Some(pay_body(draft, None)),
            futures_signals::map_ref! {
                let _picked = draft.picked.signal_cloned(),
                let _agent = draft.agent.signal(),
                let networks = session.networks.signal_cloned() => {
                    target(&view_draft).map(|chain| (chain, networks.get(&chain).cloned()))
                }
            }
            .map({
                let draft = draft.clone();
                move |target| Some(pay_body(&draft, target))
            }),
        )
}

fn pay_body(
    draft: &Rc<Draft>,
    target: Option<(Chain, Option<Rc<crate::wallet::Network>>)>,
) -> View {
    let session = crate::wallet::get();
    let chain_label = target
        .as_ref()
        .map(|(chain, _)| chain.label().to_string())
        .unwrap_or_else(|| "the chain your vault will live on".to_string());
    let from = el("span").class("sh-mono").text_signal(
        "your wallet",
        session.connection.signal_cloned().map(|connection| {
            connection
                .account()
                .map(|account| crate::format::short_hash(&format!("{account:?}")))
                .unwrap_or_else(|| "your wallet".to_string())
        }),
    );
    let list = el("dl")
        .class("sh-publish__dl")
        .child(parts::row("Publishing fee", parts::words("$1.00")))
        .child(parts::row(
            "Covers",
            parts::words("The review and the build"),
        ))
        .child(parts::row(
            "Paid on",
            View::Many(vec![text(format!("{chain_label} from ")), from.into()]),
        ));
    let action: View = match target {
        None => Button::new(Kind::Primary, "Submit for $1")
            .size(Size::Large)
            .block()
            .disabled(true)
            .build()
            .into(),
        Some((_, None)) => notice(
            NoticeTone::Caution,
            text("This chain isn't reachable from the app right now. Try again shortly."),
        )
        .into(),
        Some((chain, Some(network))) => {
            let draft = draft.clone();
            let names = Rc::new(Names::of(&network));
            let busy = Mutable::new(false);
            let sending = draft.sending.clone();
            let running = draft.running.clone();
            let button_draft = draft.clone();
            let chain_id = network.chain_id;
            el("div")
                .class("sh-publish__payaction")
                .child(wallet_gate(chain_id, move || {
                    let draft = button_draft.clone();
                    let busy = busy.clone();
                    on_click(
                        Button::new(Kind::Primary, "Submit for $1")
                            .size(Size::Large)
                            .block()
                            .busy(&busy)
                            .build(),
                        move || submit(&draft, chain, &busy),
                    )
                    .into()
                }))
                .child_signal(
                    None,
                    sending.signal_cloned().map(|sending| match sending {
                        Sending::Uploading { note } => Some(
                            el("p")
                                .class("sh-publish__note")
                                .attr("role", "status")
                                .text(note)
                                .into(),
                        ),
                        Sending::Refused(why) => Some(
                            el("div")
                                .class("sh-author__error sh-author__error--alert")
                                .attr("role", "alert")
                                .child(
                                    el("p")
                                        .class("sh-author__steptitle")
                                        .text("The upload wasn't taken"),
                                )
                                .child(el("p").text(why))
                                .child(el("p").text("Nothing was charged."))
                                .into(),
                        ),
                        _ => None,
                    }),
                )
                .child_signal(
                    None,
                    running.signal_cloned().map(move |running| {
                        running.map(|running| parts::status(&running, names.clone()).into())
                    }),
                )
                .into()
        }
    };
    el("div")
        .class("sh-publish__pay")
        .child(list)
        .child(action)
        .child(el("p").class("sh-muted sh-publish__small").text(
            "One transaction on the chain your vault will live on, or two if your wallet hasn't allowed the $1 yet. Only a payment from the wallet that uploaded the project starts the job. You never compute a hash.",
        ))
        .into()
}

/// Upload (unless this account already did, for this chain) and pay.
fn submit(draft: &Rc<Draft>, chain: Chain, busy: &Mutable<bool>) {
    if busy.get()
        || draft
            .running
            .lock_ref()
            .as_ref()
            .is_some_and(Running::is_live)
    {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    browser::submit(draft.clone(), chain, busy.clone());
    #[cfg(not(target_arch = "wasm32"))]
    let _ = chain;
}

fn aside() -> El {
    let item = |title: &str, line: &str| {
        el("li")
            .class("sh-publish__next")
            .child(
                el("span")
                    .class("sh-publish__nexttitle")
                    .text(title.to_string()),
            )
            .child(el("span").class("sh-muted").text(line.to_string()))
    };
    el("aside")
        .class("sh-publish__aside")
        .child(
            el("div")
                .class("sh-card sh-card--pad sh-publish__asidecard")
                .child(el("h2").class("sh-publish__h2").text("What happens next"))
                .child(
                    el("ol")
                        .class("sh-publish__nextlist")
                        .child(item("Queue", "Jobs run in order. Your job page shows your place."))
                        .child(item(
                            "Review",
                            "The Reviewer reads every line and rates six areas. Code written to do harm is refused and never built.",
                        ))
                        .child(item(
                            "Build",
                            "An attested machine builds it once and registers the build. Up to 20 minutes.",
                        ))
                        .child(item(
                            "You activate",
                            "Nothing runs and nothing is listed until you do. There's no deadline.",
                        )),
                )
                .child(
                    notice(
                        NoticeTone::Info,
                        text("The $1 is spent whichever way it ends. To try again, you publish a new agent."),
                    )
                    .class("sh-notice--compact"),
                ),
        )
        .child(
            el("div")
                .class("sh-card sh-card--pad sh-publish__asidecard")
                .child(el("h2").class("sh-publish__h2").text("Once it's live"))
                .child(el("p").class("sh-muted").text(
                    "You earn a share of the new profit it makes, paid every Monday as shares. A tank you fill pays for its computing and gas.",
                )),
        )
}

// The browser: reading the file, uploading, paying ----------------------------------------

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::rc::Rc;

    use futures_signals::signal::Mutable;
    use js_sys::{Function, Promise, Reflect, Uint8Array};
    use shared::{Chain, Upload};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    use super::{Draft, Picked, Sending, Source};
    use crate::tx::author::Outcome;
    use crate::tx::author::archive;
    use crate::tx::author::run::{self, Action, Running, UploadError};

    /// The first file of an `<input type=file>`.
    pub fn take_input(draft: &Rc<Draft>, input: &web_sys::Element) {
        if let Ok(files) = Reflect::get(input.as_ref(), &JsValue::from_str("files")) {
            take(draft, files);
        }
        // Choosing the same file again fires a change too.
        let _ = Reflect::set(
            input.as_ref(),
            &JsValue::from_str("value"),
            &JsValue::from_str(""),
        );
    }

    /// The first file dropped.
    pub fn take_files(draft: &Rc<Draft>, transfer: &JsValue) {
        if let Ok(files) = Reflect::get(transfer, &JsValue::from_str("files")) {
            take(draft, files);
        }
    }

    fn take(draft: &Rc<Draft>, files: JsValue) {
        let Ok(file) = Reflect::get(&files, &JsValue::from_f64(0.0)) else {
            return;
        };
        if file.is_undefined() || file.is_null() {
            return;
        }
        let name = Reflect::get(&file, &JsValue::from_str("name"))
            .ok()
            .and_then(|name| name.as_string())
            .unwrap_or_else(|| "agent.tar".to_string());
        let draft = draft.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Some(bytes) = read(&file).await else {
                return;
            };
            let summary = archive::summarise(&bytes);
            draft.agent.set(0);
            draft.sending.set(Sending::Idle);
            draft.running.set(None);
            draft.picked.set(Some(Picked {
                name,
                bytes: Rc::new(bytes),
                summary,
            }));
        });
    }

    async fn read(file: &JsValue) -> Option<Vec<u8>> {
        let method: Function = Reflect::get(file, &JsValue::from_str("arrayBuffer"))
            .ok()?
            .dyn_into()
            .ok()?;
        let promise: Promise = method.call0(file).ok()?.dyn_into().ok()?;
        let buffer = JsFuture::from(promise).await.ok()?;
        Some(Uint8Array::new(&buffer).to_vec())
    }

    pub fn submit(draft: Rc<Draft>, chain: Chain, busy: Mutable<bool>) {
        let session = crate::wallet::get();
        let Some(account) = session.account() else {
            return;
        };
        let Some(picked) = draft.picked.get_cloned() else {
            return;
        };
        let Some(agent) = super::chosen(&picked, draft.agent.get()).cloned() else {
            return;
        };
        let several = picked
            .summary
            .as_ref()
            .map(|summary| summary.agents.len() > 1)
            .unwrap_or(false);
        let author = format!("{account:#x}");
        let sealed = draft.source.get() == Source::Sealed;
        let package = if several { agent.package.clone() } else { None };
        let offered = format!(
            "{}|{sealed}|{package:?}",
            alloy_primitives::keccak256(picked.bytes.as_slice())
        );
        busy.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            // An upload this account already made for this chain is paid
            // for as it is; anything else is uploaded first.
            let ready = match draft.sending.get_cloned() {
                Sending::Uploaded {
                    job,
                    upload,
                    account: by,
                    chain: on,
                    offered: what,
                } if by == author && on == chain && what == offered => Some((job, upload)),
                _ => None,
            };
            let (job, upload) = match ready {
                Some(ready) => ready,
                None => {
                    let offer = Upload {
                        author: author.clone(),
                        archive_base64: shared::base64::encode(&picked.bytes),
                        sealed,
                        package,
                    };
                    match offer_until_taken(&draft, chain, &offer).await {
                        Some(taken) => taken,
                        None => {
                            busy.set(false);
                            return;
                        }
                    }
                }
            };
            draft.sending.set(Sending::Uploaded {
                job: job.clone(),
                upload,
                account: author,
                chain,
                offered,
            });
            let running = Running::new();
            draft.running.set(Some(running.clone()));
            busy.set(false);
            run::start(chain, Action::Pay { upload }, &running, move |outcome| {
                if matches!(outcome, Outcome::Paid { .. }) {
                    dominator::routing::go_to_url(&format!(
                        "/publish/jobs/{}",
                        crate::route::encode(&job)
                    ));
                }
            });
        });
    }

    /// Offer the project until the build service takes it, waiting while it
    /// starts (as long as it asks, for up to ten minutes). The job id and
    /// the reference to pay against, or `None` with the reason set.
    async fn offer_until_taken(
        draft: &Rc<Draft>,
        chain: Chain,
        offer: &Upload,
    ) -> Option<(String, alloy_primitives::B256)> {
        let started = js_sys::Date::now();
        draft.sending.set(Sending::Uploading {
            note: "Uploading the project…".to_string(),
        });
        loop {
            match run::upload(chain, offer).await {
                Ok(taken) => {
                    let Ok(upload) = taken.upload.parse() else {
                        draft.sending.set(Sending::Refused(
                            "The build service's answer couldn't be read.".to_string(),
                        ));
                        return None;
                    };
                    return Some((taken.job_id, upload));
                }
                Err(UploadError::Starting { retry_after }) => {
                    if js_sys::Date::now() - started > 10.0 * 60_000.0 {
                        draft.sending.set(Sending::Refused(
                            "The build service didn't start in ten minutes. Try again later."
                                .to_string(),
                        ));
                        return None;
                    }
                    draft.sending.set(Sending::Uploading {
                        note: format!(
                            "The build service is starting. Trying again in {retry_after} seconds…"
                        ),
                    });
                    gloo_timers::future::TimeoutFuture::new(retry_after * 1_000).await;
                }
                Err(UploadError::TooLarge) => {
                    draft
                        .sending
                        .set(Sending::Refused(shared::archive::upload_too_large()));
                    return None;
                }
                Err(UploadError::Refused(why)) => {
                    draft.sending.set(Sending::Refused(format!(
                        "The build service refused the project: {why}"
                    )));
                    return None;
                }
                Err(UploadError::Failed(why)) => {
                    draft
                        .sending
                        .set(Sending::Refused(format!("{why}. Try again shortly.")));
                    return None;
                }
            }
        }
    }
}
