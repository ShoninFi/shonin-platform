//! The attestation's prover.
//!
//! It asks the executor's status port what the executor wants
//! (`GET /attestation`), and whenever a seal is wanted for the token it holds,
//! proves that token in the zkVM with the pinned guest, wraps the proof in
//! Groth16 (RISC Zero runs that step in Docker), checks the receipt as a
//! chain's checker would, and posts the seal back (`POST
//! /attestation/seal`). It holds no secret: the token, the binding and the
//! seal are all public, and only a seal that verifies is of any use.
//!
//! It exits once it has had nothing to do for the idle time, so the machine
//! it runs on can power off.
//!
//! ```text
//! attestation-prover --executor-url <url> [--idle-exit-seconds 600]
//!     [--guest-elf <path>] [--keys <jwks.json>] [--poll-seconds 15]
//! ```
//!
//! `--guest-elf` names the guest's ELF when this binary was built without it
//! (an image builds the prover without Docker, and ships the ELF built
//! reproducibly elsewhere); either way the guest must be the one every
//! checker pins, or the prover refuses to start. `--keys` is the key set the
//! token is checked against: Google's as pinned when it is left out.

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use shonin_attestation::{
    Binding, Guest, Input, Jwk, PINNED_IMAGE_ID, Proof, evm_seal, pinned_image_id,
};
use shonin_attestation_core::GOOGLE_JWKS_2026_10_06;

const USAGE: &str = "usage: attestation-prover --executor-url <url> [--idle-exit-seconds 600] [--guest-elf <path>] [--keys <jwks.json>] [--poll-seconds 15]";

struct Args {
    executor_url: String,
    idle_exit: Duration,
    guest_elf: Option<String>,
    keys: Option<String>,
    poll: Duration,
}

fn args() -> Result<Args> {
    let mut args = Args {
        executor_url: std::env::var("SHONIN_EXECUTOR_URL").unwrap_or_default(),
        idle_exit: Duration::from_secs(600),
        guest_elf: std::env::var("SHONIN_GUEST_ELF").ok(),
        keys: None,
        poll: Duration::from_secs(15),
    };
    let mut given = std::env::args().skip(1);
    while let Some(arg) = given.next() {
        let mut value = || given.next().context(USAGE);
        match arg.as_str() {
            "--executor-url" => args.executor_url = value()?,
            "--idle-exit-seconds" => {
                args.idle_exit = Duration::from_secs(value()?.parse().context(USAGE)?)
            }
            "--guest-elf" => args.guest_elf = Some(value()?),
            "--keys" => args.keys = Some(value()?),
            "--poll-seconds" => args.poll = Duration::from_secs(value()?.parse().context(USAGE)?),
            _ => bail!(USAGE),
        }
    }
    if args.executor_url.is_empty() {
        bail!(USAGE);
    }
    args.executor_url = args.executor_url.trim_end_matches('/').to_string();
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

fn fetch(agent: &ureq::Agent, executor: &str) -> Result<Status> {
    let mut answer = agent
        .get(format!("{executor}/attestation"))
        .call()
        .context("asking the executor what it wants")?;
    if !answer.status().is_success() {
        bail!("the executor answered {}", answer.status());
    }
    let text = answer
        .body_mut()
        .read_to_string()
        .context("reading the executor's answer")?;
    serde_json::from_str(&text).context("the executor's answer")
}

fn post(
    agent: &ureq::Agent,
    executor: &str,
    nonce: &str,
    seal: &[u8],
    journal: &[u8],
) -> Result<()> {
    let body = serde_json::json!({
        "nonce": nonce,
        "seal": format!("0x{}", hex::encode(seal)),
        "journal": format!("0x{}", hex::encode(journal)),
    })
    .to_string();
    let mut answer = agent
        .post(format!("{executor}/attestation/seal"))
        .header("content-type", "application/json")
        .send(body)
        .context("posting the seal")?;
    let status = answer.status();
    if status.is_success() {
        return Ok(());
    }
    let why = answer.body_mut().read_to_string().unwrap_or_default();
    bail!("the executor refused the seal ({status}): {why}")
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
        "the prover is up: guest {PINNED_IMAGE_ID}, {} keys, executor {}, idle exit after {} s",
        keys.len(),
        args.executor_url,
        args.idle_exit.as_secs()
    );

    let agent = agent();
    let mut tried: HashSet<(String, u64)> = HashSet::new();
    let mut last_work = Instant::now();
    let mut unreachable_said: Option<Instant> = None;
    loop {
        match fetch(&agent, &args.executor_url).and_then(|status| work(&status, &keys, unix_now()))
        {
            Ok(Some(work)) if !tried.contains(&(work.nonce.clone(), work.issued_at)) => {
                tried.insert((work.nonce.clone(), work.issued_at));
                println!(
                    "proving the token issued at {} for nonce {}",
                    work.issued_at, work.nonce
                );
                let started = Instant::now();
                match prove(&guest, &trusted, &work) {
                    Ok((seal, journal)) => {
                        println!("proved in {} s", started.elapsed().as_secs());
                        match post(&agent, &args.executor_url, &work.nonce, &seal, &journal) {
                            Ok(()) => println!("the executor took the seal"),
                            Err(error) => println!("{error:#}"),
                        }
                    }
                    Err(error) => println!("could not prove the token: {error:#}"),
                }
                last_work = Instant::now();
            }
            Ok(_) => {}
            Err(error) => {
                if unreachable_said.is_none_or(|said| said.elapsed() >= Duration::from_secs(60)) {
                    println!("{error:#}");
                    unreachable_said = Some(Instant::now());
                }
            }
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
}
