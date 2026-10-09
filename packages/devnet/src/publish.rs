//! Publishing an agent, the way the web app does it: upload the project, pay
//! the $1 from the same wallet, follow the job, and, once it is reviewed and
//! built, activate it.
//!
//! Publishing and activating are separate on purpose. Nothing is spent but
//! the $1 until the author has read the review and chosen to open a vault.

use std::path::Path;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, bail};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::publishing::IPublishing;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::usd::ILocalUsd;
use chain::{Client, Deployments};
use shared::model::registry::{Job, JobState};
use shared::{Allowances, Strategy, Upload, UploadAccepted};

/// What a publish produced.
pub struct Published {
    pub job_id: String,
    pub outcome: JobState,
    /// Assigned when the $1 was paid.
    pub strategy_id: Option<u64>,
    /// Set once it is activated.
    pub vault: Option<Address>,
}

impl Published {
    pub fn slug(&self, chain: shared::Chain) -> Option<String> {
        self.strategy_id.map(|id| Strategy::slug_for(chain, id))
    }
}

/// Upload a project, pay for it, and follow it until it is ready, refused or
/// failed. If it is ready and `activate` is set, activate it too.
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    chain: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    project: &Path,
    author: Address,
    sealed: bool,
    activate: bool,
) -> Result<Published> {
    let archive = pack(project)?;
    let accepted = upload(
        api,
        vault_chain,
        &Upload {
            author: format!("{author:#x}"),
            archive_base64: shared::base64::encode(&archive),
            sealed,
            package: package_of(project),
        },
    )
    .await?;

    let reference: B256 = accepted.upload.parse().context("the upload's reference")?;
    pay(chain, deployments, author, reference).await?;

    let job = follow(api, &accepted.job_id).await?;
    let mut published = Published {
        job_id: accepted.job_id,
        outcome: job.state.clone(),
        strategy_id: job.strategy_id,
        vault: None,
    };
    if activate && job.state == JobState::Ready {
        let id = job.strategy_id.context("a ready job has an id")?;
        published.vault = Some(activate_it(chain, deployments, author, id).await?);
    }
    Ok(published)
}

/// Pay the $1 against the upload, from the wallet that uploaded it.
async fn pay(
    chain: &Client,
    deployments: &Deployments,
    author: Address,
    upload: B256,
) -> Result<()> {
    let fee = chain
        .read(deployments.publishing, IPublishing::feeCall {})
        .await?;
    chain
        .send(
            author,
            deployments.usd,
            ILocalUsd::approveCall {
                spender: deployments.publishing,
                value: fee,
            },
        )
        .await
        .context("the fee could not be approved")?;
    chain
        .send(
            author,
            deployments.publishing,
            IPublishing::submitCall { upload },
        )
        .await
        .context("the fee could not be paid")?;
    Ok(())
}

/// Wait for the review to be posted, then activate with the minimum tank and
/// the modest allowances.
pub async fn activate_it(
    chain: &Client,
    deployments: &Deployments,
    author: Address,
    id: u64,
) -> Result<Address> {
    let strategy_id = U256::from(id);
    for attempt in 0.. {
        let reviewed = chain
            .read(
                deployments.reviews,
                IReviewRegistry::hasReviewCall {
                    strategyId: strategy_id,
                },
            )
            .await?;
        if reviewed {
            break;
        }
        if attempt == 120 {
            bail!("the review of {id} was never posted");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let tank = chain
        .read(deployments.factory, IVaultFactory::configCall {})
        .await?
        .minimumTank;
    chain
        .send(
            author,
            deployments.usd,
            ILocalUsd::approveCall {
                spender: deployments.factory,
                value: tank,
            },
        )
        .await
        .context("the tank could not be approved")?;
    // The author signs the depositor's statement for the vault before it
    // exists, as every holder does, and names the terms they read.
    let vault = chain
        .read(
            deployments.factory,
            IVaultFactory::predictVaultCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let restricted = chain
        .read(
            deployments.factory,
            IVaultFactory::restrictedForCall {
                strategyId: strategy_id,
            },
        )
        .await?
        .text;
    let statement = chain.sign_statement(author, vault, &restricted).await?;
    let terms = chain
        .read(deployments.factory, IVaultFactory::termsCall {})
        .await?;
    chain
        .send(
            author,
            deployments.factory,
            IVaultFactory::activateCall {
                strategyId: strategy_id,
                allowances: chain::allowances(&Allowances::modest()),
                idleFloorBps: 500,
                tankFunding: tank,
                tankKeep: alloy::primitives::U256::ZERO,
                statement,
                terms,
            },
        )
        .await
        .context("the vault could not be activated")?;
    chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id,
            },
        )
        .await
        .context("reading the new vault")
}

/// How long an upload is offered for before the seed gives up: the build
/// service may be off (it runs only while there is work), and starting it,
/// on a first boot after a new image or root getting its key blessed too,
/// takes minutes.
const UPLOAD_WINDOW: Duration = Duration::from_secs(15 * 60);

/// How long to wait between offers when the API doesn't say.
const UPLOAD_PAUSE: Duration = Duration::from_secs(2);

/// The longest pause between offers the API may ask for.
const MOST_UPLOAD_PAUSE: Duration = Duration::from_secs(60);

/// How long a job is followed before the seed gives up: a review by a model
/// takes minutes, a build a minute or two more, and in the cloud the
/// reviewer and the build service may each have to be started first.
const FOLLOW_WINDOW: Duration = Duration::from_secs(60 * 60);

/// How long the queue may go unanswered while a job is followed.
const FOLLOW_UNANSWERED: Duration = Duration::from_secs(15 * 60);

/// Offer the project, waiting for the build service to be ready.
///
/// A service that starts with the chain waits out its image's timelock before
/// it can sign anything, and one in the cloud may be off until the upload
/// wakes it, so arriving early is ordinary rather than an error. The API's
/// `Retry-After` is honoured.
pub(crate) async fn upload(
    api: &str,
    chain: shared::Chain,
    upload: &Upload,
) -> Result<UploadAccepted> {
    upload_within(api, chain, upload, UPLOAD_WINDOW).await
}

async fn upload_within(
    api: &str,
    chain: shared::Chain,
    upload: &Upload,
    window: Duration,
) -> Result<UploadAccepted> {
    let client = reqwest::Client::new();
    let started = std::time::Instant::now();
    let mut said = false;
    loop {
        let reply = client
            .post(format!("{api}/api/{chain}/uploads"))
            .json(upload)
            .send()
            .await;
        let mut pause = UPLOAD_PAUSE;
        let last = match reply {
            Ok(reply) if reply.status().is_success() => {
                return reply
                    .json()
                    .await
                    .context("the build service said something unexpected");
            }
            // A refusal of the project itself won't change by waiting.
            Ok(reply) if reply.status() == reqwest::StatusCode::BAD_REQUEST => {
                bail!(
                    "the build service refused the project: {}",
                    reply.text().await.unwrap_or_default()
                );
            }
            Ok(reply) => {
                let status = reply.status();
                if let Some(asked) = retry_after(reply.headers()) {
                    pause = asked.min(MOST_UPLOAD_PAUSE);
                }
                format!(
                    "{status}: {}",
                    reply.text().await.unwrap_or_default().trim()
                )
            }
            Err(error) => error.to_string(),
        };
        let left = window.saturating_sub(started.elapsed());
        if left.is_zero() {
            bail!(
                "the build service never took the upload in {} minutes ({last})",
                window.as_secs() / 60
            );
        }
        if !said {
            tracing::info!(%last, "waiting for the build service");
            said = true;
        }
        tokio::time::sleep(pause.min(left)).await;
    }
}

/// The wait a reply asks for in seconds (`Retry-After`), if it asks for one.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Watch a job until it finishes. The queue is public, so this is the same
/// view anyone else has of it. A queue that doesn't answer for a while (the
/// API restarting, the build service off between steps) is waited out.
pub(crate) async fn follow(api: &str, job_id: &str) -> Result<Job> {
    follow_within(
        api,
        job_id,
        FOLLOW_WINDOW,
        FOLLOW_UNANSWERED,
        Duration::from_secs(1),
    )
    .await
}

async fn follow_within(
    api: &str,
    job_id: &str,
    window: Duration,
    unanswered: Duration,
    pause: Duration,
) -> Result<Job> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let started = std::time::Instant::now();
    let mut answered = std::time::Instant::now();
    while started.elapsed() < window {
        match ask_job(&client, api, job_id).await {
            Ok(job) if job.state.is_finished() => return Ok(job),
            Ok(_) => answered = std::time::Instant::now(),
            Err(error) => {
                if answered.elapsed() >= unanswered {
                    return Err(error.context(format!(
                        "the queue did not answer for job {job_id} in {} minutes",
                        unanswered.as_secs() / 60
                    )));
                }
                tracing::debug!(
                    error = format!("{error:#}"),
                    "the queue did not answer; asking again"
                );
            }
        }
        tokio::time::sleep(pause).await;
    }
    bail!(
        "job {job_id} is still running after {} minutes",
        window.as_secs() / 60
    )
}

/// The job as the queue has it now.
async fn ask_job(client: &reqwest::Client, api: &str, job_id: &str) -> Result<Job> {
    client
        .get(format!("{api}/api/jobs/{job_id}"))
        .send()
        .await
        .context("the queue did not answer")?
        .error_for_status()
        .context("the queue did not answer")?
        .json()
        .await
        .context("the queue said something unexpected")
}

/// The cargo package a project is, from its `Cargo.toml`.
pub(crate) fn package_of(project: &Path) -> Option<String> {
    let text = std::fs::read_to_string(project.join("Cargo.toml")).ok()?;
    text.lines().find_map(|line| {
        line.strip_prefix("name = ")
            .map(|name| name.trim().trim_matches('"').to_string())
    })
}

/// Pack the project the way the build service unpacks it: the whole tree the
/// agent lives in, so a workspace builds the same way here and there.
pub fn pack(project: &Path) -> Result<Vec<u8>> {
    let root = workspace_root(project);
    shared::archive::pack(&root).with_context(|| format!("packing {}", root.display()))
}

/// The directory a build has to start from: the workspace the agent is a
/// member of, if there is one.
///
/// An agent usually sits a few directories down from its workspace, with
/// nothing but folders in between, so the search keeps climbing past a
/// directory that holds no manifest at all.
fn workspace_root(project: &Path) -> std::path::PathBuf {
    let mut found = project.to_path_buf();
    let mut here = project.to_path_buf();
    for _ in 0..5 {
        let Some(parent) = here.parent().map(Path::to_path_buf) else {
            break;
        };
        let cargo = parent.join("Cargo.toml");
        if std::fs::read_to_string(&cargo)
            .map(|text| text.contains("[workspace]"))
            .unwrap_or(false)
        {
            found = parent.clone();
        }
        here = parent;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_packs_the_workspace_it_belongs_to() {
        let root = std::env::temp_dir().join(format!("shonin-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("packages/agents/one");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\nmembers = []\n").unwrap();
        std::fs::write(project.join("Cargo.toml"), b"[package]\nname = \"one\"\n").unwrap();
        assert_eq!(workspace_root(&project), root);
        assert_eq!(package_of(&project).as_deref(), Some("one"));
    }

    /// An API that answers each request with the next of `replies` (the
    /// last again once they run out): a status, a `Retry-After` if any, and
    /// a body.
    async fn an_api_answering(replies: Vec<(u16, Option<u64>, String)>) -> String {
        let replies = std::sync::Arc::new(std::sync::Mutex::new(replies));
        let app = axum::Router::new().fallback(move || {
            let replies = replies.clone();
            async move {
                let (status, after, body) = {
                    let mut replies = replies.lock().unwrap();
                    if replies.len() > 1 {
                        replies.remove(0)
                    } else {
                        replies[0].clone()
                    }
                };
                let mut answer = axum::response::Response::builder()
                    .status(status)
                    .header("content-type", "application/json");
                if let Some(after) = after {
                    answer = answer.header("retry-after", after.to_string());
                }
                answer.body(axum::body::Body::from(body)).unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn an_upload() -> Upload {
        Upload {
            author: "0x2222222222222222222222222222222222222222".to_string(),
            archive_base64: String::new(),
            sealed: false,
            package: None,
        }
    }

    fn a_job(state: JobState) -> String {
        serde_json::to_string(&Job {
            id: "8801".to_string(),
            chain: shared::Chain::Base,
            strategy_id: None,
            title: "funding basis".to_string(),
            author: "0x2222222222222222222222222222222222222222".to_string(),
            state,
            submitted: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 281).unwrap(),
                time::Time::MIDNIGHT,
            ),
            duration_seconds: 0,
            source_hash: String::new(),
            sealed: false,
            bundle_hash: None,
            review: None,
            strategy_slug: None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn an_upload_waits_as_long_as_the_api_asks_while_the_build_service_starts() {
        let accepted =
            serde_json::json!({"job_id": "8801", "upload": format!("0x{}", "11".repeat(32))})
                .to_string();
        let api = an_api_answering(vec![
            (503, Some(2), "the build service is starting".to_string()),
            (200, None, accepted),
        ])
        .await;
        let started = std::time::Instant::now();
        let taken = upload_within(
            &api,
            shared::Chain::Base,
            &an_upload(),
            Duration::from_secs(30),
        )
        .await;
        assert!(taken.is_ok(), "{taken:?}");
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "the Retry-After was honoured"
        );
    }

    #[tokio::test]
    async fn an_upload_is_given_up_on_at_the_end_of_its_window_and_a_refused_project_at_once() {
        let api = an_api_answering(vec![(503, Some(1), "starting".to_string())]).await;
        let started = std::time::Instant::now();
        let given_up = upload_within(
            &api,
            shared::Chain::Base,
            &an_upload(),
            Duration::from_secs(3),
        )
        .await;
        assert!(given_up.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));

        let api = an_api_answering(vec![(400, None, "not a project".to_string())]).await;
        let started = std::time::Instant::now();
        let refused = upload_within(
            &api,
            shared::Chain::Base,
            &an_upload(),
            Duration::from_secs(30),
        )
        .await;
        assert!(refused.unwrap_err().to_string().contains("not a project"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn following_a_job_waits_out_a_queue_that_does_not_answer_for_a_while() {
        let api = an_api_answering(vec![
            (502, None, "the build service is unavailable".to_string()),
            (200, None, a_job(JobState::Reviewing)),
            (503, None, String::new()),
            (200, None, "not a job".to_string()),
            (200, None, a_job(JobState::Ready)),
        ])
        .await;
        let job = follow_within(
            &api,
            "8801",
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert_eq!(job.state, JobState::Ready);

        // A queue that never answers is given up on once it has been silent
        // for the while allowed.
        let api = an_api_answering(vec![(503, None, String::new())]).await;
        let silent = follow_within(
            &api,
            "8801",
            Duration::from_secs(30),
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .await;
        assert!(silent.is_err());
    }

    #[test]
    fn an_agent_with_no_workspace_packs_itself() {
        let root = std::env::temp_dir().join(format!("shonin-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(workspace_root(&root), root);
    }
}
