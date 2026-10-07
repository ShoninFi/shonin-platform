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
mod solana;
mod solana_seed;

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
    /// addresses, then stop. Local and testnet only.
    Deploy,
    /// Show the funder and each role's account on every EVM chain of the
    /// network, with what each holds for gas and what it is short, and the
    /// Solana deployer, roles and programs: what to fund before a testnet
    /// deploy.
    Accounts,
    /// Deploy the Confidential Space checker on every EVM chain of the
    /// network and propose it to the registry beside the development
    /// checker. Already proposed is done.
    ProposeChecker,
    /// Propose a real image digest to every EVM chain's registry, signed
    /// by the admin. Already proposed is done.
    ProposeImage {
        /// `sha256:<64 hex>`, as the container registry names the image.
        #[arg(long)]
        digest: String,
        /// executor, builder, reviewer or unwinder.
        #[arg(long)]
        kind: String,
    },
    /// Revoke an image digest on every EVM chain's registry, signed by the
    /// admin: at once and for good. Already revoked is done.
    RevokeImage {
        /// `sha256:<64 hex>`, as the container registry names the image.
        #[arg(long)]
        digest: String,
    },
    /// Propose an operator project (by its Google Cloud project id; the
    /// registry holds its keccak256) to every EVM chain's registry, signed
    /// by the admin. Already proposed is done.
    ProposeProject {
        /// The Google Cloud project id.
        project: String,
    },
    /// Retire the development checker on every EVM chain, signed by the
    /// admin; refused on a chain where no other checker is allowed. Already
    /// retired is done.
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
    /// Allow a development checker again on every EVM chain (a fresh one,
    /// since a retired checker can't be proposed again), for a publish only
    /// it can bless. Already allowed is done. Retire it again after.
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
    /// Write a Solana keypair derived from the testnet Solana mnemonic.
    SolanaKeypair {
        /// Which: the deployer, or the registry's own address.
        #[arg(long, value_parser = ["deployer", "program"])]
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
    /// again. Every EVM chain of the network at once, or `--vault-chain`'s.
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
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    let target = args.chain_args.resolve()?;
    let local = target.network == NetworkName::Local;
    // A vault chain named on the command line: the devnet deploys to and
    // drives that one alone.
    let one = args.chain_args.vault_chain.is_some();
    let changes_chains = matches!(
        args.command,
        Some(
            Command::Deploy
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
                let keeps = deploy::still_there(&client, &file).await.is_some();
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
            for (key, client, file, _) in &chains {
                deploy::ensure(
                    client,
                    target.network,
                    key,
                    &args.artifacts,
                    file,
                    &names,
                    generation,
                )
                .await?;
            }
            if local {
                for (key, config) in only(&target, one, solana_clusters(&target)) {
                    let client = chain::connect_solana(target.network, &key, &config).await?;
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    solana::ensure(&client, &key, &config.rpc, &args.solana_programs, &file)
                        .await?;
                }
            }
            Ok(())
        }
        Some(Command::Accounts) => accounts(&target, &args.funding).await,
        Some(Command::ProposeChecker) => {
            checker::run(
                target.network,
                only(&target, one, evm_chains(&target)),
                &target.deployments_dir,
                &args.artifacts,
            )
            .await
        }
        Some(Command::ProposeImage { digest, kind }) => {
            let digest = admin::image_digest(&digest)?;
            let kind = admin::kind_named(&kind)?;
            on_each_registry(&target, one, |client, deployments| async move {
                admin::propose_image(&client, &deployments, digest, kind).await
            })
            .await
        }
        Some(Command::RevokeImage { digest }) => {
            let digest = admin::image_digest(&digest)?;
            on_each_registry(&target, one, |client, deployments| async move {
                admin::revoke_image(&client, &deployments, digest).await
            })
            .await
        }
        Some(Command::ProposeProject { project }) => {
            on_each_registry(&target, one, |client, deployments| {
                let project = project.clone();
                async move { admin::propose_project(&client, &deployments, &project).await }
            })
            .await
        }
        Some(Command::RetireChecker { dev }) => {
            if !dev {
                bail!("only the development checker is retired here: pass --dev");
            }
            on_each_registry(&target, one, |client, deployments| async move {
                admin::retire_dev_checker(&client, &deployments).await
            })
            .await
        }
        Some(Command::AllowDevChecker) => {
            let artifacts = args.artifacts.clone();
            on_each_registry(&target, one, |client, deployments| {
                let artifacts = artifacts.clone();
                async move { admin::allow_dev_checker(&client, &deployments, &artifacts).await }
            })
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
            let index = if role == "program" {
                solana::PROGRAM
            } else {
                solana::DEPLOYER
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
        Some(Command::Seed { .. }) if target.key.kind == ChainKind::Solana => {
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
            for (key, config) in only(&target, one, evm_chains(&target)) {
                let network = target.network;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                running.spawn(async move {
                    let client = connect_to(network, &key, &config).await?;
                    let deployments = client.deployments(&file).await.with_context(|| {
                        format!("no deployment on {key}: run chains:deploy first")
                    })?;
                    market::operate(&client, &deployments, every, market::TapeParams::TESTNET)
                        .await
                        .map_err(|error| error.context(format!("the market on {key}")))
                });
            }
            while let Some(finished) = running.join_next().await {
                finished??;
            }
            Ok(())
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
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    let network = target.network;
                    // A cluster that can't be brought up is said, and the
                    // EVM chains carry on: Solana needs tools of its own.
                    running.spawn(async move {
                        let driven = async {
                            let client = chain::connect_solana(network, &key, &config).await?;
                            let deployments =
                                solana::ensure(&client, &key, &config.rpc, &programs, &file)
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

/// Run one admin change against every EVM chain's registry of the network
/// (or the one named), each from its deployment file, and say what it did.
async fn on_each_registry<F, Fut>(target: &Target, one: bool, change: F) -> Result<()>
where
    F: Fn(chain::Client, Deployments) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    for (key, config) in only(target, one, evm_chains(target)) {
        let client = connect_to(target.network, &key, &config).await?;
        let file = Deployments::file_in(&target.deployments_dir, &key);
        let deployments = client
            .deployments(&file)
            .await
            .with_context(|| format!("no deployment on {key}: run chains:deploy first"))?;
        let said = change(client, deployments)
            .await
            .with_context(|| format!("on {} ({key})", config.label))?;
        println!("{} ({key}): {said}", config.label);
    }
    Ok(())
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
        let phrase = solana::testnet_mnemonic()?;
        let deployer = solana::keypair_from_mnemonic(&phrase, solana::DEPLOYER)?;
        println!("Solana devnet");
        println!(
            "  {:<20} {}  (the Solana funder: it pays for the programs and funds the roles)",
            "FUNDER (deployer)",
            chain::solana::Signer::pubkey(&deployer)
        );
        for role in fund::ROLES {
            let index = chain::solana::keys::ROLE_BASE + role.account_index() as u32;
            let pair = solana::keypair_from_mnemonic(&phrase, index)?;
            println!(
                "  {:<20} {}",
                role.label(),
                chain::solana::Signer::pubkey(&pair)
            );
        }
        for (name, index) in devnet_keys::PROGRAM_KEYS {
            let program = solana::keypair_from_mnemonic(&phrase, index)?;
            println!("  {name:<20} {}", chain::solana::Signer::pubkey(&program));
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
}
