//! Orca's real program at its real address, a pool of wrapped SOL against
//! the dollar created at Pyth's price, a stand-in for Pyth's SOL/USD account
//! at its sponsored address, and Orca's driver approved as `orca`: what a
//! station trading at Orca meets on devnet, in-process.
//!
//! Orca's program is the mainnet binary `task contracts:solana:fixtures`
//! fetched, checked against the hash `fixtures/programs.txt` pins before it
//! is loaded. Orca's settings account and the pool's fee tier are written
//! directly, as their owner's program lays them out, since only Orca's own
//! keys can create them; the pool, its tick arrays and every position are
//! made by Orca's program itself.
//!
//! Someone other than the station (`bob`) provides the pool's depth, so the
//! station's sales have a market to sell into.

use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::associated_token::{
    get_associated_token_address, get_associated_token_address_with_program_id,
};
use anchor_spl::token::spl_token;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub use shonin_orca_driver::logic::{Book, Held};
pub use shonin_orca_driver::math::{self as orca_math, U256};
pub use shonin_orca_driver::network::{
    ASSET_MINT, CONFIG, DOLLAR_MINT, FEED_ID, POOLS, PRICE_ACCOUNT, PYTH_RECEIVER,
    WHIRLPOOL_PROGRAM,
};
pub use shonin_orca_driver::{DriverError as OrcaError, MAX_SLOTS};

use crate::station::*;
use crate::*;

pub const TOKEN_2022: Pubkey = shonin_orca_driver::TOKEN_2022_PROGRAM;
/// The account Orca's program takes as the authority over a position NFT's
/// metadata.
pub const METADATA_UPDATE_AUTH: Pubkey =
    anchor_lang::prelude::pubkey!("3axbTs2z5GBy6usVbNVoqEgZMng3vZvMnAoX29BFfwhr");
/// The listed pool's tick spacing, and the ticks one tick array spans.
pub const SPACING: i32 = 64;
pub const SPAN: i32 = 88 * SPACING;
/// The pool's fee, in millionths, as devnet's tier at this spacing has it.
pub const FEE_RATE: u16 = 2_000;
/// SOL's price at the start, in Pyth's units (eight decimals): $150.
pub const SOL_PRICE: i64 = 150_00000000;
/// A lamport, and a whole SOL.
pub const SOL: u64 = 1_000_000_000;

pub fn orca() -> [u8; 32] {
    shonin_orca_driver::NAME
}

pub fn orca_error(error: OrcaError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + error as u32
}

/// `sha256(prefix:name)[..8]`.
fn discriminator(prefix: &str, name: &str) -> [u8; 8] {
    solana_sha256_hasher::hash(format!("{prefix}:{name}").as_bytes()).to_bytes()[..8]
        .try_into()
        .unwrap()
}

fn ix_data(name: &str, args: &[&[u8]]) -> Vec<u8> {
    let mut data = discriminator("global", name).to_vec();
    for arg in args {
        data.extend_from_slice(arg);
    }
    data
}

/// The bytes of a third-party program the manifest pins, checked against
/// its hash.
pub fn pinned_program(file: &str) -> Vec<u8> {
    let manifest = include_str!("../fixtures/programs.txt");
    let sha = manifest
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&file))
        .and_then(|fields| fields.get(2).map(|sha| sha.to_string()))
        .unwrap_or_else(|| panic!("{file} is not in fixtures/programs.txt"));
    let path = format!(
        "{}/../../../.shonin/fixtures/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|_| {
        panic!("{path} is missing: run `task contracts:solana:fixtures` first")
    });
    let got: String = solana_sha256_hasher::hash(&bytes)
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(got, sha, "{path} is not the pinned binary");
    bytes
}

/// The listed pool.
pub fn pool() -> Pubkey {
    POOLS[0].address
}

pub fn tick_array_address(pool: &Pubkey, start: i32) -> Pubkey {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), start.to_string().as_bytes()],
        &WHIRLPOOL_PROGRAM,
    )
    .0
}

pub fn position_address(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"position", mint.as_ref()], &WHIRLPOOL_PROGRAM).0
}

pub fn oracle_address(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"oracle", pool.as_ref()], &WHIRLPOOL_PROGRAM).0
}

/// The first tick of the array holding `tick`.
pub fn array_start(tick: i32) -> i32 {
    tick.div_euclid(SPAN) * SPAN
}

/// The ticks the fixture's tick arrays start at, around the pool's price.
pub const ARRAYS: [i32; 7] = [
    -6 * SPAN,
    -5 * SPAN,
    -4 * SPAN,
    -3 * SPAN,
    -2 * SPAN,
    -SPAN,
    0,
];

/// `price` (Pyth's units) as the driver's quote: dollar-units a lamport,
/// times 2^128.
pub fn quote(price: i64) -> shonin_orca_driver::wide::W {
    let raw = (U256::from(price as u64) << 128) / U256::from(100_000_000_000u64);
    shonin_orca_driver::wide::W(raw.0)
}

/// The pool's square-root price at `price`.
pub fn sqrt_at(price: i64) -> u128 {
    shonin_orca_driver::logic::sqrt_price_for(quote(price))
}

/// What a position of the station's holds, read from Orca's account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionState {
    pub liquidity: u128,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub fee_owed_a: u64,
    pub fee_owed_b: u64,
}

/// What the pool reads now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolState {
    /// The liquidity active at the pool's price.
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub tick: i32,
    pub vault_a: Pubkey,
    pub vault_b: Pubkey,
}

impl Env {
    /// The platform with stations set up, its dollar at the address Orca's
    /// driver lists, Orca's program and its pool, the price, the driver
    /// approved, and the pool given depth by `bob`.
    pub fn with_orca() -> Self {
        let mut env = Self::new_with_mint(Some(DOLLAR_MINT));
        env.set_up_stations();
        env.set_up_orca();
        env
    }

    pub fn set_up_orca(&mut self) {
        // Upgradeable, as Orca's program is on every cluster: its program
        // account then holds only the address of its data, and naming it in
        // a call costs what it costs there. Its upgrade authority is Orca's,
        // which nobody here holds.
        let orca_authority = Some(Pubkey::new_unique());
        self.deploy_upgradeable(
            WHIRLPOOL_PROGRAM,
            &pinned_program("orca_whirlpool.so"),
            orca_authority,
        );
        self.svm
            .add_program(shonin_orca_driver::ID, &so("shonin_orca_driver"))
            .unwrap();
        // Wrapped SOL's mint, as every cluster has it.
        if self
            .svm
            .get_account(&ASSET_MINT)
            .is_none_or(|a| a.data.is_empty())
        {
            use anchor_lang::solana_program::program_pack::Pack;
            let mint = spl_token::state::Mint {
                mint_authority: None.into(),
                supply: 0,
                decimals: 9,
                is_initialized: true,
                freeze_authority: None.into(),
            };
            let mut data = vec![0; spl_token::state::Mint::LEN];
            mint.pack_into_slice(&mut data);
            self.put(ASSET_MINT, spl_token::ID, data);
        }
        // Orca's settings, its fee authorities the operator's, and the fee
        // tier at the listed spacing.
        let operator = self.operator.pubkey();
        let mut config = discriminator("account", "WhirlpoolsConfig").to_vec();
        for _ in 0..3 {
            config.extend_from_slice(operator.as_ref());
        }
        config.extend_from_slice(&300u16.to_le_bytes());
        config.extend_from_slice(&0u16.to_le_bytes());
        self.put(CONFIG, WHIRLPOOL_PROGRAM, config);
        let fee_tier = self.fee_tier();
        let mut tier = discriminator("account", "FeeTier").to_vec();
        tier.extend_from_slice(CONFIG.as_ref());
        tier.extend_from_slice(&(SPACING as u16).to_le_bytes());
        tier.extend_from_slice(&FEE_RATE.to_le_bytes());
        self.put(fee_tier, WHIRLPOOL_PROGRAM, tier);
        self.set_price(SOL_PRICE, SOL_PRICE / 10_000, true);

        // The pool, at Pyth's price, and its tick arrays.
        let ix = self.ix_initialize_pool(SPACING as u16, sqrt_at(SOL_PRICE));
        let (ix, vaults) = ix;
        let payer = self.payer.insecure_clone();
        expect_ok(self.send(vec![ix], &[&payer, &vaults[0], &vaults[1]]));
        let arrays: Vec<Instruction> = ARRAYS
            .iter()
            .map(|start| self.ix_initialize_tick_array(&pool(), *start, false))
            .collect();
        expect_ok(self.send(arrays, &[&payer]));

        expect_ok(self.propose(ApprovalKind::Driver, orca(), shonin_orca_driver::ID));
        self.skip(TIMELOCK);
        self.set_price(SOL_PRICE, SOL_PRICE / 10_000, true);

        // Depth: bob's wide position.
        let bob = self.bob.insecure_clone();
        self.svm.airdrop(&bob.pubkey(), 10_000 * SOL).unwrap();
        self.give(&bob.pubkey(), 1_000_000_000_000);
        self.wrap(&bob, 2_000 * SOL);
        let mint = Keypair::new();
        let open = self.ix_open_position(
            &bob.pubkey(),
            &bob.pubkey(),
            &mint.pubkey(),
            -6 * SPAN + SPACING,
            SPACING * -2,
        );
        expect_ok(self.send(vec![open], &[&bob, &mint]));
        let liquidity = self.liquidity_for(
            -6 * SPAN + SPACING,
            -2 * SPACING,
            1_500 * SOL,
            200_000_000_000,
        );
        let add = self.ix_increase(
            &bob.pubkey(),
            &mint.pubkey(),
            &self.wsol_of(&bob.pubkey()),
            &get_associated_token_address(&bob.pubkey(), &self.mint),
            liquidity,
            u64::MAX,
            u64::MAX,
        );
        expect_ok(self.send(vec![add], &[&bob]));
    }

    fn put(&mut self, address: Pubkey, owner: Pubkey, data: Vec<u8>) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(
                address,
                Account {
                    lamports,
                    data,
                    owner,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
    }

    pub fn fee_tier(&self) -> Pubkey {
        Pubkey::find_program_address(
            &[
                b"fee_tier",
                CONFIG.as_ref(),
                &(SPACING as u16).to_le_bytes(),
            ],
            &WHIRLPOOL_PROGRAM,
        )
        .0
    }

    /// Writes Pyth's SOL/USD account: `price` and `confidence` in Pyth's
    /// units, published now, fully verified or not.
    pub fn set_price(&mut self, price: i64, confidence: i64, full: bool) {
        let now = self.now() as i64;
        self.set_price_account(
            PRICE_ACCOUNT,
            PYTH_RECEIVER,
            FEED_ID,
            price,
            confidence,
            now,
            full,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_price_account(
        &mut self,
        address: Pubkey,
        owner: Pubkey,
        feed: [u8; 32],
        price: i64,
        confidence: i64,
        published: i64,
        full: bool,
    ) {
        let mut data = discriminator("account", "PriceUpdateV2").to_vec();
        data.extend_from_slice(address.as_ref());
        if full {
            data.push(1);
        } else {
            data.extend_from_slice(&[0, 3]);
        }
        data.extend_from_slice(&feed);
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&(confidence as u64).to_le_bytes());
        data.extend_from_slice(&(-8i32).to_le_bytes());
        data.extend_from_slice(&published.to_le_bytes());
        data.extend_from_slice(&(published - 1).to_le_bytes());
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&(confidence as u64).to_le_bytes());
        data.extend_from_slice(&self.svm.get_sysvar::<Clock>().slot.to_le_bytes());
        data.resize(134, 0);
        self.put(address, owner, data);
    }

    /// `owner`'s wrapped-SOL account.
    pub fn wsol_of(&self, owner: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, &ASSET_MINT, &spl_token::ID)
    }

    pub fn ix_open_wsol(&self, payer: &Pubkey, owner: &Pubkey) -> Instruction {
        create_associated_token_account_idempotent(payer, owner, &ASSET_MINT, &spl_token::ID)
    }

    /// Wraps `lamports` of `who`'s SOL into its wrapped-SOL account.
    pub fn wrap(&mut self, who: &Keypair, lamports: u64) {
        let account = self.wsol_of(&who.pubkey());
        let ixs = vec![
            self.ix_open_wsol(&who.pubkey(), &who.pubkey()),
            anchor_lang::solana_program::system_instruction::transfer(
                &who.pubkey(),
                &account,
                lamports,
            ),
            spl_token::instruction::sync_native(&spl_token::ID, &account).unwrap(),
        ];
        expect_ok(self.send(ixs, &[who]));
    }

    pub fn token_amount(&self, account: &Pubkey) -> u64 {
        self.svm
            .get_account(account)
            .filter(|a| a.data.len() >= 72)
            .map_or(0, |a| {
                u64::from_le_bytes(a.data[64..72].try_into().unwrap())
            })
    }

    pub fn pool_state(&self) -> PoolState {
        let data = self.svm.get_account(&pool()).unwrap().data;
        PoolState {
            liquidity: u128::from_le_bytes(data[49..65].try_into().unwrap()),
            sqrt_price: u128::from_le_bytes(data[65..81].try_into().unwrap()),
            tick: i32::from_le_bytes(data[81..85].try_into().unwrap()),
            vault_a: Pubkey::try_from(&data[133..165]).unwrap(),
            vault_b: Pubkey::try_from(&data[213..245]).unwrap(),
        }
    }

    pub fn position_state(&self, mint: &Pubkey) -> Option<PositionState> {
        let account = self.svm.get_account(&position_address(mint))?;
        if account.data.len() < 144 {
            return None;
        }
        let d = &account.data;
        Some(PositionState {
            liquidity: u128::from_le_bytes(d[72..88].try_into().unwrap()),
            tick_lower: i32::from_le_bytes(d[88..92].try_into().unwrap()),
            tick_upper: i32::from_le_bytes(d[92..96].try_into().unwrap()),
            fee_owed_a: u64::from_le_bytes(d[112..120].try_into().unwrap()),
            fee_owed_b: u64::from_le_bytes(d[136..144].try_into().unwrap()),
        })
    }

    /// The liquidity `a` lamports and `b` dollar-units buy between two ticks
    /// at the pool's price now, a little under, as a provider asks for it.
    pub fn liquidity_for(&self, tick_lower: i32, tick_upper: i32, a: u64, b: u64) -> u128 {
        let s = self.pool_state().sqrt_price;
        let lower = orca_math::sqrt_price_at(tick_lower).unwrap();
        let upper = orca_math::sqrt_price_at(tick_upper).unwrap();
        let s = s.clamp(lower, upper);
        let for_a = if s < upper {
            let num = U256::from(a) * U256::from(s) * U256::from(upper);
            (num / U256::from(upper - s)) >> 64
        } else {
            U256::MAX
        };
        let for_b = if s > lower {
            (U256::from(b) << 64) / U256::from(s - lower)
        } else {
            U256::MAX
        };
        let l = for_a.min(for_b);
        (l - l / 1000).low_u128()
    }

    // Orca's instructions ------------------------------------------------

    /// Initializes the listed pool at `sqrt_price`; its two vault keypairs
    /// sign with it.
    pub fn ix_initialize_pool(
        &self,
        spacing: u16,
        sqrt_price: u128,
    ) -> (Instruction, [Keypair; 2]) {
        let (a, b) = shonin_orca_driver::network::mints();
        let address = Pubkey::find_program_address(
            &[
                b"whirlpool",
                CONFIG.as_ref(),
                a.as_ref(),
                b.as_ref(),
                &spacing.to_le_bytes(),
            ],
            &WHIRLPOOL_PROGRAM,
        );
        let vaults = [Keypair::new(), Keypair::new()];
        let ix = Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(CONFIG, false),
                AccountMeta::new_readonly(a, false),
                AccountMeta::new_readonly(b, false),
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new(address.0, false),
                AccountMeta::new(vaults[0].pubkey(), true),
                AccountMeta::new(vaults[1].pubkey(), true),
                AccountMeta::new_readonly(self.fee_tier(), false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
                AccountMeta::new_readonly(solana_sdk_ids::sysvar::rent::ID, false),
            ],
            data: ix_data(
                "initialize_pool",
                &[
                    &[address.1],
                    &spacing.to_le_bytes(),
                    &sqrt_price.to_le_bytes(),
                ],
            ),
        };
        (ix, vaults)
    }

    pub fn ix_initialize_tick_array(
        &self,
        pool: &Pubkey,
        start: i32,
        dynamic: bool,
    ) -> Instruction {
        let accounts = vec![
            AccountMeta::new_readonly(*pool, false),
            AccountMeta::new(self.payer.pubkey(), true),
            AccountMeta::new(tick_array_address(pool, start), false),
            AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
        ];
        let data = if dynamic {
            ix_data(
                "initialize_dynamic_tick_array",
                &[&start.to_le_bytes(), &[1]],
            )
        } else {
            ix_data("initialize_tick_array", &[&start.to_le_bytes()])
        };
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts,
            data,
        }
    }

    /// Opens a position of `owner`'s at `mint` (a Token-2022 NFT), paid by
    /// `funder`. The mint is left a non-signer when it is one of a station's
    /// position mints, which the station signs for.
    pub fn ix_open_position(
        &self,
        funder: &Pubkey,
        owner: &Pubkey,
        mint: &Pubkey,
        tick_lower: i32,
        tick_upper: i32,
    ) -> Instruction {
        let mint_is_pda = !mint.is_on_curve();
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new(*funder, true),
                AccountMeta::new_readonly(*owner, false),
                AccountMeta::new(position_address(mint), false),
                AccountMeta::new(*mint, !mint_is_pda),
                AccountMeta::new(
                    get_associated_token_address_with_program_id(owner, mint, &TOKEN_2022),
                    false,
                ),
                AccountMeta::new_readonly(pool(), false),
                AccountMeta::new_readonly(TOKEN_2022, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
                AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
                AccountMeta::new_readonly(METADATA_UPDATE_AUTH, false),
            ],
            data: ix_data(
                "open_position_with_token_extensions",
                &[&tick_lower.to_le_bytes(), &tick_upper.to_le_bytes(), &[0]],
            ),
        }
    }

    pub fn ix_close_position(
        &self,
        owner: &Pubkey,
        receiver: &Pubkey,
        mint: &Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(*owner, true),
                AccountMeta::new(*receiver, false),
                AccountMeta::new(position_address(mint), false),
                AccountMeta::new(*mint, false),
                AccountMeta::new(
                    get_associated_token_address_with_program_id(owner, mint, &TOKEN_2022),
                    false,
                ),
                AccountMeta::new_readonly(TOKEN_2022, false),
            ],
            data: ix_data("close_position_with_token_extensions", &[]),
        }
    }

    /// The tick arrays of a position's two ticks.
    fn position_arrays(&self, mint: &Pubkey) -> (Pubkey, Pubkey) {
        let position = self.position_state(mint).expect("the position exists");
        (
            tick_array_address(&pool(), array_start(position.tick_lower)),
            tick_array_address(&pool(), array_start(position.tick_upper)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn modify(
        &self,
        name: &str,
        authority: &Pubkey,
        mint: &Pubkey,
        owner_a: &Pubkey,
        owner_b: &Pubkey,
        liquidity: u128,
        limit_a: u64,
        limit_b: u64,
    ) -> Instruction {
        let state = self.pool_state();
        let (lower, upper) = self.position_arrays(mint);
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new(pool(), false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(*authority, true),
                AccountMeta::new(position_address(mint), false),
                AccountMeta::new_readonly(
                    get_associated_token_address_with_program_id(authority, mint, &TOKEN_2022),
                    false,
                ),
                AccountMeta::new(*owner_a, false),
                AccountMeta::new(*owner_b, false),
                AccountMeta::new(state.vault_a, false),
                AccountMeta::new(state.vault_b, false),
                AccountMeta::new(lower, false),
                AccountMeta::new(upper, false),
            ],
            data: ix_data(
                name,
                &[
                    &liquidity.to_le_bytes(),
                    &limit_a.to_le_bytes(),
                    &limit_b.to_le_bytes(),
                ],
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ix_increase(
        &self,
        authority: &Pubkey,
        mint: &Pubkey,
        owner_a: &Pubkey,
        owner_b: &Pubkey,
        liquidity: u128,
        max_a: u64,
        max_b: u64,
    ) -> Instruction {
        self.modify(
            "increase_liquidity",
            authority,
            mint,
            owner_a,
            owner_b,
            liquidity,
            max_a,
            max_b,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ix_decrease(
        &self,
        authority: &Pubkey,
        mint: &Pubkey,
        owner_a: &Pubkey,
        owner_b: &Pubkey,
        liquidity: u128,
        min_a: u64,
        min_b: u64,
    ) -> Instruction {
        self.modify(
            "decrease_liquidity",
            authority,
            mint,
            owner_a,
            owner_b,
            liquidity,
            min_a,
            min_b,
        )
    }

    pub fn ix_collect_fees(
        &self,
        authority: &Pubkey,
        mint: &Pubkey,
        owner_a: &Pubkey,
        owner_b: &Pubkey,
    ) -> Instruction {
        let state = self.pool_state();
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(pool(), false),
                AccountMeta::new_readonly(*authority, true),
                AccountMeta::new(position_address(mint), false),
                AccountMeta::new_readonly(
                    get_associated_token_address_with_program_id(authority, mint, &TOKEN_2022),
                    false,
                ),
                AccountMeta::new(*owner_a, false),
                AccountMeta::new(state.vault_a, false),
                AccountMeta::new(*owner_b, false),
                AccountMeta::new(state.vault_b, false),
                AccountMeta::new_readonly(spl_token::ID, false),
            ],
            data: ix_data("collect_fees", &[]),
        }
    }

    pub fn ix_update_fees(&self, mint: &Pubkey) -> Instruction {
        let (lower, upper) = self.position_arrays(mint);
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new(pool(), false),
                AccountMeta::new(position_address(mint), false),
                AccountMeta::new_readonly(lower, false),
                AccountMeta::new_readonly(upper, false),
            ],
            data: ix_data("update_fees_and_rewards", &[]),
        }
    }

    /// The three tick arrays a swap in the listed pool needs now.
    pub fn swap_arrays(&self, a_to_b: bool) -> [Pubkey; 3] {
        let state = self.pool_state();
        let base = array_start(state.tick);
        let offsets = if a_to_b {
            [0, -1, -2]
        } else if state.tick + SPACING >= base + SPAN {
            [1, 2, 3]
        } else {
            [0, 1, 2]
        };
        offsets.map(|o| tick_array_address(&pool(), base + o * SPAN))
    }

    /// A swap in the listed pool of `amount` in, A for B if `a_to_b`, no
    /// further than `limit`.
    #[allow(clippy::too_many_arguments)]
    pub fn ix_swap(
        &self,
        authority: &Pubkey,
        owner_a: &Pubkey,
        owner_b: &Pubkey,
        amount: u64,
        limit: u128,
        a_to_b: bool,
    ) -> Instruction {
        let state = self.pool_state();
        let arrays = self.swap_arrays(a_to_b);
        Instruction {
            program_id: WHIRLPOOL_PROGRAM,
            accounts: vec![
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(*authority, true),
                AccountMeta::new(pool(), false),
                AccountMeta::new(*owner_a, false),
                AccountMeta::new(state.vault_a, false),
                AccountMeta::new(*owner_b, false),
                AccountMeta::new(state.vault_b, false),
                AccountMeta::new(arrays[0], false),
                AccountMeta::new(arrays[1], false),
                AccountMeta::new(arrays[2], false),
                AccountMeta::new_readonly(oracle_address(&pool()), false),
            ],
            data: ix_data(
                "swap",
                &[
                    &amount.to_le_bytes(),
                    &0u64.to_le_bytes(),
                    &limit.to_le_bytes(),
                    &[1],
                    &[u8::from(a_to_b)],
                ],
            ),
        }
    }

    /// Bob trades `amount` in, A for B if `a_to_b`, at any price: what moves
    /// the pool's price, and earns its providers fees.
    pub fn bob_swaps(&mut self, amount: u64, a_to_b: bool) {
        let bob = self.bob.insecure_clone();
        let limit = if a_to_b {
            orca_math::MIN_SQRT_PRICE
        } else {
            orca_math::MAX_SQRT_PRICE
        };
        let ix = self.ix_swap(
            &bob.pubkey(),
            &self.wsol_of(&bob.pubkey()),
            &get_associated_token_address(&bob.pubkey(), &self.mint),
            amount,
            limit,
            a_to_b,
        );
        expect_ok(self.send(vec![ix], &[&bob]));
    }

    // The station at Orca -------------------------------------------------

    /// A vault whose station pins Orca's driver, its executor blessed and
    /// approved, its money pulled to the station, and the authority's
    /// wrapped-SOL account opened by the executor.
    pub fn orca_home(&mut self, label: &str) -> Home {
        self.orca_home_with_band(label, EXIT_BAND)
    }

    /// [`Env::orca_home`], its station's exit band `band`.
    pub fn orca_home_with_band(&mut self, label: &str, band: u16) -> Home {
        let home = self.new_home_with_band(
            label,
            &[Declared {
                name: orca(),
                program: shonin_orca_driver::ID,
            }],
            band,
        );
        expect_ok(self.pull(&home, FUNDS));
        self.approve_executor(&home);
        let executor = self.executor.insecure_clone();
        let open = self.ix_open_wsol(&executor.pubkey(), &home.authority(0));
        expect_ok(self.send(vec![open], &[&executor]));
        home
    }

    /// The authority's position mint at `slot`.
    pub fn slot_mint(&self, home: &Home, slot: u8) -> Pubkey {
        shonin_driver::position_mint(&shonin_station::ID, &home.authority(0), slot).0
    }

    /// The authority's dollar account and wrapped-SOL account, as a pool
    /// keeps its A and B.
    pub fn owners(&self, home: &Home) -> (Pubkey, Pubkey) {
        let wsol = self.wsol_of(&home.authority(0));
        let dollars = home.authority_dollars(0);
        if shonin_orca_driver::network::asset_is_a() {
            (wsol, dollars)
        } else {
            (dollars, wsol)
        }
    }

    /// The executor buys SOL for `dollars` of the station's, through the
    /// station.
    pub fn station_buys_sol(&mut self, home: &Home, dollars: u64) -> TxResult {
        let (owner_a, owner_b) = self.owners(home);
        let a_to_b = !shonin_orca_driver::network::asset_is_a();
        let limit = if a_to_b {
            orca_math::MIN_SQRT_PRICE
        } else {
            orca_math::MAX_SQRT_PRICE
        };
        let ix = self.ix_swap(
            &home.authority(0),
            &owner_a,
            &owner_b,
            dollars,
            limit,
            a_to_b,
        );
        self.execute_funded(home, ix, dollars)
    }

    /// The executor opens a position at `slot` through the station.
    pub fn station_opens(
        &mut self,
        home: &Home,
        slot: u8,
        tick_lower: i32,
        tick_upper: i32,
    ) -> TxResult {
        let executor = self.executor.pubkey();
        let mint = self.slot_mint(home, slot);
        let ix =
            self.ix_open_position(&executor, &home.authority(0), &mint, tick_lower, tick_upper);
        self.execute(home, ix)
    }

    /// The executor adds `liquidity` to the position at `slot`, funded with
    /// `dollars`.
    pub fn station_adds(
        &mut self,
        home: &Home,
        slot: u8,
        liquidity: u128,
        dollars: u64,
    ) -> TxResult {
        let (owner_a, owner_b) = self.owners(home);
        let mint = self.slot_mint(home, slot);
        let ix = self.ix_increase(
            &home.authority(0),
            &mint,
            &owner_a,
            &owner_b,
            liquidity,
            u64::MAX,
            u64::MAX,
        );
        self.execute_funded(home, ix, dollars)
    }

    /// A station holding SOL and one position at slot 0 between the two
    /// ticks, of about `sol` lamports and as many dollars as go with them.
    pub fn station_provides(&mut self, home: &Home, tick_lower: i32, tick_upper: i32, sol: u64) {
        self.station_provides_at(home, 0, tick_lower, tick_upper, sol);
    }

    pub fn station_provides_at(
        &mut self,
        home: &Home,
        slot: u8,
        tick_lower: i32,
        tick_upper: i32,
        sol: u64,
    ) {
        let wsol = self.wsol_of(&home.authority(0));
        let have = self.token_amount(&wsol);
        if have < sol {
            // Dollars for the SOL short, at about $150, and a little over.
            let dollars = (sol - have) * 153 / 1_000;
            expect_ok(self.station_buys_sol(home, dollars.max(1_000_000)));
        }
        expect_ok(self.station_opens(home, slot, tick_lower, tick_upper));
        let sol_now = self.token_amount(&wsol).min(sol);
        let liquidity = self.liquidity_for(tick_lower, tick_upper, sol_now, self.idle(home) / 2);
        expect_ok(self.station_adds(home, slot, liquidity, self.idle(home) / 2));
    }

    /// The accounts Orca's driver is given for `station`, as its layout
    /// says: the program, the authority and its dollar account, Orca, the
    /// token program, the price, the authority's wrapped SOL, the pool and
    /// its vaults, the sale's oracle and arrays, each slot, each position's
    /// token account, then the tick arrays of every position with
    /// liquidity.
    pub fn orca_group(&self, station: &Pubkey) -> Vec<AccountMeta> {
        let authority = authority_of(station, &shonin_orca_driver::ID);
        let state = self.pool_state();
        let a_to_b = shonin_orca_driver::network::asset_is_a();
        let mut group = vec![
            AccountMeta::new_readonly(shonin_orca_driver::ID, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(get_associated_token_address(&authority, &self.mint), false),
            AccountMeta::new_readonly(WHIRLPOOL_PROGRAM, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(PRICE_ACCOUNT, false),
            AccountMeta::new(self.wsol_of(&authority), false),
            AccountMeta::new(pool(), false),
            AccountMeta::new(state.vault_a, false),
            AccountMeta::new(state.vault_b, false),
            AccountMeta::new_readonly(oracle_address(&pool()), false),
        ];
        group.extend(self.swap_arrays(a_to_b).map(|a| AccountMeta::new(a, false)));
        let mut tokens = Vec::new();
        let mut arrays: Vec<Pubkey> = Vec::new();
        for slot in 0..MAX_SLOTS {
            let mint = shonin_driver::position_mint(&shonin_station::ID, &authority, slot).0;
            match self.position_state(&mint) {
                Some(position) => {
                    group.push(AccountMeta::new(position_address(&mint), false));
                    tokens.push(AccountMeta::new_readonly(
                        get_associated_token_address_with_program_id(
                            &authority,
                            &mint,
                            &TOKEN_2022,
                        ),
                        false,
                    ));
                    if position.liquidity > 0 {
                        for tick in [position.tick_lower, position.tick_upper] {
                            let array = tick_array_address(&pool(), array_start(tick));
                            if !arrays.contains(&array) {
                                arrays.push(array);
                            }
                        }
                    }
                }
                None => group.push(AccountMeta::new_readonly(mint, false)),
            }
        }
        group.extend(tokens);
        group.extend(arrays.into_iter().map(|a| AccountMeta::new(a, false)));
        group
    }

    /// The vault sends `ix` through the vault stand-in, through a lookup
    /// table when it names more than the wire carries.
    pub fn as_vault_fitting(&mut self, home: &Home, ix: Instruction) -> TxResult {
        let call = self.ix_call(home, ix, 0);
        let payer = self.payer.insecure_clone();
        self.send_fitting(vec![call], &[&payer])
    }

    /// The vault unwinds `fraction` of a station at Orca.
    pub fn orca_unwind(
        &mut self,
        home: &Home,
        fraction: u64,
    ) -> std::result::Result<(Outcome, u64, u64), FailedTx> {
        let ix = self.ix_unwind(home, &home.vault, fraction, false);
        let meta = self.as_vault_fitting(home, ix)?;
        let outcome = Outcome::try_from_slice(&meta.return_data.data).expect("an outcome");
        Ok((outcome, self.received(home), meta.compute_units_consumed))
    }
}

impl Env {
    /// The station's `value` with Orca's driver given `group`, sent as a
    /// transaction, for what it answers or why it fails.
    pub fn try_value_with(
        &mut self,
        home: &Home,
        group: Vec<AccountMeta>,
    ) -> std::result::Result<Valuation, FailedTx> {
        let mut accounts = shonin_station::accounts::Value {
            station: home.station,
        }
        .to_account_metas(None);
        let size = group.len() as u8;
        accounts.extend(group);
        let ix = Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Value { groups: vec![size] }.data(),
        };
        let payer = self.payer.insecure_clone();
        let meta = self.send_fitting(vec![ix], &[&payer])?;
        Ok(Valuation::try_from_slice(&meta.return_data.data).expect("a valuation"))
    }

    /// Orca's driver's own `value` at `band_bps`, asked directly with
    /// `group` (the program first), for what it answers or why it fails.
    pub fn driver_value(
        &mut self,
        group: &[AccountMeta],
        band_bps: u16,
    ) -> std::result::Result<Valuation, FailedTx> {
        self.driver_value_metered(group, band_bps)
            .map(|(value, _)| value)
    }

    /// [`Self::driver_value`], with the compute it took.
    pub fn driver_value_metered(
        &mut self,
        group: &[AccountMeta],
        band_bps: u16,
    ) -> std::result::Result<(Valuation, u64), FailedTx> {
        let accounts = group[1..]
            .iter()
            .map(|meta| AccountMeta::new_readonly(meta.pubkey, false))
            .collect();
        let ix = Instruction {
            program_id: shonin_orca_driver::ID,
            accounts,
            data: shonin_orca_driver::instruction::Value { band_bps }.data(),
        };
        let payer = self.payer.insecure_clone();
        let meta = self.send_fitting(vec![ix], &[&payer])?;
        Ok((
            Valuation::try_from_slice(&meta.return_data.data).expect("a valuation"),
            meta.compute_units_consumed,
        ))
    }

    /// Bob opens a sliver of liquidity between every two ticks a spacing
    /// apart, `count` of them from the one below the pool's price down:
    /// every one of those ticks initialized.
    pub fn crowd_below(&mut self, count: i32) {
        let top = self.pool_state().tick.div_euclid(SPACING) * SPACING;
        let ranges: Vec<(i32, i32)> = (1..=count)
            .map(|i| (top - i * SPACING, top - (i - 1) * SPACING))
            .collect();
        self.provide_ranges(&ranges, 1_000_000_000);
    }

    /// Bob opens a position of `liquidity` on each of `ranges`.
    pub fn provide_ranges(&mut self, ranges: &[(i32, i32)], liquidity: u128) {
        let bob = self.bob.insecure_clone();
        let usd = get_associated_token_address(&bob.pubkey(), &self.mint);
        for &(lower, upper) in ranges {
            let mint = Keypair::new();
            let open =
                self.ix_open_position(&bob.pubkey(), &bob.pubkey(), &mint.pubkey(), lower, upper);
            expect_ok(self.send(vec![open], &[&bob, &mint]));
            let add = self.ix_increase(
                &bob.pubkey(),
                &mint.pubkey(),
                &self.wsol_of(&bob.pubkey()),
                &usd,
                liquidity,
                u64::MAX,
                u64::MAX,
            );
            expect_ok(self.send(vec![add], &[&bob]));
        }
    }

    /// The tick arrays at `starts` made Orca's dynamic kind, as whoever
    /// initializes an array first may choose, before anything is placed in
    /// them.
    pub fn make_dynamic(&mut self, starts: &[i32]) {
        let payer = self.payer.insecure_clone();
        for &start in starts {
            let address = tick_array_address(&pool(), start);
            self.svm
                .set_account(address, solana_account::Account::default())
                .unwrap();
            let ix = self.ix_initialize_tick_array(&pool(), start, true);
            expect_ok(self.send(vec![ix], &[&payer]));
        }
    }

    /// Where an account stands in the driver's group (the program first).
    pub fn group_index(group: &[AccountMeta], key: &Pubkey) -> usize {
        group
            .iter()
            .position(|meta| &meta.pubkey == key)
            .expect("the account is in the group")
    }
}

impl Env {
    /// Pyth's price set to the pool's own, published now: the oracle and the
    /// pool agreeing, as arbitrage keeps them.
    pub fn price_follows_pool(&mut self) {
        let s = U256::from(self.pool_state().sqrt_price);
        let quote = s * s;
        let price = (quote * U256::from(100_000_000_000u64)) >> 128;
        let price = price.low_u64() as i64;
        self.set_price(price, price / 10_000, true);
    }

    /// Pyth's SOL price as the account holds it now.
    pub fn sol_price(&self) -> i64 {
        let data = self.svm.get_account(&PRICE_ACCOUNT).unwrap().data;
        i64::from_le_bytes(data[73..81].try_into().unwrap())
    }
}
