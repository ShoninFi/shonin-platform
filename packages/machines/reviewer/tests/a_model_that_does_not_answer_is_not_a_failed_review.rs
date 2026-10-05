//! Only a verdict ends a paid job. A review the reviewer could not make for
//! trouble of its own is answered 503 (ask again later), and the build
//! service puts the job back in its queue, its $1 kept.
//!
//! The reviewer's model not answering is that kind of trouble: the pinned
//! model's program can't be started, or exits without an answer (an outage,
//! a rate limit, an expired credential). Nothing about the job made it so,
//! and the next ask may well be answered.
//!
//! Here the reviewer is up and reviewing on its chain, with a pinned model
//! whose program exits without answering. A review is asked for.

mod black_box;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_that_does_not_answer_is_not_a_failed_review() {
    // A model that reads what it is asked and exits without answering.
    let bin = black_box::a_claude("cat > /dev/null\nexit 1");
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        ..Default::default()
    })
    .await;

    let (status, body) = reviewer.review(7, &black_box::project("model-down")).await;
    assert_eq!(
        status,
        503,
        "a review the model did not answer was not answered ask-again-later: {body}\n{}",
        reviewer.said.lock().unwrap()
    );
}
