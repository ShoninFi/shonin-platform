//! The attestation's prover.
//!
//! It asks each attested machine's status port (the executor's, the
//! builder's, the reviewer's) what it wants (`GET /attestation`), and
//! whenever one wants a seal for the token it holds, proves that token in the
//! zkVM with the pinned guest, wraps the proof in Groth16 (RISC Zero runs
//! that step in Docker), checks the receipt as a chain's checker would, and
//! posts the seal back to the machine it came from (`POST
//! /attestation/seal`). It proves one token at a time, each once. A seal the
//! machine gave no answer about (the post timed out or failed, or a server
//! error) is held and posted again, waiting longer each time, while the
//! machine still wants it, for up to half an hour from its proof, so a slow
//! answer doesn't cost a proof. A machine that is off is passed over
//! quietly: the builder and the reviewer run only on demand. It holds no
//! secret: the token, the binding and the seal are all public, and only a
//! seal that verifies is of any use.
//!
//! It exits once it has had nothing to do for the idle time, so the machine
//! it runs on can power off.
//!
//! ```text
//! attestation-prover [--executor-url <url>] [--status-url <url>]...
//!     [--idle-exit-seconds 600] [--guest-elf <path>] [--keys <jwks.json>]
//!     [--root <root.crt>] [--poll-seconds 15]
//! ```
//!
//! `--executor-url` is one status URL like the others; `SHONIN_EXECUTOR_URL`
//! and `SHONIN_STATUS_URLS` (comma-delimited) add to them.
//!
//! `--guest-elf` names the guest's ELF when this binary was built without it
//! (an image builds the prover without Docker, and ships the ELF built
//! reproducibly elsewhere); either way the guest must be the one every
//! checker pins, or the prover refuses to start. `--keys` is the key set an
//! OIDC token is checked against: Google's as pinned when it is left out. A
//! PKI token carries its own certificate chain, and its proof is checked
//! against the root certificate `--root` names (PEM): Google's Confidential
//! Space Root CA when it is left out.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use shonin_attestation::{
    Binding, Guest, Input, Jwk, PINNED_IMAGE_ID, Proof, evm_seal, pinned_image_id,
};
use shonin_attestation_core::{CONFIDENTIAL_SPACE_ROOT, PINNED_GOOGLE_JWKS, pki};

const USAGE: &str = "usage: attestation-prover [--executor-url <url>] [--status-url <url>]... [--idle-exit-seconds 600] [--guest-elf <path>] [--keys <jwks.json>] [--root <root.crt>] [--poll-seconds 15]";

/// How often a machine that doesn't answer is said to.
const UNREACHABLE_SAID_EVERY: Duration = Duration::from_secs(10 * 60);

/// How long a seal's post may take: before it answers, the machine checks
/// the seal on every chain it holds keys on, one after another, and takes
/// longer the more vaults it holds.
const POST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a seal not yet delivered is held, from its proof, while its
/// machine still wants it: the prover stays up for it that long, and for
/// the last post's answer.
const HOLD_FOR: Duration = Duration::from_secs(30 * 60);

/// The wait before a held seal's first post again, doubled after each one
/// not delivered, up to `RETRY_MOST`.
const RETRY_FIRST: Duration = Duration::from_secs(60);
const RETRY_MOST: Duration = Duration::from_secs(5 * 60);

struct Args {
    /// Every status port asked, in order, each once.
    status_urls: Vec<String>,
    idle_exit: Duration,
    guest_elf: Option<String>,
    keys: Option<String>,
    root: Option<String>,
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
        root: None,
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
            "--root" => args.root = Some(value()?),
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

/// A token by where it came from, its nonce and its issue time.
type Token = (String, String, u64);

/// What has been proved already: each token is proved once.
type Tried = HashSet<Token>;

/// A proved seal and its journal, as posted.
type Sealed = (Vec<u8>, Vec<u8>);

/// A seal proved but not yet delivered, and when to post it again.
#[derive(Debug)]
struct Hold {
    sealed: Sealed,
    proved: Instant,
    next_try: Instant,
    wait: Duration,
}

impl Hold {
    /// A seal proved at `proved` whose first post ended at `posted`.
    fn new(sealed: Sealed, proved: Instant, posted: Instant) -> Self {
        Hold {
            sealed,
            proved,
            next_try: posted + RETRY_FIRST,
            wait: RETRY_FIRST,
        }
    }

    /// Not delivered again: wait twice as long before the next post.
    fn later(&mut self, now: Instant) {
        self.wait = (self.wait * 2).min(RETRY_MOST);
        self.next_try = now + self.wait;
    }
}

/// Seals proved but not yet delivered, by the token each proves.
type Held = HashMap<Token, Hold>;

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

/// What became of a posted seal.
#[derive(Debug)]
enum Posted {
    Taken,
    /// The machine looked at the seal and turned it away (a newer binding,
    /// or a seal its chains would refuse): posting it again changes nothing.
    Refused(String),
    /// No answer about the seal itself: the post timed out or failed, the
    /// machine could not check it yet (a server error), or asked to be
    /// asked later. The seal is still good for its token, and is kept to
    /// post again.
    Undelivered(anyhow::Error),
}

/// What a machine's answer to a posted seal says became of it.
fn answered(url: &str, status: ureq::http::StatusCode, why: String) -> Posted {
    if status.is_success() {
        Posted::Taken
    } else if status.is_server_error()
        || status == ureq::http::StatusCode::REQUEST_TIMEOUT
        || status == ureq::http::StatusCode::TOO_MANY_REQUESTS
    {
        Posted::Undelivered(anyhow::anyhow!(
            "{url} could not take the seal yet ({status}): {why}"
        ))
    } else {
        Posted::Refused(format!("{url} refused the seal ({status}): {why}"))
    }
}

fn post(agent: &ureq::Agent, url: &str, nonce: &str, sealed: &Sealed) -> Posted {
    let (seal, journal) = sealed;
    let body = serde_json::json!({
        "nonce": nonce,
        "seal": format!("0x{}", hex::encode(seal)),
        "journal": format!("0x{}", hex::encode(journal)),
    })
    .to_string();
    let answer = agent
        .post(format!("{url}/attestation/seal"))
        .config()
        .timeout_global(Some(POST_TIMEOUT))
        .build()
        .header("content-type", "application/json")
        .send(body);
    match answer {
        Ok(mut answer) => {
            let status = answer.status();
            let why = answer.body_mut().read_to_string().unwrap_or_default();
            answered(url, status, why)
        }
        Err(error) => Posted::Undelivered(anyhow::Error::new(error).context("posting the seal")),
    }
}

/// Whether a status still wants a seal for this token.
fn still_wanted(status: &Status, (_, nonce, issued_at): &Token) -> bool {
    status.seal_wanted && &status.nonce == nonce && status.token_issued_at == Some(*issued_at)
}

/// Post again the held seal most overdue, if one is due, and drop the
/// held seals that are taken, refused, no longer wanted (the machine moved
/// on to a newer binding or token) or held for `HOLD_FOR` already. A
/// machine that doesn't answer keeps its seal until then. One post at a
/// time, so a new token waits on at most one slow answer.
fn deliver_held(
    held: &mut Held,
    now: Instant,
    fetch: impl Fn(&str) -> Result<Status>,
    post: impl FnOnce(&str, &str, &Sealed) -> Posted,
) {
    held.retain(|(url, nonce, _), hold| {
        let keep = now.duration_since(hold.proved) < HOLD_FOR;
        if !keep {
            println!(
                "{url} has not taken the seal for nonce {nonce} in {} s; dropped",
                HOLD_FOR.as_secs()
            );
        }
        keep
    });
    let Some(token) = held
        .iter()
        .filter(|(_, hold)| hold.next_try <= now)
        .min_by_key(|(_, hold)| hold.next_try)
        .map(|(token, _)| token.clone())
    else {
        return;
    };
    let (url, nonce, _) = &token;
    let keep = match fetch(url) {
        Err(_) => true,
        Ok(status) if !still_wanted(&status, &token) => {
            println!("{url} no longer wants the held seal for nonce {nonce}; dropped");
            false
        }
        Ok(_) => match post(url, nonce, &held[&token].sealed) {
            Posted::Taken => {
                println!("{url} took the held seal");
                false
            }
            Posted::Refused(why) => {
                println!("{why}");
                false
            }
            Posted::Undelivered(error) => {
                println!("{error:#}; the seal is kept to post again");
                true
            }
        },
    };
    if keep {
        // The wait runs from the post's end: a post that ran to its timeout
        // is not followed by another at once.
        if let Some(hold) = held.get_mut(&token) {
            hold.later(now.max(Instant::now()));
        }
    } else {
        held.remove(&token);
    }
}

/// What a fresh receipt is checked against, as the checkers do: the keys
/// given (an OIDC token's), and the root certificate's hash (a PKI token's).
fn trusted(keys: &[Jwk], root: [u8; 32]) -> Vec<[u8; 32]> {
    keys.iter().map(Jwk::key_hash).chain([root]).collect()
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
        None => PINNED_GOOGLE_JWKS.to_string(),
    };
    let keys = Jwk::from_jwks(&jwks).map_err(|error| anyhow::anyhow!("the key set: {error}"))?;
    let root = match &args.root {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading the root {path}"))?
        }
        None => CONFIDENTIAL_SPACE_ROOT.to_string(),
    };
    let root = pki::root_hash(
        &pki::pem_der(&root).map_err(|error| anyhow::anyhow!("the root certificate: {error}"))?,
    );
    let trusted = trusted(&keys, root);
    println!(
        "the prover is up: guest {PINNED_IMAGE_ID}, {} keys, status {}, idle exit after {} s",
        keys.len(),
        args.status_urls.join(", "),
        args.idle_exit.as_secs()
    );

    let agent = agent();
    let mut tried: Tried = HashSet::new();
    let mut held: Held = HashMap::new();
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
                Ok(sealed) => {
                    let proved = Instant::now();
                    println!("proved in {} s", started.elapsed().as_secs());
                    match post(&agent, &url, &work.nonce, &sealed) {
                        Posted::Taken => println!("{url} took the seal"),
                        Posted::Refused(why) => println!("{why}"),
                        Posted::Undelivered(error) => {
                            println!("{error:#}; the seal is kept to post again");
                            held.insert(
                                (url.clone(), work.nonce.clone(), work.issued_at),
                                Hold::new(sealed, proved, Instant::now()),
                            );
                        }
                    }
                }
                Err(error) => println!("could not prove the token: {error:#}"),
            }
            last_work = Instant::now();
            // Look again at once: another machine may be waiting.
            continue;
        }
        // Nothing new to prove: deliver what was proved and not taken. A
        // held seal keeps the prover up, for `HOLD_FOR` at most.
        if !held.is_empty() {
            deliver_held(
                &mut held,
                Instant::now(),
                |url| fetch(&agent, url),
                |url, nonce, sealed| post(&agent, url, nonce, sealed),
            );
        }
        if held.is_empty() && last_work.elapsed() >= args.idle_exit {
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
    fn a_receipt_is_checked_against_the_keys_given_and_the_root() {
        let keys = Jwk::from_jwks(PINNED_GOOGLE_JWKS).unwrap();
        let google = shonin_attestation_core::confidential_space_root_hash();
        let trusted = trusted(&keys, google);
        assert_eq!(trusted.len(), keys.len() + 1);
        assert!(keys.iter().all(|key| trusted.contains(&key.key_hash())));
        assert!(trusted.contains(&google));
        // Google's root is the one pinned by default.
        assert_eq!(
            pki::root_hash(&pki::pem_der(CONFIDENTIAL_SPACE_ROOT).unwrap()),
            google
        );
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

    fn token(url: &str) -> Token {
        (url.to_string(), binding().nonce(), 1_000)
    }

    fn held(url: &str, now: Instant) -> Held {
        HashMap::from([(token(url), Hold::new((vec![1], vec![2]), now, now))])
    }

    #[test]
    fn a_seal_is_kept_only_when_the_machine_gave_no_answer_about_it() {
        use ureq::http::StatusCode;
        let url = "http://executor";
        assert!(matches!(
            answered(url, StatusCode::NO_CONTENT, String::new()),
            Posted::Taken
        ));
        for refused in [
            StatusCode::CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::BAD_REQUEST,
        ] {
            assert!(matches!(
                answered(url, refused, "no".into()),
                Posted::Refused(_)
            ));
        }
        for unanswered in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::GATEWAY_TIMEOUT,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(matches!(
                answered(url, unanswered, "later".into()),
                Posted::Undelivered(_)
            ));
        }
    }

    #[test]
    fn a_held_token_is_not_proved_again_while_its_seal_is_posted_until_taken() {
        let start = Instant::now();
        let urls = vec!["http://executor".to_string()];
        let fetch = |_: &str| -> Result<Status> { Ok(status(&binding(), true)) };
        // Proved once: the token is tried, and its seal held.
        let tried: Tried = HashSet::from([token("http://executor")]);
        let mut held = held("http://executor", start);
        assert!(next_work(&urls, fetch, &[], 2_000, &tried, |_, _| {}).is_none());

        // Not due before the first wait.
        deliver_held(&mut held, start, fetch, |_, _, _| {
            panic!("posted again at once")
        });
        let mut posts = 0;
        let mut at = start + RETRY_FIRST;
        deliver_held(&mut held, at, fetch, |_, _, sealed| {
            posts += 1;
            assert_eq!(sealed, &(vec![1], vec![2]));
            Posted::Undelivered(anyhow::anyhow!("timed out"))
        });
        assert_eq!((posts, held.len()), (1, 1));
        assert!(next_work(&urls, fetch, &[], 2_000, &tried, |_, _| {}).is_none());

        // The wait doubles after each post not delivered, up to its most.
        let waits: Vec<Duration> = (0..4)
            .map(|_| {
                let wait = held[&token("http://executor")].next_try - at;
                at += wait;
                deliver_held(&mut held, at, fetch, |_, _, _| {
                    Posted::Undelivered(anyhow::anyhow!("timed out"))
                });
                wait
            })
            .collect();
        assert_eq!(
            waits,
            [RETRY_FIRST * 2, RETRY_FIRST * 4, RETRY_MOST, RETRY_MOST]
        );
        at = held[&token("http://executor")].next_try;
        deliver_held(&mut held, at, fetch, |_, _, _| Posted::Taken);
        assert!(held.is_empty());
    }

    #[test]
    fn a_held_seal_is_dropped_once_refused_or_no_longer_wanted() {
        let start = Instant::now();
        let due = start + RETRY_FIRST;
        let fetch = |_: &str| -> Result<Status> { Ok(status(&binding(), true)) };
        let mut kept = held("http://executor", start);
        deliver_held(&mut kept, due, fetch, |_, _, _| {
            Posted::Refused("409".into())
        });
        assert!(kept.is_empty());

        // The machine has a seal already, a newer token, or a newer binding.
        for moved_on in [
            |status: &mut Status| status.seal_wanted = false,
            |status: &mut Status| status.token_issued_at = Some(1_500),
            |status: &mut Status| status.nonce = "00".repeat(32),
        ] {
            let mut kept = held("http://executor", start);
            let fetch = |_: &str| -> Result<Status> {
                let mut status = status(&binding(), true);
                moved_on(&mut status);
                Ok(status)
            };
            deliver_held(&mut kept, due, fetch, |_, _, _| {
                panic!("posted a seal no longer wanted")
            });
            assert!(kept.is_empty());
        }
    }

    #[test]
    fn a_held_seal_waits_while_its_machine_does_not_answer_and_is_dropped_after_the_hold() {
        let start = Instant::now();
        let mut kept = held("http://builder", start);
        let fetch = |_: &str| -> Result<Status> { bail!("connection refused") };
        deliver_held(&mut kept, start + RETRY_FIRST, fetch, |_, _, _| {
            panic!("posted to a machine that is off")
        });
        assert_eq!(kept.len(), 1);
        deliver_held(&mut kept, start + HOLD_FOR, fetch, |_, _, _| {
            panic!("posted a seal held too long")
        });
        assert!(kept.is_empty());
    }

    #[test]
    fn one_held_seal_is_posted_at_a_time_the_most_overdue_first() {
        let start = Instant::now();
        let mut kept = held("http://executor", start);
        kept.insert(
            token("http://builder"),
            Hold::new(
                (vec![3], vec![4]),
                start - Duration::from_secs(10),
                start - Duration::from_secs(10),
            ),
        );
        let fetch = |_: &str| -> Result<Status> { Ok(status(&binding(), true)) };
        let mut posted = Vec::new();
        deliver_held(&mut kept, start + RETRY_FIRST, fetch, |url, _, _| {
            posted.push(url.to_string());
            Posted::Taken
        });
        assert_eq!(posted, ["http://builder"]);
        assert_eq!(kept.len(), 1);
    }
}
