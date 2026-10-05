//! The ops dashboard: what is running, on one local page.
//!
//! Every chain of the network (where it answers, which chain it says it is,
//! its head and its clock), every machine and service (whether `/health`
//! answers), each chain's deployment and roles, and, when a Google Cloud
//! project is configured, what is running there. Plain HTML, rendered on each
//! request and refreshed by the browser, so there is nothing to build and
//! nothing to go stale. It is served on localhost only and never deployed.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use chain::{ChainConfig, ChainConfigs, ChainKey, ChainKind, Deployments, NetworkName};
use serde_json::{Value, json};
use shared::Chain;
use tokio::sync::RwLock;

/// Everything the page needs to know that is not read live.
pub struct Dashboard {
    pub network: NetworkName,
    pub configs: ChainConfigs,
    pub deployments_dir: PathBuf,
    /// Each service's name and the URL that says it is up.
    pub services: Vec<(String, String)>,
    /// A Google Cloud project per network, where one is configured.
    pub gcp_projects: Vec<(String, String)>,
    pub gcp_region: Option<String>,
    /// What Google Cloud said last, refreshed in the background.
    cloud: RwLock<Vec<CloudReport>>,
    http: reqwest::Client,
}

/// One project's listing, as gcloud printed it.
#[derive(Clone)]
struct CloudReport {
    network: String,
    project: String,
    instances: std::result::Result<String, String>,
    services: std::result::Result<String, String>,
}

impl Dashboard {
    pub fn new(
        network: NetworkName,
        configs: ChainConfigs,
        deployments_dir: PathBuf,
        services: Vec<(String, String)>,
        gcp_projects: Vec<(String, String)>,
        gcp_region: Option<String>,
    ) -> Self {
        Self {
            network,
            configs,
            deployments_dir,
            services,
            gcp_projects,
            gcp_region,
            cloud: RwLock::new(Vec::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_millis(1_500))
                .build()
                .expect("an HTTP client"),
        }
    }
}

/// Serve the page, and keep the cloud listing fresh.
pub async fn serve(dashboard: Dashboard, address: SocketAddr) -> Result<()> {
    let dashboard = Arc::new(dashboard);

    if !dashboard.gcp_projects.is_empty() {
        let watcher = dashboard.clone();
        tokio::spawn(async move {
            loop {
                let reports = read_cloud(&watcher).await;
                *watcher.cloud.write().await = reports;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    let app = axum::Router::new()
        .route("/", get(page))
        .route("/health", get(|| async { "devnet" }))
        .with_state(dashboard);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("the dashboard could not listen on {address}"))?;
    tracing::info!("the dashboard is at http://localhost:{}/", address.port());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn page(State(dashboard): State<Arc<Dashboard>>) -> Html<String> {
    let chains = chains_of(&dashboard);
    let (chain_rows, service_rows) = tokio::join!(
        futures::future::join_all(chains.iter().map(|(vault_chain, key, config)| chain_row(
            &dashboard,
            *vault_chain,
            key,
            config
        )),),
        futures::future::join_all(
            dashboard
                .services
                .iter()
                .map(|(name, url)| service_row(&dashboard, name, url)),
        ),
    );
    let deployments: Vec<String> = chains
        .iter()
        .filter(|(_, key, _)| key.kind == ChainKind::Evm)
        .map(|(_, key, config)| deployment_section(&dashboard, key, config))
        .collect();
    let cloud = cloud_section(&dashboard).await;

    let mut html = String::new();
    html.push_str(&format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"refresh\" content=\"5\">\
         <title>Shonin ops · {network}</title><style>{STYLE}</style></head><body>\
         <header><h1>Shonin ops</h1><p>Network <b>{network}</b> · refreshes every 5 seconds · \
         local only, never deployed</p></header>",
        network = dashboard.network,
    ));

    html.push_str("<section><h2>Chains</h2><table><thead><tr><th>Vault chain</th><th>Key</th><th>Label</th><th>RPC</th><th>Says it is</th><th>Head</th><th>Chain time (UTC)</th></tr></thead><tbody>");
    for row in chain_rows {
        html.push_str(&row);
    }
    html.push_str("</tbody></table></section>");

    html.push_str("<section><h2>Machines and services</h2><table><thead><tr><th>Name</th><th>Port</th><th>Probe</th><th>Status</th></tr></thead><tbody>");
    for row in service_rows {
        html.push_str(&row);
    }
    html.push_str("</tbody></table></section>");

    html.push_str("<section><h2>Deployments</h2>");
    for section in deployments {
        html.push_str(&section);
    }
    html.push_str("</section>");

    html.push_str(&cloud);
    html.push_str("</body></html>");
    Html(html)
}

/// Every vault chain of the network, with its key and config, in a fixed
/// order.
fn chains_of(dashboard: &Dashboard) -> Vec<(Chain, ChainKey, ChainConfig)> {
    let Some(network) = dashboard.configs.network(dashboard.network) else {
        return Vec::new();
    };
    network
        .iter()
        .filter_map(|(vault_chain, key)| {
            let config = dashboard.configs.get(key)?;
            Some((vault_chain, key.clone(), config.clone()))
        })
        .collect()
}

async fn chain_row(
    dashboard: &Dashboard,
    vault_chain: Chain,
    key: &ChainKey,
    config: &ChainConfig,
) -> String {
    let probe = match key.kind {
        ChainKind::Evm => probe_evm(dashboard, &config.rpc).await,
        ChainKind::Solana => probe_solana(dashboard, &config.rpc).await,
    };
    let (identity, head, time, class) = match probe {
        Ok(probe) => {
            // A chain answering as someone else is as wrong as one not
            // answering at all.
            let class = match key.evm_chain_id() {
                Some(expected) if probe.identity != expected.to_string() => "bad",
                _ => "ok",
            };
            (
                probe.identity,
                probe.head.to_string(),
                probe.time.map(format_time).unwrap_or_else(|| "–".into()),
                class,
            )
        }
        Err(error) => (
            format!("not answering ({error})"),
            "–".into(),
            "–".into(),
            "bad",
        ),
    };
    format!(
        "<tr class=\"{class}\"><td>{}</td><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
        esc(vault_chain.name()),
        esc(&key.to_string()),
        esc(&config.label),
        esc(&config.rpc),
        esc(&identity),
        esc(&head),
        esc(&time),
    )
}

struct ChainProbe {
    /// The EVM chain id, or the Solana genesis hash.
    identity: String,
    head: u64,
    time: Option<i64>,
}

async fn rpc(dashboard: &Dashboard, url: &str, method: &str, params: Value) -> Result<Value> {
    let reply: Value = dashboard
        .http
        .post(url)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
        .map_err(|error| anyhow!(short(&error)))?
        .json()
        .await?;
    if let Some(error) = reply.get("error") {
        return Err(anyhow!("{method}: {error}"));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("{method}: no result"))
}

async fn probe_evm(dashboard: &Dashboard, url: &str) -> Result<ChainProbe> {
    let id = rpc(dashboard, url, "eth_chainId", json!([])).await?;
    let block = rpc(
        dashboard,
        url,
        "eth_getBlockByNumber",
        json!(["latest", false]),
    )
    .await?;
    let hex = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok())
    };
    Ok(ChainProbe {
        identity: hex(Some(&id)).map(|id| id.to_string()).unwrap_or_default(),
        head: hex(block.get("number")).unwrap_or(0),
        time: hex(block.get("timestamp")).map(|time| time as i64),
    })
}

async fn probe_solana(dashboard: &Dashboard, url: &str) -> Result<ChainProbe> {
    let genesis = rpc(dashboard, url, "getGenesisHash", json!([])).await?;
    let slot = rpc(dashboard, url, "getSlot", json!([]))
        .await?
        .as_u64()
        .unwrap_or(0);
    // The newest slot is not always a block yet, so its time can be missing.
    let time = rpc(dashboard, url, "getBlockTime", json!([slot]))
        .await
        .ok()
        .and_then(|time| time.as_i64());
    Ok(ChainProbe {
        identity: genesis.as_str().unwrap_or_default().to_string(),
        head: slot,
        time,
    })
}

async fn service_row(dashboard: &Dashboard, name: &str, url: &str) -> String {
    let port = url
        .parse::<reqwest::Url>()
        .ok()
        .and_then(|url| url.port_or_known_default())
        .map(|port| port.to_string())
        .unwrap_or_default();
    let (status, class) = match dashboard.http.get(url).send().await {
        // A health route says who it is; a page just says it is there.
        Ok(reply) if reply.status().is_success() => {
            let text = reply.text().await.unwrap_or_default();
            (said_with("up".to_string(), &text), "ok")
        }
        // A health route that answers it is not well says what is wrong on
        // its first line.
        Ok(reply) => {
            let status = reply.status();
            let text = reply.text().await.unwrap_or_default();
            (said_with(format!("answered {status}"), &text), "bad")
        }
        Err(error) => (format!("down ({})", short(&error)), "bad"),
    };
    format!(
        "<tr class=\"{class}\"><td>{}</td><td>{}</td><td><a href=\"{url}\"><code>{url}</code></a></td><td>{}</td></tr>",
        esc(name),
        esc(&port),
        esc(&status),
        url = esc(url),
    )
}

/// The most of a health route's first line a service's row shows.
const HEALTH_LINE: usize = 60;

/// What a service's row says: `status`, then the first line of what its
/// health route answered, cut to [`HEALTH_LINE`] characters if it runs
/// longer. A page (or nothing) adds nothing.
fn said_with(status: String, text: &str) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    if first.is_empty() || first.starts_with('<') {
        return status;
    }
    if first.chars().count() <= HEALTH_LINE {
        return format!("{status}: {first}");
    }
    let cut: String = first.chars().take(HEALTH_LINE - 1).collect();
    format!("{status}: {}…", cut.trim_end())
}

fn deployment_section(dashboard: &Dashboard, key: &ChainKey, config: &ChainConfig) -> String {
    let file = Deployments::file_in(&dashboard.deployments_dir, key);
    let title = format!(
        "<h3>{} <code>{}</code></h3><p class=\"muted\"><code>{}</code></p>",
        esc(&config.label),
        esc(&key.to_string()),
        esc(&file.display().to_string())
    );
    let deployments = match std::fs::read_to_string(&file)
        .map_err(|error| error.to_string())
        .and_then(|text| Deployments::from_json(&text).map_err(|error| error.to_string()))
    {
        Ok(deployments) => deployments,
        Err(error) => {
            return format!(
                "{title}<p class=\"bad\">No deployment here yet ({}).</p>",
                esc(&error)
            );
        }
    };

    let contracts: BTreeMap<&str, String> = [
        ("usd", deployments.usd),
        ("venue", deployments.venue),
        ("attestation verifier", deployments.attestation_verifier),
        ("attestation", deployments.attestation),
        ("strategies", deployments.strategies),
        ("reviews", deployments.reviews),
        ("profiles", deployments.profiles),
        ("factory", deployments.factory),
    ]
    .into_iter()
    .map(|(name, address)| (name, format!("{address:?}")))
    .collect();
    let roles = &deployments.roles;
    let roles: Vec<(&str, String)> = [
        ("admin", roles.admin),
        ("platform", roles.platform),
        ("attestor", roles.attestor),
        ("builder", roles.builder),
        ("reviewer", roles.reviewer),
        ("executor", roles.executor),
        ("keeper", roles.keeper),
        ("operator", roles.operator),
        ("author", roles.author),
        ("viewer", roles.viewer),
    ]
    .into_iter()
    .map(|(name, address)| (name, format!("{address:?}")))
    .collect();

    let table = |rows: Vec<(&str, String)>| {
        let mut out = String::from("<table class=\"pairs\"><tbody>");
        for (name, value) in rows {
            out.push_str(&format!(
                "<tr><th>{}</th><td><code>{}</code></td></tr>",
                esc(name),
                esc(&value)
            ));
        }
        out.push_str("</tbody></table>");
        out
    };

    format!(
        "{title}<p>Deployed at block {} on chain {}.</p><div class=\"columns\"><div><h4>Contracts</h4>{}</div><div><h4>Roles</h4>{}</div></div>",
        deployments.deployed_block,
        deployments.chain_id,
        table(contracts.into_iter().collect()),
        table(roles),
    )
}

async fn cloud_section(dashboard: &Dashboard) -> String {
    let mut html = String::from("<section><h2>Google Cloud</h2>");
    if dashboard.gcp_projects.is_empty() {
        html.push_str(
            "<p class=\"muted\">No Google Cloud project is configured, so there is nothing to \
             list. Set <code>GCP_PROJECT_TESTNET</code> or <code>GCP_PROJECT_MAINNET</code> in \
             <code>taskfiles/config.yml</code>; the gcloud default project is never used.</p>",
        );
        html.push_str("</section>");
        return html;
    }
    let reports = dashboard.cloud.read().await.clone();
    if reports.is_empty() {
        html.push_str("<p class=\"muted\">Asking gcloud…</p>");
    }
    for report in reports {
        html.push_str(&format!(
            "<h3>{} <code>{}</code></h3>",
            esc(&report.network),
            esc(&report.project)
        ));
        for (title, listing) in [
            ("Compute Engine instances", &report.instances),
            ("Cloud Run services", &report.services),
        ] {
            html.push_str(&format!("<h4>{title}</h4>"));
            match listing {
                Ok(text) if text.trim().is_empty() => html.push_str("<p class=\"muted\">None.</p>"),
                Ok(text) => html.push_str(&format!("<pre>{}</pre>", esc(text))),
                Err(error) => html.push_str(&format!("<p class=\"bad\">{}</p>", esc(error))),
            }
        }
    }
    html.push_str("</section>");
    html
}

async fn read_cloud(dashboard: &Dashboard) -> Vec<CloudReport> {
    let mut reports = Vec::new();
    for (network, project) in &dashboard.gcp_projects {
        let project_arg = format!("--project={project}");
        let instances = gcloud(&[
            "compute",
            "instances",
            "list",
            &project_arg,
            "--format=table(name,zone.basename(),machineType.basename(),status)",
        ])
        .await;
        let services = match &dashboard.gcp_region {
            Some(region) => {
                let region_arg = format!("--region={region}");
                gcloud(&[
                    "run",
                    "services",
                    "list",
                    &project_arg,
                    &region_arg,
                    "--format=table(metadata.name,status.url,status.conditions[0].status)",
                ])
                .await
            }
            None => Err("no region is configured (GCP_REGION)".to_string()),
        };
        reports.push(CloudReport {
            network: network.clone(),
            project: project.clone(),
            instances,
            services,
        });
    }
    reports
}

/// Run gcloud, always with an explicit project, never prompting.
async fn gcloud(args: &[&str]) -> std::result::Result<String, String> {
    let run = tokio::process::Command::new("gcloud")
        .args(args)
        .arg("--quiet")
        .output();
    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Err(_) => Err("gcloud took longer than 30 seconds".to_string()),
        Ok(Err(error)) => Err(format!("gcloud could not run: {error}")),
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
    }
}

fn format_time(timestamp: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(timestamp) {
        Ok(when) => format!(
            "{} {:02}:{:02}:{:02}",
            when.date(),
            when.hour(),
            when.minute(),
            when.second()
        ),
        Err(_) => timestamp.to_string(),
    }
}

/// The part of a connection error worth a table cell.
fn short(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".to_string()
    } else if error.is_connect() {
        "refused".to_string()
    } else {
        error.to_string()
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = "
:root { color-scheme: light dark; --ok: #0d9488; --bad: #dc2626; --muted: #6b7280; --line: #d1d5db; }
body { font: 14px/1.45 system-ui, sans-serif; margin: 0 auto; padding: 16px; max-width: 1200px; }
h1 { margin: 0; font-size: 22px; } h2 { margin-top: 28px; font-size: 17px; } h3 { font-size: 15px; margin-bottom: 4px; }
h4 { font-size: 13px; margin: 8px 0 4px; color: var(--muted); }
header p, .muted { color: var(--muted); }
table { border-collapse: collapse; width: 100%; font-size: 13px; }
th, td { text-align: left; padding: 4px 8px; border-bottom: 1px solid var(--line); vertical-align: top; }
tr.ok td:first-child { border-left: 3px solid var(--ok); } tr.bad td:first-child { border-left: 3px solid var(--bad); }
tr.bad td, p.bad { color: var(--bad); }
table.pairs th { width: 11em; font-weight: 500; }
code, pre { font: 12px ui-monospace, monospace; } pre { overflow-x: auto; padding: 8px; border: 1px solid var(--line); }
.columns { display: grid; grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); gap: 16px; }
section { overflow-x: auto; }
";

#[cfg(test)]
mod tests {
    use super::*;

    /// A service with much wrong says the most on its first line: a row
    /// shows that line cut to fit rather than dropping it.
    #[test]
    fn a_health_line_too_long_for_its_row_is_cut_not_dropped() {
        let long = format!("keeper: {}", "x".repeat(80));
        let said = said_with("answered 503".to_string(), &format!("{long}\ndetails"));
        let shown = said.strip_prefix("answered 503: ").expect(&said);
        assert_eq!(shown.chars().count(), HEALTH_LINE);
        assert!(
            shown.starts_with("keeper: xxx") && shown.ends_with('…'),
            "{said}"
        );
        assert_eq!(said_with("up".to_string(), &long), format!("up: {shown}"));

        assert_eq!(
            said_with(
                "answered 503".to_string(),
                "keeper: 2 chains not kept; +2 more\nmore"
            ),
            "answered 503: keeper: 2 chains not kept; +2 more"
        );
        assert_eq!(said_with("up".to_string(), "<!doctype html>"), "up");
        assert_eq!(said_with("answered 502".to_string(), ""), "answered 502");
    }
}
