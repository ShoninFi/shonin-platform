//! What the factory and its vaults keep.
//!
//! Where the EVM vault keeps a mapping or a growing array, this program keeps
//! one account per entry at an address its seeds derive: a holder's shares,
//! each request and each claim. The vault's own account holds everything
//! else, the snapshot of the last priced valuation included; every valuation
//! is also emitted as an event, which is where the record is rebuilt from.

use anchor_lang::prelude::*;
use shonin_prices::Allowances;

/// The factory's settings: one per deployment.
pub const FACTORY_SEED: &[u8] = b"factory";
/// One strategy's vault: seeds `[VAULT_SEED, strategy id as u64 LE]`, so the
/// address is known before the vault exists and the author can name it.
pub const VAULT_SEED: &[u8] = b"vault";
/// One holder's shares in one vault: seeds `[HOLDER_SEED, vault, owner]`.
pub const HOLDER_SEED: &[u8] = b"holder";
/// One request: seeds `[REQUEST_SEED, vault, maker, nonce as u64 LE]`. Its
/// maker is the key that signed for it, or for a request the program makes,
/// the claim or the vault it is made for; the nonce is the maker's to choose
/// (the program's, the id of the request it made last for the same maker).
/// Nobody can make a request at another maker's address, so no request can
/// take the place another was built for. Its id, its place in the queue, is
/// given when it is made and kept in the account.
pub const REQUEST_SEED: &[u8] = b"request";
/// One claim: seeds `[CLAIM_SEED, vault, id as u64 LE]`. A claim's id is the
/// id of the withdrawal it was booked from.
pub const CLAIM_SEED: &[u8] = b"claim";
/// The vault's restricted list: seeds `[RESTRICTED_SEED, vault]`.
pub const RESTRICTED_SEED: &[u8] = b"restricted";

/// Virtual shares and one virtual asset unit, so a share starts at $1 and the
/// first deposit can't be rounded away.
pub const OFFSET: u128 = 1_000_000_000_000;
/// `s` share units at mark `m` are worth `s * m / PRICE` asset units.
pub const PRICE: u128 = 1_000_000_000_000_000_000_000_000_000_000;
pub const BPS: u128 = 10_000;
pub const WAD: u128 = 1_000_000_000_000_000_000;
pub const YEAR: u64 = 365 * 24 * 60 * 60;
pub const UNIT_SCALE: u128 = PRICE;
/// The dollar's decimals: the share maths (`OFFSET`, the ceilings below)
/// assumes six, as the EVM vault does.
pub const DECIMALS: u8 = 6;

// Ceilings on the terms, so no change of config can make a fee take what it
// was never described as taking.
pub const MAX_HOLDING_BPS: u16 = 500;
pub const MAX_PROFIT_BPS: u16 = 5_000;
pub const MAX_TRANSACTION_BPS: u16 = 100;
pub const MAX_EXIT_BAND_BPS: u16 = 1_000;
/// The highest minimum deposit, a hundred of the six-decimal dollar: a tenth
/// of it is the dust the vault ignores at its station.
pub const MAX_MINIMUM_DEPOSIT: u64 = 100_000_000;
/// The longest any of a vault's waits may be.
pub const MAX_TIMER: u64 = 365 * 24 * 60 * 60;
/// The most codes a vault's restricted list holds: the baseline, the most a
/// record may carry and the most every driver may add, with room to grow.
pub const MAX_LIST: usize = 255;
/// The most codes one driver's list may add when it is read again: as the
/// EVM vault caps it.
pub const MAX_REFRESHED: usize = 64;
/// The most factories a replacement factory looks back through.
pub const MAX_PREVIOUS: usize = 8;
/// The most requests one settlement takes. Each of a settlement's events is a
/// call of the program's own (see `events::Said`), a request makes at most six
/// calls and booking what the station sent home one more, and a transaction
/// makes at most 64 in all: 48 and one, with the settlement itself and a few
/// compute budget instructions, leave room.
pub const MAX_SETTLE: usize = 8;
/// The most an author may ask their tank kept at: a billion dollars, far past
/// any month of allowances, so no fill asks for more than a vault can count.
pub const MAX_TANK_KEEP: u64 = 1_000_000_000_000_000;
/// The most shares a vault counts, the virtual offset included: a deposit
/// whose shares would take it past this goes back to its payer, so the fees
/// minted on top always fit 128 bits.
pub const MAX_SUPPLY: u128 = u128::MAX >> 16;

/// The fees a vault keeps for life.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct Fees {
    /// A year, on value, to the platform.
    pub holding_bps: u16,
    /// Of new profit, to the author.
    pub author_bps: u16,
    /// Of the same profit, to the platform.
    pub platform_bps: u16,
    /// On every deposit and withdrawal, kept in the pool.
    pub transaction_bps: u16,
}

/// Every timer a vault runs on, as the EVM vault's, without the ones only a
/// vault trading on other chains needs (the post deadline, money in transit,
/// the activation window).
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
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

/// The terms every new vault is created with. Changing them never touches a
/// vault that already exists.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct Terms {
    pub fees: Fees,
    /// How far from the venue's own price a leaver's close may fill.
    pub exit_band_bps: u16,
    pub minimum_deposit: u64,
    /// The least an author may activate a vault with.
    pub minimum_tank: u64,
    /// What becomes the dead shares nobody can redeem.
    pub dead_assets: u64,
    pub timers: Timers,
}

/// The factory: the dollar, the terms, and who the admin and the platform
/// are. One per deployment of this program.
#[account]
#[derive(InitSpace)]
pub struct Factory {
    /// May change the terms (for new vaults only) and kill a vault.
    pub admin: Pubkey,
    /// Where fees and bills go: its holder account and its dollar account.
    pub platform: Pubkey,
    /// The dollar, under the classic token program. Fixed for good.
    pub mint: Pubkey,
    pub token_program: Pubkey,
    /// What stands for this cluster in digests and blessings, copied from the
    /// registry, which fixes it for good.
    pub cluster: [u8; 32],
    /// This chain's name as `shonin.toml` writes it: a record naming any
    /// other chain is refused.
    pub chain_name: [u8; 32],
    /// The programs a vault reads and calls, fixed when this program was
    /// built; recorded for anyone reading the factory.
    pub registry: Pubkey,
    pub publishing: Pubkey,
    pub prices: Pubkey,
    pub station_program: Pubkey,
    /// The vault program this one replaces, if any: a strategy that already
    /// has a vault from it, or from any before it, gets no second one here.
    pub previous: Pubkey,
    pub terms: Terms,
    pub vault_count: u64,
    pub bump: u8,
}

/// Where a vault is in its life. A Solana vault trades on its own chain
/// only, so it opens at activation and is never "activating" or "cancelled".
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub enum VaultState {
    #[default]
    None,
    /// Taking deposits, trading.
    Open,
    /// Retired by its author: no deposits; the strategy runs until evicted.
    WindingDown,
    /// A claim or units went unpaid too long: no deposits, claims still
    /// served.
    Frozen,
    /// Stopped by the admin key.
    Killed,
    /// Nothing at the station: value is cash, and fees have stopped.
    Closed,
}

/// Why a vault halted.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub enum HaltReason {
    #[default]
    None,
    Killed,
    /// The admin wound down a vault whose root key was lost.
    Sealed,
    Frozen,
    /// The end of a retirement's wind-down.
    Retired,
}

/// The author's dollars for running costs. Never counted in value.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct Tank {
    pub balance: u64,
    /// "Keep my tank at": what auto-fill tops it back up to. Zero is off.
    pub keep_at: u64,
    /// The running usage total collected so far, and when it was signed.
    pub usage_settled: u64,
    pub usage_as_of: u64,
    /// Where the next collection's cap is measured from.
    pub last_collection: u64,
    /// When the author asked to withdraw without a signed total.
    pub escape_requested_at: u64,
    /// Cap earned and not yet charged: what the last collection left unused,
    /// and what earlier allowances earned since it.
    pub cap_carried: u128,
    /// A tank fill is waiting in the queue.
    pub fill_pending: bool,
    /// The id of the request `keep` last queued a fill by, zero if none: the
    /// nonce of the next one's address, whose maker is the vault.
    pub fill_request: u64,
}

/// The author's allowances, as `shonin_prices::Allowances` lays them out,
/// kept in the vault's account.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct Allowed {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    pub gas_per_day: u64,
}

impl From<Allowances> for Allowed {
    fn from(a: Allowances) -> Self {
        Self {
            fuel_per_sec: a.fuel_per_sec,
            memory_mib: a.memory_mib,
            requests_per_min: a.requests_per_min,
            bytes_per_day: a.bytes_per_day,
            streams: a.streams,
            inference_ms_per_min: a.inference_ms_per_min,
            gas_per_day: a.gas_per_day,
        }
    }
}

impl From<Allowed> for Allowances {
    fn from(a: Allowed) -> Self {
        Self {
            fuel_per_sec: a.fuel_per_sec,
            memory_mib: a.memory_mib,
            requests_per_min: a.requests_per_min,
            bytes_per_day: a.bytes_per_day,
            streams: a.streams,
            inference_ms_per_min: a.inference_ms_per_min,
            gas_per_day: a.gas_per_day,
        }
    }
}

/// The last priced valuation: what every request it settles is priced at.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct Snapshot {
    /// Its index among every valuation recorded, void ones included.
    pub index: u64,
    pub time: u64,
    /// Cash and the station at mark, plus one.
    pub gross: u128,
    /// The same at exit-side prices, never above `gross`.
    pub gross_exit: u128,
    /// Every share, plus the virtual offset.
    pub supply: u128,
    /// The profit fee owed on `gross`, and on `gross_exit`.
    pub fee: u128,
    pub fee_exit: u128,
    /// Cash at the snapshot.
    pub cash: u64,
    /// What the station's exit side was worth above its mark: a deposit pays
    /// it, so it can't buy below what the holdings could be sold for. Fees
    /// and withdrawals never count it.
    pub extra: u128,
    /// Everything the station had taken when it was valued, so the idle floor
    /// is measured on a value pulls can't shrink.
    pub sent: u128,
}

/// Pooled capital behind one strategy. It never trades: its station does.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    // Who it is ----------------------------------------------------------------
    pub strategy_id: u64,
    pub bundle_hash: [u8; 32],
    pub author: Pubkey,
    pub admin: Pubkey,
    pub platform: Pubkey,
    pub mint: Pubkey,
    /// The vault's own dollar account: its associated token account.
    pub dollars: Pubkey,
    pub station: Pubkey,
    pub cluster: [u8; 32],
    pub created_at: u64,
    pub exit_band_bps: u16,
    pub fees: Fees,
    pub timers: Timers,
    pub minimum_deposit: u64,
    /// The drivers its station pins, by name.
    #[max_len(3)]
    pub drivers: Vec<[u8; 32]>,
    pub bump: u8,
    // Where it is in its life -------------------------------------------------
    pub state: VaultState,
    pub halted_at: u64,
    pub retired_at: u64,
    pub halt_reason: HaltReason,
    // Shares ------------------------------------------------------------------
    /// Every share: the holders', the dead shares, and the shares the vault
    /// holds (pending redemptions and claims, unpaid holding fee).
    pub total_supply: u128,
    /// Shares the vault holds for redemptions and claims not yet paid.
    pub pending_shares: u128,
    /// Holding-fee shares minted to the vault, handed over on payday.
    pub unpaid_holding: u128,
    /// The shares minted at activation to nobody.
    pub dead_shares: u128,
    // Accounting ----------------------------------------------------------------
    pub cash: u64,
    pub escrow: u64,
    /// The high-water mark: `1e18` is $1.00 a share.
    pub mark: u128,
    pub last_holding: u64,
    pub last_payment: u64,
    // The tank ------------------------------------------------------------------
    pub tank: Tank,
    pub allowances: Allowed,
    pub allowances_since: u64,
    // The idle floor -------------------------------------------------------------
    pub floor_bps: u16,
    pub pending_floor_bps: u16,
    pub pending_floor_at: u64,
    // Requests, valuations and claims ---------------------------------------------
    /// How many requests have been made: the newest has this id.
    pub request_count: u64,
    /// Requests passed by settlement: the next to settle has this id plus one.
    pub request_cursor: u64,
    /// How far the priced valuation's exits are settled; its deposits follow.
    pub exit_cursor: u64,
    /// How many requests the last priced valuation prices (every one with an
    /// `earliest` at or before its time): those it still owes are the ones
    /// from the cursor up to here.
    pub priced_through: u64,
    /// The `earliest` of the newest group of requests sharing one, and where
    /// that group starts; and the same for the group before it. Requests are
    /// made in order of `earliest`, and with the slot lead shorter than the
    /// slot interval no more than two groups are ever later than a valuation
    /// being recorded, so these four numbers say how many requests it prices.
    pub tail_earliest: u64,
    pub tail_start: u64,
    pub previous_earliest: u64,
    pub previous_start: u64,
    /// How many valuations have been recorded, void ones included, and the
    /// time of the last.
    pub valuation_count: u64,
    pub last_valued_at: u64,
    /// Whether any valuation has been priced; `latest` is the last that was.
    pub priced: bool,
    pub latest: Snapshot,
    /// What the exits of the valuation being settled have paid out, and the
    /// price its deposits pay, fixed once those exits are settled, for the
    /// valuation `deposits_for` (its index plus one).
    pub exit_paid: u128,
    pub deposit_net: u128,
    pub deposit_supply: u128,
    pub deposits_for: u64,
    /// How many claims have been booked.
    pub claim_count: u64,
    /// Claims the cash covered when they were tried, waiting to be priced:
    /// nothing goes out to the station while any is.
    pub queued_claims: u64,
    // The pro-rata fallback --------------------------------------------------------
    /// The fallback's units, as raw amounts; a holder's live fraction of the
    /// station is their raw units times `unit_scale`.
    pub units: u128,
    /// Zero means `WAD`.
    pub unit_scale: u128,
    pub unit_reserve: u64,
    /// What each unit has received, scaled by `UNIT_SCALE`: a 256-bit
    /// number, little-endian.
    pub per_unit: [u8; 32],
    // The station -------------------------------------------------------------------
    /// Everything the station has ever taken from the vault.
    pub sent_total: u128,
    /// The station's running total of what it has sent home, as far as the
    /// vault has booked it: anything above this has arrived since.
    pub station_booked: u128,
    // Who may deposit ----------------------------------------------------------------
    /// The hash of the restricted list a depositor states they are not on.
    pub restricted_hash: [u8; 32],
    // Payouts held ---------------------------------------------------------------------
    /// Every payout held for its receiver, wherever it is recorded.
    pub held_total: u64,
    /// A claim's profit fee in cash the author's or the platform's dollar
    /// account couldn't take.
    pub held_author: u64,
    pub held_platform: u64,
}

impl Vault {
    /// The seeds the vault signs with.
    pub fn seeds(&self) -> [Vec<u8>; 3] {
        [
            VAULT_SEED.to_vec(),
            self.strategy_id.to_le_bytes().to_vec(),
            vec![self.bump],
        ]
    }
}

/// One holder's part of one vault. Shares never move between holders: there
/// is no transfer, so they are not transferable by construction.
#[account]
#[derive(InitSpace)]
pub struct Holder {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub shares: u128,
    /// The hash of the restricted list the owner stated they are not on, by
    /// signing a transaction that named it; zero if never.
    pub statement: [u8; 32],
    /// The fallback's units: raw, what they have received counted down from
    /// `unit_debt`, credited and not yet collected, and since when they have
    /// waited to be served.
    pub units: u128,
    pub unit_debt: u128,
    pub unit_credit: u64,
    pub units_since: u64,
    /// Deposits naming this holder that are not settled yet: the account
    /// can't be closed while any is, so settling one always finds it.
    pub pending: u32,
    /// Who paid for the account, and is refunded when it is closed.
    pub payer: Pubkey,
    pub bump: u8,
}

/// What a request asks for.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub enum RequestKind {
    /// Assets in escrow, for shares at a later valuation.
    #[default]
    Deposit,
    /// Shares held by the vault, for assets at a later valuation.
    Redeem,
    /// A claim the cash covered when it was tried, priced like a withdrawal.
    Claim,
    /// The author's shares, redeemed into the tank.
    TankFill,
}

/// One request, settled at the first priced valuation at or after `earliest`.
/// Its address is its maker's (see `REQUEST_SEED`), so it is known as the
/// vault's request `id` by what it holds.
#[account]
#[derive(InitSpace)]
pub struct Request {
    pub vault: Pubkey,
    /// Its place in the vault's queue, from 1.
    pub id: u64,
    /// Who it is for: the receiver of a deposit's shares or a withdrawal's
    /// dollars.
    pub owner: Pubkey,
    pub kind: RequestKind,
    /// The first valuation time it can settle at.
    pub earliest: u64,
    /// Settled, or taken through the fallback.
    pub done: bool,
    /// Assets for a deposit; shares for a redemption; for a tank fill, shares
    /// when `reference` is zero and dollars when it is one.
    pub amount: u128,
    /// A claim's id, or a tank fill's unit.
    pub reference: u64,
    /// Who paid for the account (and for a deposit, the dollars): refunded
    /// its rent when it is closed, and sent back a deposit that goes back.
    pub payer: Pubkey,
    /// A payout its receiver's dollar account couldn't take, held for them.
    pub held: u64,
    pub held_for: Pubkey,
    pub bump: u8,
    /// Room for a claim: a withdrawal the cash can't pay becomes a claim in an
    /// account funded by this one's rent, so its payer pays for both once.
    pub reserved: [u8; 16],
}

/// Where a claim is.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub enum ClaimState {
    #[default]
    None,
    /// Booked: the leaver's shares are pending and outstanding.
    Pending,
    /// The last try found a market outside the exit band.
    Waiting,
    /// The cash covered it when it was tried: priced at the next valuation,
    /// like any withdrawal.
    Queued,
    Paid,
    /// Taken through the pro-rata fallback.
    Fallback,
}

/// A withdrawal the cash couldn't pay.
#[account]
#[derive(InitSpace)]
pub struct Claim {
    pub id: u64,
    pub owner: Pubkey,
    pub shares: u128,
    pub booked_at: u64,
    pub last_attempt_at: u64,
    pub state: ClaimState,
    /// What it paid.
    pub paid: u64,
    /// The request a queued claim is priced by, the last if it was queued
    /// more than once: the nonce of the next one's address, whose maker is
    /// the claim.
    pub request: u64,
    /// Who paid for the account, and is refunded when it is closed.
    pub payer: Pubkey,
    /// A payout the owner's dollar account couldn't take, held for them.
    pub held: u64,
    pub bump: u8,
}

/// Who a vault refuses: short codes such as `US` or `SANCTIONED`,
/// zero-padded, sorted and without repeats. Only ever grows.
#[account]
pub struct Restricted {
    pub vault: Pubkey,
    pub bump: u8,
    pub codes: Vec<[u8; 32]>,
}

impl Restricted {
    /// The account's size holding `codes` codes.
    pub fn space(codes: usize) -> usize {
        8 + 32 + 1 + 4 + 32 * codes
    }
}

pub fn factory_address() -> Pubkey {
    Pubkey::find_program_address(&[FACTORY_SEED], &crate::ID).0
}

/// Where strategy `strategy_id`'s vault is, or will be.
pub fn vault_address(strategy_id: u64) -> (Pubkey, u8) {
    vault_address_of(&crate::ID, strategy_id)
}

/// The same under another deployment of this program.
pub fn vault_address_of(program: &Pubkey, strategy_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, &strategy_id.to_le_bytes()], program)
}

pub fn holder_address(vault: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[HOLDER_SEED, vault.as_ref(), owner.as_ref()], &crate::ID)
}

/// Where `maker`'s request with `nonce` is, or will be.
pub fn request_address(vault: &Pubkey, maker: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            REQUEST_SEED,
            vault.as_ref(),
            maker.as_ref(),
            &nonce.to_le_bytes(),
        ],
        &crate::ID,
    )
}

pub fn claim_address(vault: &Pubkey, id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CLAIM_SEED, vault.as_ref(), &id.to_le_bytes()], &crate::ID)
}

pub fn restricted_address(vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[RESTRICTED_SEED, vault.as_ref()], &crate::ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_has_room_for_the_claim_it_may_become() {
        const { assert!(Request::INIT_SPACE >= Claim::INIT_SPACE) };
    }

    #[test]
    fn the_largest_restricted_list_fits_the_room_a_program_can_give_it() {
        assert!(Restricted::space(MAX_LIST) <= 10_240);
    }
}
