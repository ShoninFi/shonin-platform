//! The reviewer runs its pinned model as a program on the machine, handing
//! it the author's description and source as its prompt. That program acts
//! on text the author wrote: a review is one place where instructions hidden
//! in the code are meant to be findings, but a program that reads them can
//! still be steered by them, and what it answers with is published.
//!
//! On a testnet the reviewer's environment carries
//! `SHONIN_TESTNET_EVM_MNEMONIC`, from which every role account is derived
//! (the admin, the attestor, the builder and the reviewer itself), the key
//! for the model that answers the battery (`SHONIN_CALIBRATOR_KEY`), and any
//! RPC override or log-query endpoint override, which may carry a
//! provider's key.
//!
//! So the model's program is given none of the reviewer's environment but a
//! `PATH`, a `HOME` and the model's own credentials; no tools, so it can
//! read nothing on the machine and run nothing; a working directory of its
//! own that starts empty; and no session kept on disk. Here the reviewer runs
//! with a testnet mnemonic, a calibrator key and an RPC override in its
//! environment, and with the model's credential; the model's program records
//! the environment, the arguments and the working directory it was given.

mod black_box;

use std::time::{Duration, Instant};

const MNEMONIC: &str = "secret words that must never reach the review model";
const CALIBRATOR_KEY: &str = "calibrator-key-0123456789abcdef";
const RPC: &str = "https://base.example.invalid/v2/provider-api-key-0123456789";
const LOGS_RPC: &str = "https://logs.example.invalid/v2/logs-provider-key-9876543210";
const MODEL_KEY: &str = "the-model-s-own-key-0123456789";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_model_is_given_none_of_the_reviewer_s_secrets() {
    let bin = black_box::a_claude("");
    let seen = bin.join("seen");
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncat > /dev/null\nmkdir {0}.part\nenv > {0}.part/environment\n\
             for argument in \"$@\"; do printf '[%s]\\n' \"$argument\"; done > {0}.part/arguments\n\
             pwd > {0}.part/directory\nls -A > {0}.part/files\nmv {0}.part {0}\nexit 1\n",
            seen.display()
        ),
    )
    .unwrap();
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        env: vec![
            ("SHONIN_TESTNET_EVM_MNEMONIC", MNEMONIC),
            ("SHONIN_CALIBRATOR_KEY", CALIBRATOR_KEY),
            ("SHONIN_RPC_BASE_SEPOLIA", RPC),
            ("SHONIN_LOGS_RPC_EVM_84532", LOGS_RPC),
            ("ANTHROPIC_API_KEY", MODEL_KEY),
        ],
        ..Default::default()
    })
    .await;

    let project = black_box::project("secrets");
    let asked = reviewer.review(7, &project);
    let started = Instant::now();
    let seen_it = async {
        while !seen.exists() {
            if started.elapsed() > Duration::from_secs(60) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        true
    };
    let (_, ran) = tokio::join!(asked, seen_it);
    assert!(
        ran,
        "the model's program never ran:\n{}",
        reviewer.said.lock().unwrap()
    );
    let read = |name: &str| std::fs::read_to_string(seen.join(name)).unwrap();

    let environment = read("environment");
    let leaked: Vec<&str> = [MNEMONIC, CALIBRATOR_KEY, RPC, LOGS_RPC]
        .into_iter()
        .filter(|secret| environment.contains(secret))
        .collect();
    assert!(
        leaked.is_empty(),
        "the review model's program, which acts on text the author wrote, was handed the \
         reviewer's secrets: {leaked:?}"
    );
    let names: Vec<&str> = environment
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    assert!(
        names.iter().all(|name| [
            "PATH",
            "HOME",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "USER",
            "LOGNAME",
            // Set by the shell running the stand-in itself.
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
        ]
        .contains(name)),
        "the review model's program was given more of the reviewer's environment than it \
         needs: {names:?}"
    );
    assert!(
        environment.contains(&format!("ANTHROPIC_API_KEY={MODEL_KEY}")),
        "the review model's program was not given its own credential:\n{environment}"
    );

    let arguments: Vec<String> = read("arguments").lines().map(String::from).collect();
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair[0] == "[--tools]" && pair[1] == "[]"),
        "the review model's program was given tools: {arguments:?}"
    );
    assert!(
        arguments.iter().any(|a| a == "[--no-session-persistence]"),
        "the review model's program keeps its session on disk: {arguments:?}"
    );

    assert_eq!(
        read("files"),
        "",
        "the review model's program started in a directory that was not empty: {}",
        read("directory")
    );
    let directory = std::path::PathBuf::from(read("directory").trim());
    assert!(
        !directory.exists(),
        "the review model's working directory was left behind: {}",
        directory.display()
    );
}
