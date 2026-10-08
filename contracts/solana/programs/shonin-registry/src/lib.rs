//! Which code is allowed to hold a key, and which keys it currently holds.
//!
//! An instance proves what it is, and gets a key blessed for exactly the work
//! that image does: an executor or unwinder key for one vault and one bundle,
//! a builder or a reviewer key for no vault at all. Everything downstream (a
//! station approving a trading key, a registry accepting a build, a review
//! being believed) asks this program's records and nothing else; `read` is how.
//!
//! Adding anything is timelocked and public, so an outsider can rebuild it,
//! disagree, and leave before it counts. Removing anything is immediate,
//! because the reason to remove one is always urgent. The admin's powers are
//! exactly those: propose behind the timelock, revoke at once, and set the
//! timelock within its floor and ceiling. It can't bless a key, change what a
//! key is blessed for, or move anything.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke, set_return_data},
};
use shonin_common::{checker, digest, loader, time::now, MAINNET_CLUSTER, MAX_TIMELOCK};

pub mod error;
pub mod events;
pub mod read;
pub mod state;

use error::RegistryError;
use events::*;
use state::*;

// The address comes from build.rs: the committed mainnet id, or the keypair
// this machine deploys with. No other address is ever written down.
include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

#[program]
pub mod shonin_registry {
    use super::*;

    /// Sets a fresh deployment up, once, signed by the key that deployed it
    /// (its upgrade authority, which is then removed on mainnet). The first
    /// checker is trusted from the start: nothing can have been blessed
    /// before it, so there is nothing for a timelock to protect.
    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        require!(
            loader::is_upgrade_authority(
                &ctx.accounts.program,
                Some(&ctx.accounts.program_data),
                ctx.accounts.deployer.key,
            ),
            RegistryError::NotTheDeployer
        );
        // A mainnet build serves mainnet alone, with the floor mainnet needs;
        // no other build ever names mainnet.
        if cfg!(feature = "mainnet") {
            require!(
                args.cluster_name == MAINNET_CLUSTER,
                RegistryError::WrongCluster
            );
            require!(
                args.min_timelock >= 2 * shonin_common::DAY,
                RegistryError::TooShort
            );
        } else {
            require!(
                args.cluster_name != MAINNET_CLUSTER,
                RegistryError::WrongCluster
            );
        }
        require!(
            args.timelock <= MAX_TIMELOCK && args.min_timelock <= MAX_TIMELOCK,
            RegistryError::TooLong
        );
        require!(args.timelock >= args.min_timelock, RegistryError::TooShort);
        let first = check_new_verifier(
            &ctx.accounts.verifier_program,
            ctx.accounts.verifier_program_data.as_deref(),
            &ctx.accounts.verifier_state,
        )?;

        let now = now()?;
        let cluster = digest::cluster_value(&args.cluster_name);
        let entry = VerifierEntry {
            program: first.0,
            state: first.1,
            ready_at: now,
        };
        ctx.accounts.config.set_inner(Config::new(
            args.admin,
            cluster,
            args.timelock,
            args.min_timelock,
            args.blessing_ttl,
            entry,
            ctx.bumps.config,
        ));
        ctx.accounts.verifier_record.set_inner(VerifierRecord {
            program: first.0,
            state: first.1,
            proposed: true,
            bump: ctx.bumps.verifier_record,
        });
        emit!(Initialized {
            admin: args.admin,
            cluster_name: args.cluster_name,
            cluster,
            timelock: args.timelock,
            min_timelock: args.min_timelock,
            blessing_ttl: args.blessing_ttl,
        });
        emit!(VerifierProposed {
            program: first.0,
            state: first.1,
            ready_at: now,
        });
        Ok(())
    }

    // The timelock --------------------------------------------------------

    /// Raising applies at once; lowering applies after the timelock in force
    /// when it was asked for.
    pub fn set_timelock(ctx: Context<Admin>, new_timelock: u64) -> Result<()> {
        let config = &mut ctx.accounts.config;
        require!(new_timelock <= MAX_TIMELOCK, RegistryError::TooLong);
        require!(new_timelock >= config.min_timelock, RegistryError::TooShort);
        let now = now()?;
        let current = config.timelock(now);
        if new_timelock >= current {
            // Making everyone wait longer can only protect them.
            config.set_timelock_now(new_timelock);
            emit!(TimelockSet {
                timelock: new_timelock,
                effective_at: now,
            });
        } else {
            // Cutting the wait has to wait out the wait it cuts, so it can
            // never be used to slip something in quietly.
            let effective_at = now.checked_add(current).ok_or(RegistryError::Overflow)?;
            config.set_timelock_later(current, new_timelock, effective_at);
            emit!(TimelockSet {
                timelock: new_timelock,
                effective_at,
            });
        }
        Ok(())
    }

    // Images and projects -------------------------------------------------

    pub fn propose_image(
        ctx: Context<ImageChange>,
        image_digest: [u8; 32],
        kind: ImageKind,
    ) -> Result<()> {
        require!(kind != ImageKind::None, RegistryError::NoKind);
        let image = &mut ctx.accounts.image;
        require!(!image.proposed, RegistryError::AlreadyProposed);
        let ready_at = ready_at(&ctx.accounts.config)?;
        // A revocation of an image never proposed revoked nothing.
        image.set_inner(ImageRecord {
            kind,
            proposed: true,
            allowed_at: ready_at,
            revoked: false,
            bump: ctx.bumps.image,
        });
        emit!(ImageProposed {
            image_digest,
            kind,
            ready_at,
        });
        Ok(())
    }

    pub fn revoke_image(ctx: Context<ImageChange>, image_digest: [u8; 32]) -> Result<()> {
        let image = &mut ctx.accounts.image;
        image.revoked = true;
        image.bump = ctx.bumps.image;
        emit!(ImageRevoked { image_digest });
        Ok(())
    }

    pub fn propose_project(ctx: Context<ProjectChange>, project_id: [u8; 32]) -> Result<()> {
        let project = &mut ctx.accounts.project;
        require!(!project.proposed, RegistryError::AlreadyProposed);
        let ready_at = ready_at(&ctx.accounts.config)?;
        // A revocation of a project never proposed revoked nothing.
        project.set_inner(ProjectRecord {
            proposed: true,
            ready_at,
            revoked: false,
            bump: ctx.bumps.project,
        });
        emit!(ProjectProposed {
            project_id,
            ready_at,
        });
        Ok(())
    }

    pub fn revoke_project(ctx: Context<ProjectChange>, project_id: [u8; 32]) -> Result<()> {
        let project = &mut ctx.accounts.project;
        project.revoked = true;
        project.bump = ctx.bumps.project;
        emit!(ProjectRevoked { project_id });
        Ok(())
    }

    // Attestation checkers ------------------------------------------------

    /// A new checker: ready after the timelock, while the ones already
    /// allowed keep working. A checker that was ever proposed can never be
    /// proposed again, so a retired one stays retired.
    pub fn propose_verifier(ctx: Context<ProposeVerifier>) -> Result<()> {
        let (program, state) = check_new_verifier(
            &ctx.accounts.verifier_program,
            ctx.accounts.verifier_program_data.as_deref(),
            &ctx.accounts.verifier_state,
        )?;
        require!(
            !ctx.accounts.verifier_record.proposed,
            RegistryError::AlreadyProposed
        );
        let ready_at = ready_at(&ctx.accounts.config)?;
        let config = &mut ctx.accounts.config;
        require!(
            config.verifiers.len() < MAX_VERIFIERS,
            RegistryError::TooManyVerifiers
        );
        config.verifiers.push(VerifierEntry {
            program,
            state,
            ready_at,
        });
        ctx.accounts.verifier_record.set_inner(VerifierRecord {
            program,
            state,
            proposed: true,
            bump: ctx.bumps.verifier_record,
        });
        emit!(VerifierProposed {
            program,
            state,
            ready_at,
        });
        Ok(())
    }

    /// At once: every key it blessed stops counting, until an allowed
    /// checker vouches for it again.
    pub fn retire_verifier(ctx: Context<Admin>, state: Pubkey) -> Result<()> {
        ctx.accounts
            .config
            .verifiers
            .retain(|entry| entry.state != state);
        emit!(VerifierRetired { state });
        Ok(())
    }

    // Drivers and bridges -------------------------------------------------

    /// The on-chain half of a driver, or a bridge, by its name in
    /// `shonin.toml`. It is approved once its timelock passes, and then only
    /// for stations created afterwards. It must be a program nobody can ever
    /// change, since a station pins it for good. A second proposal before the
    /// first is ready replaces it, and waits its own full timelock; proposing
    /// a revoked implementation again is approving it again, after a full
    /// timelock like anything else.
    pub fn propose(
        ctx: Context<Propose>,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> Result<()> {
        require_keys_neq!(
            implementation,
            Pubkey::default(),
            RegistryError::NoImplementation
        );
        require!(
            loader::is_final(
                &ctx.accounts.implementation_program,
                ctx.accounts.implementation_data.as_deref(),
            ),
            RegistryError::NotFinal
        );
        let now = now()?;
        let ready_at = ready_at(&ctx.accounts.config)?;
        let slot = &mut ctx.accounts.slot;
        start(slot, kind, name, ctx.bumps.slot);
        settle(slot, ctx.accounts.next_revoked.as_deref(), None, now)?;
        slot.next = implementation;
        slot.next_ready_at = ready_at;
        emit!(Proposed {
            kind,
            name,
            implementation,
            ready_at,
        });
        Ok(())
    }

    /// Withdraws the proposal still waiting out its timelock. What is in
    /// force, and every station that pinned it, is untouched.
    pub fn withdraw_proposal(
        ctx: Context<WithdrawProposal>,
        kind: ApprovalKind,
        name: [u8; 32],
    ) -> Result<()> {
        let now = now()?;
        let slot = &mut ctx.accounts.slot;
        start(slot, kind, name, ctx.bumps.slot);
        settle(slot, ctx.accounts.next_revoked.as_deref(), None, now)?;
        let withdrawn = slot.next;
        require_keys_neq!(
            withdrawn,
            Pubkey::default(),
            RegistryError::NoImplementation
        );
        slot.next = Pubkey::default();
        slot.next_ready_at = 0;
        emit!(ProposalWithdrawn {
            kind,
            name,
            implementation: withdrawn,
        });
        Ok(())
    }

    /// Revokes an implementation that was approved: the one in force, or an
    /// older one stations still pin. They stop trading through it at once and
    /// keep valuing and closing through it. A proposal of the same
    /// implementation still waiting is withdrawn with it, so the revocation
    /// can't lapse when that proposal would have come due.
    pub fn revoke(
        ctx: Context<Revoke>,
        kind: ApprovalKind,
        name: [u8; 32],
        implementation: Pubkey,
    ) -> Result<()> {
        let now = now()?;
        let slot = &mut ctx.accounts.slot;
        start(slot, kind, name, ctx.bumps.slot);
        settle(
            slot,
            ctx.accounts.next_revoked.as_deref(),
            Some(implementation),
            now,
        )?;
        require_keys_neq!(
            implementation,
            Pubkey::default(),
            RegistryError::NoImplementation
        );
        if implementation == slot.next {
            slot.next = Pubkey::default();
            slot.next_ready_at = 0;
        }
        ctx.accounts.revoked.set_inner(RevokedRecord {
            kind,
            name,
            implementation,
            revoked: true,
            bump: ctx.bumps.revoked,
        });
        emit!(Revoked {
            kind,
            name,
            implementation,
        });
        Ok(())
    }

    // Blessing ------------------------------------------------------------

    /// Blesses `key`, a key the attested instance generated, for one vault
    /// and one bundle on one vault chain, or, for a builder or a reviewer,
    /// for no vault at all.
    ///
    /// `journal` is what the instance attested to; its signer is the
    /// instance's own key, which names the instance and is blessed for
    /// nothing by itself. `seal` is the proof over the journal's Borsh bytes,
    /// which the allowed checkers are asked about, newest first, each with
    /// the accounts it was proposed with followed by any it reads
    /// (`verifier_accounts` gives how many accounts, from the remaining ones,
    /// each checker asked takes). `merkle_proof` shows that this key, for
    /// this vault and this bundle, on this cluster, is among the leaves the
    /// instance attested to; a builder or a reviewer proves the leaf with no
    /// vault and its own key. So one attestation and one proof bless every
    /// key an instance holds, each only for what its own leaf names. Anyone
    /// may send it, and sending it again changes nothing: a blessing lasts a
    /// TTL from the attestation, not from the transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn bless<'info>(
        ctx: Context<'_, '_, 'info, 'info, Bless<'info>>,
        journal: Journal,
        seal: Vec<u8>,
        vault_chain: [u8; 32],
        vault: [u8; 32],
        strategy_hash: [u8; 32],
        key: Pubkey,
        merkle_proof: Vec<[u8; 32]>,
        verifier_accounts: Vec<u8>,
    ) -> Result<()> {
        let now = now()?;
        let config = &ctx.accounts.config;
        let (verifier, trust_root) = verifier_for(
            config,
            now,
            &seal,
            &anchor_lang::prelude::borsh::to_vec(&journal)?,
            ctx.remaining_accounts,
            &verifier_accounts,
        )?;

        let image = read::load::<ImageRecord>(&ctx.accounts.image, ctx.accounts.image.key)?;
        require!(
            is_image_allowed(image.as_ref(), now),
            RegistryError::ImageNotAllowed
        );
        let project = read::load::<ProjectRecord>(&ctx.accounts.project, ctx.accounts.project.key)?;
        require!(
            is_project_allowed(project.as_ref(), now),
            RegistryError::ProjectNotAllowed
        );
        let kind = image.map(|image| image.kind).unwrap_or_default();

        // The instance names itself, and the key has to be a key.
        require_keys_neq!(journal.signer, Pubkey::default(), RegistryError::NoSigner);
        require_keys_neq!(key, Pubkey::default(), RegistryError::NoSigner);
        let serves_one_vault = kind == ImageKind::Executor || kind == ImageKind::Unwinder;
        let zero = [0u8; 32];
        let names_a_vault = vault_chain != zero && vault != zero && strategy_hash != zero;
        let names_nothing = vault_chain == zero && vault == zero && strategy_hash == zero;
        require!(
            if serves_one_vault {
                names_a_vault
            } else {
                names_nothing
            },
            RegistryError::WrongKindForVault
        );
        // The root key's purpose is never a bundle, so a root leaf can never
        // bless a key, whatever the vault.
        require!(
            strategy_hash != digest::root_key_purpose(),
            RegistryError::WrongKindForVault
        );
        // A blessing runs from the attestation, so replaying an old one can
        // never extend it.
        let until = journal
            .issued_at
            .checked_add(config.blessing_ttl)
            .ok_or(RegistryError::Overflow)?;
        require!(
            journal.issued_at <= now && until > now,
            RegistryError::Stale
        );

        let info = &mut ctx.accounts.key_info;
        require!(
            info.kind == ImageKind::None
                || (info.image_digest == journal.image_digest
                    && info.project_id == journal.project_id),
            RegistryError::NotThisInstance
        );
        // The key may act for this vault and this bundle only, on this
        // cluster only, and the instance had to have said so, naming the key,
        // before it saw this transaction. A builder or a reviewer proves the
        // leaf with no vault and its own key. Checked last of all: a
        // simulation that names no proof and is refused only here says every
        // other check would pass.
        let leaf = digest::blessing_leaf(
            &vault_chain,
            &vault,
            &strategy_hash,
            &config.cluster,
            &key.to_bytes(),
        );
        require!(
            digest::merkle_verify(&merkle_proof, &journal.vaults_root, leaf),
            RegistryError::NotAmongTheVaults
        );

        // A blessing only ever moves later, and remembers the checker that
        // gave it, so retiring that checker retires it. Also when the
        // checker it was recorded against has since been retired: an allowed
        // checker vouching for it again revives it. And an attestation no
        // earlier than the one recorded, accepted by a newer checker, moves
        // the blessing to that checker without changing how long it lasts,
        // so a rotation's overlap carries every key relayed through it past
        // the old checker's retirement.
        let blessing = &mut ctx.accounts.blessing;
        if until > blessing.until
            || !config.is_verifier_allowed(&blessing.verifier, now)
            || (until == blessing.until && config.is_newer(&verifier, &blessing.verifier))
        {
            blessing.until = until;
            blessing.verifier = verifier;
        }
        blessing.signer = key;
        blessing.vault_chain = vault_chain;
        blessing.vault = vault;
        blessing.strategy_hash = strategy_hash;
        blessing.bump = ctx.bumps.blessing;

        info.image_digest = journal.image_digest;
        info.project_id = journal.project_id;
        info.kind = kind;
        info.bump = ctx.bumps.key_info;
        // What the key is reported as serving moves only with a later
        // blessing, so replaying a public seal can't change it.
        if until > info.blessed_until || !config.is_verifier_allowed(&info.verifier, now) {
            info.strategy_hash = strategy_hash;
            info.vault_chain = vault_chain;
            info.vault = vault;
            info.blessed_until = until;
            info.trust_root = trust_root;
            info.verifier = verifier;
        } else if until == info.blessed_until && config.is_newer(&verifier, &info.verifier) {
            // The same attestation, vouched for by a newer checker: only the
            // checker it counts through moves.
            info.trust_root = trust_root;
            info.verifier = verifier;
        }
        emit!(Blessed {
            signer: key,
            vault_chain,
            vault,
            strategy_hash,
            image_digest: journal.image_digest,
            blessed_until: blessing.until,
        });
        Ok(())
    }
}

// Accounts -------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializeArgs {
    pub admin: Pubkey,
    /// `localnet`, `devnet`, `mainnet-beta`...: the cluster value is the hash
    /// of `solana:` and this name.
    pub cluster_name: String,
    pub timelock: u64,
    pub min_timelock: u64,
    pub blessing_ttl: u64,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,
    #[account(
        init,
        payer = deployer,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump,
    )]
    pub config: Account<'info, Config>,
    /// CHECK: this program's own account, whose upgrade authority is read.
    #[account(address = crate::ID)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: checked against `program` by `loader::is_upgrade_authority`.
    pub program_data: UncheckedAccount<'info>,
    /// CHECK: the first checker's program, checked final.
    pub verifier_program: UncheckedAccount<'info>,
    /// CHECK: its program data, if it has any; checked against it.
    pub verifier_program_data: Option<UncheckedAccount<'info>>,
    /// CHECK: the first checker's state account, checked to be its program's.
    pub verifier_state: UncheckedAccount<'info>,
    #[account(
        init,
        payer = deployer,
        space = 8 + VerifierRecord::INIT_SPACE,
        seeds = [VERIFIER_SEED, verifier_state.key().as_ref()],
        bump,
    )]
    pub verifier_record: Account<'info, VerifierRecord>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Admin<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
}

#[derive(Accounts)]
#[instruction(image_digest: [u8; 32])]
pub struct ImageChange<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ImageRecord::INIT_SPACE,
        seeds = [IMAGE_SEED, image_digest.as_ref()],
        bump,
    )]
    pub image: Account<'info, ImageRecord>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(project_id: [u8; 32])]
pub struct ProjectChange<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ProjectRecord::INIT_SPACE,
        seeds = [PROJECT_SEED, project_id.as_ref()],
        bump,
    )]
    pub project: Account<'info, ProjectRecord>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ProposeVerifier<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    /// CHECK: the checker's program, checked final.
    pub verifier_program: UncheckedAccount<'info>,
    /// CHECK: its program data, if it has any; checked against it.
    pub verifier_program_data: Option<UncheckedAccount<'info>>,
    /// CHECK: the checker's state account, checked to be its program's.
    pub verifier_state: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + VerifierRecord::INIT_SPACE,
        seeds = [VERIFIER_SEED, verifier_state.key().as_ref()],
        bump,
    )]
    pub verifier_record: Account<'info, VerifierRecord>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(kind: ApprovalKind, name: [u8; 32], implementation: Pubkey)]
pub struct Propose<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ApprovalSlot::INIT_SPACE,
        seeds = [APPROVAL_SEED, kind.seed(), name.as_ref()],
        bump,
    )]
    pub slot: Account<'info, ApprovalSlot>,
    /// CHECK: the revocation record of the proposal this settles, if it
    /// settles one; checked by `settle`.
    #[account(mut)]
    pub next_revoked: Option<UncheckedAccount<'info>>,
    /// CHECK: the implementation's program, checked final.
    #[account(address = implementation)]
    pub implementation_program: UncheckedAccount<'info>,
    /// CHECK: its program data, if it has any; checked against it.
    pub implementation_data: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(kind: ApprovalKind, name: [u8; 32])]
pub struct WithdrawProposal<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ApprovalSlot::INIT_SPACE,
        seeds = [APPROVAL_SEED, kind.seed(), name.as_ref()],
        bump,
    )]
    pub slot: Account<'info, ApprovalSlot>,
    /// CHECK: the revocation record of the proposal this settles, if it
    /// settles one; checked by `settle`.
    #[account(mut)]
    pub next_revoked: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(kind: ApprovalKind, name: [u8; 32], implementation: Pubkey)]
pub struct Revoke<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ RegistryError::NotAdmin,
    )]
    pub config: Account<'info, Config>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ApprovalSlot::INIT_SPACE,
        seeds = [APPROVAL_SEED, kind.seed(), name.as_ref()],
        bump,
    )]
    pub slot: Account<'info, ApprovalSlot>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + RevokedRecord::INIT_SPACE,
        seeds = [REVOKED_SEED, kind.seed(), name.as_ref(), implementation.as_ref()],
        bump,
    )]
    pub revoked: Account<'info, RevokedRecord>,
    /// CHECK: the revocation record of the proposal this settles, if it
    /// settles one other than `implementation`; checked by `settle`.
    #[account(mut)]
    pub next_revoked: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(
    journal: Journal,
    seal: Vec<u8>,
    vault_chain: [u8; 32],
    vault: [u8; 32],
    strategy_hash: [u8; 32],
    key: Pubkey,
)]
pub struct Bless<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// CHECK: read by `read::load`, so an image never proposed reads as not
    /// allowed.
    #[account(seeds = [IMAGE_SEED, journal.image_digest.as_ref()], bump)]
    pub image: UncheckedAccount<'info>,
    /// CHECK: read by `read::load`, so a project never proposed reads as not
    /// allowed.
    #[account(seeds = [PROJECT_SEED, journal.project_id.as_ref()], bump)]
    pub project: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + KeyInfo::INIT_SPACE,
        seeds = [KEY_SEED, key.as_ref()],
        bump,
    )]
    pub key_info: Account<'info, KeyInfo>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Blessing::INIT_SPACE,
        seeds = [
            BLESSING_SEED,
            key.as_ref(),
            vault_chain.as_ref(),
            vault.as_ref(),
            strategy_hash.as_ref(),
        ],
        bump,
    )]
    pub blessing: Account<'info, Blessing>,
    pub system_program: Program<'info, System>,
}

// Helpers --------------------------------------------------------------------

fn ready_at(config: &Config) -> Result<u64> {
    config
        .ready_at(now()?)
        .ok_or_else(|| error!(RegistryError::Overflow))
}

/// A checker about to be trusted: a program nobody can change, and a state
/// account that program owns.
fn check_new_verifier(
    program: &AccountInfo,
    program_data: Option<&AccountInfo>,
    state: &AccountInfo,
) -> Result<(Pubkey, Pubkey)> {
    require_keys_neq!(
        *state.key,
        Pubkey::default(),
        RegistryError::NoImplementation
    );
    require!(
        loader::is_final(program, program_data),
        RegistryError::NotFinal
    );
    require!(
        state.owner == program.key && !state.data_is_empty(),
        RegistryError::NotAVerifier
    );
    Ok((*program.key, *state.key))
}

/// Fills in a slot's name the first time it is touched.
fn start(slot: &mut ApprovalSlot, kind: ApprovalKind, name: [u8; 32], bump: u8) {
    slot.kind = kind;
    slot.name = name;
    slot.bump = bump;
}

/// Makes a proposal that has waited out its timelock the implementation in
/// force. It is a fresh approval, whatever it replaced, so any revocation it
/// carried is cleared, through `next_revoked`, its revocation record (passed
/// whether or not it exists). When the same instruction is about to revoke
/// that very implementation (`revoking`), the clearing would be undone at
/// once, so it is skipped.
fn settle(
    slot: &mut ApprovalSlot,
    next_revoked: Option<&AccountInfo>,
    revoking: Option<Pubkey>,
    now: u64,
) -> Result<()> {
    if !slot.next_is_ready(now) {
        return Ok(());
    }
    if revoking != Some(slot.next) {
        let account = next_revoked.ok_or(RegistryError::MissingRevocationRecord)?;
        let expected = read::revoked_address(slot.kind, &slot.name, &slot.next);
        if let Some(mut record) = read::load::<RevokedRecord>(account, &expected)? {
            if record.revoked {
                require!(account.is_writable, RegistryError::MissingRevocationRecord);
                record.revoked = false;
                let mut data = account.try_borrow_mut_data()?;
                record.try_serialize(&mut &mut data[..])?;
            }
        }
    }
    slot.current = slot.next;
    slot.next = Pubkey::default();
    slot.next_ready_at = 0;
    Ok(())
}

/// The newest allowed checker that accepts the proof, and the trust root it
/// checked against. Newest first, so that during a rotation keys are recorded
/// against the checker that will outlast the overlap. A checker that answers
/// no, or answers in a shape nobody asked for, is just a checker that didn't
/// accept it.
fn verifier_for<'info>(
    config: &Config,
    now: u64,
    seal: &[u8],
    journal: &[u8],
    remaining: &[AccountInfo<'info>],
    group_sizes: &[u8],
) -> Result<(Pubkey, [u8; 32])> {
    let mut cursor = 0usize;
    let mut groups = group_sizes.iter();
    for entry in config.verifiers.iter().rev() {
        if now < entry.ready_at {
            continue;
        }
        let Some(&size) = groups.next() else {
            break;
        };
        let size = size as usize;
        let accounts = cursor
            .checked_add(size)
            .and_then(|end| remaining.get(cursor..end))
            .filter(|accounts| accounts.len() >= 2)
            .ok_or(RegistryError::WrongVerifierAccounts)?;
        cursor += size;
        require!(
            accounts[0].key == &entry.program && accounts[1].key == &entry.state,
            RegistryError::WrongVerifierAccounts
        );
        if let Some(trust_root) = ask(accounts, seal, journal)? {
            return Ok((entry.state, trust_root));
        }
    }
    err!(RegistryError::ProofRejected)
}

/// Asks one checker. Every account it is given is read-only and none signs,
/// so a checker can only read and answer.
fn ask<'info>(
    accounts: &[AccountInfo<'info>],
    seal: &[u8],
    journal: &[u8],
) -> Result<Option<[u8; 32]>> {
    let program = accounts[0].key;
    let instruction = Instruction {
        program_id: *program,
        accounts: accounts[1..]
            .iter()
            .map(|account| AccountMeta::new_readonly(*account.key, false))
            .collect(),
        data: checker::verify_data(seal, journal),
    };
    // Whatever an earlier call left behind is not this checker's answer.
    set_return_data(&[]);
    invoke(&instruction, accounts)?;
    Ok(match get_return_data() {
        Some((from, answer)) if &from == program => checker::accepted(&answer),
        _ => None,
    })
}
