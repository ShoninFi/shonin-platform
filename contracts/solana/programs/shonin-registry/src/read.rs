//! Reading the registry from another program, or from off the chain.
//!
//! The registry answers by its accounts: a program that needs to know whether
//! a key is blessed is handed the records and checks them here. Every record
//! is checked to be the one it stands for, at the address its seeds derive
//! for this program, so a caller can't hand in a look-alike; a record that
//! doesn't exist at its address reads as "never", which is what it means.

use anchor_lang::prelude::*;

use crate::error::RegistryError;
use crate::state::*;

/// The registry's settings.
pub fn config_address() -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED], &crate::ID).0
}

pub fn image_address(image_digest: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[IMAGE_SEED, image_digest], &crate::ID).0
}

pub fn project_address(project_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[PROJECT_SEED, project_id], &crate::ID).0
}

pub fn verifier_address(state: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VERIFIER_SEED, state.as_ref()], &crate::ID).0
}

pub fn key_address(signer: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[KEY_SEED, signer.as_ref()], &crate::ID).0
}

pub fn blessing_address(
    signer: &Pubkey,
    vault_chain: &[u8; 32],
    vault: &[u8; 32],
    strategy_hash: &[u8; 32],
) -> Pubkey {
    Pubkey::find_program_address(
        &[
            BLESSING_SEED,
            signer.as_ref(),
            vault_chain,
            vault,
            strategy_hash,
        ],
        &crate::ID,
    )
    .0
}

pub fn approval_address(kind: ApprovalKind, name: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[APPROVAL_SEED, kind.seed(), name], &crate::ID).0
}

pub fn revoked_address(kind: ApprovalKind, name: &[u8; 32], implementation: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[REVOKED_SEED, kind.seed(), name, implementation.as_ref()],
        &crate::ID,
    )
    .0
}

/// The record at `account`, which must be `expected`; `None` if nothing was
/// ever recorded there.
pub fn load<T: AccountDeserialize>(account: &AccountInfo, expected: &Pubkey) -> Result<Option<T>> {
    require_keys_eq!(*account.key, *expected, RegistryError::AccountSubstituted);
    if account.owner != &crate::ID || account.data_is_empty() {
        return Ok(None);
    }
    let data = account.try_borrow_data()?;
    Ok(Some(T::try_deserialize(&mut &data[..])?))
}

/// The registry's settings, which must exist.
pub fn config(account: &AccountInfo) -> Result<Config> {
    load::<Config>(account, &config_address())?
        .ok_or_else(|| error!(RegistryError::AccountSubstituted))
}

/// Whether `signer` is blessed as `kind` at `now`. `key`, `image` and
/// `project` are the key's record and the records of its image and project;
/// for a key never blessed, the last two are not read.
pub fn blessed_as(
    config_account: &AccountInfo,
    key: &AccountInfo,
    image: &AccountInfo,
    project: &AccountInfo,
    signer: &Pubkey,
    kind: ImageKind,
    now: u64,
) -> Result<bool> {
    let config = config(config_account)?;
    let Some(key) = load::<KeyInfo>(key, &key_address(signer))? else {
        return Ok(false);
    };
    let image = load::<ImageRecord>(image, &image_address(&key.image_digest))?;
    let project = load::<ProjectRecord>(project, &project_address(&key.project_id))?;
    Ok(is_blessed_as(
        &config,
        &key,
        image.as_ref(),
        project.as_ref(),
        kind,
        now,
    ))
}

/// The key's record, which must be `signer`'s; `None` if it was never
/// blessed.
pub fn key_info(key: &AccountInfo, signer: &Pubkey) -> Result<Option<KeyInfo>> {
    load::<KeyInfo>(key, &key_address(signer))
}

/// Whether `signer` is blessed for this vault and bundle on this vault chain
/// at `now`. A zero vault is never blessed.
#[allow(clippy::too_many_arguments)]
pub fn blessed_for(
    config_account: &AccountInfo,
    blessing: &AccountInfo,
    key: &AccountInfo,
    image: &AccountInfo,
    project: &AccountInfo,
    signer: &Pubkey,
    vault_chain: &[u8; 32],
    vault: &[u8; 32],
    strategy_hash: &[u8; 32],
    now: u64,
) -> Result<bool> {
    if vault == &[0; 32] {
        return Ok(false);
    }
    let config = config(config_account)?;
    let address = blessing_address(signer, vault_chain, vault, strategy_hash);
    let Some(blessing) = load::<Blessing>(blessing, &address)? else {
        return Ok(false);
    };
    let Some(key) = load::<KeyInfo>(key, &key_address(signer))? else {
        return Ok(false);
    };
    let image = load::<ImageRecord>(image, &image_address(&key.image_digest))?;
    let project = load::<ProjectRecord>(project, &project_address(&key.project_id))?;
    Ok(is_blessed_for(
        &config,
        &blessing,
        image.as_ref(),
        project.as_ref(),
        now,
    ))
}

/// When `signer`'s blessing for this vault and bundle expires; zero if it has
/// none.
pub fn blessed_until(
    blessing: &AccountInfo,
    signer: &Pubkey,
    vault_chain: &[u8; 32],
    vault: &[u8; 32],
    strategy_hash: &[u8; 32],
) -> Result<u64> {
    let address = blessing_address(signer, vault_chain, vault, strategy_hash);
    Ok(load::<Blessing>(blessing, &address)?.map_or(0, |blessing| blessing.until))
}

/// The implementation a station created now pins for this name: the default
/// key while none is approved, or once the latest approval is revoked.
/// `revoked` is the revocation record of the implementation the name's slot
/// approves now (whether or not it exists).
pub fn trading(
    kind: ApprovalKind,
    name: &[u8; 32],
    slot: &AccountInfo,
    revoked: &AccountInfo,
    now: u64,
) -> Result<Pubkey> {
    let Some(slot) = load::<ApprovalSlot>(slot, &approval_address(kind, name))? else {
        return Ok(Pubkey::default());
    };
    let effective = slot.effective(now);
    let record = load::<RevokedRecord>(revoked, &revoked_address(kind, name, &effective))?;
    Ok(crate::state::trading(Some(&slot), record.as_ref(), now))
}

/// Whether `implementation` of this name stands revoked at `now`.
pub fn revoked(
    kind: ApprovalKind,
    name: &[u8; 32],
    implementation: &Pubkey,
    slot: &AccountInfo,
    revoked: &AccountInfo,
    now: u64,
) -> Result<bool> {
    let slot = load::<ApprovalSlot>(slot, &approval_address(kind, name))?;
    let record = load::<RevokedRecord>(revoked, &revoked_address(kind, name, implementation))?;
    Ok(is_revoked(
        slot.as_ref(),
        record.as_ref(),
        implementation,
        now,
    ))
}
