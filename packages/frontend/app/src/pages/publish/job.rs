//! `/publish/jobs/:id` (Job-Desktop, Handoff-States §4): where a publishing
//! job is, live, read from the public queue, so it works without a wallet
//! and can be bookmarked. The status is one polite live region whose words
//! change; the queue position is said as a place ("3rd in the queue"), never
//! as a time. When the job ends, its outcome card replaces the status. No
//! build log is shown, ever.

use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use shared::{Area, Failure, Job, JobState};
use ui::icons::Icon;

use super::FEE;
use super::parts::{self, ordinal};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, Size};
use crate::atoms::hash::hash;
use crate::atoms::icon;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::rating::rating;
use crate::pages::Page;
use crate::tx::author::read::PaymentRecord;
use crate::view::{El, View, el, text};
use crate::web::app_link;

/// What the page knows of the job.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone)]
enum Load {
    Loading,
    /// The queue has no job by this number (yet).
    Missing,
    /// The queue didn't answer; the last reading, if any, stays shown.
    Unanswered(Option<Rc<Job>>),
    Found(Rc<Job>),
}

pub fn page(id: &str) -> Page {
    let load = Mutable::new(Load::Loading);
    // What the publishing contract records of the payment this session
    // made for the job, while the queue still waits for it.
    let paid: Mutable<Option<PaymentRecord>> = Mutable::new(None);
    let id = id.to_string();
    let follow = load.clone();
    let follow_paid = paid.clone();
    let follow_id = id.clone();
    let seen = futures_signals::map_ref! {
        let load = load.signal_cloned(),
        let paid = paid.signal() => (load.clone(), *paid)
    }
    .broadcast();
    let body = el("div")
        .class("sh-author sh-job")
        .child(
            el("p")
                .class("sh-vh")
                .attr("aria-live", "polite")
                .text_signal(
                    "",
                    seen.signal_cloned()
                        .map(|(load, paid)| announce(&load, paid)),
                ),
        )
        .child_signal(
            Some(skeleton()),
            seen.signal_cloned().map(move |(load, paid)| {
                Some(match load {
                    Load::Loading => skeleton(),
                    Load::Missing => missing(&id),
                    Load::Unanswered(None) => unanswered(&id),
                    Load::Unanswered(Some(job)) | Load::Found(job) => job_view(&job, paid),
                })
            }),
        )
        .hook(move |dom| dom.future(watch(follow_id, follow, follow_paid)));
    Page {
        top: None,
        body: body.into(),
    }
}

/// Ask the queue every 5 s while the job runs, every 15 s once it has ended
/// (a ready job can still be activated).
async fn watch(id: String, load: Mutable<Load>, paid: Mutable<Option<PaymentRecord>>) {
    #[cfg(target_arch = "wasm32")]
    loop {
        let finished = match crate::tx::author::run::job(&id).await {
            Ok(Some(job)) => {
                let done = job.state.is_finished();
                paid.set_neq(match job.state {
                    JobState::AwaitingPayment => payment_on_chain(&id, &job).await,
                    _ => None,
                });
                // Redrawn only when the job changed.
                let same = match &*load.lock_ref() {
                    Load::Found(seen) => {
                        serde_json::to_string(&**seen).ok() == serde_json::to_string(&job).ok()
                    }
                    _ => false,
                };
                if !same {
                    load.set(Load::Found(Rc::new(job)));
                }
                done
            }
            Ok(None) => {
                load.set(Load::Missing);
                false
            }
            Err(_) => {
                let last = match load.get_cloned() {
                    Load::Found(job) | Load::Unanswered(Some(job)) => Some(job),
                    _ => None,
                };
                load.set(Load::Unanswered(last));
                false
            }
        };
        gloo_timers::future::TimeoutFuture::new(if finished { 15_000 } else { 5_000 }).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (id, load, paid);
}

/// The payment this session made for the job, as the publishing contract
/// records it under the id its receipt gave: tied to the job's upload and
/// to the wallet the job names, or nothing.
#[cfg(target_arch = "wasm32")]
async fn payment_on_chain(id: &str, job: &Job) -> Option<PaymentRecord> {
    use crate::tx::author::run::{self, Lookup};
    let made = run::payment_for_job(id)?;
    let named = job.author.parse::<alloy_primitives::Address>().ok()?;
    if made.chain != job.chain || made.uploader != named {
        return None;
    }
    run::payment(
        made.chain,
        made.upload,
        made.uploader,
        Lookup::At(made.strategy_id),
    )
    .await
    .ok()
}

/// The words the live region says for the job's state: one per change.
fn announce(load: &Load, paid: Option<PaymentRecord>) -> String {
    match load {
        Load::Found(job) | Load::Unanswered(Some(job)) => {
            let (headline, detail) = job_words(job, paid);
            format!("{headline}. {detail}")
        }
        _ => String::new(),
    }
}

/// The status words, with what the chain records of this session's payment
/// while the queue still waits for one.
pub fn job_words(job: &Job, paid: Option<PaymentRecord>) -> (String, String) {
    match (&job.state, paid) {
        (JobState::AwaitingPayment, Some(found)) => {
            paid_words(found).unwrap_or_else(|| status_words(job))
        }
        _ => status_words(job),
    }
}

/// What a payment the chain records for the job's upload means for it.
pub fn paid_words(found: PaymentRecord) -> Option<(String, String)> {
    match found {
        PaymentRecord::ByUploader { strategy_id } => Some((
            "Paid. Waiting for the payment's confirmations".to_string(),
            format!(
                "The chain records your payment for this upload from the wallet that made it, as agent {strategy_id}. The job starts once the build service has seen it confirmed; this page moves on by itself."
            ),
        )),
        PaymentRecord::ByAnother { payer, .. } => Some((
            "Paid from another wallet".to_string(),
            format!(
                "The chain records a payment for this upload from {}, which didn't make the upload, so it doesn't start the job. Only a payment from the wallet that uploaded it does.",
                crate::format::short_hash(&format!("{payer:?}"))
            ),
        )),
        PaymentRecord::None => None,
    }
}

fn skeleton() -> View {
    el("div")
        .class("sh-job__loading")
        .attr("aria-busy", "true")
        .child(crate::atoms::skeleton::skeleton_block("60%", 40))
        .child(crate::atoms::skeleton::skeleton_block("100%", 180))
        .into()
}

fn missing(id: &str) -> View {
    el("div")
        .class("sh-card sh-card--pad sh-job__card")
        .child(el("h1").class("sh-author__title").text("No job by that number yet"))
        .child(el("p").text(format!(
            "The queue has no job {id}. A job appears once its upload is taken; this page keeps looking."
        )))
        .child(app_link("/publish").class("sh-link").text("Publish an agent"))
        .into()
}

fn unanswered(id: &str) -> View {
    el("div")
        .class("sh-card sh-card--pad sh-job__card")
        .child(el("h1").class("sh-author__title").text(format!("Job {id}")))
        .child(
            notice(
                NoticeTone::Caution,
                text("The queue isn't answering right now. This page keeps asking; the job keeps going either way."),
            )
            .attr("role", "status"),
        )
        .into()
}

/// The step a job is on, of the four: queued, reviewing, building, ready.
fn step_of(state: &JobState) -> usize {
    match state {
        JobState::AwaitingPayment | JobState::Queued { .. } => 0,
        JobState::Reviewing => 1,
        JobState::Building => 2,
        _ => 3,
    }
}

/// The status card's headline and detail, in the States board's words.
pub fn status_words(job: &Job) -> (String, String) {
    match &job.state {
        JobState::AwaitingPayment => (
            "Waiting for the payment".to_string(),
            format!(
                "The job starts once the wallet that uploaded it has paid {FEE} and the build service has seen the payment confirmed on the chain. If you've just paid, this page moves on by itself."
            ),
        ),
        JobState::Queued { position } => (
            "In the queue".to_string(),
            format!(
                "{} in the queue. You can close this page; the job keeps going.",
                ordinal(*position)
            ),
        ),
        JobState::Reviewing => (
            "The Reviewer is reading your code".to_string(),
            "Every line, against your description and your claims. Each area gets one of four ratings."
                .to_string(),
        ),
        JobState::Building => (
            "Building".to_string(),
            "No build log is shown, so sealed source can't leak through it. A build stops after 20 minutes."
                .to_string(),
        ),
        JobState::Ready => (
            "Ready to activate".to_string(),
            "Review posted and build registered. Nothing runs and nothing is listed until you activate. There's no deadline."
                .to_string(),
        ),
        JobState::Activated => (
            "Activated".to_string(),
            "Its vault is open. The agent's page shows it live.".to_string(),
        ),
        JobState::Refused => (
            "Refused, nothing built".to_string(),
            "The Reviewer found code written to do harm, so nothing was built. The report says why, and it's public."
                .to_string(),
        ),
        JobState::Failed { .. } => (
            "The build didn't finish".to_string(),
            format!("Whichever way it ends, the {FEE} is spent. To try again, publish a new agent."),
        ),
    }
}

pub(crate) fn chip(state: &JobState) -> El {
    match state {
        JobState::AwaitingPayment => badge(Tone::Neutral, Some(Icon::Clock), "Waiting for payment"),
        JobState::Queued { .. } => badge(Tone::Neutral, Some(Icon::Clock), "Queued"),
        JobState::Reviewing => badge(Tone::Brand, Some(Icon::Review), "Reviewing"),
        JobState::Building => badge(Tone::Brand, Some(Icon::Machine), "Building"),
        JobState::Ready => badge(Tone::Gain, Some(Icon::Check), "Ready"),
        JobState::Activated => badge(Tone::Gain, Some(Icon::Check), "Activated"),
        JobState::Refused => badge(Tone::Loss, Some(Icon::Serious), "Refused"),
        JobState::Failed { .. } => badge(Tone::Loss, Some(Icon::Serious), "Build failed"),
    }
}

pub(crate) fn job_view(job: &Job, paid: Option<PaymentRecord>) -> View {
    let title = if job.title.trim().is_empty() {
        format!("Job {}", job.id)
    } else {
        job.title.clone()
    };
    let this_year = parts::this_year(job.submitted.year());
    let submitted = crate::format::day_time(job.submitted, this_year);
    let meta = format!(
        "Submitted {submitted} · {} · vault on {}",
        if job.sealed {
            "sealed source"
        } else {
            "public source"
        },
        job.chain.label()
    );
    let head = el("div").class("sh-author__head").child(
        el("div")
            .class("sh-author__headtext")
            .child(
                el("div")
                    .class("sh-job__titlerow")
                    .child(el("h1").class("sh-author__title").text(title.clone()))
                    .child(match job.strategy_id {
                        Some(id) => el("span")
                            .class("sh-mono sh-muted")
                            .text(format!("agent {id}"))
                            .into(),
                        None => View::Empty,
                    })
                    .child(chip(&job.state)),
            )
            .child(el("p").class("sh-muted").text(meta)),
    );
    let main = if job.state.is_finished() {
        outcome_card(job)
    } else {
        status_card(job, paid)
    };
    el("div")
        .class("sh-job__body")
        .child(crumb(&title))
        .child(head)
        .child(
            el("div")
                .class("sh-job__grid")
                .child(
                    el("div")
                        .class("sh-job__main")
                        .child(main)
                        .child(checking_card(job)),
                )
                .child(facts(job)),
        )
        .into()
}

/// "Your agents / {job}": the way back to every job and agent of yours.
fn crumb(title: &str) -> El {
    el("nav")
        .class("ag-crumb")
        .attr("aria-label", "Breadcrumb")
        .child(
            el("ol")
                .child(
                    el("li")
                        .child(app_link("/you").text("Your agents"))
                        .child(el("span").attr("aria-hidden", "true").text("/")),
                )
                .child(
                    el("li")
                        .attr("aria-current", "page")
                        .text(title.to_string()),
                ),
        )
}

fn status_card(job: &Job, paid: Option<PaymentRecord>) -> View {
    let (headline, detail) = job_words(job, paid);
    let now = step_of(&job.state);
    let steps = [
        ("Queued", "In order, one at a time"),
        ("Reviewing", "Every line read"),
        ("Building", "Stops after 20 minutes"),
        ("Ready", "Or another ending, below"),
    ];
    let running = if job.duration_seconds > 60 {
        format!("{} minutes so far.", job.duration_seconds / 60)
    } else {
        String::new()
    };
    el("section")
        .class("sh-card sh-card--pad sh-job__card")
        .attr("aria-labelledby", "job-now")
        .child(el("p").class("sh-label").text(format!("Now · step {} of 4", now + 1)))
        .child(el("h2").attr("id", "job-now").class("sh-job__headline").text(headline))
        .child(el("p").text(detail))
        .child(if running.is_empty() {
            View::Empty
        } else {
            el("p").class("sh-muted").text(running).into()
        })
        .child(
            el("ol")
                .class("sh-job__steps")
                .children(steps.iter().enumerate().map(|(at, (name, line))| {
                    el("li")
                        .class("sh-job__step")
                        .class_if(at < now, "sh-job__step--done")
                        .class_if(at == now, "sh-job__step--now")
                        .attr_if(at == now, "aria-current", "step")
                        .child(
                            el("span")
                                .class("sh-job__stepmark")
                                .attr("aria-hidden", "true")
                                .child(if at < now {
                                    icon(Icon::Check, 14).into()
                                } else {
                                    text((at + 1).to_string())
                                }),
                        )
                        .child(el("span").class("sh-job__stepname").text(*name))
                        .child(el("span").class("sh-muted sh-job__stepline").text(*line))
                })),
        )
        .child(el("p").class("sh-muted sh-author__small").text(
            "No build log is shown, so sealed source can't leak through it. You'll see where the job is and how it ended. You can close this page; the job keeps going.",
        ))
        .into()
}

fn ratings(job: &Job) -> View {
    let Some(review) = &job.review else {
        return View::Empty;
    };
    el("dl")
        .class("sh-job__ratings")
        .children(
            Area::ALL
                .iter()
                .map(|area| parts::row(area.label(), rating(review.rating(*area)))),
        )
        .into()
}

fn report_link(job: &Job) -> View {
    match &job.strategy_slug {
        Some(slug) => Button::new(Kind::Secondary, "Read the report")
            .icon_before(Icon::Review)
            .href(format!("/agents/{slug}/review"))
            .build()
            .into(),
        None => View::Empty,
    }
}

fn outcome_card(job: &Job) -> View {
    let spent = el("p")
        .class("sh-muted sh-author__small")
        .text(format!(
            "Whichever way it ends, the {FEE} is spent. To try again, publish a new agent."
        ))
        .child(text(" "))
        .child(
            app_link("/publish")
                .class("sh-link")
                .text("Publish a new agent"),
        );
    let card = el("section")
        .class("sh-card sh-card--pad sh-job__card sh-job__outcome")
        .attr("aria-labelledby", "job-outcome");
    match &job.state {
        JobState::Ready | JobState::Activated => {
            let activated = job.state == JobState::Activated;
            let slug = job
                .strategy_id
                .map(|id| shared::Strategy::slug_for(job.chain, id));
            let action: View = match (&slug, activated) {
                (Some(slug), false) => Button::new(Kind::Primary, "Activate agent")
                    .size(Size::Large)
                    .href(format!("/publish/{slug}/activate"))
                    .build()
                    .into(),
                (Some(slug), true) => View::Many(vec![
                    Button::new(Kind::Primary, "Manage your agent")
                        .href(format!("/you/agents/{slug}"))
                        .build()
                        .into(),
                    Button::new(Kind::Secondary, "View public page")
                        .href(format!("/agents/{slug}"))
                        .build()
                        .into(),
                ]),
                (None, _) => View::Empty,
            };
            card.class("sh-job__outcome--ready")
                .child(
                    el("h2")
                        .attr("id", "job-outcome")
                        .class("sh-job__headline")
                        .child(icon(Icon::Seal, 22))
                        .text(if activated {
                            "Activated"
                        } else {
                            "Ready to activate"
                        }),
                )
                .child(el("p").text(if activated {
                    "Its vault is open and the agent is running."
                } else {
                    "Review posted and build registered. Nothing runs and nothing is listed until you activate. There's no deadline."
                }))
                .child(if job.review.is_some() {
                    el("h3")
                        .class("sh-job__h3")
                        .text("The review, at a glance")
                        .into()
                } else {
                    View::Empty
                })
                .child(ratings(job))
                .child(
                    el("div")
                        .class("sh-job__actions")
                        .child(report_link(job))
                        .child(action),
                )
                .child(el("p").class("sh-muted sh-author__small").text(
                    format!("The {FEE} is spent: it paid for this review and this build."),
                ))
                .into()
        }
        JobState::Refused => card
            .class("sh-job__outcome--loss")
            .child(
                el("h2")
                    .attr("id", "job-outcome")
                    .class("sh-job__headline")
                    .child(icon(Icon::Serious, 22))
                    .text("Refused, nothing built"),
            )
            .child(el("p").text(
                "The Reviewer found code written to do harm, so nothing was built. The report says why, and it's public.",
            ))
            .child(el("p").child(el("strong").text("This agent can never be activated.")))
            .child(ratings(job))
            .child(el("div").class("sh-job__actions").child(report_link(job)))
            .child(spent)
            .into(),
        JobState::Failed { outcome } => {
            let why: View = match outcome {
                Failure::Build => View::Many(vec![
                    el("p").text("To see why, run the same build yourself:").into(),
                    el("code")
                        .class("sh-publish__code")
                        .attr("tabindex", "0")
                        .text("cargo build --release --locked --target wasm32-wasip2")
                        .into(),
                    el("p").class("sh-muted").text(
                        "Common causes are on the list you ticked: a committed Cargo.lock, a pinned toolchain, no network, clock or randomness, and 20 minutes or less.",
                    )
                    .into(),
                ]),
                Failure::TimedOut => el("p")
                    .text("It ran past the 20-minute limit, so it was stopped.")
                    .into(),
                Failure::TooLarge => el("p")
                    .text("Its source is over the 2 MB limit, every file counted.")
                    .into(),
                Failure::Manifest => el("p")
                    .text("Its shonin.toml isn't one the platform accepts.")
                    .into(),
                Failure::Service => el("p")
                    .text("It failed on our side, not because of your code.")
                    .into(),
            };
            card.class("sh-job__outcome--loss")
                .child(
                    el("h2")
                        .attr("id", "job-outcome")
                        .class("sh-job__headline")
                        .child(icon(Icon::Serious, 22))
                        .text("The build didn't finish"),
                )
                .child(why)
                .child(spent)
                .into()
        }
        _ => View::Empty,
    }
}

fn checking_card(job: &Job) -> View {
    if job.state.is_finished() {
        return View::Empty;
    }
    el("section")
        .class("sh-card sh-card--pad sh-job__card")
        .child(el("h2").class("sh-job__h3").text("What the Reviewer is checking"))
        .child(el("p").class("sh-muted").text(
            "Every line, against your description and your claims. Each area gets one of four ratings.",
        ))
        .child(
            el("ul")
                .class("sh-job__areas")
                .children(Area::ALL.iter().map(|area| el("li").text(area.label()))),
        )
        .into()
}

fn facts(job: &Job) -> View {
    let this_year = parts::this_year(job.submitted.year());
    let source: View = if job.source_hash.is_empty() {
        text("Not computed yet")
    } else {
        View::Many(vec![
            hash(&job.source_hash, None, "archive hash").into(),
            el("span")
                .class("sh-muted sh-author__small")
                .text("Computed by the attested machine, not by you")
                .into(),
        ])
    };
    el("aside")
        .class("sh-card sh-card--pad sh-job__facts")
        .child(el("h2").class("sh-job__h3").text("Job facts"))
        .child(
            el("dl")
                .class("sh-publish__dl")
                .child(parts::row("Job", parts::words(job.id.clone())))
                .child(match job.strategy_id {
                    Some(id) => parts::row("Agent id", parts::words(id.to_string())),
                    None => View::Empty,
                })
                .child(parts::row(
                    "Submitted",
                    parts::words(crate::format::day_time(job.submitted, this_year)),
                ))
                .child(parts::row("Archive hash", source))
                .child(parts::row(
                    "Sealed",
                    parts::words(if job.sealed {
                        "Yes"
                    } else {
                        "No, public source"
                    }),
                ))
                .child(parts::row(
                    "Vault chain",
                    parts::words(format!("{} ({})", job.chain.label(), job.chain.dollar())),
                ))
                .child(parts::row(
                    "Paid by",
                    match job.author.parse::<alloy_primitives::Address>() {
                        Ok(_) => hash(&job.author, None, "paying wallet").into(),
                        Err(_) => parts::words(job.author.clone()),
                    },
                ))
                .child(match &job.bundle_hash {
                    Some(bundle) => parts::row("Build", hash(bundle, None, "bundle hash")),
                    None => View::Empty,
                }),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(state: JobState) -> Job {
        Job {
            id: "8801".to_string(),
            chain: shared::Chain::Base,
            strategy_id: Some(7),
            title: "Funding basis".to_string(),
            author: "0x2222222222222222222222222222222222222222".to_string(),
            state,
            submitted: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 281).unwrap(),
                time::Time::MIDNIGHT,
            ),
            duration_seconds: 0,
            source_hash: String::new(),
            sealed: false,
            bundle_hash: None,
            review: None,
            strategy_slug: None,
        }
    }

    #[test]
    fn a_queued_job_says_its_place_never_a_time() {
        let (headline, detail) = status_words(&job(JobState::Queued { position: 3 }));
        assert_eq!(headline, "In the queue");
        assert_eq!(
            detail,
            "3rd in the queue. You can close this page; the job keeps going."
        );
    }

    #[test]
    fn every_ending_says_the_fee_is_spent() {
        for state in [
            JobState::Refused,
            JobState::Failed {
                outcome: Failure::Build,
            },
            JobState::Failed {
                outcome: Failure::Service,
            },
            JobState::Ready,
        ] {
            let html = outcome_card(&job(state.clone())).html();
            assert!(html.contains("1 USDC is spent"), "{state:?}: {html}");
        }
    }

    #[test]
    fn a_ready_job_offers_activation_at_its_agents_address() {
        let html = outcome_card(&job(JobState::Ready)).html();
        assert!(html.contains("href=\"/publish/base-7/activate\""));
        assert!(html.contains("Activate agent"));
        let html = outcome_card(&job(JobState::Refused)).html();
        assert!(!html.contains("/activate"));
        assert!(html.contains("This agent can never be activated."));
    }

    #[test]
    fn a_jobs_breadcrumb_starts_at_your_agents() {
        let html = job_view(&job(JobState::Reviewing), None).html();
        let crumb = &html[html
            .find("aria-label=\"Breadcrumb\"")
            .expect("a breadcrumb")..];
        assert!(
            crumb.starts_with("aria-label=\"Breadcrumb\"><ol><li><a href=\"/you\">Your agents</a>"),
            "{crumb}"
        );
        assert!(
            crumb.contains("aria-current=\"page\">Funding basis</li>"),
            "{crumb}"
        );
    }

    #[test]
    fn a_failed_build_names_the_command_to_run_it_yourself() {
        let html = outcome_card(&job(JobState::Failed {
            outcome: Failure::Build,
        }))
        .html();
        assert!(html.contains("cargo build --release --locked --target wasm32-wasip2"));
    }
}
