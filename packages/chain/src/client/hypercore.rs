//! Reads of HyperCore, sent only to the endpoints that serve them.
//!
//! On HyperEVM a contract can read HyperCore's state (positions, balances,
//! prices) through read precompiles at `0x0800` and up. The node running the
//! call answers them by asking a HyperCore node of its own; an endpoint whose
//! node can't reach one fails the whole call ("failed to forward read
//! precompile ..."), and does so for every such call while it can't, though
//! it answers every other read. That failure is the endpoint's, never the
//! call's answer: another endpoint of the list may serve the call.
//!
//! So each endpoint of a HyperEVM list is asked a precompile read when the
//! client connects, and whether it served it is kept. A call or estimate
//! failed for a precompile it could not forward marks its endpoint as not
//! serving them and its target as one that reads HyperCore. From then on a
//! call or estimate to such a target (or to a precompile itself) goes to
//! the endpoints shown to serve them first, then those not known, in the
//! list's order; an endpoint marked as not serving them is asked again only
//! once [`ASK_AGAIN_AFTER`] has passed, or when no endpoint shown to serve
//! them is left to ask. An endpoint shown to serve them is marked only when
//! it fails them [`FAILURES_TO_MARK`] times within [`FAILURE_SPAN`], so one
//! dropped request never stops these calls. Every other request goes
//! through the list as before, so cheap reads stay on whichever endpoint
//! answers them best.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::Address;
use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket, SerializedRequest};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};

/// The chains whose EVM reads HyperCore through precompiles: Hyperliquid's
/// mainnet and testnet.
pub(crate) const HYPERCORE_CHAINS: [u64; 2] = [999, 998];

/// The precompile an endpoint is asked at connect: HyperCore's block number,
/// which takes no input.
pub(crate) const PROBE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x09,
]);

/// How long an endpoint marked as not serving HyperCore's reads is passed
/// over before a call that reads HyperCore is sent to it again.
pub(crate) const ASK_AGAIN_AFTER: Duration = Duration::from_secs(60);

/// How many failures within [`FAILURE_SPAN`] mark an endpoint shown to
/// serve HyperCore's reads (or one failing for a reason that names no
/// precompile, such as its node refusing connections) as not serving them.
pub(crate) const FAILURES_TO_MARK: usize = 3;

/// The span within which [`FAILURES_TO_MARK`] failures mark an endpoint.
pub(crate) const FAILURE_SPAN: Duration = Duration::from_secs(30);

/// What the client says of an endpoint that failed to reach what a call
/// needs.
pub(crate) const UNREACHED: &str = "the endpoint could not reach what the call reads";

/// Whether `address` is one of HyperCore's read precompiles (`0x0800` to
/// `0x08ff`).
pub(crate) fn is_precompile(address: Address) -> bool {
    address[..18].iter().all(|byte| *byte == 0) && address[18] == 0x08
}

/// Whether a JSON-RPC error is the endpoint failing to reach something the
/// call needs (a node behind it, or a HyperCore node behind that): the
/// endpoint's trouble, not an answer about the call. A revert, whatever its
/// reason says, is the call's answer, never this.
pub(crate) fn is_forwarding_failure(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if code == 3 || message.contains("revert") {
        return false;
    }
    [
        // HyperEVM's: "failed to forward read precompile 0x…: client error
        // (Connect)".
        "failed to forward",
        "client error (connect)",
        // A gateway whose node is down: "dial tcp …: connect: connection
        // refused".
        "connection refused",
        "dial tcp",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// Whether a successful call's raw result shows the endpoint serving a read
/// of HyperCore: an empty `0x` is what an address with no code answers, so
/// it shows nothing.
pub(crate) fn is_answer(result: &str) -> bool {
    !matches!(result.trim(), "\"0x\"" | "\"\"" | "null")
}

/// Whether a forwarding failure names a HyperCore read precompile.
pub(crate) fn names_precompile(message: &str) -> bool {
    message.to_ascii_lowercase().contains("precompile")
}

/// The contracts a call or an estimate in `request` is made to.
pub(crate) fn targets(request: &RequestPacket) -> Vec<Address> {
    let each: Vec<&SerializedRequest> = match request {
        RequestPacket::Single(single) => vec![single],
        RequestPacket::Batch(batch) => batch.iter().collect(),
    };
    each.into_iter()
        .filter(|single| matches!(single.method(), "eth_call" | "eth_estimateGas"))
        .filter_map(|single| {
            let params: serde_json::Value = serde_json::from_str(single.params()?.get()).ok()?;
            params.get(0)?.get("to")?.as_str()?.parse().ok()
        })
        .collect()
}

/// What a list's endpoints have shown of serving HyperCore's reads, by their
/// place in the list, and the contracts found to read HyperCore. Shared by
/// every connection the client makes over the list.
#[derive(Clone, Default)]
pub(crate) struct HyperCore(Arc<Mutex<Known>>);

#[derive(Default)]
struct Known {
    /// Whether each endpoint served a read of HyperCore when last asked,
    /// and when that was.
    serves: HashMap<usize, (bool, tokio::time::Instant)>,
    /// The endpoints that have served a read of HyperCore at least once.
    served: HashSet<usize>,
    /// Each endpoint's failures of HyperCore's reads since it last served
    /// one or was marked, within [`FAILURE_SPAN`].
    failures: HashMap<usize, Vec<tokio::time::Instant>>,
    /// The contracts a call to which failed for a precompile it reads.
    targets: HashSet<Address>,
}

impl HyperCore {
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Note whether endpoint `at` served a read of HyperCore.
    pub(crate) fn saw(&self, at: usize, serves: bool) {
        let mut known = self.known();
        let before = known.serves.get(&at).map(|(served, _)| *served);
        known
            .serves
            .insert(at, (serves, tokio::time::Instant::now()));
        known.failures.remove(&at);
        if serves {
            known.served.insert(at);
        }
        drop(known);
        if before != Some(serves) {
            if serves {
                tracing::info!(endpoint = at + 1, "this endpoint serves HyperCore's reads");
            } else {
                tracing::warn!(
                    endpoint = at + 1,
                    "this endpoint fails HyperCore's reads; calls that make them go to the \
                     list's other endpoints"
                );
            }
        }
    }

    /// Note that a call to `to` reads HyperCore. A call made through a
    /// batch of reads (the observations' helper at the zero address, or
    /// Multicall3) says nothing of which read it was, so neither is noted:
    /// their cheap reads would follow.
    pub(crate) fn reads_hypercore(&self, to: Address) {
        let batch = to == Address::ZERO || to == crate::follow::multicall::MULTICALL3;
        if !batch && !is_precompile(to) && self.known().targets.insert(to) {
            tracing::info!(%to, "calls to this contract read HyperCore");
        }
    }

    /// Whether a call to `to` reads HyperCore, as far as is known.
    pub(crate) fn touches(&self, to: Address) -> bool {
        is_precompile(to) || self.known().targets.contains(&to)
    }

    /// The order to ask `count` endpoints in for a call that reads HyperCore:
    /// those shown to serve it, then those not known, in the list's order,
    /// then those marked as not serving it whose mark is older than
    /// [`ASK_AGAIN_AFTER`]. One marked more lately is asked only when no
    /// endpoint shown to serve it is left, last, those that once served
    /// it first: the call is never left with fewer endpoints to try than
    /// any other call has.
    pub(crate) fn order(&self, count: usize) -> Vec<usize> {
        let known = self.known();
        let rank = |at: &usize| match known.serves.get(at) {
            Some((true, _)) => 0,
            None => 1,
            Some((false, when)) if when.elapsed() >= ASK_AGAIN_AFTER => 2,
            Some((false, _)) if known.served.contains(at) => 3,
            Some((false, _)) => 4,
        };
        let mut order: Vec<(u8, usize)> = (0..count).map(|at| (rank(&at), at)).collect();
        if order.iter().any(|(rank, _)| *rank == 0) {
            order.retain(|(rank, _)| *rank < 3);
        }
        order.sort();
        order.into_iter().map(|(_, at)| at).collect()
    }

    /// Note that endpoint `at` failed a read of HyperCore as its own
    /// trouble, `names_precompile` when its failure named a precompile it
    /// could not forward. One not shown to serve these reads is marked as
    /// not serving them at a failure naming a precompile; otherwise an
    /// endpoint is marked once it has failed [`FAILURES_TO_MARK`] times
    /// within [`FAILURE_SPAN`], so that a blip on the endpoint that serves
    /// them does not stop them.
    pub(crate) fn failed(&self, at: usize, names_precompile: bool) {
        let mut known = self.known();
        let serving = matches!(known.serves.get(&at), Some((true, _)));
        let now = tokio::time::Instant::now();
        let failures = known.failures.entry(at).or_default();
        failures.retain(|when| now.duration_since(*when) < FAILURE_SPAN);
        failures.push(now);
        let repeated = failures.len() >= FAILURES_TO_MARK;
        drop(known);
        if repeated || (names_precompile && !serving) {
            self.saw(at, false);
        }
    }

    /// What a read failed for `at` as a forwarding failure says of it: it
    /// counts as the endpoint failing HyperCore's reads ([`HyperCore::failed`]),
    /// and when it names a precompile, the call's targets read HyperCore.
    pub(crate) fn failed_forwarding(&self, at: usize, targets: &[Address], message: &str) {
        let precompile = names_precompile(message);
        if precompile {
            for to in targets {
                self.reads_hypercore(*to);
            }
        }
        self.failed(at, precompile);
    }
}

/// A list's endpoints, with calls and estimates that read HyperCore sent
/// to those that serve them ([`HyperCore::order`]) one at a time, and
/// everything else to `rest` (the list's failover). A call moves on from an
/// endpoint that fails it as its own trouble (each endpoint's transport
/// makes a forwarding failure, a rate limit or a dropped connection an
/// error); an answer, a revert included, is the answer.
#[derive(Clone)]
pub(crate) struct HyperCoreFirst<F, S> {
    pub rest: F,
    pub each: Arc<[S]>,
    pub hypercore: HyperCore,
}

impl<F, S> tower::Service<RequestPacket> for HyperCoreFirst<F, S>
where
    F: tower::Service<
            RequestPacket,
            Response = ResponsePacket,
            Error = TransportError,
            Future = TransportFut<'static>,
        > + Send
        + Clone
        + 'static,
    S: tower::Service<
            RequestPacket,
            Response = ResponsePacket,
            Error = TransportError,
            Future = TransportFut<'static>,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.rest.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let reads_hypercore = matches!(&request, RequestPacket::Single(_))
            && targets(&request)
                .into_iter()
                .any(|to| self.hypercore.touches(to));
        if !reads_hypercore {
            return self.rest.call(request);
        }
        let each = self.each.clone();
        let hypercore = self.hypercore.clone();
        Box::pin(async move {
            // The failure returned is the first endpoint's, the one best
            // placed to serve the call: a rate limit there stays a rate
            // limit, to be waited out, rather than hidden behind the
            // failure of an endpoint that never serves these reads.
            let mut first = None;
            for at in hypercore.order(each.len()) {
                let mut endpoint = each[at].clone();
                match endpoint.call(request.clone()).await {
                    Ok(answer) => {
                        if answer.responses().iter().all(|r| {
                            r.payload
                                .as_success()
                                .is_some_and(|result| is_answer(result.get()))
                        }) {
                            hypercore.saw(at, true);
                        }
                        return Ok(answer);
                    }
                    Err(error) => {
                        first.get_or_insert(error);
                    }
                }
            }
            Err(first.unwrap_or_else(|| {
                TransportErrorKind::custom_str(
                    "no endpoint of the list serves HyperCore's reads now",
                )
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_precompile_that_could_not_be_forwarded_is_the_endpoint_s_failure_and_a_revert_never_is() {
        let forwarded = "Revm error: failed to forward read precompile \
                         0x000000000000000000000000000000000000080F: client error (Connect)";
        assert!(is_forwarding_failure(-32603, forwarded));
        assert!(names_precompile(forwarded));
        let down = "Post \"\": dial tcp 172.18.0.2:3001: connect: connection refused";
        assert!(is_forwarding_failure(-32603, down));
        assert!(!names_precompile(down));
        // HyperCore's own refusal of a read it was asked badly, and reverts
        // whatever their reason says, are answers.
        assert!(!is_forwarding_failure(-32003, "EVM error: PrecompileError"));
        assert!(!is_forwarding_failure(
            3,
            "execution reverted: connection refused"
        ));
        assert!(!is_forwarding_failure(
            -32000,
            "execution reverted: failed to forward"
        ));
    }

    #[test]
    fn only_hypercore_s_read_precompiles_are_taken_for_them() {
        assert!(is_precompile(PROBE));
        let mut last = Address::ZERO;
        last[18] = 0x08;
        last[19] = 0xff;
        assert!(is_precompile(last));
        let mut ecrecover = Address::ZERO;
        ecrecover[19] = 0x01;
        assert!(!is_precompile(ecrecover));
        assert!(!is_precompile(Address::repeat_byte(0x08)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_marked_as_not_serving_hypercore_is_asked_last_and_only_once_its_mark_is_old()
     {
        let hypercore = HyperCore::default();
        hypercore.saw(0, false);
        hypercore.saw(2, true);
        assert_eq!(hypercore.order(3), vec![2, 1]);
        tokio::time::advance(ASK_AGAIN_AFTER).await;
        assert_eq!(hypercore.order(3), vec![2, 1, 0]);
    }

    #[tokio::test(start_paused = true)]
    async fn one_failure_of_the_endpoint_that_serves_hypercore_keeps_it_first() {
        let hypercore = HyperCore::default();
        hypercore.saw(0, false);
        hypercore.saw(1, true);
        hypercore.failed(1, true);
        assert_eq!(hypercore.order(3), vec![1, 2]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_that_serves_hypercore_is_marked_only_after_repeated_failures_close_together()
     {
        let hypercore = HyperCore::default();
        hypercore.saw(0, true);
        // Failures spread wider than the span never add up.
        for _ in 0..FAILURES_TO_MARK + 1 {
            hypercore.failed(0, true);
            tokio::time::advance(FAILURE_SPAN).await;
        }
        assert_eq!(hypercore.order(2), vec![0, 1]);
        for _ in 0..FAILURES_TO_MARK {
            hypercore.failed(0, true);
        }
        assert_eq!(hypercore.order(2), vec![1, 0]);
    }

    #[tokio::test(start_paused = true)]
    async fn with_no_endpoint_serving_hypercore_left_the_marked_ones_are_still_asked_once_serving_first()
     {
        let hypercore = HyperCore::default();
        hypercore.saw(0, false);
        hypercore.saw(1, true);
        for _ in 0..FAILURES_TO_MARK {
            hypercore.failed(1, true);
        }
        // The endpoint not known goes first, then the one that served, then
        // the one that never did.
        assert_eq!(hypercore.order(3), vec![2, 1, 0]);
        hypercore.saw(2, false);
        assert_eq!(hypercore.order(3), vec![1, 0, 2]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_refusing_every_connection_is_marked_though_it_names_no_precompile() {
        let hypercore = HyperCore::default();
        hypercore.saw(1, true);
        for _ in 0..FAILURES_TO_MARK {
            hypercore.failed_forwarding(
                0,
                &[],
                "Post \"\": dial tcp 172.18.0.2:3001: connect: connection refused",
            );
        }
        assert_eq!(hypercore.order(2), vec![1]);
    }

    #[test]
    fn an_empty_answer_shows_no_endpoint_serving_hypercore() {
        assert!(!is_answer("\"0x\""));
        assert!(is_answer(
            "\"0x000000000000000000000000000000000000000000000000000000002c34b2e0\""
        ));
    }

    #[test]
    fn the_target_of_a_call_and_of_an_estimate_is_read_from_the_request() {
        let to = Address::repeat_byte(0x7a);
        for method in ["eth_call", "eth_estimateGas"] {
            let request = alloy::rpc::json_rpc::Request::new(
                method,
                alloy::rpc::json_rpc::Id::Number(1),
                (serde_json::json!({"to": to, "data": "0x"}), "latest"),
            )
            .serialize()
            .unwrap();
            assert_eq!(targets(&RequestPacket::Single(request)), vec![to]);
        }
        let other = alloy::rpc::json_rpc::Request::new(
            "eth_getBalance",
            alloy::rpc::json_rpc::Id::Number(1),
            (to, "latest"),
        )
        .serialize()
        .unwrap();
        assert!(targets(&RequestPacket::Single(other)).is_empty());
    }
}
