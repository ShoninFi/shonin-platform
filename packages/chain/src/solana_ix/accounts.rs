//! The program accounts and events a depositor's or an author's flow reads,
//! decoded from the bytes a node returns, on every target.
//!
//! Each type here lays its fields out in the order the program's own type
//! does, so Borsh reads the same bytes into the same values; the tests fill
//! the program's own types with a distinct value in every field, write them
//! as the program writes them, and check each field read back here. A
//! program change this module doesn't follow fails those tests.
//!
//! - [`Vault`], [`Holder`], [`Request`], [`Claim`]: the vault program's
//!   accounts.
//! - [`Station`]: the drivers a vault's station pins, for the driver groups
//!   a claim's close and a deposit's fallback pass on.
//! - [`MarginAccount`]: the stand-in venue's account of a station's
//!   authority, for the markets it holds.
//! - [`token_amount`]: a classic token account's balance.
//! - [`Event`]: the vault's events, from a transaction's logs and from the
//!   calls of its own it says them through.

use alloy_primitives::U256;
use borsh::BorshDeserialize;

use super::Pubkey;

/// Each account's and event's discriminator, as Anchor makes it: the first
/// 8 bytes of `sha256("account:<Type>")` or `sha256("event:<Type>")`.
/// Written out; the tests check each against the program's own.
pub mod discriminator {
    pub const VAULT: [u8; 8] = [211, 8, 232, 43, 2, 152, 117, 119];
    pub const HOLDER: [u8; 8] = [37, 121, 1, 40, 55, 46, 199, 157];
    pub const REQUEST: [u8; 8] = [125, 172, 150, 161, 162, 115, 39, 71];
    pub const CLAIM: [u8; 8] = [155, 70, 22, 176, 123, 215, 246, 102];
    pub const STATION: [u8; 8] = [46, 9, 120, 182, 190, 232, 8, 126];
    pub const MARGIN_ACCOUNT: [u8; 8] = [133, 220, 173, 213, 179, 211, 43, 238];
    pub const KEY_INFO: [u8; 8] = [90, 165, 168, 231, 213, 231, 23, 197];

    pub const REQUESTED: [u8; 8] = [193, 152, 94, 182, 138, 135, 173, 205];
    pub const DEPOSITED: [u8; 8] = [111, 141, 26, 45, 161, 35, 100, 57];
    pub const REFUNDED: [u8; 8] = [35, 103, 149, 246, 196, 123, 221, 99];
    pub const WITHDRAWN: [u8; 8] = [20, 89, 223, 198, 194, 124, 219, 13];
    pub const CLAIM_BOOKED: [u8; 8] = [109, 35, 5, 241, 81, 16, 50, 33];
    pub const CLAIM_PAID: [u8; 8] = [212, 155, 88, 118, 128, 99, 132, 42];
    pub const CLAIM_TRIED: [u8; 8] = [119, 229, 134, 102, 112, 197, 34, 10];
    pub const PRO_RATA_TAKEN: [u8; 8] = [78, 101, 19, 222, 201, 251, 53, 253];
    pub const UNITS_PAID: [u8; 8] = [76, 5, 118, 225, 237, 222, 178, 73];
    pub const HELD: [u8; 8] = [46, 8, 96, 23, 131, 53, 182, 98];
    pub const HELD_COLLECTED: [u8; 8] = [89, 251, 120, 176, 225, 78, 201, 149];
    pub const TANK_FUNDED: [u8; 8] = [111, 95, 139, 152, 157, 129, 209, 234];
    pub const TANK_WITHDRAWN: [u8; 8] = [132, 132, 48, 233, 30, 20, 206, 102];
    pub const TANK_FILLED: [u8; 8] = [230, 246, 61, 235, 242, 161, 14, 31];

    /// What starts the data of a call the program makes of itself to say an
    /// event (Anchor's `EVENT_IX_TAG`, little-endian).
    pub const EVENT_CALL: [u8; 8] = [228, 69, 165, 46, 81, 203, 154, 29];
}

/// An account's fields after its discriminator, if the discriminator is
/// `expected`. Anchor gives an account room it may not fill, so bytes may
/// follow.
fn body<T: BorshDeserialize>(data: &[u8], expected: [u8; 8]) -> Option<T> {
    if data.get(..8)? != expected {
        return None;
    }
    T::deserialize(&mut &data[8..]).ok()
}

fn key(bytes: [u8; 32]) -> Pubkey {
    Pubkey::new_from_array(bytes)
}

// The vault ---------------------------------------------------------------------------

/// Where a vault is in its life, as the program's `VaultState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    None,
    Open,
    WindingDown,
    Frozen,
    Killed,
    Closed,
}

impl VaultState {
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::None,
            1 => Self::Open,
            2 => Self::WindingDown,
            3 => Self::Frozen,
            4 => Self::Killed,
            5 => Self::Closed,
            _ => return None,
        })
    }
}

/// Why a vault halted, as the program's `HaltReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    None,
    Killed,
    Sealed,
    Frozen,
    Retired,
}

impl HaltReason {
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::None,
            1 => Self::Killed,
            2 => Self::Sealed,
            3 => Self::Frozen,
            4 => Self::Retired,
            _ => return None,
        })
    }
}

#[derive(BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fees {
    pub holding_bps: u16,
    pub author_bps: u16,
    pub platform_bps: u16,
    pub transaction_bps: u16,
}

#[derive(BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timers {
    pub slot_interval: u64,
    pub slot_lead: u64,
    pub slot_window: u64,
    pub fallback_after: u64,
    pub usage_freshness: u64,
    pub tank_escape_delay: u64,
    pub floor_delay: u64,
    pub halt_grace: u64,
    pub freeze_after: u64,
    pub wind_down: u64,
}

#[derive(BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tank {
    pub balance: u64,
    pub keep_at: u64,
    pub usage_settled: u64,
    pub usage_as_of: u64,
    pub last_collection: u64,
    pub escape_requested_at: u64,
    pub cap_carried: u128,
    pub fill_pending: bool,
    pub fill_request: u64,
}

/// The author's allowances as the vault keeps them.
#[derive(BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowed {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    pub gas_per_day: u64,
}

/// The last priced valuation.
#[derive(BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub index: u64,
    pub time: u64,
    pub gross: u128,
    pub gross_exit: u128,
    pub supply: u128,
    pub fee: u128,
    pub fee_exit: u128,
    pub cash: u64,
    pub extra: u128,
    pub sent: u128,
}

/// The vault account, every field in the program's order.
#[derive(BorshDeserialize)]
struct VaultBytes {
    strategy_id: u64,
    bundle_hash: [u8; 32],
    author: [u8; 32],
    admin: [u8; 32],
    platform: [u8; 32],
    mint: [u8; 32],
    dollars: [u8; 32],
    station: [u8; 32],
    cluster: [u8; 32],
    created_at: u64,
    exit_band_bps: u16,
    fees: Fees,
    timers: Timers,
    minimum_deposit: u64,
    drivers: Vec<[u8; 32]>,
    bump: u8,
    state: u8,
    halted_at: u64,
    retired_at: u64,
    halt_reason: u8,
    total_supply: u128,
    pending_shares: u128,
    unpaid_holding: u128,
    dead_shares: u128,
    cash: u64,
    escrow: u64,
    mark: u128,
    last_holding: u64,
    last_payment: u64,
    tank: Tank,
    allowances: Allowed,
    allowances_since: u64,
    floor_bps: u16,
    pending_floor_bps: u16,
    pending_floor_at: u64,
    request_count: u64,
    request_cursor: u64,
    exit_cursor: u64,
    priced_through: u64,
    tail_earliest: u64,
    tail_start: u64,
    previous_earliest: u64,
    previous_start: u64,
    valuation_count: u64,
    last_valued_at: u64,
    priced: bool,
    latest: Snapshot,
    exit_paid: u128,
    deposit_net: u128,
    deposit_supply: u128,
    deposits_for: u64,
    claim_count: u64,
    queued_claims: u64,
    units: u128,
    unit_scale: u128,
    unit_reserve: u64,
    per_unit: [u8; 32],
    sent_total: u128,
    station_booked: u128,
    restricted_hash: [u8; 32],
    held_total: u64,
    held_author: u64,
    held_platform: u64,
}

/// A vault, as its account holds it: what a flow and a quote read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vault {
    pub strategy_id: u64,
    pub bundle_hash: [u8; 32],
    pub author: Pubkey,
    pub admin: Pubkey,
    pub platform: Pubkey,
    pub mint: Pubkey,
    pub dollars: Pubkey,
    pub station: Pubkey,
    pub cluster: [u8; 32],
    pub created_at: u64,
    pub exit_band_bps: u16,
    pub fees: Fees,
    pub timers: Timers,
    pub minimum_deposit: u64,
    pub drivers: Vec<[u8; 32]>,
    pub state: VaultState,
    pub halted_at: u64,
    pub retired_at: u64,
    pub halt_reason: HaltReason,
    pub total_supply: u128,
    pub pending_shares: u128,
    pub unpaid_holding: u128,
    pub dead_shares: u128,
    pub cash: u64,
    pub escrow: u64,
    /// The high-water mark: `1e30` (the program's `PRICE`) is a dollar a share.
    pub mark: u128,
    pub tank: Tank,
    pub allowances: Allowed,
    pub allowances_since: u64,
    pub floor_bps: u16,
    pub pending_floor_bps: u16,
    pub pending_floor_at: u64,
    pub request_count: u64,
    pub request_cursor: u64,
    pub priced_through: u64,
    pub valuation_count: u64,
    pub last_valued_at: u64,
    pub priced: bool,
    pub latest: Snapshot,
    pub claim_count: u64,
    pub queued_claims: u64,
    pub units: u128,
    pub unit_scale: u128,
    pub unit_reserve: u64,
    /// What each unit has received, scaled by the program's `UNIT_SCALE`.
    pub per_unit: U256,
    pub restricted_hash: [u8; 32],
    pub held_total: u64,
    pub held_author: u64,
    pub held_platform: u64,
}

impl Vault {
    /// The vault from its account's bytes: `None` if they are not a vault's.
    pub fn read(data: &[u8]) -> Option<Self> {
        let v: VaultBytes = body(data, discriminator::VAULT)?;
        // Fields read for the layout only.
        let _ = (
            v.bump,
            v.last_holding,
            v.last_payment,
            v.exit_cursor,
            v.tail_earliest,
            v.tail_start,
            v.previous_earliest,
            v.previous_start,
            v.exit_paid,
            v.deposit_net,
            v.deposit_supply,
            v.deposits_for,
            v.sent_total,
            v.station_booked,
        );
        Some(Self {
            strategy_id: v.strategy_id,
            bundle_hash: v.bundle_hash,
            author: key(v.author),
            admin: key(v.admin),
            platform: key(v.platform),
            mint: key(v.mint),
            dollars: key(v.dollars),
            station: key(v.station),
            cluster: v.cluster,
            created_at: v.created_at,
            exit_band_bps: v.exit_band_bps,
            fees: v.fees,
            timers: v.timers,
            minimum_deposit: v.minimum_deposit,
            drivers: v.drivers,
            state: VaultState::from_index(v.state)?,
            halted_at: v.halted_at,
            retired_at: v.retired_at,
            halt_reason: HaltReason::from_index(v.halt_reason)?,
            total_supply: v.total_supply,
            pending_shares: v.pending_shares,
            unpaid_holding: v.unpaid_holding,
            dead_shares: v.dead_shares,
            cash: v.cash,
            escrow: v.escrow,
            mark: v.mark,
            tank: v.tank,
            allowances: v.allowances,
            allowances_since: v.allowances_since,
            floor_bps: v.floor_bps,
            pending_floor_bps: v.pending_floor_bps,
            pending_floor_at: v.pending_floor_at,
            request_count: v.request_count,
            request_cursor: v.request_cursor,
            priced_through: v.priced_through,
            valuation_count: v.valuation_count,
            last_valued_at: v.last_valued_at,
            priced: v.priced,
            latest: v.latest,
            claim_count: v.claim_count,
            queued_claims: v.queued_claims,
            units: v.units,
            unit_scale: v.unit_scale,
            unit_reserve: v.unit_reserve,
            per_unit: U256::from_le_bytes(v.per_unit),
            restricted_hash: v.restricted_hash,
            held_total: v.held_total,
            held_author: v.held_author,
            held_platform: v.held_platform,
        })
    }

    /// Halted (killed, frozen, or at the end of a retirement), as the
    /// program's `halted()`: its halt time is set.
    pub fn halted(&self) -> bool {
        self.halted_at != 0
    }

    /// The program's `empty()`: the station held nothing at the last priced
    /// valuation beyond dust.
    pub fn empty(&self) -> bool {
        self.latest.gross.saturating_add(self.latest.extra)
            <= self.latest.cash as u128 + 1 + (self.minimum_deposit / 10) as u128
    }

    /// The program's `ready_to_close()`: halted, and a valuation since found
    /// the station empty. Such a vault takes no new request.
    pub fn ready_to_close(&self) -> bool {
        self.halted() && self.priced && self.latest.time > self.halted_at && self.empty()
    }
}

// A holder, a request, a claim ---------------------------------------------------------

#[derive(BorshDeserialize)]
struct HolderBytes {
    vault: [u8; 32],
    owner: [u8; 32],
    shares: u128,
    statement: [u8; 32],
    units: u128,
    unit_debt: u128,
    unit_credit: u64,
    units_since: u64,
    pending: u32,
    payer: [u8; 32],
    bump: u8,
}

/// One holder's part of one vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub shares: u128,
    /// The restricted list's hash the owner stated, zero if never.
    pub statement: [u8; 32],
    pub units: u128,
    pub unit_debt: u128,
    pub unit_credit: u64,
    pub units_since: u64,
    pub pending: u32,
    pub payer: Pubkey,
}

/// What `UNIT_SCALE` is in the program: a unit's received total is scaled by
/// this.
pub const UNIT_SCALE: u128 = 1_000_000_000_000_000_000_000_000_000_000;

impl Holder {
    pub fn read(data: &[u8]) -> Option<Self> {
        let h: HolderBytes = body(data, discriminator::HOLDER)?;
        let _ = h.bump;
        Some(Self {
            vault: key(h.vault),
            owner: key(h.owner),
            shares: h.shares,
            statement: h.statement,
            units: h.units,
            unit_debt: h.unit_debt,
            unit_credit: h.unit_credit,
            units_since: h.units_since,
            pending: h.pending,
            payer: key(h.payer),
        })
    }

    /// What `collect_units` would pay now, as the program's `accrue` credits
    /// it: what is credited, plus what the units have earned past their
    /// debt (rounded down). `None` if it doesn't fit the dollar's 64 bits.
    pub fn units_claimable(&self, per_unit: U256) -> Option<u64> {
        let earned = U256::from(self.units).checked_mul(per_unit)? / U256::from(UNIT_SCALE);
        let earned = u128::try_from(earned).ok()?;
        let more = earned.saturating_sub(self.unit_debt);
        self.unit_credit.checked_add(u64::try_from(more).ok()?)
    }
}

/// What a request asks for, as the program's `RequestKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Deposit,
    Redeem,
    Claim,
    TankFill,
}

impl RequestKind {
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::Deposit,
            1 => Self::Redeem,
            2 => Self::Claim,
            3 => Self::TankFill,
            _ => return None,
        })
    }

    /// The same as the instruction builders name it.
    pub fn builder(self) -> super::vault::RequestKind {
        match self {
            Self::Deposit => super::vault::RequestKind::Deposit,
            Self::Redeem => super::vault::RequestKind::Redeem,
            Self::Claim => super::vault::RequestKind::Claim,
            Self::TankFill => super::vault::RequestKind::TankFill,
        }
    }
}

#[derive(BorshDeserialize)]
struct RequestBytes {
    vault: [u8; 32],
    id: u64,
    owner: [u8; 32],
    kind: u8,
    earliest: u64,
    done: bool,
    amount: u128,
    reference: u64,
    payer: [u8; 32],
    held: u64,
    held_for: [u8; 32],
    bump: u8,
    reserved: [u8; 16],
}

/// One request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub vault: Pubkey,
    /// Its place in the vault's queue, from 1.
    pub id: u64,
    pub owner: Pubkey,
    pub kind: RequestKind,
    /// The first valuation time it can settle at.
    pub earliest: u64,
    pub done: bool,
    /// Assets for a deposit, shares for a redemption.
    pub amount: u128,
    pub reference: u64,
    pub payer: Pubkey,
    /// A payout held for `held_for`.
    pub held: u64,
    pub held_for: Pubkey,
}

/// Where a request's fields sit in its account, for a node's filters: the
/// vault after the discriminator, then the id, and the receiver of a held
/// payout further on.
pub mod request_at {
    pub const VAULT: usize = 8;
    pub const ID: usize = 40;
    pub const HELD_FOR: usize = 154;
}

impl Request {
    pub fn read(data: &[u8]) -> Option<Self> {
        let r: RequestBytes = body(data, discriminator::REQUEST)?;
        let _ = (r.bump, r.reserved);
        Some(Self {
            vault: key(r.vault),
            id: r.id,
            owner: key(r.owner),
            kind: RequestKind::from_index(r.kind)?,
            earliest: r.earliest,
            done: r.done,
            amount: r.amount,
            reference: r.reference,
            payer: key(r.payer),
            held: r.held,
            held_for: key(r.held_for),
        })
    }
}

/// Where a claim is, as the program's `ClaimState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimState {
    None,
    Pending,
    Waiting,
    Queued,
    Paid,
    Fallback,
}

impl ClaimState {
    fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::None,
            1 => Self::Pending,
            2 => Self::Waiting,
            3 => Self::Queued,
            4 => Self::Paid,
            5 => Self::Fallback,
            _ => return None,
        })
    }
}

#[derive(BorshDeserialize)]
struct ClaimBytes {
    id: u64,
    owner: [u8; 32],
    shares: u128,
    booked_at: u64,
    last_attempt_at: u64,
    state: u8,
    paid: u64,
    request: u64,
    payer: [u8; 32],
    held: u64,
    bump: u8,
}

/// A withdrawal the cash couldn't pay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub id: u64,
    pub owner: Pubkey,
    pub shares: u128,
    pub booked_at: u64,
    pub last_attempt_at: u64,
    pub state: ClaimState,
    pub paid: u64,
    /// The nonce of the request a queued claim is priced by.
    pub request: u64,
    pub payer: Pubkey,
    pub held: u64,
}

/// Where a claim's owner sits in its account, for a node's filter.
pub const CLAIM_OWNER_AT: usize = 16;

impl Claim {
    pub fn read(data: &[u8]) -> Option<Self> {
        let c: ClaimBytes = body(data, discriminator::CLAIM)?;
        let _ = c.bump;
        Some(Self {
            id: c.id,
            owner: key(c.owner),
            shares: c.shares,
            booked_at: c.booked_at,
            last_attempt_at: c.last_attempt_at,
            state: ClaimState::from_index(c.state)?,
            paid: c.paid,
            request: c.request,
            payer: key(c.payer),
            held: c.held,
        })
    }

    /// The instruction builders' keys for this claim.
    pub fn keys(&self) -> super::vault::ClaimKeys {
        super::vault::ClaimKeys {
            id: self.id,
            owner: self.owner,
            request: self.request,
        }
    }
}

// The station and the stand-in venue ------------------------------------------------------

#[derive(BorshDeserialize)]
#[allow(dead_code)]
enum OwnBytes {
    Station,
    StationDollars,
    StationToken(Option<u8>),
    StationAddress(Vec<u8>),
    Fixed(u8),
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct PositionRule {
    index: u8,
    own: OwnBytes,
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct RuleBytes {
    discriminator: Vec<u8>,
    class: u8,
    positions: Vec<PositionRule>,
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct StationAddressBytes {
    prefix: Vec<u8>,
    address: [u8; 32],
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct PinnedBytes {
    name: [u8; 32],
    program: [u8; 32],
    venue: [u8; 32],
    authority: [u8; 32],
    authority_bump: u8,
    dollars: [u8; 32],
    key: [u8; 32],
    rules: Vec<RuleBytes>,
    fixed: Vec<Vec<[u8; 32]>>,
    addresses: Vec<StationAddressBytes>,
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct StationBytes {
    vault: [u8; 32],
    vault_program: [u8; 32],
    vault_dollars: [u8; 32],
    dollars: [u8; 32],
    mint: [u8; 32],
    bundle_hash: [u8; 32],
    cluster: [u8; 32],
    exit_band_bps: u16,
    idle: u64,
    sent_home: u128,
    stopped: bool,
    stopped_at: u64,
    payer: [u8; 32],
    bump: u8,
    drivers: Vec<PinnedBytes>,
}

/// A driver as a station pins it: what its group is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedDriver {
    pub name: [u8; 32],
    pub pinned: super::vault::Pinned,
}

/// A vault's station: the vault it serves and the drivers it pins, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub vault: Pubkey,
    pub stopped: bool,
    pub drivers: Vec<PinnedDriver>,
}

impl Station {
    pub fn read(data: &[u8]) -> Option<Self> {
        let s: StationBytes = body(data, discriminator::STATION)?;
        Some(Self {
            vault: key(s.vault),
            stopped: s.stopped,
            drivers: s
                .drivers
                .into_iter()
                .map(|d| PinnedDriver {
                    name: d.name,
                    pinned: super::vault::Pinned {
                        program: key(d.program),
                        venue: key(d.venue),
                        authority: key(d.authority),
                        dollars: key(d.dollars),
                    },
                })
                .collect(),
        })
    }
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct VenuePosition {
    market: [u8; 32],
    size: i128,
    funding_index: i128,
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct MarginBytes {
    master: [u8; 32],
    payer: [u8; 32],
    cash: i128,
    positions: Vec<VenuePosition>,
}

/// The stand-in venue's account of one master: the markets it holds, in
/// its order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarginAccount {
    pub master: Pubkey,
    /// Each held market's id.
    pub markets: Vec<[u8; 32]>,
}

impl MarginAccount {
    pub fn read(data: &[u8]) -> Option<Self> {
        let m: MarginBytes = body(data, discriminator::MARGIN_ACCOUNT)?;
        Some(Self {
            master: key(m.master),
            markets: m.positions.into_iter().map(|p| p.market).collect(),
        })
    }

    /// The held markets' accounts at `venue`, in order: what a driver
    /// group lists after the venue's own accounts.
    pub fn market_accounts(&self, venue: &Pubkey) -> Vec<Pubkey> {
        self.markets
            .iter()
            .map(|id| super::address::venue_market(venue, id))
            .collect()
    }
}

/// A classic token account's balance (its `amount`, after the mint and the
/// owner), if `data` is one: 165 bytes.
pub fn token_amount(data: &[u8]) -> Option<u64> {
    if data.len() != 165 {
        return None;
    }
    Some(u64::from_le_bytes(data.get(64..72)?.try_into().ok()?))
}

/// A classic token account's mint and owner.
pub fn token_mint_owner(data: &[u8]) -> Option<(Pubkey, Pubkey)> {
    if data.len() != 165 {
        return None;
    }
    Some((
        Pubkey::new_from_array(data.get(0..32)?.try_into().ok()?),
        Pubkey::new_from_array(data.get(32..64)?.try_into().ok()?),
    ))
}

/// The registry records a signed usage total is checked against, from the
/// signer's key record (at [`super::address::key`]): its blessing for the
/// vault, the key record itself, and its image's and project's records.
pub fn key_records(
    signer: &Pubkey,
    key_info: &[u8],
    cluster: &[u8; 32],
    vault: &Pubkey,
    bundle: &[u8; 32],
) -> Option<super::vault::KeyRecords> {
    if key_info.get(..8)? != discriminator::KEY_INFO {
        return None;
    }
    let image: [u8; 32] = key_info.get(8..40)?.try_into().ok()?;
    let project: [u8; 32] = key_info.get(40..72)?.try_into().ok()?;
    Some(super::vault::KeyRecords {
        blessing: super::address::blessing(signer, cluster, vault, bundle),
        key_info: super::address::key(signer),
        image: super::address::image(&image),
        project: super::address::project(&project),
    })
}

// Events ---------------------------------------------------------------------------------

/// The vault's events a flow reads its outcome from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Requested {
        request_id: u64,
        address: Pubkey,
        owner: Pubkey,
        kind: RequestKind,
        earliest: u64,
        amount: u128,
    },
    Deposited {
        request_id: u64,
        owner: Pubkey,
        assets: u64,
        shares: u128,
    },
    Refunded {
        request_id: u64,
        to: Pubkey,
        assets: u64,
    },
    Withdrawn {
        request_id: u64,
        owner: Pubkey,
        shares: u128,
        assets: u64,
        fee_shares: u128,
    },
    ClaimBooked {
        claim_id: u64,
        owner: Pubkey,
        shares: u128,
    },
    ClaimTried {
        claim_id: u64,
        blocked: bool,
    },
    ClaimPaid {
        claim_id: u64,
        assets: u64,
    },
    ProRataTaken {
        owner: Pubkey,
        shares: u128,
        assets: u64,
        units: u128,
    },
    UnitsPaid {
        owner: Pubkey,
        assets: u64,
    },
    Held {
        to: Pubkey,
        assets: u64,
    },
    HeldCollected {
        to: Pubkey,
        assets: u64,
    },
    TankFunded {
        from: Pubkey,
        assets: u64,
    },
    TankWithdrawn {
        assets: u64,
    },
    TankFilled {
        shares: u128,
        assets: u64,
    },
}

#[derive(BorshDeserialize)]
struct RequestedBytes {
    request_id: u64,
    address: [u8; 32],
    owner: [u8; 32],
    kind: u8,
    earliest: u64,
    amount: u128,
}

#[derive(BorshDeserialize)]
struct WithdrawnBytes {
    request_id: u64,
    owner: [u8; 32],
    shares: u128,
    assets: u64,
    fee_shares: u128,
}

#[derive(BorshDeserialize)]
struct ProRataBytes {
    owner: [u8; 32],
    shares: u128,
    assets: u64,
    units: u128,
}

impl Event {
    /// An event from its bytes: its discriminator, then its fields. `None`
    /// for an event this module doesn't read, or bytes that aren't one.
    pub fn read(data: &[u8]) -> Option<Self> {
        use discriminator as d;
        let tag: [u8; 8] = data.get(..8)?.try_into().ok()?;
        let mut rest = &data[8..];
        let rest = &mut rest;
        Some(match tag {
            d::REQUESTED => {
                let e = RequestedBytes::deserialize(rest).ok()?;
                Self::Requested {
                    request_id: e.request_id,
                    address: key(e.address),
                    owner: key(e.owner),
                    kind: RequestKind::from_index(e.kind)?,
                    earliest: e.earliest,
                    amount: e.amount,
                }
            }
            d::DEPOSITED => {
                let (request_id, owner, assets, shares) =
                    <(u64, [u8; 32], u64, u128)>::deserialize(rest).ok()?;
                Self::Deposited {
                    request_id,
                    owner: key(owner),
                    assets,
                    shares,
                }
            }
            d::REFUNDED => {
                let (request_id, to, assets) = <(u64, [u8; 32], u64)>::deserialize(rest).ok()?;
                Self::Refunded {
                    request_id,
                    to: key(to),
                    assets,
                }
            }
            d::WITHDRAWN => {
                let e = WithdrawnBytes::deserialize(rest).ok()?;
                Self::Withdrawn {
                    request_id: e.request_id,
                    owner: key(e.owner),
                    shares: e.shares,
                    assets: e.assets,
                    fee_shares: e.fee_shares,
                }
            }
            d::CLAIM_BOOKED => {
                let (claim_id, owner, shares) = <(u64, [u8; 32], u128)>::deserialize(rest).ok()?;
                Self::ClaimBooked {
                    claim_id,
                    owner: key(owner),
                    shares,
                }
            }
            d::CLAIM_TRIED => {
                let (claim_id, blocked) = <(u64, bool)>::deserialize(rest).ok()?;
                Self::ClaimTried { claim_id, blocked }
            }
            d::CLAIM_PAID => {
                let (claim_id, assets) = <(u64, u64)>::deserialize(rest).ok()?;
                Self::ClaimPaid { claim_id, assets }
            }
            d::PRO_RATA_TAKEN => {
                let e = ProRataBytes::deserialize(rest).ok()?;
                Self::ProRataTaken {
                    owner: key(e.owner),
                    shares: e.shares,
                    assets: e.assets,
                    units: e.units,
                }
            }
            d::UNITS_PAID => {
                let (owner, assets) = <([u8; 32], u64)>::deserialize(rest).ok()?;
                Self::UnitsPaid {
                    owner: key(owner),
                    assets,
                }
            }
            d::HELD => {
                let (to, assets) = <([u8; 32], u64)>::deserialize(rest).ok()?;
                Self::Held {
                    to: key(to),
                    assets,
                }
            }
            d::HELD_COLLECTED => {
                let (to, assets) = <([u8; 32], u64)>::deserialize(rest).ok()?;
                Self::HeldCollected {
                    to: key(to),
                    assets,
                }
            }
            d::TANK_FUNDED => {
                let (from, assets) = <([u8; 32], u64)>::deserialize(rest).ok()?;
                Self::TankFunded {
                    from: key(from),
                    assets,
                }
            }
            d::TANK_WITHDRAWN => Self::TankWithdrawn {
                assets: u64::deserialize(rest).ok()?,
            },
            d::TANK_FILLED => {
                let (shares, assets) = <(u128, u64)>::deserialize(rest).ok()?;
                Self::TankFilled { shares, assets }
            }
            _ => return None,
        })
    }

    /// An event the vault said through a call of its own: the call's data
    /// is the event-call tag, then the event. `program` is the program the
    /// call went to, which must be the vault program for it to count.
    pub fn from_call(program: &Pubkey, data: &[u8]) -> Option<Self> {
        if *program != super::ids::VAULT || data.get(..8)? != discriminator::EVENT_CALL {
            return None;
        }
        Self::read(&data[8..])
    }
}

/// Every event the vault program said in a transaction, from its log lines
/// and from the calls it made of itself, in that order. `logs` are the
/// node's log lines; a `Program data:` line counts only while the vault
/// program is the one running (the line before it in the invocation stack),
/// so another program can't put words in the vault's mouth. `calls` are the
/// transaction's inner calls as (program, data). `decode` turns a log
/// line's base64 into bytes.
pub fn events_in(
    logs: &[String],
    calls: &[(Pubkey, Vec<u8>)],
    decode: impl Fn(&str) -> Option<Vec<u8>>,
) -> Vec<Event> {
    let vault = super::ids::VAULT.to_string();
    let mut stack: Vec<String> = Vec::new();
    let mut found = Vec::new();
    for line in logs {
        if let Some(rest) = line.strip_prefix("Program ") {
            if let Some(program) = rest
                .split(" invoke [")
                .next()
                .filter(|_| rest.contains(" invoke ["))
            {
                stack.push(program.to_string());
                continue;
            }
            if rest.ends_with(" success") || rest.contains(" failed: ") {
                stack.pop();
                continue;
            }
            if let Some(data) = rest.strip_prefix("data: ")
                && stack.last() == Some(&vault)
                && let Some(bytes) = decode(data.trim())
                && let Some(event) = Event::read(&bytes)
            {
                found.push(event);
            }
        }
    }
    found.extend(
        calls
            .iter()
            .filter_map(|(program, data)| Event::from_call(program, data)),
    );
    found
}

// Errors ---------------------------------------------------------------------------------

/// The vault program's errors, in the order it numbers them: Anchor gives
/// the first 6000 and each next one the next number.
const VAULT_ERRORS: [&str; 30] = [
    "WrongState",
    "NotAllowed",
    "TooSmall",
    "NoStatement",
    "NotBlessed",
    "OutsideWindow",
    "Unsettled",
    "AlreadyValued",
    "NotYet",
    "Nothing",
    "BelowFloor",
    "Stale",
    "NotTheDeployer",
    "NotAdmin",
    "NotTheAuthor",
    "NotReady",
    "VaultExists",
    "TankTooSmall",
    "OutOfRange",
    "TermsChanged",
    "ChainTwice",
    "OtherChainsNotYet",
    "WrongMint",
    "WrongAccount",
    "WrongCursor",
    "StationSilent",
    "ListFull",
    "NoSignature",
    "Overflow",
    "TooMany",
];

/// The name of the vault program's error numbered `code` (a transaction's
/// `Custom` error), if it is one of the vault's own.
pub fn vault_error(code: u32) -> Option<&'static str> {
    let index = code.checked_sub(6000)?;
    VAULT_ERRORS.get(index as usize).copied()
}
