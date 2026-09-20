//! The public build queue. First come, first served, one job in flight per
//! author, and the log of a failed build is as public as a successful one.

use shared::{Job, JobState};

use super::source::{build_log, failed_build_log};
use super::{hours_ago, sha256};

/// A job as it is written here. Everything else about it is derived.
struct Spec {
    id: &'static str,
    subject: &'static str,
    author: &'static str,
    kind: &'static str,
    state: JobState,
    submitted_hours_ago: u32,
    duration_seconds: u32,
    strategy_slug: Option<&'static str>,
}

pub fn all() -> Vec<Job> {
    vec![
        Spec {
            id: "8871",
            subject: "mean-reversion-hl",
            author: "tidewater",
            kind: "strategy",
            state: JobState::Queued { position: 2 },
            submitted_hours_ago: 0,
            duration_seconds: 0,
            strategy_slug: None,
        },
        Spec {
            id: "8870",
            subject: "twap-exec",
            author: "shonin",
            kind: "component",
            state: JobState::Queued { position: 1 },
            submitted_hours_ago: 1,
            duration_seconds: 0,
            strategy_slug: None,
        },
        Spec {
            id: "8869",
            subject: "grid-sol",
            author: "moraine",
            kind: "strategy",
            state: JobState::Building {
                step: "second pass: comparing outputs".to_string(),
                percent: 72,
            },
            submitted_hours_ago: 1,
            duration_seconds: 104,
            strategy_slug: None,
        },
        Spec {
            id: "8868",
            subject: "vol-harvest",
            author: "northwind",
            kind: "strategy",
            state: JobState::Failed {
                reason: "the two passes produced different bytes".to_string(),
            },
            submitted_hours_ago: 6,
            duration_seconds: 131,
            strategy_slug: Some("vol-harvest-v1"),
        },
        Spec {
            id: "8867",
            subject: "arena",
            author: "moraine",
            kind: "strategy",
            state: JobState::Succeeded,
            submitted_hours_ago: 27 * 24,
            duration_seconds: 142,
            strategy_slug: Some("arena-v1"),
        },
        Spec {
            id: "8862",
            subject: "funding-rank",
            author: "kestrel.lab",
            kind: "component",
            state: JobState::Succeeded,
            submitted_hours_ago: 138 * 24,
            duration_seconds: 38,
            strategy_slug: None,
        },
    ]
    .into_iter()
    .map(build)
    .collect()
}

fn build(spec: Spec) -> Job {
    // A job's log only goes as far as the job has: a queued job has nothing
    // to show yet, and a job that is building shows what it has done so far.
    let full = build_log(spec.subject, spec.duration_seconds.max(1), &[]);
    let log = match &spec.state {
        JobState::Queued { .. } => Vec::new(),
        JobState::Building { .. } => full.into_iter().take(11).collect(),
        JobState::Succeeded => full,
        JobState::Failed { .. } => failed_build_log(spec.subject),
    };

    Job {
        id: spec.id.to_string(),
        subject: spec.subject.to_string(),
        author: spec.author.to_string(),
        kind: spec.kind.to_string(),
        bundle_hash: matches!(spec.state, JobState::Succeeded)
            .then(|| sha256(&format!("{}-v1-bundle", spec.subject))),
        state: spec.state,
        submitted: hours_ago(spec.submitted_hours_ago),
        duration_seconds: spec.duration_seconds,
        source_hash: sha256(&format!("{}-source", spec.subject)),
        log,
        strategy_slug: spec.strategy_slug.map(str::to_string),
    }
}
