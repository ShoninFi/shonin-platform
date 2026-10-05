//! A platform with three venues and a driver at each, on a Surfpool of its
//! own: the programs deployed from `contracts/solana/target/deploy` (built
//! by `task contracts:solana:build`), every role played by the development
//! keys. Without Surfpool 1.2.0 or later (`SHONIN_SURFPOOL`, or the one on
//! the path), the Solana CLI or the built programs, [`start`] says so and
//! answers nothing, so the test passes, as CI has no cluster.
//!
//! Each test file uses part of this, and the rest would be dead code to it.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chain::solana::keys::{self, DEVELOPMENT_MNEMONIC, RoleKeys};
use chain::solana::platform::{Declared, Platform, Registration, VaultAccounts, image_digest};
use chain::solana::programs::{common, local_venue, prices, publishing, registry, vault};
use chain::solana::{Keypair, Landed, Pubkey, Signer, SolanaClient, TransactionFailed, name};
use chain::{ChainConfig, ChainKey, ChainKind, Role};

pub const DEPLOYED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solana/target/deploy"
);
/// Every program the test runs, and whether it is deployed final, as every
/// checker and driver must be.
pub const PROGRAMS: [(&str, bool); 13] = [
    ("shonin_dev_verifier", true),
    ("shonin_local_driver", true),
    ("shonin_local_driver_2", true),
    ("shonin_local_driver_3", true),
    ("shonin_registry", false),
    ("shonin_publishing", false),
    ("shonin_prices", false),
    ("shonin_profiles", false),
    ("shonin_station", false),
    ("shonin_vault", false),
    ("shonin_local_venue", false),
    ("shonin_local_venue_2", false),
    ("shonin_local_venue_3", false),
];
pub const SPOT: &str = "SOL";
pub const PERP: &str = "SOL-PERP";
pub const PRICE: u64 = 150_000_000;
pub const LAMPORTS: u64 = 1_000_000_000_000;
pub const DOLLARS: u64 = 1_000_000_000_000;
pub const DEPOSIT: u64 = 30_000_000_000;
/// What goes to work at each venue.
pub const AT_EACH: u64 = 5_000_000_000;
pub const UNIT: u128 = 1_000_000_000_000_000_000;
pub const PATIENCE: Duration = Duration::from_secs(180);

/// A Surfpool cluster that is stopped when dropped.
pub struct Cluster {
    child: Child,
    pub dir: PathBuf,
    pub rpc: String,
    /// Every program deployed to it, and whether it is deployed final.
    programs: Vec<(&'static str, bool)>,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn surfpool() -> String {
    std::env::var("SHONIN_SURFPOOL").unwrap_or_else(|_| "surfpool".to_string())
}

/// A cluster of its own for a test that deploys `extra` programs besides
/// [`PROGRAMS`], or `None` (said) when it can't be had here.
pub fn start(extra: &[(&'static str, bool)]) -> Option<Cluster> {
    let Ok(version) = Command::new(surfpool()).arg("--version").output() else {
        eprintln!("surfpool is not installed: skipping");
        return None;
    };
    let version = String::from_utf8_lossy(&version.stdout).to_string();
    let numbers: Vec<u32> = version
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    if numbers.as_slice() < [1, 2].as_slice() {
        eprintln!("{} is older than 1.2.0: skipping", version.trim());
        return None;
    }
    if Command::new("solana").arg("--version").output().is_err() {
        eprintln!("the Solana CLI is not installed: skipping");
        return None;
    }
    let programs: Vec<(&'static str, bool)> = PROGRAMS.iter().chain(extra).copied().collect();
    if let Some((missing, _)) = programs
        .iter()
        .find(|(program, _)| !Path::new(DEPLOYED).join(format!("{program}.so")).exists())
    {
        eprintln!("{missing} is not built (`task contracts:solana:build`): skipping");
        return None;
    }
    let (rpc, ws) = (free_port(), free_port());
    let dir = std::env::temp_dir().join(format!("shonin-tables-{}-{rpc}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(surfpool())
        .args([
            "start",
            "--offline",
            "--no-tui",
            "--no-studio",
            "--no-deploy",
            "-y",
        ])
        .args(["--port", &rpc.to_string(), "--ws-port", &ws.to_string()])
        .args(["--log-path", "./logs"])
        .current_dir(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Cluster {
        child,
        dir,
        programs,
        rpc: format!("http://127.0.0.1:{rpc}"),
    })
}

pub async fn connect(cluster: &Cluster) -> SolanaClient {
    let config = ChainConfig {
        label: "Solana (test)".into(),
        rpc: cluster.rpc.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: cluster.rpc.clone(),
    };
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    SolanaClient::connect_with(
        &ChainKey::new(ChainKind::Solana, "localnet"),
        &config,
        Some(keys),
    )
    .await
    .unwrap()
}

pub fn program_id(program: &str) -> Pubkey {
    keys::read_keypair(&Path::new(DEPLOYED).join(format!("{program}-keypair.json")))
        .unwrap()
        .pubkey()
}

/// Each program through the loader, all at once.
pub fn deploy(cluster: &Cluster, deployer: &Keypair) {
    let deployer_file = cluster.dir.join("deployer.json");
    keys::write_keypair(deployer, &deployer_file).unwrap();
    let running: Vec<_> = cluster
        .programs
        .iter()
        .map(|(program, fixed)| {
            let mut command = Command::new("solana");
            command
                .args([
                    "program",
                    "deploy",
                    "--use-rpc",
                    "--commitment",
                    "confirmed",
                ])
                .arg("--url")
                .arg(&cluster.rpc)
                .arg("--keypair")
                .arg(&deployer_file)
                .arg("--program-id")
                .arg(Path::new(DEPLOYED).join(format!("{program}-keypair.json")))
                .arg(Path::new(DEPLOYED).join(format!("{program}.so")));
            if *fixed {
                command.arg("--final");
            }
            (*program, command.output())
        })
        .collect();
    for (program, output) in running {
        let output = output.unwrap();
        assert!(
            output.status.success(),
            "deploying {program} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The transaction's version, as the node keeps it.
pub async fn version_of(client: &SolanaClient, landed: &Landed) -> serde_json::Value {
    client
        .rpc
        .transaction(landed.signature())
        .await
        .unwrap()
        .expect("the node holds what landed")["version"]
        .clone()
}

pub struct Platformed {
    pub client: SolanaClient,
    pub platform: Platform,
    pub venues: [Pubkey; 3],
    pub drivers: [Declared; 3],
}

impl Platformed {
    pub fn role(&self, role: Role) -> &Keypair {
        self.client.role(role).unwrap()
    }

    /// Everything a cluster has before anyone publishes: the dollar, the
    /// development checker, every program set up, three venues with their
    /// markets and reserves, the images, the project and three drivers
    /// approved, and money handed out.
    pub async fn set_up(cluster: &Cluster) -> Self {
        let client = connect(cluster).await;
        let keys = client.keys().unwrap();
        let deployer = &keys.deployer;
        let mut everyone = vec![deployer.pubkey()];
        everyone.extend(ROLES.iter().map(|role| keys.address(*role)));
        for who in &everyone {
            client.airdrop(who, LAMPORTS).await.unwrap();
        }
        deploy(cluster, deployer);

        let admin = keys.get(Role::Admin);
        let operator = keys.get(Role::Operator);
        let mint = Keypair::new();
        let (create_checker, checker) = Platform::ix_create_checker(
            &deployer.pubkey(),
            keys.address(Role::Attestor),
            common::digest::keccak(&[b"shonin-dev-checker"]),
            &client.cluster,
        );
        let platform = Platform {
            cluster_name: client.cluster.clone(),
            cluster: client.cluster_value(),
            mint: mint.pubkey(),
            admin: admin.pubkey(),
            platform: keys.address(Role::Platform),
            operator: operator.pubkey(),
            checker: Some(checker),
        };
        let venues = [
            local_venue::ID,
            program_id("shonin_local_venue_2"),
            program_id("shonin_local_venue_3"),
        ];
        let drivers = [
            Declared::local(),
            Declared {
                name: name("local-2"),
                program: program_id("shonin_local_driver_2"),
            },
            Declared {
                name: name("local-3"),
                program: program_id("shonin_local_driver_3"),
            },
        ];

        let rent = client
            .rpc
            .rent_exempt_minimum(Platform::rent_for_mint())
            .await
            .unwrap();
        let ixs =
            Platform::ix_create_mint(&deployer.pubkey(), &mint.pubkey(), &admin.pubkey(), rent)
                .unwrap();
        client.send(ixs, &[deployer, &mint]).await.unwrap();
        client
            .send(vec![create_checker], &[deployer])
            .await
            .unwrap();
        let registry = Platform::ix_initialize_registry(
            &deployer.pubkey(),
            registry::InitializeArgs {
                admin: admin.pubkey(),
                cluster_name: client.cluster.clone(),
                timelock: 0,
                min_timelock: 0,
                blessing_ttl: 72 * 3600,
            },
            checker,
        );
        let terms = terms();
        for ix in [
            registry,
            platform.ix_initialize_publishing(&deployer.pubkey(), 1_000_000),
            platform.ix_initialize_prices(&deployer.pubkey(), 30 * 86_400, unit_prices()),
            platform.ix_initialize_stations(&deployer.pubkey()),
            platform.ix_initialize_vaults(&deployer.pubkey(), terms),
        ] {
            client.send(vec![ix], &[deployer]).await.unwrap();
        }
        for venue in &venues {
            client
                .send(
                    vec![platform.ix_initialize_venue_at(venue, &deployer.pubkey(), 1_000)],
                    &[deployer],
                )
                .await
                .unwrap();
            let list = vec![
                platform.ix_list_market_at(venue, name(SPOT), false, PRICE),
                platform.ix_list_market_at(venue, name(PERP), true, PRICE),
            ];
            client.send(list, &[operator]).await.unwrap();
            let reserves = platform
                .ix_mint_into(
                    &admin.pubkey(),
                    &local_venue::vault_address_of(venue),
                    DOLLARS * 10,
                )
                .unwrap();
            client.send(vec![reserves], &[admin]).await.unwrap();
        }

        let mut proposals = Vec::new();
        for (service, kind) in [
            ("executor", registry::state::ImageKind::Executor),
            ("builder", registry::state::ImageKind::Builder),
            ("reviewer", registry::state::ImageKind::Reviewer),
        ] {
            proposals.push(platform.ix_propose_image(image_digest(service), kind));
        }
        proposals.push(platform.ix_propose_project(chain::solana::platform::project_id()));
        client.send(proposals, &[admin]).await.unwrap();
        for driver in &drivers {
            let ix = platform
                .ix_propose(
                    &client,
                    registry::state::ApprovalKind::Driver,
                    driver.name,
                    driver.program,
                )
                .await
                .unwrap();
            client.send(vec![ix], &[admin]).await.unwrap();
        }

        let open: Vec<_> = ROLES
            .iter()
            .map(|role| platform.ix_open_dollars(&deployer.pubkey(), &keys.address(*role)))
            .collect();
        client.send(open, &[deployer]).await.unwrap();
        let minting: Vec<_> = [Role::Author, Role::Viewer, Role::Keeper]
            .iter()
            .map(|role| {
                platform
                    .ix_mint_to(&admin.pubkey(), &keys.address(*role), DOLLARS)
                    .unwrap()
            })
            .collect();
        client.send(minting, &[admin]).await.unwrap();
        Self {
            client,
            platform,
            venues,
            drivers,
        }
    }

    /// A strategy on the three drivers, registered and reviewed.
    pub async fn publish(&self) -> (u64, [u8; 32]) {
        let (author, builder, reviewer, attestor) = (
            self.role(Role::Author),
            self.role(Role::Builder),
            self.role(Role::Reviewer),
            self.role(Role::Attestor),
        );
        let digest = |part: &str| common::digest::keccak(&[part.as_bytes(), b"three venues"]);
        for (service, key) in [("builder", builder), ("reviewer", reviewer)] {
            self.platform
                .bless_service(&self.client, key, attestor, service, &key.pubkey())
                .await
                .unwrap();
        }
        let (submit, id) = self
            .platform
            .ix_submit(&self.client, &author.pubkey(), digest("upload"))
            .await
            .unwrap();
        self.client.send(vec![submit], &[author]).await.unwrap();
        let registration = Registration {
            id,
            author: author.pubkey(),
            source_hash: digest("source"),
            bundle_hash: digest("bundle"),
            manifest_hash: digest("manifest"),
            lists: publishing::state::Lists {
                chains: vec![publishing::state::ChainNeed {
                    chain: name("solana"),
                    drivers: self.drivers.iter().map(|driver| driver.name).collect(),
                }],
                bridges: vec![],
                restricted: vec![],
            },
        };
        self.platform
            .register(&self.client, builder, &registration, builder, reviewer)
            .await
            .unwrap();
        let review = publishing::state::ReviewInput {
            bundle_hash: registration.bundle_hash,
            source_hash: registration.source_hash,
            model_id_hash: digest("model"),
            prompt_hash: digest("prompt"),
            report_hash: digest("report"),
            ratings: [3; 6],
            finding_count: 0,
        };
        let ix = self
            .platform
            .ix_submit_review(
                &self.client,
                &reviewer.pubkey(),
                &reviewer.pubkey(),
                id,
                review,
            )
            .await
            .unwrap();
        self.client.send(vec![ix], &[reviewer]).await.unwrap();
        (id, registration.bundle_hash)
    }

    /// This slot's valuation, if one is due, and every settlement owed.
    pub async fn keep(&self, v: &VaultAccounts) -> Option<Landed> {
        let keeper = self.role(Role::Keeper);
        let valued = match self.platform.value(&self.client, keeper, v).await {
            Ok(landed) => Some(landed),
            Err(error) if error.downcast_ref::<TransactionFailed>().is_some() => None,
            Err(error) => panic!("a valuation could not be sent: {error:#}"),
        };
        self.platform
            .settle_all(&self.client, keeper, v)
            .await
            .unwrap();
        valued
    }
}

pub const ROLES: [Role; 10] = [
    Role::Admin,
    Role::Platform,
    Role::Attestor,
    Role::Builder,
    Role::Reviewer,
    Role::Executor,
    Role::Keeper,
    Role::Operator,
    Role::Author,
    Role::Viewer,
];

pub fn terms() -> vault::state::Terms {
    use vault::state::{Fees, Terms, Timers};
    Terms {
        fees: Fees {
            holding_bps: 100,
            author_bps: 1_000,
            platform_bps: 500,
            transaction_bps: 10,
        },
        exit_band_bps: 100,
        minimum_deposit: 10_000_000,
        minimum_tank: 40_000_000,
        dead_assets: 10_000,
        timers: Timers {
            slot_interval: 10,
            slot_lead: 5,
            slot_window: 3,
            fallback_after: 86_400,
            usage_freshness: 86_400,
            tank_escape_delay: 86_400,
            floor_delay: 86_400,
            halt_grace: 600,
            freeze_after: 7 * 86_400,
            wind_down: 7 * 86_400,
        },
    }
}

pub fn unit_prices() -> prices::UnitPrices {
    prices::UnitPrices {
        vcpu_hour: 150_000_000_000_000_000,
        gib_hour: 10_000_000_000_000_000,
        gb_egress: 150_000_000_000_000_000,
        inference_second: 150_000_000_000_000_000 / 3600,
    }
}
