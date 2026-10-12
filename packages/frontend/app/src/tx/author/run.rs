//! Starting an author flow from a page: what it acts on, the steps it
//! reaches as a signal, and a cancel the wallet session also pulls when the
//! account changes. In the browser a flow reads through the vault chain's
//! public endpoint and signs with the connected wallet. Also the API reads
//! the author's pages make: the platform's addresses on a chain, a
//! publishing job, an upload, and the executor's signed bill.

use alloy_primitives::{Address, B256, U256};
use futures_signals::signal::Mutable;
use shared::Chain;

use super::flow::{Activation, Step};
use super::form::Allowances;
use super::read::Registries;
use crate::tx::flow::Cancel;

/// What a flow does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Pay {
        upload: B256,
    },
    Activate(Activation),
    FundTank {
        vault: Address,
        amount: U256,
    },
    SetKeep {
        vault: Address,
        amount: U256,
    },
    TopUpFromShares {
        vault: Address,
        shares: U256,
    },
    SetAllowances {
        vault: Address,
        allowances: Allowances,
    },
    SetIdleFloor {
        vault: Address,
        bps: u16,
    },
    CancelFloorChange {
        vault: Address,
    },
    Retire {
        vault: Address,
    },
    RequestEscape {
        vault: Address,
    },
    WithdrawUnsigned {
        vault: Address,
        amount: U256,
    },
    /// Settle the bill with the executor's latest signed total, then pay
    /// the rest to the author.
    WithdrawTank {
        vault: Address,
        amount: U256,
    },
}

/// A flow in progress: its latest step, and its cancel.
#[derive(Clone)]
pub struct Running {
    pub step: Mutable<Option<Step>>,
    pub cancel: Cancel,
}

impl Running {
    pub fn new() -> Self {
        Self {
            step: Mutable::new(None),
            cancel: Cancel::new(),
        }
    }

    /// Whether it is past checking and not yet ended: the wallet is being
    /// asked, or a transaction is on its way.
    pub fn is_live(&self) -> bool {
        matches!(
            self.step.lock_ref().as_ref(),
            Some(Step::Checking | Step::Signing(_) | Step::Sent { .. })
        )
    }
}

impl Default for Running {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the platform's contracts are on one EVM chain, from its config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub publishing: Address,
    pub registries: Registries,
    pub usd: Address,
}

impl Platform {
    pub fn from_config(config: &shared::Config) -> Option<Self> {
        let evm = config.addresses.evm()?;
        let parse = |text: &str| text.parse::<Address>().ok();
        Some(Self {
            publishing: parse(&evm.publishing)?,
            registries: Registries {
                factory: parse(&evm.factory)?,
                strategies: parse(&evm.strategies)?,
                reviews: parse(&evm.reviews)?,
                prices: parse(&evm.prices)?,
            },
            usd: parse(&evm.usd)?,
        })
    }
}

/// Why an upload wasn't taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The build service refused the project itself (its words).
    Refused(String),
    /// Too big for the API to carry.
    TooLarge,
    /// The build service is starting: try again after this many seconds.
    Starting { retry_after: u32 },
    /// Anything else: the API's words, or that it didn't answer.
    Failed(String),
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use alloy_primitives::{Address, Bytes, U256};
    use shared::{Chain, Job, Upload, UploadAccepted};

    use super::{Action, Platform, Running, UploadError};
    use crate::api::BASE;
    use crate::tx::TxError;
    use crate::tx::author::error::AuthorError;
    use crate::tx::author::flow::{self, Ctx, Outcome, Step, Usage};
    use crate::tx::flow::Pace;
    use crate::wallet::run::Timer;
    use crate::wallet::session::Session;

    thread_local! {
        static PLATFORMS: RefCell<BTreeMap<Chain, Rc<Platform>>> = const { RefCell::new(BTreeMap::new()) };
    }

    async fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
        let reply = gloo_net::http::Request::get(&format!("{BASE}/{path}"))
            .send()
            .await
            .map_err(|_| "the platform didn't answer".to_string())?;
        if !reply.ok() {
            return Err(format!("the platform answered {}", reply.status()));
        }
        reply
            .json()
            .await
            .map_err(|_| "the platform said something unexpected".to_string())
    }

    /// The platform's contracts on a chain, read once per page load.
    pub async fn platform(chain: Chain) -> Option<Rc<Platform>> {
        if let Some(found) = PLATFORMS.with(|map| map.borrow().get(&chain).cloned()) {
            return Some(found);
        }
        let config: shared::Config = get(&format!("{chain}/config")).await.ok()?;
        let platform = Rc::new(Platform::from_config(&config)?);
        PLATFORMS.with(|map| map.borrow_mut().insert(chain, platform.clone()));
        Some(platform)
    }

    /// One publishing job, as the queue has it now.
    pub async fn job(id: &str) -> Result<Option<Job>, String> {
        let path = format!("jobs/{}", crate::route::encode(id));
        let reply = gloo_net::http::Request::get(&format!("{BASE}/{path}"))
            .send()
            .await
            .map_err(|_| "the platform didn't answer".to_string())?;
        if reply.status() == 404 {
            return Ok(None);
        }
        if !reply.ok() {
            return Err(format!("the platform answered {}", reply.status()));
        }
        reply
            .json()
            .await
            .map(Some)
            .map_err(|_| "the platform said something unexpected".to_string())
    }

    /// Every job the queue holds.
    pub async fn jobs() -> Result<Vec<Job>, String> {
        get("jobs").await
    }

    /// Offer a project to the build service, for the paying wallet.
    pub async fn upload(chain: Chain, upload: &Upload) -> Result<UploadAccepted, UploadError> {
        let request = gloo_net::http::Request::post(&format!("{BASE}/{chain}/uploads"))
            .json(upload)
            .map_err(|_| UploadError::Failed("the upload couldn't be prepared".to_string()))?;
        let reply = request
            .send()
            .await
            .map_err(|_| UploadError::Failed("the platform didn't answer".to_string()))?;
        let status = reply.status();
        if reply.ok() {
            return reply.json().await.map_err(|_| {
                UploadError::Failed("the build service's answer couldn't be read".to_string())
            });
        }
        let retry_after = reply
            .headers()
            .get("retry-after")
            .and_then(|value| value.trim().parse::<u32>().ok());
        let text = reply.text().await.unwrap_or_default();
        Err(match status {
            413 => UploadError::TooLarge,
            503 => UploadError::Starting {
                retry_after: retry_after.unwrap_or(2).clamp(1, 60),
            },
            400..=499 => UploadError::Refused(message(&text)),
            _ => UploadError::Failed(message(&text)),
        })
    }

    /// The API's refusal as text: its `message` field, or the body itself.
    fn message(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("message")?.as_str().map(str::to_string))
            .unwrap_or_else(|| body.trim().to_string())
    }

    #[derive(serde::Deserialize)]
    struct SignedTotal {
        total: String,
        as_of: u64,
        signature: String,
    }

    /// The executor's latest signed bill for a vault, if it serves one.
    async fn usage(chain: Chain, vault: Address) -> Result<Usage, AuthorError> {
        let signed: SignedTotal = get(&format!("{chain}/usage/{vault:?}"))
            .await
            .map_err(|_| AuthorError::NoUsageTotal)?;
        let total = signed
            .total
            .parse::<U256>()
            .map_err(|_| AuthorError::NoUsageTotal)?;
        let signature = crate::wallet::rpc::hex_bytes(&signed.signature)
            .map_err(|_| AuthorError::NoUsageTotal)?;
        Ok(Usage {
            total,
            as_of: signed.as_of,
            signature: Bytes::from(signature),
        })
    }

    /// Start `action` on `chain`, reporting into `running`; `done` runs with
    /// the outcome once the chain has confirmed it.
    pub fn start(
        chain: Chain,
        action: Action,
        running: &Running,
        done: impl FnOnce(&Outcome) + 'static,
    ) {
        let session = crate::wallet::get();
        let running = running.clone();
        running.step.set(Some(Step::Checking));
        session.track(running.cancel.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let step = running.step.clone();
            let on_step = move |next: Step| step.set(Some(next));
            let fail = |error: AuthorError| on_step(Step::Failed(error));
            let Some(account) = session.account() else {
                fail(TxError::NotConnected.into());
                return;
            };
            let (Some(network), Some(platform)) =
                (session.network(chain).await, platform(chain).await)
            else {
                fail(
                    TxError::Network("this chain isn't reachable from this app".to_string()).into(),
                );
                return;
            };
            let reader = Session::reader(&network);
            let signer = session.signer(network.clone());
            let ctx = Ctx {
                reader: &reader,
                signer: &signer,
                clock: &Timer,
                chain_id: network.chain_id,
                account,
                cancel: running.cancel.clone(),
                pace: Pace::LIVE,
                on_step: &on_step,
            };
            let asset = network.asset;
            let outcome = match action {
                Action::Pay { upload } => {
                    flow::report(&on_step, flow::pay(&ctx, platform.publishing, upload)).await
                }
                Action::Activate(chosen) => {
                    flow::report(
                        &on_step,
                        flow::activate(&ctx, &platform.registries, asset, &chosen),
                    )
                    .await
                }
                Action::FundTank { vault, amount } => {
                    flow::report(&on_step, flow::fund_tank(&ctx, vault, asset, amount)).await
                }
                Action::SetKeep { vault, amount } => {
                    flow::report(&on_step, flow::set_keep(&ctx, vault, amount)).await
                }
                Action::TopUpFromShares { vault, shares } => {
                    flow::report(&on_step, flow::top_up_from_shares(&ctx, vault, shares)).await
                }
                Action::SetAllowances { vault, allowances } => {
                    flow::report(&on_step, flow::set_allowances(&ctx, vault, &allowances)).await
                }
                Action::SetIdleFloor { vault, bps } => {
                    flow::report(&on_step, flow::set_idle_floor(&ctx, vault, bps)).await
                }
                Action::CancelFloorChange { vault } => {
                    flow::report(&on_step, flow::cancel_floor_change(&ctx, vault)).await
                }
                Action::Retire { vault } => flow::report(&on_step, flow::retire(&ctx, vault)).await,
                Action::RequestEscape { vault } => {
                    flow::report(&on_step, flow::request_escape(&ctx, vault)).await
                }
                Action::WithdrawUnsigned { vault, amount } => {
                    flow::report(&on_step, flow::withdraw_unsigned(&ctx, vault, amount)).await
                }
                Action::WithdrawTank { vault, amount } => {
                    let signed = usage(chain, vault).await;
                    flow::report(&on_step, async {
                        let signed = signed?;
                        flow::withdraw_tank(&ctx, vault, amount, &signed).await
                    })
                    .await
                }
            };
            if let Some(outcome) = outcome {
                session.changed.replace_with(|n| *n + 1);
                done(&outcome);
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::rc::Rc;

    use shared::Chain;

    use super::{Action, Platform, Running};
    use crate::tx::TxError;
    use crate::tx::author::flow::{Outcome, Step};

    /// Without a browser nothing runs.
    pub fn start(
        _chain: Chain,
        _action: Action,
        running: &Running,
        _done: impl FnOnce(&Outcome) + 'static,
    ) {
        running
            .step
            .set(Some(Step::Failed(TxError::NotConnected.into())));
    }

    pub async fn platform(_chain: Chain) -> Option<Rc<Platform>> {
        None
    }
}

/// A slug as the API writes it, `base-7`: the chain and the strategy id.
pub fn parse_slug(slug: &str) -> Option<(Chain, u64)> {
    let (chain, id) = slug.rsplit_once('-')?;
    let chain: Chain = chain.parse().ok()?;
    let id: u64 = id.parse().ok()?;
    (id > 0 && shared::Strategy::slug_for(chain, id) == slug).then_some((chain, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_names_its_chain_and_strategy_and_nothing_else_reads_as_one() {
        assert_eq!(parse_slug("base-7"), Some((Chain::Base, 7)));
        assert_eq!(parse_slug("hyperliquid-12"), Some((Chain::Hyperliquid, 12)));
        assert_eq!(parse_slug("base-0"), None);
        assert_eq!(parse_slug("base-07"), None);
        assert_eq!(parse_slug("nowhere-7"), None);
        assert_eq!(parse_slug("base"), None);
    }
}
