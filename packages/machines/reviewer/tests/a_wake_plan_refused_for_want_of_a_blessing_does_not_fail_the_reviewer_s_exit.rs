//! A wake plan the API refuses for want of the key's blessing is no failure
//! of the reviewer's: it is said once, at info, and the reviewer still exits
//! cleanly once idle, as the build service does.
//!
//! The API takes a wake plan only from a key the canonical chain's registry
//! blesses as the machine's kind, and answers 403 to any other. Here the
//! reviewer runs against a stand-in for the API's internal port that has no
//! state for it and refuses every wake plan 403. Told to exit after one idle
//! second, it sends its plan while it waits, is refused, sends it again at
//! exit, is refused again, and must exit 0 without ever warning that the plan
//! could not be told.

mod black_box;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{Method, StatusCode, Uri};
use axum::response::IntoResponse;

/// Answer a GET for the pending state 404, and refuse every wake-plan PUT
/// 403, as the real internal port does for a key it does not count blessed.
async fn stand_in(method: Method, uri: Uri) -> impl IntoResponse {
    if method == Method::GET {
        return (StatusCode::NOT_FOUND, "no such state");
    }
    if method == Method::PUT && uri.path().ends_with("/plan") {
        return (StatusCode::FORBIDDEN, "the signer is not blessed for this");
    }
    (StatusCode::NO_CONTENT, "")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_plan_refused_for_want_of_a_blessing_does_not_fail_the_reviewer_s_exit() {
    // A stand-in for the API's internal port that refuses every wake plan.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state_url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(stand_in);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });

    let args = vec![
        "--idle-exit-seconds",
        "1",
        "--state-url",
        state_url.as_str(),
    ];
    let mut reviewer = black_box::spawn(black_box::Options {
        node: true,
        args,
        ..Default::default()
    })
    .await;

    // It comes up against the stand-in state port.
    assert!(
        reviewer
            .wait_until_said("the reviewer is open", Duration::from_secs(30))
            .await,
        "the reviewer never came up:\n{}",
        reviewer.said.lock().unwrap()
    );

    // It idles and exits.
    let started = Instant::now();
    let status = loop {
        if let Some(status) = reviewer.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the reviewer never exited once idle:\n{}",
            reviewer.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // A refused wake plan is not the reviewer's own doing: it must exit
    // cleanly, as the build service does, rather than fail on the way out.
    let said = reviewer.said.lock().unwrap().clone();
    assert!(
        status.success(),
        "the reviewer exited non-zero ({status}) when the API refused its wake plan:\n{said}"
    );
    assert!(
        said.contains("once its key is blessed"),
        "the refusal was not said:\n{said}"
    );
    assert!(
        !said.contains("the wake plan could not be told"),
        "the refusal was warned of as a failure:\n{said}"
    );
}
