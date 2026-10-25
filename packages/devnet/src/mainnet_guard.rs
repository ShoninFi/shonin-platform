//! What stands between a mainnet command and a mainnet send.
//!
//! A mainnet command runs in one of three modes, decided from its flags
//! before any chain is asked anything ([`decide`]):
//!
//! - **a dry run** (no sending flag): the plan is printed and nothing is
//!   sent anywhere. What it reads, it reads from the mainnets: its
//!   endpoints are held to a real send's rules (none local, none anvil),
//!   since what it prints (the admin's steps still to sign, the read-back)
//!   is taken from what they answer;
//! - **a fork rehearsal** (`--on-fork`): every endpoint must be a local fork
//!   (a loopback address, answered by anvil) of the chain its key names, and
//!   what it writes goes under a rehearsal directory, never beside the
//!   committed mainnet addresses;
//! - **a real send**: only with the admin's address, a deployer key read
//!   from a file the owner names, and a confirmation naming exactly the
//!   chain ids being sent to. No endpoint may be a local one.
//!
//! The admin's address is required in every mode: every address the deploy
//! computes follows from it. So is the generation, which is
//! [`MAINNET_GENERATION`] on mainnet and nothing else.
//!
//! Every endpoint a chain names is held to the mode's rules, the log
//! queries' and the websocket's too: a client fails over between them, so
//! one that doesn't suit the mode would be reached sooner or later.

use std::path::{Path, PathBuf};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Result, anyhow, bail};
use chain::ChainConfig;

/// The generation of the mainnet deployment: the salt of the contracts at
/// one address on every chain (the station factory, the station, the
/// closer). Fixed, so the addresses a rehearsal shows are the ones mainnet
/// gets; it is given explicitly all the same, and anything else is refused.
pub const MAINNET_GENERATION: u64 = 1;

/// The flags a mainnet command takes, as given.
#[derive(Clone, Debug, Default)]
pub struct Flags {
    /// The hardware key's address: admin of everything from the first
    /// transaction.
    pub admin: Option<String>,
    /// A file holding the deployer's private key (one `0x` hex line), named
    /// by the owner.
    pub deployer_keyfile: Option<PathBuf>,
    /// The chain ids being sent to, comma-separated, exactly.
    pub confirm_mainnet: Option<String>,
    /// Rehearse on local forks.
    pub on_fork: bool,
    /// The deployment generation, given explicitly.
    pub generation: Option<u64>,
}

/// How a mainnet command runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Print the plan; send nothing.
    DryRun,
    /// Send to local forks only.
    Fork,
    /// Send to the mainnets.
    Send,
}

/// What the flags came to.
#[derive(Clone, Debug)]
pub struct Decided {
    pub mode: Mode,
    pub admin: Address,
    pub generation: u64,
}

/// The confirmation a real send to `chain_ids` takes: the ids, ascending,
/// comma-separated.
pub fn confirmation_for(chain_ids: &[u64]) -> String {
    let mut ids = chain_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// Whether `given` confirms exactly `chain_ids`, in any order.
fn confirms(given: &str, chain_ids: &[u64]) -> bool {
    let mut named: Vec<u64> = Vec::new();
    for part in given.split(',') {
        match part.trim().parse::<u64>() {
            Ok(id) => named.push(id),
            Err(_) => return false,
        }
    }
    named.sort_unstable();
    named.dedup();
    let mut wanted = chain_ids.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    !named.is_empty() && named == wanted
}

/// The mode the flags ask for, with the admin and the generation, for a
/// command that would send to `chain_ids`. Refuses before anything is read
/// from any chain: without the admin's address or a generation, a fork flag
/// beside the real send's flags, and a send missing any of its flags.
pub fn decide(flags: &Flags, chain_ids: &[u64]) -> Result<Decided> {
    parse_admin(flags)?;
    let generation = flags.generation.ok_or_else(|| {
        anyhow!(
            "GENERATION={MAINNET_GENERATION} is required on mainnet, given explicitly on the \
             command line: it fixes the station factory's and the closer's addresses. Nothing \
             was sent."
        )
    })?;
    if generation != MAINNET_GENERATION {
        bail!(
            "GENERATION={generation} is refused: the mainnet deployment is generation \
             {MAINNET_GENERATION}, and no other. Nothing was sent."
        );
    }
    let (mode, admin) = mode_and_admin(flags, chain_ids)?;
    Ok(Decided {
        mode,
        admin,
        generation,
    })
}

/// The mode the flags ask for, with the admin, for a command that takes no
/// generation of its own (the admin's steps: the steps file names what they
/// are for). The same refusals as [`decide`], but for the generation.
pub fn decide_without_generation(flags: &Flags, chain_ids: &[u64]) -> Result<(Mode, Address)> {
    if let Some(generation) = flags.generation
        && generation != MAINNET_GENERATION
    {
        bail!(
            "GENERATION={generation} is refused: the mainnet deployment is generation \
             {MAINNET_GENERATION}, and no other. Nothing was sent."
        );
    }
    mode_and_admin(flags, chain_ids)
}

/// The admin's address, required: never missing, never zero.
fn parse_admin(flags: &Flags) -> Result<Address> {
    let admin = flags
        .admin
        .as_deref()
        .map(str::trim)
        .filter(|admin| !admin.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "ADMIN=<address> is required on mainnet: the hardware key's address, admin of every \
                 contract from the first transaction. Nothing was sent."
            )
        })?;
    let admin: Address = admin
        .parse()
        .map_err(|_| anyhow!("ADMIN is not an address ({admin}). Nothing was sent."))?;
    if admin == Address::ZERO {
        bail!("ADMIN is the zero address. Nothing was sent.");
    }
    Ok(admin)
}

fn mode_and_admin(flags: &Flags, chain_ids: &[u64]) -> Result<(Mode, Address)> {
    let admin = parse_admin(flags)?;
    if chain_ids.is_empty() {
        bail!("no mainnet chain to deploy to");
    }
    let confirm = flags
        .confirm_mainnet
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if flags.on_fork {
        if confirm.is_some() {
            bail!(
                "ON_FORK and CONFIRM_MAINNET together: a rehearsal on forks never confirms a \
                 mainnet send. Pass one or the other. Nothing was sent."
            );
        }
        return Ok((Mode::Fork, admin));
    }
    let wanted = confirmation_for(chain_ids);
    match (&flags.deployer_keyfile, confirm) {
        (None, None) => Ok((Mode::DryRun, admin)),
        (None, Some(_)) => bail!(
            "sending is refused: CONFIRM_MAINNET is given but no DEPLOYER_KEYFILE=<path> (the \
             deployer's key, from a file the owner holds). Nothing was sent."
        ),
        (Some(_), None) => bail!(
            "sending is refused: DEPLOYER_KEYFILE is given but no CONFIRM_MAINNET={wanted} (the \
             chain ids being sent to). Nothing was sent."
        ),
        (Some(_), Some(given)) if !confirms(given, chain_ids) => bail!(
            "sending is refused: CONFIRM_MAINNET={given} does not name exactly the chains being \
             sent to ({wanted}). Nothing was sent."
        ),
        (Some(_), Some(_)) => Ok((Mode::Send, admin)),
    }
}

/// Whether an endpoint's URL is on this machine: a loopback address or
/// `localhost`.
pub fn is_local_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url.trim()) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Refuse a chain's endpoints that don't suit the mode: on a fork, any that
/// isn't local; otherwise (a real send, or a dry run's reads of the
/// mainnets) any that is. The URLs are never printed (a paid one carries its
/// key): the chain is named by its label.
pub fn check_endpoints(mode: Mode, label: &str, rpc_list: &str) -> Result<()> {
    let urls: Vec<&str> = rpc_list.split_whitespace().collect();
    if urls.is_empty() {
        bail!("{label} has no endpoint");
    }
    match mode {
        Mode::Fork => {
            if let Some(index) = urls.iter().position(|url| !is_local_url(url)) {
                bail!(
                    "{label}'s endpoint {} of {} is not on this machine: ON_FORK sends to local \
                     forks alone (set SHONIN_RPC_<chain>=http://127.0.0.1:<port> to the fork: \
                     SHONIN_RPC_EVM_<id>, SHONIN_RPC_SOLANA_MAINNET_BETA, and any \
                     SHONIN_LOGS_RPC_<chain> or SHONIN_WS_<chain> to the fork or unset). Nothing \
                     was sent.",
                    index + 1,
                    urls.len()
                );
            }
        }
        Mode::Send | Mode::DryRun => {
            if let Some(index) = urls.iter().position(|url| is_local_url(url)) {
                bail!(
                    "{label}'s endpoint {} of {} is on this machine: without ON_FORK=1 a mainnet \
                     command reads and sends to the mainnets alone, never to a fork (unset the \
                     SHONIN_RPC_*, SHONIN_LOGS_RPC_* and SHONIN_WS_* a rehearsal set, or pass \
                     ON_FORK=1 to rehearse). Nothing was sent.",
                    index + 1,
                    urls.len()
                );
            }
        }
    }
    Ok(())
}

/// Every list of endpoints a chain's configuration names, each with what it
/// is for: the requests', the log queries' (`logs_rpc`, which an override
/// from the environment may set apart from `rpc`) and the websocket's.
pub fn endpoint_lists(config: &ChainConfig) -> Vec<(&'static str, String)> {
    let mut lists = vec![("requests", config.rpc.clone())];
    if let Some(logs) = config
        .logs_rpc
        .as_deref()
        .filter(|list| !list.trim().is_empty())
    {
        lists.push(("log queries", logs.to_string()));
    }
    if let Some(ws) = config.ws.as_deref().filter(|list| !list.trim().is_empty()) {
        lists.push(("websocket", ws.to_string()));
    }
    lists
}

/// [`check_endpoints`] on every endpoint a chain's configuration names, the
/// log queries' and the websocket's included.
pub fn check_config_endpoints(mode: Mode, config: &ChainConfig) -> Result<()> {
    for (what, list) in endpoint_lists(config) {
        check_endpoints(mode, &format!("{} ({what})", config.label), &list)?;
    }
    Ok(())
}

/// Refuse a node that doesn't suit the mode, from what it says it is: on a
/// fork it must be anvil, and otherwise (a real send, or a dry run's reads
/// of the mainnets) it must not be.
pub fn check_node_version(mode: Mode, label: &str, version: &str) -> Result<()> {
    let anvil = version.to_ascii_lowercase().contains("anvil");
    match mode {
        Mode::Fork if !anvil => bail!(
            "{label} answers as {version:?}, not a local anvil fork: ON_FORK sends nothing to a \
             public chain. Nothing was sent."
        ),
        Mode::Send | Mode::DryRun if anvil => bail!(
            "{label} answers as anvil: without ON_FORK=1 a mainnet command never reads from or \
             sends to a fork. Nothing was sent."
        ),
        _ => Ok(()),
    }
}

/// Refuse a node answering as another chain than the one its key names: a
/// fork of the wrong chain, or an endpoint set for the wrong id.
pub fn check_chain_id(label: &str, expected: u64, answered: u64) -> Result<()> {
    if expected != answered {
        bail!(
            "{label} answers as chain {answered}, not {expected}: the endpoint is not this chain's \
             (or the fork is of another chain). Nothing was sent."
        );
    }
    Ok(())
}

/// Ask one endpoint `method`, directly (not through a failover). The URL
/// never appears in an error.
async fn ask(
    http: &reqwest::Client,
    url: &str,
    method: &str,
) -> std::result::Result<serde_json::Value, String> {
    let answer: serde_json::Value = http
        .post(url)
        .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": []}))
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?
        .json()
        .await
        .map_err(|error| error.without_url().to_string())?;
    answer
        .get("result")
        .cloned()
        .ok_or_else(|| format!("no answer to {method}"))
}

/// Ask every HTTP endpoint a chain names (the requests' and the log
/// queries', each one, since a client fails over between them) what it is
/// and which chain it serves, and refuse the chain unless each one suits the
/// mode and answers as `chain_id`. An endpoint that doesn't answer is
/// refused too: it could be anything.
pub async fn check_nodes(mode: Mode, config: &ChainConfig, chain_id: u64) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    for (what, list) in endpoint_lists(config) {
        if what == "websocket" {
            continue;
        }
        let urls: Vec<&str> = list.split_whitespace().collect();
        for (index, url) in urls.iter().enumerate() {
            let label = format!(
                "{} ({what}, endpoint {} of {})",
                config.label,
                index + 1,
                urls.len()
            );
            let version = ask(&http, url, "web3_clientVersion")
                .await
                .map_err(|error| {
                    anyhow!("asking {label} what it is: {error}. Nothing was sent.")
                })?;
            check_node_version(mode, &label, version.as_str().unwrap_or_default())?;
            let answered = ask(&http, url, "eth_chainId")
                .await
                .map_err(|error| anyhow!("asking {label} its chain: {error}. Nothing was sent."))?;
            let answered = answered
                .as_str()
                .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                .ok_or_else(|| anyhow!("{label} answers no chain id. Nothing was sent."))?;
            check_chain_id(&label, chain_id, answered)?;
        }
    }
    Ok(())
}

/// Every check a chain's endpoints take before a mainnet command reads or
/// sends anything: where each one is ([`check_config_endpoints`]), then
/// what each one is and which chain it serves ([`check_nodes`]).
pub async fn check_chain(mode: Mode, config: &ChainConfig, chain_id: u64) -> Result<()> {
    check_config_endpoints(mode, config)?;
    check_nodes(mode, config, chain_id).await
}

/// Where the committed mainnet addresses are, relative to the repository.
pub const COMMITTED: [&str; 3] = ["config", "deployments", "mainnet"];

/// This repository's committed mainnet deployments directory.
pub fn committed_dir() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    for part in COMMITTED {
        dir.push(part);
    }
    dir
}

/// `dir` as the disk resolves it, absolute: every symlink followed and every
/// `.` and `..` gone. The part that doesn't exist yet is appended to its
/// deepest existing ancestor, resolved; a `..` in that part is refused.
pub fn resolved(dir: &Path) -> Result<PathBuf> {
    use std::path::Component;
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let mut missing = Vec::new();
    let mut existing = absolute.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            let mut out = real;
            for part in missing.iter().rev() {
                match part {
                    Component::Normal(name) => out.push(name),
                    Component::CurDir => {}
                    _ => bail!(
                        "{} names a directory through one that doesn't exist yet",
                        dir.display()
                    ),
                }
            }
            return Ok(out);
        }
        let Some(parent) = existing.parent() else {
            bail!("{} has no existing ancestor", dir.display());
        };
        missing.push(existing.components().next_back().expect("a last part"));
        existing = parent;
    }
}

/// Whether `a` and `b` (both resolved) are the same directory: the same file
/// on disk when both exist (whatever their spelling, on a disk that ignores
/// case), otherwise the same path ignoring case.
fn same_dir(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
        use std::os::unix::fs::MetadataExt;
        return a.dev() == b.dev() && a.ino() == b.ino();
    }
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Whether `dir` is the committed mainnet deployments directory, or inside
/// it, however it is spelled: through a symlink, a `..`, or another letter
/// case. A path that can't be resolved counts as the committed directory,
/// and so does any whose parts name `config/deployments/mainnet` in any
/// case.
pub fn is_committed_dir(dir: &Path) -> bool {
    let Ok(dir) = resolved(dir) else {
        return true;
    };
    let committed = resolved(&committed_dir()).unwrap_or_else(|_| committed_dir());
    if dir
        .ancestors()
        .any(|ancestor| same_dir(ancestor, &committed))
    {
        return true;
    }
    let parts: Vec<String> = dir
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    parts
        .windows(COMMITTED.len())
        .any(|window| window.iter().zip(COMMITTED).all(|(a, b)| a == b))
}

/// Whether `dir` is this repository's committed mainnet deployments
/// directory itself, the same directory on disk.
fn is_the_committed_dir(dir: &Path) -> bool {
    let committed = committed_dir();
    match (resolved(dir), std::fs::canonicalize(&committed)) {
        (Ok(dir), Ok(committed)) => dir.exists() && same_dir(&dir, &committed),
        _ => false,
    }
}

/// Refuse an admin-steps directory that doesn't suit the mode, before
/// anything is sent or written: a fork's steps never go where the hardware
/// key's are kept (`.shonin/mainnet/admin-steps`) or under the committed
/// addresses, and the hardware key's never come from, or go to, the fork's
/// (`.shonin/mainnet-fork`). Each is matched however it is spelled.
pub fn check_admin_steps_dir(mode: Mode, dir: &Path) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let real_steps = root.join(".shonin").join("mainnet").join("admin-steps");
    let fork_root = root.join(".shonin").join("mainnet-fork");
    let given = resolved(dir)?;
    let under = |place: &Path| {
        let place = resolved(place).unwrap_or_else(|_| place.to_path_buf());
        given.ancestors().any(|ancestor| same_dir(ancestor, &place))
    };
    match mode {
        Mode::Fork if under(&real_steps) || is_committed_dir(dir) => bail!(
            "a fork rehearsal writes its admin steps under .shonin/mainnet-fork, never where \
             the hardware key's are kept ({}). Nothing was sent.",
            dir.display()
        ),
        Mode::Send | Mode::DryRun if under(&fork_root) => bail!(
            "without ON_FORK=1 the admin's steps are never written under the fork's \
             .shonin/mainnet-fork ({}). Nothing was sent.",
            dir.display()
        ),
        _ => Ok(()),
    }
}

/// The deployments directory a mode writes to, resolved: a fork rehearsal
/// never the committed mainnet one (however spelled), and a real send only
/// this repository's.
pub fn deployments_dir(mode: Mode, given: &Path) -> Result<PathBuf> {
    match mode {
        Mode::Fork if is_committed_dir(given) => bail!(
            "a fork rehearsal writes under .shonin/mainnet-fork/deployments, never under \
             config/deployments/mainnet ({}). Nothing was sent.",
            given.display()
        ),
        Mode::Send if !is_the_committed_dir(given) => bail!(
            "a real mainnet send writes its addresses under this repository's \
             config/deployments/mainnet, not {}. Nothing was sent.",
            given.display()
        ),
        _ => resolved(given),
    }
}

/// The deployer's key, from the file the owner named: one line, a `0x` hex
/// private key. Neither the key nor the file's text ever appears in an
/// error or a log line.
pub fn read_key_file(path: &Path) -> Result<PrivateKeySigner> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        anyhow!(
            "reading the deployer's key file {}: {}",
            path.display(),
            error.kind()
        )
    })?;
    text.trim().parse::<PrivateKeySigner>().map_err(|_| {
        anyhow!(
            "the deployer's key file {} does not hold one 0x hex private key",
            path.display()
        )
    })
}

/// Anvil's well-known development mnemonic: its accounts' keys are public.
const ANVIL_MNEMONIC: &str = "test test test test test test test test test test test junk";

/// How many of the mnemonic's accounts anvil funds by default.
const ANVIL_ACCOUNTS: usize = 10;

/// A fork rehearsal's deployer from a key file: only one of anvil's own
/// public development keys. A rehearsal signs for the mainnets' chain ids,
/// so a real key's transactions there would be valid on mainnet, ready for
/// whatever answers on loopback to broadcast.
pub fn read_fork_key_file(path: &Path) -> Result<PrivateKeySigner> {
    let key = read_key_file(path)?;
    let anvil = chain::keys_from_mnemonic(ANVIL_MNEMONIC, ANVIL_ACCOUNTS)?;
    if !anvil.iter().any(|known| known.address() == key.address()) {
        bail!(
            "a fork rehearsal signs with a throwaway key (leave DEPLOYER_KEYFILE out) or one of \
             anvil's public development keys, never another: {} holds the key of {}, whose \
             transactions on a fork would be valid on mainnet. Nothing was sent.",
            path.display(),
            key.address()
        );
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> Flags {
        Flags {
            admin: Some(format!("{:?}", Address::repeat_byte(0xad))),
            generation: Some(1),
            ..Default::default()
        }
    }

    const CHAINS: [u64; 3] = [8453, 4663, 999];

    #[test]
    fn without_the_sending_flags_a_mainnet_deploy_is_a_dry_run() {
        let decided = decide(&flags(), &CHAINS).unwrap();
        assert_eq!(decided.mode, Mode::DryRun);
        assert_eq!(decided.admin, Address::repeat_byte(0xad));
        assert_eq!(decided.generation, 1);
    }

    #[test]
    fn without_the_admin_s_address_nothing_runs_on_mainnet() {
        for admin in [None, Some(String::new()), Some("nobody".to_string())] {
            let mut given = flags();
            given.admin = admin;
            given.on_fork = true;
            assert!(decide(&given, &CHAINS).is_err());
        }
        let mut given = flags();
        given.admin = Some(format!("{:?}", Address::ZERO));
        assert!(decide(&given, &CHAINS).is_err());
    }

    #[test]
    fn a_mainnet_deploy_without_an_explicit_generation_is_refused() {
        let mut given = flags();
        given.generation = None;
        let error = decide(&given, &CHAINS).unwrap_err().to_string();
        assert!(error.contains("GENERATION"), "{error}");
    }

    #[test]
    fn a_send_is_refused_unless_the_key_file_and_a_confirmation_naming_exactly_its_chains_are_given()
     {
        let mut given = flags();
        given.confirm_mainnet = Some(confirmation_for(&CHAINS));
        assert!(decide(&given, &CHAINS).is_err(), "no key file");
        let mut given = flags();
        given.deployer_keyfile = Some(PathBuf::from("key"));
        assert!(decide(&given, &CHAINS).is_err(), "no confirmation");
        for wrong in ["yes", "8453", "999,4663", "999,4663,8453,1", "999,4663,x"] {
            given.confirm_mainnet = Some(wrong.to_string());
            assert!(decide(&given, &CHAINS).is_err(), "{wrong}");
        }
        for right in ["999,4663,8453", "8453, 4663, 999"] {
            given.confirm_mainnet = Some(right.to_string());
            assert_eq!(decide(&given, &CHAINS).unwrap().mode, Mode::Send, "{right}");
        }
    }

    #[test]
    fn a_fork_flag_beside_a_mainnet_confirmation_is_refused() {
        let mut given = flags();
        given.on_fork = true;
        assert_eq!(decide(&given, &CHAINS).unwrap().mode, Mode::Fork);
        given.confirm_mainnet = Some(confirmation_for(&CHAINS));
        assert!(decide(&given, &CHAINS).is_err());
    }

    #[test]
    fn a_fork_rehearsal_refuses_any_endpoint_not_on_this_machine() {
        for local in [
            "http://127.0.0.1:8545",
            "http://localhost:8545",
            "http://[::1]:8545",
            "http://127.0.0.1:1 http://localhost:2",
        ] {
            assert!(
                check_endpoints(Mode::Fork, "Base", local).is_ok(),
                "{local}"
            );
        }
        for public in [
            "https://mainnet.base.org",
            "http://127.0.0.1:8545 https://mainnet.base.org",
            "http://127.0.0.1.example.org:8545",
            "http://10.0.0.1:8545",
            "not a url",
        ] {
            let error = check_endpoints(Mode::Fork, "Base", public)
                .unwrap_err()
                .to_string();
            // Never the URL, which may carry a key.
            assert!(!error.contains("base.org"), "{error}");
        }
    }

    #[test]
    fn a_real_send_refuses_a_local_endpoint_and_an_anvil_node() {
        assert!(check_endpoints(Mode::Send, "Base", "https://mainnet.base.org").is_ok());
        assert!(check_endpoints(Mode::Send, "Base", "http://127.0.0.1:8545").is_err());
        assert!(check_node_version(Mode::Send, "Base", "anvil/v1.2.3").is_err());
        assert!(check_node_version(Mode::Send, "Base", "Geth/v1.14").is_ok());
        assert!(check_node_version(Mode::Fork, "Base", "Geth/v1.14").is_err());
        assert!(check_node_version(Mode::Fork, "Base", "anvil/v1.2.3").is_ok());
    }

    #[test]
    fn a_node_answering_as_another_chain_is_refused() {
        assert!(check_chain_id("Base", 8453, 8453).is_ok());
        let error = check_chain_id("Base", 8453, 1).unwrap_err().to_string();
        assert!(error.contains("not 8453"), "{error}");
        assert!(check_chain_id("Hyperliquid", 999, 31337).is_err());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shonin-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fork_rehearsal_never_writes_under_the_committed_mainnet_addresses() {
        let committed = committed_dir();
        std::fs::create_dir_all(&committed).unwrap();
        let dir = scratch("committed");
        let link = dir.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&committed, &link).unwrap();
        let committed_text = committed.to_string_lossy().into_owned();
        let mut spellings = vec![
            committed.clone(),
            committed.join("sub"),
            committed.join("..").join("mainnet"),
            committed
                .join("..")
                .join("local")
                .join("..")
                .join("mainnet"),
            PathBuf::from("config/deployments/mainnet"),
            dir.join("config/deployments/mainnet"),
            dir.join("Config/Deployments/Mainnet"),
        ];
        #[cfg(unix)]
        spellings.extend([link.clone(), link.join("sub")]);
        // The same directory in another letter case, where the disk ignores
        // case.
        let upper = PathBuf::from(committed_text.replace("deployments", "DEPLOYMENTS"));
        if upper.exists() {
            spellings.push(upper);
        }
        for spelling in &spellings {
            assert!(
                deployments_dir(Mode::Fork, spelling).is_err(),
                "{}",
                spelling.display()
            );
        }
        for spelling in [committed.clone(), committed.join("..").join("mainnet")] {
            assert!(
                deployments_dir(Mode::Send, &spelling).is_ok(),
                "{}",
                spelling.display()
            );
        }
        #[cfg(unix)]
        assert!(deployments_dir(Mode::Send, &link).is_ok());

        // Another directory merely named like it is not where a send writes.
        let elsewhere = dir.join("config/deployments/mainnet");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(deployments_dir(Mode::Send, &elsewhere).is_err());
        let rehearsal = dir.join(".shonin/mainnet-fork/deployments");
        assert_eq!(
            deployments_dir(Mode::Fork, &rehearsal).unwrap(),
            resolved(&rehearsal).unwrap()
        );
        assert!(deployments_dir(Mode::Send, &rehearsal).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mainnet_deploy_under_any_generation_but_the_mainnet_one_is_refused() {
        for generation in [0, 2, 6] {
            let mut given = flags();
            given.generation = Some(generation);
            let error = decide(&given, &CHAINS).unwrap_err().to_string();
            assert!(error.contains("GENERATION"), "{error}");
            given.on_fork = true;
            assert!(decide(&given, &CHAINS).is_err());
            assert!(decide_without_generation(&given, &CHAINS).is_err());
        }
        let mut given = flags();
        given.generation = None;
        given.on_fork = true;
        assert_eq!(
            decide_without_generation(&given, &CHAINS).unwrap().0,
            Mode::Fork
        );
    }

    #[test]
    fn without_the_fork_flag_a_local_or_anvil_endpoint_is_refused_even_for_reads() {
        assert!(check_endpoints(Mode::DryRun, "Base", "http://127.0.0.1:8545").is_err());
        assert!(
            check_endpoints(
                Mode::DryRun,
                "Base",
                "https://mainnet.base.org http://localhost:8545"
            )
            .is_err()
        );
        assert!(check_endpoints(Mode::DryRun, "Base", "https://mainnet.base.org").is_ok());
        assert!(check_node_version(Mode::DryRun, "Base", "anvil/v1.2.3").is_err());
        assert!(check_node_version(Mode::DryRun, "Base", "Geth/v1.14").is_ok());
    }

    fn config(rpc: &str, logs: Option<&str>, ws: Option<&str>) -> ChainConfig {
        let mut text = format!("label = \"Base\"\nrpc = \"{rpc}\"\ndollar = \"USDC\"\n");
        if let Some(logs) = logs {
            text.push_str(&format!("logs_rpc = \"{logs}\"\n"));
        }
        if let Some(ws) = ws {
            text.push_str(&format!("ws = \"{ws}\"\n"));
        }
        toml::from_str(&text).unwrap()
    }

    #[test]
    fn every_endpoint_a_chain_names_is_held_to_the_mode_the_log_queries_too() {
        let local = "http://127.0.0.1:8545";
        let public = "https://mainnet.base.org";
        assert!(check_config_endpoints(Mode::Fork, &config(local, Some(local), None)).is_ok());
        assert!(check_config_endpoints(Mode::Fork, &config(local, Some(public), None)).is_err());
        assert!(
            check_config_endpoints(
                Mode::Fork,
                &config(local, None, Some("wss://mainnet.base.org"))
            )
            .is_err()
        );
        assert!(check_config_endpoints(Mode::Send, &config(public, Some(public), None)).is_ok());
        for mode in [Mode::Send, Mode::DryRun] {
            assert!(check_config_endpoints(mode, &config(public, Some(local), None)).is_err());
            assert!(
                check_config_endpoints(mode, &config(public, None, Some("ws://127.0.0.1:8546")))
                    .is_err()
            );
        }
    }

    #[test]
    fn a_fork_rehearsal_signs_with_none_but_anvil_s_public_keys() {
        let dir = scratch("fork-key");
        let anvil = dir.join("anvil");
        let first = &chain::keys_from_mnemonic(ANVIL_MNEMONIC, 1).unwrap()[0];
        assert_eq!(
            first.address(),
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
                .parse::<Address>()
                .unwrap()
        );
        std::fs::write(
            &anvil,
            format!("0x{}\n", alloy::hex::encode(first.to_bytes())),
        )
        .unwrap();
        assert_eq!(
            read_fork_key_file(&anvil).unwrap().address(),
            first.address()
        );
        let real = dir.join("real");
        let key = PrivateKeySigner::random();
        std::fs::write(&real, format!("0x{}\n", alloy::hex::encode(key.to_bytes()))).unwrap();
        let error = read_fork_key_file(&real).unwrap_err().to_string();
        assert!(error.contains("anvil"), "{error}");
        assert!(
            !error.contains(&alloy::hex::encode(key.to_bytes())),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_deployer_s_key_file_is_read_and_never_quoted() {
        let dir = std::env::temp_dir().join(format!("shonin-key-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good");
        let key = PrivateKeySigner::random();
        std::fs::write(&good, format!("0x{}\n", alloy::hex::encode(key.to_bytes()))).unwrap();
        assert_eq!(read_key_file(&good).unwrap().address(), key.address());
        let bad = dir.join("bad");
        std::fs::write(&bad, "secret words that are not a key").unwrap();
        let error = read_key_file(&bad).unwrap_err().to_string();
        assert!(!error.contains("secret"), "{error}");
        assert!(read_key_file(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
