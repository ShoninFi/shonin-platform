//! The development chains, and the ops dashboard.
//!
//! On the local network it deploys the contracts to every EVM chain, funds
//! the accounts the platform's roles use, moves each chain's clock faster than
//! real time, and plays the market each venue quotes. Everything else on the
//! platform reads the files it writes and talks to the chains it prepared.
//!
//! On the testnets it deploys and seeds the same stand-in stack, signed by the
//! accounts the developer's mnemonic derives, and drives nothing: the chains
//! keep their own time. It never deploys to mainnet ([`MAINNET_MISSING`]).
//!
//! On any network it serves the ops dashboard on its port: what is running,
//! locally and in the cloud.

mod admin;
mod checker;
mod dashboard;
mod deploy;
mod devnet_keys;
mod economics;
mod fund;
mod hyperliquid_vaults;
mod market;
mod publish;
mod seed;
mod smoke;
mod solana;
mod solana_orca;
mod solana_seed;
mod solana_testnet;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chain::{
    ChainArgs, ChainConfig, ChainKey, ChainKind, Deployments, NetworkName, Target, connect_to,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Deploys and drives the local development chains")]
struct Args {
    #[command(flatten)]
    chain_args: ChainArgs,
    /// The content-addressed store the services share.
    #[arg(long)]
    store: PathBuf,
    /// The compiled contracts, as the build tool leaves them.
    #[arg(long)]
    artifacts: PathBuf,
    /// Where to serve the dashboard and answer "are you up?".
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// A service for the dashboard to watch, as name=url, where the URL
    /// answers when it is up. Repeat it for each one.
    #[arg(long = "service", value_parser = name_and_value)]
    services: Vec<(String, String)>,
    /// A Google Cloud project for the dashboard to list, as network=project.
    /// An empty project is skipped: nothing is ever read from the gcloud
    /// default.
    #[arg(long = "gcp-project", value_parser = name_and_value)]
    gcp_projects: Vec<(String, String)>,
    /// The region the Cloud Run services are listed in.
    #[arg(long)]
    gcp_region: Option<String>,
    /// The built Solana programs and their keypairs, as `anchor build`
    /// leaves them.
    #[arg(
        long,
        env = "SHONIN_SOLANA_PROGRAMS",
        default_value = "contracts/solana/target/deploy"
    )]
    solana_programs: PathBuf,
    /// Orca's program, as `task contracts:solana:fixtures` fetches it from
    /// mainnet: what a local cluster loads at Orca's own address.
    #[arg(
        long,
        env = "SHONIN_ORCA_PROGRAM",
        default_value = ".shonin/fixtures/orca_whirlpool.so"
    )]
    orca_program: PathBuf,
    /// What `fund` tops each testnet role up to.
    #[arg(long, default_value = "config/funding.toml")]
    funding: PathBuf,
    /// Seconds of chain time each block advances.
    #[arg(long, default_value_t = 3_600)]
    seconds_per_block: u64,
    /// The deployment generation: the salt of the contracts at one address
    /// on every chain (the station factory among them), the same on every
    /// chain of the network and recorded in each deployment file. A clean
    /// redeploy beside an older deployment needs a new one. Without it, a
    /// deploy takes the generation of the chains it keeps, or 0.
    #[arg(long, env = "SHONIN_GENERATION")]
    generation: Option<u64>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Deploy the contracts to every EVM chain of the network, and on the
    /// local network the programs to its Solana cluster, and write the
    /// addresses, then stop. Local and testnet only. On the testnets the
    /// Solana programs are deployed by `task contracts:solana:deploy` and set
    /// up by `solana-setup`.
    Deploy,
    /// On the testnets, set Solana devnet up from the programs already
    /// deployed there, as a development cluster is set up: every role funded
    /// by transfer from the deployer (the funding file's amounts), the
    /// dollar's mint at its fixed index of the team's mnemonic, the
    /// registry with no timelock and the development checker, the operator's
    /// project, the stand-in driver and the venue's markets, every role's
    /// dollar account; then the deployment file, with the roles. No image
    /// is proposed: a real digest is, with `propose-image`. Safe to run
    /// again.
    SolanaSetup {
        /// The operator's Google Cloud project id.
        #[arg(long)]
        project: String,
        /// The SOL the market operator's own seed in Orca's pool holds, with
        /// as many dollars, centred on Pyth's price: moved to it from the
        /// deployer as it needs. The default gives the depth a $100 demo
        /// vault's closes need. Zero sets no seed up.
        #[arg(long, default_value_t = 40.0)]
        orca_seed_sol: f64,
    },
    /// The smoke check a deploy ends with, read-only: each EVM chain's
    /// deployment read back from the chain (or only `--vault-chain`'s), and
    /// with `--api` the API's health, every one of those chains served with
    /// a fresh reading, and a vault priced. Fails if any check does.
    Smoke {
        /// The API's public address, e.g. https://testnet-api.shonin.fi
        #[arg(long)]
        api: Option<String>,
        /// Pass when no vault is priced yet (a fresh generation)
        #[arg(long)]
        allow_no_vault: bool,
    },
    /// Show the funder and each role's account on every EVM chain of the
    /// network, with what each holds for gas and what it is short, and the
    /// Solana deployer, roles and programs: what to fund before a testnet
    /// deploy.
    Accounts,
    /// Deploy the Confidential Space checker on every EVM chain of the
    /// network and propose it to the registry beside the development
    /// checker. Already proposed is done.
    ProposeChecker,
    /// Propose a real image digest to every chain's registry (each EVM
    /// chain's and the Solana cluster's, once it is set up), signed by the
    /// admin. Already proposed is done.
    ProposeImage {
        /// `sha256:<64 hex>`, as the container registry names the image.
        #[arg(long)]
        digest: String,
        /// executor, builder, reviewer or unwinder.
        #[arg(long)]
        kind: String,
    },
    /// Revoke an image digest on every chain's registry, signed by the
    /// admin: at once and for good. Already revoked is done.
    RevokeImage {
        /// `sha256:<64 hex>`, as the container registry names the image.
        #[arg(long)]
        digest: String,
    },
    /// Propose an operator project (by its Google Cloud project id; the
    /// registry holds its keccak256) to every chain's registry, signed by
    /// the admin. Already proposed is done.
    ProposeProject {
        /// The Google Cloud project id.
        project: String,
    },
    /// Retire the development checker on every chain, signed by the admin;
    /// refused on a chain where no other checker is allowed. Already retired
    /// is done.
    RetireChecker {
        /// The development checker (the only kind this retires).
        #[arg(long)]
        dev: bool,
    },
    /// Print the root key's record on the network's canonical chain (its
    /// `root` in the chain config): the signer, when and through which
    /// checker it was recorded, the image and project, and whether it is
    /// live.
    RootKey,
    /// Clear the root key's record on the network's canonical chain, signed
    /// by the admin, and print the signer cleared. That signer is refused for
    /// good; the executor makes a new root and the keeper records it. Already
    /// clear is done.
    ClearRootKey,
    /// Allow a development checker again on every chain (a fresh one, since
    /// a retired checker can't be proposed again), for a publish only it can
    /// bless. Already allowed is done. Retire it again after.
    AllowDevChecker,
    /// Top every role up on every EVM testnet from the funder, to the amounts
    /// in the funding file; buys HYPE on HyperCore for HyperEVM. Safe to run
    /// again; testnet only.
    Fund,
    /// Print the vault chain's public RPC (the file's, never an override,
    /// which may carry a key) and its deployment file, one per line, for
    /// the tasks that shell out to other tools.
    Chain,
    /// Print every served chain's deployment as
    /// `~<prefix>SHONIN_DEPLOYMENTS_JSON=[...]`, to append to a "^~^"-delimited
    /// gcloud list. Testnet addresses are never committed, so this is how a
    /// cloud launch is handed them; mainnet's are in the image, so there it
    /// prints nothing.
    DeploymentEnv {
        #[arg(long, default_value = "")]
        prefix: String,
    },
    /// Write a Solana keypair derived from the testnet Solana mnemonic, and
    /// print its address.
    SolanaKeypair {
        /// Which: the deployer, the registry's own address, or the keeper
        /// (for the cloud's keeper, which holds that key alone).
        #[arg(long, value_parser = ["deployer", "program", "keeper"])]
        role: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write every program's devnet keypair, derived from the testnet Solana
    /// mnemonic, into a directory as `<name>-keypair.json`, and print each
    /// program as `<name> <address> <final|upgradeable>`.
    SolanaProgramKeypairs {
        #[arg(long)]
        out: PathBuf,
    },
    /// On Hyperliquid testnet: deploy (or find) the vault stack whose
    /// dollar is HyperEVM's USDC, with the Hyperliquid driver approved; with
    /// `--api` and `--agent`, also publish that agent, activate it there,
    /// deposit, fund the station's HyperCore account, and claim it all back.
    HyperliquidVaults {
        #[arg(long)]
        api: Option<String>,
        #[arg(long)]
        agent: Option<PathBuf>,
        /// The depositor's deposit, in dollars (at least the factory's
        /// minimum, the default): `mm-quoter` needs a vault of about 650.
        #[arg(long)]
        deposit: Option<f64>,
        /// What the funder sends the station's HyperCore account.
        #[arg(long, default_value_t = 30.0)]
        station_dollars: f64,
        #[arg(long, default_value_t = 10)]
        trade_minutes: u64,
    },
    /// Publish the reference agents through the platform's own flows, on the
    /// vault chain.
    Seed {
        /// Where the API is answering: a local one or a remote URL. Not
        /// needed with `--strategy`, which publishes nothing.
        #[arg(long, default_value = "")]
        api: String,
        /// Where the strategies to publish live.
        #[arg(long, env = "SHONIN_AGENTS_DIR")]
        agents: Option<PathBuf>,
        /// Publish only this agent, once, at the version its manifest names,
        /// instead of the full set of examples the site needs.
        #[arg(long)]
        agent: Option<String>,
        /// With `--agent`, then use its vault as the full seed's live vault
        /// is used: a deposit priced and a claim booked and paid.
        #[arg(long)]
        live: bool,
        /// With `--agent`, publish it and stop: no vault is activated. The
        /// strategy's id is printed as `strategy <id>` for `--strategy`.
        #[arg(long, conflicts_with = "live")]
        publish_only: bool,
        /// Run the live part (activate its vault if it has none, a deposit
        /// priced, a claim booked and paid) on this already registered
        /// strategy, publishing nothing.
        #[arg(long, conflicts_with_all = ["agent", "publish_only", "live"])]
        strategy: Option<u64>,
        /// How long the executor has to take a new vault on before the seed
        /// stops and says so. Where an attestation proof is in the path
        /// (a testnet executor in an enclave), allow far longer.
        #[arg(long, default_value_t = seed::TAKE_ON_SECONDS)]
        take_on_seconds: u64,
    },
    /// On a testnet, play the stand-in venue's market in real time, signed
    /// by the operator from the mnemonic (never from the cloud): every
    /// `--every-minutes` one post of both markets' prices and the funding
    /// accrued since the last, from a tape like bitcoin's (spot about 0.6%
    /// an hour, funding about 10% a year with rare lulls), minting the venue
    /// whatever its accounts are owed beyond what it holds. It starts from
    /// the venue's prices as they stand, so it can be stopped and started
    /// again. Every EVM chain of the network and its Solana cluster once set
    /// up (the admin keeping the venue's reserves there) at once, or
    /// `--vault-chain`'s.
    Market {
        #[arg(long, default_value_t = 10)]
        every_minutes: u64,
    },
    /// A demonstration that pays its own way, on the vault chain: a fresh
    /// vault for the agent (published again, or a strategy published with
    /// `--publish-only` and never activated), activated at the modest
    /// allowances with its tank filled to and kept at `--tank-days` of the
    /// daily cap by auto-fill, a depositor's money in and at work, then left
    /// running. Needs the market operator (`market`) posting.
    Demo {
        /// Where the API is answering. Not needed with `--strategy`.
        #[arg(long, default_value = "")]
        api: String,
        #[arg(long, env = "SHONIN_AGENTS_DIR")]
        agents: Option<PathBuf>,
        #[arg(long, default_value = "funding-basis")]
        agent: String,
        /// Activate this strategy, published and never activated, rather
        /// than publish again.
        #[arg(long)]
        strategy: Option<u64>,
        /// The depositor's money, in dollars.
        #[arg(long, default_value_t = 25_000)]
        deposit: u64,
        /// The tank, and what auto-fill keeps it at, in days of the
        /// allowances' daily cap.
        #[arg(long, default_value_t = 30)]
        tank_days: u64,
        #[arg(long, default_value_t = seed::TAKE_ON_SECONDS)]
        take_on_seconds: u64,
        /// How long the agent has to put the money to work once priced.
        #[arg(long, default_value_t = 30)]
        work_minutes: u64,
    },
    /// Whether a vault pays its own way, read from the chain alone: its
    /// value, share price, tank, allowances and their daily cost, fees owed,
    /// the author's shares, the gas its key and the keeper spent on it over
    /// `--hours`, and a day projected (the bill against the author's fee,
    /// and the break-even deposit). Sends nothing.
    Economics {
        #[arg(long)]
        vault: alloy::primitives::Address,
        #[arg(long, default_value_t = 6.0)]
        hours: f64,
        /// The chain's coin in dollars for the mainnet-like gas figures
        /// (ETH 2,500 or HYPE 25 when not given).
        #[arg(long)]
        coin_usd: Option<f64>,
        /// The share of the vault held short, to project with, instead of
        /// what the station holds.
        #[arg(long)]
        carry_share: Option<f64>,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // A report asked for as JSON is the only thing on standard output; its
    // log goes to standard error.
    let json_out = matches!(args.command, Some(Command::Economics { json: true, .. }));
    let writer = if json_out {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr)
    } else {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stdout)
    };
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .with_writer(writer)
        .init();

    let target = args.chain_args.resolve()?;
    let local = target.network == NetworkName::Local;
    // A vault chain named on the command line: the devnet deploys to and
    // drives that one alone.
    let one = args.chain_args.vault_chain.is_some();
    let changes_chains = matches!(
        args.command,
        Some(
            Command::Deploy
                | Command::SolanaSetup { .. }
                | Command::Seed { .. }
                | Command::Accounts
                | Command::Fund
                | Command::ProposeChecker
                | Command::ProposeImage { .. }
                | Command::RevokeImage { .. }
                | Command::ProposeProject { .. }
                | Command::RetireChecker { .. }
                | Command::AllowDevChecker
                | Command::ClearRootKey
                | Command::HyperliquidVaults { .. }
                | Command::Market { .. }
                | Command::Demo { .. }
        )
    );
    if target.network == NetworkName::Mainnet && changes_chains {
        bail!("{MAINNET_MISSING}");
    }

    match args.command {
        Some(Command::Deploy) if !local && one && target.key.kind == ChainKind::Solana => {
            bail!(
                "on {} the Solana programs are deployed by task contracts:solana:deploy and set up by solana-setup (task chains:solana-setup)",
                target.network
            )
        }
        Some(Command::SolanaSetup {
            project,
            orca_seed_sol,
        }) => {
            if target.network != NetworkName::Testnet {
                bail!(
                    "solana-setup sets up Solana devnet (NETWORK=testnet): the local cluster is set up with the chains"
                );
            }
            let phrase = solana::testnet_mnemonic()?;
            let funding = fund::Funding::read(&args.funding)?;
            let clusters = only(&target, one, solana_clusters(&target));
            if clusters.is_empty() {
                bail!("the {} network has no Solana cluster", target.network);
            }
            for (key, config) in clusters {
                let settings = solana::Settings::testnet(&phrase, &project, &funding, &key)?;
                let client = chain::connect_solana(target.network, &key, &config).await?;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                let seed = (orca_seed_sol.max(0.0) * 1e9) as u64;
                let deployments = solana::set_up_public(
                    &client,
                    &key,
                    &config.public_rpc,
                    &file,
                    &settings,
                    seed,
                )
                .await
                .map_err(|error| for_this_build(target.network, error))?;
                println!(
                    "{} ({key}): set up; the dollar is {}, written to {}",
                    config.label,
                    deployments.mint.as_deref().unwrap_or_default(),
                    file.display()
                );
            }
            Ok(())
        }
        Some(Command::Deploy) => {
            let names = chain_names(&target);
            let chosen = only(&target, one, evm_chains(&target));
            // Every chain is looked at before anything is sent to any: which
            // are kept as they are, the generation they all go under, and
            // whether that generation's station factory is free on each of
            // the others.
            let mut chains = Vec::new();
            let mut kept = Vec::new();
            for (key, config) in evm_chains(&target) {
                let file = Deployments::file_in(&target.deployments_dir, &key);
                if !chosen.iter().any(|(chosen, _)| *chosen == key) {
                    // Not deployed to this time; its file still counts.
                    kept.extend(deploy::Kept::read(&key, &file));
                    continue;
                }
                let client = connect_to(target.network, &key, &config).await?;
                let keeps = deploy::still_there(&client, &file).await?.is_some();
                if keeps {
                    kept.extend(deploy::Kept::read(&key, &file));
                }
                chains.push((key, client, file, keeps));
            }
            let generation = deploy::settle_generation(args.generation, &kept, &args.artifacts)?;
            tracing::info!(generation, "the deployment generation");
            if chains.iter().any(|(_, _, _, keeps)| !keeps) {
                deploy::refuse_another_build(&kept, &args.artifacts, generation)?;
            }
            for (_, client, file, keeps) in &chains {
                if !keeps {
                    deploy::refuse_a_used_station_factory(
                        client,
                        &args.artifacts,
                        client.account(chain::Role::Admin)?,
                        generation,
                        file,
                    )
                    .await?;
                }
            }
            deploy::ensure_each(&chains, target.network, &args.artifacts, &names, generation)
                .await?;
            if local {
                for (key, config) in only(&target, one, solana_clusters(&target)) {
                    let client = chain::connect_solana(target.network, &key, &config).await?;
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    solana::ensure(
                        &client,
                        &key,
                        &config.rpc,
                        &args.solana_programs,
                        &args.orca_program,
                        &file,
                    )
                    .await?;
                }
            }
            Ok(())
        }
        Some(Command::Smoke {
            api,
            allow_no_vault,
        }) => {
            let chains: Vec<_> = target
                .configs
                .network(target.network)
                .into_iter()
                .flat_map(|network| network.iter())
                .filter(|(_, key)| key.kind == ChainKind::Evm)
                .filter(|(_, key)| !one || key.same_chain(&target.key))
                .filter_map(|(chain, key)| {
                    Some((chain, key.clone(), target.configs.get(key)?.clone()))
                })
                .collect();
            smoke::run(
                target.network,
                &chains,
                &target.deployments_dir,
                &smoke::Options {
                    api,
                    allow_no_vault,
                },
            )
            .await
        }
        Some(Command::Accounts) => accounts(&target, &args.funding).await,
        Some(Command::ProposeChecker) => {
            let evm = checker::run(
                target.network,
                only(&target, one, evm_chains(&target)),
                &target.deployments_dir,
                &args.artifacts,
            )
            .await;
            // The Solana cluster's checker: the same guest and Google keys.
            let image = shonin_attestation_deploy::GUEST_IMAGE_ID.0;
            let keys: Vec<[u8; 32]> = shonin_attestation_deploy::trusted_keys(
                shonin_attestation_core::GOOGLE_JWKS_2026_10_06,
            )?
            .into_iter()
            .map(|key| key.0)
            .collect();
            let solana = on_each_cluster(&target, one, |client, deployments| {
                let keys = keys.clone();
                async move {
                    admin::solana::propose_cs_checker(&client, &deployments, image, &keys).await
                }
            })
            .await;
            evm.and(solana)
        }
        Some(Command::ProposeImage { digest, kind }) => {
            let digest = admin::image_digest(&digest)?;
            let kind = admin::kind_named(&kind)?;
            on_each_registry(
                &target,
                one,
                |client, deployments| async move {
                    admin::propose_image(&client, &deployments, digest, kind).await
                },
                |client, deployments| async move {
                    admin::solana::propose_image(&client, &deployments, digest.0, kind).await
                },
            )
            .await
        }
        Some(Command::RevokeImage { digest }) => {
            let digest = admin::image_digest(&digest)?;
            on_each_registry(
                &target,
                one,
                |client, deployments| async move {
                    admin::revoke_image(&client, &deployments, digest).await
                },
                |client, deployments| async move {
                    admin::solana::revoke_image(&client, &deployments, digest.0).await
                },
            )
            .await
        }
        Some(Command::ProposeProject { project }) => on_each_registry(
            &target,
            one,
            |client, deployments| {
                let project = project.clone();
                async move { admin::propose_project(&client, &deployments, &project).await }
            },
            |client, deployments| {
                let project = project.clone();
                async move { admin::solana::propose_project(&client, &deployments, &project).await }
            },
        )
        .await,
        Some(Command::RetireChecker { dev }) => {
            if !dev {
                bail!("only the development checker is retired here: pass --dev");
            }
            on_each_registry(
                &target,
                one,
                |client, deployments| async move {
                    admin::retire_dev_checker(&client, &deployments).await
                },
                |client, deployments| async move {
                    admin::solana::retire_dev_checker(&client, &deployments).await
                },
            )
            .await
        }
        Some(Command::AllowDevChecker) => {
            let artifacts = args.artifacts.clone();
            on_each_registry(
                &target,
                one,
                |client, deployments| {
                    let artifacts = artifacts.clone();
                    async move { admin::allow_dev_checker(&client, &deployments, &artifacts).await }
                },
                |client, deployments| async move {
                    admin::solana::allow_dev_checker(&client, &deployments).await
                },
            )
            .await
        }
        Some(Command::RootKey) => {
            let (label, key, client, deployments) = root_registry(&target).await?;
            let said = admin::root_key(&client, &deployments).await?;
            println!("{label} ({key}): {said}");
            Ok(())
        }
        Some(Command::ClearRootKey) => {
            let (label, key, client, deployments) = root_registry(&target).await?;
            let said = admin::clear_root_key(&client, &deployments).await?;
            println!("{label} ({key}): {said}");
            Ok(())
        }
        Some(Command::Fund) => {
            fund::run(
                target.network,
                only(&target, one, evm_chains(&target)),
                &args.funding,
            )
            .await
        }
        Some(Command::DeploymentEnv { prefix }) => {
            match target.network {
                NetworkName::Mainnet => {}
                NetworkName::Local => bail!("the cloud never runs against local chains"),
                NetworkName::Testnet => {
                    let mut all = Vec::new();
                    for (key, _) in evm_chains(&target) {
                        let file = Deployments::file_in(&target.deployments_dir, &key);
                        let text = std::fs::read_to_string(&file).map_err(|error| {
                            anyhow::anyhow!(
                                "no testnet deployment at {} ({error}): run task chains:deploy NETWORK=testnet first",
                                file.display()
                            )
                        })?;
                        all.push(serde_json::to_value(Deployments::from_json(&text)?)?);
                    }
                    // A Solana cluster's file, where its programs are
                    // deployed, named by its key so a reader can find it.
                    for (key, _) in solana_clusters(&target) {
                        let file = Deployments::file_in(&target.deployments_dir, &key);
                        if let Ok(text) = std::fs::read_to_string(&file) {
                            let mut solana = chain::SolanaDeployments::from_json(&text)?;
                            if solana.chain_key.is_empty() {
                                solana.chain_key = key.to_string();
                            }
                            all.push(serde_json::to_value(solana)?);
                        }
                    }
                    print!(
                        "~{prefix}{}={}",
                        chain::args::DEPLOYMENTS_JSON,
                        serde_json::to_string(&all)?
                    );
                }
            }
            Ok(())
        }
        Some(Command::Chain) => {
            println!("{}", target.config.public_rpc);
            println!("{}", target.deployments_file().display());
            Ok(())
        }
        Some(Command::SolanaKeypair { role, out }) => {
            let index = match role.as_str() {
                "program" => solana::PROGRAM,
                "keeper" => {
                    chain::solana::keys::ROLE_BASE + chain::Role::Keeper.account_index() as u32
                }
                _ => solana::DEPLOYER,
            };
            let pair = solana::keypair_from_mnemonic(&solana::testnet_mnemonic()?, index)?;
            chain::solana::keys::write_keypair(&pair, &out)?;
            println!("{}", chain::solana::Signer::pubkey(&pair));
            Ok(())
        }
        Some(Command::SolanaProgramKeypairs { out }) => {
            let written = devnet_keys::write_program_keypairs(&solana::testnet_mnemonic()?, &out)?;
            for (name, address) in written {
                let kind = if solana::deployed_final(name) {
                    "final"
                } else {
                    "upgradeable"
                };
                println!("{name} {address} {kind}");
            }
            Ok(())
        }
        Some(Command::HyperliquidVaults {
            api,
            agent,
            deposit,
            station_dollars,
            trade_minutes,
        }) => {
            let client = target.connect().await?;
            let deployments = target.wait_for_deployments(&client).await?;
            // The stack goes with the deployment's generation, as its file
            // records it.
            let generation = std::fs::read_to_string(target.deployments_file())
                .ok()
                .and_then(|text| deploy::recorded_generation(&text));
            if let (Some(asked), Some(recorded)) = (args.generation, generation)
                && asked != recorded
            {
                bail!(
                    "the deployment on this chain is under generation {recorded}, not {asked}: \
                     the Hyperliquid stack goes beside the deployment as it is"
                );
            }
            match (api, agent) {
                (Some(api), Some(agent)) => {
                    hyperliquid_vaults::run(
                        &client,
                        &args.artifacts,
                        &deployments,
                        generation,
                        &target.deployments_dir,
                        hyperliquid_vaults::RunOptions {
                            api: &api,
                            agent: &agent,
                            deposit_dollars: deposit,
                            station_dollars,
                            trade_minutes,
                        },
                    )
                    .await
                }
                _ => {
                    let file =
                        hyperliquid_vaults::HyperliquidVaults::file_in(&target.deployments_dir);
                    let vaults = hyperliquid_vaults::ensure(
                        &client,
                        &args.artifacts,
                        &deployments,
                        generation,
                        &file,
                    )
                    .await?;
                    println!("{}", serde_json::to_string_pretty(&vaults)?);
                    Ok(())
                }
            }
        }
        // The local cluster's full seed plays the services' parts itself;
        // one agent, a registered strategy or any public cluster goes
        // through the API and the services, as on an EVM chain.
        Some(Command::Seed {
            agent: None,
            strategy: None,
            ..
        }) if target.key.kind == ChainKind::Solana && target.network == NetworkName::Local => {
            let served = target.serve_solana().await?;
            solana_seed::run(&served).await
        }
        Some(Command::Seed {
            api,
            agents,
            agent,
            live,
            publish_only,
            strategy,
            take_on_seconds,
        }) if target.key.kind == ChainKind::Solana => {
            let served = target.serve_solana().await?;
            let api = api.trim_end_matches('/').to_string();
            if strategy.is_none() && api.is_empty() {
                bail!("--api is needed to publish: where the API answers");
            }
            match (strategy, agent) {
                (Some(strategy), _) => {
                    solana_testnet::on_strategy(&served, strategy, take_on_seconds).await
                }
                (None, Some(name)) => {
                    solana_testnet::one(
                        &served,
                        &api,
                        agents.as_deref(),
                        &name,
                        live,
                        publish_only,
                        take_on_seconds,
                    )
                    .await
                }
                (None, None) => bail!(
                    "on a public Solana cluster the seed publishes one agent: name it with --agent"
                ),
            }
        }
        Some(Command::Seed {
            api,
            agents,
            agent,
            live,
            publish_only,
            strategy,
            take_on_seconds,
        }) => {
            let client = target.connect().await?;
            let deployments = target.wait_for_deployments(&client).await?;
            let api = api.trim_end_matches('/').to_string();
            if strategy.is_none() && api.is_empty() {
                bail!("--api is needed to publish: where the API answers");
            }
            if publish_only && agent.is_none() {
                bail!("--publish-only publishes one agent: name it with --agent");
            }
            match (strategy, agent) {
                (Some(strategy), _) => {
                    seed::on_strategy(
                        &client,
                        target.vault_chain,
                        &deployments,
                        strategy,
                        take_on_seconds,
                    )
                    .await
                }
                (None, Some(name)) => {
                    seed::one(
                        &client,
                        target.vault_chain,
                        &deployments,
                        &api,
                        agents.as_deref(),
                        &name,
                        live,
                        publish_only,
                        take_on_seconds,
                    )
                    .await
                }
                (None, None) => {
                    seed::run(
                        &client,
                        target.vault_chain,
                        &deployments,
                        &api,
                        agents.as_deref(),
                        take_on_seconds,
                    )
                    .await
                }
            }
        }
        Some(Command::Market { every_minutes }) => {
            if target.network != NetworkName::Testnet {
                bail!(
                    "the market operator is for the testnets: the local chain's market runs with the chain"
                );
            }
            let every = std::time::Duration::from_secs(every_minutes.max(1) * 60);
            let mut running = tokio::task::JoinSet::new();
            // Each Solana cluster set up, on its own as the EVM chains are.
            for (key, config) in only(&target, one, solana_clusters(&target)) {
                let file = Deployments::file_in(&target.deployments_dir, &key);
                let set_up = std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|text| chain::SolanaDeployments::from_json(&text).ok())
                    .filter(|deployments| deployments.roles.is_some());
                let Some(deployments) = set_up else {
                    tracing::info!(chain = %key, "not set up yet: no market on it");
                    continue;
                };
                let network = target.network;
                running.spawn(async move {
                    loop {
                        let ran = async {
                            let client = chain::connect_solana(network, &key, &config).await?;
                            client.check_deployments(&deployments).await?;
                            let served = chain::SolanaServed {
                                vault_chain: shared::Chain::Solana,
                                key: key.clone(),
                                platform: chain::solana::Platform::from_deployments(&deployments)?,
                                client,
                                deployments: deployments.clone(),
                                public_rpc: config.rpc.clone(),
                            };
                            solana_testnet::operate(&served, every).await
                        }
                        .await;
                        let Err(error) = ran else { continue };
                        let error =
                            for_this_build(network, error).context(format!("the market on {key}"));
                        if !market::worth_restarting(&error) {
                            return Err::<(), _>(error);
                        }
                        tracing::warn!(
                            error = format!("{error:#}"),
                            "the market operator stopped on this cluster; it starts again shortly"
                        );
                        tokio::time::sleep(market::RESTART_AFTER).await;
                    }
                });
            }
            for (key, config) in only(&target, one, evm_chains(&target)) {
                let network = target.network;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                // Each chain on its own: one whose endpoint is down, or that
                // stops, is started again after a pause, and holds up none
                // of the others.
                running.spawn(async move {
                    loop {
                        let ran = async {
                            let client = connect_to(network, &key, &config).await?;
                            let deployments =
                                client.deployments(&file).await.with_context(|| {
                                    format!("no deployment on {key}: run chains:deploy first")
                                })?;
                            market::operate(
                                &client,
                                &deployments,
                                every,
                                market::TapeParams::TESTNET,
                            )
                            .await
                        }
                        .await;
                        let Err(error) = ran else { continue };
                        let error = error.context(format!("the market on {key}"));
                        if !market::worth_restarting(&error) {
                            return Err::<(), _>(error);
                        }
                        tracing::warn!(
                            error = format!("{error:#}"),
                            "the market operator stopped on this chain; it starts again shortly"
                        );
                        tokio::time::sleep(market::RESTART_AFTER).await;
                    }
                });
            }
            let mut failed = None;
            while let Some(finished) = running.join_next().await {
                let error = match finished {
                    Ok(Ok(())) => continue,
                    Ok(Err(error)) => error,
                    Err(error) => anyhow::Error::new(error),
                };
                tracing::error!(
                    error = format!("{error:#}"),
                    "a chain's market operator stopped for good"
                );
                failed.get_or_insert(error);
            }
            failed.map_or(Ok(()), Err)
        }
        Some(Command::Demo {
            api,
            agents,
            agent,
            strategy,
            deposit,
            tank_days,
            take_on_seconds,
            work_minutes,
        }) if target.key.kind == ChainKind::Solana => {
            let served = target.serve_solana().await?;
            let api = api.trim_end_matches('/').to_string();
            if strategy.is_none() && api.is_empty() {
                bail!("--api is needed to publish: where the API answers");
            }
            solana_testnet::demo(
                &served,
                &seed::Demo {
                    network: target.network.name(),
                    api: &api,
                    agents: agents.as_deref(),
                    agent: &agent,
                    strategy,
                    deposit_dollars: deposit,
                    tank_days,
                    take_on: take_on_seconds,
                    work_within: std::time::Duration::from_secs(work_minutes * 60),
                },
            )
            .await
        }
        Some(Command::Demo {
            api,
            agents,
            agent,
            strategy,
            deposit,
            tank_days,
            take_on_seconds,
            work_minutes,
        }) => {
            let client = target.connect().await?;
            let deployments = target.wait_for_deployments(&client).await?;
            let api = api.trim_end_matches('/').to_string();
            if strategy.is_none() && api.is_empty() {
                bail!("--api is needed to publish: where the API answers");
            }
            seed::demo(
                &client,
                target.vault_chain,
                &deployments,
                &seed::Demo {
                    network: target.network.name(),
                    api: &api,
                    agents: agents.as_deref(),
                    agent: &agent,
                    strategy,
                    deposit_dollars: deposit,
                    tank_days,
                    take_on: take_on_seconds,
                    work_within: std::time::Duration::from_secs(work_minutes * 60),
                },
            )
            .await
        }
        Some(Command::Economics {
            vault,
            hours,
            coin_usd,
            carry_share,
            json,
        }) => {
            let client = target.connect().await?;
            let deployments = target.wait_for_deployments(&client).await?;
            economics::report(
                &client,
                &deployments,
                &economics::Options {
                    vault,
                    hours,
                    coin_usd,
                    billed_coin_usd: target.config.native_usd.unwrap_or(0.0),
                    carry_share,
                    mean_funding: if local {
                        market::TapeParams::LOCAL.funding_mean()
                    } else {
                        market::TESTNET_MEAN_FUNDING
                    },
                    json,
                },
            )
            .await
        }
        None => {
            let mut running = tokio::task::JoinSet::new();
            if args.port != 0 {
                let address = SocketAddr::from(([127, 0, 0, 1], args.port));
                let gcp_projects = args
                    .gcp_projects
                    .into_iter()
                    .filter(|(_, project)| !project.trim().is_empty())
                    .collect();
                let dashboard = dashboard::Dashboard::new(
                    target.network,
                    target.configs.clone(),
                    target.deployments_dir.clone(),
                    args.services,
                    gcp_projects,
                    args.gcp_region.filter(|region| !region.trim().is_empty()),
                );
                running.spawn(dashboard::serve(dashboard, address));
            }
            if local {
                // Each chain is deployed to and driven on its own, so a slow
                // one holds up nobody else.
                let names = chain_names(&target);
                for (key, config) in only(&target, one, evm_chains(&target)) {
                    let names = names.clone();
                    let artifacts = args.artifacts.clone();
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    let network = target.network;
                    let generation = args.generation.unwrap_or(0);
                    running.spawn(async move {
                        let client = connect_to(network, &key, &config).await?;
                        let deployments = deploy::ensure(
                            &client, network, &key, &artifacts, &file, &names, generation,
                        )
                        .await?;
                        tracing::info!(chain = %key, label = config.label, "the chain is up");
                        market::run(&client, &deployments, seconds_per_block)
                            .await
                            .map_err(|error| error.context(format!("the market on {key}")))
                    });
                }
                for (key, config) in only(&target, one, solana_clusters(&target)) {
                    let programs = args.solana_programs.clone();
                    let orca = args.orca_program.clone();
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    let network = target.network;
                    // A cluster that can't be brought up is said, and the
                    // EVM chains carry on: Solana needs tools of its own.
                    running.spawn(async move {
                        let driven = async {
                            let client = chain::connect_solana(network, &key, &config).await?;
                            let deployments =
                                solana::ensure(
                                    &client,
                                    &key,
                                    &config.rpc,
                                    &programs,
                                    &orca,
                                    &file,
                                )
                                .await?;
                            tracing::info!(chain = %key, label = config.label, "the cluster is up");
                            solana::run_market(&client, &deployments, seconds_per_block).await
                        };
                        if let Err(error) = driven.await {
                            tracing::error!(chain = %key, error = format!("{error:#}"), "the Solana cluster is not served");
                        }
                        Ok(())
                    });
                }
            } else {
                tracing::info!(network = %target.network, "public chains: nothing to drive, only to show");
            }
            while let Some(finished) = running.join_next().await {
                finished??;
            }
            Ok(())
        }
    }
}

/// Run one admin change against every registry of the network (or the
/// chain named): each EVM chain's with `evm`, the Solana cluster's with
/// `solana`, each from its deployment file, and say what it did.
async fn on_each_registry<F, Fut, G, Gut>(
    target: &Target,
    one: bool,
    evm: F,
    solana: G,
) -> Result<()>
where
    F: Fn(chain::Client, Deployments) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
    G: Fn(chain::solana::SolanaClient, chain::SolanaDeployments) -> Gut,
    Gut: std::future::Future<Output = Result<String>>,
{
    // Every chain at once: each registry is its own chain and its own
    // admin nonce, so one chain's slow blocks never hold up another's.
    // Each chain's outcome is printed; the command fails if any chain did.
    let evm_each = only(target, one, evm_chains(target))
        .into_iter()
        .map(|(key, config)| {
            let change = &evm;
            async move {
                let outcome = async {
                    let client = connect_to(target.network, &key, &config).await?;
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let deployments = client.deployments(&file).await.with_context(|| {
                        format!("no deployment on {key}: run chains:deploy first")
                    })?;
                    change(client, deployments).await
                }
                .await;
                (key, config, outcome)
            }
        });
    let (evm_done, solana_done) = futures::future::join(
        futures::future::join_all(evm_each),
        solana_changes(target, one, &solana),
    )
    .await;
    let mut failed = Vec::new();
    for (key, config, outcome) in evm_done.into_iter().chain(solana_done) {
        match outcome {
            Ok(said) => println!("{} ({key}): {said}", config.label),
            Err(error) => {
                println!("{} ({key}): failed: {error:#}", config.label);
                failed.push(format!("{} ({key})", config.label));
            }
        }
    }
    if !failed.is_empty() {
        bail!("not done on {}", failed.join(", "));
    }
    Ok(())
}

/// Run one admin change against every Solana cluster of the network (or the
/// one named) alone, and say what it did.
async fn on_each_cluster<G, Gut>(target: &Target, one: bool, solana: G) -> Result<()>
where
    G: Fn(chain::solana::SolanaClient, chain::SolanaDeployments) -> Gut,
    Gut: std::future::Future<Output = Result<String>>,
{
    let mut failed = Vec::new();
    for (key, config, outcome) in solana_changes(target, one, &solana).await {
        match outcome {
            Ok(said) => println!("{} ({key}): {said}", config.label),
            Err(error) => {
                println!("{} ({key}): failed: {error:#}", config.label);
                failed.push(format!("{} ({key})", config.label));
            }
        }
    }
    if !failed.is_empty() {
        bail!("not done on {}", failed.join(", "));
    }
    Ok(())
}

/// One admin change on each Solana cluster of the network (or the one
/// named), from its deployment file: each cluster's outcome.
async fn solana_changes<G, Gut>(
    target: &Target,
    one: bool,
    solana: &G,
) -> Vec<(ChainKey, ChainConfig, Result<String>)>
where
    G: Fn(chain::solana::SolanaClient, chain::SolanaDeployments) -> Gut,
    Gut: std::future::Future<Output = Result<String>>,
{
    // A cluster not set up yet is passed over and said to be, unless it was
    // named alone: the EVM chains' changes don't wait on it.
    let solana_each = only(target, one, solana_clusters(target))
        .into_iter()
        .map(|(key, config)| {
            let change = solana;
            async move {
                let outcome = async {
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let deployments = std::fs::read_to_string(&file)
                        .ok()
                        .and_then(|text| chain::SolanaDeployments::from_json(&text).ok())
                        .filter(|deployments| deployments.roles.is_some());
                    let Some(deployments) = deployments else {
                        if one {
                            bail!(
                                "{key} is not set up ({} names no roles): run chains:solana-setup first",
                                file.display()
                            );
                        }
                        return Ok(format!(
                            "not set up yet ({} names no roles): skipped",
                            file.display()
                        ));
                    };
                    let client = chain::connect_solana(target.network, &key, &config).await?;
                    client.check_deployments(&deployments).await?;
                    change(client, deployments).await
                }
                .await
                .map_err(|error| for_this_build(target.network, error));
                (key, config, outcome)
            }
        });
    futures::future::join_all(solana_each).await
}

/// A Solana command's failure, with what most likely caused it when this
/// build's program ids are another network's: a program's id is compiled
/// in, so such a build looks for the cluster's programs where they aren't
/// (the Solana-facing tasks build with the network's feature).
fn for_this_build(network: NetworkName, error: anyhow::Error) -> anyhow::Error {
    match chain::solana_built_for(network) {
        Ok(()) => error,
        Err(why) => error.context(format!("{why:#}")),
    }
}

/// The network's canonical chain for the root key, connected, with its
/// deployment.
async fn root_registry(target: &Target) -> Result<(String, ChainKey, chain::Client, Deployments)> {
    let key = target
        .configs
        .network(target.network)
        .and_then(|network| network.root())
        .map(|(_, key)| key.clone())
        .with_context(|| {
            format!(
                "the {} network names no canonical chain for the root key (`root` in the chain config)",
                target.network
            )
        })?;
    let config = target
        .configs
        .get(&key)
        .with_context(|| format!("no chain has the key {key}"))?
        .clone();
    let client = connect_to(target.network, &key, &config).await?;
    let file = Deployments::file_in(&target.deployments_dir, &key);
    let deployments = client
        .deployments(&file)
        .await
        .with_context(|| format!("no deployment on {key}: run chains:deploy first"))?;
    Ok((config.label, key, client, deployments))
}

/// Why the devnet never deploys to mainnet: what it deploys is the stand-in
/// stack, which is for local chains and testnets.
pub const MAINNET_MISSING: &str = "The devnet does not deploy to mainnet. What it deploys is the \
stand-in stack: a dollar anyone may mint, a stand-in venue, and an attestation verifier that trusts \
one development key. It is for local chains and testnets. Mainnet addresses are committed under \
config/deployments/mainnet.";

/// Each role's account on every EVM chain of the network, with its gas, and
/// on the testnets the funder, what each role is short of its amount in the
/// funding file, and the Solana deployer, roles and programs.
async fn accounts(target: &Target, funding_file: &std::path::Path) -> Result<()> {
    use alloy::primitives::utils::format_ether;
    let testnet = target.network == NetworkName::Testnet;
    let funding = if testnet {
        Some(fund::Funding::read(funding_file)?)
    } else {
        None
    };
    let funder = if testnet {
        Some(fund::funder()?.address())
    } else {
        None
    };
    for (key, config) in evm_chains(target) {
        let client = connect_to(target.network, &key, &config).await?;
        println!("{} ({key})", config.label);
        match (&funding, funder) {
            (Some(funding), Some(funder)) => {
                let short = fund::shortfalls(funding, &client, &key).await?;
                let reserve = funding.reserve(&key).unwrap_or_default();
                let held = client.balance(funder).await?;
                let needs = (short.total + reserve).saturating_sub(held);
                println!(
                    "  {:<9} {funder:?}  {}  (the roles are short {}, the reserve is {}{})",
                    "FUNDER",
                    format_ether(held),
                    format_ether(short.total),
                    format_ether(reserve),
                    if needs.is_zero() {
                        String::new()
                    } else {
                        format!("; it needs {} more", format_ether(needs))
                    }
                );
                if let Some(api) = hyperliquid::Api::for_chain(client.chain_id) {
                    let perp = api.perp_withdrawable(funder).await.unwrap_or(0.0);
                    let usdc = api.spot_balance(funder, "USDC").await.unwrap_or(0.0);
                    let hype = api.spot_balance(funder, "HYPE").await.unwrap_or(0.0);
                    println!(
                        "  {:<9} on HyperCore: ${perp} perp, ${usdc} spot, {hype} HYPE (chains:fund buys HYPE with it)",
                        ""
                    );
                }
                for (role, address, balance, need) in short.rows {
                    println!(
                        "  {:<9} {address:?}  {}{}",
                        role.label(),
                        format_ether(balance),
                        if need.is_zero() {
                            String::new()
                        } else {
                            format!("  (short {})", format_ether(need))
                        }
                    );
                }
            }
            _ => {
                for role in fund::ROLES {
                    let address = client.account(role)?;
                    let balance = client.balance(address).await?;
                    println!(
                        "  {:<9} {address:?}  {}",
                        role.label(),
                        format_ether(balance)
                    );
                }
            }
        }
    }
    if testnet {
        use chain::solana::Signer;
        let phrase = solana::testnet_mnemonic()?;
        let deployer = solana::keypair_from_mnemonic(&phrase, solana::DEPLOYER)?;
        for (key, config) in solana_clusters(target) {
            println!("{} ({key})", config.label);
            // What each holds, where the cluster answers: the addresses are
            // shown either way.
            let client = match chain::connect_solana(target.network, &key, &config).await {
                Ok(client) => Some(client),
                Err(error) => {
                    println!("  (not reached: {error:#}; the addresses alone)");
                    None
                }
            };
            let amounts = funding
                .as_ref()
                .map(|funding| funding.solana(&key))
                .transpose()?
                .flatten();
            let sol = |lamports: u64| format!("{} SOL", lamports as f64 / 1e9);
            let held = |address: chain::solana::Pubkey| {
                let client = client.clone();
                async move {
                    match &client {
                        Some(client) => client.lamports(&address).await.ok(),
                        None => None,
                    }
                }
            };
            let mut short_total = 0u64;
            let mut rows = Vec::new();
            for role in fund::ROLES {
                let index = chain::solana::keys::ROLE_BASE + role.account_index() as u32;
                let address = solana::keypair_from_mnemonic(&phrase, index)?.pubkey();
                let holds = held(address).await;
                let target_amount = amounts.as_ref().and_then(|amounts| {
                    amounts
                        .roles
                        .iter()
                        .find(|(r, _)| *r == role)
                        .map(|(_, a)| *a)
                });
                let short = match (holds, target_amount) {
                    (Some(holds), Some(target)) => target.saturating_sub(holds),
                    _ => 0,
                };
                short_total += short;
                rows.push((role, address, holds, short));
            }
            let reserve = amounts.as_ref().map_or(0, |amounts| amounts.reserve);
            let funder = deployer.pubkey();
            let funder_holds = held(funder).await;
            println!(
                "  {:<20} {funder}  {}  (the Solana funder: it pays for the programs and funds the roles; the roles are short {}, the reserve is {}{})",
                "FUNDER (deployer)",
                funder_holds.map_or("?".to_string(), sol),
                sol(short_total),
                sol(reserve),
                match funder_holds {
                    Some(holds) if holds < short_total + reserve =>
                        format!("; it needs {} more", sol(short_total + reserve - holds)),
                    _ => String::new(),
                }
            );
            for (role, address, holds, short) in rows {
                println!(
                    "  {:<20} {address}  {}{}",
                    role.label(),
                    holds.map_or("?".to_string(), sol),
                    if short == 0 {
                        String::new()
                    } else {
                        format!("  (short {})", sol(short))
                    }
                );
            }
            let mint = solana::keypair_from_mnemonic(&phrase, devnet_keys::DOLLAR_MINT)?;
            println!("  {:<20} {}", "dollar (mint)", mint.pubkey());
            for (name, index) in devnet_keys::PROGRAM_KEYS {
                let program = solana::keypair_from_mnemonic(&phrase, index)?;
                println!("  {name:<20} {}", program.pubkey());
            }
        }
    }
    Ok(())
}

/// Every EVM chain of the network, which is every chain the contracts go on.
fn evm_chains(target: &Target) -> Vec<(ChainKey, ChainConfig)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    let mut chains: Vec<(ChainKey, ChainConfig)> = network
        .iter()
        .filter(|(_, key)| key.kind == ChainKind::Evm)
        .filter_map(|(_, key)| {
            let config = target.configs.get(key)?;
            Some((key.clone(), config.clone()))
        })
        .collect();
    chains.sort_by(|a, b| a.0.cmp(&b.0));
    chains.dedup_by(|a, b| a.0 == b.0);
    chains
}

/// The chains of `chains` the devnet works on: every one, or with a vault
/// chain named, only the chain it is.
fn only(
    target: &Target,
    one: bool,
    chains: Vec<(ChainKey, ChainConfig)>,
) -> Vec<(ChainKey, ChainConfig)> {
    chains
        .into_iter()
        .filter(|(key, _)| !one || key.same_chain(&target.key))
        .collect()
}

/// Every Solana cluster of the network.
fn solana_clusters(target: &Target) -> Vec<(ChainKey, ChainConfig)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    let mut clusters: Vec<(ChainKey, ChainConfig)> = network
        .iter()
        .filter(|(_, key)| key.kind == ChainKind::Solana)
        .filter_map(|(_, key)| Some((key.clone(), target.configs.get(key)?.clone())))
        .collect();
    clusters.sort_by(|a, b| a.0.cmp(&b.0));
    clusters.dedup_by(|a, b| a.0 == b.0);
    clusters
}

/// Each EVM chain of the network by the name `shonin.toml` uses and its chain
/// id, which is how a vault factory finds the chains a strategy trades on.
fn chain_names(target: &Target) -> Vec<(String, u64)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    network
        .iter()
        .filter_map(|(chain, key)| key.evm_chain_id().map(|id| (chain.name().to_string(), id)))
        .collect()
}

/// `name=value`, for the arguments that pair the two.
fn name_and_value(text: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .ok_or_else(|| format!("{text:?} is not name=value"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    const BASE: [&str; 5] = ["devnet", "--store", "s", "--artifacts", "a"];

    fn parse(more: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(BASE.iter().chain(more))
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Args::command().debug_assert();
    }

    #[test]
    fn the_seed_takes_its_take_on_time_and_runs_on_a_registered_strategy_without_an_api() {
        let Some(Command::Seed {
            strategy,
            take_on_seconds,
            ..
        }) = parse(&["seed", "--strategy", "7", "--take-on-seconds", "1500"])
            .unwrap()
            .command
        else {
            panic!("not the seed");
        };
        assert_eq!(strategy, Some(7));
        assert_eq!(take_on_seconds, 1500);
        let Some(Command::Seed {
            take_on_seconds, ..
        }) = parse(&["seed", "--api", "https://example.org"])
            .unwrap()
            .command
        else {
            panic!("not the seed");
        };
        assert_eq!(take_on_seconds, seed::TAKE_ON_SECONDS);
        // Publishing only and running live on a strategy don't mix.
        assert!(parse(&["seed", "--agent", "x", "--publish-only", "--live"]).is_err());
        assert!(parse(&["seed", "--strategy", "7", "--agent", "x"]).is_err());
    }

    #[test]
    fn the_market_demo_and_economics_commands_parse() {
        let Some(Command::Market { every_minutes }) = parse(&["market"]).unwrap().command else {
            panic!("not the market");
        };
        assert_eq!(every_minutes, 10);
        let Some(Command::Demo {
            agent,
            deposit,
            tank_days,
            ..
        }) = parse(&["demo", "--api", "https://example.org"])
            .unwrap()
            .command
        else {
            panic!("not the demo");
        };
        assert_eq!(
            (agent.as_str(), deposit, tank_days),
            ("funding-basis", 25_000, 30)
        );
        let Some(Command::Economics { hours, json, .. }) = parse(&[
            "economics",
            "--vault",
            "0x0000000000000000000000000000000000000001",
            "--json",
        ])
        .unwrap()
        .command
        else {
            panic!("not the economics");
        };
        assert_eq!(hours, 6.0);
        assert!(json);
    }

    #[test]
    fn the_admin_commands_parse() {
        assert!(matches!(
            parse(&[
                "propose-image",
                "--digest",
                "sha256:00",
                "--kind",
                "executor"
            ])
            .unwrap()
            .command,
            Some(Command::ProposeImage { .. })
        ));
        assert!(matches!(
            parse(&["propose-project", "shonin-testnet"])
                .unwrap()
                .command,
            Some(Command::ProposeProject { .. })
        ));
        assert!(matches!(
            parse(&["retire-checker", "--dev"]).unwrap().command,
            Some(Command::RetireChecker { dev: true })
        ));
        assert!(matches!(
            parse(&["allow-dev-checker"]).unwrap().command,
            Some(Command::AllowDevChecker)
        ));
        assert!(matches!(
            parse(&["root-key"]).unwrap().command,
            Some(Command::RootKey)
        ));
        assert!(matches!(
            parse(&["clear-root-key"]).unwrap().command,
            Some(Command::ClearRootKey)
        ));
    }

    #[test]
    fn the_admin_commands_reach_the_solana_registry_when_the_vault_chain_names_it() {
        for command in [
            vec![
                "propose-image",
                "--digest",
                "sha256:00",
                "--kind",
                "builder",
            ],
            vec!["revoke-image", "--digest", "sha256:00"],
            vec!["propose-project", "shonin-testnet"],
            vec!["retire-checker", "--dev"],
            vec!["allow-dev-checker"],
        ] {
            let args = parse(
                &[
                    ["--network", "testnet", "--vault-chain", "solana"].as_slice(),
                    &command,
                ]
                .concat(),
            )
            .unwrap();
            assert_eq!(args.chain_args.vault_chain, Some(shared::Chain::Solana));
            assert!(args.command.is_some());
        }
    }

    #[test]
    fn the_solana_set_up_needs_the_operator_s_project() {
        let Some(Command::SolanaSetup {
            project,
            orca_seed_sol,
        }) = parse(&["solana-setup", "--project", "shonin-testnet"])
            .unwrap()
            .command
        else {
            panic!("not the set-up");
        };
        assert_eq!(project, "shonin-testnet");
        assert_eq!(orca_seed_sol, 40.0);
        assert!(parse(&["solana-setup"]).is_err());
    }

    #[test]
    fn a_solana_keypair_is_written_for_the_deployer_the_registry_or_the_keeper_alone() {
        for role in ["deployer", "program", "keeper"] {
            assert!(matches!(
                parse(&["solana-keypair", "--role", role, "--out", "k.json"])
                    .unwrap()
                    .command,
                Some(Command::SolanaKeypair { .. })
            ));
        }
        assert!(parse(&["solana-keypair", "--role", "admin", "--out", "k.json"]).is_err());
    }
}
