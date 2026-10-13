//! `/publish` (Publish-Desktop, Publish-Mobile): package a project, upload
//! it, choose public or sealed source, and pay the fee (1 USDC) from the
//! wallet that uploaded it, and only from that wallet. The `shonin.toml`
//! summary is read from the archive itself, read-only, before anything is
//! uploaded; a project the build service would refuse for its size is
//! refused here, in an alert, with nothing uploaded or charged.

pub mod activate;
pub mod job;
pub mod parts;

use std::rc::Rc;

use alloy_primitives::{Address, B256};
use futures_signals::signal::{Mutable, Signal, SignalExt};
use shared::Chain;
use ui::icons::Icon;

use super::Page;
use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::check::{checkbox, radio};
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::route::DOCS;
use crate::tx::author::AuthorError;
use crate::tx::author::archive::{self, Refusal, Summary};
use crate::tx::author::run::Running;
use crate::tx::author::{Prompt, Step};
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

/// The publishing fee as the page writes it before the chain's own figure
/// is read: in the token, never in dollars.
pub const FEE: &str = "1 USDC";

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
    /// Offering the project to the build service; while it starts, the
    /// time of the next try (milliseconds since the epoch).
    Uploading {
        next_try: Option<u64>,
    },
    /// The author stopped the upload before anything was paid.
    Stopped,
    /// Taken: pay against it from the account the upload named, and no
    /// other.
    Uploaded {
        job: String,
        upload: B256,
        account: Address,
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
    /// Which of the project's agents the author chose, when it holds
    /// several. None until they choose: nothing is chosen for them, so no
    /// fee is offered on a chain they didn't pick.
    agent: Mutable<Option<usize>>,
    source: Mutable<Source>,
    checks: Vec<Mutable<bool>>,
    checks_open: Mutable<bool>,
    sending: Mutable<Sending>,
    running: Mutable<Option<Running>>,
    /// Uploading or starting the payment: one at a time, however often the
    /// payment section is redrawn.
    busy: Mutable<bool>,
    /// What looking on the chain for a payment that may have landed found.
    checked: Mutable<Option<String>>,
    /// A look on the chain found no payment: looking again stays offered
    /// beside the payment, since one the wallet still holds may land.
    look_again: Mutable<bool>,
    dragging: Mutable<bool>,
    /// Which submission is current: "Cancel" moves it on, and an upload
    /// that answers after that is let go.
    attempt: Mutable<u32>,
    /// "Try now", pressed while the build service starts.
    try_now: Mutable<bool>,
}

/// Where a submission is, as the page shows it in place of Submit: the
/// steps the upload and the payment flow already report, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing under way: Submit is offered.
    Idle,
    /// Offering the project to the build service.
    Uploading,
    /// The build service is starting; the next try at this time (ms).
    Waking { next_try: u64 },
    /// Reading the chain before the wallet is asked anything.
    Checking,
    /// The wallet is asked to approve the fee.
    Approving(Prompt),
    /// The approval is on its way.
    ApprovalSent(B256),
    /// The wallet is asked to pay the fee.
    Paying(Prompt),
    /// The payment is on its way.
    PaymentSent(B256),
    /// Paid: the job is queued and its page opens.
    Queued,
}

impl Phase {
    /// Which of upload, approve, pay and queued it is at (4 once queued).
    fn at(&self) -> usize {
        match self {
            Phase::Idle | Phase::Uploading | Phase::Waking { .. } => 0,
            Phase::Checking | Phase::Approving(_) | Phase::ApprovalSent(_) => 1,
            Phase::Paying(_) | Phase::PaymentSent(_) => 2,
            Phase::Queued => 4,
        }
    }

    /// A submission is under way: Submit isn't offered.
    fn is_live(&self) -> bool {
        *self != Phase::Idle
    }
}

/// The phase a submission is in, from where the upload is and the payment
/// flow's latest step (`running`: the flow, if one was started, and its
/// step).
fn phase_of(sending: &Sending, running: Option<Option<&Step>>) -> Phase {
    match sending {
        Sending::Uploading { next_try: None } => Phase::Uploading,
        Sending::Uploading {
            next_try: Some(next_try),
        } => Phase::Waking {
            next_try: *next_try,
        },
        Sending::Uploaded { .. } => match running {
            None => Phase::Idle,
            Some(None | Some(Step::Checking)) => Phase::Checking,
            Some(Some(Step::Signing(prompt @ Prompt::Approve { .. }))) => {
                Phase::Approving(prompt.clone())
            }
            Some(Some(Step::Sent {
                prompt: Prompt::Approve { .. },
                hash,
            })) => Phase::ApprovalSent(*hash),
            Some(Some(Step::Signing(prompt))) => Phase::Paying(prompt.clone()),
            Some(Some(Step::Sent { hash, .. })) => Phase::PaymentSent(*hash),
            Some(Some(Step::Done(_))) => Phase::Queued,
            Some(Some(Step::Failed(_))) => Phase::Idle,
        },
        Sending::Idle | Sending::Stopped | Sending::Refused(_) => Phase::Idle,
    }
}

/// The phase, as it changes.
fn phase_signal(draft: &Draft) -> impl Signal<Item = Phase> + 'static {
    let running = draft
        .running
        .signal_cloned()
        .map(|running| match running {
            Some(running) => running.step.signal_cloned().map(Some).boxed_local(),
            None => futures_signals::signal::always(None).boxed_local(),
        })
        .flatten();
    futures_signals::map_ref! {
        let sending = draft.sending.signal_cloned(),
        let running = running => phase_of(sending, running.as_ref().map(Option::as_ref))
    }
}

pub fn page() -> Page {
    let draft = Draft::new();
    page_of(&draft)
}

impl Draft {
    fn new() -> Rc<Self> {
        Rc::new(Draft {
            picked: Mutable::new(None),
            agent: Mutable::new(None),
            source: Mutable::new(Source::Public),
            checks: CHECKS.iter().map(|_| Mutable::new(false)).collect(),
            checks_open: Mutable::new(false),
            sending: Mutable::new(Sending::Idle),
            running: Mutable::new(None),
            busy: Mutable::new(false),
            checked: Mutable::new(None),
            look_again: Mutable::new(false),
            dragging: Mutable::new(false),
            attempt: Mutable::new(0),
            try_now: Mutable::new(false),
        })
    }
}

fn page_of(draft: &Rc<Draft>) -> Page {
    let draft = draft.clone();
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
                            format!("Submit your code for {FEE}. We review it, build it once and seal it. You activate it when you’re happy."),
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
        ("Pay 1 USDC", "One transaction, or two".to_string()),
    ];
    // The phone's one line (Publish-Mobile): where the author is, what
    // comes next, and four segments filled up to here.
    let compact = el("div")
        .class("sh-publish__compact")
        .child(
            el("p")
                .class("sh-publish__compactline")
                .child(el("span").text(compact_words(current, steps[current].0)))
                .child(match steps.get(current + 1) {
                    Some((next, _)) => el("span")
                        .class("sh-publish__compactnext")
                        .text(format!("Next: {next}"))
                        .into(),
                    None => View::Empty,
                }),
        )
        .child(
            el("div")
                .class("sh-publish__bars")
                .attr("role", "progressbar")
                .attr("aria-label", "Publishing progress")
                .attr("aria-valuemin", "1")
                .attr("aria-valuemax", "4")
                .attr("aria-valuenow", (current + 1).to_string())
                .attr("aria-valuetext", compact_words(current, steps[current].0))
                .children((0..steps.len()).map(|index| {
                    el("span")
                        .class("sh-publish__barseg")
                        .class_if(index <= current, "sh-publish__barseg--on")
                })),
        );
    let list = el("ol")
        .class("sh-publish__steplist")
        .attr("aria-label", "Steps")
        .attr("tabindex", "0")
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
        );
    View::Many(vec![compact.into(), list.into()])
}

/// "Step 3 of 4 · Seal": the phone's line for the page's step `current`
/// (counted from 0), titled `title`.
fn compact_words(current: usize, title: &str) -> String {
    format!("Step {} of 4 · {title}", current + 1)
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
                .child(el("code")
                    .class("sh-publish__code")
                    .attr("tabindex", "0")
                    .text(PACKAGE)),
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
/// one they chose. A project holding several has none until they choose.
fn chosen(picked: &Picked, index: Option<usize>) -> Option<&archive::Agent> {
    let summary = picked.summary.as_ref().ok()?;
    match summary.agents.as_slice() {
        [only] => Some(only),
        agents => agents.get(index?),
    }
}

/// What stops this project being published as it is, in words.
fn problem(picked: &Picked, index: Option<usize>) -> Option<String> {
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
        .child(el("code")
                    .class("sh-publish__code")
                    .attr("tabindex", "0")
                    .text(PACKAGE))
        .into()
}

/// The chosen file and what its `shonin.toml` says, read-only.
fn read_back(draft: &Rc<Draft>, picked: &Picked, summary: &Summary, index: Option<usize>) -> View {
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
                .build()
                // Not while a submission is under way: its file is the one
                // being uploaded or paid for.
                .attr_signal(
                    "disabled",
                    None,
                    phase_signal(draft).map(|phase| phase.is_live().then(String::new)),
                ),
            move || {
                if replace.busy.get()
                    || replace
                        .running
                        .lock_ref()
                        .as_ref()
                        .is_some_and(Running::is_live)
                {
                    return;
                }
                replace.attempt.replace_with(|n| n.wrapping_add(1));
                replace.picked.set(None);
                replace.sending.set(Sending::Idle);
                replace.running.set(None);
                replace.checked.set(None);
                replace.look_again.set(false);
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
                radio(
                    "agent",
                    format!("agent-{at}"),
                    label,
                    Some(at),
                    &draft.agent,
                )
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
    // An option card: the radio, its icon and name, then what it means.
    let choice = |value: Source,
                  id: &str,
                  glyph: Icon,
                  label: &str,
                  chip: Option<&str>,
                  lead: &str,
                  detail: &str| {
        el("label")
            .class("sh-publish__option")
            .attr("for", id.to_string())
            .child(
                el("span")
                    .class("sh-check sh-check--radio sh-publish__optionhead")
                    .child(crate::atoms::check::radio_input(
                        "source",
                        id.to_string(),
                        value,
                        &draft.source,
                    ))
                    .child(el("span").class("sh-check__box"))
                    .child(icon(glyph, 20))
                    .child(
                        el("span")
                            .class("sh-publish__optionname")
                            .text(label.to_string()),
                    ),
            )
            .child(match chip {
                Some(chip) => el("span")
                    .class("sh-chip sh-chip-neutral sh-publish__optionchip")
                    .text(chip.to_string())
                    .into(),
                None => View::Empty,
            })
            .child(
                el("span")
                    .class("sh-publish__optionlead")
                    .text(lead.to_string()),
            )
            .child(
                el("span")
                    .class("sh-muted sh-publish__optiondetail")
                    .text(detail.to_string()),
            )
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
                .child(
                    el("div")
                        .class("sh-publish__options")
                        .child(choice(
                            Source::Public,
                            "source-public",
                            Icon::Code,
                            "Public source",
                            Some("Recommended for trust"),
                            "Anyone can read it and rebuild it to check.",
                            "Depositors can compare your code with what the Reviewer said about it.",
                        ))
                        .child(choice(
                            Source::Sealed,
                            "source-sealed",
                            Icon::Lock,
                            "Sealed",
                            None,
                            "Only our attested machines and the Reviewer's model can read it. Depositors rely on the review instead, and your page says so.",
                            "It's still reviewed and built the same way; it's just never published.",
                        )),
                ),
        )
}

// 4 · Pay the fee ---------------------------------------------------------------------------

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
                            .text(format!("Pay {FEE}")),
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
    // Once uploaded, the payment comes from the account the upload named;
    // before that, from the one connected.
    let payer = futures_signals::map_ref! {
        let connection = session.connection.signal_cloned(),
        let sending = draft.sending.signal_cloned() => {
            let account = match sending {
                Sending::Uploaded { account, .. } => Some(*account),
                _ => connection.account(),
            };
            account.map(|account| crate::format::short_hash(&format!("{account:?}")))
        }
    }
    .broadcast();
    // An address is mono; the words "your wallet" are not.
    let from = el("span")
        .class_signal("sh-mono", false, payer.signal_ref(Option::is_some))
        .text_signal(
            "your wallet",
            payer
                .signal_cloned()
                .map(|payer| payer.unwrap_or_else(|| "your wallet".to_string())),
        );
    // The fee as the publishing contract on this chain has it, once read.
    let fee: Mutable<Option<String>> = Mutable::new(None);
    let fee_text = fee
        .signal_cloned()
        .map(|fee| fee.unwrap_or_else(|| FEE.to_string()));
    let list = el("dl")
        .class("sh-publish__dl sh-publish__paylist")
        .child(parts::row_signal(
            "Publishing fee",
            FEE.to_string(),
            fee_text,
        ))
        .child(parts::row(
            "Covers",
            parts::words("The review and the build"),
        ))
        .child(parts::row("Paid on", parts::words(chain_label)))
        .child(parts::row("From", from));
    let action: View = match target {
        None if unchosen(draft) => View::Many(vec![
            submit_button(FEE).disabled(true).build().into(),
            el("p")
                .class("sh-publish__note")
                .text("Choose which agent you're publishing first: the fee is paid on its vault's chain.")
                .into(),
        ]),
        None => submit_button(FEE).disabled(true).build().into(),
        Some((_, None)) => notice(
            NoticeTone::Caution,
            text("This chain isn't reachable from the app right now. Try again shortly."),
        )
        .into(),
        Some((chain, Some(network))) => {
            let names = Rc::new(Names::of(&network));
            let read_fee = fee.clone();
            let fee_names = names.clone();
            let gate_draft = draft.clone();
            let gate_names = names.clone();
            let live_names = names.clone();
            let live_fee = fee.clone();
            let gate_fee = fee.clone();
            let chain_id = network.chain_id;
            View::Many(vec![
                // One polite region says each step once; the countdown
                // itself is never read out.
                el("p")
                    .class("sh-vh")
                    .attr("aria-live", "polite")
                    .text_signal(
                        "",
                        futures_signals::map_ref! {
                            let phase = phase_signal(draft),
                            let fee = live_fee.signal_cloned() =>
                            announce(phase, &live_names, fee.as_deref().unwrap_or(FEE))
                        },
                    )
                    .into(),
                wallet_gate(chain_id, move || {
                    let draft = gate_draft.clone();
                    let names = gate_names.clone();
                    let state = futures_signals::map_ref! {
                        let fee = gate_fee.signal_cloned(),
                        let unsure = may_have_paid(&draft),
                        let again = draft.look_again.signal(),
                        let phase = phase_signal(&draft) =>
                        (fee.clone(), unsure.is_some(), *again, phase.clone())
                    };
                    el("div")
                        .class("sh-publish__actionslot")
                        .child_signal(
                            None,
                            state.map(move |(fee, unsure, again, phase)| {
                                // While a step is under way its state
                                // stands where Submit was; Submit only
                                // comes back once nothing is.
                                Some(if phase.is_live() {
                                    progress(&draft, &names, fee.as_deref().unwrap_or(FEE), &phase)
                                } else {
                                    pay_action(&draft, chain, fee, unsure, again)
                                })
                            }),
                        )
                        .into()
                }),
                el("div")
                    .child_from(&draft.checked, |checked| {
                        checked.as_ref().map(|note| {
                            el("p")
                                .class("sh-publish__note")
                                .attr("role", "status")
                                .text(note.clone())
                                .into()
                        })
                    })
                    .child_signal(
                        None,
                        draft.sending.signal_cloned().map(|sending| match sending {
                            Sending::Stopped => Some(
                                el("p")
                                    .class("sh-publish__note")
                                    .attr("role", "status")
                                    .text("Stopped before anything was paid. Nothing was charged.")
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
                        draft
                            .running
                            .signal_cloned()
                            .map(|running| match running {
                                Some(running) => running.step.signal_cloned().boxed_local(),
                                None => futures_signals::signal::always(None).boxed_local(),
                            })
                            .flatten()
                            .map(move |step| match step {
                                Some(Step::Failed(error)) => Some(parts::failed(&error, &names)),
                                _ => None,
                            }),
                    )
                    .hook(move |dom| {
                        dom.future(async move {
                            if let Some(amount) = crate::tx::author::run::fee(chain).await {
                                read_fee.set(Some(fee_names.tokens(amount)));
                            }
                        })
                    })
                    .into(),
            ])
        }
    };
    // On a phone the action is a bar held at the foot of the screen
    // (Publish-Mobile), with the choice and the fee in one line above it.
    let line = futures_signals::map_ref! {
        let source = draft.source.signal(),
        let fee = fee.signal_cloned() => (
            match source {
                Source::Public => "Public source · one or two transactions",
                Source::Sealed => "Sealed source · one or two transactions",
            },
            fee.clone().unwrap_or_else(|| FEE.to_string()),
        )
    }
    .broadcast();
    el("div")
        .class("sh-publish__pay")
        .child(list)
        .child(
            el("div")
                .class("sh-publish__bar")
                .child(
                    el("p")
                        .class("sh-publish__barline")
                        .attr("aria-hidden", "true")
                        .child(el("span").text_signal(
                            "Public source · one or two transactions",
                            line.signal_ref(|(words, _)| words.to_string()),
                        ))
                        .child(
                            el("span")
                                .class("sh-publish__barfee")
                                .text_signal(FEE, line.signal_ref(|(_, fee)| fee.clone())),
                        ),
                )
                .child(el("div").class("sh-publish__payaction").child(action)),
        )
        .child(el("p").class("sh-muted sh-publish__small").text(
            "One transaction on the chain your vault will live on, or two if your wallet hasn't allowed the fee yet. Only a payment from the wallet that uploaded the project starts the job, so the payment is only ever sent from that wallet. You never compute a hash.",
        ))
        .into()
}

/// The words the live region says for a phase, once each.
fn announce(phase: &Phase, names: &Names, fee: &str) -> String {
    let wallet = &names.wallet;
    match phase {
        Phase::Idle => String::new(),
        Phase::Uploading => "Uploading the project".to_string(),
        Phase::Waking { .. } => {
            "The build service is starting. Starting it takes a few minutes; the page keeps trying."
                .to_string()
        }
        Phase::Checking => "Checking the chain".to_string(),
        Phase::Approving(_) => format!("Step 2 of 4: approve {fee}. Confirm in {wallet}."),
        Phase::ApprovalSent(_) => {
            "Approval sent. Waiting for the network to confirm it.".to_string()
        }
        Phase::Paying(_) => format!("Step 3 of 4: pay {fee}. Confirm in {wallet}."),
        Phase::PaymentSent(_) => "Payment sent. Waiting for the network to confirm it.".to_string(),
        Phase::Queued => "Paid. Your job is queued; its page opens now.".to_string(),
    }
}

/// Whole seconds until `next_try`, at `now` (both in milliseconds).
fn seconds_until(next_try: u64, now: f64) -> u64 {
    let left = next_try as f64 - now;
    if left <= 0.0 {
        0
    } else {
        (left / 1_000.0).ceil() as u64
    }
}

/// "Next try in 27 seconds", counting down to `next_try`.
fn countdown_words(next_try: u64, now: f64) -> String {
    match seconds_until(next_try, now) {
        0 => "Trying again now…".to_string(),
        1 => "Next try in 1 second.".to_string(),
        n => format!("Next try in {n} seconds."),
    }
}

/// A submission under way, where Submit was: the four steps (upload,
/// approve, pay, queued) with where it is, then this step's own state.
/// "Cancel" only where nothing can have been paid by stopping: before the
/// wallet is asked, or while it is asked and nothing is sent yet.
fn progress(draft: &Rc<Draft>, names: &Rc<Names>, fee: &str, phase: &Phase) -> View {
    let wallet = names.wallet.clone();
    let titles = [
        "Upload".to_string(),
        format!("Approve {fee}"),
        format!("Pay {fee}"),
        "Queued".to_string(),
    ];
    let at = phase.at();
    let steps = el("ol")
        .class("sh-publish__flow")
        .attr("aria-label", "Submitting")
        .children(titles.into_iter().enumerate().map(|(index, title)| {
            let done = index < at;
            let now = index == at;
            el("li")
                .class("sh-publish__flowstep")
                .class_if(now, "sh-publish__flowstep--now")
                .class_if(done, "sh-publish__flowstep--done")
                .attr_if(now, "aria-current", "step")
                .child(
                    el("span")
                        .class("sh-publish__flowmark")
                        .attr("aria-hidden", "true")
                        .child(if done {
                            icon(Icon::Check, 14).into()
                        } else {
                            text((index + 1).to_string())
                        }),
                )
                .child(el("span").class("sh-publish__flowtitle").text(title))
                .child(if done {
                    el("span").class("sh-vh").text(", done").into()
                } else {
                    View::Empty
                })
        }));
    let stop = {
        let draft = draft.clone();
        move || stop_upload(&draft)
    };
    let cancel_flow = {
        let draft = draft.clone();
        move || {
            if let Some(running) = draft.running.lock_ref().as_ref() {
                running.cancel.cancel();
            }
        }
    };
    let cancel = |action: Box<dyn Fn()>| -> View {
        on_click(
            Button::new(Kind::Secondary, "Cancel")
                .size(Size::Small)
                .build(),
            action,
        )
        .into()
    };
    let confirm = el("p")
        .class("sh-publish__confirm")
        .child(crate::components::wallet::hex_ring())
        .text(format!("Confirm in {wallet}…"));
    let card = |title: String, lines: Vec<View>| -> View {
        el("div")
            .class("sh-publish__now")
            .child(el("p").class("sh-author__steptitle").text(title))
            .children(lines)
            .into()
    };
    let waiting = |words: &str| -> View {
        el("p")
            .class("sh-publish__confirm")
            .child(crate::components::wallet::hex_ring())
            .text(words.to_string())
            .into()
    };
    let unseen = format!("Don’t see it? Open {wallet}. Nothing is sent until you approve.");
    let body = match phase {
        Phase::Idle => View::Empty,
        Phase::Uploading => card(
            "Step 1 of 4 · Uploading the project".to_string(),
            vec![
                waiting("Offering it to the build service…"),
                el("p")
                    .class("sh-muted sh-publish__small")
                    .text("Nothing is charged until you pay.")
                    .into(),
                cancel(Box::new(stop)),
            ],
        ),
        Phase::Waking { next_try } => {
            let next_try = *next_try;
            let try_now = draft.try_now.clone();
            card(
                "Step 1 of 4 · Starting the build service".to_string(),
                vec![
                    el("p").text("It was asleep. Starting it takes a few minutes; the page keeps trying and uploads your project as soon as it’s up. Nothing is charged until you pay.").into(),
                    el("p")
                        .class("sh-publish__confirm sh-num")
                        .child(crate::components::wallet::hex_ring())
                        .child(el("span").text_signal(
                            countdown_words(next_try, crate::clock::now().get()),
                            crate::clock::signal().map(move |now| countdown_words(next_try, now)),
                        ))
                        .into(),
                    el("div")
                        .class("sh-publish__nowactions")
                        .child(on_click(
                            Button::new(Kind::Primary, "Try now").size(Size::Small).build(),
                            move || try_now.set(true),
                        ))
                        .child(cancel(Box::new(stop)))
                        .into(),
                ],
            )
        }
        Phase::Checking => card(
            "Step 2 of 4 · Checking the chain".to_string(),
            vec![waiting(&format!(
                "Reading the fee and what your wallet has allowed on {}…",
                names.chain
            ))],
        ),
        Phase::Approving(prompt) | Phase::Paying(prompt) => {
            let (title, line) = parts::prompt_words(prompt, names);
            let n = if matches!(phase, Phase::Approving(_)) {
                2
            } else {
                3
            };
            card(
                format!("Step {n} of 4 · {title}"),
                vec![
                    el("p").text(line).into(),
                    confirm.into(),
                    el("p")
                        .class("sh-muted sh-publish__small")
                        .text(unseen)
                        .into(),
                    cancel(Box::new(cancel_flow)),
                ],
            )
        }
        Phase::ApprovalSent(hash) => card(
            "Step 2 of 4 · Approval sent".to_string(),
            vec![
                waiting(&format!(
                    "Waiting for the network to confirm it. Then {wallet} asks for the payment."
                )),
                parts::tx_link(names.chain_id, *hash, "Transaction"),
            ],
        ),
        Phase::PaymentSent(hash) => card(
            "Step 3 of 4 · Payment sent".to_string(),
            vec![
                waiting(
                    "Waiting for the network to confirm it. Your job page opens once it’s confirmed.",
                ),
                parts::tx_link(names.chain_id, *hash, "Transaction"),
            ],
        ),
        Phase::Queued => card(
            "Paid · queued".to_string(),
            vec![waiting("Opening your job page…")],
        ),
    };
    el("div")
        .class("sh-publish__progress")
        .child(steps)
        .child(body)
        .into()
}

/// Each state of a submission under way, drawn by the page's own renderer
/// from synthetic values, for the dev-only states page.
pub fn submitting_states() -> Vec<(String, View)> {
    let draft = Draft::new();
    let names = Rc::new(Names {
        wallet: "MetaMask".to_string(),
        chain: "Base".to_string(),
        chain_id: 84_532,
        symbol: "USDC".to_string(),
        decimals: 6,
        native: "ETH".to_string(),
    });
    let fee = alloy_primitives::U256::from(1_000_000u64);
    let now = crate::clock::now().get().max(0.0) as u64;
    let hash = B256::repeat_byte(0x3c);
    [
        ("Nothing under way: Submit", Phase::Idle),
        ("Uploading", Phase::Uploading),
        (
            "The build service is starting",
            Phase::Waking {
                next_try: now + 27_000,
            },
        ),
        ("Checking the chain", Phase::Checking),
        (
            "Approve 1 USDC in the wallet",
            Phase::Approving(Prompt::Approve {
                amount: fee,
                spender: crate::tx::author::Spender::Publishing,
            }),
        ),
        ("Approval sent", Phase::ApprovalSent(hash)),
        (
            "Pay 1 USDC in the wallet",
            Phase::Paying(Prompt::Pay { fee }),
        ),
        ("Payment sent", Phase::PaymentSent(hash)),
        ("Paid and queued", Phase::Queued),
    ]
    .into_iter()
    .map(|(label, phase)| {
        let view = if phase.is_live() {
            progress(&draft, &names, FEE, &phase)
        } else {
            pay_action(&draft, Chain::Base, Some(FEE.to_string()), false, false)
        };
        (label.to_string(), view)
    })
    .collect()
}

/// Stop the upload under way (or the wait for the build service), before
/// anything is paid: an answer that comes after is let go.
fn stop_upload(draft: &Draft) {
    if !matches!(draft.sending.get_cloned(), Sending::Uploading { .. }) {
        return;
    }
    draft.attempt.replace_with(|n| n.wrapping_add(1));
    draft.busy.set(false);
    draft.sending.set(Sending::Stopped);
}

/// A project holding several agents, none of them chosen yet.
fn unchosen(draft: &Draft) -> bool {
    draft.agent.get().is_none()
        && draft
            .picked
            .lock_ref()
            .as_ref()
            .and_then(|picked| picked.summary.as_ref().ok())
            .is_some_and(|summary| summary.agents.len() > 1)
}

fn submit_button(fee: &str) -> Button {
    Button::new(Kind::Primary, format!("Submit for {fee}"))
        .size(Size::Large)
        .block()
}

/// The error of a payment that may have been made, or may still land,
/// while it is the latest word: until the chain is checked, nothing offers
/// to pay again.
fn may_have_paid(draft: &Draft) -> impl Signal<Item = Option<AuthorError>> + 'static {
    draft
        .running
        .signal_cloned()
        .map(|running| match running {
            Some(running) => running
                .step
                .signal_cloned()
                .map(|step| match step {
                    Some(Step::Failed(error)) if error.may_have_paid() => Some(error),
                    _ => None,
                })
                .boxed_local(),
            None => futures_signals::signal::always(None).boxed_local(),
        })
        .flatten()
}

/// "Submit for 1 USDC", or, after a payment that may have gone through,
/// only a way to look for it on the chain; once a look found nothing, both.
fn pay_action(
    draft: &Rc<Draft>,
    chain: Chain,
    fee: Option<String>,
    unsure: bool,
    look_again: bool,
) -> View {
    let busy = draft.busy.clone();
    let draft = draft.clone();
    if unsure {
        return on_click(
            Button::new(Kind::Secondary, "Check the chain for the payment")
                .size(Size::Large)
                .block()
                .busy(&busy)
                .build(),
            move || check_payment(&draft, chain),
        )
        .into();
    }
    let submit_draft = draft.clone();
    let pay = on_click(
        submit_button(fee.as_deref().unwrap_or(FEE))
            .busy(&busy)
            .build(),
        move || submit(&submit_draft, chain),
    );
    if !look_again {
        return pay.into();
    }
    el("div")
        .class("sh-publish__payactions")
        .child(pay)
        .child(on_click(
            Button::new(Kind::Secondary, "Check the chain again")
                .size(Size::Large)
                .block()
                .busy(&busy)
                .build(),
            move || check_payment(&draft, chain),
        ))
        .into()
}

/// Upload (unless this project was already uploaded for this chain) and
/// pay, from the account the upload named.
fn submit(draft: &Rc<Draft>, chain: Chain) {
    if draft.busy.get()
        || draft
            .running
            .lock_ref()
            .as_ref()
            .is_some_and(Running::is_live)
    {
        return;
    }
    draft.checked.set(None);
    #[cfg(target_arch = "wasm32")]
    browser::submit(draft.clone(), chain);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = chain;
}

/// Look on the chain for a payment for this upload that may have landed:
/// found, the job page; not found, the payment is offered again.
fn check_payment(draft: &Rc<Draft>, chain: Chain) {
    if draft.busy.get() {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    browser::check_payment(draft.clone(), chain);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = chain;
}

/// What looking for a payment that may have landed says, when it didn't
/// find the uploader's.
pub fn check_words(found: &crate::tx::author::read::PaymentRecord, wallet: &str) -> String {
    use crate::tx::author::read::PaymentRecord;
    match found {
        PaymentRecord::ByAnother { payer, .. } => format!(
            "The chain shows a payment for this upload from {}, which didn't make the upload, so it doesn't start the job. Nothing from the wallet that made the upload yet.",
            crate::format::short_hash(&format!("{payer:?}"))
        ),
        _ => format!(
            "The chain shows no payment for this upload yet. If {wallet} still shows the payment waiting, wait for it or cancel it there before paying again."
        ),
    }
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
                        text(format!("The {FEE} is spent whichever way it ends. To try again, you publish a new agent.")),
                    )
                    .class("sh-notice--compact"),
                ),
        )
        .child(
            el("div")
                .class("sh-card sh-card--pad sh-publish__asidecard")
                .child(el("h2").class("sh-publish__h2").text("Once it's live"))
                .child(el("p").class("sh-muted").text_signal(
                    once_live(author_share()),
                    crate::store::get()
                        .loaded
                        .signal()
                        .map(|_| once_live(author_share())),
                ))
                .child(
                    el("a")
                        .class("sh-link")
                        .attr("href", format!("{DOCS}authors/"))
                        .attr("target", "_blank")
                        .attr("rel", "noopener")
                        .text("How Authors are paid")
                        .child(icon(Icon::ArrowRight, 16)),
                ),
        )
}

/// The Author's share of new profit, as every vault the snapshot holds was
/// created with it; none when the vaults disagree or there are none yet.
fn author_share() -> Option<u32> {
    let mut shares = crate::store::get()
        .all()
        .into_iter()
        .map(|agent| agent.strategy.lock_ref().vault.fees.author_bps);
    let first = shares.next()?;
    shares.all(|bps| bps == first).then_some(first)
}

fn once_live(author_bps: Option<u32>) -> String {
    let share = match author_bps {
        Some(bps) => format!("{} of", crate::format::bps(bps)),
        None => "a share of".to_string(),
    };
    format!(
        "You earn {share} the new profit it makes, paid every Monday as shares. A tank you fill pays for its computing and gas."
    )
}

// The browser: reading the file, uploading, paying ----------------------------------------

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::rc::Rc;

    use js_sys::{Function, Promise, Reflect, Uint8Array};
    use shared::{Chain, Upload};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    use super::{Draft, Picked, Sending, Source};
    use crate::tx::author::archive;
    use crate::tx::author::read::PaymentRecord;
    use crate::tx::author::run::{self, Action, Lookup, Running, UploadError};
    use crate::tx::author::{AuthorError, Outcome, Step};

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
            // Not while a submission is under way: its file is the one
            // being uploaded or paid for.
            if draft.busy.get()
                || draft
                    .running
                    .lock_ref()
                    .as_ref()
                    .is_some_and(Running::is_live)
            {
                return;
            }
            draft.agent.set(None);
            draft.sending.set(Sending::Idle);
            draft.running.set(None);
            draft.checked.set(None);
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

    pub fn submit(draft: Rc<Draft>, chain: Chain) {
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
        let sealed = draft.source.get() == Source::Sealed;
        let package = if several { agent.package.clone() } else { None };
        let offered = format!(
            "{}|{sealed}|{package:?}",
            alloy_primitives::keccak256(picked.bytes.as_slice())
        );
        let busy = draft.busy.clone();
        busy.set(true);
        // This submission; "Cancel" moves on to the next.
        let attempt = draft.attempt.get().wrapping_add(1);
        draft.attempt.set(attempt);
        wasm_bindgen_futures::spawn_local(async move {
            // This project, already uploaded for this chain, is paid for as
            // it is, from the account its upload named: the flow refuses
            // any other and says which to switch back to. Anything else is
            // uploaded first, in the connected account's name, and bound to
            // that account from then on.
            let ready = match draft.sending.get_cloned() {
                Sending::Uploaded {
                    job,
                    upload,
                    account: by,
                    chain: on,
                    offered: what,
                } if on == chain && what == offered => Some((job, upload, by)),
                _ => None,
            };
            let (job, upload, uploader) = match ready {
                Some(ready) => ready,
                None => {
                    let offer = Upload {
                        author: format!("{account:#x}"),
                        archive_base64: shared::base64::encode(&picked.bytes),
                        sealed,
                        package,
                    };
                    match offer_until_taken(&draft, chain, &offer, attempt).await {
                        Some((job, upload)) => (job, upload, account),
                        None => {
                            // A stopped submission's page has moved on.
                            if draft.attempt.get() == attempt {
                                busy.set(false);
                            }
                            return;
                        }
                    }
                }
            };
            draft.sending.set(Sending::Uploaded {
                job: job.clone(),
                upload,
                account: uploader,
                chain,
                offered,
            });
            let running = Running::new();
            draft.running.set(Some(running.clone()));
            busy.set(false);
            run::start(
                chain,
                Action::Pay { upload, uploader },
                &running,
                move |outcome| {
                    if let Outcome::Paid { strategy_id, .. }
                    | Outcome::AlreadyPaid { strategy_id } = outcome
                    {
                        run::remember_payment(
                            &job,
                            run::Paid {
                                chain,
                                upload,
                                uploader,
                                strategy_id: *strategy_id,
                            },
                        );
                        go_to_job(&job);
                    }
                },
            );
        });
    }

    fn go_to_job(job: &str) {
        dominator::routing::go_to_url(&format!("/publish/jobs/{}", crate::route::encode(job)));
    }

    /// Look for a payment for the upload that may have landed, after the
    /// payments the contract had recorded before it was sent.
    pub fn check_payment(draft: Rc<Draft>, chain: Chain) {
        let Sending::Uploaded {
            job,
            upload,
            account: uploader,
            chain: on,
            ..
        } = draft.sending.get_cloned()
        else {
            return;
        };
        if on != chain {
            return;
        }
        let since = match draft
            .running
            .lock_ref()
            .as_ref()
            .and_then(|running| running.step.get_cloned())
        {
            Some(Step::Failed(AuthorError::PaymentUnsure { since, .. })) => Some(since),
            _ => None,
        };
        let wallet = crate::wallet::get().wallet_name.get_cloned();
        let wallet = if wallet.is_empty() {
            "your wallet".to_string()
        } else {
            wallet
        };
        let busy = draft.busy.clone();
        busy.set(true);
        draft.checked.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            let found = run::payment(chain, upload, uploader, Lookup::After(since)).await;
            busy.set(false);
            match found {
                Ok(PaymentRecord::ByUploader { strategy_id }) => {
                    run::remember_payment(
                        &job,
                        run::Paid {
                            chain,
                            upload,
                            uploader,
                            strategy_id,
                        },
                    );
                    go_to_job(&job);
                }
                Ok(found) => {
                    // Nothing from the uploader on the chain: the payment is
                    // offered again, with what was found said.
                    draft.running.set(None);
                    draft.look_again.set(true);
                    draft.checked.set(Some(super::check_words(&found, &wallet)));
                }
                Err(why) => {
                    draft
                        .checked
                        .set(Some(format!("Couldn't look: {why}. Try again shortly.")));
                }
            }
        });
    }

    /// Offer the project until the build service takes it, waiting while it
    /// starts (as long as it asks, or until "Try now", for up to ten
    /// minutes). The job id and the reference to pay against, or `None`
    /// with the reason set, or once the author stopped it (`attempt` is no
    /// longer the page's), leaving the page as it is.
    async fn offer_until_taken(
        draft: &Rc<Draft>,
        chain: Chain,
        offer: &Upload,
        attempt: u32,
    ) -> Option<(String, alloy_primitives::B256)> {
        let current = || draft.attempt.get() == attempt;
        let started = js_sys::Date::now();
        draft.sending.set(Sending::Uploading { next_try: None });
        loop {
            let answer = run::upload(chain, offer).await;
            if !current() {
                return None;
            }
            match answer {
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
                    let next_try = js_sys::Date::now() + f64::from(retry_after) * 1_000.0;
                    draft.try_now.set(false);
                    draft.sending.set(Sending::Uploading {
                        next_try: Some(next_try as u64),
                    });
                    // Counted down on the page; "Try now" ends the wait.
                    while js_sys::Date::now() < next_try && !draft.try_now.get() {
                        gloo_timers::future::TimeoutFuture::new(250).await;
                        if !current() {
                            return None;
                        }
                    }
                    draft.try_now.set(false);
                    draft.sending.set(Sending::Uploading { next_try: None });
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
                        .set(Sending::Refused(crate::format::sentence(&format!(
                            "{why}. Try again shortly."
                        ))));
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::TxError;
    use crate::tx::author::read::PaymentRecord;

    /// A dollar sign and a one, written as the fee would be.
    const DOLLAR_ONE: &str = concat!("$", "1");

    /// Where the fee is written in dollars: a dollar sign and a one not
    /// followed by a digit or a thousands comma (ten dollars and a thousand
    /// dollars are other amounts).
    fn dollar_fee(text: &str) -> Option<String> {
        text.match_indices(DOLLAR_ONE).find_map(|(at, _)| {
            let next = text[at + 2..].chars().next();
            (!matches!(next, Some(c) if c.is_ascii_digit() || c == ','))
                .then(|| text[at.saturating_sub(40)..(at + 12).min(text.len())].to_string())
        })
    }

    #[test]
    fn no_page_under_publish_writes_the_fee_in_dollars() {
        let html = page().body.html();
        assert!(html.contains("Submit for 1 USDC"), "{html}");
        assert_eq!(dollar_fee(&html), None);
        for (name, source) in [
            ("mod.rs", include_str!("mod.rs")),
            ("job.rs", include_str!("job.rs")),
            ("activate.rs", include_str!("activate.rs")),
            ("parts.rs", include_str!("parts.rs")),
        ] {
            // What the page says, its tests aside.
            let source = source.split("#[cfg(test)]").next().unwrap_or_default();
            assert_eq!(dollar_fee(source), None, "{name}");
        }
    }

    /// A project as the page reads it, holding one agent per vault chain
    /// named.
    fn project(chains: &[&str]) -> Picked {
        let agents = chains
            .iter()
            .enumerate()
            .map(|(at, chain)| archive::Agent {
                path: format!("agents/{at}/shonin.toml"),
                package: Some(format!("agent-{at}")),
                manifest: shared::Manifest::from_toml(&format!(
                    "[vault]\nchain = \"{chain}\"\n[description]\ntitle = \"Agent {at}\"\ntext = \"t\"\n[drivers.local]\n"
                )),
            })
            .collect();
        Picked {
            name: "agent.tar".to_string(),
            bytes: Rc::new(Vec::new()),
            summary: Ok(Summary {
                bytes: 0,
                files: 0,
                unpacked: 0,
                agents,
            }),
        }
    }

    #[test]
    fn a_project_holding_several_agents_offers_no_payment_until_the_author_chooses_one() {
        let draft = Draft::new();
        draft.picked.set(Some(project(&["robinhood", "base"])));
        assert_eq!(draft.agent.get(), None);
        assert_eq!(target(&draft), None);
        let html = pay_body(&draft, None).html();
        assert!(html.contains("disabled"), "{html}");
        assert!(html.contains("Choose which agent"), "{html}");
        assert!(!html.contains("Robinhood"), "{html}");
        let html = read_back(
            &draft,
            &draft.picked.get_cloned().unwrap(),
            draft.picked.get_cloned().unwrap().summary.as_ref().unwrap(),
            None,
        )
        .html();
        assert!(!html.contains("checked"), "{html}");
        // The author's own choice is what's paid for, on its chain.
        draft.agent.set(Some(1));
        assert_eq!(target(&draft), Some(Chain::Base));
        assert!(!unchosen(&draft));
    }

    #[test]
    fn a_project_holding_one_agent_needs_no_choice() {
        let draft = Draft::new();
        draft.picked.set(Some(project(&["base"])));
        assert_eq!(target(&draft), Some(Chain::Base));
        assert!(!unchosen(&draft));
    }

    #[test]
    fn after_a_look_on_the_chain_found_no_payment_looking_again_stays_offered_beside_paying() {
        let draft = Draft::new();
        let html = pay_action(&draft, Chain::Base, Some("1 USDC".to_string()), false, true).html();
        assert!(html.contains("Submit for 1 USDC"), "{html}");
        assert!(html.contains("Check the chain again"), "{html}");
    }

    #[test]
    fn after_a_payment_that_may_have_landed_the_page_offers_only_to_look_for_it() {
        let draft = Draft::new();
        let html = pay_action(
            &draft,
            Chain::Base,
            Some("1 USDC".to_string()),
            false,
            false,
        )
        .html();
        assert!(html.contains("Submit for 1 USDC"), "{html}");
        assert!(!html.contains("Check the chain"), "{html}");
        let html = pay_action(&draft, Chain::Base, Some("1 USDC".to_string()), true, false).html();
        assert!(!html.contains("Submit"), "{html}");
        assert!(html.contains("Check the chain for the payment"), "{html}");
        // Which errors hold the payment back until the chain is checked.
        let unsure = AuthorError::PaymentUnsure {
            upload: B256::ZERO,
            since: alloy_primitives::U256::ZERO,
            then: Box::new(TxError::Dropped { hash: B256::ZERO }),
        };
        assert!(unsure.may_have_paid());
        assert!(AuthorError::PaymentMismatch.may_have_paid());
        assert!(AuthorError::Tx(TxError::Stuck { hash: B256::ZERO }).may_have_paid());
        assert!(AuthorError::Tx(TxError::Replaced { hash: B256::ZERO }).may_have_paid());
        assert!(!AuthorError::Tx(TxError::Rejected).may_have_paid());
        assert!(
            !AuthorError::WrongPayer {
                uploader: Address::ZERO,
                approved: None,
            }
            .may_have_paid()
        );
        assert!(
            !AuthorError::Tx(TxError::Reverted {
                hash: B256::ZERO,
                reason: None
            })
            .may_have_paid()
        );
    }

    fn metamask() -> Rc<Names> {
        Rc::new(Names {
            wallet: "MetaMask".to_string(),
            chain: "Base".to_string(),
            chain_id: 84_532,
            symbol: "USDC".to_string(),
            decimals: 6,
            native: "ETH".to_string(),
        })
    }

    fn uploaded() -> Sending {
        Sending::Uploaded {
            job: "8803".to_string(),
            upload: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            chain: Chain::Base,
            offered: String::new(),
        }
    }

    fn approve() -> Prompt {
        Prompt::Approve {
            amount: alloy_primitives::U256::from(1_000_000u64),
            spender: crate::tx::author::Spender::Publishing,
        }
    }

    #[test]
    fn the_phase_follows_the_upload_and_the_steps_the_payment_flow_reports() {
        let pay = Prompt::Pay {
            fee: alloy_primitives::U256::from(1_000_000u64),
        };
        let hash = B256::repeat_byte(3);
        assert_eq!(phase_of(&Sending::Idle, None), Phase::Idle);
        assert_eq!(phase_of(&Sending::Stopped, None), Phase::Idle);
        assert_eq!(
            phase_of(&Sending::Uploading { next_try: None }, None),
            Phase::Uploading
        );
        assert_eq!(
            phase_of(&Sending::Uploading { next_try: Some(9) }, None),
            Phase::Waking { next_try: 9 }
        );
        let after = |step: Option<Step>| phase_of(&uploaded(), Some(step.as_ref()));
        assert_eq!(after(None), Phase::Checking);
        assert_eq!(after(Some(Step::Checking)), Phase::Checking);
        assert_eq!(
            after(Some(Step::Signing(approve()))),
            Phase::Approving(approve())
        );
        assert_eq!(
            after(Some(Step::Sent {
                prompt: approve(),
                hash
            })),
            Phase::ApprovalSent(hash)
        );
        assert_eq!(
            after(Some(Step::Signing(pay.clone()))),
            Phase::Paying(pay.clone())
        );
        assert_eq!(
            after(Some(Step::Sent { prompt: pay, hash })),
            Phase::PaymentSent(hash)
        );
        assert_eq!(
            after(Some(Step::Done(crate::tx::author::Outcome::AlreadyPaid {
                strategy_id: alloy_primitives::U256::from(2u8)
            }))),
            Phase::Queued
        );
        // A failed flow offers Submit again (with its error said beside it),
        // and an upload with no flow started is nothing under way.
        assert_eq!(
            after(Some(Step::Failed(AuthorError::Tx(TxError::Rejected)))),
            Phase::Idle
        );
        assert_eq!(phase_of(&uploaded(), None), Phase::Idle);
    }

    #[test]
    fn while_a_submission_is_under_way_submit_is_never_shown_in_its_place() {
        let draft = Draft::new();
        let names = metamask();
        let hash = B256::repeat_byte(3);
        for phase in [
            Phase::Uploading,
            Phase::Waking { next_try: 30_000 },
            Phase::Checking,
            Phase::Approving(approve()),
            Phase::ApprovalSent(hash),
            Phase::Paying(Prompt::Pay {
                fee: alloy_primitives::U256::from(1_000_000u64),
            }),
            Phase::PaymentSent(hash),
            Phase::Queued,
        ] {
            assert!(phase.is_live(), "{phase:?}");
            let html = progress(&draft, &names, FEE, &phase).html();
            assert!(!html.contains("Submit for"), "{phase:?}: {html}");
            // The four steps are always drawn, with where it is.
            for step in ["Upload", "Approve 1 USDC", "Pay 1 USDC", "Queued"] {
                assert!(html.contains(step), "{phase:?}: {html}");
            }
        }
        assert!(!Phase::Idle.is_live());
    }

    #[test]
    fn each_wallet_prompt_says_its_step_to_confirm_in_the_wallet_and_offers_cancel() {
        let draft = Draft::new();
        let names = metamask();
        let html = progress(&draft, &names, FEE, &Phase::Approving(approve())).html();
        assert!(
            html.contains("Step 2 of 4 · Approve 1 USDC in MetaMask"),
            "{html}"
        );
        assert!(html.contains("Confirm in MetaMask…"), "{html}");
        assert!(html.contains("sh-hexring"), "a spinner: {html}");
        assert!(html.contains("Cancel"), "{html}");
        assert!(html.contains(r#"aria-current="step""#), "{html}");
        let pay = Phase::Paying(Prompt::Pay {
            fee: alloy_primitives::U256::from(1_000_000u64),
        });
        let html = progress(&draft, &names, FEE, &pay).html();
        assert!(
            html.contains("Step 3 of 4 · Pay 1 USDC in MetaMask"),
            "{html}"
        );
        assert!(html.contains("Confirm in MetaMask…"), "{html}");
        assert!(html.contains("Cancel"), "{html}");
        // Once something is sent, or while the chain is read, stopping
        // isn't offered: it couldn't take back what was sent.
        for phase in [
            Phase::Checking,
            Phase::ApprovalSent(B256::repeat_byte(3)),
            Phase::PaymentSent(B256::repeat_byte(3)),
            Phase::Queued,
        ] {
            let html = progress(&draft, &names, FEE, &phase).html();
            assert!(!html.contains("Cancel"), "{phase:?}: {html}");
        }
    }

    #[test]
    fn a_starting_build_service_counts_down_says_it_takes_minutes_and_offers_try_now() {
        let draft = Draft::new();
        let html = progress(
            &draft,
            &metamask(),
            FEE,
            &Phase::Waking { next_try: 30_000 },
        )
        .html();
        assert!(html.contains("Starting it takes a few minutes"), "{html}");
        assert!(html.contains("Try now"), "{html}");
        assert!(html.contains("Cancel"), "{html}");
        assert_eq!(seconds_until(30_000, 3_500.0), 27);
        assert_eq!(seconds_until(30_000, 29_001.0), 1);
        assert_eq!(seconds_until(30_000, 31_000.0), 0);
        assert_eq!(countdown_words(30_000, 3_500.0), "Next try in 27 seconds.");
        assert_eq!(countdown_words(30_000, 29_500.0), "Next try in 1 second.");
        assert_eq!(countdown_words(30_000, 30_000.0), "Trying again now…");
    }

    #[test]
    fn cancelling_an_upload_stops_it_before_payment_and_lets_its_late_answer_go() {
        let draft = Draft::new();
        draft.busy.set(true);
        draft.attempt.set(4);
        draft.sending.set(Sending::Uploading { next_try: Some(1) });
        stop_upload(&draft);
        assert_eq!(draft.sending.get_cloned(), Sending::Stopped);
        assert!(!draft.busy.get());
        // The upload under way was attempt 4: its answer is no longer the
        // page's, so it sets nothing.
        assert_ne!(draft.attempt.get(), 4);
        assert_eq!(phase_of(&draft.sending.get_cloned(), None), Phase::Idle);
        // Nothing to stop once the upload was taken.
        draft.sending.set(uploaded());
        let attempt = draft.attempt.get();
        stop_upload(&draft);
        assert_eq!(draft.attempt.get(), attempt);
        assert_eq!(draft.sending.get_cloned(), uploaded());
    }

    #[test]
    fn on_a_phone_the_page_says_step_n_of_4_and_the_fee_rows_name_the_chain_and_the_payer_apart() {
        let draft = Draft::new();
        let html = View::from(stepper(&draft)).html();
        assert!(html.contains("Step 2 of 4 · Upload"), "{html}");
        assert!(html.contains("Next: Seal"), "{html}");
        assert!(html.contains(r#"role="progressbar""#), "{html}");
        draft.picked.set(Some(project(&["base"])));
        let html = pay_body(&draft, None).html();
        assert!(html.contains("<dt>Paid on</dt>"), "{html}");
        assert!(html.contains("<dt>From</dt>"), "{html}");
        assert!(!html.contains("live on from"), "one row each: {html}");
        // The action sits in the bar a phone holds at the foot of the screen.
        assert!(html.contains("sh-publish__bar"), "{html}");
        assert_eq!(compact_words(2, "Seal"), "Step 3 of 4 · Seal");
    }

    #[test]
    fn looking_for_a_payment_says_what_the_chain_shows() {
        let none = check_words(&PaymentRecord::None, "MetaMask");
        assert!(none.starts_with("The chain shows no payment for this upload yet."));
        assert!(none.contains("MetaMask"));
        let other = check_words(
            &PaymentRecord::ByAnother {
                strategy_id: alloy_primitives::U256::from(3u8),
                payer: Address::repeat_byte(0x97),
            },
            "MetaMask",
        );
        assert!(other.contains("doesn't start the job"), "{other}");
    }
}
