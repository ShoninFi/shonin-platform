//! The attestation's prover.
//!
//! It asks each attested machine's status port (the executor's, the
//! builder's, the reviewer's) what it wants (`GET /attestation`), and
//! whenever one wants a seal for the token it holds, proves that token in the
//! zkVM with the pinned guest, wraps the proof in Groth16 (RISC Zero runs
//! that step in Docker), checks the receipt as a chain's checker would, and
//! posts the seal back to the machine it came from (`POST
//! /attestation/seal`). It proves one token at a time, each once. A machine
//! that is off is passed over quietly: the builder and the reviewer run only
//! on demand. It holds no secret: the token, the binding and the seal are
//! all public, and only a seal that verifies is of any use.
//!
//! It exits once it has had nothing to do for the idle time, so the machine
//! it runs on can power off.
//!
//! ```text
//! attestation-prover [--executor-url <url>] [--status-url <url>]...
//!     [--idle-exit-seconds 600] [--guest-elf <path>] [--keys <jwks.json>]
//!     [--poll-seconds 15]
//! ```
//!
//! `--executor-url` is one status URL like the others; `SHONIN_EXECUTOR_URL`
//! and `SHONIN_STATUS_URLS` (comma-delimited) add to them.
//!
//! `--guest-elf` names the guest's ELF when this binary was built without it
//! (an image builds the prover without Docker, and ships the ELF built
//! reproducibly elsewhere); either way the guest must be the one every
//! checker pins, or the prover refuses to start. `--keys` is the key set the
//! token is checked against: Google's as pinned when it is left out.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use shonin_attestation::{
    Binding, Guest, Input, Jwk, PINNED_IMAGE_ID, Proof, evm_seal, pinned_image_id,
};
use shonin_attestation_core::GOOGLE_JWKS_2026_10_06;

const USAGE: &str = "usage: attestation-prover [--executor-url <url>] [--status-url <url>]... [--idle-exit-seconds 600] [--guest-elf <path>] [--keys <jwks.json>] [--poll-seconds 15]";

/// How often a machine that doesn't answer is said to.
const UNREACHABLE_SAID_EVERY: Duration = Duration::from_secs(10 * 60);

struct Args {
    /// Every status port asked, in order, each once.
    status_urls: Vec<String>,
    idle_exit: Duration,
    guest_elf: Option<String>,
    keys: Option<String>,
    poll: Duration,
}

fn args() -> Result<Args> {
    let mut args = Args {
        status_urls: std::env::var("SHONIN_EXECUTOR_URL")
            .into_iter()
            .chain(
                std::env::var("SHONIN_STATUS_URLS")
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
            .collect(),
        idle_exit: Duration::from_secs(600),
        guest_elf: std::env::var("SHONIN_GUEST_ELF").ok(),
        keys: None,
        poll: Duration::from_secs(15),
    };
    let mut given = std::env::args().skip(1);
    while let Some(arg) = given.next() {
        let mut value = || given.next().context(USAGE);
        match arg.as_str() {
            "--executor-url" | "--status-url" => args.status_urls.push(value()?),
            "--idle-exit-seconds" => {
                args.idle_exit = Duration::from_secs(value()?.parse().context(USAGE)?)
            }
            "--guest-elf" => args.guest_elf = Some(value()?),
            "--keys" => args.keys = Some(value()?),
            "--poll-seconds" => args.poll = Duration::from_secs(value()?.parse().context(USAGE)?),
            _ => bail!(USAGE),
        }
    }
    let mut urls = Vec::new();
    for url in &args.status_urls {
        let url = url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() && !urls.contains(&url) {
            urls.push(url);
        }
    }
    if urls.is_empty() {
        bail!(USAGE);
    }
    args.status_urls = urls;
    Ok(args)
}

/// What the prover reads of the executor's `GET /attestation`.
#[derive(Debug, Deserialize)]
struct Status {
    nonce: String,
    binding: BindingView,
    token: Option<String>,
    token_issued_at: Option<u64>,
    seal_wanted: bool,
}

#[derive(Debug, Deserialize)]
struct BindingView {
    evm_signer: String,
    solana_signer: String,
    vaults_root: String,
}

fn bytes<const N: usize>(text: &str, what: &str) -> Result<[u8; N]> {
    let mut out = [0u8; N];
    hex::decode_to_slice(text.trim_start_matches("0x"), &mut out)
        .with_context(|| format!("the binding's {what} is not {N} bytes of hex"))?;
    Ok(out)
}

impl BindingView {
    fn binding(&self) -> Result<Binding> {
        Ok(Binding {
            evm_signer: bytes(&self.evm_signer, "EVM key")?,
            solana_signer: bytes(&self.solana_signer, "Solana key")?,
            vaults_root: bytes(&self.vaults_root, "vaults root")?,
        })
    }
}

/// A token wanting a seal: what to prove, and what it is known by so it is
/// proved once.
#[derive(Debug)]
struct Work {
    nonce: String,
    issued_at: u64,
    input: Input,
}

/// The work a status asks for, if any. The token's nonce must be the
/// binding's (a status that disagrees with itself is not proved), and the
/// prover's clock is taken no earlier than the token's issue: a clock a few
/// seconds behind Google's would otherwise find a fresh token not yet valid.
/// The chain judges freshness by the token's own time.
fn work(status: &Status, keys: &[Jwk], now: u64) -> Result<Option<Work>> {
    let (Some(token), Some(issued_at)) = (&status.token, status.token_issued_at) else {
        return Ok(None);
    };
    if !status.seal_wanted {
        return Ok(None);
    }
    let binding = status.binding.binding()?;
    if binding.nonce() != status.nonce {
        bail!("the executor's nonce is not its binding's");
    }
    Ok(Some(Work {
        nonce: status.nonce.clone(),
        issued_at,
        input: Input {
            token: token.clone(),
            keys: keys.to_vec(),
            now: now.max(issued_at),
            binding,
        },
    }))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Status> {
    let mut answer = agent
        .get(format!("{url}/attestation"))
        .call()
        .with_context(|| format!("asking {url} what it wants"))?;
    if !answer.status().is_success() {
        bail!("{url} answered {}", answer.status());
    }
    let text = answer
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}'s answer"))?;
    serde_json::from_str(&text).with_context(|| format!("{url}'s answer"))
}

/// What has been proved already, by where it came from, its nonce and its
/// token's issue time: each is proved once.
type Tried = HashSet<(String, String, u64)>;

/// The next token to prove, and the status URL it came from: the first, in
/// the order given, of the machines that answer and want a seal for a token
/// not yet tried. A machine that doesn't answer, or answers something that
/// can't be proved, is passed over; each such failure is handed to `failed`.
fn next_work(
    urls: &[String],
    fetch: impl Fn(&str) -> Result<Status>,
    keys: &[Jwk],
    now: u64,
    tried: &Tried,
    mut failed: impl FnMut(&str, anyhow::Error),
) -> Option<(String, Work)> {
    for url in urls {
        match fetch(url).and_then(|status| work(&status, keys, now)) {
            Ok(Some(work))
                if !tried.contains(&(url.clone(), work.nonce.clone(), work.issued_at)) =>
            {
                return Some((url.clone(), work));
            }
            Ok(_) => {}
            Err(error) => failed(url, error),
        }
    }
    None
}

fn post(agent: &ureq::Agent, url: &str, nonce: &str, seal: &[u8], journal: &[u8]) -> Result<()> {
    let body = serde_json::json!({
        "nonce": nonce,
        "seal": format!("0x{}", hex::encode(seal)),
        "journal": format!("0x{}", hex::encode(journal)),
    })
    .to_string();
    let mut answer = agent
        .post(format!("{url}/attestation/seal"))
        .header("content-type", "application/json")
        .send(body)
        .context("posting the seal")?;
    let status = answer.status();
    if status.is_success() {
        return Ok(());
    }
    let why = answer.body_mut().read_to_string().unwrap_or_default();
    bail!("{url} refused the seal ({status}): {why}")
}

/// Prove one token, check the receipt as a chain would, and say the EVM
/// seal and journal.
fn prove(guest: &Guest, trusted: &[[u8; 32]], work: &Work) -> Result<(Vec<u8>, Vec<u8>)> {
    shonin_attestation_core::verify(&work.input)
        .map_err(|error| anyhow::anyhow!("the token does not pass the check: {error}"))?;
    let (receipt, journal) = guest.prove(&work.input, Proof::Groth16)?;
    guest
        .verify(&receipt, trusted, false)
        .context("the fresh receipt does not verify")?;
    Ok((evm_seal(&receipt, &journal)?, journal.evm().to_vec()))
}

fn main() -> Result<()> {
    let args = args()?;
    let guest = match &args.guest_elf {
        Some(path) => Guest::from_elf(
            std::fs::read(path).with_context(|| format!("reading the guest's ELF at {path}"))?,
        )?,
        None => Guest::embedded()?,
    };
    if guest.image_id() != pinned_image_id() {
        bail!(
            "the guest's image id is {}, and every checker pins {PINNED_IMAGE_ID}: refusing to prove with it",
            hex::encode(guest.image_id())
        );
    }
    let jwks = match &args.keys {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading the key set {path}"))?
        }
        None => GOOGLE_JWKS_2026_10_06.to_string(),
    };
    let keys = Jwk::from_jwks(&jwks).map_err(|error| anyhow::anyhow!("the key set: {error}"))?;
    let trusted: Vec<[u8; 32]> = keys.iter().map(Jwk::key_hash).collect();
    println!(
        "the prover is up: guest {PINNED_IMAGE_ID}, {} keys, status {}, idle exit after {} s",
        keys.len(),
        args.status_urls.join(", "),
        args.idle_exit.as_secs()
    );

    let agent = agent();
    let mut tried: Tried = HashSet::new();
    let mut last_work = Instant::now();
    let mut unreachable_said: HashMap<String, Instant> = HashMap::new();
    loop {
        let next = next_work(
            &args.status_urls,
            |url| fetch(&agent, url),
            &keys,
            unix_now(),
            &tried,
            |url, error| {
                if unreachable_said
                    .get(url)
                    .is_none_or(|said| said.elapsed() >= UNREACHABLE_SAID_EVERY)
                {
                    println!("{error:#}");
                    unreachable_said.insert(url.to_string(), Instant::now());
                }
            },
        );
        if let Some((url, work)) = next {
            tried.insert((url.clone(), work.nonce.clone(), work.issued_at));
            println!(
                "proving {url}'s token issued at {} for nonce {}",
                work.issued_at, work.nonce
            );
            let started = Instant::now();
            match prove(&guest, &trusted, &work) {
                Ok((seal, journal)) => {
                    println!("proved in {} s", started.elapsed().as_secs());
                    match post(&agent, &url, &work.nonce, &seal, &journal) {
                        Ok(()) => println!("{url} took the seal"),
                        Err(error) => println!("{error:#}"),
                    }
                }
                Err(error) => println!("could not prove the token: {error:#}"),
            }
            last_work = Instant::now();
            // Look again at once: another machine may be waiting.
            continue;
        }
        if last_work.elapsed() >= args.idle_exit {
            println!(
                "nothing to prove for {} s; exiting",
                args.idle_exit.as_secs()
            );
            return Ok(());
        }
        std::thread::sleep(args.poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(binding: &Binding, wanted: bool) -> Status {
        Status {
            nonce: binding.nonce(),
            binding: BindingView {
                evm_signer: format!("0x{}", hex::encode(binding.evm_signer)),
                solana_signer: format!("0x{}", hex::encode(binding.solana_signer)),
                vaults_root: format!("0x{}", hex::encode(binding.vaults_root)),
            },
            token: Some("a.b.c".into()),
            token_issued_at: Some(1_000),
            seal_wanted: wanted,
        }
    }

    fn binding() -> Binding {
        Binding {
            evm_signer: [1; 20],
            solana_signer: [0; 32],
            vaults_root: [2; 32],
        }
    }

    #[test]
    fn a_token_is_proved_only_when_a_seal_is_wanted_for_it() {
        assert!(
            work(&status(&binding(), false), &[], 2_000)
                .unwrap()
                .is_none()
        );
        let mut held = status(&binding(), true);
        held.token = None;
        assert!(work(&held, &[], 2_000).unwrap().is_none());
        let wanted = work(&status(&binding(), true), &[], 2_000)
            .unwrap()
            .unwrap();
        assert_eq!(wanted.input.binding, binding());
        assert_eq!(wanted.nonce, binding().nonce());
        assert_eq!(wanted.issued_at, 1_000);
    }

    #[test]
    fn a_clock_behind_the_token_s_is_taken_as_its_issue() {
        let wanted = work(&status(&binding(), true), &[], 990).unwrap().unwrap();
        assert_eq!(wanted.input.now, 1_000);
        let wanted = work(&status(&binding(), true), &[], 1_060)
            .unwrap()
            .unwrap();
        assert_eq!(wanted.input.now, 1_060);
    }

    #[test]
    fn a_status_whose_nonce_is_not_its_binding_s_is_not_proved() {
        let mut odd = status(&binding(), true);
        odd.nonce = "00".repeat(32);
        assert!(work(&odd, &[], 2_000).is_err());
        let mut malformed = status(&binding(), true);
        malformed.binding.vaults_root = "0x12".into();
        assert!(work(&malformed, &[], 2_000).is_err());
    }

    #[test]
    fn the_executor_s_status_reads_as_the_executor_serves_it() {
        let json = serde_json::json!({
            "mode": "launcher",
            "binding": {
                "evm_signer": format!("0x{}", "11".repeat(20)),
                "solana_signer": format!("0x{}", "00".repeat(32)),
                "vaults_root": format!("0x{}", "22".repeat(32)),
            },
            "nonce": "ab",
            "token": "a.b.c",
            "token_issued_at": 5,
            "seal": null,
            "journal": null,
            "seal_wanted": true,
            "leaves": [],
        });
        let status: Status = serde_json::from_value(json).unwrap();
        assert!(status.seal_wanted);
        assert_eq!(status.token_issued_at, Some(5));
    }

    #[test]
    fn the_prover_proves_for_the_second_url_while_the_first_is_down() {
        let urls = vec!["http://executor".to_string(), "http://builder".to_string()];
        let fetch = |url: &str| -> Result<Status> {
            match url {
                "http://builder" => Ok(status(&binding(), true)),
                _ => bail!("connection refused"),
            }
        };
        let mut failures = Vec::new();
        let (url, work) = next_work(&urls, fetch, &[], 2_000, &HashSet::new(), |url, _| {
            failures.push(url.to_string())
        })
        .unwrap();
        assert_eq!(url, "http://builder");
        assert_eq!(work.nonce, binding().nonce());
        assert_eq!(failures, ["http://executor"]);
    }

    #[test]
    fn each_machine_s_token_is_proved_once_and_another_s_is_not_held_back_by_it() {
        let urls = vec!["http://executor".to_string(), "http://builder".to_string()];
        // Both want a seal for a token with the same nonce and time (as
        // two stand-ins may): tried by its URL, so each is proved once.
        let fetch = |_: &str| -> Result<Status> { Ok(status(&binding(), true)) };
        let mut tried: Tried = HashSet::new();
        let mut proved = Vec::new();
        while let Some((url, work)) = next_work(&urls, fetch, &[], 2_000, &tried, |_, _| {}) {
            tried.insert((url.clone(), work.nonce.clone(), work.issued_at));
            proved.push(url);
        }
        assert_eq!(proved, urls);
        // A fresh token from the first machine is proved again.
        let fresh = |url: &str| -> Result<Status> {
            let mut status = status(&binding(), true);
            if url == "http://executor" {
                status.token_issued_at = Some(1_500);
            }
            Ok(status)
        };
        let (url, work) = next_work(&urls, fresh, &[], 2_000, &tried, |_, _| {}).unwrap();
        assert_eq!((url.as_str(), work.issued_at), ("http://executor", 1_500));
    }

    #[test]
    fn a_service_s_status_reads_as_its_status_port_serves_it() {
        let json = serde_json::json!({
            "mode": "launcher",
            "binding": {
                "evm_signer": format!("0x{}", "11".repeat(20)),
                "solana_signer": format!("0x{}", "00".repeat(32)),
                "vaults_root": format!("0x{}", "22".repeat(32)),
            },
            "nonce": "ab",
            "token": "a.b.c",
            "token_issued_at": 5,
            "seal": null,
            "journal": null,
            "seal_wanted": true,
            "leaves": [{
                "chain_id": 84532,
                "vault_chain": 0,
                "vault": format!("0x{}", "00".repeat(20)),
                "strategy_hash": format!("0x{}", "00".repeat(32)),
                "key": format!("0x{}", "55".repeat(20)),
                "leaf": format!("0x{}", "66".repeat(32)),
                "proof": [],
            }],
            "root": null,
        });
        let status: Status = serde_json::from_value(json).unwrap();
        assert!(status.seal_wanted);
    }
}
