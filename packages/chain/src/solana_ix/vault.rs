//! The vault program's instructions a depositor and an author send.
//!
//! Each builder takes the vault as [`VaultKeys`] and whatever else the
//! instruction names that only the cluster knows (a request's address, a
//! claim's owner, the station's driver accounts), and makes the instruction
//! the program's own types make for the same choices, in the same order. A
//! payout goes to the receiver's dollar account ([`address::dollars`]),
//! which the vault checks by that address; [`super::budget::open_dollars`]
//! opens one first where it may not exist.

use borsh::BorshSerialize;

use super::budget::ed25519;
use super::discriminator as d;
use super::read::usage_digest;
use super::{AccountMeta, Instruction, Pubkey, address, data, ids, r, rs, w, ws};

/// One vault as its instructions name it: its strategy, its address and its
/// station's, its dollar and their dollar accounts, and its fee recipients.
/// The author and the platform are the vault's own (its account names them;
/// the platform is also the factory's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultKeys {
    pub strategy_id: u64,
    pub vault: Pubkey,
    pub station: Pubkey,
    pub mint: Pubkey,
    pub vault_dollars: Pubkey,
    pub station_dollars: Pubkey,
    pub author: Pubkey,
    pub platform: Pubkey,
}

impl VaultKeys {
    /// Strategy `strategy_id`'s vault, its dollar `mint`, authored by
    /// `author`, its fees going to `platform`.
    pub fn new(strategy_id: u64, mint: Pubkey, author: Pubkey, platform: Pubkey) -> Self {
        let vault = address::vault(strategy_id);
        let station = address::station(&vault);
        Self {
            strategy_id,
            vault,
            station,
            mint,
            vault_dollars: address::dollars(&vault, &mint),
            station_dollars: address::dollars(&station, &mint),
            author,
            platform,
        }
    }

    /// `owner`'s holding in this vault.
    pub fn holder(&self, owner: &Pubkey) -> Pubkey {
        address::holder(&self.vault, owner)
    }

    /// `owner`'s dollar account.
    pub fn dollars(&self, owner: &Pubkey) -> Pubkey {
        address::dollars(owner, &self.mint)
    }

    /// The accounts that price and pay (the program's `Pool`): the vault, its
    /// station, the fee recipients' holdings, the vault's dollars.
    fn pool(&self) -> [AccountMeta; 8] {
        [
            w(self.vault),
            w(self.station),
            w(self.holder(&self.author)),
            w(self.holder(&self.platform)),
            w(self.vault_dollars),
            r(self.mint),
            r(ids::TOKEN),
            r(ids::SYSTEM),
        ]
    }

    /// The vault's dollars, the dollar and the token program, which end most
    /// instructions that move money.
    fn purse(&self) -> [AccountMeta; 3] {
        [w(self.vault_dollars), r(self.mint), r(ids::TOKEN)]
    }

    fn ix(&self, accounts: Vec<AccountMeta>, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: ids::VAULT,
            accounts,
            data,
        }
    }
}

/// An optional account the program is not given: Anchor reads the program's
/// own id in its place.
fn none() -> AccountMeta {
    r(ids::VAULT)
}

/// The event authority and the program, which end the accounts of an
/// instruction that says its events through the program's own data.
fn events() -> [AccountMeta; 2] {
    [r(address::event_authority()), r(ids::VAULT)]
}

// Driver groups ---------------------------------------------------------------------

/// The accounts the station's drivers are given, one group per driver it
/// pins, in its order, and how many each takes: what a valuation, a claim's
/// close and a deposit's fallback pass on to the station.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Groups {
    pub accounts: Vec<AccountMeta>,
    pub sizes: Vec<u8>,
}

impl Groups {
    /// No groups: for a fallback that won't ask the station.
    pub fn none() -> Self {
        Self::default()
    }

    /// The next driver's group.
    pub fn push(&mut self, group: Vec<AccountMeta>) {
        self.sizes.push(group.len() as u8);
        self.accounts.extend(group);
    }
}

/// A driver as the station's account pins it: its program, the venue it
/// trades at, the station's authority there and that authority's dollar
/// account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub program: Pubkey,
    pub venue: Pubkey,
    pub authority: Pubkey,
    pub dollars: Pubkey,
}

/// The group the stand-in venue's driver takes: the driver, the authority
/// and its dollars, the venue and its settings, dollar vault, the dollar,
/// the token program and the authority's margin account, then every market
/// the authority holds there (`held`, in the order the venue lists them).
pub fn stand_in_group(pinned: &Pinned, mint: &Pubkey, held: &[Pubkey]) -> Vec<AccountMeta> {
    let venue = pinned.venue;
    let mut group = vec![
        r(pinned.program),
        r(pinned.authority),
        w(pinned.dollars),
        r(venue),
        r(address::venue_state(&venue)),
        w(address::venue_vault(&venue)),
        r(*mint),
        r(ids::TOKEN),
        w(address::venue_account(&venue, &pinned.authority)),
    ];
    group.extend(held.iter().map(|market| r(*market)));
    group
}

// Arguments ---------------------------------------------------------------------------

/// The most an agent may use, as the price list's `Allowances`.
#[derive(BorshSerialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowances {
    pub fuel_per_sec: u64,
    pub memory_mib: u32,
    pub requests_per_min: u32,
    pub bytes_per_day: u64,
    pub streams: u32,
    pub inference_ms_per_min: u32,
    pub gas_per_day: u64,
}

/// Where a held payout is held, as the program's `HeldIn`.
#[derive(BorshSerialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// On request `id`.
    Request(u64),
    /// On claim `id`.
    Claim(u64),
    /// A claim's profit fee in cash, held for the author or the platform.
    Fees,
}

/// A running usage total the vault's executor signed (served at its
/// `/usage/{cluster}/{vault}`): the total, when it was signed, the key that
/// signed, and its signature over [`usage_digest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedUsage {
    pub total: u64,
    pub as_of: u64,
    pub signer: Pubkey,
    pub signature: [u8; 64],
}

/// The registry records a signed total is checked against: the signer's
/// blessing for the vault ([`address::blessing`]), its key record
/// ([`address::key`]), and its image's and project's records (from the key
/// record).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRecords {
    pub blessing: Pubkey,
    pub key_info: Pubkey,
    pub image: Pubkey,
    pub project: Pubkey,
}

#[derive(BorshSerialize)]
struct Usage {
    total: u64,
    as_of: u64,
    signer: [u8; 32],
}

impl From<&SignedUsage> for Usage {
    fn from(signed: &SignedUsage) -> Self {
        Self {
            total: signed.total,
            as_of: signed.as_of,
            signer: signed.signer.to_bytes(),
        }
    }
}

/// The program's `Billing`: with a signed total, its signer's records;
/// without, the registry's config in each place, as nothing is read.
fn billing(v: &VaultKeys, records: Option<&KeyRecords>) -> [AccountMeta; 8] {
    let config = address::registry_config();
    let records = records.copied().unwrap_or(KeyRecords {
        blessing: config,
        key_info: config,
        image: config,
        project: config,
    });
    [
        r(config),
        r(records.blessing),
        r(records.key_info),
        r(records.image),
        r(records.project),
        r(address::prices()),
        r(ids::INSTRUCTIONS),
        w(v.dollars(&v.platform)),
    ]
}

/// The Ed25519 instruction carrying a signed total for `vault` on the
/// cluster `cluster` stands for, which goes before the instruction it pays.
pub fn usage_signature(cluster: &[u8; 32], vault: &Pubkey, signed: &SignedUsage) -> Instruction {
    ed25519(
        &signed.signer,
        &signed.signature,
        &usage_digest(cluster, vault, signed.total, signed.as_of),
    )
}

// Depositors --------------------------------------------------------------------------

/// `payer` asks to deposit `assets` of its dollars for `receiver`, the
/// request its own account at `nonce` (see [`address::request`]). The
/// statement is the deposit itself: `statement` is the hash of the
/// restricted list the depositor states they are not on
/// ([`super::read::restricted_hash`]), recorded on the receiver's holding;
/// `None` only for a receiver whose holding already states the current
/// list. Signing the transaction is making the statement.
pub fn request_deposit(
    v: &VaultKeys,
    payer: &Pubkey,
    receiver: &Pubkey,
    assets: u64,
    statement: Option<[u8; 32]>,
    nonce: u64,
) -> Instruction {
    v.ix(
        vec![
            ws(*payer),
            w(v.vault),
            w(v.holder(receiver)),
            w(address::request(&v.vault, payer, nonce)),
            w(v.dollars(payer)),
            w(v.vault_dollars),
            r(v.mint),
            r(ids::TOKEN),
            r(ids::SYSTEM),
        ],
        data(
            d::REQUEST_DEPOSIT,
            &(assets, receiver.to_bytes(), statement, nonce),
        ),
    )
}

/// `owner` states, on its own holding, that it is not on the list
/// `statement` hashes, without depositing.
pub fn record_statement(v: &VaultKeys, owner: &Pubkey, statement: [u8; 32]) -> Instruction {
    v.ix(
        vec![ws(*owner), r(v.vault), w(v.holder(owner)), r(ids::SYSTEM)],
        data(d::RECORD_STATEMENT, &statement),
    )
}

/// `owner` asks to redeem `shares` for `receiver`, the request its own
/// account at `nonce`.
pub fn request_redeem(
    v: &VaultKeys,
    owner: &Pubkey,
    shares: u128,
    receiver: &Pubkey,
    nonce: u64,
) -> Instruction {
    v.ix(
        vec![
            ws(*owner),
            w(v.vault),
            w(v.holder(owner)),
            w(address::request(&v.vault, owner, nonce)),
            r(ids::SYSTEM),
        ],
        data(d::REQUEST_REDEEM, &(shares, receiver.to_bytes(), nonce)),
    )
}

/// `owner` books its redemption request `request_id` (its account at
/// `request`) as a claim instead of taking the pro-rata fallback.
pub fn claim_instead(
    v: &VaultKeys,
    owner: &Pubkey,
    request: &Pubkey,
    request_id: u64,
) -> Instruction {
    v.ix(
        vec![
            rs(*owner),
            w(v.vault),
            w(*request),
            w(address::claim(&v.vault, request_id)),
            r(ids::SYSTEM),
        ],
        data(d::CLAIM_INSTEAD, &request_id),
    )
}

/// A claim as its account holds it: its id, its owner, and the nonce of the
/// request it was last queued by (its `request` field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimKeys {
    pub id: u64,
    pub owner: Pubkey,
    pub request: u64,
}

impl ClaimKeys {
    fn address(&self, v: &VaultKeys) -> Pubkey {
        address::claim(&v.vault, self.id)
    }

    /// The request a queued claim is priced by: the claim's own, at its
    /// last nonce.
    fn queued(&self, v: &VaultKeys) -> Pubkey {
        address::request(&v.vault, &self.address(v), self.request)
    }
}

/// `caller` settles `claim`: the station closes what it can and the claim
/// is paid, or queued (the caller paying its request's rent). `groups` are
/// the station's driver groups.
pub fn settle_claim(
    v: &VaultKeys,
    caller: &Pubkey,
    claim: &ClaimKeys,
    groups: &Groups,
) -> Instruction {
    let mut accounts = vec![ws(*caller)];
    accounts.extend(v.pool());
    accounts.extend([
        r(ids::STATION),
        w(v.station_dollars),
        w(claim.address(v)),
        w(claim.queued(v)),
        w(v.dollars(&claim.owner)),
        w(v.dollars(&v.author)),
        w(v.dollars(&v.platform)),
    ]);
    accounts.extend(events());
    accounts.extend(groups.accounts.iter().cloned());
    v.ix(
        accounts,
        data(d::SETTLE_CLAIM, &(claim.id, groups.sizes.clone())),
    )
}

/// `owner` takes the pro-rata fallback for its request `request_id` (its
/// account at `request`) that nothing priced: a redemption's share of the
/// cash and units for the rest, or a deposit back. A deposit's asks the
/// station whether it could be valued now, through `groups`; a
/// redemption's needs [`Groups::none`].
pub fn take_pro_rata(
    v: &VaultKeys,
    owner: &Pubkey,
    request: &Pubkey,
    request_id: u64,
    groups: &Groups,
) -> Instruction {
    let mut accounts = vec![ws(*owner)];
    accounts.extend(v.pool());
    accounts.extend([
        r(ids::STATION),
        w(*request),
        w(v.holder(owner)),
        w(v.dollars(owner)),
        r(ids::SYSTEM),
    ]);
    accounts.extend(events());
    accounts.extend(groups.accounts.iter().cloned());
    v.ix(
        accounts,
        data(d::TAKE_PRO_RATA, &(request_id, groups.sizes.clone())),
    )
}

/// `claim`'s owner takes the pro-rata fallback for a claim nothing served;
/// `queued` says whether the claim is queued, when its request is named.
pub fn take_pro_rata_claim(v: &VaultKeys, claim: &ClaimKeys, queued: bool) -> Instruction {
    let mut accounts = vec![ws(claim.owner)];
    accounts.extend(v.pool());
    accounts.extend([
        w(claim.address(v)),
        if queued { w(claim.queued(v)) } else { none() },
        w(v.holder(&claim.owner)),
        w(v.dollars(&claim.owner)),
        r(ids::SYSTEM),
    ]);
    v.ix(accounts, data(d::TAKE_PRO_RATA_CLAIM, &claim.id))
}

/// `owner` takes what its served units brought home, into its dollar
/// account.
pub fn collect_units(v: &VaultKeys, owner: &Pubkey) -> Instruction {
    let mut accounts = vec![
        rs(*owner),
        w(v.vault),
        w(v.holder(owner)),
        w(v.dollars(owner)),
    ];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::COLLECT_UNITS, &()))
}

/// `owner` takes a payout held for it into its dollar account: one held on
/// a request (its account at `record`) or a claim, or a claim's fee held
/// for the author or the platform.
pub fn collect_held(
    v: &VaultKeys,
    owner: &Pubkey,
    held: Held,
    request: Option<Pubkey>,
) -> Instruction {
    let record = match held {
        Held::Claim(id) => w(address::claim(&v.vault, id)),
        Held::Request(_) => request.map_or_else(none, w),
        Held::Fees => none(),
    };
    let mut accounts = vec![rs(*owner), w(v.vault), record, w(v.dollars(owner))];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::COLLECT_HELD, &held))
}

// A closed vault's doors ------------------------------------------------------------------

/// `owner` redeems `shares` of a closed vault, paid to `receiver`.
pub fn redeem_closed(
    v: &VaultKeys,
    owner: &Pubkey,
    shares: u128,
    receiver: &Pubkey,
) -> Instruction {
    let mut accounts = vec![
        rs(*owner),
        w(v.vault),
        w(v.holder(owner)),
        r(v.station),
        w(v.dollars(receiver)),
    ];
    accounts.extend(v.purse());
    v.ix(
        accounts,
        data(d::REDEEM_CLOSED, &(shares, receiver.to_bytes())),
    )
}

/// A closed vault pays `claim`, still waiting when it closed, to its owner;
/// `queued` as for [`take_pro_rata_claim`]. Anyone may send it.
pub fn claim_closed(v: &VaultKeys, claim: &ClaimKeys, queued: bool) -> Instruction {
    let mut accounts = vec![
        w(v.vault),
        r(v.station),
        w(claim.address(v)),
        if queued { w(claim.queued(v)) } else { none() },
        w(v.dollars(&claim.owner)),
    ];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::CLAIM_CLOSED, &claim.id))
}

/// What a request was, as its account says: what a closed vault does with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Deposit,
    Redeem,
    Claim,
    TankFill,
}

/// A closed vault settles request `request_id` (its account at `request`),
/// still waiting when it closed: a deposit goes back to its `payer`, a
/// redemption is paid to its `owner`, anything else to the author. Anyone
/// may send it.
pub fn settle_closed(
    v: &VaultKeys,
    request: &Pubkey,
    request_id: u64,
    kind: RequestKind,
    owner: &Pubkey,
    payer: &Pubkey,
) -> Instruction {
    let (other, to) = match kind {
        RequestKind::Deposit => (w(v.holder(owner)), v.dollars(payer)),
        RequestKind::Redeem => (none(), v.dollars(owner)),
        RequestKind::Claim | RequestKind::TankFill => (none(), v.dollars(&v.author)),
    };
    let mut accounts = vec![w(v.vault), r(v.station), w(*request), other, w(to)];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::SETTLE_CLOSED, &request_id))
}

// Rent back ---------------------------------------------------------------------------------

/// Close request `request_id` (at `request`), finished, its rent going back
/// to `payer`, who paid it.
pub fn close_request(
    v: &VaultKeys,
    request: &Pubkey,
    request_id: u64,
    payer: &Pubkey,
) -> Instruction {
    v.ix(
        vec![r(v.vault), w(*request), w(*payer)],
        data(d::CLOSE_REQUEST, &request_id),
    )
}

/// Close claim `claim_id`, paid, its rent going back to `payer`.
pub fn close_claim(v: &VaultKeys, claim_id: u64, payer: &Pubkey) -> Instruction {
    v.ix(
        vec![r(v.vault), w(address::claim(&v.vault, claim_id)), w(*payer)],
        data(d::CLOSE_CLAIM, &claim_id),
    )
}

/// `owner` closes its empty holding, its rent going back to `payer`, who
/// paid it.
pub fn close_holder(v: &VaultKeys, owner: &Pubkey, payer: &Pubkey) -> Instruction {
    v.ix(
        vec![rs(*owner), r(v.vault), w(v.holder(owner)), w(*payer)],
        data(d::CLOSE_HOLDER, &()),
    )
}

// The tank ------------------------------------------------------------------------------------

/// `funder` puts `assets` of its own dollars into the tank.
pub fn fund_tank(v: &VaultKeys, funder: &Pubkey, assets: u64) -> Instruction {
    let mut accounts = vec![rs(*funder), w(v.vault), w(v.dollars(funder))];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::FUND_TANK, &assets))
}

/// `keep`: with a signed total, the bill collected from the tank (the
/// Ed25519 instruction carrying it goes first, for the cluster `cluster`
/// stands for); either way a fill of the tank queued if the author keeps it
/// above what it holds, `caller` paying the fill's request's rent.
/// `fill_request` is the vault's tank's `fill_request`.
pub fn keep(
    v: &VaultKeys,
    caller: &Pubkey,
    fill_request: u64,
    signed: Option<(&SignedUsage, &KeyRecords)>,
    cluster: &[u8; 32],
) -> Vec<Instruction> {
    let mut accounts = vec![
        ws(*caller),
        w(v.vault),
        w(address::request(&v.vault, &v.vault, fill_request)),
    ];
    accounts.extend(billing(v, signed.map(|(_, records)| records)));
    accounts.extend(v.purse());
    accounts.push(r(ids::SYSTEM));
    let usage: Option<Usage> = signed.map(|(signed, _)| signed.into());
    let keep = v.ix(accounts, data(d::KEEP, &usage));
    match signed {
        Some((signed, _)) => vec![usage_signature(cluster, &v.vault, signed), keep],
        None => vec![keep],
    }
}

/// The author withdraws `assets` from the tank into its dollar account,
/// with the executor's signed total collected first (the Ed25519
/// instruction carrying it goes first).
pub fn withdraw_tank(
    v: &VaultKeys,
    assets: u64,
    signed: &SignedUsage,
    records: &KeyRecords,
    cluster: &[u8; 32],
) -> Vec<Instruction> {
    let mut accounts = vec![rs(v.author), w(v.vault)];
    accounts.extend(billing(v, Some(records)));
    accounts.push(w(v.dollars(&v.author)));
    accounts.extend(v.purse());
    let usage: Usage = signed.into();
    vec![
        usage_signature(cluster, &v.vault, signed),
        v.ix(accounts, data(d::WITHDRAW_TANK, &(assets, usage))),
    ]
}

/// The author asks to withdraw from the tank without a signed total, for
/// when no executor runs: allowed after the vault's escape delay, within the
/// same length of time again.
pub fn request_tank_withdrawal(v: &VaultKeys) -> Instruction {
    author(v, data(d::REQUEST_TANK_WITHDRAWAL, &()))
}

/// The author withdraws `assets` from the tank into its dollar account
/// without a signed total, once the escape asked for is open.
pub fn withdraw_tank_unsigned(v: &VaultKeys, assets: u64) -> Instruction {
    let mut accounts = vec![rs(v.author), w(v.vault), w(v.dollars(&v.author))];
    accounts.extend(v.purse());
    v.ix(accounts, data(d::WITHDRAW_TANK_UNSIGNED, &assets))
}

/// The author redeems `shares` of its own into the tank, the request its own
/// account at `nonce`.
pub fn top_up_from_shares(v: &VaultKeys, shares: u128, nonce: u64) -> Instruction {
    v.ix(
        vec![
            ws(v.author),
            w(v.vault),
            w(address::request(&v.vault, &v.author, nonce)),
            r(ids::SYSTEM),
        ],
        data(d::TOP_UP_FROM_SHARES, &(shares, nonce)),
    )
}

/// The author keeps the tank at `assets`, filled from its fee shares.
pub fn set_tank_keep(v: &VaultKeys, assets: u64) -> Instruction {
    author(v, data(d::SET_TANK_KEEP, &assets))
}

// The author ------------------------------------------------------------------------------------

/// The author's own instructions on the vault (the program's `Author`).
fn author(v: &VaultKeys, data: Vec<u8>) -> Instruction {
    v.ix(vec![rs(v.author), w(v.vault)], data)
}

/// The author sets the agent's allowances, checked against the price list.
pub fn set_allowances(v: &VaultKeys, allowances: &Allowances) -> Instruction {
    v.ix(
        vec![rs(v.author), w(v.vault), r(address::prices())],
        data(d::SET_ALLOWANCES, allowances),
    )
}

/// The author sets the share of value kept idle, in basis points.
pub fn set_idle_floor(v: &VaultKeys, bps: u16) -> Instruction {
    author(v, data(d::SET_IDLE_FLOOR, &bps))
}

/// The author retires the vault: no deposits, and the strategy runs until
/// it is evicted.
pub fn retire(v: &VaultKeys) -> Instruction {
    author(v, data(d::RETIRE, &()))
}

// Activation ---------------------------------------------------------------------------------------

/// A driver a vault's station is created with: its registry name, the
/// implementation the registry approves now (its approval slot's effective
/// program; activation refuses any other) and the venue it trades at (its
/// allowlist's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Driver {
    pub name: [u8; 32],
    pub program: Pubkey,
    pub venue: Pubkey,
}

/// What an author activates a registered, reviewed strategy with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub strategy_id: u64,
    pub allowances: Allowances,
    pub idle_floor_bps: u16,
    /// Dollars from the author's account into the tank.
    pub tank_funding: u64,
    /// What the tank is kept at from the author's fee shares; 0 for none.
    pub tank_keep: u64,
    /// The restricted list's hash the author states, as a holder of fee
    /// shares ([`super::read::restricted_hash`]).
    pub statement: [u8; 32],
    /// The factory's terms' hash the author read
    /// ([`super::read::Factory::terms_hash`]).
    pub terms: [u8; 32],
    /// The station's drivers, in order.
    pub drivers: Vec<Driver>,
    /// The vault programs the factory replaces, newest first, until one
    /// that replaces none (the factory's `previous`, then each one's): each
    /// is checked to hold no vault for the strategy.
    pub previous: Vec<Pubkey>,
}

#[derive(BorshSerialize)]
struct ActivateArgs<'a> {
    strategy_id: u64,
    allowances: &'a Allowances,
    idle_floor_bps: u16,
    tank_funding: u64,
    tank_keep: u64,
    statement: [u8; 32],
    terms: [u8; 32],
    driver_accounts: Vec<u8>,
    previous: u8,
}

/// The author activates `activation.strategy_id`: its vault, its station and
/// every account they keep, in one instruction, paid for by the author.
/// `v` is the vault to be ([`VaultKeys::new`] with the factory's mint and
/// platform and the author).
pub fn activate(v: &VaultKeys, activation: &Activation) -> Instruction {
    let id = activation.strategy_id;
    let mut accounts = vec![
        ws(v.author),
        w(address::factory()),
        r(address::strategy(id)),
        r(address::review(id)),
        w(v.vault),
        w(address::restricted(&v.vault)),
        w(v.holder(&v.author)),
        w(v.holder(&v.platform)),
        w(v.vault_dollars),
        w(v.dollars(&v.author)),
        r(v.mint),
        r(ids::TOKEN),
        r(ids::ASSOCIATED_TOKEN),
        r(ids::SYSTEM),
        r(ids::STATION),
        r(address::station_config()),
        w(v.station),
        w(v.station_dollars),
    ];
    for program in &activation.previous {
        accounts.extend([
            r(address::factory_of(program)),
            r(address::vault_of(program, id)),
        ]);
    }
    let mut groups = Groups::none();
    for driver in &activation.drivers {
        let authority = address::venue_authority(&v.station, &driver.venue);
        groups.push(vec![
            r(driver.program),
            r(address::program_data(&driver.program)),
            r(address::driver_approval(&driver.name)),
            r(address::driver_revoked(&driver.name, &driver.program)),
            r(authority),
            w(v.dollars(&authority)),
        ]);
    }
    accounts.extend(groups.accounts);
    let args = ActivateArgs {
        strategy_id: id,
        allowances: &activation.allowances,
        idle_floor_bps: activation.idle_floor_bps,
        tank_funding: activation.tank_funding,
        tank_keep: activation.tank_keep,
        statement: activation.statement,
        terms: activation.terms,
        driver_accounts: groups.sizes,
        previous: activation.previous.len() as u8,
    };
    v.ix(accounts, data(d::ACTIVATE, &args))
}
