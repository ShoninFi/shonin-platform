//! What an author says about themselves.
//!
//! Nothing here is checked and nothing depends on it. An author who sets no
//! profile is shown by their address, and their record reads exactly the
//! same, because the record is what speaks for a strategy. A handle is one
//! author's at a time: the one they leave is free for someone else.
//!
//! There is no admin. Each author pays for their own profile's room and gets
//! back what a shorter profile no longer needs.

use anchor_lang::prelude::*;
use shonin_common::{account, digest, time::now};

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// One author's profile.
pub const PROFILE_SEED: &[u8] = b"profile";
/// Who holds one handle, by its keccak256.
pub const HANDLE_SEED: &[u8] = b"handle";

#[account]
pub struct Profile {
    pub author: Pubkey,
    pub handle: String,
    pub blurb: String,
    pub link_labels: Vec<String>,
    pub link_urls: Vec<String>,
    pub joined_at: u64,
    pub bump: u8,
}

impl Profile {
    /// The account size for a profile holding these.
    pub fn space(handle: &str, blurb: &str, labels: &[String], urls: &[String]) -> usize {
        let strings = |list: &[String]| 4 + list.iter().map(|s| 4 + s.len()).sum::<usize>();
        8 + 32 + 4 + handle.len() + 4 + blurb.len() + strings(labels) + strings(urls) + 8 + 1
    }
}

/// Who holds a handle; the default key once nobody does.
#[account]
#[derive(InitSpace)]
pub struct HandleRecord {
    pub owner: Pubkey,
    pub bump: u8,
}

/// The address recording who holds `handle`.
pub fn handle_address(handle: &str) -> Pubkey {
    Pubkey::find_program_address(
        &[HANDLE_SEED, &digest::keccak(&[handle.as_bytes()])],
        &crate::ID,
    )
    .0
}

#[program]
pub mod shonin_profiles {
    use super::*;

    /// Sets the sender's profile. `handle_hash` is the handle's keccak256,
    /// which names its record.
    pub fn set_profile(
        ctx: Context<SetProfile>,
        handle: String,
        handle_hash: [u8; 32],
        blurb: String,
        link_labels: Vec<String>,
        link_urls: Vec<String>,
    ) -> Result<()> {
        require!(!handle.is_empty(), ProfilesError::EmptyHandle);
        require!(
            handle_hash == digest::keccak(&[handle.as_bytes()]),
            ProfilesError::WrongHandleHash
        );
        require!(
            link_labels.len() == link_urls.len(),
            ProfilesError::MismatchedLinks
        );
        let author = ctx.accounts.author.key();
        let owner = ctx.accounts.new_handle.owner;
        require!(
            owner == Pubkey::default() || owner == author,
            ProfilesError::HandleTaken
        );

        let account = ctx.accounts.profile.to_account_info();
        let existing = if account.owner == &crate::ID && !account.data_is_empty() {
            let data = account.try_borrow_data()?;
            Some(Profile::try_deserialize(&mut &data[..])?)
        } else {
            None
        };
        let joined_at = match &existing {
            Some(profile) => profile.joined_at,
            None => now()?,
        };
        // The handle left behind is free for anyone.
        if let Some(previous) = existing.as_ref().filter(|p| !p.handle.is_empty()) {
            if digest::keccak(&[previous.handle.as_bytes()]) != handle_hash {
                release(ctx.accounts.old_handle.as_deref(), &previous.handle)?;
            }
        }

        let profile = Profile {
            author,
            handle: handle.clone(),
            blurb,
            link_labels,
            link_urls,
            joined_at,
            bump: ctx.bumps.profile,
        };
        let needed = Profile::space(
            &profile.handle,
            &profile.blurb,
            &profile.link_labels,
            &profile.link_urls,
        );
        let author_account = ctx.accounts.author.to_account_info();
        let system = ctx.accounts.system_program.to_account_info();
        if existing.is_some() {
            account::fit(&account, &author_account, &system, needed)?;
        } else {
            account::create_pda(
                &author_account,
                &account,
                &system,
                &[PROFILE_SEED, author.as_ref(), &[ctx.bumps.profile]],
                needed,
                &crate::ID,
            )?;
        }
        {
            let mut data = account.try_borrow_mut_data()?;
            profile.try_serialize(&mut &mut data[..])?;
        }

        let record = &mut ctx.accounts.new_handle;
        record.owner = author;
        record.bump = ctx.bumps.new_handle;
        emit!(ProfileSet { author, handle });
        Ok(())
    }
}

/// Frees the handle an author is leaving, through its own record.
fn release(record: Option<&AccountInfo>, handle: &str) -> Result<()> {
    let record = record.ok_or(ProfilesError::OldHandleMissing)?;
    require_keys_eq!(
        *record.key,
        handle_address(handle),
        ProfilesError::OldHandleMissing
    );
    if record.owner == &crate::ID && !record.data_is_empty() {
        let mut held = {
            let data = record.try_borrow_data()?;
            HandleRecord::try_deserialize(&mut &data[..])?
        };
        held.owner = Pubkey::default();
        let mut data = record.try_borrow_mut_data()?;
        held.try_serialize(&mut &mut data[..])?;
    }
    Ok(())
}

#[derive(Accounts)]
#[instruction(handle: String, handle_hash: [u8; 32])]
pub struct SetProfile<'info> {
    #[account(mut)]
    pub author: Signer<'info>,
    /// CHECK: the author's profile, created or resized by hand since its size
    /// follows what it says; read and written by `set_profile`.
    #[account(mut, seeds = [PROFILE_SEED, author.key().as_ref()], bump)]
    pub profile: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = author,
        space = 8 + HandleRecord::INIT_SPACE,
        seeds = [HANDLE_SEED, handle_hash.as_ref()],
        bump,
    )]
    pub new_handle: Account<'info, HandleRecord>,
    /// CHECK: the record of the handle the author is leaving, if any;
    /// checked against it.
    #[account(mut)]
    pub old_handle: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

#[event]
pub struct ProfileSet {
    pub author: Pubkey,
    pub handle: String,
}

#[error_code]
pub enum ProfilesError {
    #[msg("A handle can't be empty")]
    EmptyHandle,
    #[msg("Every link has a label and a URL")]
    MismatchedLinks,
    #[msg("The handle is someone else's")]
    HandleTaken,
    #[msg("The handle's hash is not its keccak256")]
    WrongHandleHash,
    #[msg("The record of the handle being left is missing")]
    OldHandleMissing,
}
