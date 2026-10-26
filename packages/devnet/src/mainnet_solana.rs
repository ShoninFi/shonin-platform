//! Mainnet's Solana deploy: the eight programs that ship, set up and made
//! final, and the admin's steps prepared for the hardware key.
//!
//! What ships is fixed here ([`SHIPPING`]): the registry, publishing, the
//! price list, profiles, the station, the vault, the Confidential Space
//! checker and Orca's driver. No development program has a path onto
//! mainnet: the development checker, the stand-in venues and drivers and the
//! test programs ([`NEVER_ON_MAINNET`]) refuse a mainnet build, and nothing
//! here names them.
//!
//! The commands run in the modes [`crate::mainnet_guard`] decides, with the
//! same flags as the EVM half: without the sending flags a dry run that
//! prints the plan (each program's size, rent and writes, every set-up step
//! and admin step) and sends nothing; with `--on-fork` a rehearsal on a local
//! Surfpool fork of mainnet-beta; with `--deployer-keyfile` and
//! `--confirm-mainnet mainnet-beta` a real send. Every mode needs the admin's
//! address (`--admin`, the hardware key), which is the registry's, the price
//! list's and the vault factory's admin and the platform's fee address from
//! the first instruction.
//!
//! Program ids come from the owner's keypair directory
//! (`--program-keypairs`, one `<library name>-keypair.json` each). A
//! program's id is compiled into it, so the programs and this tool are built
//! for mainnet with those ids ([`ids`] writes the list the build reads) before
//! anything is sent. Each `mainnet-id` beside a program's `Cargo.toml` is
//! written only on a real send, once its program is on the cluster, and an id
//! already committed that differs is refused before anything is sent.
//!
//! The order matters: the checker and Orca's driver go on final (the registry
//! trusts only final checkers and drivers), as does profiles, which has no
//! set-up. The five programs with a set-up go on upgradeable by the
//! deployer, because their set-up instruction checks the deployer is their
//! upgrade authority; once all are set up, each is made final, and every
//! program's upgrade authority is read back as none.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::keys;
use chain::solana::platform::{Platform, program_data_address};
use chain::solana::programs::{
    common, cs_verifier, orca_driver, prices, publishing, registry, station, vault,
};
use chain::solana::{Instruction, Keypair, Pubkey, Signer, SolanaClient};
use chain::{ChainConfig, ChainKey, SolanaDeployments, SolanaRoles};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mainnet_guard::{self, Flags, Mode};
use crate::solana::{CIRCLE_MAINNET_USDC, Settings};

/// The cluster's name, as the registry's mainnet build requires it.
pub const CLUSTER: &str = common::MAINNET_CLUSTER;
/// What `--confirm-mainnet` must say for a real send to Solana.
pub const CONFIRMATION: &str = "mainnet-beta";
/// The chain key mainnet's Solana cluster goes by.
pub const CHAIN_KEY: &str = "solana:mainnet-beta";

/// One program that ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shipping {
    /// Its library name: its keypair is `<name>-keypair.json`, its build
    /// `<name>.so`.
    pub name: &'static str,
    /// Deployed final at once (it has no set-up), rather than upgradeable
    /// until it is set up.
    pub final_at_once: bool,
    pub what: &'static str,
}

/// Every program that ships, in the order they are deployed: those that go
/// on final first (the registry and the stations trust them as they are),
/// then those with a set-up, the largest last.
pub const SHIPPING: [Shipping; 8] = [
    Shipping {
        name: "shonin_cs_verifier",
        final_at_once: true,
        what: "the Confidential Space checker",
    },
    Shipping {
        name: "shonin_orca_driver",
        final_at_once: true,
        what: "Orca's driver: SOL/USDC at tick spacing 4, priced by Pyth's SOL/USD account",
    },
    Shipping {
        name: "shonin_profiles",
        final_at_once: true,
        what: "authors' profiles",
    },
    Shipping {
        name: "shonin_registry",
        final_at_once: false,
        what: "the registry: checkers, images, projects and drivers behind the timelock",
    },
    Shipping {
        name: "shonin_publishing",
        final_at_once: false,
        what: "publishing",
    },
    Shipping {
        name: "shonin_prices",
        final_at_once: false,
        what: "the price list",
    },
    Shipping {
        name: "shonin_station",
        final_at_once: false,
        what: "the stations",
    },
    Shipping {
        name: "shonin_vault",
        final_at_once: false,
        what: "the vaults and their factory",
    },
];

/// The development and test programs, which never reach mainnet: each one's
/// build refuses the `mainnet` feature.
pub const NEVER_ON_MAINNET: [&str; 10] = [
    "shonin_dev_verifier",
    "shonin_local_driver",
    "shonin_local_driver_2",
    "shonin_local_driver_3",
    "shonin_local_venue",
    "shonin_local_venue_2",
    "shonin_local_venue_3",
    "shonin_test_driver",
    "shonin_test_hostile_venue",
    "shonin_test_vault",
];

/// The priority price every transaction is sent with, in micro-lamports a
/// compute unit, as the devnet deploy sends.
pub const COMPUTE_UNIT_PRICE: u64 = 20_000;
/// The bytes of a program each buffer write carries, as the Solana CLI
/// (3.1) sizes them with a priority price set: measured on a fork, where
/// each program took exactly its writes at this size and two more.
pub const WRITE_CHUNK: u64 = 960;
/// What one transaction costs at most: a signature, and the priority price
/// at the default compute limit (the CLI asks for far less on a write).
pub const FEE_PER_TRANSACTION: u64 = 5_000 + COMPUTE_UNIT_PRICE * 200_000 / 1_000_000;
/// Room kept above the plan's peak before a real send starts: the set-up's
/// accounts, the fees, retries.
pub const MARGIN: u64 = LAMPORTS_PER_SOL / 2;
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// A program's data account is its code and a 45-byte header; its buffer
/// during the deploy the code and a 37-byte header; the program account 36
/// bytes.
const PROGRAM_DATA_HEADER: u64 = 45;
const BUFFER_HEADER: u64 = 37;
const PROGRAM_ACCOUNT: u64 = 36;

/// What the mainnet checker trusts: Google's Confidential Space root CA
/// alone, as on the EVM mainnets.
pub fn trusted_keys() -> Vec<[u8; 32]> {
    vec![shonin_attestation_core::confidential_space_root_hash()]
}

/// The guest image the checker pins.
pub fn image_id() -> [u8; 32] {
    shonin_attestation_deploy::GUEST_IMAGE_ID.0
}

/// The checker's state account, which its creation fixes from the image and
/// the keys it trusts.
pub fn checker_state() -> Pubkey {
    let mut keys = trusted_keys();
    keys.sort();
    cs_verifier::checker_address(&image_id(), &keys)
}

// --- The flags ---------------------------------------------------------------

/// What the flags came to on Solana.
#[derive(Clone, Debug)]
pub struct Decided {
    pub mode: Mode,
    pub admin: Pubkey,
}

/// The mode the flags ask for, and the admin. Refuses before anything is
/// read from any cluster: without the admin's address, with a fork flag
/// beside a confirmation, and a send missing either of its flags or
/// confirming anything but `mainnet-beta`. The generation is not asked: no
/// Solana address depends on it.
pub fn decide(flags: &Flags) -> Result<Decided> {
    let admin = flags
        .admin
        .as_deref()
        .map(str::trim)
        .filter(|admin| !admin.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "ADMIN=<address> is required on mainnet: the hardware key's Solana address, admin \
                 of every program from the first instruction. Nothing was sent."
            )
        })?;
    let admin: Pubkey = admin
        .parse()
        .map_err(|_| anyhow!("ADMIN is not a Solana address ({admin}). Nothing was sent."))?;
    if admin == Pubkey::default() {
        bail!("ADMIN is the system program's address. Nothing was sent.");
    }
    if !admin.is_on_curve() {
        bail!(
            "ADMIN ({admin}) is not on the ed25519 curve: no key can ever sign for it (an account \
             or program-derived address, or a mistyped one). The admin is the hardware key's own \
             address. Nothing was sent."
        );
    }
    let confirm = flags
        .confirm_mainnet
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let mode = if flags.on_fork {
        if confirm.is_some() {
            bail!(
                "ON_FORK and CONFIRM_MAINNET together: a rehearsal on a fork never confirms a \
                 mainnet send. Pass one or the other. Nothing was sent."
            );
        }
        // A rehearsal signs with its own throwaway deployer alone: a real
        // key's transactions, signed for whatever answers on loopback, might
        // be valid on mainnet.
        if let Some(file) = &flags.deployer_keyfile {
            bail!(
                "a fork rehearsal signs with its own throwaway deployer, never a key file: leave \
                 DEPLOYER_KEYFILE out ({} was given). Nothing was sent.",
                file.display()
            );
        }
        Mode::Fork
    } else {
        match (&flags.deployer_keyfile, confirm) {
            (None, None) => Mode::DryRun,
            (None, Some(_)) => bail!(
                "sending is refused: CONFIRM_MAINNET is given but no DEPLOYER_KEYFILE=<path> (the \
                 deployer's keypair, from a file the owner holds). Nothing was sent."
            ),
            (Some(_), None) => bail!(
                "sending is refused: DEPLOYER_KEYFILE is given but no CONFIRM_MAINNET={CONFIRMATION}. \
                 Nothing was sent."
            ),
            (Some(_), Some(given)) if given != CONFIRMATION => bail!(
                "sending is refused: CONFIRM_MAINNET={given} does not name the cluster being sent to \
                 ({CONFIRMATION}). Nothing was sent."
            ),
            (Some(_), Some(_)) => Mode::Send,
        }
    };
    Ok(Decided { mode, admin })
}

/// Refuse a node that doesn't suit the mode, from its `getVersion` answer:
/// on a fork it must be Surfpool (which says `surfnet-version`), and
/// otherwise (a real send, or a dry run's reads of mainnet) it must not be.
pub fn check_node_version(mode: Mode, version: &Value) -> Result<()> {
    let surfpool = version.get("surfnet-version").is_some();
    match mode {
        Mode::Fork if !surfpool => bail!(
            "the Solana endpoint answers as {version}, not a local Surfpool fork: ON_FORK sends \
             nothing to a public cluster. Nothing was sent."
        ),
        Mode::Send | Mode::DryRun if surfpool => bail!(
            "the Solana endpoint answers as Surfpool: without ON_FORK=1 a mainnet command never \
             reads from or sends to a fork. Nothing was sent."
        ),
        _ => Ok(()),
    }
}

/// Each shipping program's keypair file in `dir`, with its address. Every
/// one must be there, and no two may be the same key: a missing one is
/// refused, naming every file missing, before anything is sent.
pub fn read_program_keypairs(dir: &Path) -> Result<Vec<(&'static str, Pubkey, PathBuf)>> {
    let missing: Vec<String> = SHIPPING
        .iter()
        .map(|program| dir.join(format!("{}-keypair.json", program.name)))
        .filter(|file| !file.is_file())
        .map(|file| file.display().to_string())
        .collect();
    if !missing.is_empty() {
        bail!(
            "MAINNET_PROGRAM_KEYPAIRS ({}) lacks {} of the eight programs' keypairs: {}. Nothing \
             was sent.",
            dir.display(),
            missing.len(),
            missing.join(", ")
        );
    }
    let mut out = Vec::new();
    for program in SHIPPING {
        let file = dir.join(format!("{}-keypair.json", program.name));
        let id = keys::read_keypair(&file)?.pubkey();
        if let Some((other, _, _)) = out.iter().find(|(_, seen, _)| *seen == id) {
            bail!(
                "{} and {other} have the same keypair ({id}). Nothing was sent.",
                program.name
            );
        }
        out.push((program.name, id, file));
    }
    Ok(out)
}

/// Where a fork rehearsal's own program keypairs are made when none is
/// named: under the fork root.
pub fn throwaway_keypairs_dir(fork_root: &Path) -> PathBuf {
    fork_root.join("solana").join("throwaway-program-keypairs")
}

/// The record of every throwaway program id this tool has made, one
/// `<library name> <id>` a line, under the fork root: a fork takes these
/// alone, and a real send never any of them.
pub fn throwaway_record(fork_root: &Path) -> PathBuf {
    fork_root.join("solana").join("throwaway-program-ids")
}

/// The throwaway ids recorded under `fork_root`.
fn throwaways_made(fork_root: &Path) -> Result<std::collections::BTreeSet<Pubkey>> {
    let record = throwaway_record(fork_root);
    mainnet_guard::refuse_linked_file(&record)?;
    let Ok(text) = std::fs::read_to_string(&record) else {
        return Ok(Default::default());
    };
    text.lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(|id| {
            id.parse::<Pubkey>()
                .map_err(|_| anyhow!("{} holds {id}, not a Solana address", record.display()))
        })
        .collect()
}

/// Make, as throwaways, the program keypairs missing from `dir`, a
/// directory under `fork_root`: each a new key, recorded as made here
/// before its file is created, and its file created here (never over a
/// file or a link already there), readable by its owner alone. A file
/// already there is left as it is, and taken only if this tool made it
/// ([`check_program_keypairs`]).
pub fn make_throwaway_keypairs(dir: &Path, fork_root: &Path) -> Result<()> {
    use std::io::Write;
    if !mainnet_guard::is_under(dir, fork_root) {
        bail!(
            "a fork rehearsal's program keypairs are made under .shonin/mainnet-fork alone, and \
             {} is elsewhere: the owner's keypairs never come onto a fork. Nothing was sent.",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let record = throwaway_record(fork_root);
    for program in SHIPPING {
        let file = dir.join(format!("{}-keypair.json", program.name));
        if std::fs::symlink_metadata(&file).is_ok() {
            continue;
        }
        let pair = Keypair::new();
        mainnet_guard::refuse_linked_file(&record)?;
        if let Some(parent) = record.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&record)
            .and_then(|mut out| writeln!(out, "{} {}", program.name, pair.pubkey()))
            .with_context(|| format!("recording {}", record.display()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
        options
            .open(&file)
            .and_then(|mut out| out.write_all(format!("[{}]", bytes.join(",")).as_bytes()))
            .with_context(|| format!("making {}", file.display()))?;
    }
    Ok(())
}

/// Refuse program keypairs that don't suit the mode, before anything is
/// built or sent, file by file. A fork rehearsal takes the throwaways this
/// tool made alone ([`make_throwaway_keypairs`]): each a regular file of
/// its own (no symlink, no other name on disk), under this repository's
/// `.shonin/mainnet-fork`, whose id the tool recorded as made there, and
/// none holding the keypair of a committed `mainnet-id`, which is mainnet's
/// own program. Anything else (the owner's keypairs, linked or copied in)
/// is refused. A real send never takes one of those throwaways, however its
/// file is reached.
pub fn check_program_keypairs(
    mode: Mode,
    dir: &Path,
    ids: &[(&str, Pubkey)],
    sources: &Path,
    fork_root: &Path,
) -> Result<()> {
    let under_fork = mainnet_guard::is_under(dir, fork_root);
    let made = throwaways_made(fork_root)?;
    let file_of = |name: &str| dir.join(format!("{name}-keypair.json"));
    match mode {
        Mode::Fork => {
            if !under_fork {
                bail!(
                    "a fork rehearsal takes throwaway program keypairs from under \
                     .shonin/mainnet-fork alone (the tool makes them when \
                     MAINNET_PROGRAM_KEYPAIRS is left out), and {} is elsewhere: the owner's \
                     keypairs never come onto a fork. Nothing was sent.",
                    dir.display()
                );
            }
            for (name, id) in ids {
                let file = file_of(name);
                mainnet_guard::refuse_linked_file(&file).map_err(|error| {
                    anyhow!(
                        "{error:#} A fork rehearsal takes the keypairs this tool made alone: \
                         the owner's keypairs never come onto a fork. Nothing was sent."
                    )
                })?;
                let real = std::fs::canonicalize(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                if !mainnet_guard::is_under(&real, fork_root) || !made.contains(id) {
                    bail!(
                        "{} ({id}) is not a throwaway this tool made: a fork rehearsal takes \
                         those alone, and the owner's keypairs never come onto a fork. Remove \
                         it and run the rehearsal again (the tool makes its own). Nothing was \
                         sent.",
                        file.display()
                    );
                }
            }
            for program in SHIPPING {
                let committed = committed_id(sources, program.name)?;
                if let Some((name, id)) = ids
                    .iter()
                    .find(|(_, id)| committed.is_some_and(|held| held == *id))
                {
                    bail!(
                        "{} holds the keypair of {name} ({id}), committed as {}'s mainnet id: \
                         that is mainnet's program, never a fork's. Use throwaway keypairs. \
                         Nothing was sent.",
                        dir.display(),
                        program.name
                    );
                }
            }
        }
        Mode::Send if under_fork => bail!(
            "{} is under .shonin/mainnet-fork, where a rehearsal's throwaway keypairs are: a real \
             send takes the owner's. Nothing was sent.",
            dir.display()
        ),
        Mode::Send => {
            for (name, id) in ids {
                let file = file_of(name);
                let real = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
                if made.contains(id) || mainnet_guard::is_under(&real, fork_root) {
                    bail!(
                        "{} ({id}) is one of a fork rehearsal's throwaways, however it is \
                         reached: a real send takes the owner's. Nothing was sent.",
                        file.display()
                    );
                }
            }
        }
        Mode::DryRun => {}
    }
    Ok(())
}

/// This repository's own `contracts/solana/programs`, the one place each
/// program's `mainnet-id` is committed: every mode reads it, and a real
/// send writes it, there alone.
pub fn check_sources(sources: &Path) -> Result<()> {
    if !same_place(sources, &committed_sources()) {
        bail!(
            "each program's mainnet-id is read from, and written to, this repository's \
             contracts/solana/programs alone, not {}. Nothing was sent.",
            sources.display()
        );
    }
    Ok(())
}

/// A program's `mainnet-id` file, beside its `Cargo.toml` under `sources`
/// (`contracts/solana/programs`).
pub fn committed_file(sources: &Path, program: &str) -> PathBuf {
    sources.join(program.replace('_', "-")).join("mainnet-id")
}

/// Whether `id` agrees with what is committed for the program in `text`
/// (the file's contents, if any): `Ok(true)` if it is committed already,
/// `Ok(false)` if nothing is, and refused if another id is.
pub fn agrees_with_committed(program: &str, id: &Pubkey, text: Option<&str>) -> Result<bool> {
    match text.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(false),
        Some(held) if held == id.to_string() => Ok(true),
        Some(held) => bail!(
            "{program}'s committed mainnet id is {held}, and its keypair is {id}: a committed \
             mainnet id is never replaced. Nothing was sent."
        ),
    }
}

/// The list of ids a mainnet build reads (`SHONIN_MAINNET_IDS`).
pub fn ids_list(ids: &[(&str, Pubkey)]) -> String {
    let mut text =
        String::from("# Mainnet program ids, by library name, for a --features mainnet build.\n");
    for (name, id) in ids {
        text.push_str(&format!("{name} {id}\n"));
    }
    text
}

/// The id committed for a program in `sources`, if any.
pub fn committed_id(sources: &Path, program: &str) -> Result<Option<Pubkey>> {
    let file = committed_file(sources, program);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Ok(None);
    };
    match text.trim() {
        "" => Ok(None),
        held => held.parse().map(Some).map_err(|_| {
            anyhow!(
                "{} is not a Solana address. Nothing was sent.",
                file.display()
            )
        }),
    }
}

/// `mainnet-solana-ids`: the program ids the mainnet build is made with,
/// written to `out` as the list `SHONIN_MAINNET_IDS` names. From the owner's
/// keypairs (required to send, on a fork or for real), each checked against
/// any committed `mainnet-id`; for a dry run without them, each program's
/// committed id where there is one (after the deploy, every one), and a
/// stand-in keypair made under `stand_ins` for the rest, since a program's
/// size doesn't depend on its id.
pub fn ids(
    flags: &Flags,
    program_keypairs: Option<&Path>,
    sources: &Path,
    stand_ins: &Path,
    out: &Path,
) -> Result<()> {
    let decided = decide(flags)?;
    check_sources(sources)?;
    ids_from(
        decided.mode,
        program_keypairs,
        sources,
        stand_ins,
        out,
        &mainnet_guard::fork_root(),
    )
}

/// [`ids`] in `mode`, with the committed ids read from `sources` and a
/// fork's throwaways under `fork_root`. On a fork the throwaways are made
/// here, in the directory named or else the default one, never taken from
/// anywhere else.
fn ids_from(
    mode: Mode,
    program_keypairs: Option<&Path>,
    sources: &Path,
    stand_ins: &Path,
    out: &Path,
    fork_root: &Path,
) -> Result<()> {
    let default_throwaways = throwaway_keypairs_dir(fork_root);
    let program_keypairs = match (mode, program_keypairs) {
        (Mode::Fork, given) => {
            let dir = given.unwrap_or(&default_throwaways);
            make_throwaway_keypairs(dir, fork_root)?;
            println!("(the fork's throwaway program keypairs: {})", dir.display());
            Some(dir)
        }
        (_, given) => given,
    };
    let ids: Vec<(&str, Pubkey)> = match (mode, program_keypairs) {
        (_, Some(dir)) => {
            let pairs = read_program_keypairs(dir)?;
            let found: Vec<(&str, Pubkey)> =
                pairs.iter().map(|(name, id, _)| (*name, *id)).collect();
            check_program_keypairs(mode, dir, &found, sources, fork_root)?;
            for (name, id, _) in &pairs {
                let text = std::fs::read_to_string(committed_file(sources, name)).ok();
                if agrees_with_committed(name, id, text.as_deref())? {
                    println!("{name} {id} (committed)");
                } else {
                    println!("{name} {id}");
                }
            }
            pairs.iter().map(|(name, id, _)| (*name, *id)).collect()
        }
        (Mode::DryRun, None) => {
            let mut ids = Vec::new();
            let mut stand_in = Vec::new();
            for program in SHIPPING {
                match committed_id(sources, program.name)? {
                    Some(id) => {
                        println!("{} {id} (committed)", program.name);
                        ids.push((program.name, id));
                    }
                    None => {
                        let file = stand_ins.join(format!("{}-keypair.json", program.name));
                        if !file.is_file() {
                            keys::write_keypair(&Keypair::new(), &file)?;
                        }
                        let id = keys::read_keypair(&file)?.pubkey();
                        stand_in.push(program.name);
                        ids.push((program.name, id));
                    }
                }
            }
            if !stand_in.is_empty() {
                println!(
                    "No MAINNET_PROGRAM_KEYPAIRS: the dry run builds {} with stand-in ids (under \
                     {}); a program's size doesn't depend on its id.",
                    stand_in.join(", "),
                    stand_ins.display()
                );
            }
            ids
        }
        (_, None) => bail!(
            "MAINNET_PROGRAM_KEYPAIRS=<dir> is required to send: the owner's keypair for each \
             program, <library name>-keypair.json. Nothing was sent."
        ),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    mainnet_guard::write_file(out, ids_list(&ids))?;
    Ok(())
}

// --- The plan ------------------------------------------------------------------

/// One program in the plan.
#[derive(Clone, Debug)]
pub struct Planned {
    pub program: Shipping,
    pub id: Pubkey,
    pub so: PathBuf,
    pub size: u64,
    /// Rent of the program data account and the program account, kept for
    /// good once the program is final.
    pub rent: u64,
    /// Rent of the buffer during its deploy, returned when it is deployed.
    pub buffer_rent: u64,
    pub writes: u64,
    /// What is at the id now.
    pub found: Found,
}

/// What the cluster holds at a program's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Nothing,
    Upgradeable(Pubkey),
    Final,
}

/// The whole deploy's cost, from the programs not yet on the cluster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    pub kept: u64,
    /// The most out at once: what is kept by the programs already deployed
    /// plus the buffer of the one in hand.
    pub peak: u64,
    pub transactions: u64,
    pub fees: u64,
}

/// The cost of deploying, in order, the programs not yet there.
pub fn cost(planned: &[Planned]) -> Cost {
    let mut cost = Cost::default();
    for p in planned.iter().filter(|p| p.found == Found::Nothing) {
        cost.peak = cost.peak.max(cost.kept + p.rent + p.buffer_rent);
        cost.kept += p.rent;
        // A buffer, its writes, the deploy (with the authority set at once
        // when final).
        cost.transactions += p.writes + 2;
    }
    // The set-up and the finalising.
    cost.transactions += setup_steps().len() as u64 + finalised().count() as u64;
    cost.fees = cost.transactions * FEE_PER_TRANSACTION;
    cost
}

/// The programs made final once set up.
fn finalised() -> impl Iterator<Item = &'static Shipping> {
    SHIPPING.iter().filter(|program| !program.final_at_once)
}

/// The deployer's set-up steps after the programs are on, in order.
pub fn setup_steps() -> Vec<String> {
    let settings = Settings::mainnet(None).expect("mainnet settings");
    vec![
        format!(
            "create the Confidential Space checker's account {} (image {}, trusting Google's root CA alone)",
            checker_state(),
            hex(&image_id())
        ),
        format!(
            "registry.initialize: admin ADMIN, cluster {CLUSTER}, timelock {} s, floor {} s, blessing {} s, first and only checker the Confidential Space checker",
            settings.timelock, settings.min_timelock, settings.blessing_ttl
        ),
        format!(
            "publishing.initialize: dollar USDC {CIRCLE_MAINNET_USDC}, fee {} (a dollar) to ADMIN",
            crate::solana::MAINNET_PUBLISH_FEE
        ),
        format!(
            "prices.initialize: admin ADMIN, notice {} s (thirty days), the published prices",
            settings.price_notice
        ),
        "station.initialize: the vault program's vaults create stations, dollar USDC".to_string(),
        "vault.initialize: admin ADMIN, platform ADMIN, dollar USDC, the mainnet terms".to_string(),
    ]
}

fn hex(bytes: &[u8]) -> String {
    alloy::hex::encode(bytes)
}

fn sol(lamports: u64) -> String {
    format!("{:.4}", lamports as f64 / LAMPORTS_PER_SOL as f64)
}

/// To the lamport, for what was measured.
fn sol9(lamports: u64) -> String {
    format!("{:.9}", lamports as f64 / LAMPORTS_PER_SOL as f64)
}

/// Whether `haystack` holds `needle`'s bytes anywhere.
fn embeds(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Orca's SOL/USDC pool at tick spacing 4 on mainnet, as the page lists it.
const MAINNET_ORCA_POOL: &str = "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE";

/// Refuse a program's build that isn't the mainnet build for its id: an
/// Anchor program holds its declared id's 32 bytes, so a build made for
/// another id (a local or devnet build) doesn't; and Orca's driver's
/// mainnet build lists mainnet's SOL/USDC pool.
pub fn check_build(name: &str, id: &Pubkey, code: &[u8]) -> Result<()> {
    if !embeds(code, id.as_ref()) {
        bail!(
            "{name}.so was not built for {id}: build the programs for mainnet with these ids (the \
             task does). Nothing was sent."
        );
    }
    if name == "shonin_orca_driver" {
        let pool: Pubkey = MAINNET_ORCA_POOL.parse().expect("a valid address");
        if !embeds(code, pool.as_ref()) {
            bail!(
                "{name}.so doesn't list mainnet's SOL/USDC pool {pool}: build it with --features \
                 mainnet. Nothing was sent."
            );
        }
    }
    Ok(())
}

// --- The verifiable build ------------------------------------------------------

/// The image the programs' verifiable build runs in (`solana-verify build`),
/// pinned by digest: `SOLANA_VERIFY_IMAGE` in `taskfiles/config.yml`, which a
/// test holds to this.
pub const VERIFIABLE_IMAGE: &str = "solanafoundation/solana-verifiable-build@sha256:1a0ea3dd15017e57e9b17d67267e7d2d7ab4f083499f3f29c847ddd54f01db31";

/// The record the verifiable build writes beside the programs it built:
/// what it was built from, in which image, and each program's SHA-256.
pub const VERIFIABLE_RECORD: &str = "verifiable-build";

/// What the source of a verifiable build was.
pub const WORKING_TREE: &str = "working-tree";

/// A verifiable build's record, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiableBuild {
    /// The commit exported, or [`WORKING_TREE`] for a rehearsal of
    /// uncommitted changes.
    pub source: String,
    pub image: String,
    /// Each program's SHA-256, hex, by library name.
    pub hashes: BTreeMap<String, String>,
}

/// What a build must have been made from to be deployed or read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildSource {
    /// A rehearsal or a dry run: a commit or the working tree.
    Any,
    /// A read-back of mainnet: a commit (the mainnet ids are committed
    /// after it, so not necessarily the one checked out now).
    ACommit,
    /// A real send: the commit checked out, with nothing uncommitted under
    /// the programs' inputs.
    Head { head: String, clean: bool },
}

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read a verifiable build's record: `source <commit|working-tree>`,
/// `image <reference>`, then `<library name> <sha256>` a program.
pub fn parse_verifiable_record(text: &str) -> Result<VerifiableBuild> {
    let mut source = None;
    let mut image = None;
    let mut hashes = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(char::is_whitespace)
            .map(|(key, value)| (key, value.trim()))
            .ok_or_else(|| {
                anyhow!("the verifiable build's record has a line without a value: {line}")
            })?;
        match key {
            "source" => source = Some(value.to_string()),
            "image" => image = Some(value.to_string()),
            name => {
                hashes.insert(name.to_string(), value.to_lowercase());
            }
        }
    }
    Ok(VerifiableBuild {
        source: source.ok_or_else(|| anyhow!("the verifiable build's record names no source"))?,
        image: image.ok_or_else(|| anyhow!("the verifiable build's record names no image"))?,
        hashes,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

/// Refuse programs in `programs_dir` that aren't the verifiable build, before
/// anything is sent: the build's record must be there, made in the pinned
/// image, list the eight programs and nothing else, and each `<name>.so` must
/// be the file it recorded (a host `cargo build-sbf` put in its place is
/// refused: no verifier can reproduce it). `source` says what it must have
/// been built from.
/// Where a deploy holds the programs it checked.
pub const HELD_PROGRAMS: &str = "checked-programs";

/// Copy each program of the verifiable build `build` from `programs_dir`
/// into a directory of the deploy's own under `state_dir`, made afresh
/// (readable by its owner alone), each file created here read-only, and
/// only once its bytes are read and hashed as the record's: what is planned
/// and deployed from then on is these bytes, whatever happens to the build
/// directory meanwhile. Returns the directory.
pub fn hold_checked_build(
    programs_dir: &Path,
    build: &VerifiableBuild,
    state_dir: &Path,
) -> Result<PathBuf> {
    use std::io::Write;
    let held = state_dir.join(HELD_PROGRAMS);
    if std::fs::symlink_metadata(&held).is_ok() {
        std::fs::remove_dir_all(&held).with_context(|| format!("emptying {}", held.display()))?;
    }
    std::fs::create_dir_all(state_dir)?;
    let mut make = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        make.mode(0o700);
    }
    make.create(&held)
        .with_context(|| format!("making {}", held.display()))?;
    for program in SHIPPING {
        let name = format!("{}.so", program.name);
        let code = std::fs::read(programs_dir.join(&name))
            .with_context(|| format!("reading {}", programs_dir.join(&name).display()))?;
        if build.hashes.get(program.name).map(String::as_str) != Some(sha256_hex(&code).as_str()) {
            bail!(
                "{} changed after it was checked: it is no longer the file the verifiable build \
                 made. Nothing was sent.",
                programs_dir.join(&name).display()
            );
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o400);
        }
        options
            .open(held.join(&name))
            .and_then(|mut out| out.write_all(&code))
            .with_context(|| format!("holding {}", held.join(&name).display()))?;
    }
    Ok(held)
}

pub fn check_verifiable_build(
    programs_dir: &Path,
    source: &BuildSource,
) -> Result<VerifiableBuild> {
    let record = programs_dir.join(VERIFIABLE_RECORD);
    let text = std::fs::read_to_string(&record).map_err(|_| {
        anyhow!(
            "{} is missing: the programs are not the verifiable build (solana-verify build in the \
             pinned image), which is the only build mainnet is given. Build them with the task. \
             Nothing was sent.",
            record.display()
        )
    })?;
    let build = parse_verifiable_record(&text)?;
    if build.image != VERIFIABLE_IMAGE {
        bail!(
            "the programs were built in {}, not the pinned image {VERIFIABLE_IMAGE}: a verifier \
             rebuilds in the pinned one. Nothing was sent.",
            build.image
        );
    }
    let mut names: Vec<&str> = build.hashes.keys().map(String::as_str).collect();
    let mut expected: Vec<&str> = SHIPPING.iter().map(|program| program.name).collect();
    names.sort_unstable();
    expected.sort_unstable();
    if names != expected {
        bail!(
            "the verifiable build records {}, not the eight programs that ship. Nothing was sent.",
            names.join(", ")
        );
    }
    for program in SHIPPING {
        let so = programs_dir.join(format!("{}.so", program.name));
        let code = std::fs::read(&so)
            .with_context(|| format!("{} is missing: build the programs first", so.display()))?;
        if sha256_hex(&code) != build.hashes[program.name] {
            bail!(
                "{} is not the file the verifiable build made (its SHA-256 differs from the \
                 record's): only the verifiable build is deployed. Nothing was sent.",
                so.display()
            );
        }
    }
    match source {
        BuildSource::Any => {}
        BuildSource::ACommit if !is_commit(&build.source) => bail!(
            "the programs were built from {}, not a commit: what mainnet runs is a commit's \
             verifiable build.",
            build.source
        ),
        BuildSource::ACommit => {}
        BuildSource::Head { head, clean } => {
            if !is_commit(&build.source) {
                bail!(
                    "the programs were built from {}, not a commit: a real send deploys a \
                     commit's verifiable build alone. Commit the changes and build again. \
                     Nothing was sent.",
                    build.source
                );
            }
            if !build.source.eq_ignore_ascii_case(head) {
                bail!(
                    "the programs were built from commit {}, and {head} is checked out: build \
                     again from the commit being deployed. Nothing was sent.",
                    build.source
                );
            }
            if !clean {
                bail!(
                    "contracts/solana, packages, Cargo.toml or Cargo.lock has uncommitted \
                     changes: a real send deploys a commit's verifiable build alone. Nothing was \
                     sent."
                );
            }
        }
    }
    Ok(build)
}

/// This repository's checked-out commit, and whether anything the programs
/// are built from is uncommitted.
fn repository_state() -> Result<BuildSource> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let git = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!("git {} failed", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let head = git(&["rev-parse", "HEAD"])?;
    let changes = git(&[
        "status",
        "--porcelain",
        "--",
        "contracts/solana",
        "packages",
        "Cargo.toml",
        "Cargo.lock",
    ])?;
    Ok(BuildSource::Head {
        head,
        clean: changes.is_empty(),
    })
}

/// What a mode requires the deployed build to have been made from.
fn build_source_for(mode: Mode) -> Result<BuildSource> {
    match mode {
        Mode::Send => repository_state(),
        Mode::Fork | Mode::DryRun => Ok(BuildSource::Any),
    }
}

/// Read each program's build and what the cluster holds at its id, with its
/// rent at the cluster's rate.
async fn plan(
    client: &SolanaClient,
    ids: &BTreeMap<&'static str, Pubkey>,
    programs_dir: &Path,
) -> Result<Vec<Planned>> {
    let mut planned = Vec::new();
    for program in SHIPPING {
        let id = ids[program.name];
        let so = programs_dir.join(format!("{}.so", program.name));
        let code = std::fs::read(&so).with_context(|| {
            format!(
                "{} is missing: build the programs for mainnet first",
                so.display()
            )
        })?;
        check_build(program.name, &id, &code)?;
        let size = code.len() as u64;
        let rent = client
            .rpc
            .rent_exempt_minimum((size + PROGRAM_DATA_HEADER) as usize)
            .await?
            + client
                .rpc
                .rent_exempt_minimum(PROGRAM_ACCOUNT as usize)
                .await?;
        let buffer_rent = client
            .rpc
            .rent_exempt_minimum((size + BUFFER_HEADER) as usize)
            .await?;
        let found = match crate::solana::upgrade_authority(client, &id).await? {
            None => Found::Nothing,
            Some(None) => Found::Final,
            Some(Some(authority)) => Found::Upgradeable(authority),
        };
        planned.push(Planned {
            program,
            id,
            so,
            size,
            rent,
            buffer_rent,
            writes: size.div_ceil(WRITE_CHUNK),
            found,
        });
    }
    Ok(planned)
}

fn print_plan(planned: &[Planned], admin: &Pubkey, admin_steps: &[AdminStep], stand_in_ids: bool) {
    println!("Solana mainnet-beta: the programs that ship, in order");
    if stand_in_ids {
        println!("  (ids are stand-ins: the real ids are the owner's keypairs')");
    }
    println!(
        "  {:<20} {:<45} {:>10} {:>9} {:>9} {:>7}  on the cluster",
        "program", "id", "bytes", "kept SOL", "buffer", "writes"
    );
    for p in planned {
        println!(
            "  {:<20} {:<45} {:>10} {:>9} {:>9} {:>7}  {}{}",
            p.program.name,
            p.id.to_string(),
            p.size,
            sol(p.rent),
            sol(p.buffer_rent),
            p.writes,
            match p.found {
                Found::Nothing => "nothing yet".to_string(),
                Found::Final => "final".to_string(),
                Found::Upgradeable(by) => format!("upgradeable by {by}"),
            },
            if p.program.final_at_once {
                ", deployed final"
            } else {
                ", final after its set-up"
            }
        );
    }
    let all = cost(
        &planned
            .iter()
            .cloned()
            .map(|mut p| {
                p.found = Found::Nothing;
                p
            })
            .collect::<Vec<_>>(),
    );
    let left = cost(planned);
    let size: u64 = planned.iter().map(|p| p.size).sum();
    println!(
        "  all eight: {size} bytes; rent kept for good {} SOL; peak with the buffer in hand {} SOL; about {} transactions, fees about {} SOL",
        sol(all.kept),
        sol(all.peak),
        all.transactions,
        sol(all.fees)
    );
    if left != all {
        println!(
            "  still to deploy: rent {} SOL, peak {} SOL, about {} transactions",
            sol(left.kept),
            sol(left.peak),
            left.transactions
        );
    }
    println!("Then, signed by the deployer:");
    for step in setup_steps() {
        println!("  - {step}");
    }
    println!(
        "  - make {} final, and read every program's upgrade authority back as none",
        finalised().map(|p| p.name).collect::<Vec<_>>().join(", ")
    );
    println!("Then the admin's steps, signed by ADMIN ({admin}) with the hardware key:");
    for step in admin_steps {
        println!("  - {}", step.purpose);
    }
}

// --- The admin's steps -----------------------------------------------------------

/// One step the admin signs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminStep {
    pub name: String,
    pub purpose: String,
    pub program: String,
    pub accounts: Vec<StepAccount>,
    /// The instruction's data, hex.
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepAccount {
    pub pubkey: String,
    pub signer: bool,
    pub writable: bool,
}

/// The file of the admin's steps.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminSteps {
    pub cluster: String,
    pub admin: String,
    pub steps: Vec<AdminStep>,
}

impl AdminStep {
    fn of(name: &str, purpose: String, ix: &Instruction) -> Self {
        Self {
            name: name.to_string(),
            purpose,
            program: ix.program_id.to_string(),
            accounts: ix
                .accounts
                .iter()
                .map(|meta| StepAccount {
                    pubkey: meta.pubkey.to_string(),
                    signer: meta.is_signer,
                    writable: meta.is_writable,
                })
                .collect(),
            data: hex(&ix.data),
        }
    }
}

/// The platform as the instructions see it on mainnet: the admin is also
/// the platform's fee address; no operator, no development checker.
pub fn platform(admin: Pubkey) -> Platform {
    Platform {
        cluster_name: CLUSTER.to_string(),
        cluster: common::digest::cluster_value(CLUSTER),
        mint: CIRCLE_MAINNET_USDC,
        admin,
        platform: admin,
        operator: Pubkey::default(),
        checker: None,
    }
}

/// What the admin still has to sign, from what the registry holds: Orca's
/// driver proposed, and the operator's project once it exists. Each comes
/// with its instruction.
async fn admin_instructions(
    client: &SolanaClient,
    admin: Pubkey,
    project: Option<[u8; 32]>,
    project_name: Option<&str>,
) -> Result<Vec<(AdminStep, Instruction)>> {
    let platform = platform(admin);
    let mut steps = Vec::new();
    let slot =
        registry::read::approval_address(registry::state::ApprovalKind::Driver, &orca_driver::NAME);
    let now = client.now().await?;
    let proposed = client
        .load::<registry::state::ApprovalSlot>(&slot)
        .await?
        .is_some_and(|slot| slot.effective(now) == orca_driver::ID || slot.next == orca_driver::ID);
    if !proposed {
        let ix = platform
            .ix_propose(
                client,
                registry::state::ApprovalKind::Driver,
                orca_driver::NAME,
                orca_driver::ID,
            )
            .await?;
        steps.push((
            AdminStep::of(
                "propose-driver-orca",
                format!(
                    "registry.propose: Orca's driver {} under the name \"orca\"; it counts after the two-day timelock",
                    orca_driver::ID
                ),
                &ix,
            ),
            ix,
        ));
    }
    if let Some(project) = project
        && client
            .account(&registry::read::project_address(&project))
            .await?
            .is_none()
    {
        let ix = platform.ix_propose_project(project);
        steps.push((
            AdminStep::of(
                "propose-project",
                format!(
                    "registry.propose_project: the operator's Google Cloud project {}; it counts after the timelock",
                    project_name.unwrap_or("(given)")
                ),
                &ix,
            ),
            ix,
        ));
    }
    Ok(steps)
}

/// The admin's steps for the plan, before anything is on the cluster: the
/// same instructions, built without reading the registry.
fn planned_admin_steps(admin: Pubkey, project: Option<[u8; 32]>) -> Vec<AdminStep> {
    let platform = platform(admin);
    let mut steps = vec![AdminStep {
        name: "propose-driver-orca".to_string(),
        purpose: format!(
            "registry.propose: Orca's driver {} under the name \"orca\"; it counts after the two-day timelock",
            orca_driver::ID
        ),
        program: registry::ID.to_string(),
        accounts: Vec::new(),
        data: String::new(),
    }];
    if let Some(project) = project {
        let ix = platform.ix_propose_project(project);
        steps.push(AdminStep::of(
            "propose-project",
            "registry.propose_project: the operator's Google Cloud project; it counts after the timelock".to_string(),
            &ix,
        ));
    }
    steps
}

fn admin_steps_file(dir: &Path) -> PathBuf {
    dir.join("admin-steps-solana-mainnet-beta.json")
}

fn write_admin_steps(dir: &Path, admin: &Pubkey, steps: &[AdminStep]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let file = admin_steps_file(dir);
    let written = AdminSteps {
        cluster: CLUSTER.to_string(),
        admin: admin.to_string(),
        steps: steps.to_vec(),
    };
    mainnet_guard::write_file(&file, serde_json::to_string_pretty(&written)?)?;
    Ok(file)
}

// --- Connecting ----------------------------------------------------------------------

/// Connect to the cluster for `mode`: its endpoints suit the mode (a fork's
/// on this machine, a real send's not), its genesis is mainnet-beta's, and
/// the node is Surfpool on a fork and not on a real send.
/// Check the cluster a read-only command reads, as [`connect`] checks it:
/// every endpoint suits the mode (on a fork local and Surfpool, otherwise
/// public and no fork) and serves mainnet-beta's genesis.
pub async fn check_cluster(mode: Mode, key: &ChainKey, config: &ChainConfig) -> Result<()> {
    connect(mode, key, config).await.map(|_| ())
}

async fn connect(mode: Mode, key: &ChainKey, config: &ChainConfig) -> Result<SolanaClient> {
    if key.to_string() != CHAIN_KEY {
        bail!("the mainnet Solana commands run on {CHAIN_KEY}, not {key}");
    }
    mainnet_guard::check_config_endpoints(mode, config)?;
    // Checks the genesis is mainnet-beta's, as the key names it.
    let client = SolanaClient::connect_with(key, config, None)
        .await
        .with_context(|| format!("connecting to {}", config.label))?;
    check_nodes(mode, config, &client.genesis_hash).await?;
    Ok(client)
}

/// Ask every endpoint in the request list, one at a time and not through
/// the failover, what it is and which cluster it serves: each must suit the
/// mode and serve `genesis` (mainnet-beta's, as the client checked through
/// the first). An endpoint that doesn't answer is refused; no URL appears
/// in any error.
async fn check_nodes(mode: Mode, config: &ChainConfig, genesis: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let urls: Vec<&str> = config.rpc.split_whitespace().collect();
    for (index, url) in urls.iter().enumerate() {
        let label = format!(
            "{}'s endpoint {} of {}",
            config.label,
            index + 1,
            urls.len()
        );
        let ask = |method: &'static str| {
            let http = http.clone();
            let label = label.clone();
            async move {
                let answer: Value = http
                    .post(*url)
                    .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": []}))
                    .send()
                    .await
                    .and_then(|response| response.error_for_status())
                    .map_err(|_| anyhow!("{label} didn't answer {method}. Nothing was sent."))?
                    .json()
                    .await
                    .map_err(|_| anyhow!("{label} didn't answer {method}. Nothing was sent."))?;
                Ok::<Value, anyhow::Error>(answer["result"].clone())
            }
        };
        check_node_version(mode, &ask("getVersion").await?).with_context(|| label.clone())?;
        let answered = ask("getGenesisHash").await?;
        if answered.as_str() != Some(genesis) {
            bail!("{label} serves another cluster (genesis {answered}). Nothing was sent.");
        }
    }
    Ok(())
}

/// The outside accounts the programs are built on, as this build lists them,
/// each checked on the cluster: refuses a build that isn't mainnet's.
async fn check_outside(client: &SolanaClient) -> Result<Vec<String>> {
    use chain::solana::orca;
    let mut said = Vec::new();
    if orca::DOLLAR_MINT != CIRCLE_MAINNET_USDC {
        bail!(
            "this build's Orca driver lists the dollar {}, not Circle's USDC: build with --features mainnet",
            orca::DOLLAR_MINT
        );
    }
    let token = chain::solana::programs::anchor_spl::token::ID;
    let usdc = client
        .account(&CIRCLE_MAINNET_USDC)
        .await?
        .ok_or_else(|| anyhow!("Circle's USDC mint is not on the cluster"))?;
    if usdc.owner != token || usdc.data.get(44) != Some(&6) {
        bail!("{CIRCLE_MAINNET_USDC} is not a Token-program mint of 6 decimals");
    }
    said.push(format!(
        "USDC {CIRCLE_MAINNET_USDC}: the Token program's, 6 decimals"
    ));
    let whirlpools = client
        .account(&orca::WHIRLPOOL_PROGRAM)
        .await?
        .filter(|account| account.executable)
        .ok_or_else(|| {
            anyhow!(
                "Orca's program {} is not on the cluster",
                orca::WHIRLPOOL_PROGRAM
            )
        })?;
    drop(whirlpools);
    for (what, address, owner) in [
        ("Orca's settings", orca::CONFIG, orca::WHIRLPOOL_PROGRAM),
        (
            "Orca's SOL/USDC pool",
            orca::POOLS[0].address,
            orca::WHIRLPOOL_PROGRAM,
        ),
        (
            "Pyth's SOL/USD account",
            orca::PRICE_ACCOUNT,
            orca::PYTH_RECEIVER,
        ),
    ] {
        let account = client
            .account(&address)
            .await?
            .ok_or_else(|| anyhow!("{what} {address} is not on the cluster"))?;
        if account.owner != owner {
            bail!(
                "{what} {address} is owned by {}, not {owner}",
                account.owner
            );
        }
        said.push(format!("{what} {address}: owned by {owner}"));
    }
    if orca::POOLS[0].tick_spacing != 4 {
        bail!(
            "this build lists Orca's pool at tick spacing {}, not 4",
            orca::POOLS[0].tick_spacing
        );
    }
    Ok(said)
}

/// The ids this build compiled in for the programs that ship.
fn compiled_ids() -> BTreeMap<&'static str, Pubkey> {
    let compiled = chain::solana::programs::compiled();
    SHIPPING
        .iter()
        .map(|program| {
            let id = compiled
                .iter()
                .find(|(name, _)| *name == program.name)
                .map(|(_, id)| *id)
                .expect("every shipping program is compiled in");
            (program.name, id)
        })
        .collect()
}

// --- The Solana CLI ----------------------------------------------------------------------

/// Write `text` to `file`, readable by its owner alone from the start.
fn write_private(file: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(file);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options
        .open(file)
        .with_context(|| format!("writing {}", file.display()))?;
    out.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", file.display()))?;
    Ok(())
}

/// Every form in which the endpoint `rpc` may be printed, longest first: as
/// given, without a trailing `/`, as a URL parser writes it back (a `/`
/// added before the query, the host in lower case), and each part that may
/// carry a key on its own (the user information, the path, the query and
/// each of its values).
fn endpoint_forms(rpc: &str) -> Vec<String> {
    let rpc = rpc.trim();
    let mut forms = vec![rpc.to_string(), rpc.trim_end_matches('/').to_string()];
    if let Ok(url) = reqwest::Url::parse(rpc) {
        let written = url.to_string();
        forms.push(written.trim_end_matches('/').to_string());
        forms.push(written);
        forms.push(url.username().to_string());
        forms.extend(url.password().map(str::to_string));
        if url.path() != "/" {
            forms.push(url.path().to_string());
        }
        if let Some(query) = url.query() {
            forms.push(query.to_string());
            for (_, value) in url.query_pairs() {
                forms.push(value.into_owned());
            }
        }
        forms.extend(url.fragment().map(str::to_string));
    }
    // A part too short to be a key would only blank ordinary words.
    forms.retain(|form| form.len() >= 4);
    forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
    forms.dedup();
    forms
}

/// The Solana CLI's output with what must never be shown taken out: the
/// endpoint (it may carry a key), in any form it may be printed, and any
/// recovery seed phrase it prints for an ephemeral keypair (the line
/// announcing it, and any line that is a run of 12 to 24 lowercase words).
pub fn redact_cli_output(text: &str, rpc: &str) -> String {
    let forms = endpoint_forms(rpc);
    let mut out = String::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let words: Vec<&str> = line.split_whitespace().collect();
        let phrase = (12..=24).contains(&words.len())
            && words
                .iter()
                .all(|word| word.chars().all(|c| c.is_ascii_lowercase()));
        if lower.contains("seed phrase") || lower.contains("keygen recover") || phrase {
            out.push_str("(a recovery seed phrase was withheld)\n");
            continue;
        }
        let mut line = line.to_string();
        for form in &forms {
            line = line.replace(form.as_str(), "<endpoint>");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The Solana CLI, run against one endpoint that never appears on its
/// command line (it may carry a key): the endpoint and the deployer are in a
/// configuration file readable by the owner alone, removed when this is
/// dropped. Its output is shown only through [`redact_cli_output`].
pub struct Cli {
    config: PathBuf,
    rpc: String,
    use_rpc: bool,
}

/// One buffer the CLI lists for the deployer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    pub address: Pubkey,
    pub lamports: u64,
}

impl Cli {
    pub fn new(state_dir: &Path, rpc: &str, deployer: &Path, mode: Mode) -> Result<Self> {
        let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
        let config = state_dir.join(format!("solana-cli-{}.yml", std::process::id()));
        write_private(
            &config,
            &format!(
                "json_rpc_url: {}\nwebsocket_url: \"\"\nkeypair_path: {}\naddress_labels: {{}}\ncommitment: confirmed\n",
                quote(rpc),
                quote(&deployer.display().to_string())
            ),
        )?;
        Ok(Self {
            config,
            rpc: rpc.to_string(),
            use_rpc: mode == Mode::Fork,
        })
    }

    async fn run(&self, args: &[&std::ffi::OsStr]) -> Result<String> {
        let output = tokio::process::Command::new("solana")
            .arg("--config")
            .arg(&self.config)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .context("running the Solana CLI: is it installed?")?;
        let shown = |bytes: &[u8]| redact_cli_output(&String::from_utf8_lossy(bytes), &self.rpc);
        if !output.status.success() {
            bail!(
                "`solana {}` failed:\n{}{}",
                args.iter()
                    .take(2)
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                shown(&output.stdout),
                shown(&output.stderr)
            );
        }
        Ok(shown(&output.stdout))
    }

    /// Deploy `program` from its own buffer, `buffer` (a keypair file kept
    /// across runs): a deploy that stops part way leaves that buffer, which
    /// the next run writes on and deploys from, and the CLI never makes an
    /// ephemeral buffer (or prints its seed phrase).
    async fn deploy(&self, program: &Planned, keypair: &Path, buffer: &Path) -> Result<()> {
        let price = COMPUTE_UNIT_PRICE.to_string();
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "program".as_ref(),
            "deploy".as_ref(),
            "--program-id".as_ref(),
            keypair.as_os_str(),
            "--buffer".as_ref(),
            buffer.as_os_str(),
            "--with-compute-unit-price".as_ref(),
            price.as_ref(),
            "--max-sign-attempts".as_ref(),
            "50".as_ref(),
        ];
        // A fork has no leaders to send to: everything through its endpoint.
        if self.use_rpc {
            args.push("--use-rpc".as_ref());
        }
        if program.program.final_at_once {
            args.push("--final".as_ref());
        }
        args.push(program.so.as_os_str());
        self.run(&args).await?;
        Ok(())
    }

    async fn finalise(&self, id: &Pubkey) -> Result<()> {
        let id = id.to_string();
        self.run(&[
            "program".as_ref(),
            "set-upgrade-authority".as_ref(),
            id.as_ref(),
            "--final".as_ref(),
        ])
        .await?;
        Ok(())
    }

    /// The buffers whose authority is `authority`.
    async fn buffers(&self, authority: &Pubkey) -> Result<Vec<Buffer>> {
        let authority = authority.to_string();
        let text = self
            .run(&[
                "program".as_ref(),
                "show".as_ref(),
                "--buffers".as_ref(),
                "--buffer-authority".as_ref(),
                authority.as_ref(),
                "--output".as_ref(),
                "json".as_ref(),
            ])
            .await?;
        parse_buffers(&text)
    }

    /// Close a buffer the deployer holds, its rent back to the deployer.
    async fn close_buffer(&self, buffer: &Pubkey) -> Result<()> {
        let buffer = buffer.to_string();
        self.run(&["program".as_ref(), "close".as_ref(), buffer.as_ref()])
            .await?;
        Ok(())
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.config);
    }
}

/// The buffers in `solana program show --buffers --output json`.
pub fn parse_buffers(text: &str) -> Result<Vec<Buffer>> {
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|_| anyhow!("the Solana CLI's list of buffers is not JSON"))?;
    let mut out = Vec::new();
    for buffer in value["buffers"].as_array().cloned().unwrap_or_default() {
        let address = buffer["address"]
            .as_str()
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| anyhow!("a listed buffer has no address"))?;
        out.push(Buffer {
            address,
            lamports: buffer["lamports"].as_u64().unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Each program's buffer keypair, kept under the run's state directory so a
/// stopped deploy resumes on the same buffer.
pub fn buffer_file(state_dir: &Path, program: &str) -> PathBuf {
    state_dir
        .join("buffers")
        .join(format!("{program}-buffer.json"))
}

/// The buffer keypair for `program`: the one kept from an earlier run, or a
/// new one written readable by the owner alone.
pub fn buffer_keypair(state_dir: &Path, program: &str) -> Result<Pubkey> {
    let file = buffer_file(state_dir, program);
    if !file.is_file() {
        let pair = Keypair::new();
        let bytes: Vec<String> = pair.to_bytes().iter().map(u8::to_string).collect();
        write_private(&file, &format!("[{}]", bytes.join(",")))?;
    }
    Ok(keys::read_keypair(&file)?.pubkey())
}

/// Which of the deployer's buffers to close before the deploy: every one
/// that isn't a program's own kept buffer (`ours`), which the deploy resumes.
pub fn stray_buffers(listed: &[Buffer], ours: &[Pubkey]) -> Vec<Buffer> {
    listed
        .iter()
        .filter(|buffer| !ours.contains(&buffer.address))
        .cloned()
        .collect()
}

/// Whether the program at `id` runs exactly `so`: its program data holds
/// the code after its header, and nothing after it but zeros.
async fn runs(client: &SolanaClient, id: &Pubkey, so: &[u8]) -> Result<bool> {
    let data = client
        .account(&program_data_address(id))
        .await?
        .map(|account| account.data)
        .unwrap_or_default();
    let header = PROGRAM_DATA_HEADER as usize;
    Ok(data.len() >= header + so.len()
        && &data[header..header + so.len()] == so
        && data[header + so.len()..].iter().all(|byte| *byte == 0))
}

/// The cluster's transaction count, on a fork alone: on mainnet it is the
/// whole cluster's, not this deploy's.
async fn fork_count(mode: Mode, client: &SolanaClient) -> Option<u64> {
    if mode != Mode::Fork {
        return None;
    }
    client
        .rpc
        .call::<u64>("getTransactionCount", json!([]))
        .await
        .ok()
}

// --- The set-up, read back ----------------------------------------------------------

/// One check of the read-back.
pub struct Check {
    pub what: String,
    pub ok: bool,
    pub found: String,
    /// Failed because the account isn't there yet, not because it holds
    /// something else.
    pub absent: bool,
}

fn check(checks: &mut Vec<Check>, what: impl Into<String>, ok: bool, found: impl Into<String>) {
    checks.push(Check {
        what: what.into(),
        ok,
        found: found.into(),
        absent: false,
    });
}

fn absent(checks: &mut Vec<Check>, what: impl Into<String>) {
    checks.push(Check {
        what: what.into(),
        ok: false,
        found: "not there".to_string(),
        absent: true,
    });
}

/// What the set-up's accounts hold on the cluster.
#[derive(Default)]
pub struct SetupRead {
    pub checker: Option<cs_verifier::Checker>,
    pub checker_owner: Option<Pubkey>,
    pub registry: Option<registry::state::Config>,
    pub publishing: Option<publishing::state::Config>,
    pub prices: Option<prices::PriceList>,
    pub station: Option<station::state::Config>,
    pub factory: Option<vault::state::Factory>,
}

/// Read every account the set-up makes, from the cluster.
pub async fn read_setup(client: &SolanaClient) -> Result<SetupRead> {
    Ok(SetupRead {
        checker: client.load(&checker_state()).await?,
        checker_owner: client.account(&checker_state()).await?.map(|a| a.owner),
        registry: client.load(&registry::read::config_address()).await?,
        publishing: client
            .load(&chain::solana::platform::publishing_config())
            .await?,
        prices: client.load(&prices::read::prices_address()).await?,
        station: client.load(&station::config_address()).await?,
        factory: client.load(&vault::state::factory_address()).await?,
    })
}

/// Every setting the set-up fixes, as read from the cluster, against this
/// deploy's ADMIN and the mainnet settings. An account not there yet is a
/// failed check marked absent.
pub fn setup_checks(read: &SetupRead, admin: &Pubkey, settings: &Settings, now: u64) -> Vec<Check> {
    let admin = *admin;
    let cluster = common::digest::cluster_value(CLUSTER);
    let token = chain::solana::programs::anchor_spl::token::ID;
    let mut checks = Vec::new();
    let checks_ = &mut checks;
    match &read.checker {
        None => absent(checks_, "the Confidential Space checker's account is there"),
        Some(c) => {
            check(
                checks_,
                "the checker pins the guest image",
                c.image_id == image_id(),
                hex(&c.image_id),
            );
            check(
                checks_,
                "the checker trusts Google's root CA alone",
                c.trusted_keys == trusted_keys(),
                c.trusted_keys
                    .iter()
                    .map(|k| hex(k))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            check(
                checks_,
                "the checker's account is the checker program's",
                read.checker_owner == Some(cs_verifier::ID),
                format!("{:?}", read.checker_owner),
            );
        }
    }
    match &read.registry {
        None => absent(checks_, "the registry is set up"),
        Some(c) => {
            check(
                checks_,
                "registry admin is ADMIN",
                c.admin == admin,
                c.admin.to_string(),
            );
            check(
                checks_,
                "registry floor is two days",
                c.min_timelock == settings.min_timelock,
                format!("{} s", c.min_timelock),
            );
            check(
                checks_,
                "registry timelock is two days",
                c.timelock(now) == settings.timelock,
                format!("{} s", c.timelock(now)),
            );
            check(
                checks_,
                "registry blessings last as set",
                c.blessing_ttl == settings.blessing_ttl,
                format!("{} s", c.blessing_ttl),
            );
            check(
                checks_,
                "registry is bound to mainnet-beta",
                c.cluster == cluster,
                hex(&c.cluster),
            );
            let only = c.verifiers.len() == 1
                && c.verifiers[0].program == cs_verifier::ID
                && c.verifiers[0].state == checker_state();
            check(
                checks_,
                "the Confidential Space checker is the only checker",
                only,
                c.verifiers
                    .iter()
                    .map(|v| format!("{} {}", v.program, v.state))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            check(
                checks_,
                "no development checker is listed",
                c.verifiers.iter().all(|v| v.program == cs_verifier::ID),
                "",
            );
            check(
                checks_,
                "the checker counts now",
                c.verifiers.first().is_some_and(|v| v.ready_at <= now),
                "",
            );
        }
    }
    match &read.publishing {
        None => absent(checks_, "publishing is set up"),
        Some(c) => {
            check(
                checks_,
                "publishing takes USDC",
                c.mint == CIRCLE_MAINNET_USDC && c.token_program == token && c.decimals == 6,
                format!("{} ({} decimals)", c.mint, c.decimals),
            );
            check(
                checks_,
                "publishing fees go to ADMIN",
                c.platform == admin,
                c.platform.to_string(),
            );
            check(
                checks_,
                "publishing's fee is a dollar",
                c.fee == crate::solana::MAINNET_PUBLISH_FEE,
                c.fee.to_string(),
            );
        }
    }
    match &read.prices {
        None => absent(checks_, "the price list is set up"),
        Some(c) => {
            check(
                checks_,
                "price list admin is ADMIN",
                c.admin == admin,
                c.admin.to_string(),
            );
            check(
                checks_,
                "price list notice is thirty days",
                c.notice == settings.price_notice,
                format!("{} s", c.notice),
            );
            check(
                checks_,
                "the price list holds the published prices alone",
                c.history.len() == 1 && c.history[0].prices == crate::solana::published_prices(),
                format!("{} period(s)", c.history.len()),
            );
        }
    }
    match &read.station {
        None => absent(checks_, "the stations are set up"),
        Some(c) => {
            check(
                checks_,
                "stations take USDC",
                c.mint == CIRCLE_MAINNET_USDC,
                c.mint.to_string(),
            );
            check(
                checks_,
                "stations are created by the vault program",
                c.vault_program == vault::ID,
                c.vault_program.to_string(),
            );
            check(
                checks_,
                "stations are bound to mainnet-beta",
                c.cluster == cluster,
                hex(&c.cluster),
            );
        }
    }
    match &read.factory {
        None => absent(checks_, "the vault factory is set up"),
        Some(f) => {
            check(
                checks_,
                "vault factory admin is ADMIN",
                f.admin == admin,
                f.admin.to_string(),
            );
            check(
                checks_,
                "vault factory platform is ADMIN",
                f.platform == admin,
                f.platform.to_string(),
            );
            check(
                checks_,
                "vaults take USDC",
                f.mint == CIRCLE_MAINNET_USDC && f.token_program == token,
                f.mint.to_string(),
            );
            check(
                checks_,
                "vaults are on the mainnet terms",
                f.terms == settings.terms,
                "",
            );
            check(
                checks_,
                "the vault factory is bound to mainnet-beta and the chain \"solana\"",
                f.cluster == cluster && f.chain_name == chain::solana::name("solana"),
                "",
            );
            check(
                checks_,
                "the vault factory names this deploy's programs and replaces none",
                f.registry == registry::ID
                    && f.publishing == publishing::ID
                    && f.prices == prices::ID
                    && f.station_program == station::ID
                    && f.previous == Pubkey::default(),
                format!("previous {}", f.previous),
            );
        }
    }
    checks
}

/// Refuse a set-up already on the cluster that isn't this deploy's: an
/// account there holding another admin or other settings (from an earlier
/// run with another ADMIN, say). Accounts not there yet are fine.
pub fn refuse_another_setup(checks: &[Check]) -> Result<()> {
    let other: Vec<String> = checks
        .iter()
        .filter(|c| !c.ok && !c.absent)
        .map(|c| format!("  {}: {}", c.what, c.found))
        .collect();
    if !other.is_empty() {
        bail!(
            "the set-up already on the cluster is not this deploy's (another ADMIN, or other \
             settings, from an earlier run):\n{}\nNothing was sent, and no program was made final: \
             check ADMIN. A program still upgradeable stays so, by the deployer alone.",
            other.join("\n")
        );
    }
    Ok(())
}

/// Require every set-up account there and holding this deploy's settings,
/// as read back from the cluster: the last check before anything is made
/// final.
pub fn require_setup(checks: &[Check]) -> Result<()> {
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| format!("  {}: {}", c.what, c.found))
        .collect();
    if !failed.is_empty() {
        bail!(
            "the set-up read back from the cluster is not this deploy's:\n{}\nNo program was made \
             final.",
            failed.join("\n")
        );
    }
    Ok(())
}

// --- The deploy ----------------------------------------------------------------------

pub struct DeployInputs<'a> {
    pub key: &'a ChainKey,
    pub config: &'a ChainConfig,
    pub flags: Flags,
    pub program_keypairs: Option<PathBuf>,
    /// Where the mainnet build of the programs is (`<name>.so`).
    pub programs_dir: &'a Path,
    /// `contracts/solana/programs`, where each `mainnet-id` goes.
    pub sources: &'a Path,
    pub deployments_dir: &'a Path,
    pub admin_steps_dir: &'a Path,
    /// Where a fork rehearsal keeps its throwaway deployer.
    pub state_dir: &'a Path,
    pub project: Option<String>,
}

/// What one program's deploy took, measured on the cluster.
struct Measured {
    name: &'static str,
    /// Counted on a fork alone: on mainnet the count is the whole cluster's.
    transactions: Option<u64>,
    lamports: i128,
}

/// Where the mainnet-id files are written on a real send: this
/// repository's own `contracts/solana/programs`, the same directory on disk.
fn committed_sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("contracts")
        .join("solana")
        .join("programs")
}

fn same_place(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Refuse directories that don't suit the mode, before anything is sent or
/// written: every mode reads each `mainnet-id` from this repository's own
/// programs alone, where a real send writes it; the admin's steps of a fork never go where the
/// hardware key's are kept (`.shonin/mainnet/admin-steps`) or under the
/// committed addresses, and the hardware key's never come from the fork's.
pub fn check_dirs(mode: Mode, sources: &Path, admin_steps_dir: &Path) -> Result<()> {
    check_sources(sources)?;
    mainnet_guard::check_admin_steps_dir(mode, admin_steps_dir)
}

/// `mainnet-solana-deploy`: the plan, and in a sending mode the deploy.
pub async fn deploy(inputs: DeployInputs<'_>) -> Result<()> {
    let decided = decide(&inputs.flags)?;
    let mode = decided.mode;
    let admin = decided.admin;
    let settings = Settings::mainnet(inputs.project.as_deref())?;
    let ids = compiled_ids();

    // Everything checked before anything is sent, or written: the endpoints
    // first.
    mainnet_guard::check_config_endpoints(mode, inputs.config)?;
    check_dirs(mode, inputs.sources, inputs.admin_steps_dir)?;
    // A fork takes the throwaways the ids step made, in the directory named
    // or the default one.
    let program_keypairs = match (&inputs.program_keypairs, mode) {
        (None, Mode::Fork) => Some(throwaway_keypairs_dir(&mainnet_guard::fork_root())),
        (given, _) => given.clone(),
    };
    let keypairs = match (&program_keypairs, mode) {
        (Some(dir), _) => {
            let pairs = read_program_keypairs(dir)?;
            let found: Vec<(&str, Pubkey)> =
                pairs.iter().map(|(name, id, _)| (*name, *id)).collect();
            check_program_keypairs(
                mode,
                dir,
                &found,
                inputs.sources,
                &mainnet_guard::fork_root(),
            )?;
            Some(pairs)
        }
        (None, Mode::DryRun) => None,
        (None, _) => bail!(
            "MAINNET_PROGRAM_KEYPAIRS=<dir> is required to send: the owner's keypair for each \
             program. Nothing was sent."
        ),
    };
    if let Some(pairs) = &keypairs {
        for (name, id, _) in pairs {
            if ids[name] != *id {
                bail!(
                    "{name}'s keypair is {id}, and this build compiled in {}: build for mainnet with \
                     these keypairs' ids (the task does). Nothing was sent.",
                    ids[name]
                );
            }
        }
    }
    let mut all_committed = true;
    for program in SHIPPING {
        let text = std::fs::read_to_string(committed_file(inputs.sources, program.name)).ok();
        // A dry run without the keypairs builds with the committed ids
        // where there are any.
        agrees_with_committed(program.name, &ids[program.name], text.as_deref())?;
        all_committed &= committed_id(inputs.sources, program.name)?.is_some();
    }
    let deployments_dir = mainnet_guard::deployments_dir(mode, inputs.deployments_dir)?;
    let deployer = match mode {
        Mode::DryRun => None,
        Mode::Send => {
            let file = inputs
                .flags
                .deployer_keyfile
                .clone()
                .expect("a send has its key file");
            Some((
                keys::read_keypair(&file).map_err(|_| {
                    anyhow!(
                        "{} is not a Solana keypair file. Nothing was sent.",
                        file.display()
                    )
                })?,
                file,
            ))
        }
        Mode::Fork => {
            // The throwaway deployer, never a key file given (`decide`
            // refuses one). Kept across runs, so a rehearsal stopped part
            // way resumes with the deployer that holds its programs.
            let file = inputs.state_dir.join("throwaway-deployer.json");
            if !file.is_file() {
                keys::write_keypair(&Keypair::new(), &file)?;
            }
            Some((keys::read_keypair(&file)?, file))
        }
    };
    if let Some((deployer, _)) = &deployer {
        if deployer.pubkey() == admin {
            bail!(
                "the deployer is ADMIN: the hot deployer must hold no power once the deploy ends, \
                 and the admin is the hardware key. Nothing was sent."
            );
        }
        if ids.values().any(|id| *id == deployer.pubkey()) {
            bail!("the deployer's key is one of the programs' keys. Nothing was sent.");
        }
    }

    // Only the verifiable build is deployed: the one anyone can rebuild from
    // the commit and compare with what mainnet runs.
    let build = check_verifiable_build(inputs.programs_dir, &build_source_for(mode)?)?;
    // What is deployed is the bytes checked, held where nothing else writes:
    // the build directory may change between the check and the deploy.
    let programs_dir = hold_checked_build(inputs.programs_dir, &build, inputs.state_dir)?;
    let programs_dir = programs_dir.as_path();
    if build.source == WORKING_TREE {
        println!(
            "The programs are the verifiable build of the working tree, with uncommitted \
             changes: a rehearsal's. A real send deploys a commit's alone."
        );
    } else {
        println!(
            "The programs are the verifiable build of commit {} ({}).",
            build.source, build.image
        );
    }
    let client = connect(mode, inputs.key, inputs.config).await?;
    for line in check_outside(&client).await? {
        tracing::info!("{line}");
    }
    let planned = plan(&client, &ids, programs_dir).await?;
    let admin_steps = planned_admin_steps(admin, settings.project);
    print_plan(
        &planned,
        &admin,
        &admin_steps,
        keypairs.is_none() && !all_committed,
    );
    for p in &planned {
        if let Found::Upgradeable(by) = p.found
            && deployer.as_ref().is_some_and(|(d, _)| d.pubkey() != by)
        {
            bail!(
                "{} at {} can be replaced by {by}, not this deployer: it isn't this deploy's. Nothing was sent.",
                p.program.name,
                p.id
            );
        }
    }
    // A set-up already there from an earlier run must be this run's: the
    // same ADMIN and settings. Read before anything is sent.
    let now = client.now().await?;
    let found_setup = setup_checks(&read_setup(&client).await?, &admin, &settings, now);
    refuse_another_setup(&found_setup)?;
    let (deployer, deployer_file) = match deployer {
        None => {
            println!(
                "Dry run: nothing was sent. Rehearse with ON_FORK=1, or send with DEPLOYER_KEYFILE=<path> CONFIRM_MAINNET={CONFIRMATION}."
            );
            return Ok(());
        }
        Some(deployer) => deployer,
    };
    let keypairs = keypairs.expect("a sending mode has the keypairs");

    let rpc = inputs
        .config
        .rpc
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let cli = Cli::new(inputs.state_dir, &rpc, &deployer_file, mode)?;

    // Each program's own buffer, kept across runs; any other buffer the
    // deployer holds (a stray from an earlier deploy) is closed, its rent
    // back to the deployer.
    let mut ours = Vec::new();
    for p in planned.iter().filter(|p| p.found == Found::Nothing) {
        ours.push(buffer_keypair(inputs.state_dir, p.program.name)?);
    }
    let mut closed = 0;
    for stray in stray_buffers(&cli.buffers(&deployer.pubkey()).await?, &ours) {
        cli.close_buffer(&stray.address)
            .await
            .with_context(|| format!("closing the deployer's stray buffer {}", stray.address))?;
        println!(
            "closed the deployer's stray buffer {} ({} SOL back)",
            stray.address,
            sol(stray.lamports)
        );
        closed += 1;
    }
    // What this deploy's own buffers already hold counts toward their rent.
    let mut held_in_buffers = 0;
    for buffer in &ours {
        held_in_buffers += client.lamports(buffer).await?;
    }

    // The deployer's coin: on a fork from Surfpool's faucet; for real, the
    // owner's, and the run stops before sending if it can't cover the peak.
    let need = (cost(&planned).peak + cost(&planned).fees + MARGIN).saturating_sub(held_in_buffers);
    let holds = client.lamports(&deployer.pubkey()).await?;
    if holds < need {
        match mode {
            Mode::Fork => {
                client
                    .airdrop(&deployer.pubkey(), need - holds + LAMPORTS_PER_SOL)
                    .await?;
                tracing::info!(deployer = %deployer.pubkey(), "the throwaway deployer is funded from the fork's faucet");
            }
            _ => bail!(
                "the deployer {} holds {} SOL and the deploy needs {} SOL at its peak (with the fees \
                 and a margin). Fund it and run this again. {}",
                deployer.pubkey(),
                sol(holds),
                sol(need),
                if closed == 0 {
                    "Nothing was sent.".to_string()
                } else {
                    format!(
                        "Nothing was deployed: only the closes of the deployer's {closed} stray \
                         buffer(s) were sent, their rent back to the deployer."
                    )
                }
            ),
        }
    }

    let started_lamports = client.lamports(&deployer.pubkey()).await?;
    let started_count = fork_count(mode, &client).await;
    let deployed_slot = client.rpc.slot().await?;
    let mut measured = Vec::new();

    // 1. Every program, one at a time, so the most out at once is the rent
    // kept so far and one buffer.
    for p in &planned {
        let so = std::fs::read(&p.so).with_context(|| format!("reading {}", p.so.display()))?;
        let keypair = &keypairs
            .iter()
            .find(|(name, _, _)| *name == p.program.name)
            .expect("every keypair")
            .2;
        match crate::solana::upgrade_authority(&client, &p.id).await? {
            None => {
                let buffer = buffer_file(inputs.state_dir, p.program.name);
                buffer_keypair(inputs.state_dir, p.program.name)?;
                let before_lamports = client.lamports(&deployer.pubkey()).await?;
                let before_count = fork_count(mode, &client).await;
                // A fork fetches every account it hasn't seen from its
                // datasource, which now and then fails a request: a rehearsal
                // tries a program again. A real send doesn't: run it again
                // and it resumes on the same buffer.
                let tries = if mode == Mode::Fork { 3 } else { 1 };
                let mut attempt = 1;
                loop {
                    match cli.deploy(p, keypair, &buffer).await {
                        Ok(()) => break,
                        Err(_)
                            if crate::solana::upgrade_authority(&client, &p.id)
                                .await?
                                .is_some() =>
                        {
                            break;
                        }
                        Err(error) if attempt < tries => {
                            tracing::warn!(
                                program = p.program.name,
                                attempt,
                                error = format!("{error:#}"),
                                "the deploy failed on the fork; trying again"
                            );
                            attempt += 1;
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        }
                        Err(error) => {
                            return Err(error.context(format!(
                                "deploying {} (its buffer is kept: run this again to resume it)",
                                p.program.name
                            )));
                        }
                    }
                }
                let after_count = fork_count(mode, &client).await;
                let after_lamports = client.lamports(&deployer.pubkey()).await?;
                measured.push(Measured {
                    name: p.program.name,
                    transactions: before_count
                        .zip(after_count)
                        .map(|(a, b)| b.saturating_sub(a)),
                    lamports: before_lamports as i128 - after_lamports as i128,
                });
                tracing::info!(program = p.program.name, id = %p.id, "deployed");
            }
            Some(_) => {
                tracing::info!(program = p.program.name, id = %p.id, "already on the cluster")
            }
        }
        if !runs(&client, &p.id, &so).await? {
            bail!("{} at {} doesn't run this build", p.program.name, p.id);
        }
        match crate::solana::upgrade_authority(&client, &p.id).await? {
            Some(None) => {}
            Some(Some(by)) if by == deployer.pubkey() && p.program.final_at_once => {
                // Deployed, but its finalising didn't land: it has no
                // set-up, so it is made final now, before anything trusts it.
                cli.finalise(&p.id)
                    .await
                    .with_context(|| format!("making {} final", p.program.name))?;
                if crate::solana::upgrade_authority(&client, &p.id).await? != Some(None) {
                    bail!("{} at {} is not final", p.program.name, p.id);
                }
                tracing::info!(program = p.program.name, "final");
            }
            Some(Some(by)) if by == deployer.pubkey() => {}
            other => bail!(
                "{} at {} is {other:?} after its deploy",
                p.program.name,
                p.id
            ),
        }
        if mode == Mode::Send {
            let file = committed_file(inputs.sources, p.program.name);
            let held = std::fs::read_to_string(&file).ok();
            if !agrees_with_committed(p.program.name, &p.id, held.as_deref())? {
                mainnet_guard::write_file(&file, format!("{}\n", p.id))?;
                println!("wrote {}: commit it", file.display());
            }
        }
    }
    // No buffer is left holding the deployer's coin.
    for left in cli.buffers(&deployer.pubkey()).await? {
        cli.close_buffer(&left.address)
            .await
            .with_context(|| format!("closing the buffer {} left by the deploy", left.address))?;
        println!(
            "closed the buffer {} left by the deploy ({} SOL back)",
            left.address,
            sol(left.lamports)
        );
    }
    if !cli.buffers(&deployer.pubkey()).await?.is_empty() {
        bail!("the deployer still holds a buffer after closing them; nothing was made final");
    }

    // 2. The set-up, signed by the deployer while it can still upgrade the
    // programs whose set-up checks that. A step whose account is there was
    // checked above to hold this deploy's settings.
    let setup_count = fork_count(mode, &client).await;
    let setup_lamports = client.lamports(&deployer.pubkey()).await?;
    let platform = platform(admin);
    let (create_checker, checker) =
        Platform::ix_create_cs_checker(&deployer.pubkey(), image_id(), &trusted_keys());
    if client.account(&checker).await?.is_none() {
        client
            .send(vec![create_checker], &[&deployer])
            .await
            .context("creating the checker's account")?;
    }
    if client
        .account(&registry::read::config_address())
        .await?
        .is_none()
    {
        let ix = Platform::ix_initialize_registry_trusting(
            &deployer.pubkey(),
            registry::InitializeArgs {
                admin,
                cluster_name: CLUSTER.to_string(),
                timelock: settings.timelock,
                min_timelock: settings.min_timelock,
                blessing_ttl: settings.blessing_ttl,
            },
            cs_verifier::ID,
            checker,
        );
        client
            .send(vec![ix], &[&deployer])
            .await
            .context("setting the registry up")?;
    }
    if client
        .account(&chain::solana::platform::publishing_config())
        .await?
        .is_none()
    {
        let ix = platform
            .ix_initialize_publishing(&deployer.pubkey(), crate::solana::MAINNET_PUBLISH_FEE);
        client
            .send(vec![ix], &[&deployer])
            .await
            .context("setting publishing up")?;
    }
    if client
        .account(&prices::read::prices_address())
        .await?
        .is_none()
    {
        let ix = platform.ix_initialize_prices(
            &deployer.pubkey(),
            settings.price_notice,
            crate::solana::published_prices(),
        );
        client
            .send(vec![ix], &[&deployer])
            .await
            .context("setting the price list up")?;
    }
    if client.account(&station::config_address()).await?.is_none() {
        let ix = platform.ix_initialize_stations(&deployer.pubkey());
        client
            .send(vec![ix], &[&deployer])
            .await
            .context("setting the stations up")?;
    }
    if client
        .account(&vault::state::factory_address())
        .await?
        .is_none()
    {
        let ix = platform.ix_initialize_vaults(&deployer.pubkey(), settings.terms);
        client
            .send(vec![ix], &[&deployer])
            .await
            .context("setting the vault factory up")?;
    }

    // 3. Every set-up account read back from the cluster, then, last, the
    // programs made final and every authority read back as none.
    let now = client.now().await?;
    let read_back = setup_checks(&read_setup(&client).await?, &admin, &settings, now);
    require_setup(&read_back)?;
    println!(
        "The set-up is read back from the cluster: {} checks passed",
        read_back.len()
    );
    for p in planned.iter().filter(|p| !p.program.final_at_once) {
        if let Some(Some(_)) = crate::solana::upgrade_authority(&client, &p.id).await? {
            cli.finalise(&p.id)
                .await
                .with_context(|| format!("making {} final", p.program.name))?;
            tracing::info!(program = p.program.name, "final");
        }
    }
    for p in &planned {
        if crate::solana::upgrade_authority(&client, &p.id).await? != Some(None) {
            bail!("{} at {} is not final", p.program.name, p.id);
        }
    }
    let ended_count = fork_count(mode, &client).await;
    let ended_lamports = client.lamports(&deployer.pubkey()).await?;
    drop(cli);

    // 4. Written down: the deployment, and the admin's steps.
    let deployments = SolanaDeployments {
        chain_key: CHAIN_KEY.to_string(),
        cluster: CLUSTER.to_string(),
        rpc_url: inputs.config.public_rpc.clone(),
        genesis_hash: client.genesis_hash.clone(),
        deployed_slot,
        programs: ids
            .iter()
            .map(|(name, id)| (name.to_string(), id.to_string()))
            .collect(),
        mint: Some(CIRCLE_MAINNET_USDC.to_string()),
        token_program: Some(chain::solana::programs::anchor_spl::token::ID.to_string()),
        // The development checker's slot: mainnet has none.
        checker: None,
        roles: Some(roles(&deployer.pubkey(), &admin)),
    };
    let mut written: Value = serde_json::from_str(&deployments.to_json())?;
    written["mainnet"] = json!({
        "checker_program": cs_verifier::ID.to_string(),
        "checker": checker.to_string(),
        "image_id": hex(&image_id()),
        "trusted_keys": trusted_keys().iter().map(|k| hex(k)).collect::<Vec<_>>(),
        "orca_pool": chain::solana::orca::POOLS[0].address.to_string(),
        "pyth_price_account": chain::solana::orca::PRICE_ACCOUNT.to_string(),
        "deployer": deployer.pubkey().to_string(),
    });
    std::fs::create_dir_all(&deployments_dir)?;
    let file = deployments_dir.join(format!("{}.json", inputs.key.file_stem()));
    mainnet_guard::write_file(&file, serde_json::to_string_pretty(&written)?)?;
    println!("The deployment is written to {}", file.display());

    let steps: Vec<AdminStep> =
        admin_instructions(&client, admin, settings.project, inputs.project.as_deref())
            .await?
            .into_iter()
            .map(|(step, _)| step)
            .collect();
    let steps_file = write_admin_steps(inputs.admin_steps_dir, &admin, &steps)?;
    println!(
        "The admin's {} step(s) are written to {}",
        steps.len(),
        steps_file.display()
    );

    // 5. What it took.
    println!("Measured:");
    for m in &measured {
        println!(
            "  {:<20} {} transactions, {} SOL",
            m.name,
            m.transactions.map_or("?".to_string(), |n| n.to_string()),
            sol9(m.lamports.max(0) as u64)
        );
    }
    if let (Some(a), Some(b)) = (setup_count, ended_count) {
        println!(
            "  set-up and finalising: {} transactions, {} SOL",
            b.saturating_sub(a),
            sol9(setup_lamports.saturating_sub(ended_lamports))
        );
    }
    if let (Some(a), Some(b)) = (started_count, ended_count) {
        println!("  in all: {} transactions", b.saturating_sub(a));
    }
    println!(
        "  the deployer spent {} SOL",
        sol9(started_lamports.saturating_sub(ended_lamports))
    );
    Ok(())
}

/// The roles a mainnet deployment records: the deployer, and the admin as
/// admin and platform. Every other role is none until its key exists.
fn roles(deployer: &Pubkey, admin: &Pubkey) -> SolanaRoles {
    let none = Pubkey::default().to_string();
    SolanaRoles {
        deployer: deployer.to_string(),
        admin: admin.to_string(),
        platform: admin.to_string(),
        attestor: none.clone(),
        builder: none.clone(),
        reviewer: none.clone(),
        executor: none.clone(),
        keeper: none.clone(),
        operator: none.clone(),
        author: none.clone(),
        viewer: none,
    }
}

// --- The admin's steps, sent or printed ------------------------------------------------

/// `mainnet-solana-admin-steps`: on a fork, the admin's steps still to do
/// sent from a stand-in admin's keypair (`admin_keyfile`, which must be
/// ADMIN), and with `warp` the fork's clock moved past the timelock; on
/// mainnet never sent: each step still to do is printed and written for the
/// hardware key.
#[allow(clippy::too_many_arguments)]
pub async fn admin(
    key: &ChainKey,
    config: &ChainConfig,
    flags: &Flags,
    admin_keyfile: Option<&Path>,
    admin_steps_dir: &Path,
    project: Option<&str>,
    image: Option<([u8; 32], registry::state::ImageKind)>,
    warp: bool,
) -> Result<()> {
    let decided = decide(flags)?;
    let project_id = Settings::mainnet(project)?.project;
    match decided.mode {
        Mode::Send => bail!(
            "the admin's steps are never sent by this tool on mainnet: the hardware key signs them. \
             Run without DEPLOYER_KEYFILE and CONFIRM_MAINNET to print them. Nothing was sent."
        ),
        Mode::DryRun if admin_keyfile.is_some() => bail!(
            "ADMIN_KEYFILE is a fork's stand-in admin: on mainnet the admin is the hardware key. \
             Nothing was sent."
        ),
        _ => {}
    }
    check_dirs(decided.mode, &committed_sources(), admin_steps_dir)?;
    let client = connect(decided.mode, key, config).await?;
    let mut steps = admin_instructions(&client, decided.admin, project_id, project).await?;
    if let Some((digest, kind)) = image {
        // A real image digest, given: proposed once, as one kind, and a
        // revoked one never again.
        let held: Option<registry::state::ImageRecord> =
            client.load(&registry::read::image_address(&digest)).await?;
        let shown = hex(&digest);
        match held.filter(|image| image.proposed) {
            Some(image) if image.kind != kind => bail!(
                "the image {shown} is already proposed as another kind ({:?}); an image is \
                 proposed once. Nothing was sent.",
                image.kind
            ),
            Some(image) if image.revoked => bail!(
                "the image {shown} was revoked; a revoked image can't be proposed again. Nothing \
                 was sent."
            ),
            Some(_) => println!("The image {shown} is proposed already."),
            None => {
                let ix = platform(decided.admin).ix_propose_image(digest, kind);
                steps.push((
                    AdminStep::of(
                        "propose-image",
                        format!(
                            "registry.propose_image: the {kind:?} image {shown}; it can be blessed \
                             once the timelock has passed"
                        ),
                        &ix,
                    ),
                    ix,
                ));
            }
        }
    }
    let file = write_admin_steps(
        admin_steps_dir,
        &decided.admin,
        &steps
            .iter()
            .map(|(step, _)| step.clone())
            .collect::<Vec<_>>(),
    )?;
    if decided.mode == Mode::DryRun {
        if steps.is_empty() {
            println!("Nothing is left for the admin to sign on Solana.");
        }
        for (step, _) in &steps {
            println!("{}: {}", step.name, step.purpose);
            println!("  program {}", step.program);
            for account in &step.accounts {
                println!(
                    "  account {}{}{}",
                    account.pubkey,
                    if account.signer { " (signer)" } else { "" },
                    if account.writable { " (writable)" } else { "" }
                );
            }
            println!("  data {}", step.data);
        }
        println!(
            "Written to {}. Sign each with the hardware key (ADMIN {}) as its fee payer and only \
             signer. Nothing was sent.",
            file.display(),
            decided.admin
        );
        return Ok(());
    }
    // A fork: the stand-in admin signs.
    let admin_keyfile = admin_keyfile.ok_or_else(|| {
        anyhow!("ADMIN_KEYFILE=<the stand-in admin's keypair> is required to send the admin's steps on a fork")
    })?;
    let stand_in = keys::read_keypair(admin_keyfile)?;
    if stand_in.pubkey() != decided.admin {
        bail!(
            "ADMIN_KEYFILE is {}, not ADMIN ({}). Nothing was sent.",
            stand_in.pubkey(),
            decided.admin
        );
    }
    if client.lamports(&stand_in.pubkey()).await? < LAMPORTS_PER_SOL / 10 {
        client.airdrop(&stand_in.pubkey(), LAMPORTS_PER_SOL).await?;
    }
    for (step, ix) in steps {
        client
            .send(vec![ix], &[&stand_in])
            .await
            .with_context(|| format!("sending {}", step.name))?;
        println!("sent {}: {}", step.name, step.purpose);
    }
    if warp {
        let config: registry::state::Config = client
            .load(&registry::read::config_address())
            .await?
            .ok_or_else(|| anyhow!("the registry is not set up"))?;
        let now = client.now().await?;
        let to = now + config.timelock(now) + 60;
        client.rpc.time_travel(to).await?;
        // The clock moves with the next slot.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        println!(
            "the fork's clock is moved {} s on, past the timelock",
            to - now
        );
    }
    Ok(())
}

// --- The read-back -----------------------------------------------------------------------

/// `mainnet-solana-verify`: read every program and setting back from the
/// cluster and compare it with the deployment file, this build and the
/// mainnet settings. Read-only.
#[allow(clippy::too_many_arguments)]
pub async fn verify(
    key: &ChainKey,
    config: &ChainConfig,
    flags: &Flags,
    deployments_dir: &Path,
    programs_dir: &Path,
    project: Option<&str>,
    expect_in_force: bool,
) -> Result<()> {
    let decided = decide(flags)?;
    // The read-back sends nothing: a send's flags are not needed, and on a
    // fork the endpoint must still be the fork.
    let mode = if decided.mode == Mode::Fork {
        Mode::Fork
    } else {
        Mode::Send
    };
    let admin = decided.admin;
    let settings = Settings::mainnet(project)?;
    let file = deployments_dir.join(format!("{}.json", key.file_stem()));
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let deployments = SolanaDeployments::from_json(&text)?;
    // What each program is compared with byte for byte is the verifiable
    // build the deploy deployed: on mainnet a commit's.
    let build = check_verifiable_build(
        programs_dir,
        &if mode == Mode::Fork {
            BuildSource::Any
        } else {
            BuildSource::ACommit
        },
    )?;
    println!(
        "Compared with the verifiable build of {} ({}).",
        build.source, build.image
    );
    let client = connect(mode, key, config).await?;
    let ids = compiled_ids();
    let now = client.now().await?;
    let mut checks = Vec::new();

    check(
        &mut checks,
        "the cluster is mainnet-beta",
        deployments.cluster == CLUSTER && client.genesis_hash == deployments.genesis_hash,
        client.genesis_hash.clone(),
    );
    let recorded: Vec<&str> = deployments.programs.keys().map(String::as_str).collect();
    let mut expected: Vec<&str> = SHIPPING.iter().map(|p| p.name).collect();
    expected.sort_unstable();
    check(
        &mut checks,
        "the file records the eight programs that ship and nothing else",
        recorded == expected,
        recorded.join(", "),
    );
    check(
        &mut checks,
        "no development program is recorded",
        !recorded.iter().any(|name| NEVER_ON_MAINNET.contains(name)),
        "",
    );
    for program in SHIPPING {
        let id = ids[program.name];
        let in_file = deployments.program(program.name).unwrap_or_default();
        check(
            &mut checks,
            format!("{}: the recorded id is this build's", program.name),
            in_file == id.to_string(),
            in_file.to_string(),
        );
        let authority = crate::solana::upgrade_authority(&client, &id).await?;
        check(
            &mut checks,
            format!("{}: final (no upgrade authority)", program.name),
            authority == Some(None),
            format!("{authority:?}"),
        );
        let so = std::fs::read(programs_dir.join(format!("{}.so", program.name)))
            .with_context(|| format!("reading {}.so from the deployed build", program.name))?;
        check(
            &mut checks,
            format!("{}: runs the deployed build's code", program.name),
            runs(&client, &id, &so).await?,
            format!("{} bytes", so.len()),
        );
    }

    // Every setting the set-up fixed, read from the cluster.
    checks.extend(setup_checks(
        &read_setup(&client).await?,
        &admin,
        &settings,
        now,
    ));

    // Orca's driver: what its code on the cluster lists, and the pool and
    // price account it names, read from the cluster.
    use chain::solana::orca;
    let pool: Pubkey = MAINNET_ORCA_POOL.parse().expect("a valid address");
    let driver_code = client
        .account(&program_data_address(&orca_driver::ID))
        .await?
        .map(|account| account.data)
        .unwrap_or_default();
    check(
        &mut checks,
        "Orca's driver on the cluster lists the SOL/USDC pool",
        embeds(&driver_code, pool.as_ref()),
        pool.to_string(),
    );
    let pool_account = client.account(&pool).await?;
    let pool_data = pool_account
        .as_ref()
        .map(|a| a.data.clone())
        .unwrap_or_default();
    // A whirlpool: its settings at 8, its tick spacing at 41, its second
    // mint at 181.
    let tick_spacing = pool_data
        .get(41..43)
        .map(|b| u16::from_le_bytes([b[0], b[1]]));
    check(
        &mut checks,
        "the pool is Orca's, under its settings, at tick spacing 4",
        pool_account.as_ref().map(|a| a.owner) == Some(orca::WHIRLPOOL_PROGRAM)
            && pool_data.get(8..40) == Some(orca::CONFIG.as_ref())
            && tick_spacing == Some(4),
        format!("tick spacing {tick_spacing:?}"),
    );
    check(
        &mut checks,
        "the pool trades against USDC",
        pool_data.get(181..213) == Some(CIRCLE_MAINNET_USDC.as_ref())
            || pool_data.get(101..133) == Some(CIRCLE_MAINNET_USDC.as_ref()),
        "",
    );
    let price_owner = client.account(&orca::PRICE_ACCOUNT).await?.map(|a| a.owner);
    check(
        &mut checks,
        "the price account is Pyth's receiver's",
        price_owner == Some(orca::PYTH_RECEIVER),
        format!("{price_owner:?}"),
    );
    let slot: Option<registry::state::ApprovalSlot> = client
        .load(&registry::read::approval_address(
            registry::state::ApprovalKind::Driver,
            &orca_driver::NAME,
        ))
        .await?;
    let (proposed, in_force) = slot.map_or((false, false), |slot| {
        (
            slot.next == orca_driver::ID || slot.current == orca_driver::ID,
            slot.effective(now) == orca_driver::ID,
        )
    });
    if expect_in_force {
        check(&mut checks, "Orca's driver is in force", in_force, "");
    } else {
        check(&mut checks, "Orca's driver is proposed", proposed, "");
    }
    if let Some(project) = settings.project {
        let record = client
            .account(&registry::read::project_address(&project))
            .await?;
        check(
            &mut checks,
            "the operator's project is proposed",
            record.is_some(),
            "",
        );
    }
    let roles = deployments.roles.clone().unwrap_or_default();
    check(
        &mut checks,
        "the deployment file's deployer is not ADMIN",
        roles.deployer != admin.to_string() && !roles.deployer.is_empty(),
        roles.deployer.clone(),
    );
    check(
        &mut checks,
        "the deployment file's admin and platform are ADMIN",
        roles.admin == admin.to_string() && roles.platform == admin.to_string(),
        roles.admin.clone(),
    );
    // The keeper's hot key, once recorded (`task contracts:solana:record-
    // keeper`): one a key can sign for, holding no other role.
    if roles.keeper.is_empty() || roles.keeper == Pubkey::default().to_string() {
        check(
            &mut checks,
            "the keeper: not recorded yet (task contracts:solana:record-keeper)",
            true,
            "",
        );
    } else {
        let signs = roles
            .keeper
            .parse::<Pubkey>()
            .is_ok_and(|keeper| keeper.is_on_curve());
        check(
            &mut checks,
            "the keeper is a key that signs and holds no other role",
            signs && ![&roles.admin, &roles.platform, &roles.deployer].contains(&&roles.keeper),
            roles.keeper.clone(),
        );
    }

    let failed = checks.iter().filter(|c| !c.ok).count();
    for c in &checks {
        println!(
            "  [{}] {}{}",
            if c.ok { "ok" } else { "FAILED" },
            c.what,
            if c.found.is_empty() {
                String::new()
            } else {
                format!(": {}", c.found)
            }
        );
    }
    if failed > 0 {
        bail!("{failed} of {} checks failed", checks.len());
    }
    println!("Solana mainnet-beta: all {} checks passed", checks.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> Flags {
        Flags {
            admin: Some(Keypair::new().pubkey().to_string()),
            ..Default::default()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shonin-mainnet-solana-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn without_the_sending_flags_the_solana_deploy_is_a_dry_run() {
        let decided = decide(&flags()).unwrap();
        assert_eq!(decided.mode, Mode::DryRun);
    }

    #[test]
    fn without_the_admin_s_address_nothing_runs_on_solana_mainnet() {
        for admin in [
            None,
            Some(String::new()),
            Some("0xabc".to_string()),
            Some(Pubkey::default().to_string()),
        ] {
            let mut given = flags();
            given.admin = admin;
            assert!(decide(&given).is_err());
            given.on_fork = true;
            assert!(decide(&given).is_err());
        }
    }

    #[test]
    fn a_solana_send_needs_the_key_file_and_a_confirmation_naming_mainnet_beta() {
        let mut given = flags();
        given.confirm_mainnet = Some(CONFIRMATION.to_string());
        assert!(decide(&given).is_err(), "no key file");
        let mut given = flags();
        given.deployer_keyfile = Some(PathBuf::from("deployer.json"));
        assert!(decide(&given).is_err(), "no confirmation");
        for wrong in ["yes", "solana", "8453", "mainnet", "devnet"] {
            given.confirm_mainnet = Some(wrong.to_string());
            assert!(decide(&given).is_err(), "{wrong}");
        }
        given.confirm_mainnet = Some(CONFIRMATION.to_string());
        assert_eq!(decide(&given).unwrap().mode, Mode::Send);
        given.on_fork = true;
        assert!(
            decide(&given).is_err(),
            "a fork flag beside the confirmation"
        );
    }

    #[test]
    fn a_solana_fork_rehearsal_signs_with_its_throwaway_deployer_alone() {
        let mut given = flags();
        given.on_fork = true;
        assert_eq!(decide(&given).unwrap().mode, Mode::Fork);
        given.deployer_keyfile = Some(PathBuf::from("deployer.json"));
        let error = decide(&given).unwrap_err().to_string();
        assert!(error.contains("throwaway"), "{error}");
    }

    #[test]
    fn a_solana_fork_rehearsal_refuses_a_public_endpoint_and_a_node_that_is_not_surfpool() {
        assert!(
            mainnet_guard::check_endpoints(
                Mode::Fork,
                "Solana",
                "https://api.mainnet-beta.solana.com"
            )
            .is_err()
        );
        assert!(
            mainnet_guard::check_endpoints(Mode::Fork, "Solana", "http://127.0.0.1:19210").is_ok()
        );
        let surfpool = json!({"surfnet-version": "1.6.0", "solana-core": "4.2.1"});
        let validator = json!({"solana-core": "3.0.6", "feature-set": 1});
        assert!(check_node_version(Mode::Fork, &surfpool).is_ok());
        assert!(check_node_version(Mode::Fork, &validator).is_err());
        assert!(check_node_version(Mode::Send, &surfpool).is_err());
        assert!(check_node_version(Mode::Send, &validator).is_ok());
        // A dry run reads mainnet: never a fork.
        assert!(check_node_version(Mode::DryRun, &surfpool).is_err());
        assert!(check_node_version(Mode::DryRun, &validator).is_ok());
        assert!(
            mainnet_guard::check_endpoints(Mode::Send, "Solana", "http://localhost:19210").is_err()
        );
    }

    #[test]
    fn a_missing_program_keypair_is_refused_naming_it() {
        let dir = scratch("missing");
        for program in SHIPPING.iter().filter(|p| p.name != "shonin_vault") {
            keys::write_keypair(
                &Keypair::new(),
                &dir.join(format!("{}-keypair.json", program.name)),
            )
            .unwrap();
        }
        let error = read_program_keypairs(&dir).unwrap_err().to_string();
        assert!(error.contains("shonin_vault-keypair.json"), "{error}");
        assert!(error.contains("Nothing was sent"), "{error}");
        keys::write_keypair(&Keypair::new(), &dir.join("shonin_vault-keypair.json")).unwrap();
        assert_eq!(read_program_keypairs(&dir).unwrap().len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A programs directory holding the eight programs (stand-in bytes) and
    /// the verifiable build's record of them, from `source`.
    fn verifiable_dir(name: &str, source: &str) -> PathBuf {
        let dir = scratch(name);
        let mut record = format!("source {source}\nimage {VERIFIABLE_IMAGE}\n");
        for program in SHIPPING {
            let code = format!("the verifiable build of {}", program.name);
            std::fs::write(dir.join(format!("{}.so", program.name)), &code).unwrap();
            record.push_str(&format!(
                "{} {}\n",
                program.name,
                sha256_hex(code.as_bytes())
            ));
        }
        std::fs::write(dir.join(VERIFIABLE_RECORD), record).unwrap();
        dir
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn the_deploy_sends_the_bytes_it_checked_whatever_the_build_directory_holds_after() {
        let dir = verifiable_dir("held", COMMIT);
        let build = check_verifiable_build(&dir, &BuildSource::Any).unwrap();
        let state = dir.join("state");
        let held = hold_checked_build(&dir, &build, &state).unwrap();
        assert_ne!(held, dir);
        // The build directory changes after the check: what is held doesn't.
        let checked = std::fs::read(dir.join("shonin_vault.so")).unwrap();
        std::fs::write(dir.join("shonin_vault.so"), b"swapped after the check").unwrap();
        assert_eq!(
            std::fs::read(held.join("shonin_vault.so")).unwrap(),
            checked
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&held), 0o700);
            assert_eq!(mode(&held.join("shonin_vault.so")), 0o400);
        }
        // Holding it again from the changed directory is refused.
        let error = hold_checked_build(&dir, &build, &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed after it was checked"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_verifiable_build_is_deployed_and_a_host_build_in_its_place_is_refused() {
        let dir = verifiable_dir("verifiable", COMMIT);
        let head = BuildSource::Head {
            head: COMMIT.to_string(),
            clean: true,
        };
        let build = check_verifiable_build(&dir, &head).unwrap();
        assert_eq!(build.source, COMMIT);
        assert_eq!(build.hashes.len(), 8);

        // A host build put in one program's place: its bytes aren't the
        // record's.
        std::fs::write(dir.join("shonin_vault.so"), b"a host cargo build-sbf").unwrap();
        for source in [BuildSource::Any, BuildSource::ACommit, head.clone()] {
            let error = check_verifiable_build(&dir, &source)
                .unwrap_err()
                .to_string();
            assert!(error.contains("shonin_vault.so is not the file"), "{error}");
        }

        // No record at all: programs from anywhere but the verifiable build.
        let bare = verifiable_dir("verifiable-bare", COMMIT);
        std::fs::remove_file(bare.join(VERIFIABLE_RECORD)).unwrap();
        let error = check_verifiable_build(&bare, &BuildSource::Any)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not the verifiable build"), "{error}");

        // Built in another image.
        let other = verifiable_dir("verifiable-image", COMMIT);
        let text = std::fs::read_to_string(other.join(VERIFIABLE_RECORD))
            .unwrap()
            .replace(
                VERIFIABLE_IMAGE,
                "solanafoundation/solana-verifiable-build:latest",
            );
        std::fs::write(other.join(VERIFIABLE_RECORD), text).unwrap();
        let error = check_verifiable_build(&other, &BuildSource::Any)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not the pinned image"), "{error}");

        // A program left out of the record.
        let short = verifiable_dir("verifiable-short", COMMIT);
        let text: String = std::fs::read_to_string(short.join(VERIFIABLE_RECORD))
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("shonin_station "))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(short.join(VERIFIABLE_RECORD), text).unwrap();
        let error = check_verifiable_build(&short, &BuildSource::Any)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not the eight programs"), "{error}");
        for dir in [dir, bare, other, short] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_real_send_deploys_the_checked_out_commit_s_verifiable_build_alone() {
        let tree = verifiable_dir("verifiable-tree", WORKING_TREE);
        // A rehearsal may build the working tree; a send and a read-back of
        // mainnet may not.
        assert!(check_verifiable_build(&tree, &BuildSource::Any).is_ok());
        for source in [
            BuildSource::ACommit,
            BuildSource::Head {
                head: COMMIT.to_string(),
                clean: true,
            },
        ] {
            let error = check_verifiable_build(&tree, &source)
                .unwrap_err()
                .to_string();
            assert!(error.contains("not a commit"), "{error}");
        }
        let built = verifiable_dir("verifiable-commit", COMMIT);
        let other_head = BuildSource::Head {
            head: "fedcba9876543210fedcba9876543210fedcba98".to_string(),
            clean: true,
        };
        let error = check_verifiable_build(&built, &other_head)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is checked out"), "{error}");
        let dirty = BuildSource::Head {
            head: COMMIT.to_string(),
            clean: false,
        };
        let error = check_verifiable_build(&built, &dirty)
            .unwrap_err()
            .to_string();
        assert!(error.contains("uncommitted"), "{error}");
        // The read-back of mainnet takes the commit's build, whatever is
        // checked out since (the mainnet ids are committed after it).
        assert!(check_verifiable_build(&built, &BuildSource::ACommit).is_ok());
        // A send's requirement comes from the repository; a rehearsal's
        // takes any source.
        assert!(matches!(
            build_source_for(Mode::Send).unwrap(),
            BuildSource::Head { .. }
        ));
        assert_eq!(build_source_for(Mode::Fork).unwrap(), BuildSource::Any);
        let _ = std::fs::remove_dir_all(tree);
        let _ = std::fs::remove_dir_all(built);
    }

    #[test]
    fn the_pinned_image_is_the_one_the_tasks_build_in() {
        let config = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../taskfiles/config.yml"),
        )
        .unwrap();
        assert!(
            config.contains(&format!("SOLANA_VERIFY_IMAGE: \"{VERIFIABLE_IMAGE}\"")),
            "taskfiles/config.yml's SOLANA_VERIFY_IMAGE is not {VERIFIABLE_IMAGE}"
        );
    }

    fn keypairs_in(dir: &Path) -> Vec<(&'static str, Pubkey)> {
        std::fs::create_dir_all(dir).unwrap();
        SHIPPING
            .iter()
            .map(|program| {
                let keypair = Keypair::new();
                keys::write_keypair(
                    &keypair,
                    &dir.join(format!("{}-keypair.json", program.name)),
                )
                .unwrap();
                (program.name, keypair.pubkey())
            })
            .collect()
    }

    /// The ids of the keypairs in `dir`, as the commands read them.
    fn ids_of(dir: &Path) -> Vec<(&'static str, Pubkey)> {
        read_program_keypairs(dir)
            .unwrap()
            .into_iter()
            .map(|(name, id, _)| (name, id))
            .collect()
    }

    #[test]
    fn a_fork_takes_only_the_throwaways_the_tool_made_and_a_send_never_takes_them() {
        let root = scratch("keypairs");
        let fork_root = root.join(".shonin/mainnet-fork");
        let sources = root.join("programs");
        std::fs::create_dir_all(&sources).unwrap();
        let throwaway = throwaway_keypairs_dir(&fork_root);
        make_throwaway_keypairs(&throwaway, &fork_root).unwrap();
        let throwaway_ids = ids_of(&throwaway);
        let owners = root.join("owner-keys");
        let owner_ids = keypairs_in(&owners);

        // A fork takes the tool's throwaways, however their directory is
        // spelled, and making them again changes none.
        check_program_keypairs(Mode::Fork, &throwaway, &throwaway_ids, &sources, &fork_root)
            .unwrap();
        let spelled =
            root.join("programs/../.shonin/mainnet-fork/solana/throwaway-program-keypairs");
        check_program_keypairs(Mode::Fork, &spelled, &throwaway_ids, &sources, &fork_root).unwrap();
        make_throwaway_keypairs(&throwaway, &fork_root).unwrap();
        assert_eq!(ids_of(&throwaway), throwaway_ids);
        // A directory anywhere else is the owner's, and refused on a fork;
        // the tool never makes its throwaways there either.
        let error = check_program_keypairs(Mode::Fork, &owners, &owner_ids, &sources, &fork_root)
            .unwrap_err()
            .to_string();
        assert!(error.contains("never come onto a fork"), "{error}");
        assert!(make_throwaway_keypairs(&root.join("elsewhere"), &fork_root).is_err());
        // Under the fork's directory, the owner's keypairs are refused
        // however they got there: linked in, hard-linked in, or copied in.
        for (how, place) in [("linked", 0), ("hard-linked", 1), ("copied", 2)] {
            let dir = fork_root.join(format!("solana/{how}"));
            std::fs::create_dir_all(&dir).unwrap();
            for program in SHIPPING {
                let name = format!("{}-keypair.json", program.name);
                let (from, to) = (owners.join(&name), dir.join(&name));
                match place {
                    0 => std::os::unix::fs::symlink(&from, &to).unwrap(),
                    1 => std::fs::hard_link(&from, &to).unwrap(),
                    _ => {
                        std::fs::copy(&from, &to).unwrap();
                    }
                }
            }
            // Making the throwaways leaves what is there as it is.
            make_throwaway_keypairs(&dir, &fork_root).unwrap();
            assert_eq!(ids_of(&dir), owner_ids, "{how}");
            let error = check_program_keypairs(Mode::Fork, &dir, &owner_ids, &sources, &fork_root)
                .unwrap_err()
                .to_string();
            assert!(error.contains("never come onto a fork"), "{how}: {error}");
        }
        // Even one of the tool's throwaways is refused once its id is
        // committed as a mainnet id: that is mainnet's own program.
        let held = sources.join("shonin-vault");
        std::fs::create_dir_all(&held).unwrap();
        let vault = throwaway_ids
            .iter()
            .find(|(name, _)| *name == "shonin_vault")
            .unwrap()
            .1;
        std::fs::write(held.join("mainnet-id"), format!("{vault}\n")).unwrap();
        let error =
            check_program_keypairs(Mode::Fork, &throwaway, &throwaway_ids, &sources, &fork_root)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("committed as shonin_vault's mainnet id"),
            "{error}"
        );
        std::fs::remove_file(held.join("mainnet-id")).unwrap();
        // A real send takes the owner's and never the fork's throwaways:
        // not their directory, not their files linked from elsewhere, and
        // not a copy of them.
        check_program_keypairs(Mode::Send, &owners, &owner_ids, &sources, &fork_root).unwrap();
        let error =
            check_program_keypairs(Mode::Send, &throwaway, &throwaway_ids, &sources, &fork_root)
                .unwrap_err()
                .to_string();
        assert!(error.contains("a real send takes the owner's"), "{error}");
        for (how, place) in [("linked", 0), ("copied", 2)] {
            let dir = root.join(format!("owner-{how}"));
            std::fs::create_dir_all(&dir).unwrap();
            for program in SHIPPING {
                let name = format!("{}-keypair.json", program.name);
                let (from, to) = (throwaway.join(&name), dir.join(&name));
                if place == 0 {
                    std::os::unix::fs::symlink(&from, &to).unwrap();
                } else {
                    std::fs::copy(&from, &to).unwrap();
                }
            }
            let error =
                check_program_keypairs(Mode::Send, &dir, &throwaway_ids, &sources, &fork_root)
                    .unwrap_err()
                    .to_string();
            assert!(
                error.contains("a real send takes the owner's"),
                "{how}: {error}"
            );
        }
        check_program_keypairs(Mode::DryRun, &owners, &owner_ids, &sources, &fork_root).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_dry_run_after_the_deploy_builds_with_the_committed_ids() {
        let root = scratch("committed-ids");
        let sources = root.join("programs");
        let mut committed = Vec::new();
        for program in SHIPPING {
            let id = Keypair::new().pubkey();
            let held = sources.join(program.name.replace('_', "-"));
            std::fs::create_dir_all(&held).unwrap();
            std::fs::write(held.join("mainnet-id"), format!("{id}\n")).unwrap();
            committed.push((program.name, id));
        }
        let out = root.join("ids");
        ids_from(
            Mode::DryRun,
            None,
            &sources,
            &root.join("stand-ins"),
            &out,
            &root.join(".shonin/mainnet-fork"),
        )
        .unwrap();
        let listed = std::fs::read_to_string(&out).unwrap();
        for (name, id) in committed {
            assert!(
                listed.contains(&format!("{name} {id}")),
                "the dry run listed a stand-in for {name} beside its committed id {id}:\n{listed}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_mode_reads_the_committed_ids_from_this_repository_s_programs_alone() {
        check_sources(&committed_sources()).unwrap();
        let spelled = committed_sources().join("../programs");
        check_sources(&spelled).unwrap();
        let elsewhere = scratch("sources");
        assert!(check_sources(&elsewhere).is_err());
        assert!(check_sources(&elsewhere.join("missing")).is_err());
        for mode in [Mode::DryRun, Mode::Fork, Mode::Send] {
            assert!(
                check_dirs(mode, &elsewhere, &elsewhere.join("steps")).is_err(),
                "{mode:?}"
            );
        }
        let _ = std::fs::remove_dir_all(elsewhere);
    }

    #[test]
    fn a_different_committed_mainnet_id_is_refused_and_the_same_one_is_kept() {
        let id = Pubkey::new_unique();
        assert!(!agrees_with_committed("shonin_registry", &id, None).unwrap());
        assert!(!agrees_with_committed("shonin_registry", &id, Some("  \n")).unwrap());
        assert!(agrees_with_committed("shonin_registry", &id, Some(&format!("{id}\n"))).unwrap());
        let other = Pubkey::new_unique().to_string();
        let error = agrees_with_committed("shonin_registry", &id, Some(&other))
            .unwrap_err()
            .to_string();
        assert!(error.contains("never replaced"), "{error}");
    }

    #[test]
    fn the_ids_step_refuses_a_different_committed_id_and_writes_no_committed_file() {
        let dir = scratch("ids");
        let keypairs = dir.join("keys");
        let sources = dir.join("programs");
        for program in SHIPPING {
            keys::write_keypair(
                &Keypair::new(),
                &keypairs.join(format!("{}-keypair.json", program.name)),
            )
            .unwrap();
            std::fs::create_dir_all(sources.join(program.name.replace('_', "-"))).unwrap();
        }
        // A dry run with the owner's keypairs (a fork takes throwaways from
        // under .shonin/mainnet-fork alone).
        let out = dir.join("ids");
        let fork_root = dir.join(".shonin/mainnet-fork");
        ids_from(
            Mode::DryRun,
            Some(&keypairs),
            &sources,
            &dir.join("stand-ins"),
            &out,
            &fork_root,
        )
        .unwrap();
        let listed = std::fs::read_to_string(&out).unwrap();
        assert_eq!(listed.lines().filter(|l| !l.starts_with('#')).count(), 8);
        assert!(
            SHIPPING
                .iter()
                .all(|p| !committed_file(&sources, p.name).exists())
        );
        std::fs::write(
            committed_file(&sources, "shonin_station"),
            Pubkey::new_unique().to_string(),
        )
        .unwrap();
        assert!(
            ids_from(
                Mode::DryRun,
                Some(&keypairs),
                &sources,
                &dir.join("stand-ins"),
                &out,
                &fork_root,
            )
            .is_err()
        );
        // To send, the keypairs must be given (a fork makes its own).
        assert!(
            ids_from(
                Mode::Send,
                None,
                &sources,
                &dir.join("stand-ins"),
                &out,
                &fork_root
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mainnet_plan_has_the_eight_programs_that_ship_and_no_development_program() {
        let names: Vec<&str> = SHIPPING.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), 8);
        for never in NEVER_ON_MAINNET {
            assert!(!names.contains(&never), "{never}");
        }
        for name in [
            "shonin_registry",
            "shonin_publishing",
            "shonin_prices",
            "shonin_profiles",
            "shonin_station",
            "shonin_vault",
            "shonin_cs_verifier",
            "shonin_orca_driver",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        // Every development program is one this build knows, so the list
        // can't drift from the programs that exist.
        let compiled: Vec<&str> = chain::solana::programs::compiled()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert!(
            compiled.contains(&"shonin_dev_verifier")
                && NEVER_ON_MAINNET.contains(&"shonin_dev_verifier")
        );
        // The checker and the driver go on final before anything trusts them.
        for name in ["shonin_cs_verifier", "shonin_orca_driver"] {
            let at = names.iter().position(|n| *n == name).unwrap();
            assert!(SHIPPING[at].final_at_once);
            assert!(at < names.iter().position(|n| *n == "shonin_registry").unwrap());
        }
        // No step names the development checker.
        assert!(setup_steps().iter().all(|s| !s.contains("development")));
    }

    #[test]
    fn the_cost_counts_the_kept_rent_the_peak_buffer_and_the_writes() {
        let planned = |name, size: u64, found| Planned {
            program: *SHIPPING.iter().find(|p| p.name == name).unwrap(),
            id: Pubkey::new_unique(),
            so: PathBuf::new(),
            size,
            rent: size * 10,
            buffer_rent: size * 10,
            writes: size.div_ceil(WRITE_CHUNK),
            found,
        };
        let all = [
            planned("shonin_registry", 2_000, Found::Nothing),
            planned("shonin_vault", 5_000, Found::Nothing),
        ];
        let c = cost(&all);
        assert_eq!(c.kept, 70_000);
        assert_eq!(c.peak, 20_000 + 50_000 + 50_000);
        assert_eq!(
            c.transactions,
            (2_000u64.div_ceil(WRITE_CHUNK) + 2)
                + (5_000u64.div_ceil(WRITE_CHUNK) + 2)
                + setup_steps().len() as u64
                + 5
        );
        let resumed = [
            planned("shonin_registry", 2_000, Found::Final),
            planned("shonin_vault", 5_000, Found::Nothing),
        ];
        assert_eq!(cost(&resumed).kept, 50_000);
    }

    #[test]
    fn the_mainnet_checker_trusts_the_root_alone_and_pins_the_guest() {
        assert_eq!(trusted_keys().len(), 1);
        assert_eq!(image_id(), shonin_attestation_deploy::GUEST_IMAGE_ID.0);
    }

    #[test]
    fn the_admin_is_the_platform_and_no_development_checker_is_named() {
        let admin = Pubkey::new_unique();
        let p = platform(admin);
        assert_eq!(p.admin, admin);
        assert_eq!(p.platform, admin);
        assert_eq!(p.mint, CIRCLE_MAINNET_USDC);
        assert!(p.checker.is_none());
        let deployer = Pubkey::new_unique();
        let r = roles(&deployer, &admin);
        assert_eq!(r.admin, admin.to_string());
        assert_eq!(r.keeper, Pubkey::default().to_string());
    }

    #[test]
    fn an_admin_no_key_can_sign_for_is_refused_in_every_mode() {
        let (pda, _) = Pubkey::find_program_address(&[b"config"], &registry::ID);
        assert!(!pda.is_on_curve());
        let mut given = flags();
        given.admin = Some(pda.to_string());
        let error = decide(&given).unwrap_err().to_string();
        assert!(error.contains("curve"), "{error}");
        given.on_fork = true;
        assert!(decide(&given).is_err());
        given.admin = Some(Keypair::new().pubkey().to_string());
        assert!(decide(&given).is_ok());
    }

    /// A set-up as this deploy makes it, for `admin`.
    fn setup_for(admin: Pubkey) -> SetupRead {
        let settings = Settings::mainnet(None).unwrap();
        let cluster = common::digest::cluster_value(CLUSTER);
        SetupRead {
            checker: Some(cs_verifier::Checker {
                image_id: image_id(),
                trusted_keys: trusted_keys(),
                bump: 255,
            }),
            checker_owner: Some(cs_verifier::ID),
            registry: Some(registry::state::Config {
                admin,
                cluster,
                min_timelock: settings.min_timelock,
                blessing_ttl: settings.blessing_ttl,
                stored_timelock: settings.timelock,
                pending_timelock_value: 0,
                pending_timelock_at: 0,
                verifiers: vec![registry::state::VerifierEntry {
                    program: cs_verifier::ID,
                    state: checker_state(),
                    ready_at: 0,
                }],
                bump: 255,
            }),
            publishing: Some(publishing::state::Config {
                mint: CIRCLE_MAINNET_USDC,
                token_program: chain::solana::programs::anchor_spl::token::ID,
                decimals: 6,
                platform: admin,
                fee: crate::solana::MAINNET_PUBLISH_FEE,
                count: 0,
                bump: 255,
            }),
            prices: Some(prices::PriceList {
                admin,
                notice: settings.price_notice,
                bump: 255,
                history: vec![prices::PricePeriod {
                    effective_from: 0,
                    prices: crate::solana::published_prices(),
                }],
            }),
            station: Some(station::state::Config {
                vault_program: vault::ID,
                mint: CIRCLE_MAINNET_USDC,
                cluster,
                bump: 255,
            }),
            factory: Some(vault::state::Factory {
                admin,
                platform: admin,
                mint: CIRCLE_MAINNET_USDC,
                token_program: chain::solana::programs::anchor_spl::token::ID,
                cluster,
                chain_name: chain::solana::name("solana"),
                registry: registry::ID,
                publishing: publishing::ID,
                prices: prices::ID,
                station_program: station::ID,
                previous: Pubkey::default(),
                terms: settings.terms,
                vault_count: 0,
                bump: 255,
            }),
        }
    }

    #[test]
    fn a_set_up_left_by_a_run_with_another_admin_is_refused_before_anything_is_sent() {
        let settings = Settings::mainnet(None).unwrap();
        let admin = Keypair::new().pubkey();
        // This run's own set-up, whole or in part, is resumed.
        assert!(
            refuse_another_setup(&setup_checks(&setup_for(admin), &admin, &settings, 1)).is_ok()
        );
        assert!(
            refuse_another_setup(&setup_checks(&SetupRead::default(), &admin, &settings, 1))
                .is_ok()
        );
        let part = SetupRead {
            registry: setup_for(admin).registry,
            ..SetupRead::default()
        };
        assert!(refuse_another_setup(&setup_checks(&part, &admin, &settings, 1)).is_ok());
        // An earlier run's registry, set up for another ADMIN: refused.
        let other = Keypair::new().pubkey();
        let left = SetupRead {
            registry: setup_for(other).registry,
            ..SetupRead::default()
        };
        let error = refuse_another_setup(&setup_checks(&left, &admin, &settings, 1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("registry admin is ADMIN"), "{error}");
        assert!(error.contains("no program was made final"), "{error}");
        // Each other account the set-up makes, holding another admin or
        // settings, is refused too.
        for change in [
            |r: &mut SetupRead, other| r.prices.as_mut().unwrap().admin = other,
            |r: &mut SetupRead, other| r.publishing.as_mut().unwrap().platform = other,
            |r: &mut SetupRead, other| r.factory.as_mut().unwrap().admin = other,
            |r: &mut SetupRead, other| r.factory.as_mut().unwrap().platform = other,
            |r: &mut SetupRead, _| r.publishing.as_mut().unwrap().fee += 1,
            |r: &mut SetupRead, _| r.registry.as_mut().unwrap().min_timelock = 0,
            |r: &mut SetupRead, _| r.registry.as_mut().unwrap().blessing_ttl += 1,
            |r: &mut SetupRead, _| r.prices.as_mut().unwrap().notice = 0,
            |r: &mut SetupRead, other| r.factory.as_mut().unwrap().previous = other,
            |r: &mut SetupRead, _| r.factory.as_mut().unwrap().chain_name = [0; 32],
            |r: &mut SetupRead, other| r.station.as_mut().unwrap().vault_program = other,
            |r: &mut SetupRead, _| r.checker.as_mut().unwrap().trusted_keys.push([7; 32]),
        ] {
            let mut read = setup_for(admin);
            change(&mut read, other);
            assert!(refuse_another_setup(&setup_checks(&read, &admin, &settings, 1)).is_err());
        }
    }

    #[test]
    fn nothing_is_made_final_until_every_set_up_account_reads_back_as_this_deploy_s() {
        let settings = Settings::mainnet(None).unwrap();
        let admin = Keypair::new().pubkey();
        assert!(require_setup(&setup_checks(&setup_for(admin), &admin, &settings, 1)).is_ok());
        let mut missing = setup_for(admin);
        missing.factory = None;
        let error = require_setup(&setup_checks(&missing, &admin, &settings, 1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("vault factory"), "{error}");
        let other = setup_for(Keypair::new().pubkey());
        assert!(require_setup(&setup_checks(&other, &admin, &settings, 1)).is_err());
    }

    #[test]
    fn the_cli_s_output_never_shows_a_seed_phrase_or_the_endpoint() {
        let rpc = "https://example.invalid/key-abc123";
        let text = "Error: Data writes to account failed\n\
            ==================================================================================\n\
            Recover the intermediate account's ephemeral keypair file with\n\
            `solana-keygen recover` and the following 12-word seed phrase:\n\
            ==================================================================================\n\
            abandon ability able about above absent absorb abstract absurd abuse access accident\n\
            ==================================================================================\n\
            sent to https://example.invalid/key-abc123/ and failed\n";
        let shown = redact_cli_output(text, rpc);
        assert!(!shown.contains("abandon"), "{shown}");
        assert!(!shown.contains("key-abc123"), "{shown}");
        assert!(shown.contains("Data writes to account failed"), "{shown}");
        assert!(shown.contains("withheld"), "{shown}");
    }

    #[test]
    fn the_cli_s_output_never_shows_the_endpoint_in_the_form_a_url_parser_prints() {
        for (printed, rpc) in [
            (
                "error sending request for url (https://rpc.example.com/?api-key=SECRET)",
                "https://rpc.example.com?api-key=SECRET",
            ),
            (
                "error sending request for url (https://rpc.example.com/?api-key=SECRET)",
                "https://RPC.Example.com?api-key=SECRET",
            ),
            (
                "error sending request for url (https://rpc.example.com/v2/SECRET)",
                "https://RPC.example.com/v2/SECRET",
            ),
            (
                "error sending request for url (https://user:SECRET@rpc.example.com/)",
                "https://user:SECRET@RPC.example.com",
            ),
            (
                "request to ?api-key=SECRET failed",
                "https://rpc.example.com/?api-key=SECRET",
            ),
        ] {
            let shown = redact_cli_output(printed, rpc);
            assert!(!shown.contains("SECRET"), "{rpc}: {shown}");
        }
        // Ordinary words survive an endpoint with no key.
        let shown = redact_cli_output("connection refused", "http://127.0.0.1:8899");
        assert!(shown.contains("connection refused"), "{shown}");
    }

    #[test]
    fn the_cli_reads_its_endpoint_from_a_file_only_its_owner_reads_and_removed_after() {
        let dir = scratch("cli");
        let rpc = "https://example.invalid/key-abc123";
        let cli = Cli::new(&dir, rpc, &dir.join("deployer.json"), Mode::Send).unwrap();
        let config = cli.config.clone();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains(&format!("json_rpc_url: \"{rpc}\"")), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&config).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!cli.use_rpc);
        drop(cli);
        assert!(!config.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_program_resumes_on_its_own_kept_buffer_and_any_other_is_closed() {
        let dir = scratch("buffers");
        let vault = buffer_keypair(&dir, "shonin_vault").unwrap();
        // The same buffer on every run, written for its owner alone.
        assert_eq!(buffer_keypair(&dir, "shonin_vault").unwrap(), vault);
        let station = buffer_keypair(&dir, "shonin_station").unwrap();
        assert_ne!(vault, station);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = buffer_file(&dir, "shonin_vault");
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let stray = Pubkey::new_unique();
        let listed = parse_buffers(
            &json!({"buffers": [
                {"address": vault.to_string(), "authority": "x", "dataLen": 10, "lamports": 5},
                {"address": stray.to_string(), "authority": "x", "dataLen": 10, "lamports": 7}
            ]})
            .to_string(),
        )
        .unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            stray_buffers(&listed, &[vault, station]),
            vec![Buffer {
                address: stray,
                lamports: 7
            }]
        );
        assert!(parse_buffers(r#"{"buffers": []}"#).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_build_made_for_another_id_or_not_for_mainnet_is_refused() {
        let id = Pubkey::new_unique();
        let mut code = vec![1u8; 300];
        code[100..132].copy_from_slice(id.as_ref());
        assert!(check_build("shonin_registry", &id, &code).is_ok());
        assert!(check_build("shonin_registry", &Pubkey::new_unique(), &code).is_err());
        // Orca's driver lists mainnet's pool only in the mainnet build.
        let driver = Pubkey::new_unique();
        let mut code = vec![0u8; 300];
        code[0..32].copy_from_slice(driver.as_ref());
        assert!(check_build("shonin_orca_driver", &driver, &code).is_err());
        let pool: Pubkey = MAINNET_ORCA_POOL.parse().unwrap();
        code[200..232].copy_from_slice(pool.as_ref());
        assert!(check_build("shonin_orca_driver", &driver, &code).is_ok());
    }

    #[test]
    fn a_real_send_writes_mainnet_ids_under_this_repository_s_programs_alone() {
        let dir = scratch("dirs");
        let steps = dir.join("admin-steps");
        assert!(check_dirs(Mode::Send, &committed_sources(), &steps).is_ok());
        assert!(check_dirs(Mode::Send, &dir, &steps).is_err());
        assert!(check_dirs(Mode::Fork, &dir, &steps).is_err());
        assert!(check_dirs(Mode::Fork, &committed_sources(), &steps).is_ok());
        let dir_ = dir.clone();
        let dir = committed_sources();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // A fork's admin steps never where the hardware key's are kept, by
        // any spelling, and the hardware key's never from the fork's.
        let real = root.join(".shonin/mainnet/admin-steps");
        assert!(check_dirs(Mode::Fork, &dir, &real).is_err());
        assert!(
            check_dirs(
                Mode::Fork,
                &dir,
                &root.join(".shonin/mainnet-fork/../mainnet/admin-steps")
            )
            .is_err()
        );
        assert!(check_dirs(Mode::Fork, &dir, &root.join("config/deployments/mainnet")).is_err());
        assert!(
            check_dirs(
                Mode::DryRun,
                &dir,
                &root.join(".shonin/mainnet-fork/admin-steps")
            )
            .is_err()
        );
        assert!(check_dirs(Mode::DryRun, &dir, &real).is_ok());
        let _ = std::fs::remove_dir_all(&dir_);
    }
}
