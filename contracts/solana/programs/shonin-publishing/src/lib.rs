//! Publishing: the paid submission that gives a strategy its id, every
//! strategy there is, and every strategy's one review.
//!
//! The fee covers the review and the build, and is spent whatever the
//! outcome: a refusal, a failed build and a job that fails on our side all
//! cost it, so nothing an author can trigger earns a free retry. Ids are
//! assigned here, by the first on-chain act, so they are unique without
//! anyone choosing them.
//!
//! Anyone may send a registration; only a blessed builder's signature over
//! what it built, and a blessed reviewer's go-ahead for the source it read,
//! make it stick. So the only bundles here are ones an allowlisted builder
//! compiled from source an allowlisted reviewer read first and did not
//! refuse, and a vault asks this program what its strategy's bundle is and
//! gets an answer nobody could have forged.
//!
//! A review is a model's reading of code, signed by the image that ran it. It
//! is never a gate for anything but malicious code, which the reviewer refuses
//! before it is built. Everything else is shown, however it reads, and the
//! depositor decides. Nobody can buy a fresh review, so nobody can probe a
//! sealed strategy's code by asking again.
//!
//! There is no admin: nothing here can be changed once set up.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use shonin_common::{account, ed25519, loader, time::now};
use shonin_registry::state::ImageKind;

pub mod error;
pub mod events;
pub mod state;

use error::PublishingError;
use events::*;
use state::*;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

#[program]
pub mod shonin_publishing {
    use super::*;

    /// Sets a fresh deployment up, once, signed by the key that deployed it:
    /// the dollar the fee is paid in, who it is paid to, and how much.
    pub fn initialize(ctx: Context<Initialize>, platform: Pubkey, fee: u64) -> Result<()> {
        require!(
            loader::is_upgrade_authority(
                &ctx.accounts.program,
                Some(&ctx.accounts.program_data),
                ctx.accounts.deployer.key,
            ),
            PublishingError::NotTheDeployer
        );
        ctx.accounts.config.set_inner(Config {
            mint: ctx.accounts.mint.key(),
            token_program: ctx.accounts.token_program.key(),
            decimals: ctx.accounts.mint.decimals,
            platform,
            fee,
            count: 0,
            bump: ctx.bumps.config,
        });
        Ok(())
    }

    // Submitting ----------------------------------------------------------

    /// Pays the fee, in the dollar, from the author's own account, and takes
    /// the next id.
    pub fn submit(ctx: Context<Submit>, upload: [u8; 32]) -> Result<()> {
        let config = &ctx.accounts.config;
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.author_token.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.platform_token.to_account_info(),
                    authority: ctx.accounts.author.to_account_info(),
                },
            ),
            config.fee,
            config.decimals,
        )?;
        let strategy_id = config.count + 1;
        let fee = config.fee;
        ctx.accounts.config.count = strategy_id;
        ctx.accounts.submission.set_inner(Submission {
            strategy_id,
            author: ctx.accounts.author.key(),
            upload,
            paid_at: now()?,
            bump: ctx.bumps.submission,
        });
        emit!(Submitted {
            strategy_id,
            author: ctx.accounts.author.key(),
            upload,
            fee,
        });
        Ok(())
    }

    // Registering ---------------------------------------------------------

    /// Starts assembling a registration, in a draft of the sender's own.
    #[allow(clippy::too_many_arguments)]
    pub fn stage(
        ctx: Context<Stage>,
        strategy_id: u64,
        author: Pubkey,
        source_hash: [u8; 32],
        bundle_hash: [u8; 32],
        manifest_hash: [u8; 32],
        lists: Lists,
    ) -> Result<()> {
        require!(lists.within_bounds(), PublishingError::TooMany);
        ctx.accounts.draft.set_inner(Draft {
            strategy_id,
            stager: ctx.accounts.stager.key(),
            author,
            source_hash,
            bundle_hash,
            manifest_hash,
            lists,
            bump: ctx.bumps.draft,
        });
        Ok(())
    }

    /// Adds to a draft's lists, for a registration longer than one
    /// transaction holds. The stager pays for the room.
    #[allow(unused_variables)]
    pub fn extend(ctx: Context<Extend>, strategy_id: u64, more: Lists) -> Result<()> {
        let draft = &mut ctx.accounts.draft;
        draft.lists.chains.extend(more.chains);
        draft.lists.bridges.extend(more.bridges);
        draft.lists.restricted.extend(more.restricted);
        require!(draft.lists.within_bounds(), PublishingError::TooMany);
        let needed = Draft::space(&draft.lists);
        if needed > draft.to_account_info().data_len() {
            account::fit(
                &draft.to_account_info(),
                &ctx.accounts.stager.to_account_info(),
                &ctx.accounts.system_program.to_account_info(),
                needed,
            )?;
        }
        Ok(())
    }

    /// Gives up a draft, refunding its rent to whoever assembled it.
    #[allow(unused_variables)]
    pub fn discard(ctx: Context<Discard>, strategy_id: u64) -> Result<()> {
        Ok(())
    }

    /// Registers what a draft says, if a blessed builder signed exactly that
    /// and a blessed reviewer cleared its source, both in this transaction.
    /// The id belongs to whoever paid for it, and to nobody else. The draft
    /// is closed to whoever assembled it.
    pub fn register(
        ctx: Context<Register>,
        strategy_id: u64,
        builder: Pubkey,
        reviewer: Pubkey,
    ) -> Result<()> {
        let now = now()?;
        let accounts = &ctx.accounts;
        require!(
            load_strategy(&accounts.strategy)?.is_none(),
            PublishingError::AlreadyRegistered
        );
        let draft = &accounts.draft;
        // Zero means "nothing" everywhere else: not registered, or refused.
        require!(
            draft.source_hash != [0; 32] && draft.bundle_hash != [0; 32],
            PublishingError::NothingBuilt
        );
        let submission = load_submission(&accounts.submission)?;
        require!(
            draft.author != Pubkey::default()
                && submission.is_some_and(|submission| submission.author == draft.author),
            PublishingError::NotSubmittedByTheAuthor
        );
        require!(draft.lists.is_sorted(), PublishingError::NotSorted);

        let cluster = shonin_registry::read::config(&accounts.registry_config)?.cluster;
        let digest = registration_digest(
            &cluster,
            strategy_id,
            &draft.author,
            &draft.source_hash,
            &draft.bundle_hash,
            &draft.manifest_hash,
            &draft.lists,
        );
        require!(
            ed25519::signed_in_transaction(&accounts.instructions, &builder, &digest, None)?
                && shonin_registry::read::blessed_as(
                    &accounts.registry_config,
                    &accounts.builder_key,
                    &accounts.builder_image,
                    &accounts.builder_project,
                    &builder,
                    ImageKind::Builder,
                    now,
                )?,
            PublishingError::NotTheBuilder
        );
        // The review comes before the build: the reviewer has to have read
        // this source and let it through before anything compiled it.
        let clearance = clearance_digest(&cluster, strategy_id, &draft.source_hash);
        require!(
            ed25519::signed_in_transaction(&accounts.instructions, &reviewer, &clearance, None)?
                && shonin_registry::read::blessed_as(
                    &accounts.registry_config,
                    &accounts.reviewer_key,
                    &accounts.reviewer_image,
                    &accounts.reviewer_project,
                    &reviewer,
                    ImageKind::Reviewer,
                    now,
                )?,
            PublishingError::NotCleared
        );

        let draft = &ctx.accounts.draft;
        let entry = Strategy {
            strategy_id,
            author: draft.author,
            source_hash: draft.source_hash,
            bundle_hash: draft.bundle_hash,
            manifest_hash: draft.manifest_hash,
            registered_at: now,
            lists: draft.lists.clone(),
            bump: ctx.bumps.strategy,
        };
        // Its size follows its lists, so it is made by hand; the sender pays.
        let record = ctx.accounts.strategy.to_account_info();
        account::create_pda(
            &ctx.accounts.payer.to_account_info(),
            &record,
            &ctx.accounts.system_program.to_account_info(),
            &[
                STRATEGY_SEED,
                &strategy_id.to_le_bytes(),
                &[ctx.bumps.strategy],
            ],
            Strategy::space(&entry.lists),
            &crate::ID,
        )?;
        {
            let mut data = record.try_borrow_mut_data()?;
            entry.try_serialize(&mut &mut data[..])?;
        }
        emit!(Registered {
            strategy_id,
            author: entry.author,
            bundle_hash: entry.bundle_hash,
            source_hash: entry.source_hash,
            manifest_hash: entry.manifest_hash,
        });
        Ok(())
    }

    // Reviews -------------------------------------------------------------

    /// A blessed reviewer key posts the review of a registered strategy. The
    /// hashes must be the ones the strategy's record holds: a review is of
    /// one bundle built from one source, and saying otherwise would let a
    /// good report follow code nobody read.
    pub fn submit_review(
        ctx: Context<SubmitReview>,
        strategy_id: u64,
        review: ReviewInput,
    ) -> Result<()> {
        let now = now()?;
        check_review(
            &ctx.accounts.review,
            &review,
            &ctx.accounts.reviewer_accounts,
            now,
        )?;
        let strategy = load_strategy(&ctx.accounts.strategy)?
            .ok_or_else(|| error!(PublishingError::NotRegistered))?;
        require!(
            strategy.bundle_hash == review.bundle_hash
                && strategy.source_hash == review.source_hash,
            PublishingError::WrongBundle
        );
        let image = reviewer_image(&ctx.accounts.reviewer_accounts)?;
        record(
            &mut ctx.accounts.review,
            strategy_id,
            &review,
            image,
            now,
            false,
            ctx.bumps.review,
        );
        emit!(ReviewSubmitted {
            strategy_id,
            reviewer: ctx.accounts.reviewer_accounts.reviewer.key(),
            ratings: review.ratings,
            report_hash: review.report_hash,
        });
        Ok(())
    }

    /// A blessed reviewer key records that it refused a submitted strategy's
    /// source. A refusal is recorded against the source it read, and a
    /// refused strategy never has a review a vault can follow.
    pub fn submit_refusal(
        ctx: Context<SubmitRefusal>,
        strategy_id: u64,
        review: ReviewInput,
    ) -> Result<()> {
        let now = now()?;
        check_review(
            &ctx.accounts.review,
            &review,
            &ctx.accounts.reviewer_accounts,
            now,
        )?;
        require!(
            load_submission(&ctx.accounts.submission)?.is_some(),
            PublishingError::NotSubmitted
        );
        require!(
            load_strategy(&ctx.accounts.strategy)?.is_none(),
            PublishingError::AlreadyRegistered
        );
        require!(review.bundle_hash == [0; 32], PublishingError::WrongBundle);
        require!(review.source_hash != [0; 32], PublishingError::NoSource);
        let image = reviewer_image(&ctx.accounts.reviewer_accounts)?;
        record(
            &mut ctx.accounts.review,
            strategy_id,
            &review,
            image,
            now,
            true,
            ctx.bumps.review,
        );
        emit!(Refused {
            strategy_id,
            reviewer: ctx.accounts.reviewer_accounts.reviewer.key(),
            source_hash: review.source_hash,
            report_hash: review.report_hash,
        });
        Ok(())
    }
}

// Accounts -------------------------------------------------------------------

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
    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Submit<'info> {
    #[account(mut)]
    pub author: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = author,
        space = 8 + Submission::INIT_SPACE,
        seeds = [SUBMISSION_SEED, &(config.count + 1).to_le_bytes()],
        bump,
    )]
    pub submission: Account<'info, Submission>,
    #[account(address = config.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    /// The fee comes from the author's own account, as from the sender's
    /// balance on the EVM.
    #[account(
        mut,
        token::mint = mint,
        token::authority = author,
        token::token_program = token_program,
    )]
    pub author_token: InterfaceAccount<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = config.platform,
        associated_token::token_program = token_program,
    )]
    pub platform_token: InterfaceAccount<'info, TokenAccount>,
    #[account(address = config.token_program)]
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64, author: Pubkey, source_hash: [u8; 32], bundle_hash: [u8; 32], manifest_hash: [u8; 32], lists: Lists)]
pub struct Stage<'info> {
    #[account(mut)]
    pub stager: Signer<'info>,
    #[account(
        init,
        payer = stager,
        space = Draft::space(&lists),
        seeds = [DRAFT_SEED, &strategy_id.to_le_bytes(), stager.key().as_ref()],
        bump,
    )]
    pub draft: Account<'info, Draft>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64)]
pub struct Extend<'info> {
    #[account(mut)]
    pub stager: Signer<'info>,
    #[account(
        mut,
        seeds = [DRAFT_SEED, &strategy_id.to_le_bytes(), stager.key().as_ref()],
        bump = draft.bump,
    )]
    pub draft: Account<'info, Draft>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64)]
pub struct Discard<'info> {
    #[account(mut)]
    pub stager: Signer<'info>,
    #[account(
        mut,
        seeds = [DRAFT_SEED, &strategy_id.to_le_bytes(), stager.key().as_ref()],
        bump = draft.bump,
        close = stager,
    )]
    pub draft: Account<'info, Draft>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64)]
pub struct Register<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the draft's stager, who gets its rent back.
    #[account(mut, address = draft.stager)]
    pub stager: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [DRAFT_SEED, &strategy_id.to_le_bytes(), stager.key().as_ref()],
        bump = draft.bump,
        close = stager,
    )]
    pub draft: Account<'info, Draft>,
    /// CHECK: read by `load_submission`; may never have been paid for.
    #[account(seeds = [SUBMISSION_SEED, &strategy_id.to_le_bytes()], bump)]
    pub submission: UncheckedAccount<'info>,
    /// CHECK: the strategy's record, which must not exist yet; made by
    /// `register`, since its size follows its lists.
    #[account(mut, seeds = [STRATEGY_SEED, &strategy_id.to_le_bytes()], bump)]
    pub strategy: UncheckedAccount<'info>,
    /// CHECK: the registry's settings, checked by `shonin_registry::read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the builder key's record, checked by `shonin_registry::read`.
    pub builder_key: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `shonin_registry::read`.
    pub builder_image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `shonin_registry::read`.
    pub builder_project: UncheckedAccount<'info>,
    /// CHECK: the reviewer key's record, checked by `shonin_registry::read`.
    pub reviewer_key: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `shonin_registry::read`.
    pub reviewer_image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `shonin_registry::read`.
    pub reviewer_project: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar, by address.
    #[account(address = instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// The reviewer key sending a review, and the registry records that say
/// whether it is blessed as one.
#[derive(Accounts)]
pub struct ReviewerAccounts<'info> {
    pub reviewer: Signer<'info>,
    /// CHECK: the registry's settings, checked by `shonin_registry::read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the reviewer key's record, checked by `shonin_registry::read`.
    pub reviewer_key: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `shonin_registry::read`.
    pub reviewer_image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `shonin_registry::read`.
    pub reviewer_project: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64)]
pub struct SubmitReview<'info> {
    pub reviewer_accounts: ReviewerAccounts<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Review::INIT_SPACE,
        seeds = [REVIEW_SEED, &strategy_id.to_le_bytes()],
        bump,
    )]
    pub review: Account<'info, Review>,
    /// CHECK: read by `load_strategy`; may not be registered.
    #[account(seeds = [STRATEGY_SEED, &strategy_id.to_le_bytes()], bump)]
    pub strategy: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(strategy_id: u64)]
pub struct SubmitRefusal<'info> {
    pub reviewer_accounts: ReviewerAccounts<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Review::INIT_SPACE,
        seeds = [REVIEW_SEED, &strategy_id.to_le_bytes()],
        bump,
    )]
    pub review: Account<'info, Review>,
    /// CHECK: read by `load_submission`; may never have been paid for.
    #[account(seeds = [SUBMISSION_SEED, &strategy_id.to_le_bytes()], bump)]
    pub submission: UncheckedAccount<'info>,
    /// CHECK: read by `load_strategy`; must not be registered.
    #[account(seeds = [STRATEGY_SEED, &strategy_id.to_le_bytes()], bump)]
    pub strategy: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

// Helpers --------------------------------------------------------------------

/// The record at a program-derived address of this program that Anchor has
/// already checked; `None` if nothing was ever recorded there.
fn load<T: AccountDeserialize>(account: &AccountInfo) -> Result<Option<T>> {
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(None);
    }
    let data = account.try_borrow_data()?;
    Ok(Some(T::try_deserialize(&mut &data[..])?))
}

fn load_submission(account: &AccountInfo) -> Result<Option<Submission>> {
    load::<Submission>(account)
}

fn load_strategy(account: &AccountInfo) -> Result<Option<Strategy>> {
    load::<Strategy>(account)
}

/// What every review and refusal must be: from a blessed reviewer key, the
/// first for its strategy, with every area rated.
fn check_review(
    stored: &Review,
    review: &ReviewInput,
    reviewer: &ReviewerAccounts,
    now: u64,
) -> Result<()> {
    require!(
        shonin_registry::read::blessed_as(
            &reviewer.registry_config,
            &reviewer.reviewer_key,
            &reviewer.reviewer_image,
            &reviewer.reviewer_project,
            reviewer.reviewer.key,
            ImageKind::Reviewer,
            now,
        )?,
        PublishingError::NotTheReviewer
    );
    // One review per strategy, ever.
    require!(stored.reviewed_at == 0, PublishingError::AlreadyReviewed);
    require!(
        review
            .ratings
            .iter()
            .all(|rating| (LOWEST_RATING..=HIGHEST_RATING).contains(rating)),
        PublishingError::BadRating
    );
    Ok(())
}

/// The image the registry knows the sending key by.
fn reviewer_image(reviewer: &ReviewerAccounts) -> Result<[u8; 32]> {
    let key = shonin_registry::read::key_info(&reviewer.reviewer_key, reviewer.reviewer.key)?
        .ok_or_else(|| error!(PublishingError::NotTheReviewer))?;
    Ok(key.image_digest)
}

fn record(
    stored: &mut Review,
    strategy_id: u64,
    review: &ReviewInput,
    reviewer_image: [u8; 32],
    now: u64,
    refused: bool,
    bump: u8,
) {
    *stored = Review {
        strategy_id,
        bundle_hash: review.bundle_hash,
        source_hash: review.source_hash,
        reviewer_image,
        model_id_hash: review.model_id_hash,
        prompt_hash: review.prompt_hash,
        report_hash: review.report_hash,
        ratings: review.ratings,
        finding_count: review.finding_count,
        reviewed_at: now,
        refused,
        bump,
    };
}
