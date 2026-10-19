//! The platform as the development chain deploys it, in-process: the programs
//! `anchor build` wrote to `target/deploy`, run by LiteSVM, so every test
//! starts from the arrangement the running system actually has.
//!
//! The programs that are set up by their deployer are deployed upgradeable,
//! with the deployer as their upgrade authority, as `anchor deploy` leaves
//! them before they are made final; the development checker, the stand-in
//! venue's driver, and every stand-in driver or bridge program, is deployed
//! final. `station` adds the stations, the stand-in venue and its driver.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::{
    get_associated_token_address_with_program_id,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
};
use anchor_spl::token::spl_token;
use litesvm::types::{FailedTransactionMetadata, TransactionMetadata};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::error::InstructionError;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

pub use shonin_common::digest::{blessing_leaf, cluster_value, keccak};
pub use shonin_prices::{Allowances, PricePeriod, UnitPrices};
pub use shonin_publishing::state::{ChainNeed, Lists, Review, ReviewInput, Strategy, Submission};
pub use shonin_registry::state::{ApprovalKind, ImageKind, Journal, KeyInfo};

pub mod orca;
pub mod real_pki_token;
pub mod real_token;
pub mod station;
pub mod vault;

use shonin_registry::read as registry;

pub const MINUTE: u64 = 60;
pub const HOUR: u64 = 3600;
pub const DAY: u64 = 86_400;
/// A date far enough in that a timelock and a blessing have room behind
/// them; a chain that starts at zero makes both meaningless.
pub const START: u64 = 365 * DAY;
pub const TIMELOCK: u64 = 2 * DAY;
pub const BLESSING_TTL: u64 = 72 * HOUR;
pub const PUBLISH_FEE: u64 = 1_000_000;
pub const PRICE_NOTICE: u64 = 30 * DAY;
pub const CLUSTER: &str = "localnet";
/// The most a transaction may weigh on the wire.
pub const PACKET_DATA_SIZE: usize = 1232;
/// The most accounts one transaction may lock.
pub const MAX_TX_ACCOUNTS: usize = 64;

pub fn executor_image() -> [u8; 32] {
    keccak(&[b"executor image"])
}
pub fn builder_image() -> [u8; 32] {
    keccak(&[b"builder image"])
}
pub fn reviewer_image() -> [u8; 32] {
    keccak(&[b"reviewer image"])
}
pub fn unwinder_image() -> [u8; 32] {
    keccak(&[b"unwinder image"])
}
pub fn project() -> [u8; 32] {
    keccak(&[b"project"])
}
/// A 32-byte name, as `bytes32("name")` lays it out.
pub fn name(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}
pub fn hash(text: &str) -> [u8; 32] {
    keccak(&[text.as_bytes()])
}

pub type TxResult = std::result::Result<TransactionMetadata, FailedTransactionMetadata>;

/// The custom error code a failed transaction ended with, if any.
pub fn code_of(result: &TxResult) -> Option<u32> {
    match result {
        Err(failed) => match &failed.err {
            TransactionError::InstructionError(_, InstructionError::Custom(code)) => Some(*code),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// An Anchor program error's code.
pub fn code(error: impl Into<u32>) -> u32 {
    error.into()
}

pub fn registry_error(error: shonin_registry::error::RegistryError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn publishing_error(error: shonin_publishing::error::PublishingError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn prices_error(error: shonin_prices::PricesError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn profiles_error(error: shonin_profiles::ProfilesError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}
pub fn anchor_error(error: anchor_lang::error::ErrorCode) -> u32 {
    error as u32
}

/// Asserts the transaction failed with exactly this custom error.
#[track_caller]
pub fn expect_code(result: TxResult, expected: u32) {
    match &result {
        Ok(_) => panic!("expected error {expected}, but the transaction went through"),
        Err(failed) => assert_eq!(
            code_of(&result),
            Some(expected),
            "expected error {expected}, got {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        ),
    }
}

/// Asserts the transaction failed, however.
#[track_caller]
pub fn expect_failure(result: TxResult) {
    assert!(result.is_err(), "expected the transaction to fail");
}

/// Asserts the transaction went through.
#[track_caller]
pub fn expect_ok(result: TxResult) -> TransactionMetadata {
    match result {
        Ok(meta) => meta,
        Err(failed) => panic!(
            "transaction failed: {:?}\n{}",
            failed.err,
            failed.meta.logs.join("\n")
        ),
    }
}

pub(crate) fn so(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/deploy/{name}.so", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing: run `anchor build` first"))
}

/// The address of a program's program-data account.
pub fn program_data_address(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[program.as_ref()],
        &solana_sdk_ids::bpf_loader_upgradeable::ID,
    )
    .0
}

pub struct Env {
    pub svm: LiteSVM,
    /// Pays for transactions nobody in particular sends.
    pub payer: Keypair,
    /// The upgrade authority of the programs set up at deployment.
    pub deployer: Keypair,
    pub admin: Keypair,
    /// The key that stands for the hardware.
    pub attestor: Keypair,
    /// The first checker: the development checker trusting `attestor`.
    pub checker: Pubkey,
    pub mint: Pubkey,
    pub mint_authority: Keypair,
    pub platform: Keypair,
    pub builder: Keypair,
    pub reviewer: Keypair,
    pub executor: Keypair,
    pub author: Keypair,
    pub alice: Keypair,
    pub bob: Keypair,
    /// Moves the stand-in venue's prices.
    pub operator: Keypair,
    /// The stand-in driver and bridge programs approved at setup.
    pub driver: Pubkey,
    pub bridge: Pubkey,
    /// Program bytes for stand-in programs.
    elf: Vec<u8>,
    seeds: u64,
    /// The nonce the next vault request a helper builds is made at, so no
    /// two requests share an address whoever makes them.
    pub(crate) nonces: std::cell::Cell<u64>,
    /// Every vault request's address the helpers have seen, by vault and id,
    /// so one closed since can still be named.
    pub(crate) requests: std::cell::RefCell<std::collections::HashMap<(Pubkey, u64), Pubkey>>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    /// The whole platform, set up as the development chain sets it up: the
    /// registry with a two-day timelock and no floor, the executor, builder
    /// and reviewer images and the project proposed, a driver and a bridge
    /// proposed, the timelock passed, and the builder and reviewer keys
    /// blessed.
    pub fn new() -> Self {
        Self::new_with_mint(None)
    }

    /// The same, the dollar's mint at `mint` when one is given (as a driver
    /// that lists the dollar by its address needs), else at a fresh address.
    pub fn new_with_mint(mint: Option<Pubkey>) -> Self {
        let mut env = Self::bare_with_mint(mint);
        env.initialize_registry(TIMELOCK, 0);
        env.initialize_publishing();
        env.initialize_prices();

        env.driver = shonin_local_driver::ID;
        env.bridge = env.final_program();
        let admin = env.admin.insecure_clone();
        let driver = env.driver;
        let bridge = env.bridge;
        let ixs = vec![
            env.ix_propose_image(&admin.pubkey(), executor_image(), ImageKind::Executor),
            env.ix_propose_image(&admin.pubkey(), builder_image(), ImageKind::Builder),
            env.ix_propose_image(&admin.pubkey(), reviewer_image(), ImageKind::Reviewer),
            env.ix_propose_project(&admin.pubkey(), project()),
        ];
        expect_ok(env.send(ixs, &[&admin]));
        expect_ok(env.propose(ApprovalKind::Driver, name("local"), driver));
        expect_ok(env.propose(ApprovalKind::Bridge, name("local-bridge"), bridge));
        env.skip(TIMELOCK);
        let (builder, reviewer) = (env.builder.pubkey(), env.reviewer.pubkey());
        env.bless_service_key(builder_image(), builder);
        env.bless_service_key(reviewer_image(), reviewer);
        env
    }

    /// The programs deployed and the first checker created, and nothing set
    /// up: for tests of setting up.
    pub fn bare() -> Self {
        Self::bare_with_mint(None)
    }

    fn bare_with_mint(mint: Option<Pubkey>) -> Self {
        let mut svm = LiteSVM::new();
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = START as i64;
        svm.set_sysvar(&clock);

        let deployer = Keypair::new();
        let elf = so("shonin_dev_verifier");
        let mut env = Self {
            svm,
            payer: Keypair::new(),
            deployer,
            admin: Keypair::new(),
            attestor: Keypair::new(),
            checker: Pubkey::default(),
            mint: Pubkey::default(),
            mint_authority: Keypair::new(),
            platform: Keypair::new(),
            builder: Keypair::new(),
            reviewer: Keypair::new(),
            executor: Keypair::new(),
            author: Keypair::new(),
            alice: Keypair::new(),
            bob: Keypair::new(),
            operator: Keypair::new(),
            driver: Pubkey::default(),
            bridge: Pubkey::default(),
            elf,
            seeds: 0,
            nonces: Default::default(),
            requests: Default::default(),
        };
        for who in [
            env.payer.pubkey(),
            env.deployer.pubkey(),
            env.admin.pubkey(),
            env.mint_authority.pubkey(),
            env.platform.pubkey(),
            env.builder.pubkey(),
            env.reviewer.pubkey(),
            env.executor.pubkey(),
            env.author.pubkey(),
            env.alice.pubkey(),
            env.bob.pubkey(),
            env.operator.pubkey(),
        ] {
            env.svm.airdrop(&who, 1_000_000_000_000).unwrap();
        }
        let authority = Some(env.deployer.pubkey());
        for (id, file) in [
            (shonin_registry::ID, "shonin_registry"),
            (shonin_publishing::ID, "shonin_publishing"),
            (shonin_prices::ID, "shonin_prices"),
            (shonin_profiles::ID, "shonin_profiles"),
            (shonin_station::ID, "shonin_station"),
            (shonin_vault::ID, "shonin_vault"),
            (shonin_local_venue::ID, "shonin_local_venue"),
            (shonin_local_venue_2::ID, "shonin_local_venue_2"),
            (shonin_local_venue_3::ID, "shonin_local_venue_3"),
        ] {
            env.deploy_upgradeable(id, &so(file), authority);
        }
        let elf = env.elf.clone();
        env.svm.add_program(shonin_dev_verifier::ID, &elf).unwrap();
        env.svm
            .add_program(shonin_cs_verifier::ID, &so("shonin_cs_verifier"))
            .unwrap();
        // The drivers are final, as every driver must be; the vault stand-in
        // needs no setting up.
        for (id, file) in [
            (shonin_local_driver::ID, "shonin_local_driver"),
            (shonin_local_driver_2::ID, "shonin_local_driver_2"),
            (shonin_local_driver_3::ID, "shonin_local_driver_3"),
            (shonin_test_driver::ID, "shonin_test_driver"),
            (shonin_test_vault::ID, "shonin_test_vault"),
        ] {
            env.svm.add_program(id, &so(file)).unwrap();
        }
        let attestor = env.attestor.pubkey();
        env.checker = env.create_checker(attestor);
        match mint {
            Some(address) => env.place_mint(address),
            None => env.create_mint(),
        }
        env
    }

    // Deploying ---------------------------------------------------------

    /// Deploys `elf` at `program` under the upgradeable loader, with this
    /// upgrade authority (none makes it final).
    pub fn deploy_upgradeable(&mut self, program: Pubkey, elf: &[u8], authority: Option<Pubkey>) {
        let data_address = program_data_address(&program);
        let mut data = vec![3, 0, 0, 0];
        data.extend_from_slice(&0u64.to_le_bytes());
        match authority {
            Some(key) => {
                data.push(1);
                data.extend_from_slice(key.as_ref());
            }
            None => {
                data.push(0);
                data.extend_from_slice(&[0; 32]);
            }
        }
        data.extend_from_slice(elf);
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(
                data_address,
                Account {
                    lamports,
                    data,
                    owner: solana_sdk_ids::bpf_loader_upgradeable::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        let mut account = vec![2, 0, 0, 0];
        account.extend_from_slice(data_address.as_ref());
        let lamports = self.svm.minimum_balance_for_rent_exemption(account.len());
        self.svm
            .set_account(
                program,
                Account {
                    lamports,
                    data: account,
                    owner: solana_sdk_ids::bpf_loader_upgradeable::ID,
                    executable: true,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    }

    /// A stand-in program nobody can change, at a fresh address.
    pub fn final_program(&mut self) -> Pubkey {
        let id = Pubkey::new_unique();
        let elf = self.elf.clone();
        self.svm.add_program(id, &elf).unwrap();
        id
    }

    /// A stand-in program deployed upgradeable, with this authority, at a
    /// fresh address.
    pub fn upgradeable_program(&mut self, authority: Option<Pubkey>) -> Pubkey {
        let id = Pubkey::new_unique();
        let elf = self.elf.clone();
        self.deploy_upgradeable(id, &elf, authority);
        id
    }

    /// A development checker trusting `attestation_key`.
    pub fn create_checker(&mut self, attestation_key: Pubkey) -> Pubkey {
        self.seeds += 1;
        let seed = keccak(&[&self.seeds.to_le_bytes()]);
        let checker = Pubkey::find_program_address(
            &[
                shonin_dev_verifier::CHECKER_SEED,
                attestation_key.as_ref(),
                &seed,
            ],
            &shonin_dev_verifier::ID,
        )
        .0;
        let payer = self.payer.insecure_clone();
        let ix = Instruction {
            program_id: shonin_dev_verifier::ID,
            accounts: shonin_dev_verifier::accounts::CreateChecker {
                payer: payer.pubkey(),
                checker,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_dev_verifier::instruction::CreateChecker {
                attestation_key,
                seed,
                cluster_name: CLUSTER.to_string(),
            }
            .data(),
        };
        expect_ok(self.send(vec![ix], &[&payer]));
        checker
    }

    /// A Confidential Space checker for the guest `image_id`, trusting the
    /// Google keys `keys` (sorted here, as a checker takes them).
    pub fn create_cs_checker(&mut self, image_id: [u8; 32], keys: &[[u8; 32]]) -> Pubkey {
        let mut keys = keys.to_vec();
        keys.sort();
        let ix = self.ix_create_cs_checker(image_id, keys.clone());
        let payer = self.payer.insecure_clone();
        expect_ok(self.send(vec![ix], &[&payer]));
        shonin_cs_verifier::checker_address(&image_id, &keys)
    }

    /// The instruction creating a Confidential Space checker, the keys as
    /// given.
    pub fn ix_create_cs_checker(&self, image_id: [u8; 32], keys: Vec<[u8; 32]>) -> Instruction {
        let checker = Pubkey::find_program_address(
            &[
                shonin_cs_verifier::CHECKER_SEED,
                &image_id,
                &shonin_cs_verifier::keys_digest(&keys),
            ],
            &shonin_cs_verifier::ID,
        )
        .0;
        Instruction {
            program_id: shonin_cs_verifier::ID,
            accounts: shonin_cs_verifier::accounts::CreateChecker {
                payer: self.payer.pubkey(),
                checker,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_cs_verifier::instruction::CreateChecker {
                image_id,
                keys: shonin_cs_verifier::keys_digest(&keys),
                trusted_keys: keys,
            }
            .data(),
        }
    }

    /// Proposes the Confidential Space checker `state`.
    pub fn propose_cs_checker(&mut self, state: Pubkey) -> TxResult {
        let ix =
            self.ix_propose_verifier(&self.admin.pubkey(), shonin_cs_verifier::ID, None, state);
        self.as_admin(ix)
    }

    // Time ----------------------------------------------------------------

    pub fn now(&self) -> u64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp as u64
    }

    pub fn warp(&mut self, time: u64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp = time as i64;
        self.svm.set_sysvar(&clock);
    }

    pub fn skip(&mut self, seconds: u64) {
        let now = self.now();
        self.warp(now + seconds);
    }

    // Transactions --------------------------------------------------------

    /// Sends `ixs` in one transaction, the first signer paying. Every
    /// transaction is checked to fit the wire, as a real one must. One that
    /// calls a station, directly or through the vault stand-in, asks for the
    /// heap the station's callers ask for.
    pub fn send(&mut self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> TxResult {
        self.svm.expire_blockhash();
        let tx = self.by_address(ixs, signers);
        let size = bincode::serialize(&tx).unwrap().len();
        assert!(
            size <= PACKET_DATA_SIZE,
            "a transaction of {size} bytes is over the wire's limit"
        );
        self.svm.send_transaction(tx)
    }

    /// Sends `ixs` as [`Env::send`] does when they fit the wire with every
    /// account named by address, and otherwise as
    /// [`Env::send_through_table`] does, every account they name but the
    /// signers in the table: how a transaction too large for the wire is
    /// sent.
    pub fn send_fitting(&mut self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> TxResult {
        let size = bincode::serialize(&self.by_address(ixs.clone(), signers))
            .unwrap()
            .len();
        if size <= PACKET_DATA_SIZE {
            return self.send(ixs, signers);
        }
        let mut table: Vec<Pubkey> = Vec::new();
        for meta in ixs.iter().flat_map(|ix| ix.accounts.iter()) {
            let signs = signers.iter().any(|signer| signer.pubkey() == meta.pubkey);
            if !signs && !table.contains(&meta.pubkey) {
                table.push(meta.pubkey);
            }
        }
        self.send_through_table(ixs, signers, &table)
    }

    /// The bytes `ixs` take on the wire as one transaction naming every
    /// account by address, signed by `signers`.
    pub fn wire_size(&self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> usize {
        bincode::serialize(&self.by_address(ixs, signers))
            .unwrap()
            .len()
    }

    /// `ixs` as one transaction naming every account by address, behind the
    /// compute (and, for a station's caller, the heap) `send` asks for.
    fn by_address(&self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> Transaction {
        let mut all = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
        if ixs.iter().any(|ix| {
            [shonin_station::ID, shonin_test_vault::ID, shonin_vault::ID].contains(&ix.program_id)
        }) {
            all.push(ComputeBudgetInstruction::request_heap_frame(
                shonin_station::heap::MAX_HEAP as u32,
            ));
        }
        all.extend(ixs);
        let payer = signers[0].pubkey();
        let mut unique: Vec<&Keypair> = Vec::new();
        for signer in signers {
            if !unique.iter().any(|seen| seen.pubkey() == signer.pubkey()) {
                unique.push(signer);
            }
        }
        Transaction::new(
            &unique,
            Message::new(&all, Some(&payer)),
            self.svm.latest_blockhash(),
        )
    }

    /// Sends `ixs` as a version-0 transaction whose accounts, other than the
    /// signers and the programs it calls itself, are found through an
    /// address lookup table holding `table`: how a transaction that names
    /// more accounts than the wire carries is sent. Asks for the compute and
    /// heap `send` asks a station's callers for, and is checked to fit the
    /// wire and the most accounts one transaction may lock.
    pub fn send_through_table(
        &mut self,
        ixs: Vec<Instruction>,
        signers: &[&Keypair],
        table: &[Pubkey],
    ) -> TxResult {
        self.svm.expire_blockhash();
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::request_heap_frame(shonin_station::heap::MAX_HEAP as u32),
        ];
        all.extend(ixs);
        let tx = self.through_table(&all, signers, table);
        self.svm.send_transaction(tx)
    }

    /// `ixs` as a version-0 transaction, the first signer paying, whose
    /// accounts other than the signers and the programs it calls are found
    /// through a lookup table holding `table`, which is set up first.
    /// Checked to fit the wire and the most accounts one transaction may
    /// lock, as a cluster checks it before it runs.
    fn through_table(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
        table: &[Pubkey],
    ) -> solana_transaction::versioned::VersionedTransaction {
        use solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta};
        use solana_message::{v0, AddressLookupTableAccount, VersionedMessage};
        use solana_transaction::versioned::VersionedTransaction;

        let key = Pubkey::new_unique();
        let state = AddressLookupTable {
            meta: LookupTableMeta {
                // Every address counts as added before this slot.
                last_extended_slot_start_index: table.len() as u8,
                ..LookupTableMeta::default()
            },
            addresses: std::borrow::Cow::Borrowed(table),
        };
        let data = state.serialize_for_tests().unwrap();
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(
                key,
                Account {
                    lamports,
                    data,
                    owner: solana_sdk_ids::address_lookup_table::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        let lookup = AddressLookupTableAccount {
            key,
            addresses: table.to_vec(),
        };
        let message = v0::Message::try_compile(
            &signers[0].pubkey(),
            ixs,
            &[lookup],
            self.svm.latest_blockhash(),
        )
        .unwrap();
        let locked = message.account_keys.len()
            + message
                .address_table_lookups
                .iter()
                .map(|l| l.writable_indexes.len() + l.readonly_indexes.len())
                .sum::<usize>();
        assert!(
            locked <= MAX_TX_ACCOUNTS,
            "a transaction locking {locked} accounts is over the limit"
        );
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), signers).unwrap();
        let size = bincode::serialize(&tx).unwrap().len();
        assert!(
            size <= PACKET_DATA_SIZE,
            "a transaction of {size} bytes is over the wire's limit"
        );
        tx
    }

    /// Runs `ixs` without keeping anything, for what they return. Like
    /// every transaction, it must fit the wire: one that doesn't with every
    /// account named by address names them through a lookup table, as a
    /// caller asking a cluster would, and the table is all it leaves
    /// behind. (LiteSVM doesn't hold a transaction to the wire's limit, and
    /// one over it can call a program at an index a cluster could never
    /// receive, which LiteSVM panics on.)
    pub fn simulate(&mut self, ixs: Vec<Instruction>) -> TransactionMetadata {
        self.svm.expire_blockhash();
        let payer = self.payer.insecure_clone();
        let tx = Transaction::new(
            &[&payer],
            Message::new(&ixs, Some(&payer.pubkey())),
            self.svm.latest_blockhash(),
        );
        let result = if bincode::serialize(&tx).unwrap().len() <= PACKET_DATA_SIZE {
            self.svm.simulate_transaction(tx)
        } else {
            let mut table: Vec<Pubkey> = Vec::new();
            for meta in ixs.iter().flat_map(|ix| ix.accounts.iter()) {
                if !meta.is_signer && meta.pubkey != payer.pubkey() && !table.contains(&meta.pubkey)
                {
                    table.push(meta.pubkey);
                }
            }
            let tx = self.through_table(&ixs, &[&payer], &table);
            self.svm.simulate_transaction(tx)
        };
        match result {
            Ok(info) => info.meta,
            Err(failed) => panic!(
                "simulation failed: {:?}\n{}",
                failed.err,
                failed.meta.logs.join("\n")
            ),
        }
    }

    /// An Ed25519 instruction verifying `signer`'s signature over `message`.
    pub fn ed25519(signer: &Keypair, message: &[u8]) -> Instruction {
        let signature = signer.sign_message(message);
        let signature: [u8; 64] = signature.as_ref().try_into().unwrap();
        solana_ed25519_program::new_ed25519_instruction_with_signature(
            message,
            &signature,
            &signer.pubkey().to_bytes(),
        )
    }

    // Accounts ------------------------------------------------------------

    pub fn load<T: AccountDeserialize>(&self, key: &Pubkey) -> Option<T> {
        let account = self.svm.get_account(key)?;
        if account.data.is_empty() {
            return None;
        }
        T::try_deserialize(&mut &account.data[..]).ok()
    }

    /// Runs `f` over the accounts at `keys` as a program would see them.
    pub fn with_accounts<R>(&self, keys: &[Pubkey], f: impl FnOnce(&[AccountInfo]) -> R) -> R {
        let mut stored: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> = keys
            .iter()
            .map(|key| {
                let account = self.svm.get_account(key).unwrap_or_default();
                (*key, account.lamports, account.data, account.owner)
            })
            .collect();
        let infos: Vec<AccountInfo> = stored
            .iter_mut()
            .map(|(key, lamports, data, owner)| {
                AccountInfo::new(key, false, false, lamports, data, owner, false, 0)
            })
            .collect();
        f(&infos)
    }

    pub fn lamports(&self, key: &Pubkey) -> u64 {
        self.svm
            .get_account(key)
            .map_or(0, |account| account.lamports)
    }

    // The registry --------------------------------------------------------

    pub fn initialize_registry(&mut self, timelock: u64, min_timelock: u64) {
        let deployer = self.deployer.insecure_clone();
        let ix = self.ix_initialize_registry(
            &deployer.pubkey(),
            shonin_registry::InitializeArgs {
                admin: self.admin.pubkey(),
                cluster_name: CLUSTER.to_string(),
                timelock,
                min_timelock,
                blessing_ttl: BLESSING_TTL,
            },
            shonin_dev_verifier::ID,
            None,
            self.checker,
        );
        expect_ok(self.send(vec![ix], &[&deployer]));
    }

    pub fn ix_initialize_registry(
        &self,
        deployer: &Pubkey,
        args: shonin_registry::InitializeArgs,
        verifier_program: Pubkey,
        verifier_program_data: Option<Pubkey>,
        verifier_state: Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Initialize {
                deployer: *deployer,
                config: registry::config_address(),
                program: shonin_registry::ID,
                program_data: program_data_address(&shonin_registry::ID),
                verifier_program,
                verifier_program_data,
                verifier_state,
                verifier_record: registry::verifier_address(&verifier_state),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::Initialize { args }.data(),
        }
    }

    fn admin_metas(&self, admin: &Pubkey) -> Vec<AccountMeta> {
        shonin_registry::accounts::Admin {
            admin: *admin,
            config: registry::config_address(),
        }
        .to_account_metas(None)
    }

    pub fn ix_set_timelock(&self, admin: &Pubkey, new_timelock: u64) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: self.admin_metas(admin),
            data: shonin_registry::instruction::SetTimelock { new_timelock }.data(),
        }
    }

    pub fn ix_retire_verifier(&self, admin: &Pubkey, state: Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: self.admin_metas(admin),
            data: shonin_registry::instruction::RetireVerifier { state }.data(),
        }
    }

    pub fn ix_propose_image(
        &self,
        admin: &Pubkey,
        digest: [u8; 32],
        kind: ImageKind,
    ) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ImageChange {
                admin: *admin,
                config: registry::config_address(),
                image: registry::image_address(&digest),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeImage {
                image_digest: digest,
                kind,
            }
            .data(),
        }
    }

    pub fn ix_revoke_image(&self, admin: &Pubkey, digest: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ImageChange {
                admin: *admin,
                config: registry::config_address(),
                image: registry::image_address(&digest),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::RevokeImage {
                image_digest: digest,
            }
            .data(),
        }
    }

    pub fn ix_propose_project(&self, admin: &Pubkey, id: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ProjectChange {
                admin: *admin,
                config: registry::config_address(),
                project: registry::project_address(&id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeProject { project_id: id }.data(),
        }
    }

    pub fn ix_revoke_project(&self, admin: &Pubkey, id: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ProjectChange {
                admin: *admin,
                config: registry::config_address(),
                project: registry::project_address(&id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::RevokeProject { project_id: id }.data(),
        }
    }

    pub fn ix_propose_verifier(
        &self,
        admin: &Pubkey,
        program: Pubkey,
        program_data: Option<Pubkey>,
        state: Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ProposeVerifier {
                admin: *admin,
                config: registry::config_address(),
                verifier_program: program,
                verifier_program_data: program_data,
                verifier_state: state,
                verifier_record: registry::verifier_address(&state),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeVerifier {}.data(),
        }
    }

    /// The revocation record a change to `name` settles, if it settles one.
    fn settling(&self, kind: ApprovalKind, name: &[u8; 32]) -> Option<Pubkey> {
        let slot: shonin_registry::state::ApprovalSlot =
            self.load(&registry::approval_address(kind, name))?;
        slot.next_is_ready(self.now())
            .then(|| registry::revoked_address(kind, name, &slot.next))
    }

    pub fn ix_propose(
        &self,
        admin: &Pubkey,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
        implementation_data: Option<Pubkey>,
    ) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Propose {
                admin: *admin,
                config: registry::config_address(),
                slot: registry::approval_address(kind, &name),
                next_revoked: self.settling(kind, &name),
                implementation_program: implementation,
                implementation_data,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::Propose {
                kind,
                name,
                implementation,
            }
            .data(),
        }
    }

    pub fn ix_withdraw(&self, admin: &Pubkey, kind: ApprovalKind, name: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::WithdrawProposal {
                admin: *admin,
                config: registry::config_address(),
                slot: registry::approval_address(kind, &name),
                next_revoked: self.settling(kind, &name),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::WithdrawProposal { kind, name }.data(),
        }
    }

    pub fn ix_revoke(
        &self,
        admin: &Pubkey,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> Instruction {
        let settling = self
            .settling(kind, &name)
            .filter(|record| *record != registry::revoked_address(kind, &name, &implementation));
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Revoke {
                admin: *admin,
                config: registry::config_address(),
                slot: registry::approval_address(kind, &name),
                revoked: registry::revoked_address(kind, &name, &implementation),
                next_revoked: settling,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::Revoke {
                kind,
                name,
                implementation,
            }
            .data(),
        }
    }

    /// As the admin.
    pub fn as_admin(&mut self, ix: Instruction) -> TxResult {
        let admin = self.admin.insecure_clone();
        self.send(vec![ix], &[&admin])
    }

    pub fn propose(
        &mut self,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> TxResult {
        let ix = self.ix_propose(&self.admin.pubkey(), kind, name, implementation, None);
        self.as_admin(ix)
    }

    pub fn withdraw(&mut self, kind: ApprovalKind, name: [u8; 32]) -> TxResult {
        let ix = self.ix_withdraw(&self.admin.pubkey(), kind, name);
        self.as_admin(ix)
    }

    pub fn revoke(
        &mut self,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> TxResult {
        let ix = self.ix_revoke(&self.admin.pubkey(), kind, name, implementation);
        self.as_admin(ix)
    }

    pub fn propose_image(&mut self, digest: [u8; 32], kind: ImageKind) -> TxResult {
        let ix = self.ix_propose_image(&self.admin.pubkey(), digest, kind);
        self.as_admin(ix)
    }

    pub fn revoke_image(&mut self, digest: [u8; 32]) -> TxResult {
        let ix = self.ix_revoke_image(&self.admin.pubkey(), digest);
        self.as_admin(ix)
    }

    pub fn propose_project(&mut self, id: [u8; 32]) -> TxResult {
        let ix = self.ix_propose_project(&self.admin.pubkey(), id);
        self.as_admin(ix)
    }

    pub fn revoke_project(&mut self, id: [u8; 32]) -> TxResult {
        let ix = self.ix_revoke_project(&self.admin.pubkey(), id);
        self.as_admin(ix)
    }

    /// Proposes a development checker trusting `attestation_key`, newly
    /// created; returns its state account.
    pub fn propose_checker(&mut self, attestation_key: Pubkey) -> Pubkey {
        let state = self.create_checker(attestation_key);
        let ix =
            self.ix_propose_verifier(&self.admin.pubkey(), shonin_dev_verifier::ID, None, state);
        expect_ok(self.as_admin(ix));
        state
    }

    pub fn retire_verifier(&mut self, state: Pubkey) -> TxResult {
        let ix = self.ix_retire_verifier(&self.admin.pubkey(), state);
        self.as_admin(ix)
    }

    pub fn set_timelock(&mut self, value: u64) -> TxResult {
        let ix = self.ix_set_timelock(&self.admin.pubkey(), value);
        self.as_admin(ix)
    }

    // Reading the registry ------------------------------------------------

    pub fn config(&self) -> shonin_registry::state::Config {
        self.load(&registry::config_address())
            .expect("the registry is set up")
    }

    pub fn cluster(&self) -> [u8; 32] {
        cluster_value(CLUSTER)
    }

    pub fn timelock(&self) -> u64 {
        self.config().timelock(self.now())
    }

    pub fn pending_timelock(&self) -> (u64, u64) {
        self.config().pending_timelock(self.now())
    }

    pub fn is_image_allowed(&self, digest: [u8; 32]) -> bool {
        let record = self.load(&registry::image_address(&digest));
        shonin_registry::state::is_image_allowed(record.as_ref(), self.now())
    }

    pub fn image_of(&self, digest: [u8; 32]) -> Option<shonin_registry::state::ImageRecord> {
        self.load(&registry::image_address(&digest))
    }

    pub fn is_project_allowed(&self, id: [u8; 32]) -> bool {
        let record = self.load(&registry::project_address(&id));
        shonin_registry::state::is_project_allowed(record.as_ref(), self.now())
    }

    pub fn is_verifier_allowed(&self, state: &Pubkey) -> bool {
        self.config().is_verifier_allowed(state, self.now())
    }

    pub fn key_info(&self, signer: &Pubkey) -> Option<KeyInfo> {
        self.load(&registry::key_address(signer))
    }

    /// Whether `signer` is blessed as `kind`, read as another program reads
    /// it.
    pub fn is_blessed_as(&self, signer: &Pubkey, kind: ImageKind) -> bool {
        let (image, project) = self
            .key_info(signer)
            .map(|key| {
                (
                    registry::image_address(&key.image_digest),
                    registry::project_address(&key.project_id),
                )
            })
            .unwrap_or_default();
        let keys = [
            registry::config_address(),
            registry::key_address(signer),
            image,
            project,
        ];
        let now = self.now();
        self.with_accounts(&keys, |a| {
            registry::blessed_as(&a[0], &a[1], &a[2], &a[3], signer, kind, now).unwrap()
        })
    }

    /// Whether `signer` is blessed for this vault and bundle on this vault
    /// chain, read as another program reads it.
    pub fn is_blessed_for(
        &self,
        signer: &Pubkey,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
    ) -> bool {
        let (image, project) = self
            .key_info(signer)
            .map(|key| {
                (
                    registry::image_address(&key.image_digest),
                    registry::project_address(&key.project_id),
                )
            })
            .unwrap_or_default();
        let keys = [
            registry::config_address(),
            registry::blessing_address(signer, &vault_chain, &vault, &strategy_hash),
            registry::key_address(signer),
            image,
            project,
        ];
        let now = self.now();
        self.with_accounts(&keys, |a| {
            registry::blessed_for(
                &a[0],
                &a[1],
                &a[2],
                &a[3],
                &a[4],
                signer,
                &vault_chain,
                &vault,
                &strategy_hash,
                now,
            )
            .unwrap()
        })
    }

    pub fn blessed_until(
        &self,
        signer: &Pubkey,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
    ) -> u64 {
        let keys = [registry::blessing_address(
            signer,
            &vault_chain,
            &vault,
            &strategy_hash,
        )];
        self.with_accounts(&keys, |a| {
            registry::blessed_until(&a[0], signer, &vault_chain, &vault, &strategy_hash).unwrap()
        })
    }

    /// What a station created now would pin for this name.
    pub fn trading(&self, kind: ApprovalKind, name: [u8; 32]) -> Pubkey {
        let slot_address = registry::approval_address(kind, &name);
        let effective = self
            .load::<shonin_registry::state::ApprovalSlot>(&slot_address)
            .map(|slot| slot.effective(self.now()))
            .unwrap_or_default();
        let keys = [
            slot_address,
            registry::revoked_address(kind, &name, &effective),
        ];
        let now = self.now();
        self.with_accounts(&keys, |a| {
            registry::trading(kind, &name, &a[0], &a[1], now).unwrap()
        })
    }

    pub fn trading_driver(&self, name: [u8; 32]) -> Pubkey {
        self.trading(ApprovalKind::Driver, name)
    }

    pub fn bridge_of(&self, name: [u8; 32]) -> Pubkey {
        self.trading(ApprovalKind::Bridge, name)
    }

    pub fn is_revoked(&self, kind: ApprovalKind, name: [u8; 32], implementation: Pubkey) -> bool {
        let keys = [
            registry::approval_address(kind, &name),
            registry::revoked_address(kind, &name, &implementation),
        ];
        let now = self.now();
        self.with_accounts(&keys, |a| {
            registry::revoked(kind, &name, &implementation, &a[0], &a[1], now).unwrap()
        })
    }

    // Blessing ------------------------------------------------------------

    /// What an instance attests to, attested now, in the fixture's project.
    pub fn journal_for(&self, image: [u8; 32], signer: Pubkey, vaults_root: [u8; 32]) -> Journal {
        Journal {
            image_digest: image,
            project_id: project(),
            signer,
            vaults_root,
            issued_at: self.now(),
        }
    }

    /// The leaf a builder or a reviewer commits to for its `key`: no vault,
    /// on this cluster.
    pub fn service_leaf(&self, key: Pubkey) -> [u8; 32] {
        blessing_leaf(
            &[0; 32],
            &[0; 32],
            &[0; 32],
            &self.cluster(),
            &key.to_bytes(),
        )
    }

    /// The leaf an instance commits to for its `key`, for one vault on this
    /// cluster and one bundle.
    pub fn leaf_for(&self, vault: [u8; 32], strategy_hash: [u8; 32], key: Pubkey) -> [u8; 32] {
        blessing_leaf(
            &self.cluster(),
            &vault,
            &strategy_hash,
            &self.cluster(),
            &key.to_bytes(),
        )
    }

    /// The seal `attestor` puts on `journal` for the development checker:
    /// nothing in the seal argument, and the Ed25519 instruction carrying its
    /// signature over what the checker checks.
    pub fn seal(&self, attestor: &Keypair, journal: &Journal) -> (Vec<u8>, Instruction) {
        let bytes = borsh::to_vec(journal).unwrap();
        let message = shonin_dev_verifier::attestation_digest(&self.cluster(), &bytes);
        (Vec::new(), Self::ed25519(attestor, &message))
    }

    /// The bless instruction for the instance's own key (the journal's
    /// signer, named in its leaf), with every allowed checker's accounts,
    /// newest first, as a relayer would send it.
    #[allow(clippy::too_many_arguments)]
    pub fn ix_bless(
        &self,
        payer: &Pubkey,
        journal: &Journal,
        seal: Vec<u8>,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        merkle_proof: Vec<[u8; 32]>,
    ) -> Instruction {
        self.ix_bless_key(
            payer,
            journal,
            seal,
            vault_chain,
            vault,
            strategy_hash,
            journal.signer,
            merkle_proof,
            3,
        )
    }

    /// The bless instruction for `key`, with every allowed checker's
    /// accounts, newest first: a Confidential Space checker its program and
    /// its state, and any other `dev_accounts` accounts (its program, its
    /// state and the instructions sysvar).
    #[allow(clippy::too_many_arguments)]
    pub fn ix_bless_key(
        &self,
        payer: &Pubkey,
        journal: &Journal,
        seal: Vec<u8>,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        key: Pubkey,
        merkle_proof: Vec<[u8; 32]>,
        dev_accounts: u8,
    ) -> Instruction {
        let config = self.config();
        let now = self.now();
        let mut groups = Vec::new();
        let mut remaining = Vec::new();
        for entry in config.verifiers.iter().rev() {
            if now < entry.ready_at {
                continue;
            }
            remaining.push(AccountMeta::new_readonly(entry.program, false));
            remaining.push(AccountMeta::new_readonly(entry.state, false));
            if entry.program == shonin_cs_verifier::ID {
                groups.push(2);
            } else {
                groups.push(dev_accounts);
                if dev_accounts > 2 {
                    remaining.push(AccountMeta::new_readonly(
                        anchor_lang::solana_program::sysvar::instructions::ID,
                        false,
                    ));
                }
            }
        }
        let mut accounts = shonin_registry::accounts::Bless {
            payer: *payer,
            config: registry::config_address(),
            image: registry::image_address(&journal.image_digest),
            project: registry::project_address(&journal.project_id),
            key_info: registry::key_address(&key),
            blessing: registry::blessing_address(&key, &vault_chain, &vault, &strategy_hash),
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None);
        accounts.extend(remaining);
        Instruction {
            program_id: shonin_registry::ID,
            accounts,
            data: shonin_registry::instruction::Bless {
                journal: *journal,
                seal,
                vault_chain,
                vault,
                strategy_hash,
                key,
                merkle_proof,
                verifier_accounts: groups,
            }
            .data(),
        }
    }

    /// Blesses `journal` sealed by `attestor`, sent by `sender`.
    #[allow(clippy::too_many_arguments)]
    pub fn bless_by(
        &mut self,
        sender: &Keypair,
        attestor: &Keypair,
        journal: &Journal,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        merkle_proof: Vec<[u8; 32]>,
    ) -> TxResult {
        let (seal, ed25519) = self.seal(attestor, journal);
        let bless = self.ix_bless(
            &sender.pubkey(),
            journal,
            seal,
            vault_chain,
            vault,
            strategy_hash,
            merkle_proof,
        );
        self.send(vec![ed25519, bless], &[sender])
    }

    /// Blesses `journal` sealed by the fixture's attestor.
    pub fn bless(
        &mut self,
        journal: &Journal,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        merkle_proof: Vec<[u8; 32]>,
    ) -> TxResult {
        let (payer, attestor) = (self.payer.insecure_clone(), self.attestor.insecure_clone());
        self.bless_by(
            &payer,
            &attestor,
            journal,
            vault_chain,
            vault,
            strategy_hash,
            merkle_proof,
        )
    }

    /// A builder or a reviewer key: it serves every strategy and no vault.
    pub fn bless_service_key(&mut self, image: [u8; 32], signer: Pubkey) {
        let journal = self.journal_for(image, signer, self.service_leaf(signer));
        expect_ok(self.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]));
    }

    /// An executor key: it may act for one vault on this cluster running one
    /// bundle.
    pub fn bless_executor_key(&mut self, signer: Pubkey, vault: [u8; 32], strategy_hash: [u8; 32]) {
        let journal = self.journal_for(
            executor_image(),
            signer,
            self.leaf_for(vault, strategy_hash, signer),
        );
        let cluster = self.cluster();
        expect_ok(self.bless(&journal, cluster, vault, strategy_hash, vec![]));
    }

    // The dollar ----------------------------------------------------------

    fn create_mint(&mut self) {
        let mint = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(82);
        let payer = self.payer.insecure_clone();
        let create = anchor_lang::solana_program::system_instruction::create_account(
            &payer.pubkey(),
            &mint.pubkey(),
            rent,
            82,
            &spl_token::ID,
        );
        let init = spl_token::instruction::initialize_mint2(
            &spl_token::ID,
            &mint.pubkey(),
            &self.mint_authority.pubkey(),
            None,
            6,
        )
        .unwrap();
        expect_ok(self.send(vec![create, init], &[&payer, &mint]));
        self.mint = mint.pubkey();
        let platform = self.platform.pubkey();
        self.token_account(&platform);
    }

    /// The dollar's mint, placed at `address` with the fixture's mint
    /// authority: a mint nobody holds the key of, which only a test can put
    /// there.
    fn place_mint(&mut self, address: Pubkey) {
        use anchor_lang::solana_program::program_pack::Pack;
        let mint = spl_token::state::Mint {
            mint_authority: Some(self.mint_authority.pubkey()).into(),
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: None.into(),
        };
        let mut data = vec![0; spl_token::state::Mint::LEN];
        mint.pack_into_slice(&mut data);
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(
                address,
                Account {
                    lamports,
                    data,
                    owner: spl_token::ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        self.mint = address;
        let platform = self.platform.pubkey();
        self.token_account(&platform);
    }

    /// `owner`'s associated account for the dollar, created if need be.
    pub fn token_account(&mut self, owner: &Pubkey) -> Pubkey {
        let payer = self.payer.insecure_clone();
        let ix = create_associated_token_account_idempotent(
            &payer.pubkey(),
            owner,
            &self.mint,
            &spl_token::ID,
        );
        expect_ok(self.send(vec![ix], &[&payer]));
        get_associated_token_address_with_program_id(owner, &self.mint, &spl_token::ID)
    }

    pub fn give(&mut self, owner: &Pubkey, amount: u64) {
        let account = self.token_account(owner);
        let authority = self.mint_authority.insecure_clone();
        let ix = spl_token::instruction::mint_to(
            &spl_token::ID,
            &self.mint,
            &account,
            &authority.pubkey(),
            &[],
            amount,
        )
        .unwrap();
        expect_ok(self.send(vec![ix], &[&authority]));
    }

    pub fn usd_balance(&self, owner: &Pubkey) -> u64 {
        let address =
            get_associated_token_address_with_program_id(owner, &self.mint, &spl_token::ID);
        self.svm
            .get_account(&address)
            .filter(|account| account.data.len() >= 72)
            .map_or(0, |account| {
                u64::from_le_bytes(account.data[64..72].try_into().unwrap())
            })
    }

    // Publishing ----------------------------------------------------------

    pub fn initialize_publishing(&mut self) {
        let deployer = self.deployer.insecure_clone();
        let ix =
            self.ix_initialize_publishing(&deployer.pubkey(), self.platform.pubkey(), PUBLISH_FEE);
        expect_ok(self.send(vec![ix], &[&deployer]));
    }

    pub fn ix_initialize_publishing(
        &self,
        deployer: &Pubkey,
        platform: Pubkey,
        fee: u64,
    ) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Initialize {
                deployer: *deployer,
                config: publishing_config(),
                program: shonin_publishing::ID,
                program_data: program_data_address(&shonin_publishing::ID),
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Initialize { platform, fee }.data(),
        }
    }

    pub fn publishing_config(&self) -> shonin_publishing::state::Config {
        self.load(&publishing_config())
            .expect("publishing is set up")
    }

    pub fn ix_submit(&self, author: &Pubkey, upload: [u8; 32]) -> Instruction {
        let next = self.publishing_config().count + 1;
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Submit {
                author: *author,
                config: publishing_config(),
                submission: submission_address(next),
                mint: self.mint,
                author_token: get_associated_token_address_with_program_id(
                    author,
                    &self.mint,
                    &spl_token::ID,
                ),
                platform_token: get_associated_token_address_with_program_id(
                    &self.platform.pubkey(),
                    &self.mint,
                    &spl_token::ID,
                ),
                token_program: spl_token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Submit { upload }.data(),
        }
    }

    /// Pays to publish, as `who`; returns the id.
    pub fn submit_as(&mut self, who: &Keypair) -> u64 {
        self.give(&who.pubkey(), PUBLISH_FEE);
        let count = self.publishing_config().count;
        let upload = keccak(&[b"upload", who.pubkey().as_ref(), &count.to_le_bytes()]);
        let ix = self.ix_submit(&who.pubkey(), upload);
        expect_ok(self.send(vec![ix], &[who]));
        count + 1
    }

    pub fn submission_of(&self, id: u64) -> Option<Submission> {
        self.load(&submission_address(id))
    }

    pub fn strategy_of(&self, id: u64) -> Option<Strategy> {
        self.load(&strategy_address(id))
    }

    pub fn review_of(&self, id: u64) -> Option<Review> {
        self.load(&review_address(id))
    }

    /// Whether strategy `id` has a review a vault can follow.
    pub fn has_review(&self, id: u64) -> bool {
        self.review_of(id).is_some_and(|review| review.is_review())
    }

    pub fn ix_stage(&self, stager: &Pubkey, registration: &Registration) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Stage {
                stager: *stager,
                draft: draft_address(registration.id, stager),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Stage {
                strategy_id: registration.id,
                author: registration.author,
                source_hash: registration.source_hash,
                bundle_hash: registration.bundle_hash,
                manifest_hash: registration.manifest_hash,
                lists: registration.lists.clone(),
            }
            .data(),
        }
    }

    /// The registry records a key's blessing is read from: its record, and
    /// the records of its image and its project.
    pub fn records_of(&self, signer: &Pubkey) -> [Pubkey; 3] {
        let (image, project) = self
            .key_info(signer)
            .map(|key| {
                (
                    registry::image_address(&key.image_digest),
                    registry::project_address(&key.project_id),
                )
            })
            .unwrap_or_default();
        [registry::key_address(signer), image, project]
    }

    pub fn ix_register(
        &self,
        payer: &Pubkey,
        stager: &Pubkey,
        id: u64,
        builder: &Pubkey,
        reviewer: &Pubkey,
    ) -> Instruction {
        let [builder_key, builder_image, builder_project] = self.records_of(builder);
        let [reviewer_key, reviewer_image, reviewer_project] = self.records_of(reviewer);
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Register {
                payer: *payer,
                stager: *stager,
                draft: draft_address(id, stager),
                submission: submission_address(id),
                strategy: strategy_address(id),
                registry_config: registry::config_address(),
                builder_key,
                builder_image,
                builder_project,
                reviewer_key,
                reviewer_image,
                reviewer_project,
                instructions: anchor_lang::solana_program::sysvar::instructions::ID,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Register {
                strategy_id: id,
                builder: *builder,
                reviewer: *reviewer,
            }
            .data(),
        }
    }

    /// What the builder signs for `registration`.
    pub fn registration_digest(&self, registration: &Registration) -> [u8; 32] {
        shonin_publishing::state::registration_digest(
            &self.cluster(),
            registration.id,
            &registration.author,
            &registration.source_hash,
            &registration.bundle_hash,
            &registration.manifest_hash,
            &registration.lists,
        )
    }

    /// What the reviewer signs to clear `source_hash` for strategy `id`.
    pub fn clearance_digest(&self, id: u64, source_hash: [u8; 32]) -> [u8; 32] {
        shonin_publishing::state::clearance_digest(&self.cluster(), id, &source_hash)
    }

    /// Stages `registration` and registers it, the builder having signed
    /// `builder_signed` and the reviewer `reviewer_signed` (`None`: no
    /// signature in the transaction at all). Staged by `payer`.
    pub fn register_signed(
        &mut self,
        registration: &Registration,
        builder: &Keypair,
        builder_signed: Option<[u8; 32]>,
        reviewer: &Keypair,
        reviewer_signed: Option<[u8; 32]>,
    ) -> TxResult {
        let payer = self.payer.insecure_clone();
        let stage = self.ix_stage(&payer.pubkey(), registration);
        expect_ok(self.send(vec![stage], &[&payer]));
        let mut ixs = Vec::new();
        if let Some(digest) = builder_signed {
            ixs.push(Self::ed25519(builder, &digest));
        }
        if let Some(digest) = reviewer_signed {
            ixs.push(Self::ed25519(reviewer, &digest));
        }
        ixs.push(self.ix_register(
            &payer.pubkey(),
            &payer.pubkey(),
            registration.id,
            &builder.pubkey(),
            &reviewer.pubkey(),
        ));
        let result = self.send(ixs, &[&payer]);
        if result.is_err() {
            self.discard(&payer, registration.id);
        }
        result
    }

    /// Registers `registration` the way the build service does: the
    /// reviewer's go-ahead, then the builder's signature over what it built.
    pub fn register_with(
        &mut self,
        registration: &Registration,
        builder: &Keypair,
        reviewer: &Keypair,
    ) -> TxResult {
        let digest = self.registration_digest(registration);
        let clearance = self.clearance_digest(registration.id, registration.source_hash);
        self.register_signed(
            registration,
            builder,
            Some(digest),
            reviewer,
            Some(clearance),
        )
    }

    /// Registers strategy `id` for `author` with `bundle_hash` and the
    /// fixture's lists, signed by the fixture's builder and reviewer.
    pub fn register(&mut self, id: u64, author: Pubkey, bundle_hash: [u8; 32]) {
        let registration = Registration::new(id, author, bundle_hash);
        let (builder, reviewer) = (
            self.builder.insecure_clone(),
            self.reviewer.insecure_clone(),
        );
        expect_ok(self.register_with(&registration, &builder, &reviewer));
    }

    pub fn discard(&mut self, stager: &Keypair, id: u64) {
        let ix = Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Discard {
                stager: stager.pubkey(),
                draft: draft_address(id, &stager.pubkey()),
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Discard { strategy_id: id }.data(),
        };
        let _ = self.send(vec![ix], &[stager]);
    }

    /// Submitted, reviewed and built: ready for its author to activate.
    pub fn ready(&mut self) -> (u64, [u8; 32]) {
        let author = self.author.insecure_clone();
        let id = self.submit_as(&author);
        let bundle = keccak(&[b"bundle", &id.to_le_bytes()]);
        self.register(id, author.pubkey(), bundle);
        let reviewer = self.reviewer.insecure_clone();
        expect_ok(self.submit_review(&reviewer, id, review_for(bundle, 1)));
        (id, bundle)
    }

    fn review_metas(&self, reviewer: &Pubkey) -> shonin_publishing::accounts::ReviewerAccounts {
        let [reviewer_key, reviewer_image, reviewer_project] = self.records_of(reviewer);
        shonin_publishing::accounts::ReviewerAccounts {
            reviewer: *reviewer,
            registry_config: registry::config_address(),
            reviewer_key,
            reviewer_image,
            reviewer_project,
        }
    }

    pub fn ix_submit_review(
        &self,
        payer: &Pubkey,
        reviewer: &Pubkey,
        id: u64,
        review: ReviewInput,
    ) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::SubmitReview {
                reviewer_accounts: self.review_metas(reviewer),
                payer: *payer,
                review: review_address(id),
                strategy: strategy_address(id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::SubmitReview {
                strategy_id: id,
                review,
            }
            .data(),
        }
    }

    pub fn ix_submit_refusal(
        &self,
        payer: &Pubkey,
        reviewer: &Pubkey,
        id: u64,
        review: ReviewInput,
    ) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::SubmitRefusal {
                reviewer_accounts: self.review_metas(reviewer),
                payer: *payer,
                review: review_address(id),
                submission: submission_address(id),
                strategy: strategy_address(id),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::SubmitRefusal {
                strategy_id: id,
                review,
            }
            .data(),
        }
    }

    /// `reviewer` posts the review of strategy `id`, the fixture paying.
    pub fn submit_review(&mut self, reviewer: &Keypair, id: u64, review: ReviewInput) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_submit_review(&payer.pubkey(), &reviewer.pubkey(), id, review);
        self.send(vec![ix], &[&payer, reviewer])
    }

    pub fn submit_refusal(&mut self, reviewer: &Keypair, id: u64, review: ReviewInput) -> TxResult {
        let payer = self.payer.insecure_clone();
        let ix = self.ix_submit_refusal(&payer.pubkey(), &reviewer.pubkey(), id, review);
        self.send(vec![ix], &[&payer, reviewer])
    }

    // Prices --------------------------------------------------------------

    pub fn initialize_prices(&mut self) {
        let deployer = self.deployer.insecure_clone();
        let ix = self.ix_initialize_prices(
            &deployer.pubkey(),
            self.admin.pubkey(),
            PRICE_NOTICE,
            published_prices(),
        );
        expect_ok(self.send(vec![ix], &[&deployer]));
    }

    pub fn ix_initialize_prices(
        &self,
        deployer: &Pubkey,
        admin: Pubkey,
        notice: u64,
        initial: UnitPrices,
    ) -> Instruction {
        Instruction {
            program_id: shonin_prices::ID,
            accounts: shonin_prices::accounts::Initialize {
                deployer: *deployer,
                prices: shonin_prices::read::prices_address(),
                program: shonin_prices::ID,
                program_data: program_data_address(&shonin_prices::ID),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_prices::instruction::Initialize {
                admin,
                notice,
                initial,
            }
            .data(),
        }
    }

    pub fn ix_schedule(&self, admin: &Pubkey, prices: UnitPrices) -> Instruction {
        Instruction {
            program_id: shonin_prices::ID,
            accounts: shonin_prices::accounts::Schedule {
                admin: *admin,
                prices: shonin_prices::read::prices_address(),
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_prices::instruction::Schedule { prices }.data(),
        }
    }

    pub fn schedule(&mut self, prices: UnitPrices) -> TxResult {
        let ix = self.ix_schedule(&self.admin.pubkey(), prices);
        self.as_admin(ix)
    }

    fn with_history<R>(&self, f: impl FnOnce(&shonin_prices::read::History) -> R) -> R {
        let key = shonin_prices::read::prices_address();
        let account = self.svm.get_account(&key).expect("prices are set up");
        let history =
            shonin_prices::read::History::new(&key, &account.owner, &account.data).unwrap();
        f(&history)
    }

    pub fn history(&self) -> Vec<PricePeriod> {
        self.with_history(|h| (0..h.len()).map(|i| h.period(i).unwrap()).collect())
    }

    pub fn current_prices(&self) -> UnitPrices {
        let now = self.now();
        self.with_history(|h| h.current(now).unwrap())
    }

    /// The cost, read from the account as a program reads it.
    pub fn cost_of(&self, allowances: &Allowances, from: u64, to: u64) -> u128 {
        self.with_history(|h| h.cost_of(allowances, from, to).unwrap().as_u128())
    }

    pub fn daily_cost(&self, allowances: &Allowances) -> u128 {
        let now = self.now();
        self.with_history(|h| h.daily_cost(allowances, now).unwrap().as_u128())
    }

    /// The cost as the program's view answers it, and the compute it took.
    pub fn cost_of_on_chain(&mut self, allowances: Allowances, from: u64, to: u64) -> (u128, u64) {
        let ix = Instruction {
            program_id: shonin_prices::ID,
            accounts: shonin_prices::accounts::View {
                prices: shonin_prices::read::prices_address(),
            }
            .to_account_metas(None),
            data: shonin_prices::instruction::CostOf {
                allowances,
                from,
                to,
            }
            .data(),
        };
        let meta = self.simulate(vec![ix]);
        let data = meta.return_data.data;
        assert_eq!(data.len(), 32);
        let value = shonin_common::math::U256::from_little_endian(&data);
        (value.as_u128(), meta.compute_units_consumed)
    }

    // Profiles ------------------------------------------------------------

    pub fn ix_set_profile(
        &self,
        author: &Pubkey,
        handle: &str,
        blurb: &str,
        labels: Vec<String>,
        urls: Vec<String>,
    ) -> Instruction {
        let old = self
            .profile_of(author)
            .filter(|profile| !profile.handle.is_empty() && profile.handle != handle)
            .map(|profile| shonin_profiles::handle_address(&profile.handle));
        Instruction {
            program_id: shonin_profiles::ID,
            accounts: shonin_profiles::accounts::SetProfile {
                author: *author,
                profile: profile_address(author),
                new_handle: shonin_profiles::handle_address(handle),
                old_handle: old,
                system_program: solana_sdk_ids::system_program::ID,
            }
            .to_account_metas(None),
            data: shonin_profiles::instruction::SetProfile {
                handle: handle.to_string(),
                handle_hash: keccak(&[handle.as_bytes()]),
                blurb: blurb.to_string(),
                link_labels: labels,
                link_urls: urls,
            }
            .data(),
        }
    }

    pub fn set_profile(
        &mut self,
        author: &Keypair,
        handle: &str,
        blurb: &str,
        labels: Vec<String>,
        urls: Vec<String>,
    ) -> TxResult {
        let ix = self.ix_set_profile(&author.pubkey(), handle, blurb, labels, urls);
        self.send(vec![ix], &[author])
    }

    pub fn profile_of(&self, author: &Pubkey) -> Option<shonin_profiles::Profile> {
        self.load(&profile_address(author))
    }

    pub fn author_of_handle(&self, handle: &str) -> Pubkey {
        self.load::<shonin_profiles::HandleRecord>(&shonin_profiles::handle_address(handle))
            .map_or(Pubkey::default(), |record| record.owner)
    }
}

/// A registration as the build service sends it.
#[derive(Clone, Debug)]
pub struct Registration {
    pub id: u64,
    pub author: Pubkey,
    pub source_hash: [u8; 32],
    pub bundle_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
    pub lists: Lists,
}

impl Registration {
    /// The fixture's registration: one chain with the local driver, no
    /// bridges, and the baseline restricted list.
    pub fn new(id: u64, author: Pubkey, bundle_hash: [u8; 32]) -> Self {
        Self {
            id,
            author,
            source_hash: hash("source"),
            bundle_hash,
            manifest_hash: hash("manifest"),
            lists: Lists {
                chains: one_chain(),
                bridges: vec![],
                restricted: vec![name("SANCTIONED"), name("US")],
            },
        }
    }
}

pub fn one_chain() -> Vec<ChainNeed> {
    vec![ChainNeed {
        chain: name("solana"),
        drivers: vec![name("local")],
    }]
}

pub fn review_for(bundle_hash: [u8; 32], rating: u8) -> ReviewInput {
    ReviewInput {
        bundle_hash,
        source_hash: hash("source"),
        model_id_hash: hash("model"),
        prompt_hash: hash("prompt"),
        report_hash: hash("report"),
        ratings: [rating; 6],
        finding_count: 2,
    }
}

/// The prices the Rust side publishes: $0.15 a core-hour, $0.01 a GiB-hour,
/// $0.15 a GB, and inference at the processor rate.
pub fn published_prices() -> UnitPrices {
    UnitPrices {
        vcpu_hour: 150_000_000_000_000_000,
        gib_hour: 10_000_000_000_000_000,
        gb_egress: 150_000_000_000_000_000,
        inference_second: 150_000_000_000_000_000 / 3600,
    }
}

/// Allowances whose day costs exactly `per_day`: all of it gas, which is
/// charged at face value.
pub fn allowances_costing(per_day: u64) -> Allowances {
    Allowances {
        gas_per_day: per_day,
        ..Allowances::default()
    }
}

pub fn publishing_config() -> Pubkey {
    Pubkey::find_program_address(
        &[shonin_publishing::state::CONFIG_SEED],
        &shonin_publishing::ID,
    )
    .0
}
pub fn submission_address(id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[shonin_publishing::state::SUBMISSION_SEED, &id.to_le_bytes()],
        &shonin_publishing::ID,
    )
    .0
}
pub fn strategy_address(id: u64) -> Pubkey {
    shonin_publishing::state::strategy_address(id)
}
pub fn review_address(id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[shonin_publishing::state::REVIEW_SEED, &id.to_le_bytes()],
        &shonin_publishing::ID,
    )
    .0
}
pub fn draft_address(id: u64, stager: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            shonin_publishing::state::DRAFT_SEED,
            &id.to_le_bytes(),
            stager.as_ref(),
        ],
        &shonin_publishing::ID,
    )
    .0
}
pub fn profile_address(author: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[shonin_profiles::PROFILE_SEED, author.as_ref()],
        &shonin_profiles::ID,
    )
    .0
}

/// The root of two leaves, and each one's proof.
pub fn pair(a: [u8; 32], b: [u8; 32]) -> ([u8; 32], Vec<[u8; 32]>, Vec<[u8; 32]>) {
    let root = if a <= b {
        keccak(&[&a, &b])
    } else {
        keccak(&[&b, &a])
    };
    (root, vec![b], vec![a])
}

/// A small deterministic generator for the property tests: the same seed
/// gives the same cases on every machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn word(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.word() % n
    }

    pub fn bool(&mut self) -> bool {
        self.word() & 1 == 1
    }

    /// True four times in five: for an input that is usually valid, so the
    /// path where everything holds is reached often.
    pub fn mostly(&mut self) -> bool {
        self.below(5) != 0
    }

    pub fn bytes(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.word().to_le_bytes());
        }
        out
    }
}

/// How many cases each property test runs: `SHONIN_PROPERTY_RUNS`, or a
/// quick default.
pub fn runs(default: u64) -> u64 {
    std::env::var("SHONIN_PROPERTY_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The seed property tests start from: `SHONIN_PROPERTY_SEED`, or a fixed
/// default so a plain run is reproducible.
pub fn seed() -> u64 {
    std::env::var("SHONIN_PROPERTY_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}
