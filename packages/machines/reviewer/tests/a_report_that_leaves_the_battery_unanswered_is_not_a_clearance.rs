//! The one gate in a review is the battery's refusing question: code the
//! model finds malicious is refused, and everything else is cleared. The
//! ratings are computed from the battery's answers, so a go-ahead is given
//! only over a battery answered whole, each question once.
//!
//! The schema the model is held to can't require one answer per question
//! (the ids are data, not schema), so the reviewer checks it: a report that
//! leaves any question unanswered is no answer, 503 (ask again later), as an
//! off-schema answer is, never a clearance.
//!
//! Here the reviewer runs with a pinned model whose program answers on
//! schema but leaves the battery unanswered: every question, and only the
//! refusing one.

mod black_box;

use shonin_reviewer::battery::Battery;

/// A `claude` of the test's own that answers the structured output `report`.
fn a_model_answering(report: &serde_json::Value) -> std::path::PathBuf {
    let reply = serde_json::json!({ "structured_output": report }).to_string();
    // The reply holds no single quotes, so the shell takes it as one word.
    assert!(!reply.contains('\''));
    black_box::a_claude(&format!("cat > /dev/null\nprintf '%s' '{reply}'"))
}

async fn reviewed_by(report: &serde_json::Value, tag: &str) -> (u16, String, String) {
    let bin = a_model_answering(report);
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        ..Default::default()
    })
    .await;
    let (status, body) = reviewer.review(7, &black_box::project(tag)).await;
    let said = reviewer.said.lock().unwrap().clone();
    (status, body, said)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_that_answers_no_battery_question_is_not_a_clearance() {
    let report = serde_json::json!({
        "summary": "A plain agent.",
        "findings": [],
        "limits": [],
        "answers": [],
    });
    let (status, body, said) = reviewed_by(&report, "no-answers").await;
    assert_eq!(
        status, 503,
        "a report that answered none of the battery was taken as a clearance: {body}\n{said}"
    );
    assert!(
        !body.contains("clearance"),
        "a go-ahead was signed over an unanswered battery: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_that_leaves_the_refusing_question_unanswered_is_not_a_clearance() {
    // Every question answered as cleanly as can be, except the one that
    // refuses a build, which is left out.
    let answers: Vec<serde_json::Value> = Battery::pinned()
        .questions
        .iter()
        .filter(|question| !question.refuses)
        .map(|question| {
            serde_json::json!({
                "id": question.id,
                "probability": if question.good { 0.95 } else { 0.05 },
                "confidence": 0.9,
            })
        })
        .collect();
    assert!(
        answers.len() < Battery::pinned().questions.len(),
        "the battery has a refusing question to leave out"
    );
    let report = serde_json::json!({
        "summary": "A plain agent.",
        "findings": [],
        "limits": [],
        "answers": answers,
    });
    let (status, body, said) = reviewed_by(&report, "no-malicious-answer").await;
    assert_eq!(
        status, 503,
        "a report that never answered whether the code is malicious was taken as a clearance: {body}\n{said}"
    );
    assert!(
        !body.contains("clearance"),
        "a go-ahead was signed without the refusing question answered: {body}"
    );
}
