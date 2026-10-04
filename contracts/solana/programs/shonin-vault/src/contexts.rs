//! The accounts each instruction takes.
//!
//! A vault, a holder, the station and the dollar accounts are checked against
//! the vault's own record of them; claims, which are addressed by id, are
//! checked by address where they are read, and requests, which are addressed
//! by their maker, by the vault and the id they hold.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token};

use crate::state::*;

// The factory ---------------------------------------------------------------------

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,
    #[account(
        init,
        payer = deployer,
        space = 8 + Factory::INIT_SPACE,
        seeds = [FACTORY_SEED],
        bump,
    )]
    pub factory: Box<Account<'info, Factory>>,
    pub mint: Account<'info, Mint>,
    /// CHECK: read by `read::config`, which checks it is the registry's.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: this program's own account, whose upgrade authority is read.
    #[account(address = crate::ID)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: checked against `program` by `loader::is_upgrade_authority`.
    pub program_data: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetTerms<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [FACTORY_SEED], bump = factory.bump, has_one = admin)]
    pub factory: Box<Account<'info, Factory>>,
}

#[derive(Accounts)]
#[instruction(args: crate::factory::ActivateArgs)]
pub struct Activate<'info> {
    /// The strategy's author, who pays for everything created here.
    #[account(mut)]
    pub author: Signer<'info>,
    /// Counts the vaults it has made.
    #[account(mut, seeds = [FACTORY_SEED], bump = factory.bump)]
    pub factory: Box<Account<'info, Factory>>,
    /// CHECK: the strategy's record in publishing, checked by address, owner
    /// and kind.
    pub strategy: UncheckedAccount<'info>,
    /// CHECK: its review, likewise.
    pub review: UncheckedAccount<'info>,
    /// CHECK: created here, at the address its seeds derive.
    #[account(mut, seeds = [VAULT_SEED, &args.strategy_id.to_le_bytes()], bump)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: created here, at the address its seeds derive.
    #[account(mut, seeds = [RESTRICTED_SEED, vault.key().as_ref()], bump)]
    pub restricted: UncheckedAccount<'info>,
    /// CHECK: created here, at the address its seeds derive.
    #[account(mut, seeds = [HOLDER_SEED, vault.key().as_ref(), author.key().as_ref()], bump)]
    pub author_holder: UncheckedAccount<'info>,
    /// CHECK: created here, at the address its seeds derive.
    #[account(mut, seeds = [HOLDER_SEED, vault.key().as_ref(), factory.platform.as_ref()], bump)]
    pub platform_holder: UncheckedAccount<'info>,
    /// CHECK: the vault's associated dollar account, created here.
    #[account(mut)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the author's dollars the tank is funded from; the token program
    /// checks the author signs for it.
    #[account(mut)]
    pub author_dollars: UncheckedAccount<'info>,
    #[account(address = factory.mint)]
    pub mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: the station program, by address.
    #[account(address = shonin_station::ID)]
    pub station_program: UncheckedAccount<'info>,
    /// CHECK: the station program's settings, checked by the station.
    pub station_config: UncheckedAccount<'info>,
    /// CHECK: the vault's station, created by the station program.
    #[account(mut)]
    pub station: UncheckedAccount<'info>,
    /// CHECK: the station's dollar account, created by the station program.
    #[account(mut)]
    pub station_dollars: UncheckedAccount<'info>,
}

// The station's question ----------------------------------------------------------

#[derive(Accounts)]
pub struct StationView<'info> {
    #[account(seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
}

// Who may deposit -------------------------------------------------------------------

#[derive(Accounts)]
pub struct RecordStatement<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + Holder::INIT_SPACE,
        seeds = [HOLDER_SEED, vault.key().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub holder: Box<Account<'info, Holder>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RefreshRestricted<'info> {
    /// Pays for any room the longer list needs.
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the vault's list, checked by address and owner.
    #[account(mut, seeds = [RESTRICTED_SEED, vault.key().as_ref()], bump)]
    pub restricted: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

// Requests -----------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(assets: u64, receiver: Pubkey)]
pub struct RequestDeposit<'info> {
    /// Pays the dollars and the request's rent, and is sent the deposit back
    /// if it goes back.
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Holder::INIT_SPACE,
        seeds = [HOLDER_SEED, vault.key().as_ref(), receiver.as_ref()],
        bump,
    )]
    pub holder: Box<Account<'info, Holder>>,
    /// CHECK: created here, at the payer's request address for the nonce.
    #[account(mut)]
    pub request: UncheckedAccount<'info>,
    /// CHECK: the payer's dollars; the token program checks the payer signs
    /// for them.
    #[account(mut)]
    pub payer_dollars: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RequestRedeem<'info> {
    /// The holder, who pays the request's rent.
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        mut,
        seeds = [HOLDER_SEED, vault.key().as_ref(), owner.key().as_ref()],
        bump = holder.bump,
        constraint = holder.vault == vault.key() && holder.owner == owner.key() @ crate::error::VaultError::WrongAccount,
    )]
    pub holder: Box<Account<'info, Holder>>,
    /// CHECK: created here, at the owner's request address for the nonce.
    #[account(mut)]
    pub request: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// The accounts that price and pay: the vault, its station (read for what it
/// sent home), the fee recipients' holdings, and the vault's dollars.
#[derive(Accounts)]
pub struct Pool<'info> {
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the vault's station, by address; read for what it sent home,
    /// and written by the station program when the vault asks it to close.
    #[account(mut, address = vault.station)]
    pub station: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [HOLDER_SEED, vault.key().as_ref(), vault.author.as_ref()],
        bump = author_holder.bump,
        constraint = author_holder.vault == vault.key() && author_holder.owner == vault.author @ crate::error::VaultError::WrongAccount,
    )]
    pub author_holder: Box<Account<'info, Holder>>,
    #[account(
        mut,
        seeds = [HOLDER_SEED, vault.key().as_ref(), vault.platform.as_ref()],
        bump = platform_holder.bump,
        constraint = platform_holder.vault == vault.key() && platform_holder.owner == vault.platform @ crate::error::VaultError::WrongAccount,
    )]
    pub platform_holder: Box<Account<'info, Holder>>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct RecordValuation<'info> {
    pub pool: Pool<'info>,
    /// CHECK: the station program, by address.
    #[account(address = shonin_station::ID)]
    pub station_program: UncheckedAccount<'info>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct Settle<'info> {
    pub pool: Pool<'info>,
}

// Claims ------------------------------------------------------------------------------

/// The station's accounts for a close the vault asks for.
#[derive(Accounts)]
pub struct Closing<'info> {
    /// CHECK: the station program, by address.
    #[account(address = shonin_station::ID)]
    pub station_program: UncheckedAccount<'info>,
    /// CHECK: the station's dollar account; the station checks it.
    #[account(mut)]
    pub station_dollars: UncheckedAccount<'info>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct SettleClaim<'info> {
    /// Pays the rent of a request, if the claim is queued.
    #[account(mut)]
    pub caller: Signer<'info>,
    pub pool: Pool<'info>,
    pub close: Closing<'info>,
    /// CHECK: the claim, checked by address.
    #[account(mut)]
    pub claim: UncheckedAccount<'info>,
    /// CHECK: the request a queued claim is priced by, created here when it
    /// is queued, at the claim's request address for the last it was queued
    /// by; checked by address.
    #[account(mut)]
    pub request: Option<UncheckedAccount<'info>>,
    /// CHECK: the claim's owner's associated dollar account, checked by
    /// address where it is paid.
    #[account(mut)]
    pub owner_dollars: UncheckedAccount<'info>,
    /// CHECK: the author's associated dollar account, likewise.
    #[account(mut)]
    pub author_dollars: UncheckedAccount<'info>,
    /// CHECK: the platform's associated dollar account, likewise.
    #[account(mut)]
    pub platform_dollars: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ClaimInstead<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the request, checked by the vault and id it holds.
    #[account(mut)]
    pub request: UncheckedAccount<'info>,
    /// CHECK: created here, at the claim's address (the request's id).
    #[account(mut)]
    pub claim: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Freeze<'info> {
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the claim, checked by address.
    pub claim: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct FreezeUnits<'info> {
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        seeds = [HOLDER_SEED, vault.key().as_ref(), holder.owner.as_ref()],
        bump = holder.bump,
        constraint = holder.vault == vault.key() @ crate::error::VaultError::WrongAccount,
    )]
    pub holder: Box<Account<'info, Holder>>,
}

// The pro-rata fallback --------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct TakeProRata<'info> {
    /// The request's owner, who pays for a holding of units if they have none.
    #[account(mut)]
    pub owner: Signer<'info>,
    pub pool: Pool<'info>,
    /// CHECK: the station program, by address: a deposit's fallback asks the
    /// station whether it could be valued now.
    #[account(address = shonin_station::ID)]
    pub station_program: UncheckedAccount<'info>,
    /// CHECK: the request, checked by the vault and id it holds.
    #[account(mut)]
    pub request: UncheckedAccount<'info>,
    /// CHECK: the owner's holding, created here if they have none; checked
    /// by address.
    #[account(mut)]
    pub holder: UncheckedAccount<'info>,
    /// CHECK: where the payout goes: the owner's associated dollar account
    /// (a deposit's, its payer's), checked by address where it is paid.
    #[account(mut)]
    pub to_dollars: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TakeProRataClaim<'info> {
    /// The claim's owner, who pays for a holding of units if they have none.
    #[account(mut)]
    pub owner: Signer<'info>,
    pub pool: Pool<'info>,
    /// CHECK: the claim, checked by address.
    #[account(mut)]
    pub claim: UncheckedAccount<'info>,
    /// CHECK: a queued claim's request, checked by the vault and id it
    /// holds.
    #[account(mut)]
    pub request: Option<UncheckedAccount<'info>>,
    /// CHECK: the owner's holding, created here if they have none; checked
    /// by address.
    #[account(mut)]
    pub holder: UncheckedAccount<'info>,
    /// CHECK: the owner's associated dollar account, checked where paid.
    #[account(mut)]
    pub to_dollars: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct ServeProRata<'info> {
    pub caller: Signer<'info>,
    pub pool: Pool<'info>,
    pub close: Closing<'info>,
    /// CHECK: the unit holder's holding, checked by address against the
    /// owner named.
    #[account(mut)]
    pub holder: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CollectUnits<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        mut,
        seeds = [HOLDER_SEED, vault.key().as_ref(), owner.key().as_ref()],
        bump = holder.bump,
        constraint = holder.vault == vault.key() && holder.owner == owner.key() @ crate::error::VaultError::WrongAccount,
    )]
    pub holder: Box<Account<'info, Holder>>,
    /// CHECK: any of the owner's dollar accounts.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

// The closed vault's doors -------------------------------------------------------------------

#[derive(Accounts)]
pub struct RedeemClosed<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        mut,
        seeds = [HOLDER_SEED, vault.key().as_ref(), owner.key().as_ref()],
        bump = holder.bump,
        constraint = holder.vault == vault.key() && holder.owner == owner.key() @ crate::error::VaultError::WrongAccount,
    )]
    pub holder: Box<Account<'info, Holder>>,
    /// CHECK: the vault's station, by address; read for what it sent home.
    #[account(address = vault.station)]
    pub station: UncheckedAccount<'info>,
    /// CHECK: the receiver's associated dollar account, checked where paid.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClosedDoor<'info> {
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the vault's station, by address; read for what it sent home.
    #[account(address = vault.station)]
    pub station: UncheckedAccount<'info>,
    /// CHECK: the claim or request, checked by address or by the vault and
    /// id it holds.
    #[account(mut)]
    pub record: UncheckedAccount<'info>,
    /// CHECK: a queued claim's request, or a deposit's holder; checked where
    /// read.
    #[account(mut)]
    pub other: Option<UncheckedAccount<'info>>,
    /// CHECK: the receiver's associated dollar account, checked where paid.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CollectHeld<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the request or claim the payout is held on; none for the
    /// author's or the platform's fees.
    #[account(mut)]
    pub record: Option<UncheckedAccount<'info>>,
    /// CHECK: any of the owner's dollar accounts.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

// Rent back ----------------------------------------------------------------------------------

#[derive(Accounts)]
pub struct CloseRecord<'info> {
    #[account(seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the request or claim, checked by address or by the vault and
    /// id it holds.
    #[account(mut)]
    pub record: UncheckedAccount<'info>,
    /// CHECK: whoever paid its rent, as the record names them.
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseHolder<'info> {
    pub owner: Signer<'info>,
    #[account(seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    #[account(
        mut,
        close = payer,
        seeds = [HOLDER_SEED, vault.key().as_ref(), owner.key().as_ref()],
        bump = holder.bump,
        constraint = holder.vault == vault.key() && holder.owner == owner.key() @ crate::error::VaultError::WrongAccount,
    )]
    pub holder: Box<Account<'info, Holder>>,
    /// CHECK: whoever paid its rent, as the holding names them.
    #[account(mut, address = holder.payer)]
    pub payer: UncheckedAccount<'info>,
}

// The tank ---------------------------------------------------------------------------------------

#[derive(Accounts)]
pub struct FundTank<'info> {
    pub funder: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the funder's dollars; the token program checks they sign.
    #[account(mut)]
    pub from: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

/// What a signed usage total is checked with: the signer's records in the
/// registry, the price list its cap is worked out from, the transaction's
/// instructions (where the signature is), and the platform's dollar account
/// the bill is paid to.
#[derive(Accounts)]
pub struct Billing<'info> {
    /// CHECK: the registry's config, checked by `read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the signer's blessing for this vault, checked by `read`.
    pub blessing: UncheckedAccount<'info>,
    /// CHECK: the signer's key record, checked by `read`.
    pub key_info: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `read`.
    pub image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `read`.
    pub project: UncheckedAccount<'info>,
    /// CHECK: the price list, checked by `History::new`.
    pub prices: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar, checked by the Ed25519 scan.
    pub instructions: UncheckedAccount<'info>,
    /// CHECK: the platform's associated dollar account, checked where paid.
    #[account(mut)]
    pub platform_dollars: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Keep<'info> {
    /// Pays the rent of a tank fill's request, if one is queued.
    #[account(mut)]
    pub caller: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: created here if a fill is queued, at the vault's request
    /// address for the last fill it queued.
    #[account(mut)]
    pub request: Option<UncheckedAccount<'info>>,
    /// Read only when a signed usage total is given.
    pub billing: Billing<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct WithdrawTank<'info> {
    pub author: Signer<'info>,
    #[account(mut, has_one = author, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// Read only when a signed usage total is given.
    pub billing: Billing<'info>,
    /// CHECK: any of the author's dollar accounts.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct WithdrawTankUnsigned<'info> {
    pub author: Signer<'info>,
    #[account(mut, has_one = author, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: any of the author's dollar accounts.
    #[account(mut)]
    pub to: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Author<'info> {
    pub author: Signer<'info>,
    #[account(mut, has_one = author, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
}

#[derive(Accounts)]
pub struct SetAllowances<'info> {
    pub author: Signer<'info>,
    #[account(mut, has_one = author, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the price list, checked by `History::new`.
    pub prices: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TopUpFromShares<'info> {
    #[account(mut)]
    pub author: Signer<'info>,
    #[account(mut, has_one = author, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: created here, at the author's request address for the nonce.
    #[account(mut)]
    pub request: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

// The lifecycle ---------------------------------------------------------------------------------

#[derive(Accounts)]
pub struct Admin<'info> {
    pub admin: Signer<'info>,
    #[account(mut, has_one = admin, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
}

#[derive(Accounts)]
pub struct Anyone<'info> {
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
}

#[derive(Accounts)]
pub struct CloseVault<'info> {
    pub pool: Pool<'info>,
}

// Moving money between the vault and its station -------------------------------------------------

#[derive(Accounts)]
pub struct Move<'info> {
    /// The executor key the station has approved.
    pub key: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, &vault.strategy_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,
    /// CHECK: the vault's station, by address.
    #[account(mut, address = vault.station)]
    pub station: UncheckedAccount<'info>,
    /// CHECK: the station program, by address.
    #[account(address = shonin_station::ID)]
    pub station_program: UncheckedAccount<'info>,
    /// CHECK: the station's dollar account; the station checks it.
    #[account(mut)]
    pub station_dollars: UncheckedAccount<'info>,
    /// CHECK: the vault's dollar account, by address.
    #[account(mut, address = vault.dollars)]
    pub vault_dollars: UncheckedAccount<'info>,
    /// CHECK: the dollar, by address.
    #[account(address = vault.mint)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    /// CHECK: the registry's config, checked by `read`.
    pub registry_config: UncheckedAccount<'info>,
    /// CHECK: the key's blessing for this vault, checked by `read`.
    pub blessing: UncheckedAccount<'info>,
    /// CHECK: the key's record, checked by `read`.
    pub key_info: UncheckedAccount<'info>,
    /// CHECK: its image's record, checked by `read`.
    pub image: UncheckedAccount<'info>,
    /// CHECK: its project's record, checked by `read`.
    pub project: UncheckedAccount<'info>,
}
