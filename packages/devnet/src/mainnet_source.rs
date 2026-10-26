//! Source verification of the EVM mainnet deploy: every contract it
//! created, published with its source on each chain's explorers.
//!
//! What is submitted is worked out from the deployment file and this build
//! alone: the file's admin and generation are held to the `ADMIN` and
//! `GENERATION` given, its build to this build, and each address it records
//! to where this build's plan puts that contract. Each creation of the plan
//! (the platform's contracts, the vault's libraries, the checker and its
//! RISC Zero verifier, the driver, and the contracts at one address on every
//! chain) is then submitted with the compiler settings its build artifact
//! records, the libraries linked into its creation code and the constructor
//! arguments appended to it. Nothing is typed by hand.
//!
//! Three modes. Alone it is a dry run: it prints every submission (address,
//! contract, compiler, libraries, constructor arguments, and the command it
//! would run) and sends nothing to any explorer. `ON_FORK=1` reads the
//! rehearsal's deployment file, checks each address has code on the local
//! fork, and is a dry run only (an explorer can't see a fork). `SUBMIT=1`
//! submits, for the committed deployment file alone, after checking each
//! address has code on the mainnet, the source of the commit checked out
//! (a clean export of it and of its submodules, never the working tree); a contract an explorer already shows
//! verified is skipped there (`forge verify-contract` asks first), so it is
//! safe to run again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use alloy::primitives::Address;
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfig, ChainKey, Deployments, NetworkName, Signing};
use serde::Deserialize;

use crate::deploy::{self, Creation};
use crate::mainnet_evm::{self, ChainPlan, MAINNET_KEY, MainnetRecord, SHIPPING, Venues};
use crate::mainnet_guard::{self, Mode};

/// Etherscan's v2 API: one key, every chain it serves, by `chainid`.
pub const ETHERSCAN_V2: &str = "https://api.etherscan.io/v2/api";

/// The variable the Etherscan key is passed in. It is read by `forge`
/// from its environment, never put on a command line or in a URL, and
/// taken out of everything printed.
pub const ETHERSCAN_KEY_VAR: &str = "ETHERSCAN_API_KEY";

/// An explorer a chain's contracts are verified on, as
/// `config/mainnet.toml` lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Explorer {
    /// Etherscan's v2 API for the chain: needs `ETHERSCAN_API_KEY`.
    Etherscan,
    /// Sourcify, keyless; Blockscout explorers show what it has verified.
    Sourcify,
}

impl Explorer {
    pub fn name(self) -> &'static str {
        match self {
            Explorer::Etherscan => "etherscan",
            Explorer::Sourcify => "sourcify",
        }
    }

    /// The `forge verify-contract` arguments naming this explorer for
    /// chain `chain_id`. No key: `forge` reads it from its environment.
    pub fn forge_args(self, chain_id: u64) -> Vec<String> {
        match self {
            Explorer::Etherscan => vec![
                "--verifier".into(),
                "etherscan".into(),
                "--verifier-url".into(),
                format!("{ETHERSCAN_V2}?chainid={chain_id}"),
            ],
            Explorer::Sourcify => vec!["--verifier".into(), "sourcify".into()],
        }
    }
}

/// `config/mainnet.toml`'s explorers, by chain id (read beside the
/// venues, which ignore them).
#[derive(Debug, Deserialize)]
struct ExplorersFile {
    evm: ExplorersEvm,
}

#[derive(Debug, Deserialize)]
struct ExplorersEvm {
    chains: BTreeMap<String, ExplorersChain>,
}

#[derive(Debug, Deserialize)]
struct ExplorersChain {
    #[serde(default)]
    explorers: Vec<Explorer>,
}

/// The explorers `config/mainnet.toml` lists for each chain; a chain with
/// none is refused when it is verified.
pub fn read_explorers(text: &str) -> Result<BTreeMap<u64, Vec<Explorer>>> {
    let file: ExplorersFile = toml::from_str(text).context("parsing the explorers")?;
    file.evm
        .chains
        .into_iter()
        .map(|(id, chain)| {
            let id: u64 = id.parse().map_err(|_| anyhow!("{id} is not a chain id"))?;
            Ok((id, chain.explorers))
        })
        .collect()
}

/// One contract's submission, as the explorer is given it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub chain_id: u64,
    pub name: &'static str,
    pub address: Address,
    /// `<source path>:<contract>`, relative to the Foundry project.
    pub contract: String,
    /// solc's long version, `v` first, as Etherscan names it.
    pub compiler: String,
    pub optimizer_runs: Option<u64>,
    pub via_ir: bool,
    pub evm_version: String,
    /// Each library linked into the creation code: `<source path>:<name>`
    /// and the address linked.
    pub libraries: Vec<(String, Address)>,
    pub constructor_args: Vec<u8>,
}

impl Submission {
    /// The `forge verify-contract` command line for one explorer, from the
    /// Foundry project at `root`. Never holds a key.
    pub fn forge_args(&self, root: &Path, explorer: Explorer) -> Vec<String> {
        let mut args = vec![
            "verify-contract".to_string(),
            self.address.to_string(),
            self.contract.clone(),
            "--root".into(),
            root.display().to_string(),
            "--chain".into(),
            self.chain_id.to_string(),
        ];
        args.extend(explorer.forge_args(self.chain_id));
        args.extend(["--compiler-version".into(), self.compiler.clone()]);
        if let Some(runs) = self.optimizer_runs {
            args.extend(["--num-of-optimizations".into(), runs.to_string()]);
        }
        if self.via_ir {
            args.push("--via-ir".into());
        }
        args.extend(["--evm-version".into(), self.evm_version.clone()]);
        if !self.constructor_args.is_empty() {
            args.extend([
                "--constructor-args".into(),
                format!("0x{}", alloy::hex::encode(&self.constructor_args)),
            ]);
        }
        for (library, address) in &self.libraries {
            args.extend(["--libraries".into(), format!("{library}:{address}")]);
        }
        args.push("--watch".into());
        args
    }
}

/// What a build artifact records of how its contract was compiled.
struct Compiled {
    contract: String,
    compiler: String,
    optimizer_runs: Option<u64>,
    via_ir: bool,
    evm_version: String,
    /// The creation code's length, before any constructor arguments.
    code_len: usize,
    /// Each library and where its address sits in the creation code.
    links: Vec<(String, usize)>,
}

fn compiled(artifacts: &Path, name: &str) -> Result<Compiled> {
    let path = artifacts
        .join(format!("{name}.sol"))
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}. Run the contracts' build first: `task contracts:build`",
            path.display()
        )
    })?;
    let artifact: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let metadata = &artifact["metadata"];
    let settings = &metadata["settings"];
    let targets = settings["compilationTarget"]
        .as_object()
        .ok_or_else(|| anyhow!("{name}'s artifact names no compilation target"))?;
    let [(source, contract)] = targets.iter().collect::<Vec<_>>()[..] else {
        bail!(
            "{name}'s artifact names {} compilation targets",
            targets.len()
        );
    };
    let contract = contract
        .as_str()
        .ok_or_else(|| anyhow!("{name}'s compilation target is not a name"))?;
    if contract != name {
        bail!("{name}'s artifact is {contract}'s");
    }
    let compiler = metadata["compiler"]["version"]
        .as_str()
        .ok_or_else(|| anyhow!("{name}'s artifact names no compiler"))?;
    let optimizer = &settings["optimizer"];
    let optimizer_runs = (optimizer["enabled"].as_bool() == Some(true))
        .then(|| optimizer["runs"].as_u64())
        .flatten();
    let code = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?
        .trim_start_matches("0x");
    let mut links = Vec::new();
    if let Some(references) = artifact["bytecode"]["linkReferences"].as_object() {
        for (file, libraries) in references {
            for (library, places) in libraries.as_object().into_iter().flatten() {
                let first = places
                    .as_array()
                    .and_then(|places| places.first())
                    .and_then(|place| place["start"].as_u64())
                    .ok_or_else(|| anyhow!("{name} links {library} nowhere"))?;
                links.push((format!("{file}:{library}"), first as usize));
            }
        }
    }
    Ok(Compiled {
        contract: format!("{source}:{contract}"),
        compiler: format!("v{}", compiler.trim_start_matches('v')),
        optimizer_runs,
        via_ir: settings["viaIR"].as_bool().unwrap_or(false),
        evm_version: settings["evmVersion"]
            .as_str()
            .ok_or_else(|| anyhow!("{name}'s artifact names no EVM version"))?
            .to_string(),
        code_len: code.len() / 2,
        links,
    })
}

/// A creation's submission: its build's settings, the libraries its
/// creation code links (read from the code itself) and the arguments
/// appended to it.
fn submission(artifacts: &Path, chain_id: u64, creation: &Creation) -> Result<Submission> {
    let built = compiled(artifacts, creation.name)?;
    if creation.code.len() < built.code_len {
        bail!(
            "{}'s creation code is shorter than its build's",
            creation.name
        );
    }
    let mut libraries = Vec::new();
    for (library, at) in built.links {
        let address = creation
            .code
            .get(at..at + 20)
            .map(Address::from_slice)
            .ok_or_else(|| anyhow!("{} links {library} past its code", creation.name))?;
        libraries.push((library, address));
    }
    libraries.sort();
    Ok(Submission {
        chain_id,
        name: creation.name,
        address: creation.address,
        contract: built.contract,
        compiler: built.compiler,
        optimizer_runs: built.optimizer_runs,
        via_ir: built.via_ir,
        evm_version: built.evm_version,
        libraries,
        constructor_args: creation.code[built.code_len..].to_vec(),
    })
}

/// Everything one chain's deployment file names, checked against this
/// build's plan for `admin` and `generation`, and every creation of that
/// plan as it is submitted. Refused when the file records another admin,
/// generation or build, or any address but the plan's: the explorer would
/// be given source that isn't what is on the chain.
pub fn submissions(
    artifacts: &Path,
    venues: &Venues,
    chain_id: u64,
    text: &str,
    admin: Address,
    generation: u64,
    names: &[(String, u64)],
) -> Result<(ChainPlan, Vec<Submission>)> {
    let deployments = Deployments::from_json(text).context("not a deployment file")?;
    let value: serde_json::Value = serde_json::from_str(text)?;
    let record: MainnetRecord = serde_json::from_value(
        value
            .get(MAINNET_KEY)
            .cloned()
            .ok_or_else(|| anyhow!("the deployment file has no mainnet record"))?,
    )
    .context("reading the deployment file's mainnet record")?;
    if deployments.chain_id != chain_id {
        bail!(
            "the deployment file is chain {}'s, not {chain_id}'s",
            deployments.chain_id
        );
    }
    if deployments.roles.admin != admin {
        bail!(
            "the deployment file's admin is {}, not ADMIN {admin}",
            deployments.roles.admin
        );
    }
    let recorded_generation = deploy::recorded_generation(text);
    if recorded_generation != Some(generation) {
        bail!("the deployment file's generation is {recorded_generation:?}, not {generation}");
    }
    let build = deploy::build_of_these(artifacts, &SHIPPING)?;
    if record.build != build {
        bail!(
            "the deployment file records build {}, and this checkout's contracts are build \
             {build}: verify from the commit that deployed them",
            record.build
        );
    }
    let plan = mainnet_evm::plan_chain(artifacts, venues, chain_id, admin, generation, names)?;
    let p = &plan.platform;
    let station_implementation: Option<Address> = value
        .get(deploy::STATION_IMPLEMENTATION_KEY)
        .and_then(|at| serde_json::from_value(at.clone()).ok());
    let recorded: [(&str, Address, Address); 15] = [
        ("RiscZeroGroth16Verifier", record.risc_zero, plan.risc_zero),
        (
            "ConfidentialSpaceVerifier",
            deployments.attestation_verifier,
            plan.checker,
        ),
        (
            "AttestationRegistry",
            deployments.attestation,
            p.attestation,
        ),
        ("Publishing", deployments.publishing, p.publishing),
        ("Prices", deployments.prices, p.prices),
        ("StrategyRegistry", deployments.strategies, p.strategies),
        ("ReviewRegistry", deployments.reviews, p.reviews),
        ("AuthorProfiles", deployments.profiles, p.profiles),
        (
            "StationFactory",
            deployments.station_factory,
            p.station_factory,
        ),
        (
            "Station",
            station_implementation.unwrap_or_default(),
            p.station_implementation,
        ),
        ("Closer", deployments.closer, p.closer),
        ("Vault", record.vault_implementation, p.vault_implementation),
        ("VaultFactory", deployments.factory, p.factory),
        (
            plan.driver.contract,
            record
                .drivers
                .get(&plan.driver.name)
                .copied()
                .unwrap_or_default(),
            plan.driver.address,
        ),
        ("dollar", deployments.usd, plan.dollar),
    ];
    for (name, at, planned) in recorded {
        if at != planned {
            bail!(
                "the deployment file records {name} at {at}, and this build puts it at {planned} \
                 for ADMIN {admin} under generation {generation}"
            );
        }
    }
    if record.drivers.len() != 1 {
        bail!(
            "the deployment file records {} drivers, and the plan has one",
            record.drivers.len()
        );
    }
    let mut out = Vec::new();
    for creation in &plan.creations {
        out.push(submission(artifacts, chain_id, creation)?);
    }
    Ok((plan, out))
}

/// The record `dir` holds for `key`, read; on a real submission only the
/// committed file, unchanged from the commit checked out.
fn read_record(mode: Mode, submit: bool, dir: &Path, key: &ChainKey) -> Result<(PathBuf, String)> {
    let dir = mainnet_guard::deployments_dir(if submit { Mode::Send } else { mode }, dir)?;
    let file = Deployments::file_in(&dir, key);
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    if submit {
        committed_unchanged(&file)?;
    }
    Ok((file, text))
}

/// Refuse a deployment file that isn't committed exactly as it is on
/// disk: the explorers are given what the committed record says.
fn committed_unchanged(file: &Path) -> Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .arg(file)
            .output()
            .with_context(|| "running git")
    };
    let tracked = run(&["ls-files", "--error-unmatch", "--"])?;
    let unchanged = run(&["diff", "--quiet", "HEAD", "--"])?;
    if !tracked.status.success() || !unchanged.status.success() {
        bail!(
            "{} is not committed as it is: commit the deployment file before verifying its \
             contracts. Nothing was submitted.",
            file.display()
        );
    }
    Ok(())
}

/// Run git in `dir` and return what it printed, refusing a failure.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} in {} failed: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Write `commit`'s tree of the repository at `repo` (only `prefix`, if
/// given) under `into`, then each submodule it records at the commit it
/// records, recursively. A submodule never checked out here (no part of
/// any build) is left out.
fn export_tree(repo: &Path, commit: &str, prefix: &str, into: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    std::fs::create_dir_all(into).with_context(|| format!("making {}", into.display()))?;
    let mut archive = Command::new("git");
    archive
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", commit]);
    if !prefix.is_empty() {
        archive.args(["--", prefix]);
    }
    let mut archive = archive
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git archive")?;
    let unpacked = Command::new("tar")
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(into)
        .stdin(archive.stdout.take().expect("piped"))
        .status()
        .context("running tar")?;
    let archived = archive.wait_with_output().context("running git archive")?;
    if !archived.status.success() || !unpacked.success() {
        bail!(
            "exporting {commit} of {}: {}",
            repo.display(),
            String::from_utf8_lossy(&archived.stderr).trim()
        );
    }
    let mut listing = vec!["ls-tree", "-r", "-z", commit];
    if !prefix.is_empty() {
        listing.extend(["--", prefix]);
    }
    for entry in git(repo, &listing)?.split('\0').filter(|e| !e.is_empty()) {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split_whitespace();
        let (_, kind, sha) = (meta.next(), meta.next(), meta.next());
        if kind != Some("commit") {
            continue;
        }
        let sha = sha.ok_or_else(|| anyhow!("git ls-tree gave no commit for {path}"))?;
        let submodule = repo.join(path);
        if !submodule.join(".git").exists() {
            continue;
        }
        export_tree(&submodule, sha, "", &into.join(path))?;
    }
    Ok(())
}

/// A clean export of the commit checked out, for the Foundry project at
/// `contracts_root`: its files as `HEAD` holds them, and every submodule's
/// as `HEAD` records it, under `into` (emptied first), never the working
/// tree's. Returns the commit and the project's root in the export, which
/// is what a submission publishes as the source.
pub fn export_head(contracts_root: &Path, into: &Path) -> Result<(String, PathBuf)> {
    let top = PathBuf::from(git(contracts_root, &["rev-parse", "--show-toplevel"])?.trim());
    let prefix = git(contracts_root, &["rev-parse", "--show-prefix"])?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let commit = git(&top, &["rev-parse", "HEAD"])?.trim().to_string();
    if into.exists() {
        std::fs::remove_dir_all(into).with_context(|| format!("emptying {}", into.display()))?;
    }
    export_tree(&top, &commit, &prefix, into)?;
    Ok((commit, into.join(&prefix)))
}

/// Everything a source verification run needs.
pub struct Inputs<'a> {
    pub chains: Vec<(ChainKey, ChainConfig)>,
    pub names: Vec<(String, u64)>,
    pub artifacts: &'a Path,
    pub venues: &'a Venues,
    pub explorers: BTreeMap<u64, Vec<Explorer>>,
    pub deployments_dir: &'a Path,
    /// The Foundry project the sources are in.
    pub contracts_root: &'a Path,
    pub flags: mainnet_guard::Flags,
    pub submit: bool,
}

/// The mode a run is in, refused before anything is read: it never sends a
/// transaction, so a deployer's key or a send's confirmation is refused,
/// and a fork's contracts are never submitted.
pub fn decide(
    flags: &mainnet_guard::Flags,
    submit: bool,
    ids: &[u64],
) -> Result<mainnet_guard::Decided> {
    if flags.deployer_keyfile.is_some() || flags.confirm_mainnet.is_some() {
        bail!(
            "source verification sends no transaction: DEPLOYER_KEYFILE and CONFIRM_MAINNET are \
             refused (SUBMIT=1 submits to the explorers). Nothing was submitted."
        );
    }
    let decided = mainnet_guard::decide(flags, ids)?;
    if submit && decided.mode == Mode::Fork {
        bail!(
            "ON_FORK=1 with SUBMIT=1: an explorer can't see a fork, so a rehearsal's contracts \
             are never submitted. Nothing was submitted."
        );
    }
    Ok(decided)
}

/// Run a source verification: a dry run, a fork's dry run, or a
/// submission, as the flags decide.
pub async fn run(inputs: Inputs<'_>) -> Result<()> {
    let ids: Vec<u64> = inputs
        .chains
        .iter()
        .filter_map(|(key, _)| key.evm_chain_id())
        .collect();
    let decided = decide(&inputs.flags, inputs.submit, &ids)?;
    let key_value = std::env::var(ETHERSCAN_KEY_VAR)
        .ok()
        .filter(|key| !key.trim().is_empty());
    let mut planned = Vec::new();
    for (key, config) in &inputs.chains {
        let id = key
            .evm_chain_id()
            .ok_or_else(|| anyhow!("{key} is not an EVM chain"))?;
        let explorers = inputs
            .explorers
            .get(&id)
            .filter(|explorers| !explorers.is_empty())
            .ok_or_else(|| anyhow!("config/mainnet.toml lists no explorer for chain {id}"))?
            .clone();
        if inputs.submit && explorers.contains(&Explorer::Etherscan) && key_value.is_none() {
            bail!(
                "{ETHERSCAN_KEY_VAR} is empty in .env, and {} is verified on Etherscan. Nothing \
                 was submitted.",
                config.label
            );
        }
        let (file, text) = read_record(decided.mode, inputs.submit, inputs.deployments_dir, key)?;
        let (_, submissions) = submissions(
            inputs.artifacts,
            inputs.venues,
            id,
            &text,
            decided.admin,
            decided.generation,
            &inputs.names,
        )
        .with_context(|| format!("{}: {}", config.label, file.display()))?;
        planned.push((key.clone(), config.clone(), file, explorers, submissions));
    }

    // Every address has code where it was deployed: the fork in a
    // rehearsal, the mainnet otherwise. A dry run that can't reach the
    // mainnet says so and goes on; a submission stops.
    for (key, config, _, _, submissions) in &planned {
        let id = key.evm_chain_id().unwrap_or_default();
        let read_mode = if decided.mode == Mode::Fork {
            Mode::Fork
        } else {
            Mode::Send
        };
        let read = async {
            mainnet_guard::check_chain(read_mode, config, id).await?;
            let client =
                chain::connect_with(key, config, NetworkName::Mainnet, Signing::ReadOnly).await?;
            mainnet_guard::check_chain_id(&config.label, id, client.chain_id)?;
            let mut missing = Vec::new();
            for submission in submissions {
                if !client.has_code(submission.address).await? {
                    missing.push(format!("{} at {}", submission.name, submission.address));
                }
            }
            anyhow::Ok(missing)
        };
        match read.await {
            Ok(missing) if missing.is_empty() => println!(
                "{}: all {} contracts have code{}",
                config.label,
                submissions.len(),
                if decided.mode == Mode::Fork {
                    " on the fork"
                } else {
                    ""
                }
            ),
            Ok(missing) => bail!(
                "{}: no code at {}. Nothing was submitted.",
                config.label,
                missing.join(", ")
            ),
            Err(error) if !inputs.submit && decided.mode == Mode::DryRun => {
                println!("{}: the chain was not read: {error:#}", config.label)
            }
            Err(error) => return Err(error.context(format!("{}: reading the chain", config.label))),
        }
    }

    // A submission publishes the commit's source, from a clean export of
    // it, never the working tree's: an edit that leaves the bytecode as it
    // is (a comment, a name) would otherwise be published as the source.
    let source_root = if inputs.submit {
        let into = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(".shonin")
            .join("mainnet")
            .join("verify-source")
            .join("export");
        let (commit, root) = export_head(inputs.contracts_root, &into)?;
        println!(
            "Submitting the source of commit {commit}, exported clean (never the working tree)."
        );
        root
    } else {
        inputs.contracts_root.to_path_buf()
    };
    for (key, config, file, explorers, submissions) in &planned {
        println!(
            "\n{} ({key}), from {}: {} contracts, on {}",
            config.label,
            file.display(),
            submissions.len(),
            explorers
                .iter()
                .map(|e| e.name())
                .collect::<Vec<_>>()
                .join(" and ")
        );
        for submission in submissions {
            print_submission(submission);
            for explorer in explorers {
                let args = submission.forge_args(&source_root, *explorer);
                if !inputs.submit {
                    println!("      {}: forge {}", explorer.name(), args.join(" "));
                    continue;
                }
                submit(&args, explorer.name(), submission, key_value.as_deref())?;
            }
        }
    }
    match (inputs.submit, decided.mode) {
        (true, _) => println!("\nevery contract is verified on every explorer listed"),
        (false, Mode::Fork) => println!(
            "\nA fork's dry run: nothing was submitted (an explorer can't see a fork). On the \
             mainnet, SUBMIT=1 submits each contract above, from the committed deployment file."
        ),
        (false, _) => println!(
            "\nA dry run: nothing was submitted. SUBMIT=1 submits each contract above \
             ({ETHERSCAN_KEY_VAR} from .env for Etherscan); one already verified is skipped."
        ),
    }
    Ok(())
}

fn print_submission(submission: &Submission) {
    println!("  {:<26} {}", submission.name, submission.address);
    println!("      contract   {}", submission.contract);
    println!(
        "      compiler   {} (optimizer {}, via IR {}, EVM {})",
        submission.compiler,
        submission
            .optimizer_runs
            .map(|runs| format!("{runs} runs"))
            .unwrap_or_else(|| "off".into()),
        submission.via_ir,
        submission.evm_version
    );
    for (library, address) in &submission.libraries {
        println!("      library    {library} at {address}");
    }
    if submission.constructor_args.is_empty() {
        println!("      arguments  none");
    } else {
        println!(
            "      arguments  0x{}",
            alloy::hex::encode(&submission.constructor_args)
        );
    }
}

/// `text` with every appearance of `secret` taken out.
pub fn redacted(text: &str, secret: Option<&str>) -> String {
    let Some(secret) = secret.filter(|secret| !secret.trim().is_empty()) else {
        return text.to_string();
    };
    // Every form a tool may print it in: as given, trimmed, and as a URL
    // carries it (percent-encoded), longest first.
    let trimmed = secret.trim();
    let encoded: String = trimmed
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    let mut forms = vec![
        secret.to_string(),
        trimmed.to_string(),
        encoded.clone(),
        encoded.to_lowercase(),
    ];
    forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
    forms.dedup();
    let mut out = text.to_string();
    for form in forms {
        out = out.replace(&form, "<redacted>");
    }
    out
}

/// Submit one contract to one explorer through `forge verify-contract`,
/// which asks the explorer first and skips one already verified. Its output
/// is printed with the key taken out.
fn submit(
    args: &[String],
    explorer: &str,
    submission: &Submission,
    key: Option<&str>,
) -> Result<()> {
    println!("      {explorer}: submitting");
    let output = std::process::Command::new("forge")
        .args(args)
        .env_remove("VERIFIER_API_KEY")
        .output()
        .context("running forge verify-contract")?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for line in redacted(&said, key)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        println!("        {line}");
    }
    if !output.status.success() {
        bail!(
            "{explorer} did not verify {} at {} on chain {}; run again to carry on (verified \
             ones are skipped)",
            submission.name,
            submission.address,
            submission.chain_id
        );
    }
    Ok(())
}

/// The record's build, for a test's file.
#[cfg(test)]
fn build(artifacts: &Path) -> Result<alloy::primitives::B256> {
    deploy::build_of_these(artifacts, &SHIPPING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use std::collections::BTreeMap as Map;

    fn artifacts() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
        // The contracts are built by `task contracts:build`; without them
        // there is nothing to plan from.
        dir.join("Vault.sol/Vault.json").exists().then_some(dir)
    }

    fn venues() -> Venues {
        Venues::read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/mainnet.toml"))
            .unwrap()
    }

    fn names() -> Vec<(String, u64)> {
        vec![
            ("base".into(), 8453),
            ("hyperliquid".into(), 999),
            ("robinhood".into(), 4663),
        ]
    }

    const ADMIN: Address = Address::new([0x11; 20]);

    /// The deployment file a real send writes for `plan`.
    fn record_for(artifacts: &Path, plan: &ChainPlan) -> serde_json::Value {
        let key: ChainKey = format!("evm:{}", plan.chain_id).parse().unwrap();
        let deployments = mainnet_evm::deployments_of(&key, plan, 1);
        let mut value: serde_json::Value =
            serde_json::from_str(&deploy::deployment_json(&deployments, plan.generation).unwrap())
                .unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert(
            deploy::STATION_IMPLEMENTATION_KEY.into(),
            serde_json::json!(plan.platform.station_implementation),
        );
        object.insert(
            MAINNET_KEY.into(),
            serde_json::to_value(MainnetRecord {
                risc_zero: plan.risc_zero,
                vault_implementation: plan.platform.vault_implementation,
                drivers: Map::from([(plan.driver.name.clone(), plan.driver.address)]),
                deployer: Address::new([0x22; 20]),
                build: build(artifacts).unwrap(),
            })
            .unwrap(),
        );
        value
    }

    fn plan(artifacts: &Path, chain_id: u64) -> ChainPlan {
        mainnet_evm::plan_chain(artifacts, &venues(), chain_id, ADMIN, 1, &names()).unwrap()
    }

    #[test]
    fn every_creation_the_deploy_makes_is_submitted_with_the_address_the_file_records() {
        let Some(artifacts) = artifacts() else { return };
        for chain_id in [8453, 999, 4663] {
            let plan = plan(&artifacts, chain_id);
            let text = record_for(&artifacts, &plan).to_string();
            let (_, submissions) =
                submissions(&artifacts, &venues(), chain_id, &text, ADMIN, 1, &names()).unwrap();
            let planned: Vec<_> = plan.creations.iter().map(|c| (c.name, c.address)).collect();
            let submitted: Vec<_> = submissions.iter().map(|s| (s.name, s.address)).collect();
            assert_eq!(planned, submitted);
            for name in [
                "RiscZeroGroth16Verifier",
                "ConfidentialSpaceVerifier",
                "StationCloses",
                "Valuations",
                "Claims",
                "Posts",
                "Upkeep",
                "Closer",
            ] {
                assert!(
                    submitted.iter().any(|(n, _)| *n == name),
                    "{name} on {chain_id}"
                );
            }
        }
    }

    #[test]
    fn a_submission_carries_the_build_s_compiler_its_linked_libraries_and_its_arguments() {
        let Some(artifacts) = artifacts() else { return };
        let plan = plan(&artifacts, 8453);
        let text = record_for(&artifacts, &plan).to_string();
        let (_, submissions) =
            submissions(&artifacts, &venues(), 8453, &text, ADMIN, 1, &names()).unwrap();
        let by_name = |name: &str| submissions.iter().find(|s| s.name == name).unwrap();
        let address = |name: &str| by_name(name).address;
        let vault = by_name("Vault");
        let mut wanted: Vec<(String, Address)> = ["Claims", "Posts", "Upkeep", "Valuations"]
            .iter()
            .map(|name| (format!("src/vault/{name}.sol:{name}"), address(name)))
            .collect();
        wanted.sort();
        assert_eq!(vault.libraries, wanted);
        // A library is listed where the build links it, and nowhere else.
        assert!(by_name("Valuations").libraries.is_empty());
        let station = by_name("Station");
        assert_eq!(station.libraries.len(), 1);
        assert_eq!(station.libraries[0].1, address("StationCloses"));
        // The checker's arguments are what the plan created it with.
        let checker = by_name("ConfidentialSpaceVerifier");
        let creation = plan
            .creations
            .iter()
            .find(|c| c.name == "ConfidentialSpaceVerifier")
            .unwrap();
        assert!(creation.code.ends_with(&checker.constructor_args));
        assert!(!checker.constructor_args.is_empty());
        assert!(by_name("Valuations").constructor_args.is_empty());
        for submission in &submissions {
            assert!(
                submission.compiler.starts_with("v0.8."),
                "{}",
                submission.compiler
            );
            assert_eq!(submission.evm_version, "cancun");
            assert!(submission.via_ir);
            assert_eq!(submission.optimizer_runs, Some(200));
        }
        assert_eq!(
            by_name("RiscZeroGroth16Verifier").contract,
            "lib/risc0-ethereum/contracts/src/groth16/RiscZeroGroth16Verifier.sol:RiscZeroGroth16Verifier"
        );
    }

    #[test]
    fn a_file_recording_another_admin_generation_build_or_address_is_refused() {
        let Some(artifacts) = artifacts() else { return };
        let plan = plan(&artifacts, 4663);
        let good = record_for(&artifacts, &plan);
        let attempt = |value: &serde_json::Value, admin: Address, generation: u64| {
            submissions(
                &artifacts,
                &venues(),
                4663,
                &value.to_string(),
                admin,
                generation,
                &names(),
            )
        };
        assert!(attempt(&good, ADMIN, 1).is_ok());
        let error = attempt(&good, Address::new([0x33; 20]), 1).unwrap_err();
        assert!(format!("{error:#}").contains("admin"), "{error:#}");
        let error = attempt(&good, ADMIN, 2).unwrap_err();
        assert!(format!("{error:#}").contains("generation"), "{error:#}");

        let mut other_build = good.clone();
        other_build[MAINNET_KEY]["build"] = serde_json::json!(B256::repeat_byte(7));
        let error = attempt(&other_build, ADMIN, 1).unwrap_err();
        assert!(format!("{error:#}").contains("build"), "{error:#}");

        let mut moved = good.clone();
        moved["publishing"] = serde_json::json!(Address::new([0x44; 20]));
        let error = attempt(&moved, ADMIN, 1).unwrap_err();
        assert!(format!("{error:#}").contains("Publishing"), "{error:#}");

        let mut other_driver = good.clone();
        other_driver[MAINNET_KEY]["drivers"] =
            serde_json::json!({ "robinhood": Address::new([0x55; 20]) });
        let error = attempt(&other_driver, ADMIN, 1).unwrap_err();
        assert!(
            format!("{error:#}").contains("RobinhoodDriver"),
            "{error:#}"
        );

        let mut other_chain = good;
        other_chain["chain_id"] = serde_json::json!(8453);
        assert!(attempt(&other_chain, ADMIN, 1).is_err());
    }

    #[test]
    fn the_command_names_each_explorer_and_never_carries_a_key() {
        let submission = Submission {
            chain_id: 4663,
            name: "Upkeep",
            address: Address::new([0x66; 20]),
            contract: "src/vault/Upkeep.sol:Upkeep".into(),
            compiler: "v0.8.30+commit.73712a01".into(),
            optimizer_runs: Some(200),
            via_ir: true,
            evm_version: "cancun".into(),
            libraries: vec![(
                "src/vault/Valuations.sol:Valuations".into(),
                Address::new([0x77; 20]),
            )],
            constructor_args: vec![0xab, 0xcd],
        };
        let root = Path::new("/project");
        let etherscan = submission.forge_args(root, Explorer::Etherscan).join(" ");
        assert!(etherscan.contains(
            "--verifier etherscan --verifier-url https://api.etherscan.io/v2/api?chainid=4663"
        ));
        assert!(etherscan.contains("--compiler-version v0.8.30+commit.73712a01"));
        assert!(etherscan.contains("--num-of-optimizations 200 --via-ir --evm-version cancun"));
        assert!(etherscan.contains("--constructor-args 0xabcd"));
        assert!(etherscan.contains(&format!(
            "--libraries src/vault/Valuations.sol:Valuations:{}",
            Address::new([0x77; 20])
        )));
        assert!(!etherscan.contains("api-key") && !etherscan.contains("apikey"));
        let sourcify = submission.forge_args(root, Explorer::Sourcify).join(" ");
        assert!(sourcify.contains("--verifier sourcify") && !sourcify.contains("etherscan"));
        assert_eq!(
            redacted("url ...&apikey=SECRET123 failed", Some("SECRET123")),
            "url ...&apikey=<redacted> failed"
        );
        // Trimmed, or percent-encoded in a URL, it is taken out too.
        assert_eq!(
            redacted("key=SECRET123; ?apikey=a%2Bb%2Fc", Some(" SECRET123 ")),
            "key=<redacted>; ?apikey=a%2Bb%2Fc"
        );
        assert_eq!(
            redacted("?apikey=a%2Bb%2Fc and a%2bb%2fc", Some("a+b/c")),
            "?apikey=<redacted> and <redacted>"
        );
    }

    #[test]
    fn a_fork_is_never_submitted_and_no_send_flag_is_taken() {
        let flags = |on_fork: bool| mainnet_guard::Flags {
            admin: Some(ADMIN.to_string()),
            on_fork,
            generation: Some(1),
            ..Default::default()
        };
        let ids = [8453, 999, 4663];
        assert!(decide(&flags(true), true, &ids).is_err());
        assert_eq!(decide(&flags(true), false, &ids).unwrap().mode, Mode::Fork);
        assert_eq!(
            decide(&flags(false), false, &ids).unwrap().mode,
            Mode::DryRun
        );
        assert_eq!(
            decide(&flags(false), true, &ids).unwrap().mode,
            Mode::DryRun
        );
        let mut with_key = flags(false);
        with_key.deployer_keyfile = Some(PathBuf::from("/key"));
        assert!(decide(&with_key, true, &ids).is_err());
        let mut confirmed = flags(false);
        confirmed.confirm_mainnet = Some("999,4663,8453".into());
        assert!(decide(&confirmed, false, &ids).is_err());
        let mut no_generation = flags(false);
        no_generation.generation = None;
        assert!(decide(&no_generation, false, &ids).is_err());
    }

    #[test]
    fn a_submission_reads_the_committed_directory_alone_and_a_fork_never_reads_it() {
        let key: ChainKey = "evm:8453".parse().unwrap();
        let elsewhere = std::env::temp_dir().join("shonin-verify-source-test");
        let error = read_record(Mode::DryRun, true, &elsewhere, &key).unwrap_err();
        assert!(
            format!("{error:#}").contains("config/deployments/mainnet"),
            "{error:#}"
        );
        let error =
            read_record(Mode::Fork, false, &mainnet_guard::committed_dir(), &key).unwrap_err();
        assert!(format!("{error:#}").contains("mainnet-fork"), "{error:#}");
    }

    #[test]
    fn every_mainnet_chain_lists_etherscan_and_sourcify() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/mainnet.toml"),
        )
        .unwrap();
        let explorers = read_explorers(&text).unwrap();
        for id in [8453, 999, 4663] {
            assert_eq!(
                explorers.get(&id),
                Some(&vec![Explorer::Etherscan, Explorer::Sourcify]),
                "{id}"
            );
        }
    }

    /// Run git in `dir` for a test's scratch repository.
    fn scratch_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args([
                "-c",
                "protocol.file.allow=always",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    #[test]
    fn a_submission_publishes_the_commit_s_source_never_the_working_tree_s() {
        let root =
            std::env::temp_dir().join(format!("shonin-source-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A library, a submodule of the repository at one commit.
        let library = root.join("library");
        std::fs::create_dir_all(&library).unwrap();
        scratch_git(&library, &["init", "-q"]);
        std::fs::write(library.join("Lib.sol"), "// the library as committed\n").unwrap();
        scratch_git(&library, &["add", "."]);
        scratch_git(&library, &["commit", "-q", "-m", "library"]);
        let repo = root.join("repo");
        let project = repo.join("contracts/evm");
        std::fs::create_dir_all(project.join("src")).unwrap();
        scratch_git(&repo, &["init", "-q"]);
        std::fs::write(
            project.join("src/Station.sol"),
            "// the station as committed\n",
        )
        .unwrap();
        std::fs::write(repo.join("outside.txt"), "not the project\n").unwrap();
        scratch_git(&repo, &["add", "."]);
        scratch_git(
            &repo,
            &[
                "submodule",
                "add",
                "-q",
                library.to_str().unwrap(),
                "contracts/evm/lib/library",
            ],
        );
        scratch_git(&repo, &["commit", "-q", "-m", "repo"]);
        // The working tree then differs from the commit, in ways that leave
        // the bytecode as it is: a comment, a new file, the submodule's file
        // edited and the submodule moved to a commit the repository doesn't
        // record.
        std::fs::write(project.join("src/Station.sol"), "// an edited comment\n").unwrap();
        std::fs::write(project.join("src/New.sol"), "// not committed\n").unwrap();
        let checked_out = project.join("lib/library");
        std::fs::write(checked_out.join("Lib.sol"), "// edited in the submodule\n").unwrap();
        scratch_git(&checked_out, &["commit", "-q", "-am", "moved"]);
        std::fs::write(checked_out.join("Lib.sol"), "// edited again\n").unwrap();

        let into = root.join("export");
        std::fs::create_dir_all(into.join("stale")).unwrap();
        let (commit, exported) = export_head(&project, &into).unwrap();
        assert_eq!(commit.len(), 40, "{commit}");
        assert_eq!(exported, into.join("contracts/evm"));
        let read = |path: &str| std::fs::read_to_string(exported.join(path)).unwrap();
        assert_eq!(read("src/Station.sol"), "// the station as committed\n");
        assert_eq!(read("lib/library/Lib.sol"), "// the library as committed\n");
        assert!(!exported.join("src/New.sol").exists());
        assert!(!into.join("outside.txt").exists());
        assert!(!into.join("stale").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn this_repository_s_contracts_export_with_their_libraries() {
        let into = std::env::temp_dir().join(format!("shonin-own-export-{}", std::process::id()));
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm");
        let (_, exported) = export_head(&project, &into).unwrap();
        assert!(exported.join("foundry.toml").exists());
        assert!(exported.join("lib/forge-std/src/Test.sol").exists());
        assert!(
            exported
                .join("lib/openzeppelin-contracts/contracts")
                .is_dir()
        );
        let _ = std::fs::remove_dir_all(&into);
    }
}
