//! Several sends from one account in flight at once.
//!
//! A [`Client::pipelined`] client holds an account's turn to send only while
//! a transaction is numbered, signed and handed to the endpoints. Its
//! receipt is then waited for outside the turn, so the account's next send
//! can go out before the last one has landed, and both land in the same
//! block.
//!
//! What keeps that safe:
//!
//! - A nonce is held for a send from the moment it is numbered until the
//!   chain shows it used ([`NonceFloor`]), so no two sends of this client
//!   share one, and a node that hasn't seen the last send yet can't have the
//!   next numbered the same.
//! - A transaction is signed once. One the node no longer knows is handed
//!   over again as the same bytes, so it can land only once.
//! - The only other transaction ever signed for a nonce is a filler, a
//!   transfer of nothing to the account itself, which either lands instead
//!   of the send or doesn't land at all. It is sent when the send was
//!   dropped and its bytes are refused, when it is the account's next to
//!   land and hasn't two thirds of the way to its deadline, or when a nonce
//!   below a send that no send holds keeps it from landing; so a send that
//!   never lands never holds the account's later sends up for long.
//! - A send whose nonce the chain shows used by another transaction (a
//!   filler, an external send of the same key, a send of the process before
//!   a restart) can never land, and is said to have been [`Superseded`]:
//!   whatever it was for may be sent again, and only that send can land.
//!   That takes a sign that another transaction used it: a filler's
//!   receipt, or the send's own receipt still missing [`USED_BLOCKS`] blocks
//!   after the nonce was first seen used, which no node a few blocks behind
//!   another explains. A nonce used with neither is the send's own whose
//!   receipt hasn't reached the node asked yet: its receipt is looked for
//!   until the send's deadline, and past it the send is said
//!   [`NeverLanded`], not superseded, since it may have landed.

use std::time::Duration;

use alloy::consensus::Transaction as _;
use alloy::eips::eip2718::Encodable2718;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use anyhow::{Context, Result, anyhow};
use tokio::time::Instant;

use super::{
    Client, Endpoints, Handed, LANDING_TIMEOUT, NeverLanded, NeverSent, NonceFloor, POLL_TIMEOUT,
    SigningProvider, hand_over, nonce_too_low, pending_count,
};

/// A send whose nonce the chain shows used by another transaction: it can
/// never land.
#[derive(Debug)]
pub struct Superseded;

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("its nonce was used by another transaction, so it can never land")
    }
}

impl std::error::Error for Superseded {}

/// How many receipt polls apart a send in flight is looked at more closely:
/// whether the node still knows it, and whether its nonce has been used.
const LOOK_EVERY: u32 = 3;

/// How many blocks past the head at which a nonce was first seen used the
/// chain must be, with no receipt of the send's or its fillers' found all
/// along, before the send is taken as superseded. Behind a load balancer the
/// node answering for receipts may be a few blocks behind the one that
/// counts the account's sends, so a missing receipt alone says nothing
/// until it has stayed missing far longer than such a node lags.
const USED_BLOCKS: u64 = 32;

/// How many looks a send may wait behind a nonce no send holds before that
/// nonce is filled.
const GAP_LOOKS: u32 = 2;

/// What a filler's fees must exceed the transaction it replaces by, in
/// quarters: a node takes a replacement only at a higher fee (10% for most).
const FEE_BUMP_QUARTERS: u128 = 5;

impl Client {
    /// This client, with its sends from an account it signs for no longer
    /// waiting for each other's receipts: each holds the account's turn only
    /// until it is handed over (see the module's documentation). Every clone
    /// of it shares the nonces it holds with every other clone of the
    /// client. A node that signs for the account numbers its sends itself,
    /// and they never waited for each other.
    pub fn pipelined(mut self) -> Self {
        self.pipelined = true;
        self
    }

    /// This client, with every send waiting at most `within` for its
    /// receipt (a minute unless shorter already): past it the send is said
    /// never to have landed, and a send in flight on a
    /// [`Client::pipelined`] client is filled two thirds of the way there.
    pub fn waiting_at_most(mut self, within: Duration) -> Self {
        self.receipt_wait = Some(within);
        self
    }

    /// Number, sign and hand over a send while the account's `turn` is held;
    /// then let the turn go and wait for what becomes of it.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn sent_in_flight(
        &self,
        turn: tokio::sync::OwnedMutexGuard<()>,
        signing: SigningProvider,
        from: Address,
        request: TransactionRequest,
        within: Duration,
        sending: &str,
        what: &str,
        sent_as: impl FnOnce(B256) + Send,
    ) -> Result<TransactionReceipt> {
        let started = Instant::now();
        let deadline = started + within;
        let mut taken_once = false;
        let (flight, claim) = loop {
            let counted = pending_count(&self.provider, from)
                .await
                .with_context(|| sending.to_string())?;
            let claim = Claim::new(&self.nonces, from, counted);
            let filled = signing
                .fill(request.clone().with_nonce(claim.nonce))
                .await
                .with_context(|| sending.to_string())
                .map_err(NeverSent::wrap)?;
            let Some(signed) = filled.as_envelope() else {
                return Err(NeverSent::wrap(anyhow!(
                    "{what} was filled in but not signed"
                )));
            };
            let flight = Flight {
                hash: *signed.tx_hash(),
                raw: signed.encoded_2718(),
                fees: Fees::of(signed),
            };
            match hand_over(&self.endpoints, &flight.raw, deadline).await {
                Handed::MayLand => break (flight, claim),
                // The account's count moved under this send (an external
                // send of the same key, or one of the process before a
                // restart, holds the nonce): numbered again past it, once.
                Handed::Refused(refusal) if !taken_once && nonce_taken(&refusal) => {
                    claim.used();
                    taken_once = true;
                }
                Handed::Refused(refusal) => {
                    return Err(anyhow::Error::new(refusal).context(sending.to_string()));
                }
                Handed::NotTaken(limited) => {
                    return Err(NeverSent::wrap(
                        anyhow::Error::new(limited).context(sending.to_string()),
                    ));
                }
            }
        };
        drop(turn);
        sent_as(flight.hash);
        let nonce = claim.keep();
        let (told, outcome) = tokio::sync::oneshot::channel();
        let watch = Watch {
            provider: self.provider.clone(),
            endpoints: self.endpoints.clone(),
            signing,
            nonces: self.nonces.clone(),
            from,
            nonce,
        };
        // Watched on a task of its own: a caller that stops waiting leaves
        // it watched, and filled if it never lands.
        tokio::spawn(watch.until_used(flight, started, within, told));
        match tokio::time::timeout_at(deadline, outcome).await {
            Ok(Ok(outcome)) => outcome.with_context(|| what.to_string()),
            _ => Err(anyhow::Error::new(NeverLanded)).with_context(|| what.to_string()),
        }
    }
}

/// Whether a node's refusal of a send says another transaction holds its
/// nonce: one mined ("nonce too low", the send's own receipt already looked
/// for by [`hand_over`]), or one in the pool the send would have to outbid.
fn nonce_taken(refusal: &alloy::transports::TransportError) -> bool {
    let alloy::rpc::json_rpc::RpcError::ErrorResp(payload) = refusal else {
        return false;
    };
    nonce_too_low(refusal)
        || payload
            .message
            .to_lowercase()
            .contains("replacement transaction underpriced")
}

/// A nonce held for a send until it is known to be handed over. One dropped
/// before that (the send refused, or its caller gone) is freed again.
struct Claim {
    nonces: NonceFloor,
    from: Address,
    nonce: u64,
    held: bool,
}

impl Claim {
    fn new(nonces: &NonceFloor, from: Address, counted: u64) -> Self {
        Self {
            nonces: nonces.clone(),
            from,
            nonce: nonces.claim(from, counted),
            held: true,
        }
    }

    /// Handed over: the nonce stays held for the watch.
    fn keep(mut self) -> u64 {
        self.held = false;
        self.nonce
    }

    /// The chain holds the nonce for another transaction.
    fn used(mut self) {
        self.held = false;
        self.nonces.landed(self.from, self.nonce);
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.held {
            self.nonces.released(self.from, self.nonce);
        }
    }
}

/// A transaction handed over: its hash, its signed bytes and its fees.
struct Flight {
    hash: B256,
    raw: Vec<u8>,
    fees: Fees,
}

/// What a transaction offers to pay for its gas.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fees {
    /// An EIP-1559 transaction's cap and tip.
    Capped { max: u128, tip: u128 },
    /// A legacy transaction's gas price.
    Priced(u128),
}

impl Fees {
    fn of(signed: &alloy::consensus::TxEnvelope) -> Self {
        match signed.gas_price() {
            Some(price) => Fees::Priced(price),
            None => Fees::Capped {
                max: signed.max_fee_per_gas(),
                tip: signed.max_priority_fee_per_gas().unwrap_or_default(),
            },
        }
    }

    /// Fees a node takes as a replacement for these, never below `now`'s.
    fn outbidding(self, now: Option<Fees>) -> Self {
        let bump = |fee: u128| {
            fee.saturating_mul(FEE_BUMP_QUARTERS)
                .div_ceil(4)
                .saturating_add(1)
        };
        match (self, now) {
            (
                Fees::Capped { max, tip },
                Some(Fees::Capped {
                    max: now,
                    tip: now_tip,
                }),
            ) => Fees::Capped {
                max: bump(max).max(now),
                tip: bump(tip).max(now_tip),
            },
            (Fees::Capped { max, tip }, _) => Fees::Capped {
                max: bump(max),
                tip: bump(tip),
            },
            (Fees::Priced(price), Some(Fees::Priced(now))) => Fees::Priced(bump(price).max(now)),
            (Fees::Priced(price), _) => Fees::Priced(bump(price)),
        }
    }
}

/// What a look at a send in flight found.
#[derive(Debug, PartialEq)]
enum Seen {
    /// The account's nonce count has passed it with no receipt of its own
    /// found; `head` is the chain's latest block, as the node says.
    Used { head: u64 },
    /// No node asked knows any transaction signed for it, and its nonce is
    /// unused.
    Unknown,
    /// Known and waiting; `gap` is a nonce below it that the chain hasn't
    /// used, if any.
    Waiting { gap: Option<u64> },
}

/// A send in flight, watched until its nonce is used.
struct Watch {
    provider: DynProvider,
    endpoints: Endpoints,
    signing: SigningProvider,
    nonces: NonceFloor,
    from: Address,
    nonce: u64,
}

impl Watch {
    /// Watch the send until its nonce is used, by it (its receipt), a filler
    /// or another transaction, or until [`LANDING_TIMEOUT`]; tell the caller
    /// on `told` as soon as that is known. Two thirds of the way to the
    /// caller's deadline (`within` from `started`), a send that hasn't
    /// landed and is the account's next to land is filled, and filled again
    /// with higher fees every third of `within` after while the nonce stays
    /// unused.
    async fn until_used(
        self,
        flight: Flight,
        started: Instant,
        within: Duration,
        told: tokio::sync::oneshot::Sender<Result<TransactionReceipt>>,
    ) {
        let outcome = self.watched(flight, started, within).await;
        let _ = told.send(outcome);
    }

    async fn watched(
        &self,
        flight: Flight,
        started: Instant,
        within: Duration,
    ) -> Result<TransactionReceipt> {
        let every = self.provider.client().poll_interval();
        let ends = started + LANDING_TIMEOUT.max(within);
        let mut fill_at = started + within * 2 / 3;
        // Every transaction signed for the nonce: the send, then its fillers.
        let mut hashes = vec![flight.hash];
        let mut fees = flight.fees;
        let mut polls = 0u32;
        // The head at which the nonce was first seen used with no receipt,
        // and the highest head seen since.
        let mut used_at: Option<(u64, u64)> = None;
        let mut gap_looks = 0u32;
        loop {
            if let Some(receipt) = self.receipt_of_any(&hashes).await {
                self.nonces.landed(self.from, self.nonce);
                if receipt.transaction_hash == flight.hash {
                    return Ok(receipt);
                }
                return Err(anyhow::Error::new(Superseded));
            }
            polls += 1;
            let now = Instant::now();
            if now >= ends {
                // Nothing more is known: the nonce is let go, and a send that
                // takes it next either lands or is superseded.
                self.nonces.released(self.from, self.nonce);
                return Err(anyhow::Error::new(NeverLanded));
            }
            if polls.is_multiple_of(LOOK_EVERY) || now >= fill_at {
                match self.look(&hashes).await {
                    Some(Seen::Used { head }) => {
                        // Used, by this send or another: no later send is
                        // numbered at it.
                        self.nonces.landed(self.from, self.nonce);
                        let (first, highest) = used_at.get_or_insert((head, head));
                        *highest = (*highest).max(head);
                        if *highest >= first.saturating_add(USED_BLOCKS) {
                            return Err(anyhow::Error::new(Superseded));
                        }
                        // Its receipt may still be on its way: nothing says
                        // it didn't land, so it is never said superseded
                        // for want of time.
                        if now >= started + within {
                            return Err(anyhow::Error::new(NeverLanded));
                        }
                    }
                    Some(Seen::Unknown) => {
                        used_at = None;
                        // The same bytes again: they can land only once.
                        // Refused (its fee too low now, say), the nonce is
                        // filled instead.
                        let handed =
                            hand_over(&self.endpoints, &flight.raw, now + POLL_TIMEOUT).await;
                        let refused =
                            matches!(&handed, Handed::Refused(refusal) if !nonce_taken(refusal));
                        if (refused || now >= fill_at)
                            && let Some(filled) = self.fill(self.nonce, fees).await
                        {
                            hashes.push(filled.0);
                            fees = filled.1;
                            fill_at = now + within / 3;
                        }
                    }
                    Some(Seen::Waiting { gap }) => {
                        used_at = None;
                        // A transaction the node holds at that nonce (one
                        // of a process before a restart, or an external
                        // send) usually lands within a block or two: only
                        // one still in the way a third of the way to this
                        // send's deadline is filled over.
                        match gap {
                            Some(gap)
                                if now >= started + within / 3
                                    && self.nonces.unheld(self.from, gap) =>
                            {
                                gap_looks += 1;
                                if gap_looks >= GAP_LOOKS {
                                    gap_looks = 0;
                                    self.fill_gap(gap, fees).await;
                                }
                            }
                            _ => gap_looks = 0,
                        }
                        // Behind an unused nonce this send can't land
                        // whatever it offers: the send holding that nonce
                        // fills it, or the gap is filled above.
                        if now >= fill_at
                            && gap.is_none()
                            && let Some(filled) = self.fill(self.nonce, fees).await
                        {
                            hashes.push(filled.0);
                            fees = filled.1;
                            fill_at = now + within / 3;
                        }
                    }
                    None => {}
                }
            }
            tokio::time::sleep(every).await;
        }
    }

    /// The receipt of whichever of `hashes` has one.
    async fn receipt_of_any(&self, hashes: &[B256]) -> Option<TransactionReceipt> {
        for hash in hashes {
            if let Ok(Ok(Some(receipt))) =
                tokio::time::timeout(POLL_TIMEOUT, self.provider.get_transaction_receipt(*hash))
                    .await
            {
                return Some(receipt);
            }
        }
        None
    }

    /// Whether the nonce is used, and whether the node knows any of
    /// `hashes`. A read that fails says nothing.
    async fn look(&self, hashes: &[B256]) -> Option<Seen> {
        let mined = tokio::time::timeout(
            POLL_TIMEOUT,
            self.provider.get_transaction_count(self.from).latest(),
        )
        .await
        .ok()?
        .ok()?;
        if mined > self.nonce {
            // A receipt that came between the two reads is its own.
            if self.receipt_of_any(hashes).await.is_some() {
                return None;
            }
            let head = tokio::time::timeout(POLL_TIMEOUT, self.provider.get_block_number())
                .await
                .ok()?
                .ok()?;
            return Some(Seen::Used { head });
        }
        for hash in hashes {
            let known =
                tokio::time::timeout(POLL_TIMEOUT, self.provider.get_transaction_by_hash(*hash))
                    .await
                    .ok()?
                    .ok()?;
            if known.is_some() {
                let gap = (mined < self.nonce).then_some(mined);
                return Some(Seen::Waiting { gap });
            }
        }
        Some(Seen::Unknown)
    }

    /// Sign and hand over a filler at `nonce` (a transfer of nothing to the
    /// account itself) whose fees outbid `fees`; its hash and fees, if it
    /// was handed over.
    async fn fill(&self, nonce: u64, fees: Fees) -> Option<(B256, Fees)> {
        let now = self.fees_now().await;
        let fees = fees.outbidding(now);
        let mut request = TransactionRequest::default()
            .with_from(self.from)
            .with_to(self.from)
            .with_value(U256::ZERO)
            .with_nonce(nonce)
            .with_gas_limit(21_000);
        request = match fees {
            Fees::Capped { max, tip } => request
                .with_max_fee_per_gas(max)
                .with_max_priority_fee_per_gas(tip),
            Fees::Priced(price) => request.with_gas_price(price),
        };
        let filled = self.signing.fill(request).await.ok()?;
        let signed = filled.as_envelope()?;
        let hash = *signed.tx_hash();
        let deadline = Instant::now() + POLL_TIMEOUT;
        match hand_over(&self.endpoints, &signed.encoded_2718(), deadline).await {
            Handed::MayLand => {
                tracing::warn!(
                    from = %self.from,
                    nonce,
                    "a send that did not land in time is replaced by a transfer of nothing, so the sends after it can land"
                );
                Some((hash, fees))
            }
            Handed::Refused(_) | Handed::NotTaken(_) => None,
        }
    }

    /// Fill a nonce below this send's that the chain hasn't used and no send
    /// holds: something dropped there keeps every later send from landing.
    /// The nonce is held while it is filled, so no send is numbered at it
    /// meanwhile; a send numbered there before is the chain's to choose.
    async fn fill_gap(&self, gap: u64, fees: Fees) {
        if !self.nonces.hold(self.from, gap) {
            return;
        }
        let filled = self.fill(gap, fees).await;
        // Whether the filler or something else lands there, the next look
        // says; a send numbered later starts at the node's count again.
        self.nonces.released(self.from, gap);
        if filled.is_none() {
            tracing::debug!(from = %self.from, gap, "a nonce below a send in flight could not be filled");
        }
    }

    /// The fees a send made now would offer, if the node says.
    async fn fees_now(&self) -> Option<Fees> {
        match self.provider.estimate_eip1559_fees().await {
            Ok(estimate) => Some(Fees::Capped {
                max: estimate.max_fee_per_gas,
                tip: estimate.max_priority_fee_per_gas,
            }),
            Err(_) => self.provider.get_gas_price().await.ok().map(Fees::Priced),
        }
    }
}

impl NonceFloor {
    /// Whether no send of `from`'s holds `nonce`.
    fn unheld(&self, from: Address, nonce: u64) -> bool {
        self.lock()
            .get(&from)
            .is_none_or(|numbered| !numbered.flying.contains(&nonce))
    }

    /// Hold exactly `nonce` for `from`, if no send holds it.
    fn hold(&self, from: Address, nonce: u64) -> bool {
        self.lock().entry(from).or_default().flying.insert(nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_in_flight_holds_its_nonce_and_the_next_send_takes_the_one_after() {
        let nonces = NonceFloor::default();
        let one = Address::repeat_byte(1);
        assert_eq!(nonces.claim(one, 4), 4);
        // The node hasn't counted the first yet.
        assert_eq!(nonces.claim(one, 4), 5);
        assert_eq!(nonces.claim(one, 5), 6);
        // The first landed; the second never went out: its nonce is the
        // next one numbered.
        nonces.landed(one, 4);
        nonces.released(one, 5);
        assert_eq!(nonces.claim(one, 5), 5);
        assert_eq!(nonces.number(one, 5), 7);
    }

    #[test]
    fn a_filler_outbids_the_send_it_replaces_and_the_fees_of_the_moment() {
        let old = Fees::Capped { max: 100, tip: 10 };
        assert_eq!(old.outbidding(None), Fees::Capped { max: 126, tip: 14 });
        assert_eq!(
            old.outbidding(Some(Fees::Capped { max: 300, tip: 1 })),
            Fees::Capped { max: 300, tip: 14 }
        );
        assert_eq!(Fees::Priced(80).outbidding(None), Fees::Priced(101));
    }
}
