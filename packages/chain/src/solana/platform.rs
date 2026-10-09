//! Every instruction the platform sends on Solana, with the accounts each
//! one needs found from the deployment and, where an instruction's accounts
//! depend on what is on the cluster (a request's kind, the markets a station
//! holds, the registry's checkers), read from it.
//!
//! Each builder fills in a program's own `accounts::` struct and
//! `instruction::` arguments, so the order and meaning of every account is
//! the program's, never this file's. What this file decides is only which
//! account goes in each place.

use anchor_lang::{AnchorDeserialize, Discriminator, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::{
    get_associated_token_address,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
};
use anchor_spl::token::spl_token;
use anyhow::{Context, Result, anyhow, bail};

use super::client::{Landed, SolanaClient};
use super::rpc::parse_pubkey;
use super::{AccountMeta, Instruction, Keypair, Pubkey, Signer};
use crate::deployments::SolanaDeployments;

use shonin_common::digest::{blessing_leaf, cluster_value, keccak};
use shonin_prices::{Allowances, UnitPrices};
use shonin_publishing::state::{Lists, ReviewInput};
use shonin_registry::read as registry;
use shonin_registry::state::{ApprovalKind, ApprovalSlot, ImageKind, Journal, KeyInfo};
use shonin_vault::state::{
    self as vs, Claim, Holder, Request, RequestKind, Terms, Vault, VaultState,
};

const SYSTEM: Pubkey = anchor_lang::solana_program::system_program::ID;
const INSTRUCTIONS: Pubkey = anchor_lang::solana_program::sysvar::instructions::ID;
/// How large a mint account is.
const MINT_SIZE: usize = 82;

/// The vault program's event authority, which signs its self-calls.
pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &shonin_vault::ID).0
}

/// A program's program-data account under the upgradeable loader.
pub fn program_data_address(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[program.as_ref()],
        &anchor_lang::solana_program::bpf_loader_upgradeable::ID,
    )
    .0
}

/// What a measured image would be: on a development cluster no enclave is
/// measured, so each service's name stands for its image, as on the EVM.
pub fn image_digest(service: &str) -> [u8; 32] {
    keccak(&[format!("shonin-dev-image:{service}").as_bytes()])
}

pub fn project_id() -> [u8; 32] {
    keccak(&[b"shonin-dev-project"])
}

/// One vault: its strategy, its address, its station and dollar accounts.
#[derive(Clone, Debug)]
pub struct VaultAccounts {
    pub strategy_id: u64,
    pub vault: Pubkey,
    pub station: Pubkey,
    pub vault_dollars: Pubkey,
    pub station_dollars: Pubkey,
}

/// A driver a station is created with: its registry name and its program.
#[derive(Clone, Copy, Debug)]
pub struct Declared {
    pub name: [u8; 32],
    pub program: Pubkey,
}

impl Declared {
    /// The stand-in venue's driver.
    pub fn local() -> Self {
        Self {
            name: shonin_local_driver::NAME,
            program: shonin_local_driver::ID,
        }
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

/// The platform on one cluster: its dollar, its roles, and every
/// instruction.
#[derive(Clone, Debug)]
pub struct Platform {
    pub cluster_name: String,
    pub cluster: [u8; 32],
    pub mint: Pubkey,
    pub admin: Pubkey,
    /// Receives the platform's fees.
    pub platform: Pubkey,
    pub operator: Pubkey,
    /// The development checker's state account.
    pub checker: Option<Pubkey>,
}

impl Platform {
    /// The platform a set-up deployment describes.
    pub fn from_deployments(deployments: &SolanaDeployments) -> Result<Self> {
        let roles = deployments
            .roles
            .as_ref()
            .ok_or_else(|| anyhow!("the deployment has not set the platform up"))?;
        let mint = deployments
            .mint
            .as_deref()
            .ok_or_else(|| anyhow!("the deployment has no dollar mint"))?;
        Ok(Self {
            cluster_name: deployments.cluster.clone(),
            cluster: cluster_value(&deployments.cluster),
            mint: parse_pubkey(mint)?,
            admin: parse_pubkey(&roles.admin)?,
            platform: parse_pubkey(&roles.platform)?,
            operator: parse_pubkey(&roles.operator)?,
            checker: deployments
                .checker
                .as_deref()
                .map(parse_pubkey)
                .transpose()?,
        })
    }

    /// `owner`'s dollar account: its associated token account.
    pub fn dollars(&self, owner: &Pubkey) -> Pubkey {
        get_associated_token_address(owner, &self.mint)
    }

    pub fn vault_accounts(&self, strategy_id: u64) -> VaultAccounts {
        let vault = vs::vault_address(strategy_id).0;
        let station = shonin_station::station_address(&vault);
        VaultAccounts {
            strategy_id,
            vault,
            station,
            vault_dollars: self.dollars(&vault),
            station_dollars: self.dollars(&station),
        }
    }

    // The dollar --------------------------------------------------------------

    /// Create the dollar's mint at `mint`, six decimals, `authority` minting.
    pub fn ix_create_mint(
        payer: &Pubkey,
        mint: &Pubkey,
        authority: &Pubkey,
        rent: u64,
    ) -> Result<Vec<Instruction>> {
        Ok(vec![
            anchor_lang::solana_program::system_instruction::create_account(
                payer,
                mint,
                rent,
                MINT_SIZE as u64,
                &spl_token::ID,
            ),
            spl_token::instruction::initialize_mint2(
                &spl_token::ID,
                mint,
                authority,
                None,
                vs::DECIMALS,
            )?,
        ])
    }

    pub fn rent_for_mint() -> usize {
        MINT_SIZE
    }

    /// Create `owner`'s dollar account if it isn't there.
    pub fn ix_open_dollars(&self, payer: &Pubkey, owner: &Pubkey) -> Instruction {
        create_associated_token_account_idempotent(payer, owner, &self.mint, &spl_token::ID)
    }

    pub fn ix_mint_to(&self, authority: &Pubkey, to: &Pubkey, amount: u64) -> Result<Instruction> {
        Ok(spl_token::instruction::mint_to(
            &spl_token::ID,
            &self.mint,
            &self.dollars(to),
            authority,
            &[],
            amount,
        )?)
    }

    /// Mint straight into a token account, not an owner's associated one.
    pub fn ix_mint_into(
        &self,
        authority: &Pubkey,
        account: &Pubkey,
        amount: u64,
    ) -> Result<Instruction> {
        Ok(spl_token::instruction::mint_to(
            &spl_token::ID,
            &self.mint,
            account,
            authority,
            &[],
            amount,
        )?)
    }

    // Setting up --------------------------------------------------------------

    /// The development checker trusting `attestation_key`, at the address
    /// `seed` gives it.
    pub fn ix_create_checker(
        payer: &Pubkey,
        attestation_key: Pubkey,
        seed: [u8; 32],
        cluster_name: &str,
    ) -> (Instruction, Pubkey) {
        let checker = Pubkey::find_program_address(
            &[
                shonin_dev_verifier::CHECKER_SEED,
                attestation_key.as_ref(),
                &seed,
            ],
            &shonin_dev_verifier::ID,
        )
        .0;
        let ix = Instruction {
            program_id: shonin_dev_verifier::ID,
            accounts: shonin_dev_verifier::accounts::CreateChecker {
                payer: *payer,
                checker,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_dev_verifier::instruction::CreateChecker {
                attestation_key,
                seed,
                cluster_name: cluster_name.to_string(),
            }
            .data(),
        };
        (ix, checker)
    }

    /// The Confidential Space checker for the attestation guest `image_id`,
    /// trusting the Google keys `keys` (sorted here, as a checker takes
    /// them), at the address they give it.
    pub fn ix_create_cs_checker(
        payer: &Pubkey,
        image_id: [u8; 32],
        keys: &[[u8; 32]],
    ) -> (Instruction, Pubkey) {
        let mut keys = keys.to_vec();
        keys.sort();
        keys.dedup();
        let checker = shonin_cs_verifier::checker_address(&image_id, &keys);
        let ix = Instruction {
            program_id: shonin_cs_verifier::ID,
            accounts: shonin_cs_verifier::accounts::CreateChecker {
                payer: *payer,
                checker,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_cs_verifier::instruction::CreateChecker {
                image_id,
                keys: shonin_cs_verifier::keys_digest(&keys),
                trusted_keys: keys,
            }
            .data(),
        };
        (ix, checker)
    }

    /// Set the registry up, trusting the development checker, which must be
    /// a final program by then.
    pub fn ix_initialize_registry(
        deployer: &Pubkey,
        args: shonin_registry::InitializeArgs,
        checker: Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Initialize {
                deployer: *deployer,
                config: registry::config_address(),
                program: shonin_registry::ID,
                program_data: program_data_address(&shonin_registry::ID),
                verifier_program: shonin_dev_verifier::ID,
                verifier_program_data: Some(program_data_address(&shonin_dev_verifier::ID)),
                verifier_state: checker,
                verifier_record: registry::verifier_address(&checker),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::Initialize { args }.data(),
        }
    }

    pub fn ix_initialize_publishing(&self, deployer: &Pubkey, fee: u64) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Initialize {
                deployer: *deployer,
                config: publishing_config(),
                program: shonin_publishing::ID,
                program_data: program_data_address(&shonin_publishing::ID),
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Initialize {
                platform: self.platform,
                fee,
            }
            .data(),
        }
    }

    pub fn ix_initialize_prices(
        &self,
        deployer: &Pubkey,
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
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_prices::instruction::Initialize {
                admin: self.admin,
                notice,
                initial,
            }
            .data(),
        }
    }

    /// The station program, with the vault program as the one whose vaults
    /// may create stations.
    pub fn ix_initialize_stations(&self, deployer: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Initialize {
                deployer: *deployer,
                config: shonin_station::config_address(),
                mint: self.mint,
                registry_config: registry::config_address(),
                program: shonin_station::ID,
                program_data: program_data_address(&shonin_station::ID),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Initialize {
                vault_program: shonin_vault::ID,
            }
            .data(),
        }
    }

    pub fn ix_initialize_venue(&self, deployer: &Pubkey, margin_bps: u64) -> Instruction {
        self.ix_initialize_venue_at(&shonin_local_venue::ID, deployer, margin_bps)
    }

    /// The same for `venue`, the stand-in venue or one of its copies.
    pub fn ix_initialize_venue_at(
        &self,
        venue: &Pubkey,
        deployer: &Pubkey,
        margin_bps: u64,
    ) -> Instruction {
        let venue = *venue;
        Instruction {
            program_id: venue,
            accounts: shonin_local_venue::accounts::Initialize {
                deployer: *deployer,
                venue: shonin_local_venue::venue_address_of(&venue),
                vault: shonin_local_venue::vault_address_of(&venue),
                mint: self.mint,
                program: venue,
                program_data: program_data_address(&venue),
                token_program: spl_token::ID,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::Initialize {
                operator: self.operator,
                margin_bps,
            }
            .data(),
        }
    }

    /// The vault factory, on `terms`, this cluster's vaults named `solana`.
    pub fn ix_initialize_vaults(&self, deployer: &Pubkey, terms: Terms) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Initialize {
                deployer: *deployer,
                factory: vs::factory_address(),
                mint: self.mint,
                registry_config: registry::config_address(),
                program: shonin_vault::ID,
                program_data: program_data_address(&shonin_vault::ID),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Initialize {
                args: shonin_vault::InitializeArgs {
                    admin: self.admin,
                    platform: self.platform,
                    chain_name: super::name("solana"),
                    terms,
                    previous: Pubkey::default(),
                },
            }
            .data(),
        }
    }

    // The stand-in venue ------------------------------------------------------

    pub fn ix_list_market(&self, id: [u8; 32], is_perp: bool, price: u64) -> Instruction {
        self.ix_list_market_at(&shonin_local_venue::ID, id, is_perp, price)
    }

    /// The same at `venue`.
    pub fn ix_list_market_at(
        &self,
        venue: &Pubkey,
        id: [u8; 32],
        is_perp: bool,
        price: u64,
    ) -> Instruction {
        let venue = *venue;
        Instruction {
            program_id: venue,
            accounts: shonin_local_venue::accounts::ListMarket {
                operator: self.operator,
                venue: shonin_local_venue::venue_address_of(&venue),
                market: shonin_local_venue::market_address_of(&venue, &id),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::ListMarket { id, is_perp, price }.data(),
        }
    }

    pub fn ix_set_prices(&self, quote: shonin_local_venue::Quote) -> Instruction {
        Instruction {
            program_id: shonin_local_venue::ID,
            accounts: shonin_local_venue::accounts::Operate {
                operator: self.operator,
                venue: shonin_local_venue::venue_address(),
                market: shonin_local_venue::market_address(&quote.market),
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::SetPrices { quote }.data(),
        }
    }

    pub fn ix_accrue_funding(&self, market: [u8; 32], paid_per_unit: i128) -> Instruction {
        Instruction {
            program_id: shonin_local_venue::ID,
            accounts: shonin_local_venue::accounts::Operate {
                operator: self.operator,
                venue: shonin_local_venue::venue_address(),
                market: shonin_local_venue::market_address(&market),
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::AccrueFunding { paid_per_unit }.data(),
        }
    }

    /// Everything the operator has to say in one instruction: quotes and
    /// funding, for the markets named.
    pub fn ix_post(
        &self,
        quotes: Vec<shonin_local_venue::Quote>,
        funding: Vec<shonin_local_venue::Funding>,
    ) -> Instruction {
        let mut markets: Vec<[u8; 32]> = quotes.iter().map(|quote| quote.market).collect();
        for entry in &funding {
            if !markets.contains(&entry.market) {
                markets.push(entry.market);
            }
        }
        let mut accounts = shonin_local_venue::accounts::Post {
            operator: self.operator,
            venue: shonin_local_venue::venue_address(),
        }
        .to_account_metas(None);
        accounts.extend(
            markets
                .iter()
                .map(|market| AccountMeta::new(shonin_local_venue::market_address(market), false)),
        );
        Instruction {
            program_id: shonin_local_venue::ID,
            accounts,
            data: shonin_local_venue::instruction::Post { quotes, funding }.data(),
        }
    }

    /// The venue's account of `master` (a station's venue authority).
    pub async fn margin_account(
        &self,
        client: &SolanaClient,
        master: &Pubkey,
    ) -> Result<Option<shonin_local_venue::MarginAccount>> {
        self.margin_account_at(client, &shonin_local_venue::ID, master)
            .await
    }

    /// The same at `venue`, the stand-in venue or one of its copies.
    pub async fn margin_account_at(
        &self,
        client: &SolanaClient,
        venue: &Pubkey,
        master: &Pubkey,
    ) -> Result<Option<shonin_local_venue::MarginAccount>> {
        client
            .load(&shonin_local_venue::account_address_of(venue, master))
            .await
    }

    /// The markets `master` holds at the venue, as accounts, in its order.
    pub async fn held_markets(
        &self,
        client: &SolanaClient,
        master: &Pubkey,
    ) -> Result<Vec<Pubkey>> {
        self.held_markets_at(client, &shonin_local_venue::ID, master)
            .await
    }

    /// The same at `venue`.
    pub async fn held_markets_at(
        &self,
        client: &SolanaClient,
        venue: &Pubkey,
        master: &Pubkey,
    ) -> Result<Vec<Pubkey>> {
        Ok(self
            .margin_account_at(client, venue, master)
            .await?
            .map(|account| {
                account
                    .positions
                    .iter()
                    .map(|position| shonin_local_venue::market_address_of(venue, &position.market))
                    .collect()
            })
            .unwrap_or_default())
    }

    // The registry --------------------------------------------------------------

    pub fn ix_propose_image(&self, digest: [u8; 32], kind: ImageKind) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ImageChange {
                admin: self.admin,
                config: registry::config_address(),
                image: registry::image_address(&digest),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeImage {
                image_digest: digest,
                kind,
            }
            .data(),
        }
    }

    /// Revoke an image, at once and for good.
    pub fn ix_revoke_image(&self, digest: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ImageChange {
                admin: self.admin,
                config: registry::config_address(),
                image: registry::image_address(&digest),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::RevokeImage {
                image_digest: digest,
            }
            .data(),
        }
    }

    /// Propose a checker (a final program and its state account): it counts
    /// once the timelock has passed.
    pub fn ix_propose_verifier(&self, program: Pubkey, state: Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ProposeVerifier {
                admin: self.admin,
                config: registry::config_address(),
                verifier_program: program,
                verifier_program_data: Some(program_data_address(&program)),
                verifier_state: state,
                verifier_record: registry::verifier_address(&state),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeVerifier {}.data(),
        }
    }

    /// Retire the checker named by its state account, at once and for good.
    pub fn ix_retire_verifier(&self, state: Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Admin {
                admin: self.admin,
                config: registry::config_address(),
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::RetireVerifier { state }.data(),
        }
    }

    pub fn ix_propose_project(&self, id: [u8; 32]) -> Instruction {
        Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::ProjectChange {
                admin: self.admin,
                config: registry::config_address(),
                project: registry::project_address(&id),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::ProposeProject { project_id: id }.data(),
        }
    }

    /// Propose `implementation` (a final program) under `name`: the record
    /// of a change it settles comes along when there is one.
    pub async fn ix_propose(
        &self,
        client: &SolanaClient,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> Result<Instruction> {
        let slot_address = registry::approval_address(kind, &name);
        let now = client.now().await?;
        let settling = client
            .load::<ApprovalSlot>(&slot_address)
            .await?
            .filter(|slot| slot.next_is_ready(now))
            .map(|slot| registry::revoked_address(kind, &name, &slot.next));
        Ok(Instruction {
            program_id: shonin_registry::ID,
            accounts: shonin_registry::accounts::Propose {
                admin: self.admin,
                config: registry::config_address(),
                slot: slot_address,
                next_revoked: settling,
                implementation_program: implementation,
                implementation_data: Some(program_data_address(&implementation)),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_registry::instruction::Propose {
                kind,
                name,
                implementation,
            }
            .data(),
        })
    }

    pub async fn registry_config(
        &self,
        client: &SolanaClient,
    ) -> Result<shonin_registry::state::Config> {
        client
            .load(&registry::config_address())
            .await?
            .ok_or_else(|| anyhow!("the registry is not set up"))
    }

    pub async fn key_info(
        &self,
        client: &SolanaClient,
        signer: &Pubkey,
    ) -> Result<Option<KeyInfo>> {
        client.load(&registry::key_address(signer)).await
    }

    /// The registry records a key's blessing is read from: its record, and
    /// the records of its image and its project.
    pub async fn records_of(&self, client: &SolanaClient, signer: &Pubkey) -> Result<[Pubkey; 3]> {
        let (image, project) = self
            .key_info(client, signer)
            .await?
            .map(|key| {
                (
                    registry::image_address(&key.image_digest),
                    registry::project_address(&key.project_id),
                )
            })
            .unwrap_or_default();
        Ok([registry::key_address(signer), image, project])
    }

    /// What a key's blessing for a vault is read from: the config, the
    /// blessing, the key's record, its image's and its project's.
    pub async fn key_records(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        bundle: &[u8; 32],
        key: &Pubkey,
    ) -> Result<[Pubkey; 5]> {
        let [key_info, image, project] = self.records_of(client, key).await?;
        Ok([
            registry::config_address(),
            registry::blessing_address(key, &self.cluster, &vault.to_bytes(), bundle),
            key_info,
            image,
            project,
        ])
    }

    // Blessing ------------------------------------------------------------------

    /// The leaf a builder or a reviewer commits to for its `key`: no vault,
    /// on this cluster.
    pub fn service_leaf(&self, key: &Pubkey) -> [u8; 32] {
        blessing_leaf(&[0; 32], &[0; 32], &[0; 32], &self.cluster, &key.to_bytes())
    }

    /// The leaf an executor commits to for its `key`, for one vault on this
    /// cluster and one bundle.
    pub fn vault_leaf(&self, vault: &Pubkey, bundle: &[u8; 32], key: &Pubkey) -> [u8; 32] {
        blessing_leaf(
            &self.cluster,
            &vault.to_bytes(),
            bundle,
            &self.cluster,
            &key.to_bytes(),
        )
    }

    /// The development seal: the attestor's Ed25519 signature over what the
    /// checker checks, as an instruction of the blessing's transaction.
    pub fn dev_seal(&self, attestor: &Keypair, journal: &Journal) -> Result<Instruction> {
        let bytes = anchor_lang::AnchorSerialize::try_to_vec(journal)?;
        let message = shonin_dev_verifier::attestation_digest(&self.cluster, &bytes);
        Ok(ed25519(attestor, &message))
    }

    /// The bless instruction for `key`, with every allowed checker's
    /// accounts, newest first, as a relayer sends it: a Confidential Space
    /// checker reads only its state, and the development checker its state
    /// and the transaction's instructions, where its seal is (`seal` is then
    /// empty, and the Ed25519 instruction goes before this one).
    #[allow(clippy::too_many_arguments)]
    pub async fn ix_bless(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        journal: &Journal,
        seal: Vec<u8>,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        key: &Pubkey,
        merkle_proof: Vec<[u8; 32]>,
    ) -> Result<Instruction> {
        let config = self.registry_config(client).await?;
        let now = client.now().await?;
        Ok(Self::bless_instruction(
            &config,
            now,
            payer,
            journal,
            seal,
            vault_chain,
            vault,
            strategy_hash,
            key,
            merkle_proof,
        ))
    }

    /// [`Platform::ix_bless`], from the registry's settings as read at `now`.
    #[allow(clippy::too_many_arguments)]
    pub fn bless_instruction(
        config: &shonin_registry::state::Config,
        now: u64,
        payer: &Pubkey,
        journal: &Journal,
        seal: Vec<u8>,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        key: &Pubkey,
        merkle_proof: Vec<[u8; 32]>,
    ) -> Instruction {
        let mut groups = Vec::new();
        let mut remaining = Vec::new();
        for entry in config.verifiers.iter().rev() {
            if now < entry.ready_at {
                continue;
            }
            remaining.push(AccountMeta::new_readonly(entry.program, false));
            remaining.push(AccountMeta::new_readonly(entry.state, false));
            if entry.program == shonin_cs_verifier::ID {
                groups.push(2u8);
            } else {
                groups.push(3u8);
                remaining.push(AccountMeta::new_readonly(INSTRUCTIONS, false));
            }
        }
        let mut accounts = shonin_registry::accounts::Bless {
            payer: *payer,
            config: registry::config_address(),
            image: registry::image_address(&journal.image_digest),
            project: registry::project_address(&journal.project_id),
            key_info: registry::key_address(key),
            blessing: registry::blessing_address(key, &vault_chain, &vault, &strategy_hash),
            system_program: SYSTEM,
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
                key: *key,
                merkle_proof,
                verifier_accounts: groups,
            }
            .data(),
        }
    }

    /// Bless `signer` as a `service` (builder, reviewer: no vault), sealed by
    /// the development attestor, attested now.
    pub async fn bless_service(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        attestor: &Keypair,
        service: &str,
        signer: &Pubkey,
    ) -> Result<Landed> {
        let journal = Journal {
            image_digest: image_digest(service),
            project_id: project_id(),
            signer: *signer,
            vaults_root: self.service_leaf(signer),
            issued_at: client.now().await?,
        };
        let seal = self.dev_seal(attestor, &journal)?;
        let bless = self
            .ix_bless(
                client,
                &payer.pubkey(),
                &journal,
                Vec::new(),
                [0; 32],
                [0; 32],
                [0; 32],
                signer,
                vec![],
            )
            .await?;
        client.send(vec![seal, bless], &[payer]).await
    }

    /// Bless `signer` as the executor of one vault running one bundle.
    pub async fn bless_executor(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        attestor: &Keypair,
        signer: &Pubkey,
        vault: &Pubkey,
        bundle: &[u8; 32],
    ) -> Result<Landed> {
        let journal = Journal {
            image_digest: image_digest("executor"),
            project_id: project_id(),
            signer: *signer,
            vaults_root: self.vault_leaf(vault, bundle, signer),
            issued_at: client.now().await?,
        };
        let seal = self.dev_seal(attestor, &journal)?;
        let bless = self
            .ix_bless(
                client,
                &payer.pubkey(),
                &journal,
                Vec::new(),
                self.cluster,
                vault.to_bytes(),
                *bundle,
                signer,
                vec![],
            )
            .await?;
        client.send(vec![seal, bless], &[payer]).await
    }

    // Publishing ------------------------------------------------------------------

    pub async fn publishing_config(
        &self,
        client: &SolanaClient,
    ) -> Result<shonin_publishing::state::Config> {
        client
            .load(&publishing_config())
            .await?
            .ok_or_else(|| anyhow!("publishing is not set up"))
    }

    /// Pay to publish: the strategy id is the one the submission takes.
    pub async fn ix_submit(
        &self,
        client: &SolanaClient,
        author: &Pubkey,
        upload: [u8; 32],
    ) -> Result<(Instruction, u64)> {
        let next = self.publishing_config(client).await?.count + 1;
        let ix = Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Submit {
                author: *author,
                config: publishing_config(),
                submission: submission_address(next),
                mint: self.mint,
                author_token: self.dollars(author),
                platform_token: self.dollars(&self.platform),
                token_program: spl_token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Submit { upload }.data(),
        };
        Ok((ix, next))
    }

    pub fn ix_stage(&self, stager: &Pubkey, registration: &Registration) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Stage {
                stager: *stager,
                draft: draft_address(registration.id, stager),
                system_program: SYSTEM,
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

    /// What the builder signs for a registration.
    pub fn registration_digest(&self, registration: &Registration) -> [u8; 32] {
        shonin_publishing::state::registration_digest(
            &self.cluster,
            registration.id,
            &registration.author,
            &registration.source_hash,
            &registration.bundle_hash,
            &registration.manifest_hash,
            &registration.lists,
        )
    }

    /// What the Reviewer signs to clear a source for a strategy.
    pub fn clearance_digest(&self, id: u64, source_hash: &[u8; 32]) -> [u8; 32] {
        shonin_publishing::state::clearance_digest(&self.cluster, id, source_hash)
    }

    pub async fn ix_register(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        id: u64,
        builder: &Pubkey,
        reviewer: &Pubkey,
    ) -> Result<Instruction> {
        let [builder_key, builder_image, builder_project] =
            self.records_of(client, builder).await?;
        let [reviewer_key, reviewer_image, reviewer_project] =
            self.records_of(client, reviewer).await?;
        Ok(Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Register {
                payer: *payer,
                stager: *payer,
                draft: draft_address(id, payer),
                submission: submission_address(id),
                strategy: shonin_publishing::state::strategy_address(id),
                registry_config: registry::config_address(),
                builder_key,
                builder_image,
                builder_project,
                reviewer_key,
                reviewer_image,
                reviewer_project,
                instructions: INSTRUCTIONS,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Register {
                strategy_id: id,
                builder: *builder,
                reviewer: *reviewer,
            }
            .data(),
        })
    }

    /// Stage a registration and register it, carrying the builder's
    /// signature over it and the Reviewer's go-ahead, as the build service
    /// does. `payer` stages and pays.
    pub async fn register(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        registration: &Registration,
        builder: &Keypair,
        reviewer: &Keypair,
    ) -> Result<Landed> {
        client
            .send(vec![self.ix_stage(&payer.pubkey(), registration)], &[payer])
            .await
            .context("staging the registration")?;
        let ixs = vec![
            ed25519(builder, &self.registration_digest(registration)),
            ed25519(
                reviewer,
                &self.clearance_digest(registration.id, &registration.source_hash),
            ),
            self.ix_register(
                client,
                &payer.pubkey(),
                registration.id,
                &builder.pubkey(),
                &reviewer.pubkey(),
            )
            .await?,
        ];
        client.send(ixs, &[payer]).await
    }

    pub async fn ix_submit_review(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        reviewer: &Pubkey,
        id: u64,
        review: ReviewInput,
    ) -> Result<Instruction> {
        let [reviewer_key, reviewer_image, reviewer_project] =
            self.records_of(client, reviewer).await?;
        Ok(Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::SubmitReview {
                reviewer_accounts: shonin_publishing::accounts::ReviewerAccounts {
                    reviewer: *reviewer,
                    registry_config: registry::config_address(),
                    reviewer_key,
                    reviewer_image,
                    reviewer_project,
                },
                payer: *payer,
                review: review_address(id),
                strategy: shonin_publishing::state::strategy_address(id),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::SubmitReview {
                strategy_id: id,
                review,
            }
            .data(),
        })
    }

    /// The reviewer's refusal of a submitted strategy's source, `payer`
    /// paying for its record.
    pub async fn ix_submit_refusal(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        reviewer: &Pubkey,
        id: u64,
        review: ReviewInput,
    ) -> Result<Instruction> {
        let [reviewer_key, reviewer_image, reviewer_project] =
            self.records_of(client, reviewer).await?;
        Ok(Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::SubmitRefusal {
                reviewer_accounts: shonin_publishing::accounts::ReviewerAccounts {
                    reviewer: *reviewer,
                    registry_config: registry::config_address(),
                    reviewer_key,
                    reviewer_image,
                    reviewer_project,
                },
                payer: *payer,
                review: review_address(id),
                submission: submission_address(id),
                strategy: shonin_publishing::state::strategy_address(id),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::SubmitRefusal {
                strategy_id: id,
                review,
            }
            .data(),
        })
    }

    /// Stage a registration and register it with the builder's signature
    /// over it and the reviewer's go-ahead, each carried by an Ed25519
    /// instruction, as the build service does once both are signed.
    /// `stager` (the builder's key) stages it, pays and sends both.
    pub async fn register_signed(
        &self,
        client: &SolanaClient,
        stager: &Keypair,
        registration: &Registration,
        reviewer: &Pubkey,
        clearance: &[u8; 64],
    ) -> Result<Landed> {
        let digest = self.registration_digest(registration);
        let builder_signature: [u8; 64] = stager
            .sign_message(&digest)
            .as_ref()
            .try_into()
            .expect("an ed25519 signature is 64 bytes");
        let clearance_digest = self.clearance_digest(registration.id, &registration.source_hash);
        let staged: Option<shonin_publishing::state::Draft> = client
            .load(&draft_address(registration.id, &stager.pubkey()))
            .await?;
        match staged {
            // A draft from a try that went no further is used as it is if it
            // says the same, and given up otherwise.
            Some(draft)
                if draft.author == registration.author
                    && draft.source_hash == registration.source_hash
                    && draft.bundle_hash == registration.bundle_hash
                    && draft.manifest_hash == registration.manifest_hash
                    && draft.lists == registration.lists => {}
            Some(_) => {
                client
                    .send(
                        vec![self.ix_discard(&stager.pubkey(), registration.id)],
                        &[stager],
                    )
                    .await
                    .context("giving up a draft that says something else")?;
                client
                    .send(
                        vec![self.ix_stage(&stager.pubkey(), registration)],
                        &[stager],
                    )
                    .await
                    .context("staging the registration")?;
            }
            None => {
                client
                    .send(
                        vec![self.ix_stage(&stager.pubkey(), registration)],
                        &[stager],
                    )
                    .await
                    .context("staging the registration")?;
            }
        }
        let ixs = vec![
            solana_ed25519_program::new_ed25519_instruction_with_signature(
                &digest,
                &builder_signature,
                &stager.pubkey().to_bytes(),
            ),
            solana_ed25519_program::new_ed25519_instruction_with_signature(
                &clearance_digest,
                clearance,
                &reviewer.to_bytes(),
            ),
            self.ix_register(
                client,
                &stager.pubkey(),
                registration.id,
                &stager.pubkey(),
                reviewer,
            )
            .await?,
        ];
        client.send(ixs, &[stager]).await
    }

    pub fn ix_discard(&self, stager: &Pubkey, id: u64) -> Instruction {
        Instruction {
            program_id: shonin_publishing::ID,
            accounts: shonin_publishing::accounts::Discard {
                stager: *stager,
                draft: draft_address(id, stager),
            }
            .to_account_metas(None),
            data: shonin_publishing::instruction::Discard { strategy_id: id }.data(),
        }
    }

    /// The submission paying for strategy `id`, as the cluster has it at its
    /// finalized commitment: a payment that could still be rolled back is
    /// not read.
    pub async fn finalized_submission(
        &self,
        client: &SolanaClient,
        id: u64,
    ) -> Result<Option<shonin_publishing::state::Submission>> {
        client.load_finalized(&submission_address(id)).await
    }

    /// How many submissions publishing has taken, and the cluster's time, at
    /// its finalized commitment.
    pub async fn finalized_count(&self, client: &SolanaClient) -> Result<(u64, u64)> {
        let config: shonin_publishing::state::Config = client
            .load_finalized(&publishing_config())
            .await?
            .ok_or_else(|| anyhow!("publishing is not set up"))?;
        Ok((config.count, client.now_finalized().await?))
    }

    pub async fn strategy(
        &self,
        client: &SolanaClient,
        id: u64,
    ) -> Result<Option<shonin_publishing::state::Strategy>> {
        client
            .load(&shonin_publishing::state::strategy_address(id))
            .await
    }

    pub async fn review(
        &self,
        client: &SolanaClient,
        id: u64,
    ) -> Result<Option<shonin_publishing::state::Review>> {
        client.load(&review_address(id)).await
    }

    // Profiles ------------------------------------------------------------------------

    pub async fn ix_set_profile(
        &self,
        client: &SolanaClient,
        author: &Pubkey,
        handle: &str,
        blurb: &str,
    ) -> Result<Instruction> {
        let profile = profile_address(author);
        let old = client
            .load::<shonin_profiles::Profile>(&profile)
            .await?
            .filter(|profile| !profile.handle.is_empty() && profile.handle != handle)
            .map(|profile| shonin_profiles::handle_address(&profile.handle));
        Ok(Instruction {
            program_id: shonin_profiles::ID,
            accounts: shonin_profiles::accounts::SetProfile {
                author: *author,
                profile,
                new_handle: shonin_profiles::handle_address(handle),
                old_handle: old,
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_profiles::instruction::SetProfile {
                handle: handle.to_string(),
                handle_hash: keccak(&[handle.as_bytes()]),
                blurb: blurb.to_string(),
                link_labels: Vec::new(),
                link_urls: Vec::new(),
            }
            .data(),
        })
    }

    pub async fn profile(
        &self,
        client: &SolanaClient,
        author: &Pubkey,
    ) -> Result<Option<shonin_profiles::Profile>> {
        client.load(&profile_address(author)).await
    }

    // Vaults: reading ---------------------------------------------------------------

    pub async fn factory(&self, client: &SolanaClient) -> Result<vs::Factory> {
        client
            .load(&vs::factory_address())
            .await?
            .ok_or_else(|| anyhow!("the vault factory is not set up"))
    }

    pub async fn vault(&self, client: &SolanaClient, vault: &Pubkey) -> Result<Vault> {
        client
            .load(vault)
            .await?
            .ok_or_else(|| anyhow!("no vault at {vault}"))
    }

    pub async fn holder(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        owner: &Pubkey,
    ) -> Result<Option<Holder>> {
        client.load(&vs::holder_address(vault, owner).0).await
    }

    pub async fn shares(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        owner: &Pubkey,
    ) -> Result<u128> {
        Ok(self
            .holder(client, vault, owner)
            .await?
            .map_or(0, |holder| holder.shares))
    }

    pub async fn claim(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        id: u64,
    ) -> Result<Option<Claim>> {
        client.load(&vs::claim_address(vault, id).0).await
    }

    pub async fn station(
        &self,
        client: &SolanaClient,
        station: &Pubkey,
    ) -> Result<shonin_station::state::Station> {
        client
            .load(station)
            .await?
            .ok_or_else(|| anyhow!("no station at {station}"))
    }

    /// The vault's request `id`, wherever its maker put it: found among the
    /// vault program's accounts by the vault and the id it holds.
    pub async fn find_request(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        id: u64,
    ) -> Result<Option<(Pubkey, Request)>> {
        let found = client
            .rpc
            .program_accounts(
                &shonin_vault::ID,
                &[
                    (0, Request::DISCRIMINATOR.to_vec()),
                    (8, vault.to_bytes().to_vec()),
                    (40, id.to_le_bytes().to_vec()),
                ],
            )
            .await?;
        for (address, account) in found {
            let request = Request::deserialize(&mut &account.data[8..])
                .context("a request account that doesn't read as one")?;
            if request.vault == *vault && request.id == id {
                return Ok(Some((address, request)));
            }
        }
        Ok(None)
    }

    /// The list a strategy's vault will refuse, and its hash: the record's
    /// own list merged with each driver's, as the vault merges them.
    pub async fn restricted_for(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        id: u64,
        drivers: &[Declared],
    ) -> Result<(Vec<[u8; 32]>, [u8; 32])> {
        let record = self
            .strategy(client, id)
            .await?
            .ok_or_else(|| anyhow!("strategy {id} is not registered"))?;
        let mut lists = Vec::new();
        for driver in drivers {
            let ask = Instruction {
                program_id: driver.program,
                accounts: vec![],
                data: shonin_driver::discriminator::RESTRICTED.to_vec(),
            };
            let simulated = client.simulate(vec![ask], payer).await?;
            let (_, data) = simulated
                .return_data
                .ok_or_else(|| anyhow!("a driver answered no restricted list"))?;
            lists.push(Vec::<[u8; 32]>::try_from_slice(&data)?);
        }
        let list =
            shonin_vault::codes::merged(&record.lists.restricted, lists.iter().map(Vec::as_slice));
        let hash = shonin_vault::codes::hash(&list);
        Ok((list, hash))
    }

    /// The venue `driver` trades at, as its allowlist says: where a station
    /// created with it gets its authority.
    pub async fn venue_of(
        &self,
        client: &SolanaClient,
        payer: &Pubkey,
        driver: &Pubkey,
    ) -> Result<Pubkey> {
        let ask = Instruction {
            program_id: *driver,
            accounts: vec![],
            data: shonin_driver::discriminator::ALLOWLIST.to_vec(),
        };
        let (_, data) = client
            .simulate(vec![ask], payer)
            .await?
            .return_data
            .ok_or_else(|| anyhow!("the driver {driver} answered no allowlist"))?;
        Ok(shonin_driver::Allowlist::try_from_slice(&data)?.venue)
    }

    // Vaults: the author -----------------------------------------------------------------

    /// The author's `activate` of strategy `id`, its station pinning
    /// `drivers`, stating for the list `statement` hashes and the factory's
    /// terms `terms` hashes.
    #[allow(clippy::too_many_arguments)]
    pub async fn ix_activate(
        &self,
        client: &SolanaClient,
        author: &Pubkey,
        id: u64,
        drivers: &[Declared],
        allowances: Allowances,
        idle_floor_bps: u16,
        tank: u64,
        statement: [u8; 32],
        terms: [u8; 32],
    ) -> Result<Instruction> {
        let v = self.vault_accounts(id);
        let mut accounts = shonin_vault::accounts::Activate {
            author: *author,
            factory: vs::factory_address(),
            strategy: shonin_publishing::state::strategy_address(id),
            review: review_address(id),
            vault: v.vault,
            restricted: vs::restricted_address(&v.vault).0,
            author_holder: vs::holder_address(&v.vault, author).0,
            platform_holder: vs::holder_address(&v.vault, &self.platform).0,
            vault_dollars: v.vault_dollars,
            author_dollars: self.dollars(author),
            mint: self.mint,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: SYSTEM,
            station_program: shonin_station::ID,
            station_config: shonin_station::config_address(),
            station: v.station,
            station_dollars: v.station_dollars,
        }
        .to_account_metas(None);
        let now = client.now().await?;
        let mut sizes = Vec::new();
        for driver in drivers {
            // The implementation a station created now pins, and the
            // revocation record that says whether it is revoked.
            let slot = registry::approval_address(ApprovalKind::Driver, &driver.name);
            let effective = client
                .load::<ApprovalSlot>(&slot)
                .await?
                .map(|slot| slot.effective(now))
                .unwrap_or_default();
            if effective != driver.program {
                bail!(
                    "the registry doesn't approve {} for {} yet",
                    driver.program,
                    String::from_utf8_lossy(&driver.name).trim_end_matches('\0')
                );
            }
            let revoked = registry::revoked_address(ApprovalKind::Driver, &driver.name, &effective);
            let venue = self.venue_of(client, author, &driver.program).await?;
            let authority = venue_authority(&v.station, &venue);
            let group = [
                AccountMeta::new_readonly(driver.program, false),
                AccountMeta::new_readonly(program_data_address(&driver.program), false),
                AccountMeta::new_readonly(slot, false),
                AccountMeta::new_readonly(revoked, false),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new(self.dollars(&authority), false),
            ];
            sizes.push(group.len() as u8);
            accounts.extend(group);
        }
        Ok(Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::Activate {
                args: shonin_vault::ActivateArgs {
                    strategy_id: id,
                    allowances,
                    idle_floor_bps,
                    tank_funding: tank,
                    tank_keep: 0,
                    statement,
                    terms,
                    driver_accounts: sizes,
                    previous: 0,
                },
            }
            .data(),
        })
    }

    /// `funder` puts `assets` of its own dollars into the vault's tank.
    pub fn ix_fund_tank(&self, funder: &Pubkey, vault: &Pubkey, assets: u64) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::FundTank {
                funder: *funder,
                vault: *vault,
                from: self.dollars(funder),
                vault_dollars: self.dollars(vault),
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::FundTank { assets }.data(),
        }
    }

    /// The author keeps the tank at `assets` (auto-fill), from its fee
    /// shares.
    pub fn ix_set_tank_keep(&self, author: &Pubkey, vault: &Pubkey, assets: u64) -> Instruction {
        self.ix_author(
            author,
            vault,
            shonin_vault::instruction::SetTankKeep { assets }.data(),
        )
    }

    /// What `allowances` cost a day at the price list in force now, in the
    /// dollar's units: what the vault's tank pays for at most.
    pub async fn daily_cost(&self, client: &SolanaClient, allowances: &Allowances) -> Result<u64> {
        let address = shonin_prices::read::prices_address();
        let account = client
            .account(&address)
            .await?
            .ok_or_else(|| anyhow!("the price list is not set up"))?;
        let now = client.now().await?;
        let cost = shonin_prices::read::History::new(&address, &account.owner, &account.data)?
            .daily_cost(allowances, now)?;
        cost.to_string()
            .parse()
            .map_err(|_| anyhow!("a day's cost past what the dollar holds"))
    }

    pub fn ix_author(&self, author: &Pubkey, vault: &Pubkey, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Author {
                author: *author,
                vault: *vault,
            }
            .to_account_metas(None),
            data,
        }
    }

    pub fn ix_retire(&self, author: &Pubkey, vault: &Pubkey) -> Instruction {
        self.ix_author(author, vault, shonin_vault::instruction::Retire {}.data())
    }

    pub fn ix_kill(&self, admin: &Pubkey, vault: &Pubkey) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::Admin {
                admin: *admin,
                vault: *vault,
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Kill {}.data(),
        }
    }

    // Vaults: depositors -------------------------------------------------------------------

    /// `payer` asks to deposit `assets` for `receiver`, stating for the list
    /// whose hash is `statement`; the request is the payer's own account at
    /// `nonce`.
    pub fn ix_request_deposit(
        &self,
        vault: &Pubkey,
        payer: &Pubkey,
        receiver: &Pubkey,
        assets: u64,
        statement: Option<[u8; 32]>,
        nonce: u64,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RequestDeposit {
                payer: *payer,
                vault: *vault,
                holder: vs::holder_address(vault, receiver).0,
                request: vs::request_address(vault, payer, nonce).0,
                payer_dollars: self.dollars(payer),
                vault_dollars: self.dollars(vault),
                mint: self.mint,
                token_program: spl_token::ID,
                system_program: SYSTEM,
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

    pub fn ix_request_redeem(
        &self,
        vault: &Pubkey,
        owner: &Pubkey,
        shares: u128,
        receiver: &Pubkey,
        nonce: u64,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RequestRedeem {
                owner: *owner,
                vault: *vault,
                holder: vs::holder_address(vault, owner).0,
                request: vs::request_address(vault, owner, nonce).0,
                system_program: SYSTEM,
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

    /// A holder of a closed vault takes their share of what came home.
    pub fn ix_redeem_closed(
        &self,
        v: &VaultAccounts,
        owner: &Pubkey,
        shares: u128,
        receiver: &Pubkey,
    ) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::RedeemClosed {
                owner: *owner,
                vault: v.vault,
                holder: vs::holder_address(&v.vault, owner).0,
                station: v.station,
                to: self.dollars(receiver),
                vault_dollars: v.vault_dollars,
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

    // Vaults: the keeper ------------------------------------------------------------------

    pub fn pool(&self, v: &VaultAccounts, vault: &Vault) -> shonin_vault::accounts::Pool {
        shonin_vault::accounts::Pool {
            vault: v.vault,
            station: v.station,
            author_holder: vs::holder_address(&v.vault, &vault.author).0,
            platform_holder: vs::holder_address(&v.vault, &vault.platform).0,
            vault_dollars: v.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            system_program: SYSTEM,
        }
    }

    /// The accounts the stand-in driver is given for `station`: the program,
    /// the station's authority at the venue and its dollar account, then the
    /// venue's accounts and every market the authority holds there.
    pub async fn driver_group(
        &self,
        client: &SolanaClient,
        station: &Pubkey,
        program: &Pubkey,
    ) -> Result<Vec<AccountMeta>> {
        let record = self.station(client, station).await?;
        let pinned = record
            .drivers
            .iter()
            .find(|driver| driver.program == *program)
            .ok_or_else(|| anyhow!("the station {station} pins no driver {program}"))?;
        self.pinned_group(client, pinned).await
    }

    /// The accounts a pinned driver is given: the program, the station's
    /// authority at its venue and that authority's dollar account, then the
    /// stand-in venue's layout at that venue (its own or a copy's), with
    /// every market the authority holds there.
    async fn pinned_group(
        &self,
        client: &SolanaClient,
        pinned: &shonin_station::state::Pinned,
    ) -> Result<Vec<AccountMeta>> {
        let venue = pinned.venue;
        let authority = pinned.authority;
        let mut group = vec![
            AccountMeta::new_readonly(pinned.program, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(pinned.dollars, false),
            AccountMeta::new_readonly(venue, false),
            AccountMeta::new_readonly(shonin_local_venue::venue_address_of(&venue), false),
            AccountMeta::new(shonin_local_venue::vault_address_of(&venue), false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new(
                shonin_local_venue::account_address_of(&venue, &authority),
                false,
            ),
        ];
        group.extend(
            self.held_markets_at(client, &venue, &authority)
                .await?
                .into_iter()
                .map(|market| AccountMeta::new_readonly(market, false)),
        );
        Ok(group)
    }

    /// One group per driver the station pins, and their sizes.
    pub async fn groups(
        &self,
        client: &SolanaClient,
        station: &Pubkey,
    ) -> Result<(Vec<AccountMeta>, Vec<u8>)> {
        let record = self.station(client, station).await?;
        let mut accounts = Vec::new();
        let mut sizes = Vec::new();
        for driver in &record.drivers {
            let group = self.pinned_group(client, driver).await?;
            sizes.push(group.len() as u8);
            accounts.extend(group);
        }
        Ok((accounts, sizes))
    }

    pub async fn ix_record_valuation(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
    ) -> Result<Instruction> {
        let vault = self.vault(client, &v.vault).await?;
        let (groups, sizes) = self.groups(client, &v.station).await?;
        let mut accounts = shonin_vault::accounts::RecordValuation {
            pool: self.pool(v, &vault),
            station_program: shonin_station::ID,
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Ok(Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::RecordValuation { groups: sizes }.data(),
        })
    }

    /// One request's accounts in a settlement, as its state now says: a
    /// request that is gone became a claim, which stands in for it.
    async fn entry(
        &self,
        client: &SolanaClient,
        vault: &Pubkey,
        id: u64,
    ) -> Result<Vec<AccountMeta>> {
        let Some((address, request)) = self.find_request(client, vault, id).await? else {
            return Ok(vec![AccountMeta::new(
                vs::claim_address(vault, id).0,
                false,
            )]);
        };
        let mut out = vec![AccountMeta::new(address, false)];
        if request.done {
            return Ok(out);
        }
        match request.kind {
            RequestKind::Deposit => {
                out.push(AccountMeta::new(
                    vs::holder_address(vault, &request.owner).0,
                    false,
                ));
                out.push(AccountMeta::new(self.dollars(&request.payer), false));
            }
            RequestKind::Redeem => {
                out.push(AccountMeta::new(vs::claim_address(vault, id).0, false));
                out.push(AccountMeta::new(self.dollars(&request.owner), false));
            }
            RequestKind::Claim => {
                out.push(AccountMeta::new(
                    vs::claim_address(vault, request.reference).0,
                    false,
                ));
                out.push(AccountMeta::new(self.dollars(&request.owner), false));
            }
            RequestKind::TankFill => {}
        }
        Ok(out)
    }

    /// Whether settlement has nothing more to do for request `id` than pass
    /// it: it is done, or it became a claim.
    async fn passable(&self, client: &SolanaClient, vault: &Pubkey, id: u64) -> Result<bool> {
        Ok(match self.find_request(client, vault, id).await? {
            Some((_, request)) => request.done,
            None => self.claim(client, vault, id).await?.is_some(),
        })
    }

    /// Where the next settlement starts, or `None` when there is nothing to
    /// settle: the exits from their cursor once they are a whole batch
    /// ahead, otherwise from the request cursor.
    pub async fn settle_from(
        &self,
        client: &SolanaClient,
        vault: &Vault,
        address: &Pubkey,
    ) -> Result<Option<u64>> {
        if !vault.priced {
            return Ok(None);
        }
        let cursor = vault.request_cursor;
        let through = vault.priced_through;
        let done_next =
            cursor < vault.request_count && self.passable(client, address, cursor + 1).await?;
        if cursor >= through && !done_next {
            return Ok(None);
        }
        let exits = vault.exit_cursor.max(cursor);
        if exits < through && exits >= cursor + vs::MAX_SETTLE as u64 {
            return Ok(Some(exits));
        }
        Ok(Some(cursor))
    }

    pub async fn ix_settle(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        vault: &Vault,
        from: u64,
    ) -> Result<Instruction> {
        let mut accounts = shonin_vault::accounts::Settle {
            pool: self.pool(v, vault),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        let last = vault.request_count.min(from + vs::MAX_SETTLE as u64);
        for id in from + 1..=last {
            accounts.extend(self.entry(client, &v.vault, id).await?);
        }
        Ok(Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::Settle { from }.data(),
        })
    }

    /// Record this slot's valuation, as anyone may.
    pub async fn value(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        v: &VaultAccounts,
    ) -> Result<Landed> {
        let ix = self.ix_record_valuation(client, v).await?;
        client.send(vec![ix], &[payer]).await
    }

    /// Settle everything the last priced valuation owes, a batch at a time,
    /// as the keeper does. Answers how many settlements it sent.
    pub async fn settle_all(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        v: &VaultAccounts,
    ) -> Result<usize> {
        for sent in 0..1_000 {
            let vault = self.vault(client, &v.vault).await?;
            let Some(from) = self.settle_from(client, &vault, &v.vault).await? else {
                return Ok(sent);
            };
            let ix = self.ix_settle(client, v, &vault, from).await?;
            client.send(vec![ix], &[payer]).await?;
            let after = self.vault(client, &v.vault).await?;
            if after.request_cursor == vault.request_cursor
                && after.exit_cursor == vault.exit_cursor
            {
                bail!("settlement of {} made no progress", v.vault);
            }
        }
        bail!("settlement of {} never finished", v.vault)
    }

    pub async fn ix_settle_claim(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        caller: &Pubkey,
        claim_id: u64,
    ) -> Result<Instruction> {
        let vault = self.vault(client, &v.vault).await?;
        let claim = self.claim(client, &v.vault, claim_id).await?;
        let owner = claim.as_ref().map_or(Pubkey::default(), |c| c.owner);
        let claim_address = vs::claim_address(&v.vault, claim_id).0;
        let (groups, sizes) = self.groups(client, &v.station).await?;
        let mut accounts = shonin_vault::accounts::SettleClaim {
            caller: *caller,
            pool: self.pool(v, &vault),
            close: shonin_vault::accounts::Closing {
                station_program: shonin_station::ID,
                station_dollars: v.station_dollars,
            },
            claim: claim_address,
            request: Some(
                vs::request_address(
                    &v.vault,
                    &claim_address,
                    claim.as_ref().map_or(0, |c| c.request),
                )
                .0,
            ),
            owner_dollars: self.dollars(&owner),
            author_dollars: self.dollars(&vault.author),
            platform_dollars: self.dollars(&vault.platform),
            event_authority: event_authority(),
            program: shonin_vault::ID,
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Ok(Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data: shonin_vault::instruction::SettleClaim {
                claim_id,
                groups: sizes,
            }
            .data(),
        })
    }

    pub fn ix_close_vault(&self, v: &VaultAccounts, vault: &Vault) -> Instruction {
        Instruction {
            program_id: shonin_vault::ID,
            accounts: shonin_vault::accounts::CloseVault {
                pool: self.pool(v, vault),
            }
            .to_account_metas(None),
            data: shonin_vault::instruction::Close {}.data(),
        }
    }

    // The station -----------------------------------------------------------------

    pub fn ix_stop(&self, v: &VaultAccounts) -> Instruction {
        Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::Stop {
                station: v.station,
                vault: v.vault,
                vault_program: shonin_vault::ID,
            }
            .to_account_metas(None),
            data: shonin_station::instruction::Stop {}.data(),
        }
    }

    /// A passer-by's unwind of `fraction` (of `WAD`) of the station.
    pub async fn ix_unwind(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        caller: &Pubkey,
        fraction: u64,
    ) -> Result<Instruction> {
        let (groups, sizes) = self.groups(client, &v.station).await?;
        let mut accounts = shonin_station::accounts::Unwind {
            caller: *caller,
            station: v.station,
            station_dollars: v.station_dollars,
            vault_dollars: v.vault_dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            registry_config: None,
            blessing: None,
            key_info: None,
            image: None,
            project: None,
            vault: Some(v.vault),
            vault_program: Some(shonin_vault::ID),
        }
        .to_account_metas(None);
        accounts.extend(groups);
        Ok(Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Unwind {
                fraction,
                groups: sizes,
            }
            .data(),
        })
    }

    /// A halted vault brought to its close: the station stopped once the
    /// halt's grace has passed, unwound in full, the vault valued and
    /// settled, and closed. Each step is skipped once done, so a wind-up
    /// interrupted part way is finished by calling it again.
    pub async fn wind_up(
        &self,
        client: &SolanaClient,
        payer: &Keypair,
        v: &VaultAccounts,
    ) -> Result<()> {
        let vault = self.vault(client, &v.vault).await?;
        if vault.state == VaultState::Closed {
            return Ok(());
        }
        if !self.station(client, &v.station).await?.stopped {
            if client.now().await? < vault.halted_at + vault.timers.halt_grace {
                bail!("the halt's grace has not passed yet");
            }
            client
                .send(vec![self.ix_stop(v)], &[payer])
                .await
                .context("stopping the station")?;
        }
        self.settle_all(client, payer, v).await?;
        let unwind = self
            .ix_unwind(client, v, &payer.pubkey(), shonin_driver::WAD)
            .await?;
        client
            .send(vec![unwind], &[payer])
            .await
            .context("unwinding the station")?;
        self.value(client, payer, v)
            .await
            .context("valuing the stopped vault")?;
        self.settle_all(client, payer, v).await?;
        let vault = self.vault(client, &v.vault).await?;
        client
            .send(vec![self.ix_close_vault(v, &vault)], &[payer])
            .await
            .context("closing the vault")?;
        Ok(())
    }

    /// `caller` (the key blessed for the vault) approves `key` for the
    /// station's driver `index`; the records of the key in place come along
    /// when there is one.
    pub async fn ix_approve_key(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        bundle: &[u8; 32],
        caller: &Pubkey,
        index: usize,
        key: &Pubkey,
    ) -> Result<Instruction> {
        let station = self.station(client, &v.station).await?;
        let driver = station
            .drivers
            .get(index)
            .ok_or_else(|| anyhow!("the station has no driver {index}"))?;
        let [config, blessing, key_info, image, project] =
            self.key_records(client, &v.vault, bundle, key).await?;
        let slot = registry::approval_address(ApprovalKind::Driver, &driver.name);
        let revoked =
            registry::revoked_address(ApprovalKind::Driver, &driver.name, &driver.program);
        let current = match station
            .drivers
            .iter()
            .map(|d| d.key)
            .find(|held| *held != Pubkey::default() && held != key)
        {
            Some(held) => Some(self.key_records(client, &v.vault, bundle, &held).await?),
            None => None,
        };
        Ok(Instruction {
            program_id: shonin_station::ID,
            accounts: shonin_station::accounts::ApproveKey {
                caller: *caller,
                station: v.station,
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
        })
    }

    /// The station's approved key moves `amount` of the vault's cash to the
    /// station (`to`) or back.
    pub async fn ix_move(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        bundle: &[u8; 32],
        key: &Pubkey,
        amount: u64,
        to: bool,
    ) -> Result<Instruction> {
        let [config, blessing, key_info, image, project] =
            self.key_records(client, &v.vault, bundle, key).await?;
        let accounts = shonin_vault::accounts::Move {
            key: *key,
            vault: v.vault,
            station: v.station,
            station_program: shonin_station::ID,
            station_dollars: v.station_dollars,
            vault_dollars: v.vault_dollars,
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
        Ok(Instruction {
            program_id: shonin_vault::ID,
            accounts,
            data,
        })
    }

    /// `key` sends `venue_ix` through the station as driver `index`'s,
    /// first moving `fund` of the station's counted dollars to that venue's
    /// authority. The guard checks it against the driver's allowlist.
    #[allow(clippy::too_many_arguments)]
    pub async fn ix_execute(
        &self,
        client: &SolanaClient,
        v: &VaultAccounts,
        bundle: &[u8; 32],
        key: &Pubkey,
        index: u8,
        venue_ix: Instruction,
        fund: u64,
    ) -> Result<Instruction> {
        let station = self.station(client, &v.station).await?;
        let driver = station
            .drivers
            .get(index as usize)
            .ok_or_else(|| anyhow!("the station has no driver {index}"))?;
        let [config, blessing, key_info, image, project] =
            self.key_records(client, &v.vault, bundle, key).await?;
        let mut accounts = shonin_station::accounts::Execute {
            key: *key,
            station: v.station,
            station_dollars: v.station_dollars,
            authority: driver.authority,
            authority_dollars: driver.dollars,
            mint: self.mint,
            token_program: spl_token::ID,
            registry_config: config,
            blessing,
            key_info,
            image,
            project,
            slot: registry::approval_address(ApprovalKind::Driver, &driver.name),
            revoked: registry::revoked_address(ApprovalKind::Driver, &driver.name, &driver.program),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new_readonly(venue_ix.program_id, false));
        let authorities: Vec<Pubkey> = station.drivers.iter().map(|d| d.authority).collect();
        accounts.extend(venue_ix.accounts.into_iter().map(|mut meta| {
            // The station signs for its authorities; the transaction can't.
            if meta.pubkey == v.station || authorities.contains(&meta.pubkey) {
                meta.is_signer = false;
            }
            meta
        }));
        Ok(Instruction {
            program_id: shonin_station::ID,
            accounts,
            data: shonin_station::instruction::Execute {
                driver: index,
                data: venue_ix.data,
                fund,
            }
            .data(),
        })
    }

    // Stand-in venue calls, made through a station ----------------------------------------

    pub fn venue_open(&self, payer: &Pubkey, master: &Pubkey) -> Instruction {
        self.venue_open_at(&shonin_local_venue::ID, payer, master)
    }

    /// The same at `venue`, the stand-in venue or one of its copies, as
    /// each `_at` call below.
    pub fn venue_open_at(&self, venue: &Pubkey, payer: &Pubkey, master: &Pubkey) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::OpenAccount {
                payer: *payer,
                master: *master,
                account: shonin_local_venue::account_address_of(venue, master),
                system_program: SYSTEM,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::OpenAccount {}.data(),
        }
    }

    pub fn venue_deposit(&self, master: &Pubkey, amount: u64) -> Instruction {
        self.venue_deposit_at(&shonin_local_venue::ID, master, amount)
    }

    pub fn venue_deposit_at(&self, venue: &Pubkey, master: &Pubkey, amount: u64) -> Instruction {
        Instruction {
            program_id: *venue,
            accounts: shonin_local_venue::accounts::Deposit {
                master: *master,
                account: shonin_local_venue::account_address_of(venue, master),
                master_dollars: self.dollars(master),
                venue: shonin_local_venue::venue_address_of(venue),
                vault: shonin_local_venue::vault_address_of(venue),
                mint: self.mint,
                token_program: spl_token::ID,
            }
            .to_account_metas(None),
            data: shonin_local_venue::instruction::Deposit { amount }.data(),
        }
    }

    pub async fn venue_place(
        &self,
        client: &SolanaClient,
        master: &Pubkey,
        orders: Vec<shonin_local_venue::OrderRequest>,
    ) -> Result<Instruction> {
        self.venue_place_at(client, &shonin_local_venue::ID, master, orders)
            .await
    }

    pub async fn venue_place_at(
        &self,
        client: &SolanaClient,
        venue: &Pubkey,
        master: &Pubkey,
        orders: Vec<shonin_local_venue::OrderRequest>,
    ) -> Result<Instruction> {
        let mut accounts = shonin_local_venue::accounts::Place {
            master: *master,
            account: shonin_local_venue::account_address_of(venue, master),
            venue: shonin_local_venue::venue_address_of(venue),
        }
        .to_account_metas(None);
        let mut markets = self.held_markets_at(client, venue, master).await?;
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
        Ok(Instruction {
            program_id: *venue,
            accounts,
            data: shonin_local_venue::instruction::Place { orders }.data(),
        })
    }
}

/// The station's signing authority at `venue`.
pub fn venue_authority(station: &Pubkey, venue: &Pubkey) -> Pubkey {
    shonin_station::venue_authority(station, venue).0
}

/// An Ed25519 instruction verifying `signer`'s signature over `message`: how
/// a signature travels in someone else's transaction.
pub fn ed25519(signer: &Keypair, message: &[u8]) -> Instruction {
    let signature: [u8; 64] = signer
        .sign_message(message)
        .as_ref()
        .try_into()
        .expect("an ed25519 signature is 64 bytes");
    solana_ed25519_program::new_ed25519_instruction_with_signature(
        message,
        &signature,
        &signer.pubkey().to_bytes(),
    )
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
