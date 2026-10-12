//! Conservative quiet-vault observations. Every action still uses the keeper's
//! ordinary fresh reads; a missing proof leaves that vault on its active path.

use alloy::eips::BlockId;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::SolCall;
use anyhow::{Context, Result, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::driver::{ExitPath, IDriver};
use chain::bindings::station::{IStation, IStationFactory, IStationVault};
use chain::bindings::vault::{ClaimState, IVault, VaultState};
use chain::sealed_views::{Proofs, SealedViews};
use chain::{Client, Deployments};
use std::time::Instant;

pub struct QuietInput {
    pub daily_at: u64,
    pub logs_read_to: u64,
}

pub struct Hint {
    pub head: (u64, B256),
    pub due: u64,
    pub addresses: Vec<Address>,
    pub answered: Option<Instant>,
}

/// Ordinary (no override) reads on the same endpoint and exact hash whose
/// pin semantics the sealed observation has just proved. Never fail over.
async fn pinned<C: SolCall>(
    endpoint: &DynProvider,
    address: Address,
    hash: B256,
    limit: usize,
    call: C,
    remaining: &mut u64,
) -> Result<C::Return> {
    if *remaining == 0 {
        bail!("a quiet observation exceeds its request bound");
    }
    *remaining -= 1;
    let output = endpoint
        .call(
            TransactionRequest::default()
                .with_from(Address::ZERO)
                .with_to(address)
                .with_input(call.abi_encode()),
        )
        .block(BlockId::hash_canonical(hash))
        .await?;
    if output.len() > limit {
        bail!("a quiet observation exceeds its output bound");
    }
    Ok(C::abi_decode_returns(&output)?)
}

/// This whole optional probe is bounded by the caller's read budget and runs
/// off the action loop. It cannot cancel a send or delay another vault.
pub async fn candidate(
    client: &Client,
    deployments: &Deployments,
    vault: Address,
    observed: (u64, u64, B256),
    input: QuietInput,
    answered: Option<Instant>,
) -> Result<Option<Hint>> {
    if input.logs_read_to <= observed.0 {
        return Ok(None);
    }
    let views = SealedViews::observe_quiet(client, vault, observed, &mut Proofs::default()).await?;
    macro_rules! get {
        ($call:ident) => {
            views
                .observed(vault, &IVault::$call {})
                .context("an incomplete quiet observation")?
        };
    }
    macro_rules! station_get {
        ($call:ident) => {
            views
                .observed(vault, &IStationVault::$call {})
                .context("an incomplete quiet observation")?
        };
    }
    if !matches!(get!(stateCall), VaultState::Open)
        || station_get!(haltedAtCall) != 0
        || station_get!(valuingCall)
        || station_get!(unsettledCall)
        || get!(needsValuationFirstCall)
        || get!(nextUnpricedCall) != 0
    {
        return Ok(None);
    }
    crate::validate_timers(client, vault, &get!(timersCall))?;
    let tank = get!(tankCall);
    let units = get!(unitsCall);
    let count = get!(claimCountCall);
    let priced = get!(lastPricedCall);
    if tank.escapeRequestedAt != 0
        || tank.fillPending
        || !units.total.is_zero()
        || !units.reserve.is_zero()
        || !priced.priced
    {
        return Ok(None);
    }
    let mut remaining = client.rpc_policy().view_batch_reads;
    let endpoint = client
        .each_endpoint()
        .into_iter()
        .next()
        .context("no dedicated read endpoint")?;
    let limit = client.rpc_policy().view_result_bytes as usize;
    // A terminal prefix remembered by another round cannot certify this
    // numbered observation. Prove every claim here, or decline on bounds.
    let count: u64 = count
        .try_into()
        .context("too many claims to certify quietness")?;
    if count > client.rpc_policy().history_page_reads {
        return Ok(None);
    }
    for id in 1..=count {
        let claim = pinned(
            &endpoint,
            vault,
            observed.2,
            limit,
            IVault::claimAtCall {
                claimId: U256::from(id),
            },
            &mut remaining,
        )
        .await?;
        if !matches!(claim.state, ClaimState::Paid | ClaimState::Fallback) {
            return Ok(None);
        }
    }
    let station = get!(homeStationCall);
    let factory = pinned(
        &endpoint,
        station,
        observed.2,
        limit,
        IStation::factoryCall {},
        &mut remaining,
    )
    .await?;
    let registry = pinned(
        &endpoint,
        factory,
        observed.2,
        limit,
        IStationFactory::attestationCall {},
        &mut remaining,
    )
    .await?;
    let asset = pinned(
        &endpoint,
        factory,
        observed.2,
        limit,
        IStationFactory::assetCall {},
        &mut remaining,
    )
    .await?;
    let spec = pinned(
        &endpoint,
        station,
        observed.2,
        limit,
        IStation::specCall {},
        &mut remaining,
    )
    .await?;
    let stage = pinned(
        &endpoint,
        station,
        observed.2,
        limit,
        IStation::stageCall {},
        &mut remaining,
    )
    .await?;
    if stage.stagedAt != 0 {
        return Ok(None);
    }
    if spec.vault != vault || spec.vaultChain != U256::from(client.chain_id) {
        bail!("the station does not name this quiet vault");
    }
    let mut due = input
        .daily_at
        .saturating_add(crate::DAY)
        .min(priced.time.saturating_add(crate::HEARTBEAT))
        .min(crate::next_monday(get!(lastPaymentCall)));
    let mut addresses = vec![
        vault,
        station,
        factory,
        registry,
        asset,
        deployments.factory,
    ];
    for driver in spec.drivers {
        let implementation = pinned(
            &endpoint,
            station,
            observed.2,
            limit,
            IStation::pinnedDriverCall { driver },
            &mut remaining,
        )
        .await?;
        let exit = pinned(
            &endpoint,
            implementation,
            observed.2,
            limit,
            IDriver::exitPathCall {},
            &mut remaining,
        )
        .await?;
        if matches!(exit, ExitPath::Delayed) {
            return Ok(None);
        }
        addresses.push(implementation);
        let key = pinned(
            &endpoint,
            station,
            observed.2,
            limit,
            IStation::approvedKeyCall { driver },
            &mut remaining,
        )
        .await?;
        if key != Address::ZERO {
            let blessed = pinned(
                &endpoint,
                registry,
                observed.2,
                limit,
                IAttestationRegistry::isBlessedForCall {
                    signer: key,
                    vaultChain: spec.vaultChain,
                    vault,
                    strategyHash: spec.bundleHash,
                },
                &mut remaining,
            )
            .await?;
            let info = pinned(
                &endpoint,
                registry,
                observed.2,
                limit,
                IAttestationRegistry::keyInfoCall { signer: key },
                &mut remaining,
            )
            .await?;
            let revoked = pinned(
                &endpoint,
                registry,
                observed.2,
                limit,
                IAttestationRegistry::isDriverRevokedCall {
                    name: driver,
                    implementation,
                },
                &mut remaining,
            )
            .await?;
            if !blessed
                || !matches!(info.kind, chain::bindings::attestation::ImageKind::Executor)
                || revoked
            {
                return Ok(None);
            }
            let until = pinned(
                &endpoint,
                registry,
                observed.2,
                limit,
                IAttestationRegistry::blessedUntilCall {
                    signer: key,
                    vaultChain: spec.vaultChain,
                    vault,
                    strategyHash: spec.bundleHash,
                },
                &mut remaining,
            )
            .await?;
            // A blessing is valid through this second, not just before it.
            due = due.min(until.saturating_add(1));
        }
    }
    if due <= observed.1 {
        return Ok(None);
    }
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.len() > client.rpc_policy().log_batch_addresses as usize {
        return Ok(None);
    }
    Ok(Some(Hint {
        head: (observed.0, observed.2),
        due,
        addresses,
        answered,
    }))
}

use chain::client::HeadObservation;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

#[derive(Default)]
pub struct Schedule {
    head: Option<HeadObservation>,
    headers: VecDeque<HeadObservation>,
    hints: HashMap<Address, Hint>,
    pending: HashMap<Address, tokio::task::JoinHandle<Result<Option<Hint>>>>,
    tried: HashMap<Address, Instant>,
    listed: Option<Vec<Address>>,
    factories: Vec<Address>,
    usable: bool,
    observed_at: Option<Instant>,
    observer: Option<tokio::task::JoinHandle<()>>,
    observations: Option<tokio::sync::mpsc::Receiver<Result<Observation>>>,
    ready: VecDeque<Result<Observation>>,
}

struct Observation {
    next: HeadObservation,
    added: Vec<HeadObservation>,
    replaced: bool,
    gap: bool,
    started: Instant,
}

impl Drop for Schedule {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            observer.abort();
        }
        for task in self.pending.values() {
            task.abort();
        }
    }
}

impl Schedule {
    #[cfg(test)]
    pub(super) fn remember_test_hint(&mut self, vault: Address, head: HeadObservation, hint: Hint) {
        self.headers.push_back(head.clone());
        self.head = Some(head);
        self.observed_at = Some(Instant::now());
        self.hints.insert(vault, hint);
        self.usable = true;
    }

    #[cfg(test)]
    pub(super) fn remember_test_parent(&mut self, parent: HeadObservation) {
        self.headers.push_front(parent);
    }

    fn invalidate(&mut self) {
        self.hints.clear();
        for (_, task) in self.pending.drain() {
            task.abort();
        }
        self.listed = None;
        self.usable = false;
    }

    /// Consume completed observations only. Network reads run on an
    /// independent task and never add a serial wait to the money round.
    pub fn poll(&mut self, client: &Client) -> Result<bool> {
        if self.observer.is_none() {
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let client = client.clone();
            let follow_events = self.quiet_time_is_bounded();
            self.observer = Some(tokio::spawn(async move {
                let mut previous = None;
                loop {
                    let budget = Duration::from_millis(client.rpc_policy().read_budget_ms);
                    let result = tokio::time::timeout(
                        budget,
                        Self::fetch(&client, previous.as_ref(), follow_events),
                    )
                    .await
                    .context("the event observation exceeded its read budget")
                    .and_then(|result| result);
                    if let Ok(observation) = &result {
                        previous = Some(observation.next.clone());
                    }
                    // Backpressure preserves every observed branch change/event.
                    // Later heads require ancestry; event coverage requires full linkage.
                    if sender.send(result).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(
                        client
                            .rpc_policy()
                            .head_poll_ms
                            .min(client.rpc_policy().event_scan_ms),
                    ))
                    .await;
                }
            }));
            self.observations = Some(receiver);
        }
        let mut replaced = false;
        let mut error = None;
        loop {
            let result = self
                .ready
                .pop_front()
                .or_else(|| self.observations.as_mut().and_then(|rx| rx.try_recv().ok()));
            let Some(result) = result else {
                break;
            };
            match result {
                Ok(observation) => replaced |= self.apply(client, observation),
                Err(failure) => {
                    self.invalidate();
                    error = Some(failure);
                }
            }
        }
        // Preserve a branch-change notification even if a subsequent read failed.
        if replaced {
            return Ok(true);
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(false)
    }

    /// Observations are consumed by the next ordinary paced round. An
    /// observation alone must not advance logical money retry counters.
    pub async fn wait(&mut self, pause: Duration) {
        tokio::time::sleep(pause).await;
    }

    /// Neither preceding intervals nor request age bounds endpoint head lag
    /// or future chain-time advance. Keep ordinary reads until a bound can
    /// be established; an estimate cannot authorize skipping money duties.
    fn quiet_time_is_bounded(&self) -> bool {
        false
    }

    #[cfg(test)]
    async fn observe(&mut self, client: &Client) -> Result<bool> {
        match Self::fetch(client, self.head.as_ref(), true).await {
            Ok(observation) => Ok(self.apply(client, observation)),
            Err(error) => {
                self.invalidate();
                Err(error)
            }
        }
    }

    /// Quiet coverage requires every intervening header to connect. Active
    /// rounds may prove ancestry in the head itself; they certify no events.
    async fn fetch(
        client: &Client,
        previous: Option<&HeadObservation>,
        follow_events: bool,
    ) -> Result<Observation> {
        let started = Instant::now();
        let mut gap = false;
        let next = client.head_details().await?;
        let mut replaced = false;
        let mut added = Vec::new();
        if let Some(previous) = previous {
            if next.number == previous.number && next.hash == previous.hash {
                if next.timestamp != previous.timestamp
                    || next.parent_hash != previous.parent_hash
                    || next.logs_bloom != previous.logs_bloom
                {
                    bail!("the same header hash has contradictory details");
                }
            } else if next.number <= previous.number {
                // Endpoints behind one list routinely answer a block apart, so
                // a lower or competing head proves nothing about the remembered
                // one. Adopt nothing; only a later head that does not link to
                // the remembered head shows that its branch was replaced.
                bail!("a head at or below the observed head; waiting for a later one");
            } else if next.number - previous.number > client.rpc_policy().log_batch_blocks {
                // No ancestry proof ties this head to the remembered branch.
                // A numbered anchor from another endpoint cannot establish it.
                // Reset ordinary money memories before adopting the new head.
                replaced = true;
                gap = true;
            } else {
                let linked = Self::link_headers(client, previous, &next);
                let distance = next.number - previous.number;
                let (changed, walked) = if !follow_events && (4..=256).contains(&distance) {
                    // Both reads start together. A failed optional proof adds
                    // no serial wait before the ordinary linkage walk.
                    let ancestry = chain::header_read::ancestor(client, &next, previous.number);
                    tokio::pin!(linked, ancestry);
                    tokio::select! {
                        answer = &mut ancestry => match answer {
                            Ok(hash) => (hash != previous.hash, Vec::new()),
                            Err(_) => linked.await?,
                        },
                        answer = &mut linked => match answer {
                            Ok(linked) => linked,
                            Err(error) => match ancestry.await {
                                Ok(hash) => (hash != previous.hash, Vec::new()),
                                Err(_) => return Err(error),
                            },
                        },
                    }
                } else {
                    linked.await?
                };
                replaced = changed;
                added = walked;
                // Active rounds never consume an interval certificate, even
                // when the full walk won this race. Quiet authority stays off.
                gap = !follow_events;
            }
        }
        Ok(Observation {
            next,
            added,
            replaced,
            gap,
            started,
        })
    }

    /// Preserve the original parent linkage as the fallback observation.
    async fn link_headers(
        client: &Client,
        previous: &HeadObservation,
        next: &HeadObservation,
    ) -> Result<(bool, Vec<HeadObservation>)> {
        let mut parent = previous.clone();
        let mut added = Vec::new();
        for number in previous.number + 1..=next.number {
            let header = if number == next.number {
                next.clone()
            } else {
                client.header_at(number).await?
            };
            if header.parent_hash != parent.hash || header.timestamp < parent.timestamp {
                return Ok((true, added));
            }
            parent = header.clone();
            added.push(header);
        }
        Ok((false, added))
    }

    fn apply(&mut self, client: &Client, observation: Observation) -> bool {
        let Observation {
            next,
            mut added,
            replaced,
            gap,
            started,
        } = observation;
        if replaced || gap {
            self.invalidate();
            self.headers.clear();
            added.clear();
        }
        for header in &added {
            self.hints
                .retain(|_, hint| !header.may_have_logs(&hint.addresses));
            if header.may_have_logs(&self.factories) {
                self.listed = None;
            }
        }
        if self.headers.is_empty() && added.is_empty() {
            self.headers.push_back(next.clone());
        }
        self.headers.extend(added);
        while self.headers.len() > client.rpc_policy().log_batch_blocks as usize {
            self.headers.pop_front();
        }
        self.head = Some(next);
        self.observed_at = Some(started);
        self.usable = true;
        let finished: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, task)| task.is_finished())
            .map(|(&vault, _)| vault)
            .collect();
        for vault in finished {
            if let Some(task) = self.pending.remove(&vault)
                && let Some(Ok(Ok(Some(hint)))) = futures::FutureExt::now_or_never(task)
                && self.covers(&hint)
            {
                self.hints.insert(vault, hint);
            }
        }
        replaced
    }

    fn covers(&self, hint: &Hint) -> bool {
        self.headers
            .iter()
            .any(|header| (header.number, header.hash) == hint.head)
            && self
                .headers
                .iter()
                .filter(|header| header.number > hint.head.0)
                .all(|header| !header.may_have_logs(&hint.addresses))
    }

    /// The ordinary valuation path prepares for the next block, rather than
    /// waiting for the due slot's header. Missing ancestry cannot establish
    /// that landing opportunity and keeps the vault on its active path.
    fn waking_at(&self, client: &Client) -> Option<u64> {
        let head = self.head.as_ref()?;
        let parent = self.headers.iter().find(|parent| {
            parent.number.checked_add(1) == Some(head.number)
                && parent.hash == head.parent_hash
                && parent.timestamp <= head.timestamp
        })?;
        let interval = head.timestamp.saturating_sub(parent.timestamp).max(1);
        let age = self.observed_at?.elapsed().as_secs().saturating_add(1);
        Some(
            head.timestamp
                .saturating_add(interval)
                .saturating_add(age)
                .saturating_add(client.rpc_policy().due_lookahead_ms.div_ceil(1000)),
        )
    }

    pub fn sleeping(&self, client: &Client, vault: Address, answered: Option<Instant>) -> bool {
        self.quiet_time_is_bounded()
            && self.fresh(client)
            && self
                .waking_at(client)
                .zip(self.hints.get(&vault))
                .is_some_and(|(waking, hint)| {
                    hint.answered == answered && self.covers(hint) && waking < hint.due
                })
    }

    fn fresh(&self, client: &Client) -> bool {
        self.usable
            && self.observed_at.is_some_and(|at| {
                at.elapsed()
                    <= Duration::from_millis(
                        client
                            .rpc_policy()
                            .head_poll_ms
                            .min(client.rpc_policy().event_scan_ms),
                    )
            })
    }

    pub fn vaults(&self, client: &Client) -> Option<Vec<Address>> {
        (self.quiet_time_is_bounded() && self.fresh(client))
            .then(|| self.listed.clone())
            .flatten()
    }

    pub fn listed(&mut self, vaults: Vec<Address>, factories: Vec<Address>, complete: bool) {
        self.factories = factories;
        self.listed = complete.then_some(vaults);
    }

    pub fn offer(
        &mut self,
        client: &Client,
        deployments: &Deployments,
        vault: Address,
        input: QuietInput,
        answered: Option<Instant>,
    ) {
        if !self.quiet_time_is_bounded() || !self.fresh(client) {
            return;
        }
        let Some(waking) = self.waking_at(client) else {
            return;
        };
        let Some(head) = self.head.as_ref() else {
            return;
        };
        if self
            .hints
            .get(&vault)
            .is_some_and(|hint| hint.answered != answered || waking >= hint.due)
        {
            self.hints.remove(&vault);
        }
        if self.pending.contains_key(&vault)
            || self.hints.contains_key(&vault)
            || self.pending.len() + self.hints.len()
                >= client.rpc_policy().log_batch_addresses as usize
            || self.tried.get(&vault).is_some_and(|at| {
                at.elapsed() < Duration::from_millis(client.rpc_policy().quiet_probe_retry_ms)
            })
        {
            return;
        }
        let observed = (head.number, head.timestamp, head.hash);
        let client = client.clone();
        let deployments = deployments.clone();
        if self.tried.len() >= client.rpc_policy().log_batch_addresses as usize
            && !self.tried.contains_key(&vault)
            && let Some(oldest) = self
                .tried
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(&at, _)| at)
        {
            self.tried.remove(&oldest);
        }
        self.tried.insert(vault, Instant::now());
        self.pending.insert(
            vault,
            tokio::spawn(async move {
                let budget = Duration::from_millis(client.rpc_policy().quiet_probe_budget_ms);
                tokio::time::timeout(
                    budget,
                    candidate(&client, &deployments, vault, observed, input, answered),
                )
                .await
                .context("the optional quiet probe exceeded its read budget")?
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Bloom, BloomInput};
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    fn header(number: u64, parent: B256, emitted: Option<Address>) -> HeadObservation {
        let mut logs_bloom = Bloom::ZERO;
        if let Some(address) = emitted {
            logs_bloom.accrue(BloomInput::Raw(address.as_slice()));
        }
        HeadObservation {
            number,
            timestamp: 100 + number,
            hash: B256::repeat_byte(number as u8),
            parent_hash: parent,
            logs_bloom,
        }
    }

    async fn node(headers: Arc<Mutex<Vec<HeadObservation>>>, current: Arc<Mutex<usize>>) -> Client {
        delayed_node(headers, current, Duration::ZERO).await
    }

    async fn delayed_node(
        headers: Arc<Mutex<Vec<HeadObservation>>>,
        current: Arc<Mutex<usize>>,
        delay: Duration,
    ) -> Client {
        recording_node(headers, current, delay, Arc::default()).await
    }

    async fn recording_node(
        headers: Arc<Mutex<Vec<HeadObservation>>>,
        current: Arc<Mutex<usize>>,
        delay: Duration,
        reads: Arc<Mutex<Vec<String>>>,
    ) -> Client {
        let app = Router::new().route(
            "/",
            post(move |Json(request): Json<Value>| {
                let headers = headers.clone();
                let current = current.clone();
                let reads = reads.clone();
                async move {
                    if request["method"] == "eth_getBlockByNumber" {
                        reads
                            .lock()
                            .unwrap()
                            .push(request["params"][0].as_str().unwrap().to_string());
                        tokio::time::sleep(delay).await;
                    }
                    let result = match request["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => json!("0x7a69"),
                        "eth_getBlockByNumber" => {
                            let headers = headers.lock().unwrap();
                            let tag = request["params"][0].as_str().unwrap();
                            let found = if tag == "latest" {
                                headers.get(*current.lock().unwrap())
                            } else {
                                u64::from_str_radix(tag.trim_start_matches("0x"), 16)
                                    .ok()
                                    .and_then(|number| {
                                        headers.iter().find(|header| header.number == number)
                                    })
                            };
                            match found {
                                Some(header) => {
                                    let mut block = alloy::rpc::types::Block::<
                                        alloy::rpc::types::Transaction,
                                    >::default(
                                    );
                                    block.header.inner.number = header.number;
                                    block.header.inner.timestamp = header.timestamp;
                                    block.header.inner.parent_hash = header.parent_hash;
                                    block.header.inner.logs_bloom = header.logs_bloom;
                                    block.header.hash = header.hash;
                                    serde_json::to_value(block).unwrap()
                                }
                                None => Value::Null,
                            }
                        }
                        "eth_call" => {
                            let headers = headers.lock().unwrap();
                            let hash = request["params"][1]["blockHash"].as_str().unwrap();
                            let block = headers
                                .iter()
                                .find(|h| format!("{:#x}", h.hash) == hash)
                                .unwrap();
                            let input = request["params"][0]["input"]
                                .as_str()
                                .or_else(|| request["params"][0]["data"].as_str())
                                .unwrap();
                            let raw = alloy::hex::decode(input).unwrap();
                            let call =
                                chain::header_read::HeaderRead::readCall::abi_decode(&raw).unwrap();
                            let number: u64 = call.ancestorNumber.try_into().unwrap();
                            reads.lock().unwrap().push("ancestor".into());
                            let ancestor = headers.iter().find(|h| h.number == number).unwrap();
                            let result = chain::header_read::HeaderRead::readReturn {
                                number: U256::from(block.number),
                                timestamp: U256::from(block.timestamp),
                                parentHash: block.parent_hash,
                                ancestorHash: ancestor.hash,
                            };
                            json!(format!(
                                "0x{}",
                                alloy::hex::encode(
                                    chain::header_read::HeaderRead::readCall::abi_encode_returns(
                                        &result
                                    )
                                )
                            ))
                        }
                        _ => Value::Null,
                    };
                    Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = chain::ChainConfig {
            label: "event clock".into(),
            rpc: url.clone(),
            public_rpc: url,
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            logs_rpc: None,
            log_range: None,
            confirmations: None,
        };
        Client::connect(&config, chain::Signing::ReadOnly)
            .await
            .unwrap()
    }

    fn hint(head: &HeadObservation, watched: Address) -> Hint {
        Hint {
            head: (head.number, head.hash),
            due: 1000,
            addresses: vec![watched],
            answered: None,
        }
    }

    #[tokio::test]
    async fn an_unlinked_large_head_gap_resets_the_rounds_derived_state() {
        for changed_anchor in [false, true] {
            let previous = header(10, B256::repeat_byte(9), None);
            let reads = Arc::new(Mutex::new(Vec::new()));
            let current = Arc::new(Mutex::new(0));
            let headers = Arc::new(Mutex::new(vec![previous.clone()]));
            let client = recording_node(
                headers.clone(),
                current.clone(),
                Duration::ZERO,
                reads.clone(),
            )
            .await;
            let next = header(
                previous.number + client.rpc_policy().log_batch_blocks + 1,
                B256::repeat_byte(70),
                None,
            );
            if changed_anchor {
                headers.lock().unwrap()[0].hash = B256::repeat_byte(80);
            }
            headers.lock().unwrap().push(next.clone());
            *current.lock().unwrap() = 1;
            reads.lock().unwrap().clear();
            let observation = Schedule::fetch(&client, Some(&previous), false)
                .await
                .unwrap();
            assert!(observation.replaced);
            assert!(observation.gap);
            assert!(observation.added.is_empty());
            assert_eq!(
                *reads.lock().unwrap(),
                vec!["latest"],
                "a separately numbered old header must not authorize keeping state"
            );
            let mut schedule = Schedule::default();
            schedule.remember_test_hint(
                Address::repeat_byte(7),
                previous.clone(),
                hint(&previous, Address::repeat_byte(7)),
            );
            schedule.ready.push_back(Ok(observation));
            assert!(schedule.poll(&client).unwrap());
            assert!(schedule.hints.is_empty());
            assert!(schedule.listed.is_none());
            assert_eq!(schedule.head.as_ref().unwrap().hash, next.hash);
        }
    }

    #[tokio::test]
    async fn an_active_adjacent_head_must_still_link_to_the_previous_anchor() {
        let first = header(10, B256::repeat_byte(9), None);
        let changed = header(11, B256::repeat_byte(80), None);
        let client = node(
            Arc::new(Mutex::new(vec![first.clone(), changed])),
            Arc::new(Mutex::new(1)),
        )
        .await;
        let observed = Schedule::fetch(&client, Some(&first), false).await.unwrap();
        assert!(observed.replaced);
        assert!(observed.gap);
        assert!(observed.added.is_empty());
    }

    #[tokio::test]
    async fn active_round_observations_check_the_anchor_without_scanning_unused_headers() {
        let first = header(10, B256::repeat_byte(9), None);
        let later = header(20, B256::repeat_byte(19), None);
        let headers = Arc::new(Mutex::new(vec![first.clone(), later.clone()]));
        let current = Arc::new(Mutex::new(0));
        let reads = Arc::new(Mutex::new(Vec::new()));
        let client = recording_node(
            headers.clone(),
            current.clone(),
            Duration::ZERO,
            reads.clone(),
        )
        .await;
        let mut schedule = Schedule::default();
        let stop = Instant::now() + Duration::from_secs(6);
        while schedule.head.as_ref().is_none_or(|h| h.number != 10) {
            assert!(Instant::now() < stop, "first observation was not delivered");
            schedule.poll(&client).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        reads.lock().unwrap().clear();
        *current.lock().unwrap() = 1;
        while schedule.head.as_ref().is_none_or(|h| h.number != 20) {
            assert!(
                Instant::now() < stop,
                "active observation fetched unavailable middle headers"
            );
            assert!(!schedule.poll(&client).unwrap());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let read = reads.lock().unwrap().clone();
        assert_eq!(read.iter().filter(|r| r.as_str() == "latest").count(), 1);
        assert_eq!(read.iter().filter(|r| r.as_str() == "ancestor").count(), 1);
        assert!(
            read.iter()
                .all(|r| matches!(r.as_str(), "latest" | "ancestor" | "0xb"))
        );
        assert!(
            read.len() <= 3,
            "the losing walk may ask only its first middle header"
        );
        assert_eq!(
            schedule.headers.len(),
            1,
            "unscanned interval must not be certified"
        );
        assert!(!schedule.sleeping(&client, Address::repeat_byte(7), None));
        assert!(schedule.vaults(&client).is_none());

        // A changed earlier anchor remains a branch replacement even though
        // the current head is ahead and no intermediate headers are scanned.
        headers.lock().unwrap()[0].hash = B256::repeat_byte(80);
        let newest = header(30, B256::repeat_byte(29), None);
        headers.lock().unwrap().push(newest);
        *current.lock().unwrap() = 2;
        let stop = Instant::now() + Duration::from_secs(5);
        let mut replaced = false;
        // The preceding anchor is20. Replace that exact canonical header.
        headers.lock().unwrap()[1].hash = B256::repeat_byte(90);
        while schedule.head.as_ref().is_none_or(|h| h.number != 30) {
            assert!(
                Instant::now() < stop,
                "replacement observation was not delivered"
            );
            replaced |= schedule.poll(&client).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            replaced,
            "a replaced anchor must reset ordinary round memory"
        );
    }

    #[tokio::test]
    async fn slow_header_reads_do_not_delay_the_money_round() {
        let watched = Address::repeat_byte(7);
        let first = header(10, B256::repeat_byte(9), None);
        let client = delayed_node(
            Arc::new(Mutex::new(vec![first.clone()])),
            Arc::new(Mutex::new(0)),
            Duration::from_millis(300),
        )
        .await;
        let mut schedule = Schedule::default();
        // An unfinished network call must not turn into a wait here.
        let started = Instant::now();
        assert!(!schedule.poll(&client).unwrap());
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!schedule.sleeping(&client, watched, None));
        let waiting = Instant::now();
        schedule.wait(Duration::from_millis(400)).await;
        assert!(waiting.elapsed() >= Duration::from_millis(400));
        schedule.poll(&client).unwrap();
        assert_eq!(schedule.head.as_ref().unwrap().hash, first.hash);
        schedule.hints.insert(watched, hint(&first, watched));
        assert!(!schedule.sleeping(&client, watched, None));
        schedule.remember_test_parent(header(9, B256::repeat_byte(8), None));
        assert!(!schedule.sleeping(&client, watched, None));
        schedule.listed(vec![watched], Vec::new(), true);
        schedule.observed_at = Some(Instant::now() - Duration::from_secs(4));
        assert!(!schedule.sleeping(&client, watched, None));
        assert!(schedule.vaults(&client).is_none());
    }

    #[tokio::test]
    async fn a_queued_replacement_is_reported_even_when_the_next_observation_fails() {
        let first = header(10, B256::repeat_byte(9), None);
        let client = node(
            Arc::new(Mutex::new(vec![first.clone()])),
            Arc::new(Mutex::new(0)),
        )
        .await;
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let mut schedule = Schedule::default();
        schedule.observer = Some(tokio::spawn(std::future::pending()));
        schedule.observations = Some(receiver);
        sender
            .send(Ok(Observation {
                next: first,
                added: Vec::new(),
                replaced: true,
                gap: false,
                started: Instant::now(),
            }))
            .await
            .unwrap();
        sender
            .send(Err(anyhow::anyhow!("unavailable")))
            .await
            .unwrap();
        assert!(schedule.poll(&client).unwrap());
        assert!(!schedule.usable);
    }

    #[tokio::test]
    async fn queued_observations_do_not_advance_the_active_round() {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let mut schedule = Schedule::default();
        schedule.observations = Some(receiver);
        sender
            .send(Err(anyhow::anyhow!("observation")))
            .await
            .unwrap();
        let started = Instant::now();
        schedule.wait(Duration::from_millis(100)).await;
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(schedule.ready.is_empty());
        assert!(schedule.observations.as_mut().unwrap().try_recv().is_ok());
    }

    #[tokio::test]
    async fn exhausted_quiet_read_budget_declines_before_another_request() {
        let client = node(Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(0))).await;
        let endpoint = client.each_endpoint().remove(0);
        let mut remaining = 0;
        let error = pinned(
            &endpoint,
            Address::ZERO,
            B256::ZERO,
            32,
            IVault::stateCall {},
            &mut remaining,
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("request bound"));
    }

    #[tokio::test]
    async fn an_event_in_a_polling_gap_invalidates_the_quiet_observation() {
        let watched = Address::repeat_byte(7);
        let first = header(10, B256::repeat_byte(9), None);
        let event = header(11, first.hash, Some(watched));
        let last = header(12, event.hash, None);
        let headers = Arc::new(Mutex::new(vec![first.clone(), event, last]));
        let current = Arc::new(Mutex::new(0));
        let client = node(headers, current.clone()).await;
        let mut schedule = Schedule::default();
        assert!(!schedule.observe(&client).await.unwrap());
        schedule.hints.insert(watched, hint(&first, watched));
        assert!(!schedule.sleeping(&client, watched, None));
        schedule.remember_test_parent(header(9, B256::repeat_byte(8), None));
        assert!(!schedule.sleeping(&client, watched, None));
        *current.lock().unwrap() = 2;
        assert!(!schedule.observe(&client).await.unwrap());
        assert!(!schedule.sleeping(&client, watched, None));
    }

    #[tokio::test]
    async fn negative_connected_headers_retain_quietness_until_a_deadline_or_answer() {
        let watched = Address::repeat_byte(7);
        let first = header(10, B256::repeat_byte(9), None);
        let second = header(11, first.hash, None);
        let third = header(12, second.hash, None);
        let headers = Arc::new(Mutex::new(vec![first.clone(), second, third]));
        let current = Arc::new(Mutex::new(0));
        let client = node(headers, current.clone()).await;
        let mut schedule = Schedule::default();
        schedule.observe(&client).await.unwrap();
        schedule.hints.insert(watched, hint(&first, watched));
        *current.lock().unwrap() = 2;
        schedule.observe(&client).await.unwrap();
        assert!(!schedule.sleeping(&client, watched, None));
        assert!(!schedule.sleeping(&client, watched, Some(Instant::now())));
        let time = schedule.head.as_ref().unwrap().timestamp;
        let early = client.rpc_policy().due_lookahead_ms.div_ceil(1000) + 2;
        schedule.hints.get_mut(&watched).unwrap().due = time + early;
        assert!(!schedule.sleeping(&client, watched, None));
        schedule.hints.get_mut(&watched).unwrap().due += 1;
        assert!(!schedule.sleeping(&client, watched, None));
    }

    /// A competing header at the observed height is not adopted and proves
    /// nothing about the remembered branch, but it does discard certificates
    /// and factory discovery; a later head must link to the remembered one.
    #[tokio::test]
    async fn a_same_height_competitor_discards_certificates_and_factory_discovery() {
        let watched = Address::repeat_byte(7);
        let first = header(10, B256::repeat_byte(9), None);
        let headers = Arc::new(Mutex::new(vec![first.clone()]));
        let current = Arc::new(Mutex::new(0));
        let client = node(headers.clone(), current).await;
        let mut schedule = Schedule::default();
        schedule.observe(&client).await.unwrap();
        schedule.hints.insert(watched, hint(&first, watched));
        schedule.listed(vec![watched], vec![Address::repeat_byte(3)], true);
        assert!(schedule.vaults(&client).is_none());
        headers.lock().unwrap()[0].hash = B256::repeat_byte(99);
        assert!(schedule.observe(&client).await.is_err());
        assert_eq!(schedule.head.as_ref().unwrap().hash, first.hash);
        assert!(schedule.listed.is_none());
        assert!(schedule.hints.is_empty());
        assert!(schedule.vaults(&client).is_none());
        assert!(!schedule.sleeping(&client, watched, None));
    }

    #[tokio::test]
    async fn a_missing_intermediate_header_forces_active_reads_without_advancing_confidence() {
        let watched = Address::repeat_byte(7);
        let first = header(10, B256::repeat_byte(9), None);
        let last = header(12, B256::repeat_byte(11), None);
        let headers = Arc::new(Mutex::new(vec![first.clone(), last]));
        let current = Arc::new(Mutex::new(0));
        let client = node(headers, current.clone()).await;
        let mut schedule = Schedule::default();
        schedule.observe(&client).await.unwrap();
        schedule.hints.insert(watched, hint(&first, watched));
        *current.lock().unwrap() = 1;
        assert!(schedule.observe(&client).await.is_err());
        assert!(!schedule.sleeping(&client, watched, None));
        assert_eq!(schedule.head.as_ref().unwrap().number, 10);
    }
}
