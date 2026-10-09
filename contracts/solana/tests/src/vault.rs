//! Vaults, as the development chain sets them up: the station program
//! configured with the vault program as the one whose vaults may create
//! stations, the factory initialised on the EVM fixture's terms, and helpers
//! that build every vault instruction the way an author, a depositor, the
//! keeper or a passer-by would send it.
//!
//! A [`V`] is one vault: its strategy, its address, and its station as a
//! [`Home`], so every station helper works on it too.

use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub use shonin_vault::error::VaultError;
pub use shonin_vault::state::{
    Claim, ClaimState, Fees, Holder, Request, RequestKind, Terms, Timers, Vault, VaultState,
    OFFSET, PRICE as PRICE_SCALE,
};
pub use shonin_vault::{HeldIn, Usage};

use crate::station::*;
use crate::*;
use shonin_registry::read as registry;
use shonin_vault::state as vs;

pub const MINIMUM_TANK: u64 = 40_000_000;
pub const DEAD_ASSETS: u64 = 10_000;
pub const MINIMUM_DEPOSIT: u64 = 10_000_000;
pub const SLOT: u64 = 10;
/// The most requests the helpers settle in one transaction: the most one
/// settlement takes.
pub const SETTLE_BATCH: usize = shonin_vault::state::MAX_SETTLE;

pub fn vault_error(error: VaultError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}

/// The EVM fixture's terms.
pub fn terms() -> Terms {
    Terms {
        fees: Fees {
            holding_bps: 100,
            author_bps: 1_000,
            platform_bps: 500,
            transaction_bps: 10,
        },
        exit_band_bps: EXIT_BAND,
        minimum_deposit: MINIMUM_DEPOSIT,
        minimum_tank: MINIMUM_TANK,
        dead_assets: DEAD_ASSETS,
        timers: Timers {
            slot_interval: SLOT,
            slot_lead: 5,
            slot_window: 3,
            fallback_after: DAY,
            usage_freshness: 10 * MINUTE,
            tank_escape_delay: DAY,
            floor_delay: DAY,
            halt_grace: HALT_GRACE,
            freeze_after: 7 * DAY,
            wind_down: 7 * DAY,
        },
    }
}

/// One vault: its strategy, its address and its station.
#[derive(Clone, Debug)]
pub struct V {
    pub id: u64,
    pub vault: Pubkey,
    pub bundle: [u8; 32],
    pub home: Home,
}

pub fn factory_address() -> Pubkey {
    vs::factory_address()
}

pub fn holder_address(vault: &Pubkey, owner: &Pubkey) -> Pubkey {
    vs::holder_address(vault, owner).0
}

/// Where `maker`'s request with `nonce` is: a signer's own, or for a request
/// the program makes, a claim's or the vault's.
pub fn request_address(vault: &Pubkey, maker: &Pubkey, nonce: u64) -> Pubkey {
    vs::request_address(vault, maker, nonce).0
}

pub fn claim_address(vault: &Pubkey, id: u64) -> Pubkey {
    vs::claim_address(vault, id).0
}

pub fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(owner, mint)
}

pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &shonin_vault::ID).0
}

impl Env {
    // Setting up ---------------------------------------------------------------

    /// The platform, the stand-in venues and their stations, and the vault
    /// factory on the fixture's terms.
    pub fn with_vaults() -> Self {
        let mut env = Self::new();
        env.set_up_stations_for(shonin_vault::ID);
        expect_ok(env.initialize_vaults(terms(), Pubkey::default()));
        env
    }

    /// The same, with one vault on the stand-in driver, activated by the
    /// author with a tank of [`MINIMUM_TANK`] and allowances costing $2 a
    /// day, and the executor blessed for it.
    pub fn with_vault() -> (Self, V) {
        let mut env = Self::with_vaults();
        let v = env.open(&[Declared::local()]);
        (env, v)
    }

    pub fn ix_initialize_vaults(
        &self,
        deployer: &Pubkey,
        terms: Terms,
        previous: Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Initialize {
                deployer: *deployer,
                factory: factory_address(),
                mint: self.mint,
                registry_config: registry::config_address(),
                program: shonin_vault::ID,
                program_data: program_data_address(&shonin_vault::ID),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Initialize {
                args: shonin_vault::InitializeArgs {
                    admin: self.admin.pubkey(),
                    platform: self.platform.pubkey(),
                    chain_name: name("solana"),
                    terms,
                    previous,
                },
            }
            .data(),
        }
    }

    pub fn initialize_vaults(&mut self, terms: Terms, previous: Pubkey) -> TxResult {
        let deployer = self.deployer.insecure_clone();
        let ix = self.ix_initialize_vaults(&deployer.pubkey(), terms, previous);
        self.send(vec![ix], &[&deployer])
    }

    pub fn factory(&self) -> shonin_vault::state::Factory {
        self.load(&factory_address())
            .expect("the factory is set up")
    }

    pub fn terms_hash(&self) -> [u8; 32] {
        shonin_vault::factory::terms_hash(&self.factory().terms)
    }

    // Strategies ---------------------------------------------------------------

    /// A strategy submitted, reviewed and built by the author, trading through
    /// the drivers named.
    pub fn ready_with(
        &mut self,
        drivers: &[[u8; 32]],
        restricted: Vec<[u8; 32]>,
    ) -> (u64, [u8; 32]) {
        let author = self.author.insecure_clone();
        let id = self.submit_as(&author);
        let bundle = keccak(&[b"bundle", &id.to_le_bytes()]);
        let mut registration = Registration::new(id, author.pubkey(), bundle);
        registration.lists.chains = vec![ChainNeed {
            chain: name("solana"),
            drivers: drivers.to_vec(),
        }];
        registration.lists.restricted = restricted;
        let (builder, reviewer) = (
            self.builder.insecure_clone(),
            self.reviewer.insecure_clone(),
        );
        expect_ok(self.register_with(&registration, &builder, &reviewer));
        expect_ok(self.submit_review(&reviewer, id, review_for(bundle, 1)));
        (id, bundle)
    }

    /// A driver's restricted list, asked as a vault asks it.
    pub fn driver_restricted(&mut self, program: &Pubkey) -> Vec<[u8; 32]> {
        let ix = Instruction {
            program_id: *program,
            accounts: vec![],
            data: shonin_driver::discriminator::RESTRICTED.to_vec(),
        };
        let meta = self.simulate(vec![ix]);
        Vec::<[u8; 32]>::try_from_slice(&meta.return_data.data).unwrap()
    }

    /// The list strategy `id`'s vault will refuse, and its hash.
    pub fn restricted_for(&mut self, id: u64, drivers: &[Declared]) -> (Vec<[u8; 32]>, [u8; 32]) {
        let record = self.strategy_of(id).expect("registered");
        let lists: Vec<Vec<[u8; 32]>> = drivers
            .iter()
            .map(|d| self.driver_restricted(&d.program))
            .collect();
        let list =
            shonin_vault::codes::merged(&record.lists.restricted, lists.iter().map(Vec::as_slice));
        let hash = shonin_vault::codes::hash(&list);
        (list, hash)
    }

    /// Where strategy `id`'s vault and its station are.
    pub fn v_of(&self, id: u64, bundle: [u8; 32], drivers: &[Pubkey]) -> V {
        let vault = vs::vault_address(id).0;
        let station = shonin_station::station_address(&vault);
        V {
            id,
            vault,
            bundle,
            home: Home {
                label: name("vault"),
                vault,
                vault_dollars: ata(&vault, &self.mint),
                station,
                dollars: ata(&station, &self.mint),
                mint: self.mint,
                bundle,
                drivers: drivers.to_vec(),
                program: shonin_vault::ID,
            },
        }
    }

    /// The author's `activate` of strategy `id`.
    #[allow(clippy::too_many_arguments)]
    pub fn ix_activate(
        &self,
        id: u64,
        bundle: [u8; 32],
        drivers: &[Declared],
        tank: u64,
        statement: [u8; 32],
        terms: [u8; 32],
        previous: &[(Pubkey, Pubkey)],
    ) -> Instruction {
        let programs: Vec<Pubkey> = drivers.iter().map(|d| d.program).collect();
        let v = self.v_of(id, bundle, &programs);
        let author = self.author.pubkey();
        let mut accounts = shonin_vault::accounts::Activate {
            author,
            factory: factory_address(),
            strategy: strategy_address(id),
            review: review_address(id),
            vault: v.vault,
            restricted: vs::restricted_address(&v.vault).0,
            author_holder: holder_address(&v.vault, &author),
            platform_holder: holder_address(&v.vault, &self.platform.pubkey()),
            vault_dollars: v.home.vault_dollars,
            author_dollars: ata(&author, &self.mint),
            mint: self.mint,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: solana_sdk_ids::system_program::ID,
            station_program: shonin_station::ID,
            station_config: shonin_station::config_address(),
            station: v.home.station,
            station_dollars: v.home.dollars,
        }
        .to_account_metas(None);
        for (config, vault) in previous {
            accounts.push(AccountMeta::new_readonly(*config, false));
            accounts.push(AccountMeta::new_readonly(*vault, false));
        }
        let create = self.ix_create(&v.home, &author, terms_band(), drivers);
        // The station's own accounts come first in its `create`; the driver
        // groups follow.
        let fixed = shonin_station::accounts::Create {
            payer: author,
            vault: v.vault,
            vault_dollars: v.home.vault_dollars,
            config: shonin_station::config_address(),
            station: v.home.station,
            station_dollars: v.home.dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None)
        .len();
        let groups: Vec<AccountMeta> = create.accounts[fixed..].to_vec();
        let mut sizes = Vec::new();
        let mut at = 0;
        for d in drivers {
            let size = if d.program == shonin_test_driver::ID {
                7
            } else {
                6
            };
            sizes.push(size as u8);
            at += size;
        }
        assert_eq!(at, groups.len());
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::Activate {
                args: shonin_vault::ActivateArgs {
                    strategy_id: id,
                    allowances: allowances_costing(2_000_000),
                    idle_floor_bps: 500,
                    tank_funding: tank,
                    tank_keep: 0,
                    statement,
                    terms,
                    driver_accounts: sizes,
                    previous: previous.len() as u8,
                },
            }
            .data(),
        }
    }

    /// The author activates strategy `id` on the terms in force, stating for
    /// the list as it will be.
    pub fn activate_vault(
        &mut self,
        id: u64,
        bundle: [u8; 32],
        drivers: &[Declared],
        tank: u64,
    ) -> TxResult {
        let (_, statement) = self.restricted_for(id, drivers);
        let terms = self.terms_hash();
        let author = self.author.insecure_clone();
        self.give(&author.pubkey(), tank);
        let ix = self.ix_activate(id, bundle, drivers, tank, statement, terms, &[]);
        self.send_fitting(vec![ix], &[&author])
    }

    /// A strategy made ready and activated through `drivers`, its executor
    /// blessed.
    pub fn open(&mut self, drivers: &[Declared]) -> V {
        let names: Vec<[u8; 32]> = drivers.iter().map(|d| d.name).collect();
        let (id, bundle) = self.ready_with(&names, vec![name("SANCTIONED"), name("US")]);
        expect_ok(self.activate_vault(id, bundle, drivers, MINIMUM_TANK));
        let programs: Vec<Pubkey> = drivers.iter().map(|d| d.program).collect();
        let v = self.v_of(id, bundle, &programs);
        let executor = self.executor.pubkey();
        self.bless_executor_key(executor, v.vault.to_bytes(), bundle);
        v
    }

    // Reading ------------------------------------------------------------------

    pub fn vault(&self, v: &V) -> Vault {
        self.load(&v.vault).expect("the vault exists")
    }

    pub fn holder(&self, v: &V, owner: &Pubkey) -> Option<Holder> {
        self.load(&holder_address(&v.vault, owner))
    }

    pub fn shares(&self, v: &V, owner: &Pubkey) -> u128 {
        self.holder(v, owner).map_or(0, |h| h.shares)
    }

    /// The vault's request `id`, wherever its maker put it, found as a
    /// client finds it: among the program's accounts, by the vault and the
    /// id it holds.
    pub fn request(&self, v: &V, id: u64) -> Option<Request> {
        self.find_request(&v.vault, id).map(|(_, r)| r)
    }

    fn find_request(&self, vault: &Pubkey, id: u64) -> Option<(Pubkey, Request)> {
        use anchor_lang::Discriminator;
        use solana_account::ReadableAccount;
        let found = self
            .svm
            .accounts_db()
            .inner
            .iter()
            .find_map(|(key, account)| {
                let data = account.data();
                if account.owner() != &shonin_vault::ID || !data.starts_with(Request::DISCRIMINATOR)
                {
                    return None;
                }
                let r = Request::try_deserialize(&mut &data[..]).ok()?;
                (r.vault == *vault && r.id == id).then_some((*key, r))
            });
        if let Some((key, _)) = &found {
            self.requests.borrow_mut().insert((*vault, id), *key);
        }
        found
    }

    /// Where the vault's request `id` is, or was before it was closed.
    pub fn request_key(&self, v: &V, id: u64) -> Pubkey {
        if let Some((key, _)) = self.find_request(&v.vault, id) {
            return key;
        }
        *self
            .requests
            .borrow()
            .get(&(v.vault, id))
            .unwrap_or_else(|| panic!("request {id} was never seen"))
    }

    /// A nonce no request a helper built was made at.
    pub fn next_nonce(&self) -> u64 {
        let nonce = self.nonces.get() + 1;
        self.nonces.set(nonce);
        nonce
    }

    /// When a request was due to be priced; its record may be closed since,
    /// so this is read from the claim it became where need be.
    pub fn request_earliest_of(&self, v: &V, id: u64) -> u64 {
        self.request(v, id)
            .map(|r| r.earliest)
            .or_else(|| self.claim(v, id).map(|c| c.booked_at))
            .unwrap_or(0)
    }

    pub fn claim(&self, v: &V, id: u64) -> Option<Claim> {
        self.load(&claim_address(&v.vault, id))
    }

    pub fn restricted_list(&self, v: &V) -> Vec<[u8; 32]> {
        self.load::<vs::Restricted>(&vs::restricted_address(&v.vault).0)
            .expect("the list exists")
            .codes
    }

    /// The price of a share at the last valuation, after fees, in dollar
    /// units per 1e18 share units.
    pub fn share_price(&self, v: &V) -> u128 {
        let vault = self.vault(v);
        if !vault.priced {
            return 1_000_000;
        }
        let l = vault.latest;
        (l.gross - l.fee) * 1_000_000_000_000_000_000 / l.supply
    }

    // Time ----------------------------------------------------------------------

    /// To the slot boundary every request made so far is priced at.
    pub fn to_next_slot(&mut self) {
        let vault_earliest = self.now() + 5;
        let t = vault_earliest.div_ceil(SLOT) * SLOT;
        self.warp(t);
    }

    /// To the next slot boundary at or after now.
    pub fn to_boundary(&mut self) {
        let t = self.now().div_ceil(SLOT) * SLOT;
        self.warp(t);
    }

    // Who may deposit ------------------------------------------------------------

    pub fn ix_record_statement(&self, v: &V, owner: &Pubkey, statement: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RecordStatement {
                owner: *owner,
                vault: v.vault,
                holder: holder_address(&v.vault, owner),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::RecordStatement { statement }.data(),
        }
    }

    // Requests ------------------------------------------------------------------------

    pub fn ix_request_deposit(
        &self,
        v: &V,
        payer: &Pubkey,
        receiver: &Pubkey,
        assets: u64,
        statement: Option<[u8; 32]>,
    ) -> Instruction {
        let nonce = self.next_nonce();
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RequestDeposit {
                payer: *payer,
                vault: v.vault,
                holder: holder_address(&v.vault, receiver),
                request: request_address(&v.vault, payer, nonce),
                payer_dollars: ata(payer, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::RequestDeposit {
                assets,
                receiver: *receiver,
                statement,
                nonce,
            }
            .data(),
        }
    }

    /// `who` deposits `assets` for themselves, stating for the list as it
    /// is; answers with the request's id.
    pub fn deposit(&mut self, v: &V, who: &Keypair, assets: u64) -> u64 {
        self.give(&who.pubkey(), assets);
        let statement = self.vault(v).restricted_hash;
        let ix = self.ix_request_deposit(v, &who.pubkey(), &who.pubkey(), assets, Some(statement));
        expect_ok(self.send(vec![ix], &[who]));
        self.vault(v).request_count
    }

    pub fn ix_request_redeem(
        &self,
        v: &V,
        owner: &Pubkey,
        shares: u128,
        receiver: &Pubkey,
    ) -> Instruction {
        let nonce = self.next_nonce();
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RequestRedeem {
                owner: *owner,
                vault: v.vault,
                holder: holder_address(&v.vault, owner),
                request: request_address(&v.vault, owner, nonce),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::RequestRedeem {
                shares,
                receiver: *receiver,
                nonce,
            }
            .data(),
        }
    }

    /// `who` redeems `shares` to themselves; answers with the request's id.
    pub fn redeem(&mut self, v: &V, who: &Keypair, shares: u128) -> u64 {
        let ix = self.ix_request_redeem(v, &who.pubkey(), shares, &who.pubkey());
        expect_ok(self.send(vec![ix], &[who]));
        self.vault(v).request_count
    }

    // Valuations ------------------------------------------------------------------------

    pub fn pool(&self, v: &V) -> shonin_vault::accounts::Pool {
        let vault = self.vault(v);
        shonin_vault::accounts::Pool {
            vault: v.vault,
            station: v.home.station,
            author_holder: holder_address(&v.vault, &vault.author),
            platform_holder: holder_address(&v.vault, &vault.platform),
            vault_dollars: v.home.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            system_program: solana_sdk_ids::system_program::ID,
        }
    }

    pub fn ix_record_valuation(&self, v: &V) -> Instruction {
        let (groups, sizes) = self.groups(&v.home, &v.home.drivers);
        let mut accounts = shonin_vault::accounts::RecordValuation {
            pool: self.pool(v),
            station_program: shonin_station::ID,
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::RecordValuation { groups: sizes }.data(),
        }
    }

    /// Anyone records this slot's valuation.
    pub fn value_vault(&mut self, v: &V) -> TxResult {
        let ix = self.ix_record_valuation(v);
        let payer = self.payer.insecure_clone();
        self.send_fitting(vec![ix], &[&payer])
    }

    /// One request's accounts in a settlement, as its state now says: a
    /// request that is gone became a claim, which stands in for it.
    pub fn entry(&self, v: &V, id: u64) -> Vec<AccountMeta> {
        let Some((request, r)) = self.find_request(&v.vault, id) else {
            return vec![AccountMeta::new(claim_address(&v.vault, id), false)];
        };
        let mut out = vec![AccountMeta::new(request, false)];
        if r.done {
            return out;
        }
        match r.kind {
            RequestKind::Deposit => {
                out.push(AccountMeta::new(holder_address(&v.vault, &r.owner), false));
                out.push(AccountMeta::new(ata(&r.payer, &self.mint), false));
            }
            RequestKind::Redeem => {
                out.push(AccountMeta::new(claim_address(&v.vault, id), false));
                out.push(AccountMeta::new(ata(&r.owner, &self.mint), false));
            }
            RequestKind::Claim => {
                out.push(AccountMeta::new(
                    claim_address(&v.vault, r.reference),
                    false,
                ));
                out.push(AccountMeta::new(ata(&r.owner, &self.mint), false));
            }
            RequestKind::TankFill => {}
        }
        out
    }

    /// A settlement of up to `count` requests from `from` (an index: the
    /// cursor, or the exits' cursor).
    pub fn ix_settle(&self, v: &V, from: u64, count: usize) -> Instruction {
        let vault = self.vault(v);
        let mut accounts = shonin_vault::accounts::Settle {
            pool: self.pool(v),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        let last = vault.request_count.min(from + count as u64);
        for id in from + 1..=last {
            accounts.extend(self.entry(v, id));
        }
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::Settle { from }.data(),
        }
    }

    /// Whether settlement has nothing more to do for request `id` than pass
    /// it: it is done (taken through the fallback, say), or it became a claim,
    /// which stands in for it.
    pub fn passable(&self, v: &V, id: u64) -> bool {
        match self.request(v, id) {
            Some(r) => r.done,
            None => self.claim(v, id).is_some(),
        }
    }

    /// Where a keeper's next settlement starts, or `None` if there is nothing
    /// for one to do: the last priced valuation is owed nothing, and the
    /// request at the cursor is not one the cursor can pass. The exits go
    /// first, from the exits' cursor once they are a whole batch ahead of the
    /// cursor; the deposits after, from the cursor, which also passes
    /// requests already done.
    pub fn settle_from(&self, v: &V) -> Option<u64> {
        let vault = self.vault(v);
        if !vault.priced {
            return None;
        }
        let cursor = vault.request_cursor;
        let through = vault.priced_through;
        let done_next = cursor < vault.request_count && self.passable(v, cursor + 1);
        if cursor >= through && !done_next {
            return None;
        }
        let exits = vault.exit_cursor.max(cursor);
        if exits < through && exits >= cursor + SETTLE_BATCH as u64 {
            return Some(exits);
        }
        Some(cursor)
    }

    /// One settlement as a keeper sends it: a batch of up to `SETTLE_BATCH`
    /// requests from where `settle_from` says, each with its accounts as
    /// `entry` gives them (a claim in place of a request that became one).
    /// Answers whether there was anything to settle.
    pub fn settle_batch(&mut self, v: &V) -> bool {
        let Some(from) = self.settle_from(v) else {
            return false;
        };
        let before = self.vault(v);
        let ix = self.ix_settle(v, from, SETTLE_BATCH);
        let payer = self.payer.insecure_clone();
        expect_ok(self.send_fitting(vec![ix], &[&payer]));
        let after = self.vault(v);
        if after.request_cursor == before.request_cursor && after.exit_cursor == before.exit_cursor
        {
            panic!("settlement made no progress");
        }
        true
    }

    /// Settles everything the last priced valuation owes, a batch at a time,
    /// as a keeper must.
    pub fn settle_all(&mut self, v: &V) {
        for _ in 0..1_000 {
            if !self.settle_batch(v) {
                return;
            }
        }
        panic!("settlement never finished");
    }

    /// Records the valuation of the slot every request so far is priced at,
    /// and settles it.
    pub fn price_and_settle(&mut self, v: &V) {
        self.to_next_slot();
        expect_ok(self.value_vault(v));
        self.settle_all(v);
    }

    /// To the next slot boundary, a valuation recorded there and everything
    /// it prices settled, as the EVM fixture's `value` does.
    pub fn price(&mut self, v: &V) {
        self.to_next_slot();
        expect_ok(self.value_vault(v));
        self.settle_all(v);
    }

    /// `who` deposits `assets` and has it priced; answers with the shares it
    /// bought.
    pub fn deposit_as(&mut self, v: &V, who: &Keypair, assets: u64) -> u128 {
        let before = self.shares(v, &who.pubkey());
        self.deposit(v, who, assets);
        self.price(v);
        self.shares(v, &who.pubkey()) - before
    }

    /// `who` redeems `shares` and has it priced; answers with what it paid.
    pub fn redeem_as(&mut self, v: &V, who: &Keypair, shares: u128) -> u64 {
        let before = self.usd_balance(&who.pubkey());
        self.redeem(v, who, shares);
        self.price(v);
        self.usd_balance(&who.pubkey()) - before
    }

    /// Spot bought at the venue with `amount` of the vault's cash, through
    /// the station, at the venue's price.
    pub fn deploy_spot(&mut self, v: &V, amount: u64) {
        let size = amount as u128 * UNIT / PRICE as u128;
        self.trade(v, amount, vec![order(spot(), true, size, PRICE)]);
    }

    /// The spot market's price, with `spread_bps` between its bid and ask,
    /// the venue's own price at the mark.
    pub fn set_spot(&mut self, price: u64, spread_bps: u64) {
        let half = price * spread_bps / 20_000;
        self.set_prices(spot(), price, price, price - half, price + half);
    }

    /// The stand-in venue's reserves, as a real venue's other traders would
    /// be, so a gain can always be withdrawn.
    pub fn venue_reserves(&mut self, amount: u64) {
        let authority = self.mint_authority.insecure_clone();
        let ix = spl_token::instruction::mint_to(
            &spl_token::ID,
            &self.mint,
            &shonin_local_venue::vault_address(),
            &authority.pubkey(),
            &[],
            amount,
        )
        .unwrap();
        expect_ok(self.send(vec![ix], &[&authority]));
    }

    /// The spot market's mark at the stand-in venue.
    pub fn spot_price(&self) -> u64 {
        self.load::<shonin_local_venue::Market>(&shonin_local_venue::market_address(&spot()))
            .expect("the market is listed")
            .mark
    }

    /// The station's spot position at the stand-in venue gains `dollars`
    /// (loses, if negative): its price moves, its bid and ask with it at a
    /// hundredth of a percent either side.
    pub fn gain(&mut self, v: &V, dollars: i128) {
        let size = self.position(&v.home.authority(0), spot());
        assert!(size > 0, "the station holds no spot");
        let price = self.spot_price() as i128 + dollars * UNIT as i128 / size;
        self.set_spot(price as u64, 2);
    }

    // Claims -----------------------------------------------------------------------------

    pub fn ix_settle_claim(&self, v: &V, caller: &Pubkey, claim_id: u64) -> Instruction {
        let vault = self.vault(v);
        let claim = self.claim(v, claim_id);
        let owner = claim.as_ref().map_or(Pubkey::default(), |c| c.owner);
        let (groups, sizes) = self.groups(&v.home, &v.home.drivers);
        let mut accounts = shonin_vault::accounts::SettleClaim {
            caller: *caller,
            pool: self.pool(v),
            close: shonin_vault::accounts::Closing {
                station_program: shonin_station::ID,
                station_dollars: v.home.dollars,
            },
            claim: claim_address(&v.vault, claim_id),
            request: Some(request_address(
                &v.vault,
                &claim_address(&v.vault, claim_id),
                claim.as_ref().map_or(0, |c| c.request),
            )),
            owner_dollars: ata(&owner, &self.mint),
            author_dollars: ata(&vault.author, &self.mint),
            platform_dollars: ata(&vault.platform, &self.mint),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::SettleClaim {
                claim_id,
                groups: sizes,
            }
            .data(),
        }
    }

    /// A try serving only `shares` of a claim's shares.
    pub fn settle_claim_part(&mut self, v: &V, claim_id: u64, shares: u128) -> TxResult {
        let payer = self.payer.insecure_clone();
        let mut ix = self.ix_settle_claim(v, &payer.pubkey(), claim_id);
        let (_, sizes) = self.groups(&v.home, &v.home.drivers);
        ix.data = shonin_vault::instruction::SettleClaimPart {
            claim_id,
            groups: sizes,
            shares,
        }
        .data();
        self.send_fitting(vec![ix], &[&payer])
    }

    pub fn settle_claim(&mut self, v: &V, claim_id: u64) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_settle_claim(v, &payer.pubkey(), claim_id);
        self.send_fitting(vec![ix], &[&payer])
    }

    pub fn ix_claim_instead(&self, v: &V, owner: &Pubkey, request_id: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::ClaimInstead {
                owner: *owner,
                vault: v.vault,
                request: self.request_key(v, request_id),
                claim: claim_address(&v.vault, request_id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::ClaimInstead { request_id }.data(),
        }
    }

    pub fn ix_freeze(&self, v: &V, claim_id: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Freeze {
                vault: v.vault,
                claim: claim_address(&v.vault, claim_id),
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Freeze { claim_id }.data(),
        }
    }

    pub fn freeze(&mut self, v: &V, claim_id: u64) -> TxResult {
        let ix = self.ix_freeze(v, claim_id);
        let payer = self.payer.insecure_clone();
        self.send(vec![ix], &[&payer])
    }

    pub fn ix_freeze_units(&self, v: &V, holder: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::FreezeUnits {
                vault: v.vault,
                holder: holder_address(&v.vault, holder),
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::FreezeUnits {}.data(),
        }
    }

    // The pro-rata fallback -----------------------------------------------------------------

    pub fn ix_take_pro_rata(&self, v: &V, owner: &Pubkey, request_id: u64) -> Instruction {
        let r = self.request(v, request_id);
        let to = match r {
            Some(r) if r.kind == RequestKind::Deposit => ata(&r.payer, &self.mint),
            _ => ata(owner, &self.mint),
        };
        let (groups, sizes) = self.groups(&v.home, &v.home.drivers);
        let mut accounts = shonin_vault::accounts::TakeProRata {
            owner: *owner,
            pool: self.pool(v),
            station_program: shonin_station::ID,
            request: self.request_key(v, request_id),
            holder: holder_address(&v.vault, owner),
            to_dollars: to,
            system_program: solana_sdk_ids::system_program::ID,
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::TakeProRata {
                request_id,
                groups: sizes,
            }
            .data(),
        }
    }

    pub fn take_pro_rata(&mut self, v: &V, owner: &Keypair, request_id: u64) -> TxResult {
        let ix = self.ix_take_pro_rata(v, &owner.pubkey(), request_id);
        self.send_fitting(vec![ix], &[owner])
    }

    pub fn ix_take_pro_rata_claim(&self, v: &V, owner: &Pubkey, claim_id: u64) -> Instruction {
        let request = self
            .claim(v, claim_id)
            .filter(|c| c.request != 0)
            .map(|c| self.request_key(v, c.request));
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::TakeProRataClaim {
                owner: *owner,
                pool: self.pool(v),
                claim: claim_address(&v.vault, claim_id),
                request,
                holder: holder_address(&v.vault, owner),
                to_dollars: ata(owner, &self.mint),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::TakeProRataClaim { claim_id }.data(),
        }
    }

    pub fn ix_serve(&self, v: &V, caller: &Pubkey, holder: &Pubkey) -> Instruction {
        let (groups, sizes) = self.groups(&v.home, &v.home.drivers);
        let mut accounts = shonin_vault::accounts::ServeProRata {
            caller: *caller,
            pool: self.pool(v),
            close: shonin_vault::accounts::Closing {
                station_program: shonin_station::ID,
                station_dollars: v.home.dollars,
            },
            holder: holder_address(&v.vault, holder),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::ServeProRata {
                owner: *holder,
                groups: sizes,
            }
            .data(),
        }
    }

    pub fn serve(&mut self, v: &V, holder: &Pubkey) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_serve(v, &payer.pubkey(), holder);
        self.send_fitting(vec![ix], &[&payer])
    }

    pub fn ix_collect_units(&self, v: &V, owner: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CollectUnits {
                owner: *owner,
                vault: v.vault,
                holder: holder_address(&v.vault, owner),
                to: ata(owner, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CollectUnits {}.data(),
        }
    }

    // The closed vault ------------------------------------------------------------------

    pub fn ix_redeem_closed(
        &self,
        v: &V,
        owner: &Pubkey,
        shares: u128,
        receiver: &Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RedeemClosed {
                owner: *owner,
                vault: v.vault,
                holder: holder_address(&v.vault, owner),
                station: v.home.station,
                to: ata(receiver, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::RedeemClosed {
                shares,
                receiver: *receiver,
            }
            .data(),
        }
    }

    pub fn ix_claim_closed(&self, v: &V, claim_id: u64) -> Instruction {
        let claim = self.claim(v, claim_id).expect("a claim");
        let other = (claim.state == ClaimState::Queued).then(|| self.request_key(v, claim.request));
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.home.station,
                record: claim_address(&v.vault, claim_id),
                other,
                to: ata(&claim.owner, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::ClaimClosed { claim_id }.data(),
        }
    }

    pub fn ix_settle_closed(&self, v: &V, request_id: u64) -> Instruction {
        let r = self.request(v, request_id).expect("a request");
        let (other, to) = match r.kind {
            RequestKind::Deposit => (
                Some(holder_address(&v.vault, &r.owner)),
                ata(&r.payer, &self.mint),
            ),
            _ => (None, ata(&r.owner, &self.mint)),
        };
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::ClosedDoor {
                vault: v.vault,
                station: v.home.station,
                record: self.request_key(v, request_id),
                other,
                to,
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::SettleClosed { request_id }.data(),
        }
    }

    pub fn ix_collect_held(&self, v: &V, owner: &Pubkey, held: HeldIn) -> Instruction {
        let record = match held {
            HeldIn::Request(id) => Some(self.request_key(v, id)),
            HeldIn::Claim(id) => Some(claim_address(&v.vault, id)),
            HeldIn::Fees => None,
        };
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CollectHeld {
                owner: *owner,
                vault: v.vault,
                record,
                to: ata(owner, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CollectHeld { held }.data(),
        }
    }

    pub fn ix_close_request(&self, v: &V, request_id: u64) -> Instruction {
        let payer = self
            .request(v, request_id)
            .map_or(Pubkey::default(), |r| r.payer);
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseRecord {
                vault: v.vault,
                record: self.request_key(v, request_id),
                payer,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CloseRequest { request_id }.data(),
        }
    }

    pub fn ix_close_claim(&self, v: &V, claim_id: u64) -> Instruction {
        let payer = self
            .claim(v, claim_id)
            .map_or(Pubkey::default(), |c| c.payer);
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseRecord {
                vault: v.vault,
                record: claim_address(&v.vault, claim_id),
                payer,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::CloseClaim { claim_id }.data(),
        }
    }

    // The tank -------------------------------------------------------------------------------

    pub fn ix_fund_tank(&self, v: &V, funder: &Pubkey, assets: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::FundTank {
                funder: *funder,
                vault: v.vault,
                from: ata(funder, &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::FundTank { assets }.data(),
        }
    }

    pub fn billing(&self, v: &V, signer: &Pubkey) -> shonin_vault::accounts::Billing {
        let [config, blessing, key_info, image, project] = self.key_records(&v.home, signer);
        shonin_vault::accounts::Billing {
            registry_config: config,
            blessing,
            key_info,
            image,
            project,
            prices: shonin_prices::read::prices_address(),
            instructions: anchor_lang::solana_program::sysvar::instructions::ID,
            platform_dollars: ata(&self.platform.pubkey(), &self.mint),
        }
    }

    /// What the vault's executor signs for a usage total.
    pub fn usage_digest(&self, v: &V, total: u64, as_of: u64) -> [u8; 32] {
        shonin_vault::upkeep::usage_digest(&self.cluster(), &v.vault, total, as_of)
    }

    /// `keep`, with a usage total signed by `signer` when one is given.
    pub fn ix_keep(&self, v: &V, caller: &Pubkey, usage: Option<Usage>) -> Instruction {
        let signer = usage.map_or(self.executor.pubkey(), |u| u.signer);
        let nonce = self.vault(v).tank.fill_request;
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Keep {
                caller: *caller,
                vault: v.vault,
                request: Some(request_address(&v.vault, &v.vault, nonce)),
                billing: self.billing(v, &signer),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Keep { usage }.data(),
        }
    }

    /// Anyone keeps the vault with the executor's signed total.
    pub fn keep_signed(&mut self, v: &V, total: u64, as_of: u64) -> TxResult {
        let executor = self.executor.insecure_clone();
        self.keep_signed_by(v, &executor, total, as_of)
    }

    pub fn keep_signed_by(&mut self, v: &V, signer: &Keypair, total: u64, as_of: u64) -> TxResult {
        let digest = self.usage_digest(v, total, as_of);
        let usage = Usage {
            total,
            as_of,
            signer: signer.pubkey(),
        };
        let payer = self.payer.insecure_clone();
        let ixs = vec![
            Self::ed25519(signer, &digest),
            self.ix_keep(v, &payer.pubkey(), Some(usage)),
        ];
        self.send(ixs, &[&payer])
    }

    pub fn keep_unsigned(&mut self, v: &V) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_keep(v, &payer.pubkey(), None);
        self.send(vec![ix], &[&payer])
    }

    pub fn ix_withdraw_tank(&self, v: &V, assets: u64, usage: Usage) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::WithdrawTank {
                author: self.author.pubkey(),
                vault: v.vault,
                billing: self.billing(v, &usage.signer),
                to: ata(&self.author.pubkey(), &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::WithdrawTank { assets, usage }.data(),
        }
    }

    pub fn ix_author(&self, v: &V, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Author {
                author: self.author.pubkey(),
                vault: v.vault,
            }
            .to_account_metas(None),
            data,
        }
    }

    pub fn as_author(&mut self, ix: Instruction) -> TxResult {
        let author = self.author.insecure_clone();
        self.send(vec![ix], &[&author])
    }

    pub fn ix_withdraw_tank_unsigned(&self, v: &V, assets: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::WithdrawTankUnsigned {
                author: self.author.pubkey(),
                vault: v.vault,
                to: ata(&self.author.pubkey(), &self.mint),
                vault_dollars: v.home.vault_dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::WithdrawTankUnsigned { assets }.data(),
        }
    }

    pub fn ix_top_up_from_shares(&self, v: &V, shares: u128) -> Instruction {
        let nonce = self.next_nonce();
        let author = self.author.pubkey();
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::TopUpFromShares {
                author,
                vault: v.vault,
                request: request_address(&v.vault, &author, nonce),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::TopUpFromShares { shares, nonce }.data(),
        }
    }

    pub fn ix_set_allowances(&self, v: &V, allowances: Allowances) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::SetAllowances {
                author: self.author.pubkey(),
                vault: v.vault,
                prices: shonin_prices::read::prices_address(),
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::SetAllowances { allowances }.data(),
        }
    }

    // The lifecycle ---------------------------------------------------------------------------

    pub fn ix_admin(&self, v: &V, admin: &Pubkey, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Admin {
                admin: *admin,
                vault: v.vault,
            }
            .to_account_metas(None),
            data,
        }
    }

    pub fn kill(&mut self, v: &V) -> TxResult {
        let admin = self.admin.insecure_clone();
        let ix = self.ix_admin(
            v,
            &admin.pubkey(),
            shonin_vault::instruction::Kill {}.data(),
        );
        self.send(vec![ix], &[&admin])
    }

    pub fn ix_anyone(&self, v: &V, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Anyone { vault: v.vault }.to_account_metas(None),
            data,
        }
    }

    pub fn ix_close_vault(&self, v: &V) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseVault { pool: self.pool(v) }
                .to_account_metas(None),
            data: shonin_vault::instruction::Close {}.data(),
        }
    }

    pub fn close_vault(&mut self, v: &V) -> TxResult {
        let ix = self.ix_close_vault(v);
        let payer = self.payer.insecure_clone();
        self.send(vec![ix], &[&payer])
    }

    /// The station stopped once the halt's grace has passed, unwound in full
    /// by a passer-by, the vault valued and settled, and closed: what the
    /// keeper sends.
    pub fn wind_up(&mut self, v: &V) {
        let halted = self.vault(v).halted_at;
        if self.now() < halted + HALT_GRACE {
            self.warp(halted + HALT_GRACE);
        }
        if !self.station(&v.home).stopped {
            expect_ok(self.stop(&v.home));
        }
        self.settle_all(v);
        let payer = self.payer.insecure_clone();
        let unwind = self.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
        expect_ok(self.send_fitting(vec![unwind], &[&payer]));
        self.to_next_slot();
        expect_ok(self.value_vault(v));
        self.settle_all(v);
        expect_ok(self.close_vault(v));
    }

    // Moving money --------------------------------------------------------------------------

    pub fn ix_move(&self, v: &V, key: &Pubkey, amount: u64, to: bool) -> Instruction {
        let [config, blessing, key_info, image, project] = self.key_records(&v.home, key);
        let accounts = shonin_vault::accounts::Move {
            key: *key,
            vault: v.vault,
            station: v.home.station,
            station_program: shonin_station::ID,
            station_dollars: v.home.dollars,
            vault_dollars: v.home.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            registry_config: config,
            blessing,
            key_info,
            image,
            project,
        }
        .to_account_metas(None);
        let data = if to {
            shonin_vault::instruction::ToStation { amount }.data()
        } else {
            shonin_vault::instruction::FromStation { amount }.data()
        };
        Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data,
        }
    }

    /// The approved executor sends `amount` of the vault's cash to the
    /// station.
    pub fn to_station(&mut self, v: &V, amount: u64) -> TxResult {
        let executor = self.executor.insecure_clone();
        let ix = self.ix_move(v, &executor.pubkey(), amount, true);
        self.send(vec![ix], &[&executor])
    }

    pub fn from_station(&mut self, v: &V, amount: u64) -> TxResult {
        let executor = self.executor.insecure_clone();
        let ix = self.ix_move(v, &executor.pubkey(), amount, false);
        self.send(vec![ix], &[&executor])
    }

    /// The executor approved at the station, `amount` of the vault's cash
    /// sent there and deposited at the venue, and `orders` placed.
    pub fn trade(&mut self, v: &V, amount: u64, orders: Vec<OrderRequest>) {
        let station = self.station(&v.home);
        if station.drivers[0].key == Pubkey::default() {
            self.approve_executor(&v.home);
        }
        expect_ok(self.to_station(v, amount));
        self.fund_venue(&v.home, amount);
        if !orders.is_empty() {
            self.place(&v.home, orders);
        }
    }
}

/// The events of one kind the vault emitted through its own instruction
/// data in a transaction, in order: where a node keeps them whole, however
/// long the logs run.
pub fn emitted<E: anchor_lang::Discriminator + AnchorDeserialize>(
    meta: &litesvm::types::TransactionMetadata,
) -> Vec<E> {
    let tag = anchor_lang::event::EVENT_IX_TAG_LE;
    meta.inner_instructions
        .iter()
        .flatten()
        .map(|inner| &inner.instruction.data)
        .filter_map(|data| data.strip_prefix(tag))
        .filter_map(|data| data.strip_prefix(E::DISCRIMINATOR))
        .map(|mut data| E::deserialize(&mut data).unwrap())
        .collect()
}

/// Asserts `a` is within `rel` (scaled by 1e18) of `b`, as Foundry's
/// `assertApproxEqRel`.
#[track_caller]
pub fn approx_rel(a: u128, b: u128, rel: u128) {
    let diff = a.abs_diff(b);
    assert!(
        diff * 1_000_000_000_000_000_000 <= rel * b.max(1),
        "{a} is not within {rel}e-18 of {b}"
    );
}

/// Asserts `a` is within `tolerance` of `b`.
#[track_caller]
pub fn approx(a: u128, b: u128, tolerance: u128) {
    assert!(
        a.abs_diff(b) <= tolerance,
        "{a} is not within {tolerance} of {b}"
    );
}

/// The exit band the fixture's terms give a station.
pub fn terms_band() -> u16 {
    EXIT_BAND
}

pub use anchor_lang::prelude::{AccountMeta, AnchorDeserialize, Pubkey};
