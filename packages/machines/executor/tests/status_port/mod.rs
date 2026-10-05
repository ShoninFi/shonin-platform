//! Starting the executor binary on a status port the system picks, and
//! finding the port it bound: a port picked by the test and let go before
//! the executor binds it can be taken by another test's executor first, and
//! its reads would then be that executor's.

#![allow(dead_code)]

use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The executor process and everything it has said.
pub struct Started {
    pub child: Child,
    pub said: Arc<Mutex<String>>,
    /// The status port it bound.
    pub port: u16,
}

/// Run `command` (the executor, with everything but its port) with
/// `--port 0`, keep what it says, and wait for the status port it bound.
pub async fn start(command: &mut Command) -> Started {
    let mut child = command
        .args(["--port", "0"])
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let said = Arc::new(Mutex::new(String::new()));
    let streams: [Option<Box<dyn std::io::Read + Send>>; 2] = [
        child.stdout.take().map(|s| Box::new(s) as _),
        child.stderr.take().map(|s| Box::new(s) as _),
    ];
    for stream in streams.into_iter().flatten() {
        let said = said.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                let mut said = said.lock().unwrap();
                said.push_str(&line);
                said.push('\n');
            }
        });
    }
    // A binary just built can take a while to start the first time (the
    // system looks it over), so this waits generously.
    let began = Instant::now();
    loop {
        let port = bound(&said.lock().unwrap());
        if let Some(port) = port {
            return Started { child, said, port };
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "the executor stopped ({status}) before binding its status port:\n{}",
                said.lock().unwrap()
            );
        }
        assert!(
            began.elapsed() < Duration::from_secs(180),
            "the executor never said what status port it bound:\n{}",
            said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The status port the executor said it bound, if it has.
pub fn bound(said: &str) -> Option<u16> {
    said.lines()
        .find(|line| line.contains("the status port is bound"))?
        .split_whitespace()
        .find_map(|word| word.strip_prefix("port=")?.parse().ok())
}
