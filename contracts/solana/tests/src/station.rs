//! Stations, the stand-in venue and its driver, as the development chain sets
//! them up: the venue with a spot and a perp market, the station program
//! configured with the dollar and the vault stand-in as the program whose
//! vaults may create stations, and helpers that build every instruction the
//! way a vault, an executor or a passer-by would send it.
//!
//! A [`Home`] is one vault (the vault stand-in's account) and its station.
//! Everything a vault does to its station goes through the vault stand-in's
//! `call`, signed by the vault's own address, at the call depth the vault
//! will use.
//!
//! A station takes one driver per venue, so the stand-in venue is deployed
//! three times ([`VENUES`]), each copy with its own driver: the stand-in
//! driver (and the test driver) at the first, `local-2` and `local-3` at the
//! others. Every venue helper has an `_at` form naming the venue; the plain
//! form is the first.
//!
//! At each venue the station has an authority of its own ([`Home::authority`]),
//! the master of its account there, with a dollar account of its own
//! ([`Home::authority_dollars`]): every venue instruction names those, never
//! the station's address or its dollar account. Dollars reach a venue only as
//! `execute`'s funding ([`Env::execute_funded`]).

use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub use shonin_driver::{Plan, Valuation, BPS, WAD};
pub use shonin_local_venue::{MarginAccount, OrderRequest, Quote};
pub use shonin_station::error::StationError;
pub use shonin_station::state::{Outcome, Station};

use crate::*;
use shonin_registry::read as registry;
use shonin_test_driver::mode;

/// The markets the venue lists, as the EVM fixture's.
pub fn spot() -> [u8; 32] {
    name("BTC")
}
pub fn perp() -> [u8; 32] {
    name("BTC-PERP")
}
/// 60,000 dollars, in the dollar's six decimals.
pub const PRICE: u64 = 60_000_000_000;
/// A whole unit of size.
pub const UNIT: u128 = 1_000_000_000_000_000_000;
pub const MARGIN_BPS: u64 = 1_000;
pub const EXIT_BAND: u16 = 100;
pub const HALT_GRACE: u64 = 10 * MINUTE;
/// A hundred thousand dollars.
pub const FUNDS: u64 = 100_000_000_000;

pub fn local() -> [u8; 32] {
    shonin_local_driver::NAME
}
/// The drivers of the stand-in venue's copies, as the registry names them.
pub fn local_2() -> [u8; 32] {
    name("local-2")
}
pub fn local_3() -> [u8; 32] {
    name("local-3")
}

/// The stand-in venue and its two copies.
pub const VENUES: [Pubkey; 3] = [
    shonin_local_venue::ID,
    shonin_local_venue_2::ID,
    shonin_local_venue_3::ID,
];

/// The venue a driver program trades at.
pub fn venue_of(driver: &Pubkey) -> Pubkey {
    if *driver == shonin_orca_driver::ID {
        shonin_orca_driver::network::WHIRLPOOL_PROGRAM
    } else if *driver == shonin_local_driver_2::ID {
        shonin_local_venue_2::ID
    } else if *driver == shonin_local_driver_3::ID {
        shonin_local_venue_3::ID
    } else {
        shonin_local_venue::ID
    }
}
pub fn test_driver() -> [u8; 32] {
    name("test")
}
pub fn bundle() -> [u8; 32] {
    hash("bundle")
}

pub fn station_error(error: StationError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn venue_error(error: shonin_local_venue::VenueError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn driver_error(error: shonin_local_driver::DriverError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}

/// One vault and its station.
#[derive(Clone, Debug)]
pub struct Home {
    pub label: [u8; 32],
    pub vault: Pubkey,
    pub vault_dollars: Pubkey,
    pub station: Pubkey,
    pub dollars: Pubkey,
    pub mint: Pubkey,
    pub bundle: [u8; 32],
    /// The driver programs it pins, in its order.
    pub drivers: Vec<Pubkey>,
    /// The program the vault belongs to: the vault stand-in, or the vault.
    pub program: Pubkey,
}

impl Home {
    /// The station's authority at the venue of its driver `index`: the
    /// master of its account there.
    pub fn authority(&self, index: usize) -> Pubkey {
        authority_of(&self.station, &self.drivers[index])
    }

    /// That authority's dollar account.
    pub fn authority_dollars(&self, index: usize) -> Pubkey {
        get_associated_token_address(&self.authority(index), &self.mint)
    }
}

/// The authority of `station` at the venue `driver` trades at.
pub fn authority_of(station: &Pubkey, driver: &Pubkey) -> Pubkey {
    shonin_station::venue_authority(station, &venue_of(driver)).0
}

/// One driver a station is created with: its name, the program the test
/// expects the registry to approve for it, and anything its allowlist reads.
pub struct Declared {
    pub name: [u8; 32],
    pub program: Pubkey,
}

impl Declared {
    pub fn local() -> Self {
        Self {
            name: local(),
            program: shonin_local_driver::ID,
        }
    }
    pub fn test() -> Self {
        Self {
            name: test_driver(),
            program: shonin_test_driver::ID,
        }
    }
    /// The driver of the stand-in venue's second copy.
    pub fn local_2() -> Self {
        Self {
            name: local_2(),
            program: shonin_local_driver_2::ID,
        }
    }
    /// The driver of the stand-in venue's third copy.
    pub fn local_3() -> Self {
        Self {
            name: local_3(),
            program: shonin_local_driver_3::ID,
        }
    }
    /// One driver at each venue, as many as a station may declare.
    pub fn one_per_venue() -> Vec<Self> {
        vec![Self::local(), Self::local_2(), Self::local_3()]
    }
}

pub fn mode_address() -> Pubkey {
    Pubkey::find_program_address(&[shonin_test_driver::MODE_SEED], &shonin_test_driver::ID).0
}

impl Env {
    /// The platform with the stand-in venue and stations set up, the test
    /// driver approved as `test`, and one vault (`home`) whose station pins
    /// the stand-in driver, holding [`FUNDS`] in its own dollar account, with
    /// the executor blessed for it.
    pub fn with_station() -> (Self, Home) {
        let mut env = Self::new();
        env.set_up_stations();
        let home = env.open_vault("home");
        expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local()]));
        let home = env.home_of("home", &[shonin_local_driver::ID]);
        let executor = env.executor.pubkey();
        env.bless_executor_key(executor, home.vault.to_bytes(), home.bundle);
        (env, home)
    }

    /// Another vault, `label`, with [`FUNDS`], whose station pins `drivers`,
    /// with the executor blessed for it.
    pub fn new_home(&mut self, label: &str, drivers: &[Declared]) -> Home {
        let home = self.open_vault(label);
        expect_ok(self.create_station(&home, EXIT_BAND, drivers));
        let programs: Vec<Pubkey> = drivers.iter().map(|driver| driver.program).collect();
        let home = self.home_of(label, &programs);
        let executor = self.executor.pubkey();
        self.bless_executor_key(executor, home.vault.to_bytes(), home.bundle);
        home
    }

    /// The stand-in venue and its copies, each with its two markets, the
    /// station program set up, the test driver approved as `test` and the
    /// copies' drivers as `local-2` and `local-3`.
    pub fn set_up_stations(&mut self) {
        self.set_up_stations_for(shonin_test_vault::ID);
    }

    /// The same, with `vault_program` as the program whose vaults may create
    /// stations: the real vault's, or the stand-in's.
    pub fn set_up_stations_for(&mut self, vault_program: Pubkey) {
        let deployer = self.deployer.insecure_clone();
        let operator = self.operator.pubkey();
        let mut ixs: Vec<Instruction> = VENUES
            .iter()
            .map(|venue| Instruction {
                program_id: *venue,
                accounts: shonin_local_venue::accounts::Initialize {
                    deployer: deployer.pubkey(),
                    venue: shonin_local_venue::venue_address_of(venue),
                    vault: shonin_local_venue::vault_address_of(venue),
                    mint: self.mint,
                    program: *venue,
                    program_data: program_data_address(venue),
                    token_program: spl_token::ID,
                    system_program: solana_sdk_ids::system_program::ID,
                }
                .to_account_metas(None),
                data: shonin_local_venue::instruction::Initialize {
                    operator,
                    margin_bps: MARGIN_BPS,
                }
                .data(),
            })
            .collect();
        ixs.push(self.ix_initialize_stations(&deployer.pubkey(), vault_program));
        expect_ok(self.send(ixs, &[&deployer]));
        let operator = self.operator.insecure_clone();
        let list = VENUES
            .iter()
            .flat_map(|venue| {
                [
                    self.ix_list_market_at(venue, spot(), false, PRICE),
                    self.ix_list_market_at(venue, perp(), true, PRICE),
                ]
            })
            .collect();
        expect_ok(self.send(list, &[&operator]));
        expect_ok(self.propose(ApprovalKind::Driver, test_driver(), shonin_test_driver::ID));
        expect_ok(self.propose(ApprovalKind::Driver, local_2(), shonin_local_driver_2::ID));
        expect_ok(self.propose(ApprovalKind::Driver, local_3(), shonin_local_driver_3::ID));
        self.skip(TIMELOCK);
        self.set_mode(mode::HONEST);
    }

    pub fn ix_initialize_stations(&self, deployer: &Pubkey, vault_program: Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Initialize {
                deployer: *deployer,
                config: shonin_station::config_address(),
                mint: self.mint,
                registry_config: registry::config_address(),
                program: shonin_station::ID,
                program_data: program_data_address(&shonin_station::ID),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Initialize { vault_program }.data(),
        }
    }

    // The venue -------------------------------------------------------------

    pub fn ix_list_market(&self, id: [u8; 32], is_perp: bool, price: u64) -> Instruction {
        self.ix_list_market_at(&shonin_local_venue::ID, id, is_perp, price)
    }

    pub fn ix_list_market_at(
        &self,
        venue: &Pubkey,
        id: [u8; 32],
        is_perp: bool,
        price: u64,
    ) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::ListMarket {
                operator: self.operator.pubkey(),
                venue: shonin_local_venue::venue_address_of(venue),
                market: shonin_local_venue::market_address_of(venue, &id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::ListMarket { id, is_perp, price }.data(),
        }
    }

    pub fn ix_set_prices(
        &self,
        market: [u8; 32],
        mark: u64,
        oracle: u64,
        bid: u64,
        ask: u64,
    ) -> Instruction {
        self.ix_set_prices_at(&shonin_local_venue::ID, market, mark, oracle, bid, ask)
    }

    pub fn ix_set_prices_at(
        &self,
        venue: &Pubkey,
        market: [u8; 32],
        mark: u64,
        oracle: u64,
        bid: u64,
        ask: u64,
    ) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::Operate {
                operator: self.operator.pubkey(),
                venue: shonin_local_venue::venue_address_of(venue),
                market: shonin_local_venue::market_address_of(venue, &market),
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::SetPrices {
                quote: Quote {
                    market,
                    mark,
                    oracle,
                    bid,
                    ask,
                },
            }
            .data(),
        }
    }

    pub fn set_prices(&mut self, market: [u8; 32], mark: u64, oracle: u64, bid: u64, ask: u64) {
        self.set_prices_at(&shonin_local_venue::ID, market, mark, oracle, bid, ask);
    }

    pub fn set_prices_at(
        &mut self,
        venue: &Pubkey,
        market: [u8; 32],
        mark: u64,
        oracle: u64,
        bid: u64,
        ask: u64,
    ) {
        let ix = self.ix_set_prices_at(venue, market, mark, oracle, bid, ask);
        let operator = self.operator.insecure_clone();
        expect_ok(self.send(vec![ix], &[&operator]));
    }

    pub fn accrue_funding(&mut self, market: [u8; 32], paid_per_unit: i128) {
        let operator = self.operator.insecure_clone();
        let ix = Instruction {
            program_id: shonin_local_venue::ID,
            accounts: shonin_local_venue::accounts::Operate {
                operator: operator.pubkey(),
                venue: shonin_local_venue::venue_address(),
                market: shonin_local_venue::market_address(&market),
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::AccrueFunding { paid_per_unit }.data(),
        };
        expect_ok(self.send(vec![ix], &[&operator]));
    }

    /// The stand-in venue's upgrade authority replaces its code with the
    /// test-only hostile venue's (`shonin-test-hostile-venue`), which turns
    /// the signature the station lends it, its authority at that venue, on
    /// whatever it can reach.
    pub fn turn_venue_hostile(&mut self) {
        let elf = crate::so("shonin_test_hostile_venue");
        self.svm.add_program(shonin_local_venue::ID, &elf).unwrap();
    }

    pub fn venue_vault_balance(&self) -> u64 {
        self.token_balance(&shonin_local_venue::vault_address())
    }

    pub fn token_balance(&self, account: &Pubkey) -> u64 {
        self.svm
            .get_account(account)
            .filter(|account| account.data.len() >= 72)
            .map_or(0, |account| {
                u64::from_le_bytes(account.data[64..72].try_into().unwrap())
            })
    }

    /// `master`'s account at the venue.
    pub fn margin_account(&self, master: &Pubkey) -> Option<MarginAccount> {
        self.margin_account_at(&shonin_local_venue::ID, master)
    }

    pub fn margin_account_at(&self, venue: &Pubkey, master: &Pubkey) -> Option<MarginAccount> {
        self.load(&shonin_local_venue::account_address_of(venue, master))
    }

    pub fn position(&self, master: &Pubkey, market: [u8; 32]) -> i128 {
        self.position_at(&shonin_local_venue::ID, master, market)
    }

    pub fn position_at(&self, venue: &Pubkey, master: &Pubkey, market: [u8; 32]) -> i128 {
        self.margin_account_at(venue, master)
            .map_or(0, |account| account.size_in(&market))
    }

    pub fn venue_cash(&self, master: &Pubkey) -> i128 {
        self.margin_account(master)
            .map_or(0, |account| account.cash)
    }

    /// The markets `master` holds, in its account's order.
    pub fn held_markets(&self, master: &Pubkey) -> Vec<Pubkey> {
        self.held_markets_at(&shonin_local_venue::ID, master)
    }

    pub fn held_markets_at(&self, venue: &Pubkey, master: &Pubkey) -> Vec<Pubkey> {
        self.margin_account_at(venue, master)
            .map_or(Vec::new(), |account| {
                account
                    .positions
                    .iter()
                    .map(|position| shonin_local_venue::market_address_of(venue, &position.market))
                    .collect()
            })
    }

    pub fn ix_venue_open(&self, payer: &Pubkey, master: &Pubkey) -> Instruction {
        self.ix_venue_open_at(&shonin_local_venue::ID, payer, master)
    }

    pub fn ix_venue_open_at(&self, venue: &Pubkey, payer: &Pubkey, master: &Pubkey) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::OpenAccount {
                payer: *payer,
                master: *master,
                account: shonin_local_venue::account_address_of(venue, master),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::OpenAccount {}.data(),
        }
    }

    pub fn ix_venue_close(&self, master: &Pubkey, payer: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_local_venue::ID,
            accounts: shonin_local_venue::accounts::CloseAccount {
                master: *master,
                account: shonin_local_venue::account_address(master),
                payer: *payer,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::CloseAccount {}.data(),
        }
    }

    pub fn ix_venue_deposit(&self, master: &Pubkey, from: &Pubkey, amount: u64) -> Instruction {
        self.ix_venue_deposit_at(&shonin_local_venue::ID, master, from, amount)
    }

    pub fn ix_venue_deposit_at(
        &self,
        venue: &Pubkey,
        master: &Pubkey,
        from: &Pubkey,
        amount: u64,
    ) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::Deposit {
                master: *master,
                account: shonin_local_venue::account_address_of(venue, master),
                master_dollars: *from,
                venue: shonin_local_venue::venue_address_of(venue),
                vault: shonin_local_venue::vault_address_of(venue),
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::Deposit { amount }.data(),
        }
    }

    pub fn ix_venue_withdraw(&self, master: &Pubkey, to: &Pubkey, amount: u64) -> Instruction {
        self.ix_venue_withdraw_at(&shonin_local_venue::ID, master, to, amount)
    }

    pub fn ix_venue_withdraw_at(
        &self,
        venue: &Pubkey,
        master: &Pubkey,
        to: &Pubkey,
        amount: u64,
    ) -> Instruction {
        let mut accounts = shonin_local_venue::accounts::Withdraw {
            master: *master,
            account: shonin_local_venue::account_address_of(venue, master),
            master_dollars: *to,
            venue: shonin_local_venue::venue_address_of(venue),
            vault: shonin_local_venue::vault_address_of(venue),
            mint: self.mint,
            token_program: spl_token::ID,
        }
        .to_account_metas(None);
        accounts.extend(
            self.held_markets_at(venue, master)
                .into_iter()
                .map(|market| AccountMeta::new_readonly(market, false)),
        );
        Instruction {
            program_id: *venue,
            accounts,
            data: shonin_local_venue::instruction::Withdraw { amount }.data(),
        }
    }

    /// Places `orders` for `master`, with every market it holds and every
    /// market an order names.
    pub fn ix_venue_place(&self, master: &Pubkey, orders: Vec<OrderRequest>) -> Instruction {
        self.ix_venue_place_at(&shonin_local_venue::ID, master, orders)
    }

    pub fn ix_venue_place_at(
        &self,
        venue: &Pubkey,
        master: &Pubkey,
        orders: Vec<OrderRequest>,
    ) -> Instruction {
        let mut accounts = shonin_local_venue::accounts::Place {
            master: *master,
            account: shonin_local_venue::account_address_of(venue, master),
            venue: shonin_local_venue::venue_address_of(venue),
        }
        .to_account_metas(None);
        let mut markets = self.held_markets_at(venue, master);
        for order in &orders {
            let market = shonin_local_venue::market_address_of(venue, &order.market);
            if !markets.contains(&market) {
                markets.push(market);
            }
        }
        accounts.extend(
            markets
                .into_iter()
                .map(|market| AccountMeta::new_readonly(market, false)),
        );
        Instruction {
            program_id: *venue,
            accounts,
            data: shonin_local_venue::instruction::Place { orders }.data(),
        }
    }

    // The test driver ---------------------------------------------------------

    pub fn set_mode(&mut self, mode: u8) {
        let payer = self.payer.insecure_clone();
        let ix = Instruction {
            program_id: shonin_test_driver::ID,
            accounts: shonin_test_driver::accounts::SetMode {
                payer: payer.pubkey(),
                mode: mode_address(),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_test_driver::instruction::SetMode { mode }.data(),
        };
        expect_ok(self.send(vec![ix], &[&payer]));
    }

    // Vaults and stations ---------------------------------------------------------

    /// Opens a vault stand-in named `label`, with its dollar account, and
    /// gives it [`FUNDS`].
    pub fn open_vault(&mut self, label: &str) -> Home {
        let label = name(label);
        let payer = self.payer.insecure_clone();
        let vault = shonin_test_vault::vault_address(&label);
        let ix = Instruction {
            program_id: shonin_test_vault::ID,
            accounts: shonin_test_vault::accounts::Open {
                payer: payer.pubkey(),
                vault,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_test_vault::instruction::Open { label }.data(),
        };
        expect_ok(self.send(vec![ix], &[&payer]));
        self.give(&vault, FUNDS);
        self.home_of_label(label, &[])
    }

    pub fn home_of(&self, label: &str, drivers: &[Pubkey]) -> Home {
        self.home_of_label(name(label), drivers)
    }

    pub fn home_of_label(&self, label: [u8; 32], drivers: &[Pubkey]) -> Home {
        let vault = shonin_test_vault::vault_address(&label);
        let station = shonin_station::station_address(&vault);
        Home {
            label,
            vault,
            vault_dollars: get_associated_token_address(&vault, &self.mint),
            station,
            dollars: get_associated_token_address(&station, &self.mint),
            mint: self.mint,
            bundle: bundle(),
            drivers: drivers.to_vec(),
            program: shonin_test_vault::ID,
        }
    }

    /// The station's record.
    pub fn station(&self, home: &Home) -> Station {
        self.load(&home.station).expect("the station exists")
    }

    /// The vault stand-in's `call` of `ix`, signed by the vault, with `depth`
    /// calls of the stand-in in between.
    pub fn ix_call(&self, home: &Home, ix: Instruction, depth: u8) -> Instruction {
        let mut accounts = shonin_test_vault::accounts::Call {
            vault: home.vault,
            dollars: home.vault_dollars,
            program: ix.program_id,
        }
        .to_account_metas(None);
        accounts.extend(ix.accounts.into_iter().map(|mut meta| {
            // The vault program signs for the vault; the transaction doesn't.
            if meta.pubkey == home.vault {
                meta.is_signer = false;
            }
            meta
        }));
        Instruction {
            program_id: shonin_test_vault::ID,
            accounts,
            data: shonin_test_vault::instruction::Call {
                data: ix.data,
                depth,
            }
            .data(),
        }
    }

    /// Sends `ix` as the vault, paid for by the fixture's payer.
    pub fn as_vault(&mut self, home: &Home, ix: Instruction) -> TxResult {
        let call = self.ix_call(home, ix, 0);
        let payer = self.payer.insecure_clone();
        self.send(vec![call], &[&payer])
    }

    /// What the vault stand-in measured arriving in its last call.
    pub fn received(&self, home: &Home) -> u64 {
        self.load::<shonin_test_vault::VaultState>(&home.vault)
            .unwrap()
            .received
    }

    pub fn set_view(&mut self, home: &Home, halted_at: u64, halt_grace: u64, unsettled: bool) {
        let payer = self.payer.insecure_clone();
        let ix = Instruction {
            program_id: shonin_test_vault::ID,
            accounts: shonin_test_vault::accounts::SetView { vault: home.vault }
                .to_account_metas(None),
            data: shonin_test_vault::instruction::SetView {
                halted_at,
                halt_grace,
                unsettled,
            }
            .data(),
        };
        expect_ok(self.send(vec![ix], &[&payer]));
    }

    /// The implementation a station created now pins for `name`, and the
    /// revocation record `create` is given with it.
    fn approval(&self, name: [u8; 32]) -> (Pubkey, Pubkey) {
        let slot = registry::approval_address(ApprovalKind::Driver, &name);
        let effective = self
            .load::<shonin_registry::state::ApprovalSlot>(&slot)
            .map(|slot| slot.effective(self.now()))
            .unwrap_or_default();
        (
            slot,
            registry::revoked_address(ApprovalKind::Driver, &name, &effective),
        )
    }

    /// The station's `create`, as the vault sends it.
    pub fn ix_create(
        &self,
        home: &Home,
        payer: &Pubkey,
        band: u16,
        drivers: &[Declared],
    ) -> Instruction {
        let mut accounts = shonin_station::accounts::Create {
            payer: *payer,
            vault: home.vault,
            vault_dollars: home.vault_dollars,
            config: shonin_station::config_address(),
            station: home.station,
            station_dollars: home.dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None);
        let mut sizes = Vec::new();
        for driver in drivers {
            let (slot, revoked) = self.approval(driver.name);
            let authority = authority_of(&home.station, &driver.program);
            let mut group = vec![
                AccountMeta::new_readonly(driver.program, false),
                AccountMeta::new_readonly(driver.program, false),
                AccountMeta::new_readonly(slot, false),
                AccountMeta::new_readonly(revoked, false),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(get_associated_token_address(&authority, &self.mint), false),
            ];
            if driver.program == shonin_test_driver::ID {
                group.push(AccountMeta::new_readonly(mode_address(), false));
            }
            sizes.push(group.len() as u8);
            accounts.extend(group);
        }
        let bump = Pubkey::find_program_address(
            &[shonin_test_vault::VAULT_SEED, &home.label],
            &shonin_test_vault::ID,
        )
        .1;
        Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Create {
                args: shonin_station::CreateArgs {
                    bundle_hash: home.bundle,
                    exit_band_bps: band,
                    drivers: drivers.iter().map(|driver| driver.name).collect(),
                    driver_accounts: sizes,
                    vault_seeds: vec![
                        shonin_test_vault::VAULT_SEED.to_vec(),
                        home.label.to_vec(),
                        vec![bump],
                    ],
                },
            }
            .data(),
        }
    }

    /// The vault creates its station. With every driver a station may
    /// declare, each with its venue authority and that authority's dollar
    /// account, the call names more accounts than the wire carries by
    /// address, so it goes through a lookup table, as the vault's own would.
    pub fn create_station(&mut self, home: &Home, band: u16, drivers: &[Declared]) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_create(home, &payer.pubkey(), band, drivers);
        let call = self.ix_call(home, ix, 0);
        self.send_fitting(vec![call], &[&payer])
    }

    // Keys ---------------------------------------------------------------------

    /// The registry records the station reads for `key`: the config, the
    /// key's blessing for this vault, its key record, and its image's and
    /// project's records.
    pub fn key_records(&self, home: &Home, key: &Pubkey) -> [Pubkey; 5] {
        let (image, project) = self
            .key_info(key)
            .map(|info| {
                (
                    registry::image_address(&info.image_digest),
                    registry::project_address(&info.project_id),
                )
            })
            .unwrap_or_default();
        [
            registry::config_address(),
            registry::blessing_address(key, &self.cluster(), &home.vault.to_bytes(), &home.bundle),
            registry::key_address(key),
            image,
            project,
        ]
    }

    /// The approval slot of `name` and the revocation record of `program`.
    pub fn driver_records(&self, name: [u8; 32], program: &Pubkey) -> [Pubkey; 2] {
        [
            registry::approval_address(ApprovalKind::Driver, &name),
            registry::revoked_address(ApprovalKind::Driver, &name, program),
        ]
    }

    /// `caller` approves `key` for the driver at `index`; the records of the
    /// key in place come along when there is one.
    pub fn ix_approve_key(
        &self,
        home: &Home,
        caller: &Pubkey,
        index: usize,
        key: &Pubkey,
    ) -> Instruction {
        let station = self.station(home);
        let driver = &station.drivers[index];
        let [config, blessing, key_info, image, project] = self.key_records(home, key);
        let [slot, revoked] = self.driver_records(driver.name, &driver.program);
        // The key in place: whichever other key any driver has approved.
        let current = station
            .drivers
            .iter()
            .map(|d| d.key)
            .find(|held| *held != Pubkey::default() && held != key)
            .map(|held| self.key_records(home, &held));
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::ApproveKey {
                caller: *caller,
                station: home.station,
                registry_config: config,
                blessing,
                key_info,
                image,
                project,
                slot,
                revoked,
                current_blessing: current.map(|r| r[1]),
                current_key_info: current.map(|r| r[2]),
                current_image: current.map(|r| r[3]),
                current_project: current.map(|r| r[4]),
            }
            .to_account_metas(None),
            data: shonin_station::instruction::ApproveKey {
                driver: driver.name,
                key: *key,
            }
            .data(),
        }
    }

    /// `caller` signs and pays to approve `key` for the station's first
    /// driver.
    pub fn approve_key_by(&mut self, home: &Home, caller: &Keypair, key: &Pubkey) -> TxResult {
        self.approve_key_at(home, caller, 0, key)
    }

    pub fn approve_key_at(
        &mut self,
        home: &Home,
        caller: &Keypair,
        index: usize,
        key: &Pubkey,
    ) -> TxResult {
        let ix = self.ix_approve_key(home, &caller.pubkey(), index, key);
        self.send(vec![ix], &[caller])
    }

    /// The executor approves itself for the first driver.
    pub fn approve_executor(&mut self, home: &Home) {
        let executor = self.executor.insecure_clone();
        expect_ok(self.approve_key_by(home, &executor, &executor.pubkey()));
    }

    pub fn ix_revoke_key(&self, home: &Home, index: usize, key: &Pubkey) -> Instruction {
        let station = self.station(home);
        let driver = &station.drivers[index];
        let [config, blessing, key_info, image, project] = self.key_records(home, key);
        let [slot, revoked] = self.driver_records(driver.name, &driver.program);
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::RevokeKey {
                station: home.station,
                registry_config: config,
                blessing,
                key_info,
                image,
                project,
                slot,
                revoked,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::RevokeKey {
                driver: driver.name,
                key: *key,
            }
            .data(),
        }
    }

    // The guard -----------------------------------------------------------------

    /// `key` sends `venue_ix` through the station as driver `index`'s, with
    /// no dollars.
    pub fn ix_execute(
        &self,
        home: &Home,
        key: &Pubkey,
        index: u8,
        venue_ix: Instruction,
    ) -> Instruction {
        self.ix_execute_funded(home, key, index, venue_ix, 0)
    }

    /// The same, the station first moving `fund` of its counted dollars to
    /// the driver's venue authority.
    pub fn ix_execute_funded(
        &self,
        home: &Home,
        key: &Pubkey,
        index: u8,
        venue_ix: Instruction,
        fund: u64,
    ) -> Instruction {
        let station = self.station(home);
        // A driver the station doesn't have is sent with its first driver's
        // records, to be refused for what it is.
        let driver = station
            .drivers
            .get(index as usize)
            .unwrap_or(&station.drivers[0]);
        let [config, blessing, key_info, image, project] = self.key_records(home, key);
        let [slot, revoked] = self.driver_records(driver.name, &driver.program);
        let mut accounts = shonin_station::accounts::Execute {
            key: *key,
            station: home.station,
            station_dollars: home.dollars,
            authority: driver.authority,
            authority_dollars: driver.dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            registry_config: config,
            blessing,
            key_info,
            image,
            project,
            slot,
            revoked,
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new_readonly(venue_ix.program_id, false));
        accounts.extend(venue_ix.accounts.into_iter().map(|mut meta| {
            // The station signs for its authorities; the transaction can't.
            if meta.pubkey == home.station
                || station.drivers.iter().any(|d| d.authority == meta.pubkey)
            {
                meta.is_signer = false;
            }
            meta
        }));
        Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Execute {
                driver: index,
                data: venue_ix.data,
                fund,
            }
            .data(),
        }
    }

    /// The executor sends `venue_ix` through the station.
    pub fn execute(&mut self, home: &Home, venue_ix: Instruction) -> TxResult {
        let executor = self.executor.insecure_clone();
        self.execute_as(home, &executor, 0, venue_ix)
    }

    /// The executor sends `venue_ix` through the station's first driver,
    /// funded with `fund` of its counted dollars.
    pub fn execute_funded(&mut self, home: &Home, venue_ix: Instruction, fund: u64) -> TxResult {
        let executor = self.executor.insecure_clone();
        self.execute_as_funded(home, &executor, 0, venue_ix, fund)
    }

    pub fn execute_as(
        &mut self,
        home: &Home,
        key: &Keypair,
        index: u8,
        venue_ix: Instruction,
    ) -> TxResult {
        self.execute_as_funded(home, key, index, venue_ix, 0)
    }

    pub fn execute_as_funded(
        &mut self,
        home: &Home,
        key: &Keypair,
        index: u8,
        venue_ix: Instruction,
        fund: u64,
    ) -> TxResult {
        let ix = self.ix_execute_funded(home, &key.pubkey(), index, venue_ix, fund);
        // A call naming every market an account may hold is more than the
        // wire carries by address beside the station's own accounts; the
        // executor sends that one through a lookup table.
        self.send_fitting(vec![ix], &[key])
    }

    // Money ----------------------------------------------------------------------

    pub fn ix_pull(&self, home: &Home, amount: u64) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Pull {
                vault: home.vault,
                station: home.station,
                vault_dollars: home.vault_dollars,
                station_dollars: home.dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Pull { amount }.data(),
        }
    }

    pub fn ix_push(&self, home: &Home, amount: u64) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Push {
                vault: home.vault,
                station: home.station,
                vault_dollars: home.vault_dollars,
                station_dollars: home.dollars,
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Push { amount }.data(),
        }
    }

    pub fn pull(&mut self, home: &Home, amount: u64) -> TxResult {
        let ix = self.ix_pull(home, amount);
        self.as_vault(home, ix)
    }

    pub fn push(&mut self, home: &Home, amount: u64) -> TxResult {
        let ix = self.ix_push(home, amount);
        self.as_vault(home, ix)
    }

    /// The vault's money at the venue: [`FUNDS`] pulled, the executor
    /// approved, the station's venue account opened, everything deposited,
    /// and one BTC bought spot against one sold short on the perp.
    pub fn start_trading(&mut self, home: &Home) {
        expect_ok(self.pull(home, FUNDS));
        self.approve_executor(home);
        self.fund_venue(home, FUNDS);
        let orders = vec![
            order(spot(), true, UNIT, PRICE),
            order(perp(), false, UNIT, PRICE),
        ];
        self.place(home, orders);
    }

    /// Opens the station's venue account if need be and deposits `amount`,
    /// funded from the station's counted dollars.
    pub fn fund_venue(&mut self, home: &Home, amount: u64) {
        self.fund_venue_at(home, 0, amount);
    }

    /// The same at the venue of the station's driver `index`, through it.
    pub fn fund_venue_at(&mut self, home: &Home, index: u8, amount: u64) {
        let executor = self.executor.insecure_clone();
        let venue = venue_of(&home.drivers[index as usize]);
        let authority = home.authority(index as usize);
        if self.margin_account_at(&venue, &authority).is_none() {
            let open = self.ix_venue_open_at(&venue, &executor.pubkey(), &authority);
            expect_ok(self.execute_as(home, &executor, index, open));
        }
        let deposit = self.ix_venue_deposit_at(
            &venue,
            &authority,
            &home.authority_dollars(index as usize),
            amount,
        );
        expect_ok(self.execute_as_funded(home, &executor, index, deposit, amount));
    }

    /// The executor places `orders` for the station.
    pub fn place(&mut self, home: &Home, orders: Vec<OrderRequest>) {
        self.place_at(home, 0, orders);
    }

    /// The same at the venue of the station's driver `index`, through it.
    pub fn place_at(&mut self, home: &Home, index: u8, orders: Vec<OrderRequest>) {
        let executor = self.executor.insecure_clone();
        let venue = venue_of(&home.drivers[index as usize]);
        let ix = self.ix_venue_place_at(&venue, &home.authority(index as usize), orders);
        expect_ok(self.execute_as(home, &executor, index, ix));
    }

    // Driver groups, value and unwinding ----------------------------------------------

    /// The accounts a driver is given for `station`: the program, the
    /// station's authority at the driver's venue and its dollar account, then
    /// the stand-in driver's layout at that venue, behind the mode account for
    /// the test driver.
    pub fn driver_group(&self, station: &Pubkey, program: &Pubkey) -> Vec<AccountMeta> {
        if *program == shonin_orca_driver::ID {
            return self.orca_group(station);
        }
        let venue = venue_of(program);
        let authority = authority_of(station, program);
        let mut group = vec![
            AccountMeta::new_readonly(*program, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(get_associated_token_address(&authority, &self.mint), false),
        ];
        if *program == shonin_test_driver::ID {
            group.push(AccountMeta::new_readonly(mode_address(), false));
        }
        group.extend([
            AccountMeta::new_readonly(venue, false),
            AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&venue), false),
            AccountMeta::new(shonin_local_venue::vault_address_of(&venue), false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new(
                shonin_local_venue::account_address_of(&venue, &authority),
                false,
            ),
        ]);
        group.extend(
            self.held_markets_at(&venue, &authority)
                .into_iter()
                .map(|market| AccountMeta::new_readonly(market, false)),
        );
        group
    }

    /// One group per driver named, and their sizes.
    pub fn groups(&self, home: &Home, programs: &[Pubkey]) -> (Vec<AccountMeta>, Vec<u8>) {
        let mut accounts = Vec::new();
        let mut sizes = Vec::new();
        for program in programs {
            let group = self.driver_group(&home.station, program);
            sizes.push(group.len() as u8);
            accounts.extend(group);
        }
        (accounts, sizes)
    }

    pub fn ix_value(&self, home: &Home) -> Instruction {
        let (groups, sizes) = self.groups(home, &home.drivers);
        let mut accounts = shonin_station::accounts::Value {
            station: home.station,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Value { groups: sizes }.data(),
        }
    }

    /// What the station is worth, asked as anyone may ask it.
    pub fn value(&mut self, home: &Home) -> Valuation {
        let ix = self.ix_value(home);
        let meta = self.simulate(vec![
            solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(
                1_400_000,
            ),
            ix,
        ]);
        Valuation::try_from_slice(&meta.return_data.data).expect("a valuation")
    }

    /// The station's `unwind` of `fraction`, by `caller`: the vault, or a key
    /// whose records come along (`as_key`), or a passer-by. `view` brings the
    /// vault's account for a stopped station's full unwind by someone other
    /// than the vault.
    pub fn ix_unwind(
        &self,
        home: &Home,
        caller: &Pubkey,
        fraction: u64,
        as_key: bool,
    ) -> Instruction {
        self.ix_unwind_through(home, caller, fraction, as_key, &home.drivers.clone())
    }

    pub fn ix_unwind_through(
        &self,
        home: &Home,
        caller: &Pubkey,
        fraction: u64,
        as_key: bool,
        programs: &[Pubkey],
    ) -> Instruction {
        let groups = self.groups(home, programs);
        self.ix_unwind_with(home, caller, fraction, as_key, groups)
    }

    /// The same with the driver groups given as they are, and their sizes.
    pub fn ix_unwind_with(
        &self,
        home: &Home,
        caller: &Pubkey,
        fraction: u64,
        as_key: bool,
        (groups, sizes): (Vec<AccountMeta>, Vec<u8>),
    ) -> Instruction {
        let records = as_key.then(|| self.key_records(home, caller));
        let view = *caller != home.vault;
        let mut accounts = shonin_station::accounts::Unwind {
            caller: *caller,
            station: home.station,
            station_dollars: home.dollars,
            vault_dollars: home.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            registry_config: records.map(|r| r[0]),
            blessing: records.map(|r| r[1]),
            key_info: records.map(|r| r[2]),
            image: records.map(|r| r[3]),
            project: records.map(|r| r[4]),
            vault: view.then_some(home.vault),
            vault_program: view.then_some(home.program),
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Unwind {
                fraction,
                groups: sizes,
            }
            .data(),
        }
    }

    /// The vault unwinds `fraction`; the outcome, and what the vault measured
    /// arriving.
    pub fn unwind_as_vault(
        &mut self,
        home: &Home,
        fraction: u64,
    ) -> std::result::Result<(Outcome, u64), FailedTx> {
        let ix = self.ix_unwind(home, &home.vault, fraction, false);
        let meta = self.as_vault(home, ix)?;
        let outcome = Outcome::try_from_slice(&meta.return_data.data).expect("an outcome");
        Ok((outcome, self.received(home)))
    }

    /// A passer-by unwinds `fraction`.
    pub fn unwind_as(
        &mut self,
        home: &Home,
        who: &Keypair,
        fraction: u64,
    ) -> std::result::Result<Outcome, FailedTx> {
        let ix = self.ix_unwind(home, &who.pubkey(), fraction, false);
        let meta = self.send(vec![ix], &[who])?;
        Ok(Outcome::try_from_slice(&meta.return_data.data).expect("an outcome"))
    }

    pub fn ix_unwind_stopped(&self, home: &Home, caller: &Pubkey, indices: &[u8]) -> Instruction {
        let programs: Vec<Pubkey> = indices.iter().map(|i| home.drivers[*i as usize]).collect();
        let (groups, sizes) = self.groups(home, &programs);
        let mut accounts = shonin_station::accounts::UnwindStopped {
            caller: *caller,
            station: home.station,
            station_dollars: home.dollars,
            vault_dollars: home.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            vault: home.vault,
            vault_program: home.program,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::UnwindStopped {
                drivers: indices.to_vec(),
                groups: sizes,
            }
            .data(),
        }
    }

    pub fn unwind_stopped(
        &mut self,
        home: &Home,
        who: &Keypair,
        indices: &[u8],
    ) -> std::result::Result<Outcome, FailedTx> {
        let ix = self.ix_unwind_stopped(home, &who.pubkey(), indices);
        let meta = self.send(vec![ix], &[who])?;
        Ok(Outcome::try_from_slice(&meta.return_data.data).expect("an outcome"))
    }

    pub fn ix_stop(&self, home: &Home) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Stop {
                station: home.station,
                vault: home.vault,
                vault_program: home.program,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Stop {}.data(),
        }
    }

    pub fn stop(&mut self, home: &Home) -> TxResult {
        let ix = self.ix_stop(home);
        let payer = self.payer.insecure_clone();
        self.send(vec![ix], &[&payer])
    }

    /// The vault halts now, its grace passes, and anyone stops the station.
    pub fn halt_and_stop(&mut self, home: &Home) {
        let now = self.now();
        self.set_view(home, now, HALT_GRACE, false);
        self.skip(HALT_GRACE);
        expect_ok(self.stop(home));
    }

    /// The station's counted dollars.
    pub fn idle(&self, home: &Home) -> u64 {
        self.station(home).idle
    }

    /// Everything the station has ever sent home, as it counts it.
    pub fn sent_home(&self, home: &Home) -> u128 {
        self.station(home).sent_home
    }
}

pub type FailedTx = litesvm::types::FailedTransactionMetadata;

/// The station's `SentHome` events in a transaction's logs, in order.
pub fn sent_home_events(logs: &[String]) -> Vec<shonin_station::events::SentHome> {
    events(logs)
}

/// The events of one kind in a transaction's logs, in order.
pub fn events<E: anchor_lang::Discriminator + AnchorDeserialize>(logs: &[String]) -> Vec<E> {
    use base64::Engine;
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
        .filter(|bytes| bytes.starts_with(E::DISCRIMINATOR))
        .map(|bytes| E::try_from_slice(&bytes[E::DISCRIMINATOR.len()..]).unwrap())
        .collect()
}

/// An order at a limit, neither reduce-only.
pub fn order(market: [u8; 32], is_buy: bool, size: u128, limit_price: u64) -> OrderRequest {
    OrderRequest {
        market,
        is_buy,
        size,
        limit_price,
        reduce_only: false,
    }
}

pub fn reduce(market: [u8; 32], is_buy: bool, size: u128, limit_price: u64) -> OrderRequest {
    OrderRequest {
        reduce_only: true,
        ..order(market, is_buy, size, limit_price)
    }
}

/// The custom error a failed transaction ended with, by any program.
pub fn failed_code(result: &std::result::Result<impl Sized, FailedTx>) -> Option<u32> {
    match result {
        Err(failed) => match &failed.err {
            solana_transaction_error::TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(code),
            ) => Some(*code),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// Asserts the call failed with exactly this custom error.
#[track_caller]
pub fn expect_err<T>(result: std::result::Result<T, FailedTx>, expected: u32) {
    match &result {
        Ok(_) => panic!("expected error {expected}, but it went through"),
        Err(failed) => assert_eq!(
            failed_code(&result),
            Some(expected),
            "expected error {expected}, got {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        ),
    }
}

/// Unwraps a call that should have gone through, with its logs if not.
#[track_caller]
pub fn ok<T>(result: std::result::Result<T, FailedTx>) -> T {
    match result {
        Ok(value) => value,
        Err(failed) => panic!(
            "it failed: {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        ),
    }
}
